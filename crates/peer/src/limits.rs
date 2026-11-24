//! Abuse limits for the peer listener:
//! - connection level ([`PeerListener`]): at most `MAX_CONNECTIONS` accepted connections (any phase),
//!   `MAX_UNAUTH_TOTAL` unauthenticated ones in total and `MAX_UNAUTH_PER_IP` per remote IP, and a
//!   deadline that closes a connection which has not authenticated in time, covering the HTTP header
//!   read and the upgrade as well as the first frame (no slow-loris);
//! - pairing level ([`PairLimiter`]): failed pair attempts per remote IP (or relay sender), and coalescing of the
//!   `PairRejected` events they produce.
//!
//! IPs are keyed per IPv4 address, or per IPv6 /64 (one host usually owns a whole /64).

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

/// Accepted connections at once, authenticated links included.
pub(crate) const MAX_CONNECTIONS: usize = 64;
/// Connections that have not authenticated yet, in total.
pub(crate) const MAX_UNAUTH_TOTAL: usize = 16;
/// Connections that have not authenticated yet, per remote IP.
pub(crate) const MAX_UNAUTH_PER_IP: usize = 4;

/// Failed pair attempts per IP within `PAIR_WINDOW` before the IP is blocked.
pub(crate) const PAIR_MAX_FAILURES: u32 = 5;
/// Pair attempts per PAIR_WINDOW, all origins together (each one costs a box open).
pub(crate) const PAIR_ATTEMPTS_PER_WINDOW: u32 = 60;
pub(crate) const PAIR_WINDOW: Duration = Duration::from_secs(60);
/// How long pair requests from a blocked IP are dropped.
pub(crate) const PAIR_BLOCK: Duration = Duration::from_secs(60);
/// At most one `PairRejected` event per IP per this period.
pub(crate) const PAIR_EVENT_EVERY: Duration = Duration::from_secs(60);
/// IPs the pair limiter remembers (bounded memory).
const PAIR_MAX_TRACKED: usize = 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The rate-limit key of a remote IP: the IPv4 address (IPv4-mapped IPv6 included), or the IPv6 /64.
pub(crate) fn ip_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

// ---------- pairing ----------

#[derive(Debug)]
struct PairIp {
    window_start: Instant,
    failures: u32,
    blocked_until: Option<Instant>,
    last_event: Option<Instant>,
    suppressed: u32,
}

impl PairIp {
    fn idle(&self, now: Instant) -> bool {
        self.blocked_until.is_none_or(|t| now >= t)
            && now.duration_since(self.window_start) >= PAIR_WINDOW
            && self.last_event.is_none_or(|t| now.duration_since(t) >= PAIR_EVENT_EVERY)
    }
}

/// What to do after a rejected pair attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Failure {
    /// Emit a `PairRejected` event now; the number is how many earlier rejections were coalesced
    /// into it (not emitted).
    pub emit: Option<u32>,
    /// This failure blocked the IP.
    pub blocked: bool,
}

/// Who a pair attempt came from: a remote IP (LAN listener; IPv6 by /64), or the relay. All relay
/// origins share one bucket: anyone can make up sender keys, so per-key state would limit nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PairKey {
    Ip(IpAddr),
    Relay,
}

impl PairIp {
    fn new(now: Instant) -> Self {
        PairIp { window_start: now, failures: 0, blocked_until: None, last_event: None, suppressed: 0 }
    }
}

impl From<IpAddr> for PairKey {
    fn from(ip: IpAddr) -> Self {
        PairKey::Ip(ip_key(ip))
    }
}

#[derive(Debug, Default)]
pub(crate) struct PairLimiter {
    ips: HashMap<IpAddr, PairIp>,
    /// The shared relay bucket, kept apart so relay traffic never evicts an IP entry.
    relay: Option<PairIp>,
    /// Pair attempts (any origin, any outcome) in the current window: (window start, count).
    attempts: Option<(Instant, u32)>,
}

