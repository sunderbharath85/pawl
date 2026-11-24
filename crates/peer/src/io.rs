//! WebSocket plumbing shared by the listener (axum) and the dialer (tokio-tungstenite): both sockets
//! become a pair of channels carrying text frames, so the link logic is written once.

use futures::{Sink, SinkExt, Stream, StreamExt};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::AbortHandle;

/// Max WebSocket frame and message size, both directions. Fits the envelope of the largest body
/// `PeerNode::send` accepts (MIRROR_FRAME_MAX_BYTES + 16 KiB, about 288 KiB once boxed and base64'd).
pub(crate) const MAX_FRAME: usize = 320 * 1024;
/// A peer that stops reading must not wedge our writer.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a closing socket may linger for the peer's close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
const INBOUND_QUEUE: usize = 16;
/// Outbound frames queued per socket. Senders use `try_send`; a full queue means the peer is not
/// reading and the link is treated as dead.
pub(crate) const OUTBOUND_QUEUE: usize = 64;

pub(crate) enum Outbound {
    Text(String),
    Close,
}

/// One WebSocket as channels. Dropping it closes the socket.
pub(crate) struct WsIo {
    /// Text frames from the peer; `None` once the socket is closed (or sent a binary frame).
    pub rx: mpsc::Receiver<String>,
    pub tx: mpsc::Sender<Outbound>,
    reader: AbortHandle,
    writer: AbortHandle,
    /// Close code the peer sent (0 = none yet).
    close_code: Arc<AtomicU32>,
}

/// The sending half of a `WsIo` (see `WsIo::split`).
pub(crate) struct WsTx {
    tx: mpsc::Sender<Outbound>,
    reader: AbortHandle,
    writer: AbortHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendFail {
    /// The socket is not draining its queue (yet).
    Full,
    Closed,
}

impl WsIo {
    /// Queue a text frame.
    pub fn send(&self, text: String) -> Result<(), SendFail> {
        self.tx.try_send(Outbound::Text(text)).map_err(|e| match e {
            TrySendError::Full(_) => SendFail::Full,
            TrySendError::Closed(_) => SendFail::Closed,
        })
    }

    /// Close after the frames already queued; abort at once if the queue is full (a stuck peer).
    pub fn close(self) {
        if self.tx.try_send(Outbound::Close).is_err() {
            self.abort();
        }
    }

    /// Drop the socket now, discarding queued frames.
    pub fn abort(self) {
        self.reader.abort();
        self.writer.abort();
    }

    /// Queue a text frame, waiting for room.
    pub async fn send_wait(&self, text: String) -> Result<(), SendFail> {
        self.tx.send(Outbound::Text(text)).await.map_err(|_| SendFail::Closed)
    }

    /// The close code the peer sent, once `rx` has ended.
    pub fn close_code(&self) -> Option<u16> {
        match self.close_code.load(Ordering::SeqCst) {
            0 => None,
            c => u16::try_from(c).ok(),
        }
    }

    /// Separate the frames from the peer from the sending side.
    pub fn split(self) -> (mpsc::Receiver<String>, WsTx) {
        (self.rx, WsTx { tx: self.tx, reader: self.reader, writer: self.writer })
    }
}

impl WsTx {
    pub fn send(&self, text: String) -> Result<(), SendFail> {
        self.tx.try_send(Outbound::Text(text)).map_err(|e| match e {
            TrySendError::Full(_) => SendFail::Full,
            TrySendError::Closed(_) => SendFail::Closed,
        })
    }

    /// Free slots in the outbound queue.
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// Wait until the outbound queue has room; false once the socket is gone.
    pub async fn room(&self) -> bool {
        self.tx.reserve().await.is_ok()
    }

    /// Close after the frames already queued; abort at once if the queue is full (a stuck peer).
    pub fn close(self) {
        if self.tx.try_send(Outbound::Close).is_err() {
            self.abort();
        }
    }

    /// Drop the socket now, discarding queued frames.
    pub fn abort(self) {
        self.reader.abort();
        self.writer.abort();
    }
}

pub(crate) enum Classified {
    Text(String),
    Control,
    /// Closed (with the peer's close code, if it sent one) or a binary frame.
    End(Option<u16>),
}

pub(crate) trait WsMessage: Sized + Send + 'static {
    fn text(s: String) -> Self;
    fn close() -> Self;
    fn classify(self) -> Classified;
}

