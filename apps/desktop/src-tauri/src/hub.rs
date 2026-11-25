//! Hub discovery for the desktop shell: credentials (URL + bearer token from the
//! shared data dir), a tiny `/health` probe, and spawning `pawl serve` when the
//! hub is not running.
//!
//! The hub only ever listens on localhost, so `/health` is probed with a minimal
//! HTTP/1.1 client over `std::net::TcpStream` instead of pulling in a full HTTP stack.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Folder under the OS data dir. Mirrors `HUB_DATA_DIR_NAME` in `@pawl/protocol` / `pawl-protocol`.
pub const HUB_DATA_DIR_NAME: &str = "pawl";
/// Token file inside the data dir. Mirrors `HUB_TOKEN_FILE`.
pub const HUB_TOKEN_FILE: &str = "token";
/// The hub's config file inside the data dir (its top-level `port` is where clients find the hub).
pub const HUB_CONFIG_FILE: &str = "config.toml";
/// Written by the running hub: `{port, pid, startedAt}`.
pub const HUB_RUNTIME_FILE: &str = "runtime.json";
/// This machine's identity (`id`, `name`), written by the hub on first start.
pub const HUB_MACHINE_FILE: &str = "machine.toml";
pub const HUB_DEFAULT_PORT: u16 = 47800;
pub const HUB_HOST: &str = "127.0.0.1";
const SPAWN_LOG_FILE: &str = "desktop-hub.log";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubCredentials {
    /// HTTP base, e.g. `http://127.0.0.1:47800`.
    pub url: String,
    /// WebSocket endpoint, e.g. `ws://127.0.0.1:47800/ws`.
    pub ws_url: String,
    /// Bearer token; empty when the hub never ran (no token file yet).
    pub token: String,
    /// "env" | "file" | "missing".
    pub token_source: &'static str,
    pub data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubHealth {
    pub ok: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    /// The answering hub's machine id (protocol v7 hubs), used to verify a runtime.json port.
    pub machine_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartHubResult {
    /// Path of the binary that was launched (or would have been).
    pub path: Option<String>,
    pub pid: Option<u32>,
    /// The hub already answered `/health`; nothing was spawned.
    pub already_running: bool,
    /// `/health` answered OK within the wait window.
    pub healthy: bool,
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct HealthBody {
    ok: bool,
    version: Option<String>,
    #[serde(default, rename = "machineId")]
    machine_id: Option<String>,
}

/// The `pawl serve` child we launched (if any). Quitting the app stops it ("Quit Pawl"), but only that one: a hub
/// the user runs themselves (`pawl serve`), or one this app found already running, keeps running.
#[derive(Default)]
pub struct HubProcess {
    child: Mutex<Option<Child>>,
}

/// How long a stopped hub may take to exit before it is ended forcefully (Unix; Windows ends it at once).
const STOP_GRACE: Duration = Duration::from_secs(3);

impl HubProcess {
    /// Stop the hub this app started, with the agents it runs, if it is still running. Returns its pid when it
    /// stopped one. On Unix the hub gets SIGTERM (it shuts down cleanly: peers told, agents stopped), then SIGKILL
    /// after STOP_GRACE. On Windows a windowless process cannot be asked to close, so its process tree is ended
    /// (`taskkill /T /F`).
    pub fn stop_owned(&self) -> Option<u32> {
        let mut guard = self.child.lock().ok()?;
        let child = guard.as_mut()?;
        if !matches!(child.try_wait(), Ok(None)) {
            // It already exited (crashed, or restarted itself: the replacement is not a child of ours).
            *guard = None;
            return None;
        }
        let pid = child.id();
        stop_tree(pid);
        let deadline = Instant::now() + STOP_GRACE;
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
        *guard = None;
        Some(pid)
    }
}

#[cfg(windows)]
fn stop_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(unix)]
fn stop_tree(pid: u32) {
    // The hub runs in its own process group (see start_hub_blocking): signal the group, agents included.
    let _ = Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(HUB_DATA_DIR_NAME))
}

/// How long checking a runtime.json port may take (localhost: a closed port fails at once).
const VERIFY_TIMEOUT: Duration = Duration::from_millis(800);

/// Hub HTTP base URL: `PAWL_HUB_URL` (http/ws, with or without `/ws`), else the port the running hub bound
/// (`runtime.json`, only once its `/health` names this machine), else the `port` saved in config.toml
/// (Settings → Hub port), else the default. A saved port that is not applied yet therefore never hides the
/// running hub, and a stale or planted runtime.json never receives the token.
pub fn hub_http_url() -> String {
    resolve_hub_url().0
}

/// The hub's health right now, at the URL `hub_http_url()` picks (reusing the check of a runtime.json port).
pub fn hub_health_now(timeout: Duration) -> HubHealth {
    let (url, verified) = resolve_hub_url();
    verified.unwrap_or_else(|| probe_health(&url, timeout))
}

/// The URL, plus the `/health` answer when it came from a verified runtime.json port.
fn resolve_hub_url() -> (String, Option<HubHealth>) {
    if let Ok(raw) = std::env::var("PAWL_HUB_URL") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return (normalize_http_url(raw), None);
        }
    }
    let machine_id = local_machine_id();
    let mut verified = None;
    let port = choose_port(runtime_port(), config_port(), |port| {
        // Only /health (no token) is sent to an unverified port.
        let Some(id) = machine_id.as_deref() else { return false };
        let health = probe_health(&local_url(port), VERIFY_TIMEOUT);
        let ok = runtime_verified(&health, id);
        if ok {
            verified = Some(health);
        }
        ok
    });
    (local_url(port), verified)
}

