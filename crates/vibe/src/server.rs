//! The listening sockets: accept HTTP/1.1 connections and hand each request
//! to a [`Handler`]: [`Ingress`] for `vibe serve`, the local run's own front
//! for `vibe dev` (crate::tool's `local`), which hands the Application's
//! requests on to its own [`Ingress`].
//!
//! **Bounds.** At most [`Limits::connections`] connections at once, over
//! every listener (more wait in the system's accept backlog); a request's
//! head within [`Limits::header_read`], and hyper's own bound on its size
//! (about 400 KiB, at most 100 headers); a connection open at most
//! [`Limits::connection`]. A request's body is the handler's to bound.
//!
//! **TLS** ([`Tls`]): when the program serves TLS itself, each accepted
//! connection's handshake is made in its own task, holding its permit like
//! any other connection, and is ended with the connection when it is not
//! finished within [`Tls::handshake`]. Bytes that are no handshake end it
//! at once, with no HTTP answer.
//!
//! **Stopping** ([`serve_until`]): when its `stop` future ends, no
//! connection is accepted any more, every connection is asked to end once
//! the request it serves is answered, and those still open after
//! [`Limits::drain`] are dropped.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

use crate::ingress::Ingress;
use crate::log;

/// The limits of the server.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Connections served at once. When all are taken, new connections
    /// wait in the operating system's accept backlog.
    pub connections: usize,
    /// The longest a client may take to send a request's head.
    pub header_read: Duration,
    /// The longest one connection may stay open.
    pub connection: Duration,
    /// Once stopping: the longest the connections open then are given to
    /// finish the requests they serve.
    pub drain: Duration,
}

/// The connections `vibe serve` serves at once ([`Limits::connections`] by
/// default). The bound on sessions renewed at once
/// (crate::oidc::sessions::MAX_RENEWALS) is a quarter of it, derived from
/// it: raising one moves the other.
pub const CONNECTIONS: usize = 1024;

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: CONNECTIONS,
            header_read: Duration::from_secs(10),
            connection: Duration::from_secs(60),
            drain: Duration::from_secs(10),
        }
    }
}

/// The program's own TLS on every listener.
pub struct Tls {
    /// Its configuration, shared by every connection.
    pub acceptor: TlsAcceptor,
    /// The longest a handshake may take from the connection's accept.
    pub handshake: Duration,
}

/// What answers each request.
pub trait Handler: Send + Sync + 'static {
    fn handle(
        &self,
        request: Request<Incoming>,
    ) -> impl Future<Output = Response<Full<Bytes>>> + Send;
}

impl Handler for Ingress {
    fn handle(
        &self,
        request: Request<Incoming>,
    ) -> impl Future<Output = Response<Full<Bytes>>> + Send {
        Ingress::handle(self, request)
    }
}

/// Serves until the process ends: HTTPS when `tls` is given, plain HTTP
/// otherwise.
pub async fn serve(listener: TcpListener, ingress: Arc<Ingress>, limits: Limits, tls: Option<Tls>) {
    serve_until(vec![listener], ingress, limits, tls, std::future::pending()).await;
}

/// Serves on every listener until `stop` ends, then stops (see the
/// module's text) and returns once every connection has ended.
pub async fn serve_until<H: Handler>(
    listeners: Vec<TcpListener>,
    handler: Arc<H>,
    limits: Limits,
    tls: Option<Tls>,
    stop: impl Future<Output = ()>,
) {
    // Bound: `limits.connections` connections at once, over every
    // listener. Accepting waits for a free one.
    let permits = Arc::new(Semaphore::new(limits.connections.max(1)));
    // Told once: every connection then ends after its request.
    let (stopping, stopped) = watch::channel(false);
    // Every connection's task is owned here and reaped as it ends.
    let mut connections = JoinSet::new();
    // One accepting task per listener, owned here, each sending what it
    // accepted, with its permit, through a channel of one: an accepted
    // connection waits there until the loop below takes it.
    let (accepted, mut incoming) =
        tokio::sync::mpsc::channel::<(TcpStream, OwnedSemaphorePermit)>(1);
    let mut accepting = JoinSet::new();
    // Shared by every connection's task.
    let tls = tls.map(Arc::new);
    for listener in listeners {
        let accepted = accepted.clone();
        let permits = Arc::clone(&permits);
        accepting.spawn(async move {
            loop {
                // A permit before accepting: the backlog holds the rest.
                let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                    return;
                };
                match listener.accept().await {
                    Ok((stream, _)) => {
                        // The permit goes with the stream to the
                        // connection's task, which holds it until it ends,
                        // however it ends. Not sent: dropped here.
                        if accepted.send((stream, permit)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        drop(permit);
                        log::record(
                            "accept-failed",
                            serde_json::json!({ "error": error.to_string() }),
                        );
                        // An error such as too many open files passes; do
                        // not spin on it.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        });
    }
    drop(accepted);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            stream = incoming.recv() => {
                let Some((stream, permit)) = stream else { break };
                let handler = Arc::clone(&handler);
                let header_read = limits.header_read;
                let connection_limit = limits.connection;
                let stopped = stopped.clone();
                let tls = tls.as_ref().map(Arc::clone);
                connections.spawn(async move {
                    let served = Connection { handler, header_read, connection_limit, stopped };
                    match tls {
                        None => served.serve(stream).await,
                        Some(tls) => {
                            // A handshake not finished in time, or that
                            // fails, ends the connection: the stream is
                            // dropped, which closes it.
                            if let Ok(Ok(stream)) =
                                tokio::time::timeout(tls.handshake, tls.acceptor.accept(stream)).await
                            {
                                served.serve(stream).await;
                            }
                        }
                    }
                    // Given back here, or when the task is dropped.
                    drop(permit);
                });
            }
        }
    }
    // No connection is taken any more.
    accepting.shutdown().await;
    drop(incoming);
    stopping.send(true).ok();
    let drained = tokio::time::timeout(limits.drain, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        // Those still open are dropped with their tasks.
        connections.shutdown().await;
    }
}

/// What one connection is served with.
struct Connection<H> {
    handler: Arc<H>,
    header_read: Duration,
    connection_limit: Duration,
    stopped: watch::Receiver<bool>,
}

impl<H: Handler> Connection<H> {
    /// Serves HTTP/1.1 on `io` until the client is done, the connection's
    /// time is over, or the server stops.
    async fn serve<S>(mut self, io: S)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let handler = self.handler;
        let service = service_fn(move |request| {
            let handler = Arc::clone(&handler);
            async move { Ok::<_, Infallible>(handler.handle(request).await) }
        });
        let mut builder = http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(self.header_read);
        let connection = builder.serve_connection(TokioIo::new(io), service);
        tokio::pin!(connection);
        // A connection past its time is dropped; a client that went away
        // needs nothing more.
        let stopped = &mut self.stopped;
        tokio::time::timeout(self.connection_limit, async {
            tokio::select! {
                _ = connection.as_mut() => return,
                _ = stopped.wait_for(|stop| *stop) => {}
            }
            connection.as_mut().graceful_shutdown();
            connection.as_mut().await.ok();
        })
        .await
        .ok();
    }
}
