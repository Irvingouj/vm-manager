//! What goes over the wire (blackbox/CONTRACT.md, "Outbound calls: What
//! goes over the wire"): the connection to an address that was checked,
//! TLS to the host's name (or address), HTTP/1.1 through hyper, and the
//! request's body as a body hyper sends.
//!
//! **The address checked is the address connected.** A name is resolved
//! here, once, to at most 16 addresses; all of them are judged
//! ([`super::address::refuses`]) before one is connected to, and the
//! connection is made to one of those `SocketAddr`s, never to the name.
//!
//! **Bounds.** Resolving and connecting (TLS included) take at most
//! [`CONNECT_TIME`]; a body being uploaded that takes no byte for
//! [`STALL`] is failed.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use openworkers_runtime_v8::runtime::stream_manager::StreamChunk;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::address;
use super::limits::NotResolved;
use super::target::{Target, Where};
use super::{PrivateRanges, Program};

/// The longest resolving the name and connecting, TLS included, may take.
pub const CONNECT_TIME: Duration = Duration::from_secs(10);

/// How long a body in either direction may take no byte.
pub const STALL: Duration = Duration::from_secs(30);

/// The most bytes of a body each way.
pub const MOST_BODY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// The most headers of an answer, and the most bytes of them together.
pub const MOST_HEADERS: usize = 100;
pub const MOST_HEADER_BYTES: usize = 64 * 1024;

/// Why a connection was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotConnected {
    /// The name did not resolve.
    Dns,
    /// No connection within its time, or refused.
    Connect,
    /// The certificate did not hold, or the other side does not speak TLS.
    Tls,
    /// An address of the name is never called.
    NotAllowed,
    /// The 30 seconds of the call ran out meanwhile.
    Time,
}

/// The connection, to an address that was checked, up to its TLS.
///
/// `overall` is when the call's own 30 seconds end.
pub async fn connect(
    target: &Target,
    private: &PrivateRanges,
    program: &Program,
    application: &str,
    tls: &Arc<ClientConfig>,
    overall: Instant,
) -> Result<TlsStream<TcpStream>, NotConnected> {
    let by = (Instant::now() + CONNECT_TIME).min(overall);
    let late = |fail| {
        if Instant::now() >= overall {
            NotConnected::Time
        } else {
            fail
        }
    };
    let addresses: Vec<IpAddr> = match &target.host {
        Where::Address(address) => vec![*address],
        Where::Name(name) => program
            .resolve(application, name, target.port, address::MOST_ADDRESSES, by)
            .await
            .map_err(|why| match why {
                NotResolved::Time => late(NotConnected::Dns),
                NotResolved::Failed => NotConnected::Dns,
            })?,
    };
    if addresses.is_empty() {
        return Err(NotConnected::Dns);
    }
    if address::refuses(&addresses, private) {
        return Err(NotConnected::NotAllowed);
    }
    let server_name = match &target.host {
        Where::Address(address) => ServerName::IpAddress((*address).into()),
        Where::Name(name) => ServerName::try_from(name.clone()).map_err(|_| NotConnected::Tls)?,
    };
    let connector = TlsConnector::from(Arc::clone(tls));
    tokio::time::timeout_at(by, async {
        let mut last = NotConnected::Connect;
        for address in &addresses {
            let socket = SocketAddr::new(*address, target.port);
            match TcpStream::connect(socket).await {
                Ok(stream) => {
                    // A call is a request and its answer: nothing waits
                    // for more to send.
                    drop(stream.set_nodelay(true));
                    return connector
                        .connect(server_name.clone(), stream)
                        .await
                        .map_err(|_| NotConnected::Tls);
                }
                Err(_) => last = NotConnected::Connect,
            }
        }
        Err(last)
    })
    .await
    .map_err(|_| late(NotConnected::Connect))?
}

/// How the exchange ended before an answer's headers came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failed {
    /// The other side does not speak HTTP/1.1, or went away.
    Protocol,
    /// The answer's headers are more than 100 or 64 KiB.
    Headers,
    /// The call's own body failed before its end.
    BodyFailed,
    /// The call's own body was more than 5 GiB.
    BodyTooLarge,
}

/// What `Payload::failed` holds: nothing happened to the body.
const UPLOAD_WELL: u8 = 0;
/// The body failed on this side (stalled, or its source failed).
const UPLOAD_FAILED: u8 = 1;
/// The body passed [`MOST_BODY_BYTES`].
const UPLOAD_TOO_LARGE: u8 = 2;

/// The answer's status and headers, and what remains to be read.
pub struct Head {
    pub response: Response<Incoming>,
    /// The connection, driven by whoever reads the body.
    pub connection: Connection,
}

/// The HTTP connection hyper drives, boxed.
pub type Connection =
    Pin<Box<hyper::client::conn::http1::Connection<TokioIo<TlsStream<TcpStream>>, Payload>>>;