fn local_url(port: u16) -> String {
    format!("http://{HUB_HOST}:{port}")
}

/// Port choice: a runtime.json port that differs from the configured one is used only when `verify` accepts it
/// (its `/health` names this machine); otherwise the config.toml port, else the default.
fn choose_port(runtime: Option<u16>, config: Option<u16>, verify: impl FnOnce(u16) -> bool) -> u16 {
    let fallback = config.unwrap_or(HUB_DEFAULT_PORT);
    match runtime {
        Some(port) if port != fallback && verify(port) => port,
        _ => fallback,
    }
}

/// True when a `/health` answer is this machine's hub.
fn runtime_verified(health: &HubHealth, machine_id: &str) -> bool {
    health.ok && health.machine_id.as_deref() == Some(machine_id)
}

/// This machine's id from `<data dir>/machine.toml` (written by the hub on first start).
fn local_machine_id() -> Option<String> {
    let text = fs::read_to_string(data_dir()?.join(HUB_MACHINE_FILE)).ok()?;
    machine_id_from_toml(&text)
}

/// Top-level `id = "…"` of machine.toml (a 12-character [a-z0-9] machine id).
fn machine_id_from_toml(text: &str) -> Option<String> {
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        let Some(rest) = line.strip_prefix("id") else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        let value = value.split('#').next().unwrap_or_default().trim();
        let id = ['"', '\''].iter().find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))?;
        let valid = id.len() == 12 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        return valid.then(|| id.to_string());
    }
    None
}

/// `{port, pid, startedAt}` that the hub writes to `<data dir>/runtime.json` at startup (removed on a clean exit).
fn runtime_port() -> Option<u16> {
    let text = fs::read_to_string(data_dir()?.join(HUB_RUNTIME_FILE)).ok()?;
    port_from_runtime_json(&text)
}

fn port_from_runtime_json(text: &str) -> Option<u16> {
    #[derive(Deserialize)]
    struct Runtime {
        port: u16,
    }
    serde_json::from_str::<Runtime>(text.trim_start_matches('\u{feff}')).ok().map(|r| r.port).filter(|p| *p > 0)
}

/// The top-level `port = N` in `<data dir>/config.toml`, if set.
fn config_port() -> Option<u16> {
    let text = fs::read_to_string(data_dir()?.join(HUB_CONFIG_FILE)).ok()?;
    port_from_toml(&text)
}