impl PairLimiter {
    /// True while pair requests from `key` are dropped.
    pub fn blocked(&self, key: impl Into<PairKey>, now: Instant) -> bool {
        let s = match key.into() {
            PairKey::Ip(ip) => self.ips.get(&ip),
            PairKey::Relay => self.relay.as_ref(),
        };
        s.and_then(|s| s.blocked_until).is_some_and(|t| now < t)
    }

    /// Count one pair attempt against the global cap (PAIR_ATTEMPTS_PER_WINDOW per PAIR_WINDOW, all
    /// origins together). False: over the cap, drop the attempt.
    pub fn attempt(&mut self, now: Instant) -> bool {
        let w = self.attempts.get_or_insert((now, 0));
        if now.duration_since(w.0) >= PAIR_WINDOW {
            *w = (now, 0);
        }
        if w.1 >= PAIR_ATTEMPTS_PER_WINDOW {
            return false;
        }
        w.1 += 1;
        true
    }

    pub fn failure(&mut self, key: impl Into<PairKey>, now: Instant) -> Failure {
        let s = match key.into() {
            PairKey::Relay => self.relay.get_or_insert_with(|| PairIp::new(now)),
            PairKey::Ip(ip) => {
                if !self.ips.contains_key(&ip) && self.ips.len() >= PAIR_MAX_TRACKED {
                    self.ips.retain(|_, s| !s.idle(now));
                    if self.ips.len() >= PAIR_MAX_TRACKED {
                        // Still full: forget the IP whose window started first.
                        if let Some(oldest) = self.ips.iter().min_by_key(|(_, s)| s.window_start).map(|(k, _)| *k) {
                            self.ips.remove(&oldest);
                        }
                    }
                }
                self.ips.entry(ip).or_insert_with(|| PairIp::new(now))
            }
        };
        if now.duration_since(s.window_start) >= PAIR_WINDOW {
            s.window_start = now;
            s.failures = 0;
        }
        s.failures += 1;
        let blocked = s.failures >= PAIR_MAX_FAILURES;
        if blocked {
            s.blocked_until = Some(now + PAIR_BLOCK);
            s.failures = 0;
            s.window_start = now;
        }
        let emit = match s.last_event {
            Some(t) if now.duration_since(t) < PAIR_EVENT_EVERY => {
                s.suppressed += 1;
                None
            }
            _ => {
                s.last_event = Some(now);
                Some(std::mem::take(&mut s.suppressed))
            }
        };
        Failure { emit, blocked }
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.ips.clear();
        self.relay = None;
        self.attempts = None;
    }

    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.ips.len() + usize::from(self.relay.is_some())
    }
}

// ---------- connections ----------

#[derive(Debug, Default)]
struct UnauthCounts {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

#[derive(Debug)]
pub(crate) struct ConnLimits {
    total: Arc<Semaphore>,
    unauth: Mutex<UnauthCounts>,
}

impl ConnLimits {
    pub fn new() -> Arc<Self> {
        Arc::new(ConnLimits { total: Arc::new(Semaphore::new(MAX_CONNECTIONS)), unauth: Mutex::default() })
    }

    fn try_unauth(self: &Arc<Self>, ip: IpAddr) -> Option<UnauthSlot> {
        let key = ip_key(ip);
        let mut c = lock(&self.unauth);
        let n = c.per_ip.get(&key).copied().unwrap_or(0);
        if c.total >= MAX_UNAUTH_TOTAL || n >= MAX_UNAUTH_PER_IP {
            return None;
        }
        c.total += 1;
        c.per_ip.insert(key, n + 1);
        Some(UnauthSlot { limits: self.clone(), key })
    }
}

/// One unauthenticated-connection slot; released on drop.
#[derive(Debug)]
struct UnauthSlot {
    limits: Arc<ConnLimits>,
    key: IpAddr,
}

impl Drop for UnauthSlot {
    fn drop(&mut self) {
        let mut c = lock(&self.limits.unauth);
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_ip.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                c.per_ip.remove(&self.key);
            }
        }
    }
}