/// Sends `request` on `stream` and waits for the answer's headers. The
/// connection is driven here beside the answer; it is handed back with the
/// head, to be driven beside the body.
pub async fn exchange(
    stream: TlsStream<TcpStream>,
    request: Request<Payload>,
    uploaded: &Arc<AtomicU8>,
) -> Result<Head, Failed> {
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_headers(MOST_HEADERS)
        .max_buf_size(2 * MOST_HEADER_BYTES)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Failed::Protocol)?;
    let mut connection: Connection = Box::pin(connection);
    let sent = sender.send_request(request);
    tokio::pin!(sent);
    let mut over = false;
    let response = loop {
        tokio::select! {
            answer = &mut sent => break answer,
            ended = &mut connection, if !over => {
                over = true;
                // The connection ended before the answer came.
                if let Err(error) = ended {
                    return Err(failure(&error, uploaded));
                }
            }
        }
    };
    match response {
        Ok(response) => Ok(Head {
            response,
            connection,
        }),
        Err(error) => Err(failure(&error, uploaded)),
    }
}

fn failure(error: &hyper::Error, uploaded: &AtomicU8) -> Failed {
    let uploaded = uploaded.load(Ordering::SeqCst);
    if uploaded == UPLOAD_TOO_LARGE {
        Failed::BodyTooLarge
    } else if uploaded == UPLOAD_FAILED {
        Failed::BodyFailed
    } else if error.is_parse_too_large() {
        Failed::Headers
    } else {
        Failed::Protocol
    }
}

/// The request's body, as hyper sends it.
pub struct Payload {
    source: Source,
    /// Set when the body failed on this side.
    failed: Arc<AtomicU8>,
}

enum Source {
    Empty,
    Whole(Option<Bytes>),
    Stream {
        chunks: mpsc::Receiver<StreamChunk>,
        length: Option<u64>,
        sent: u64,
        stall: Pin<Box<Sleep>>,
        ended: bool,
    },
}

impl Payload {
    /// A call with no body.
    pub fn empty() -> Self {
        Self {
            source: Source::Empty,
            failed: Arc::new(AtomicU8::new(UPLOAD_WELL)),
        }
    }

    /// A body in hand, sent with its `content-length`.
    pub fn whole(bytes: Bytes) -> Self {
        Self {
            source: Source::Whole(Some(bytes)),
            failed: Arc::new(AtomicU8::new(UPLOAD_WELL)),
        }
    }

    /// A body fed through `chunks`, chunked unless `length` is known.
    pub fn stream(chunks: mpsc::Receiver<StreamChunk>, length: Option<u64>) -> Self {
        Self {
            source: Source::Stream {
                chunks,
                length,
                sent: 0,
                stall: Box::pin(tokio::time::sleep(STALL)),
                ended: false,
            },
            failed: Arc::new(AtomicU8::new(UPLOAD_WELL)),
        }
    }

    /// Says whether the body failed on this side, once it did.
    pub fn failed(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.failed)
    }

    fn fail(&self, how: u8) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.failed.store(how, Ordering::SeqCst);
        Poll::Ready(Some(Err(io::Error::other("the call's body failed"))))
    }
}

impl Body for Payload {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match &mut this.source {
            Source::Empty => Poll::Ready(None),
            Source::Whole(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
            Source::Stream {
                chunks,
                sent,
                stall,
                ended,
                ..
            } => {
                if *ended {
                    return Poll::Ready(None);
                }
                match chunks.poll_recv(cx) {
                    Poll::Ready(Some(StreamChunk::Data(bytes))) => {
                        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        *sent = sent.saturating_add(size);
                        if *sent > MOST_BODY_BYTES {
                            *ended = true;
                            return this.fail(UPLOAD_TOO_LARGE);
                        }
                        stall.as_mut().reset(Instant::now() + STALL);
                        Poll::Ready(Some(Ok(Frame::data(bytes))))
                    }
                    Poll::Ready(Some(StreamChunk::Done)) => {
                        *ended = true;
                        Poll::Ready(None)
                    }
                    // An error, or a stream closed before its end.
                    Poll::Ready(Some(StreamChunk::Error(_)) | None) => {
                        *ended = true;
                        this.fail(UPLOAD_FAILED)
                    }
                    Poll::Pending => {
                        if stall.as_mut().poll(cx).is_ready() {
                            *ended = true;
                            return this.fail(UPLOAD_FAILED);
                        }
                        Poll::Pending
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.source {
            Source::Empty => true,
            Source::Whole(bytes) => bytes.is_none(),
            Source::Stream { ended, .. } => *ended,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.source {
            Source::Empty => SizeHint::with_exact(0),
            Source::Whole(bytes) => SizeHint::with_exact(
                bytes
                    .as_ref()
                    .map_or(0, |b| u64::try_from(b.len()).unwrap_or(0)),
            ),
            Source::Stream { length, sent, .. } => match length {
                Some(length) => SizeHint::with_exact(length.saturating_sub(*sent)),
                None => SizeHint::new(),
            },
        }
    }
}

use std::future::Future;