/// Top-level `port = N` (before the first `[table]`); comments and other keys are ignored. No TOML crate needed
/// for one integer key.
fn port_from_toml(text: &str) -> Option<u16> {
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        let Some(rest) = line.strip_prefix("port") else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        let value = value.split('#').next().unwrap_or_default().trim();
        return value.parse::<u16>().ok().filter(|p| *p > 0);
    }
    None
}

/// This machine's address on its main network (the source address of the default route), for suggesting a
/// peer-listener address. Connecting a UDP socket only picks a route; no packet is sent.
pub fn lan_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?; // TEST-NET-1: never routed anywhere real
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

fn normalize_http_url(raw: &str) -> String {
    let mut url = raw.trim().trim_end_matches('/').to_string();
    if let Some(rest) = url.strip_prefix("ws://") {
        url = format!("http://{rest}");
    } else if let Some(rest) = url.strip_prefix("wss://") {
        url = format!("https://{rest}");
    } else if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }
    if let Some(stripped) = url.strip_suffix("/ws") {
        url = stripped.to_string();
    }
    url.trim_end_matches('/').to_string()
}

fn ws_url_for(http: &str) -> String {
    let base = if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{http}")
    };
    format!("{base}/ws")
}

pub fn credentials() -> HubCredentials {
    let url = hub_http_url();
    let ws_url = ws_url_for(&url);
    let dir = data_dir();
    let (token, token_source) = match std::env::var("PAWL_HUB_TOKEN") {
        Ok(t) if !t.trim().is_empty() => (t.trim().to_string(), "env"),
        _ => match dir
            .as_ref()
            .map(|d| d.join(HUB_TOKEN_FILE))
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
        {
            Some(t) if !t.is_empty() => (t, "file"),
            _ => (String::new(), "missing"),
        },
    };
    HubCredentials {
        url,
        ws_url,
        token,
        token_source,
        data_dir: dir.map(|d| d.display().to_string()),
    }
}

struct HostPort {
    host: String,
    port: u16,
}

fn parse_host_port(http_url: &str) -> Result<HostPort, String> {
    let (rest, default_port) = if let Some(r) = http_url.strip_prefix("http://") {
        (r, 80)
    } else if http_url.starts_with("https://") {
        return Err("TLS hub URLs are not supported by the desktop health probe".into());
    } else {
        (http_url, 80)
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(format!("invalid hub URL: {http_url}"));
    }
    // [::1]:port
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("invalid hub URL: {http_url}"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("invalid port in {http_url}"))?,
            None => default_port,
        };
        return Ok(HostPort { host: host.to_string(), port });
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok(HostPort {
            host: host.to_string(),
            port: port.parse().map_err(|_| format!("invalid port in {http_url}"))?,
        }),
        None => Ok(HostPort { host: authority.to_string(), port: default_port }),
    }
}

/// Blocking `GET {url}/health` with a short timeout.
pub fn probe_health(http_url: &str, timeout: Duration) -> HubHealth {
    match probe_health_inner(http_url, timeout) {
        Ok(body) => HubHealth {
            ok: body.ok,
            version: body.version,
            error: if body.ok { None } else { Some("hub reported not ok".into()) },
            machine_id: body.machine_id,
        },
        Err(e) => HubHealth { ok: false, version: None, error: Some(e), machine_id: None },
    }
}