/// State shared between an accepted connection's stream and its request handler.
#[derive(Debug)]
pub(crate) struct ConnShared {
    authenticated: AtomicBool,
    slot: Mutex<Option<UnauthSlot>>,
}

impl ConnShared {
    /// The connection carries an authenticated link: lift the deadline, free the unauthenticated slot.
    pub fn authenticated(&self) {
        self.authenticated.store(true, Ordering::SeqCst);
        lock(&self.slot).take();
    }

    fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::SeqCst)
    }
}

/// `axum::serve` listener enforcing the connection limits.
pub(crate) struct PeerListener {
    inner: TcpListener,
    limits: Arc<ConnLimits>,
    deadline: Duration,
}

impl PeerListener {
    pub fn new(inner: TcpListener, limits: Arc<ConnLimits>, deadline: Duration) -> Self {
        PeerListener { inner, limits, deadline }
    }
}

impl Listener for PeerListener {
    type Io = GuardedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            // At the cap, stop accepting: further connections wait in the OS backlog.
            let permit = self.limits.total.clone().acquire_owned().await.expect("semaphore never closed");
            let (stream, addr) = match self.inner.accept().await {
                Ok(x) => x,
                Err(e) => {
                    warn!(error = %e, "peer listener accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Some(slot) = self.limits.try_unauth(addr.ip()) else {
                debug!(remote = %addr, "peer connection dropped: unauthenticated connection limit");
                continue;
            };
            let _ = stream.set_nodelay(true);
            let shared = Arc::new(ConnShared { authenticated: AtomicBool::new(false), slot: Mutex::new(Some(slot)) });
            let stream = GuardedStream {
                stream,
                _permit: permit,
                shared,
                deadline: Box::pin(tokio::time::sleep(self.deadline)),
            };
            return (stream, addr);
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// An accepted TCP stream that fails with `TimedOut` once its deadline passes while it is still
/// unauthenticated. Holds its connection permit and unauthenticated slot until dropped.
pub(crate) struct GuardedStream {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
    shared: Arc<ConnShared>,
    deadline: Pin<Box<tokio::time::Sleep>>,
}

impl GuardedStream {
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        // Polling the timer registers the waker, so a silent peer is woken (and cut) at the deadline.
        !self.shared.is_authenticated() && self.deadline.as_mut().poll(cx).is_ready()
    }
}

fn deadline_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "peer connection did not authenticate in time")
}

