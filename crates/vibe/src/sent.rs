//! A body the program sends to a client: whole in hand, or streamed.
//!
//! Every answer but one is whole: the Application's `Response` collected in
//! the runner, at most 8 MiB, the platform's own pages and JSON. The one
//! body that streams is a body the host itself feeds and the Application's
//! code did not read — today the request's own body given back
//! (`new Response(request.body)`) — sent from the connection to the client
//! chunk by chunk, after the code has ended if need be
//! (blackbox/CONTRACT.md, "What an Application is handed").
//!
//! **A body being sent is ended early, the connection closed without a clean
//! end**, when the client takes no byte for [`NO_BYTE`], when the Application
//! is disabled, when the client goes away (the connection's own write fails)
//! or when the program stops (the connection's task is dropped). A
//! [`SentStream`] owns the watchdog that watches the first two; the connection
//! is ended by [`SentControl::cut`], which [`crate::server`] selects on.
//!
//! **Ownership.** Every task here is owned: the watchdog and the body's
//! source pump (handed in as [`OwnedTask`]s) are aborted when the
//! [`SentStream`] is dropped, which happens when the connection has sent the
//! body, or has gone, or has been cut.
//!
//! **Bounds.** The chunks come from the runner through a bounded channel
//! (16 chunks, the runtime's `stream_buffer_size`), each chunk at most
//! 64 KiB (vendor/VENDORED.md, patch 7): at most about 1 MiB of one body is
//! held between the source and the connection, however large the body is.
//! The body is never collected here.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Frame, SizeHint};
use tokio::sync::{mpsc, watch};

use crate::registry::Stop;

/// How long a client may take no byte of a body being sent before its
/// connection is ended (blackbox/CONTRACT.md, "What an Application is
/// handed"). Counted from the last chunk handed to the connection's writes:
/// a client that has stopped reading stops taking chunks once the socket's
/// send buffer holds what was written, and the connection is cut [`NO_BYTE`]
/// after that.
pub const NO_BYTE: Duration = Duration::from_secs(30);

/// A body the program sends: whole, or streamed chunk by chunk.
#[derive(Debug)]
pub enum Sent {
    /// The whole body in hand: every answer but a host-fed one.
    Whole(Full<Bytes>),
    /// A body the host itself feeds, sent as its chunks come.
    Stream(SentStream),
}

impl From<Full<Bytes>> for Sent {
    fn from(whole: Full<Bytes>) -> Self {
        Sent::Whole(whole)
    }
}

impl Body for Sent {
    type Data = Bytes;
    // A whole body cannot fail (`Infallible`); a streamed one fails when its
    // source fails, and hyper then ends the connection without a clean end.
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            Sent::Whole(whole) => match Pin::new(whole).poll_frame(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
                Poll::Ready(Some(Err(never))) => match never {},
            },
            Sent::Stream(stream) => Pin::new(stream).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Sent::Whole(whole) => whole.is_end_stream(),
            Sent::Stream(stream) => stream.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Sent::Whole(whole) => whole.size_hint(),
            // Unknown: the connection chunk-encodes the body when the
            // platform set no `content-length`, which is what the contract
            // asks for a request body that stated none.
            Sent::Stream(_) => SizeHint::default(),
        }
    }
}

/// The body the host itself feeds, as the connection sends it.
#[derive(Debug)]
pub struct SentStream {
    /// The chunks, from the runner: at most 16 in the channel (the
    /// runtime's bound), each at most 64 KiB; an error ends the connection
    /// without a clean end.
    chunks: mpsc::Receiver<Result<Bytes, String>>,
    /// When the last chunk was handed to the connection's writes: the
    /// watchdog's clock (see [`NO_BYTE`]).
    progress: watch::Sender<Instant>,
    /// The connection this body is sent on, to say the sending has ended.
    control: Option<SentControl>,
    /// Tasks this body owns and ends with it: the watchdog, and the pump
    /// that feeds the body's source.
    owned: Vec<OwnedTask>,
}

impl SentStream {
    /// A body sent chunk by chunk. `owned` are the tasks that end with it
    /// (its source pump); with `control` and `stop`, the connection is
    /// watched and cut when the client takes no byte for [`NO_BYTE`] or the
    /// Application is disabled.
    pub fn new(
        chunks: mpsc::Receiver<Result<Bytes, String>>,
        control: Option<SentControl>,
        stop: Option<Arc<Stop>>,
        mut owned: Vec<OwnedTask>,
    ) -> Self {
        let (progress, clock) = watch::channel(Instant::now());
        if let Some(control) = &control {
            // The connection's life limit waits while this body is sent
            // (crate::server): said before the response is handed over.
            control.sending();
            if let Some(stop) = stop {
                owned.push(watchdog(control.clone(), stop, clock));
            }
        }
        Self {
            chunks,
            progress,
            control,
            owned,
        }
    }
}