fn probe_health_inner(http_url: &str, timeout: Duration) -> Result<HealthBody, String> {
    let hp = parse_host_port(http_url)?;
    let addr = (hp.host.as_str(), hp.port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", hp.host))?
        .next()
        .ok_or_else(|| format!("no address for {}", hp.host))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let host_header = if hp.host.contains(':') { format!("[{}]:{}", hp.host, hp.port) } else { format!("{}:{}", hp.host, hp.port) };
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: pawl-desktop\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;

    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 64 * 1024 {
                    return Err("health response too large".into());
                }
                if response_complete(&buf) {
                    break;
                }
            }
            Err(e) if buf.is_empty() => return Err(format!("read: {e}")),
            Err(_) => break,
        }
    }
    let (status, headers, body) = split_response(&buf).ok_or("malformed HTTP response")?;
    if status != 200 {
        return Err(format!("/health returned HTTP {status}"));
    }
    let body = if headers.contains("transfer-encoding: chunked") { decode_chunked(body) } else { body.to_vec() };
    serde_json::from_slice(&body).map_err(|e| format!("bad /health body: {e}"))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn response_complete(buf: &[u8]) -> bool {
    let Some(end) = find_header_end(buf) else { return false };
    let headers = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let body_len = buf.len() - (end + 4);
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                return body_len >= n;
            }
        }
    }
    if headers.contains("transfer-encoding: chunked") {
        return buf.ends_with(b"0\r\n\r\n");
    }
    false
}

fn split_response(buf: &[u8]) -> Option<(u16, String, &[u8])> {
    let end = find_header_end(buf)?;
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let status_line = head.lines().next()?;
    let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = &buf[end + 4..];
    let lower = head.to_ascii_lowercase();
    for line in lower.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                body = &body[..n.min(body.len())];
            }
        }
    }
    Some((status, lower, body))
}

fn decode_chunked(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") {
        let size_str = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        data = &data[line_end + 2..];
        if size == 0 || data.len() < size {
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[(size + 2).min(data.len())..];
    }
    out
}

fn hub_exe_name() -> &'static str {
    if cfg!(windows) {
        "pawl.exe"
    } else {
        "pawl"
    }
}

fn is_file(p: &Path) -> bool {
    p.metadata().map(|m| m.is_file()).unwrap_or(false)
}