impl WsMessage for axum::extract::ws::Message {
    fn text(s: String) -> Self {
        axum::extract::ws::Message::Text(s.into())
    }
    fn close() -> Self {
        axum::extract::ws::Message::Close(None)
    }
    fn classify(self) -> Classified {
        use axum::extract::ws::Message as M;
        match self {
            M::Text(t) => Classified::Text(t.as_str().to_owned()),
            M::Ping(_) | M::Pong(_) => Classified::Control,
            M::Close(Some(f)) => Classified::End(Some(f.code)),
            // Binary frames are not part of the protocol.
            M::Binary(_) | M::Close(None) => Classified::End(None),
        }
    }
}

impl WsMessage for tokio_tungstenite::tungstenite::Message {
    fn text(s: String) -> Self {
        tokio_tungstenite::tungstenite::Message::text(s)
    }
    fn close() -> Self {
        tokio_tungstenite::tungstenite::Message::Close(None)
    }
    fn classify(self) -> Classified {
        use tokio_tungstenite::tungstenite::Message as M;
        match self {
            M::Text(t) => Classified::Text(t.as_str().to_owned()),
            M::Ping(_) | M::Pong(_) => Classified::Control,
            M::Close(Some(f)) => Classified::End(Some(f.code.into())),
            M::Binary(_) | M::Close(None) | M::Frame(_) => Classified::End(None),
        }
    }
}

/// Split a WebSocket into a reader task and a writer task joined by channels.
pub(crate) fn spawn_io<S, M, E>(ws: S) -> WsIo
where
    S: Stream<Item = Result<M, E>> + Sink<M> + Send + Unpin + 'static,
    M: WsMessage,
    E: Send + 'static,
    <S as Sink<M>>::Error: Send,
{
    let (mut sink, mut stream) = ws.split();
    let (in_tx, in_rx) = mpsc::channel::<String>(INBOUND_QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(OUTBOUND_QUEUE);
    let close_code = Arc::new(AtomicU32::new(0));

    let code = close_code.clone();
    let mut reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg.classify() {
                Classified::Text(t) => {
                    if in_tx.send(t).await.is_err() {
                        break;
                    }
                }
                Classified::Control => {}
                Classified::End(c) => {
                    code.store(c.map_or(0, u32::from), Ordering::SeqCst);
                    break;
                }
            }
        }
    });

    let reader_abort = reader.abort_handle();
    let writer = tokio::spawn(async move {
        let mut reader_done = false;
        loop {
            let out = if reader_done {
                // The reader is gone: the peer closed, or our side stopped reading (a link that queued
                // its bye and close, then dropped `rx`). Flush what is already queued, then stop; if the
                // peer is gone the first write fails.
                match out_rx.try_recv() {
                    Ok(o) => Some(o),
                    Err(_) => break,
                }
            } else {
                tokio::select! {
                    // Queued frames first: a bye must not lose a race against the reader ending.
                    biased;
                    out = out_rx.recv() => out,
                    _ = &mut reader => {
                        reader_done = true;
                        continue;
                    }
                }
            };
            match out {
                Some(Outbound::Text(s)) => match tokio::time::timeout(WRITE_TIMEOUT, sink.send(M::text(s))).await {
                    Ok(Ok(())) => {}
                    _ => break,
                },
                Some(Outbound::Close) | None => {
                    let _ = tokio::time::timeout(CLOSE_GRACE, sink.send(M::close())).await;
                    break;
                }
            }
        }
        let _ = tokio::time::timeout(CLOSE_GRACE, sink.close()).await;
        if !reader.is_finished() && tokio::time::timeout(CLOSE_GRACE, &mut reader).await.is_err() {
            reader.abort();
        }
        // Dropping both halves closes the TCP socket.
    });

    WsIo { rx: in_rx, tx: out_tx, reader: reader_abort, writer: writer.abort_handle(), close_code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_frames_are_sent_even_if_the_reader_ends_first() {
        // A link queues its bye and close, then drops `rx` while the peer keeps talking: the reader
        // ends on the next peer frame, and the bye must still go out before the close.
        for _ in 0..20 {
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            let server = WebSocketStream::from_raw_socket(ours, Role::Server, None).await;
            let mut peer = WebSocketStream::from_raw_socket(theirs, Role::Client, None).await;
            let io = spawn_io(server);
            peer.send(Message::text("hi")).await.unwrap();
            let (rx, tx) = io.split();
            tx.send("bye".into()).unwrap();
            tx.close();
            drop(rx);
            for _ in 0..5 {
                let _ = peer.send(Message::text("more")).await;
            }
            let mut got_bye = false;
            while let Some(Ok(m)) = peer.next().await {
                match m {
                    Message::Text(t) if t.as_str() == "bye" => got_bye = true,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            assert!(got_bye, "the bye was dropped");
        }
    }
}