impl Body for SentStream {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        match this.chunks.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(bytes))) => {
                // A change the watchdog sees; nothing waits on the send.
                let _ = this.progress.send_replace(Instant::now());
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Some(Err(_))) => Poll::Ready(Some(Err(io::Error::other(
                "the body being sent ended early",
            )))),
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }
}

impl Drop for SentStream {
    fn drop(&mut self) {
        if let Some(control) = &self.control {
            // The sending is over (sent, failed, or the connection went):
            // the connection's life limit starts again.
            control.done();
        }
        // End the tasks this body owns: the watchdog, and the pump that
        // feeds the body's source.
        self.owned.clear();
    }
}

impl std::fmt::Debug for SentControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SentControl")
    }
}

impl std::fmt::Debug for SentWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SentWatch")
    }
}

/// The connection's side of a body being sent, in the handler's hands (it
/// travels in the request's extensions; [`crate::server`] makes the pair per
/// connection). Both senders are `watch`es — one value, nothing queues.
#[derive(Clone)]
pub struct SentControl {
    /// A body of this connection is being sent.
    streaming: watch::Sender<bool>,
    /// A body being sent asked for the connection to end, now.
    cut: watch::Sender<u64>,
}

/// What the connection's task watches (see [`crate::server`]).
pub struct SentWatch {
    /// A body of this connection is being sent (the life limit waits).
    pub streaming: watch::Receiver<bool>,
    /// A body being sent asked for the connection to end.
    pub cut: watch::Receiver<u64>,
}

/// The control a handler is given and the watch its connection keeps.
pub fn pair() -> (SentControl, SentWatch) {
    let (streaming, streaming_rx) = watch::channel(false);
    let (cut, cut_rx) = watch::channel(0_u64);
    (
        SentControl { streaming, cut },
        SentWatch {
            streaming: streaming_rx,
            cut: cut_rx,
        },
    )
}

impl SentControl {
    /// A body starts being sent on this connection.
    fn sending(&self) {
        self.streaming.send_replace(true);
    }

    /// The body being sent has ended (or its connection has).
    fn done(&self) {
        self.streaming.send_replace(false);
    }

    /// Ends the connection without a clean end: the client can tell by the
    /// `content-length` it was promised.
    pub fn cut(&self) {
        self.cut.send_modify(|cuts| *cuts = cuts.wrapping_add(1));
    }
}

/// A task with an owner: dropped (and so aborted) with the value that holds
/// it (Rust rules, "Every spawned task has an owner").
#[derive(Debug)]
pub struct OwnedTask(tokio::task::JoinHandle<()>);

impl OwnedTask {
    pub fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(handle)
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Watches a body being sent: ends the connection when the Application is
/// disabled, or when the client takes no byte for [`NO_BYTE`]. Owned by the
/// [`SentStream`] it watches.
fn watchdog(
    control: SentControl,
    stop: Arc<Stop>,
    mut clock: watch::Receiver<Instant>,
) -> OwnedTask {
    OwnedTask::new(tokio::spawn(async move {
        loop {
            let last = *clock.borrow_and_update();
            let wait = (last + NO_BYTE).saturating_duration_since(Instant::now());
            tokio::select! {
                () = stop.stopped() => {
                    control.cut();
                    return;
                }
                changed = clock.changed() => {
                    // A chunk went out; or the body ended and the sender is
                    // gone, which means nobody needs this watchdog.
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wait) => {
                    // A chunk may have gone out as the sleep ended.
                    if *clock.borrow_and_update() > last {
                        continue;
                    }
                    control.cut();
                    return;
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn a_whole_body_is_the_whole_body() {
        let mut body = Sent::Whole(Full::new(Bytes::from_static(b"abc")));
        let collected = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(collected, Bytes::from_static(b"abc"));
        assert!(body.frame().await.is_none());
    }

    #[tokio::test]
    async fn a_streamed_body_sends_its_chunks_and_says_when_it_is_over() {
        let (tx, rx) = mpsc::channel(4);
        let stream = SentStream::new(rx, None, None, Vec::new());
        tx.send(Ok(Bytes::from_static(b"one"))).await.unwrap();
        tx.send(Ok(Bytes::from_static(b"two"))).await.unwrap();
        drop(tx);

        let mut body = Sent::Stream(stream);
        let one = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let two = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(
            (one, two),
            (Bytes::from_static(b"one"), Bytes::from_static(b"two"))
        );
        assert!(body.frame().await.is_none());
    }

    #[tokio::test]
    async fn a_failed_source_fails_the_body() {
        let (tx, rx) = mpsc::channel(4);
        let mut body = Sent::Stream(SentStream::new(rx, None, None, Vec::new()));
        tx.send(Err("the request's body stalled".to_owned()))
            .await
            .unwrap();
        let failed = body.frame().await.unwrap();
        assert!(failed.is_err());
    }
}
