//! The listening sockets: accept HTTP/1.1 connections and hand each request
//! to a [`Handler`]: [`Ingress`] for `vm serve`, the local run's own front
//! for `vm dev` (crate::tool's `local`), which hands the Application's
//! requests on to its own [`Ingress`].
//!
//! **Bounds.** At most [`Limits::connections`] connections at once, over
//! every listener (more wait in the system's accept backlog); a request's
//! head within [`Limits::header_read`], and hyper's own bound on its size
//! (about 400 KiB, at most 100 headers); a connection open at most
//! [`Limits::connection`]. A request's body is the handler's to bound.
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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;

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

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 1024,
            header_read: Duration::from_secs(10),
            connection: Duration::from_secs(60),
            drain: Duration::from_secs(10),
        }
    }
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

/// Serves until the process ends.
pub async fn serve(listener: TcpListener, ingress: Arc<Ingress>, limits: Limits) {
    serve_until(vec![listener], ingress, limits, std::future::pending()).await;
}

/// Serves on every listener until `stop` ends, then stops (see the
/// module's text) and returns once every connection has ended.
pub async fn serve_until<H: Handler>(
    listeners: Vec<TcpListener>,
    handler: Arc<H>,
    limits: Limits,
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
                let mut stopped = stopped.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let handler = Arc::clone(&handler);
                        async move { Ok::<_, Infallible>(handler.handle(request).await) }
                    });
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(header_read);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::pin!(connection);
                    // A connection past its time is dropped; a client that
                    // went away needs nothing more.
                    tokio::time::timeout(connection_limit, async {
                        tokio::select! {
                            _ = connection.as_mut() => return,
                            _ = stopped.wait_for(|stop| *stop) => {}
                        }
                        connection.as_mut().graceful_shutdown();
                        connection.as_mut().await.ok();
                    })
                    .await
                    .ok();
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
