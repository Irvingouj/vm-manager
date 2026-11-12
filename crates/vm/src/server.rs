//! The listening socket: accepts HTTP/1.1 connections and hands each
//! request to [`Ingress`].

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 1024,
            header_read: Duration::from_secs(10),
            connection: Duration::from_secs(60),
        }
    }
}

/// Serves until the process ends.
pub async fn serve(listener: TcpListener, ingress: Arc<Ingress>, limits: Limits) {
    // Bound: `limits.connections` connections at once. Accepting waits for
    // a free one.
    let permits = Arc::new(Semaphore::new(limits.connections.max(1)));
    // Every connection's task is owned here and reaped as it ends.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            permit = Arc::clone(&permits).acquire_owned() => {
                let Ok(permit) = permit else { break };
                let (stream, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        log::record("accept-failed", serde_json::json!({ "error": error.to_string() }));
                        // An error such as too many open files passes; do
                        // not spin on it.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let ingress = Arc::clone(&ingress);
                let header_read = limits.header_read;
                let connection_limit = limits.connection;
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let ingress = Arc::clone(&ingress);
                        async move { Ok::<_, Infallible>(ingress.handle(request).await) }
                    });
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(header_read);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    // A connection past its time is dropped; a client that
                    // went away needs nothing more.
                    tokio::time::timeout(connection_limit, connection).await.ok();
                    drop(permit);
                });
            }
        }
    }
}
