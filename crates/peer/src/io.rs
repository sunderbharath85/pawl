//! WebSocket plumbing shared by the listener (axum) and the dialer (tokio-tungstenite): both sockets
//! become a pair of channels carrying text frames, so the link logic is written once.

use futures::{Sink, SinkExt, Stream, StreamExt};
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
}

pub(crate) enum Classified {
    Text(String),
    Control,
    End,
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
            // Binary frames are not part of the protocol.
            M::Binary(_) | M::Close(_) => Classified::End,
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
            M::Binary(_) | M::Close(_) | M::Frame(_) => Classified::End,
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

    let mut reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg.classify() {
                Classified::Text(t) => {
                    if in_tx.send(t).await.is_err() {
                        break;
                    }
                }
                Classified::Control => {}
                Classified::End => break,
            }
        }
    });

    let reader_abort = reader.abort_handle();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                out = out_rx.recv() => match out {
                    Some(Outbound::Text(s)) => {
                        match tokio::time::timeout(WRITE_TIMEOUT, sink.send(M::text(s))).await {
                            Ok(Ok(())) => {}
                            _ => break,
                        }
                    }
                    Some(Outbound::Close) | None => {
                        let _ = tokio::time::timeout(CLOSE_GRACE, sink.send(M::close())).await;
                        break;
                    }
                },
                // The peer closed (or broke protocol): stop writing.
                _ = &mut reader => break,
            }
        }
        let _ = tokio::time::timeout(CLOSE_GRACE, sink.close()).await;
        if !reader.is_finished() && tokio::time::timeout(CLOSE_GRACE, &mut reader).await.is_err() {
            reader.abort();
        }
        // Dropping both halves closes the TCP socket.
    });

    WsIo { rx: in_rx, tx: out_tx, reader: reader_abort, writer: writer.abort_handle() }
}