impl AsyncRead for GuardedStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.expired(cx) {
            return Poll::Ready(Err(deadline_error()));
        }
        Pin::new(&mut this.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for GuardedStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.expired(cx) {
            return Poll::Ready(Err(deadline_error()));
        }
        Pin::new(&mut this.stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.expired(cx) {
            return Poll::Ready(Err(deadline_error()));
        }
        Pin::new(&mut this.stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Connect info handed to the request handler: the remote address and the connection's shared state.
#[derive(Debug, Clone)]
pub(crate) struct PeerConn {
    pub remote: SocketAddr,
    pub shared: Arc<ConnShared>,
}

impl Connected<IncomingStream<'_, PeerListener>> for PeerConn {
    fn connect_info(stream: IncomingStream<'_, PeerListener>) -> Self {
        PeerConn { remote: *stream.remote_addr(), shared: stream.io().shared.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn keys() {
        assert_eq!(ip_key(ip("10.0.0.1")), ip("10.0.0.1"));
        assert_eq!(ip_key(ip("::ffff:10.0.0.1")), ip("10.0.0.1"));
        assert_eq!(ip_key(ip("2001:db8:1:2:3:4:5:6")), ip("2001:db8:1:2::"));
        assert_eq!(ip_key(ip("2001:db8:1:2:ffff::1")), ip_key(ip("2001:db8:1:2::9")));
    }

    #[test]
    fn pair_limiter_blocks_and_coalesces() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        let a = ip("192.168.1.9");
        // First failure emits; the next ones within a minute are coalesced.
        assert_eq!(l.failure(a, t0), Failure { emit: Some(0), blocked: false });
        for i in 1..PAIR_MAX_FAILURES - 1 {
            assert_eq!(l.failure(a, t0 + Duration::from_secs(u64::from(i))), Failure { emit: None, blocked: false });
        }
        assert!(!l.blocked(a, t0 + Duration::from_secs(5)));
        assert_eq!(l.failure(a, t0 + Duration::from_secs(5)), Failure { emit: None, blocked: true });
        assert!(l.blocked(a, t0 + Duration::from_secs(6)));
        assert!(!l.blocked(ip("192.168.1.10"), t0 + Duration::from_secs(6)), "other IPs unaffected");
        assert!(!l.blocked(a, t0 + Duration::from_secs(5) + PAIR_BLOCK));
        // After a minute the next event carries the count of coalesced rejections.
        assert_eq!(l.failure(a, t0 + Duration::from_secs(70)), Failure { emit: Some(4), blocked: false });
    }

    #[test]
    fn pair_limiter_window_resets() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        let a = ip("10.0.0.1");
        for i in 0..PAIR_MAX_FAILURES - 1 {
            l.failure(a, t0 + Duration::from_secs(u64::from(i)));
        }
        // Slow failures (one window apart) never block.
        assert!(!l.failure(a, t0 + PAIR_WINDOW + Duration::from_secs(10)).blocked);
        assert!(!l.blocked(a, t0 + PAIR_WINDOW + Duration::from_secs(11)));
    }

    #[test]
    fn relay_origins_share_one_bucket_that_never_evicts_ips() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        // Fill the IP table.
        for i in 0..PAIR_MAX_TRACKED as u32 {
            l.failure(IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i)), t0);
        }
        let first = IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000));
        assert_eq!(l.tracked(), PAIR_MAX_TRACKED);
        // Relay failures (whatever key they claim) go to the one shared bucket and block it.
        for i in 0..PAIR_MAX_FAILURES {
            l.failure(PairKey::Relay, t0 + Duration::from_secs(u64::from(i)));
        }
        assert!(l.blocked(PairKey::Relay, t0 + Duration::from_secs(6)));
        assert_eq!(l.tracked(), PAIR_MAX_TRACKED + 1, "no IP entry was evicted");
        assert!(l.ips.contains_key(&first));
        assert!(!l.blocked(first, t0 + Duration::from_secs(6)), "LAN IPs are unaffected");
    }

    #[test]
    fn global_cap_on_pair_attempts() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        for _ in 0..PAIR_ATTEMPTS_PER_WINDOW {
            assert!(l.attempt(t0));
        }
        assert!(!l.attempt(t0 + Duration::from_secs(1)), "over the cap");
        assert!(l.attempt(t0 + PAIR_WINDOW), "a new window");
    }

    #[test]
    fn pair_limiter_is_bounded() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        for i in 0..(PAIR_MAX_TRACKED as u32 + 50) {
            let a = IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i));
            l.failure(a, t0);
        }
        assert!(l.tracked() <= PAIR_MAX_TRACKED);
    }

    #[test]
    fn unauth_slots_per_ip_and_total() {
        let limits = ConnLimits::new();
        let a = ip("127.0.0.1");
        let mut held: Vec<UnauthSlot> = (0..MAX_UNAUTH_PER_IP).map(|_| limits.try_unauth(a).unwrap()).collect();
        assert!(limits.try_unauth(a).is_none(), "per-IP cap");
        held.pop();
        assert!(limits.try_unauth(a).is_some(), "freed on drop");
        let mut others = Vec::new();
        for i in 0..MAX_UNAUTH_TOTAL {
            if let Some(s) = limits.try_unauth(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, i as u8))) {
                others.push(s);
            }
        }
        assert_eq!(held.len() + others.len(), MAX_UNAUTH_TOTAL, "total cap");
        assert!(limits.try_unauth(ip("10.9.9.9")).is_none());
        drop(others);
        drop(held);
        assert_eq!(lock(&limits.unauth).total, 0);
        assert!(lock(&limits.unauth).per_ip.is_empty());
    }
}