/// Look for `pawl` in PATH (honours PATHEXT on Windows).
fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    let stem = name.trim_end_matches(".exe");
    for dir in std::env::split_paths(&path) {
        if cfg!(windows) {
            for ext in &exts {
                let candidate = dir.join(format!("{stem}{ext}"));
                if is_file(&candidate) {
                    return Some(candidate);
                }
            }
        } else {
            let candidate = dir.join(name);
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where `pawl` lives: `PAWL_HUB_BIN`, next to this executable (bundled installs),
/// on PATH, or (debug builds only) the repo's cargo target dirs.
pub fn find_hub_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PAWL_HUB_BIN") {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(p);
        }
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let candidate = dir.join(hub_exe_name());
        if is_file(&candidate) {
            return Some(candidate);
        }
        // macOS bundles: Contents/MacOS/<app> → Contents/Resources/pawl
        let resources = dir.join("../Resources").join(hub_exe_name());
        if is_file(&resources) {
            return Some(resources);
        }
    }
    if let Some(p) = which_in_path(hub_exe_name()) {
        return Some(p);
    }
    if cfg!(debug_assertions) {
        // apps/desktop/src-tauri → repo root
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for profile in ["debug", "release"] {
            let candidate = repo.join("target").join(profile).join(hub_exe_name());
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn open_spawn_log() -> Option<File> {
    let dir = data_dir()?;
    fs::create_dir_all(&dir).ok()?;
    OpenOptions::new().create(true).write(true).truncate(true).open(dir.join(SPAWN_LOG_FILE)).ok()
}

/// Blocking: if the hub is healthy return immediately, otherwise spawn `pawl serve`
/// detached and wait up to ~10 s for `/health`.
pub fn start_hub_blocking(proc_state: &HubProcess) -> Result<StartHubResult, String> {
    let health = hub_health_now(Duration::from_millis(800));
    if health.ok {
        return Ok(StartHubResult { path: None, pid: None, already_running: true, healthy: true, version: health.version });
    }

    let mut guard = proc_state.child.lock().map_err(|_| "hub process lock poisoned")?;
    // A previous spawn may still be booting.
    let still_booting = match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    };

    let (path, pid) = if still_booting {
        let child = guard.as_ref().expect("checked above");
        (None, Some(child.id()))
    } else {
        let bin = find_hub_binary().ok_or_else(|| {
            format!(
                "Could not find `{}`. Install it on PATH, place it next to the desktop app, or set PAWL_HUB_BIN.",
                hub_exe_name()
            )
        })?;
        let mut cmd = Command::new(&bin);
        // No --port: the hub reads PAWL_HUB_URL (inherited) and config.toml itself, in the same order as
        // hub_http_url(). A flag would pin the port and lock the Hub port setting in the UI.
        cmd.arg("serve");
        cmd.stdin(Stdio::null());
        match open_spawn_log() {
            Some(log) => {
                let err = log.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());
                cmd.stdout(Stdio::from(log)).stderr(err);
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group: Ctrl+C in a dev terminal must not take the hub down with us.
            cmd.process_group(0);
        }
        let child = cmd.spawn().map_err(|e| format!("failed to launch {}: {e}", bin.display()))?;
        let pid = child.id();
        *guard = Some(child);
        (Some(bin.display().to_string()), Some(pid))
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Re-resolve each time: the new hub writes runtime.json with the port it actually bound.
        let h = hub_health_now(Duration::from_millis(500));
        if h.ok {
            return Ok(StartHubResult { path, pid, already_running: false, healthy: true, version: h.version });
        }
        if let Some(child) = guard.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                *guard = None;
                let log_hint = data_dir()
                    .map(|d| format!(" See {}.", d.join(SPAWN_LOG_FILE).display()))
                    .unwrap_or_default();
                return Err(format!("pawl exited early ({status}).{log_hint}"));
            }
        }
        if Instant::now() >= deadline {
            return Ok(StartHubResult { path, pid, already_running: false, healthy: false, version: None });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long-running stand-in for `pawl serve` (it sleeps; the test stops it).
    fn long_running() -> Child {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sleep");
            c.arg("60");
            use std::os::unix::process::CommandExt;
            c.process_group(0);
            c
        };
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn")
    }

    #[test]
    fn quitting_stops_only_the_hub_this_app_started() {
        // Nothing started: nothing to stop (a hub the user runs is never a child of ours).
        assert_eq!(HubProcess::default().stop_owned(), None);

        let hub = HubProcess::default();
        let child = long_running();
        let pid = child.id();
        *hub.child.lock().unwrap() = Some(child);
        assert_eq!(hub.stop_owned(), Some(pid));
        assert!(hub.child.lock().unwrap().is_none());
        // Stopping again does nothing.
        assert_eq!(hub.stop_owned(), None);
    }

    #[test]
    fn a_hub_that_already_exited_is_forgotten() {
        let hub = HubProcess::default();
        let mut child = long_running();
        child.kill().unwrap();
        child.wait().unwrap();
        *hub.child.lock().unwrap() = Some(child);
        assert_eq!(hub.stop_owned(), None);
        assert!(hub.child.lock().unwrap().is_none());
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(normalize_http_url("ws://127.0.0.1:47800/ws"), "http://127.0.0.1:47800");
        assert_eq!(normalize_http_url("127.0.0.1:9000/"), "http://127.0.0.1:9000");
        assert_eq!(normalize_http_url("http://localhost:47800"), "http://localhost:47800");
        assert_eq!(ws_url_for("http://127.0.0.1:47800"), "ws://127.0.0.1:47800/ws");
    }

    #[test]
    fn parses_host_port() {
        let hp = parse_host_port("http://127.0.0.1:47800").unwrap();
        assert_eq!((hp.host.as_str(), hp.port), ("127.0.0.1", 47800));
        let hp = parse_host_port("http://[::1]:5000").unwrap();
        assert_eq!((hp.host.as_str(), hp.port), ("::1", 5000));
    }

    #[test]
    fn reads_top_level_port_from_config() {
        assert_eq!(port_from_toml("port = 47900\n"), Some(47900));
        assert_eq!(port_from_toml("\u{feff}# hub\r\noffline = true\r\nport=48000 # mine\r\n"), Some(48000));
        assert_eq!(port_from_toml("# port = 47900\n"), None);
        assert_eq!(port_from_toml("[peer]\nport = 47901\n"), None);
        assert_eq!(port_from_toml("port = 99999\n"), None);
        assert_eq!(port_from_toml("port = \"47900\"\n"), None);
        assert_eq!(port_from_toml("ports = 1\nport = 2000\n"), Some(2000));
    }

    #[test]
    fn reads_machine_id_from_machine_toml() {
        assert_eq!(machine_id_from_toml("id = \"abcdef012345\"\nname = \"PC\"\n").as_deref(), Some("abcdef012345"));
        assert_eq!(machine_id_from_toml("# m\r\nname = 'PC'\r\nid='abcdef012345' # x\r\n").as_deref(), Some("abcdef012345"));
        assert_eq!(machine_id_from_toml("id = \"ABCDEF012345\"\n"), None);
        assert_eq!(machine_id_from_toml("id = \"short\"\n"), None);
        assert_eq!(machine_id_from_toml("[x]\nid = \"abcdef012345\"\n"), None);
        assert_eq!(machine_id_from_toml("ids = 1\nname = \"x\"\n"), None);
    }

    #[test]
    fn runtime_port_is_used_only_when_verified() {
        // Different from config + verified -> runtime port.
        assert_eq!(choose_port(Some(47900), Some(47950), |p| p == 47900), 47900);
        // Not verified (stale, or another program on that port) -> config port, else default.
        assert_eq!(choose_port(Some(47900), Some(47950), |_| false), 47950);
        assert_eq!(choose_port(Some(47900), None, |_| false), HUB_DEFAULT_PORT);
        // Same as the configured port: nothing to verify.
        assert_eq!(choose_port(Some(47950), Some(47950), |_| panic!("no check needed")), 47950);
        assert_eq!(choose_port(None, Some(47950), |_| panic!("no check needed")), 47950);
        let h = |ok, id: Option<&str>| HubHealth { ok, version: None, error: None, machine_id: id.map(str::to_string) };
        assert!(runtime_verified(&h(true, Some("abcdef012345")), "abcdef012345"));
        assert!(!runtime_verified(&h(true, Some("zzzzzz012345")), "abcdef012345"));
        assert!(!runtime_verified(&h(true, None), "abcdef012345"));
        assert!(!runtime_verified(&h(false, Some("abcdef012345")), "abcdef012345"));
    }

    #[test]
    fn health_probe_reports_the_machine_id() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let n = s.read(&mut req).unwrap();
            let text = String::from_utf8_lossy(&req[..n]).to_string();
            let body = r#"{"ok":true,"version":"0.7.0","machineId":"abcdef012345"}"#;
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
            s.write_all(resp.as_bytes()).unwrap();
            text
        });
        let h = probe_health(&local_url(port), Duration::from_secs(2));
        let request = server.join().unwrap();
        assert!(h.ok);
        assert_eq!(h.machine_id.as_deref(), Some("abcdef012345"));
        // The check never carries the token.
        assert!(request.starts_with("GET /health "));
        assert!(!request.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn reads_running_port_from_runtime_json() {
        assert_eq!(port_from_runtime_json(r#"{"port":47900,"pid":12,"startedAt":1}"#), Some(47900));
        assert_eq!(port_from_runtime_json("\u{feff}{\"port\":48000}"), Some(48000));
        assert_eq!(port_from_runtime_json(r#"{"port":"47900"}"#), None);
        assert_eq!(port_from_runtime_json(r#"{"port":0}"#), None);
        assert_eq!(port_from_runtime_json("{"), None);
    }

    #[test]
    fn parses_responses() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        assert!(response_complete(raw));
        let (status, _, body) = split_response(raw).unwrap();
        assert_eq!(status, 200);
        let parsed: HealthBody = serde_json::from_slice(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.version.as_deref(), Some("0.1.0"));

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n{\"ok\":true}\r\n0\r\n\r\n";
        assert!(response_complete(chunked));
        let (_, headers, body) = split_response(chunked).unwrap();
        assert!(headers.contains("transfer-encoding: chunked"));
        assert_eq!(decode_chunked(body), b"{\"ok\":true}");
    }
}
