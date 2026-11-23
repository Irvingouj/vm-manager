//! The listening sockets: accept HTTP/1.1 connections and hand each request
//! to a [`Handler`]: [`Ingress`] for `vibe serve`, the local run's own front
//! for `vibe dev` (crate::tool's `local`), which hands the Application's
//! requests on to its own [`Ingress`].
//!
//! **Bounds.** At most [`Limits::connections`] connections at once, over
//! every listener (more wait in the system's accept backlog); a request's
//! head within [`Limits::header_read`], and hyper's own bound on its size
//! (about 400 KiB, at most 100 headers); a connection open at most
//! [`Limits::connection`], except while a body is being sent on it (the
//! contract: the limit on one connection's life does not cut a body being
//! sent). A request's body is the handler's to bound.
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
//!
//! **A failed accept** (too many open files, say) is tried again 50 ms
//! later, and logged once per cause while it lasts ([`AcceptFailures`]).

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::sent::{self, Sent};

/// The limits of the server.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Connections served at once. When all are taken, new connections
    /// wait in the operating system's accept backlog.
    pub connections: usize,
    /// The longest a client may take to send a request's head.
    pub header_read: Duration,
    /// The longest one connection may stay open, while no body is being
    /// sent on it; a body being sent suspends the limit, which starts again
    /// when the sending has ended (blackbox/CONTRACT.md, "What an
    /// Application is handed").
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

/// What answers each request. It answers a [`Sent`] body: whole for every
/// body the program has in hand, streamed for a body the host itself feeds
/// (blackbox/CONTRACT.md, "What an Application is handed").
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Request<Incoming>) -> impl Future<Output = Response<Sent>> + Send;
}

impl Handler for Ingress {
    fn handle(&self, request: Request<Incoming>) -> impl Future<Output = Response<Sent>> + Send {
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
            let mut failures = AcceptFailures::default();
            loop {
                // A permit before accepting: the backlog holds the rest.
                let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                    return;
                };
                match listener.accept().await {
                    Ok((stream, _)) => {
                        failures.ended();
                        // The permit goes with the stream to the
                        // connection's task, which holds it until it ends,
                        // however it ends. Not sent: dropped here.
                        if accepted.send((stream, permit)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        drop(permit);
                        failures.failed(&error, Instant::now());
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
    /// time is over, the server stops, or a body being sent is ended early.
    async fn serve<S>(mut self, io: S)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let handler = self.handler;
        // The handler hears about the connection through this pair: a body
        // it sends as a stream marks itself here, and asks for the
        // connection to end (`cut`) when it must be ended early.
        let (control, watch) = sent::pair();
        let service = service_fn(move |mut request: Request<Incoming>| {
            request.extensions_mut().insert(control.clone());
            let handler = Arc::clone(&handler);
            async move { Ok::<_, Infallible>(handler.handle(request).await) }
        });
        let mut builder = http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(self.header_read);
        let connection = builder.serve_connection(TokioIo::new(io), service);
        tokio::pin!(connection);
        // One connection's life: `connection_limit` while nothing is being
        // sent. The time a body was being sent is not counted, and after the
        // sending the connection has its limit again as after any answer
        // (CONTRACT.md, "What an Application is handed"): the timer's branch
        // is off while `sending`, and it is reset whole when the sending
        // ends. A client that went away ends the connection's own future;
        // nothing more is needed.
        let limit = self.connection_limit;
        let life = tokio::time::sleep(limit);
        tokio::pin!(life);
        let mut sending = false;
        let mut stream_begins = watch.streaming.clone();
        let mut stream_ends = watch.streaming.clone();
        let mut cut = watch.cut;
        loop {
            let mut stopping = false;
            tokio::select! {
                _ = connection.as_mut() => return,
                // Fires when the server stops (or the signal's sender is
                // gone, which only happens when the server is ending).
                _ = self.stopped.wait_for(|stopped| *stopped) => {
                    stopping = true;
                }
                _ = cut.changed() => return,
                _ = stream_begins.wait_for(|streaming| *streaming), if !sending => {
                    sending = true;
                }
                _ = stream_ends.wait_for(|streaming| !*streaming), if sending => {
                    sending = false;
                    life.as_mut().reset(tokio::time::Instant::now() + limit);
                }
                _ = &mut life, if !sending => return,
            }
            if stopping {
                // The server stops: the request it serves is finished, and
                // no more are taken on this connection.
                connection.as_mut().graceful_shutdown();
                connection.as_mut().await.ok();
                return;
            }
        }
    }
}

/// How often a failure of `accept` that goes on is logged again, with the
/// count of failures since the last line.
pub const ACCEPT_FAILED_EVERY: Duration = Duration::from_secs(10);

/// The failures of one listener's `accept` and what of them was logged:
/// `accept-failed` once when a cause begins (`failures: 1`), then at most
/// once every [`ACCEPT_FAILED_EVERY`] while it lasts, with the failures
/// since the last line; the failures not yet said are said when the cause
/// changes or an accept succeeds again. Not twenty lines a second.
#[derive(Debug, Default)]
pub struct AcceptFailures {
    /// The cause going on (the error's words) and when it was last said.
    going_on: Option<(String, Instant)>,
    /// Failures of that cause since the last line.
    unsaid: u64,
}

impl AcceptFailures {
    /// An accept failed with `error` at `now`.
    pub fn failed(&mut self, error: &std::io::Error, now: Instant) {
        self.failed_saying(&error.to_string(), now, &mut say_accept_failed);
    }

    /// An accept succeeded: the failures not yet said are said.
    pub fn ended(&mut self) {
        self.ended_saying(&mut say_accept_failed);
    }

    fn failed_saying(&mut self, cause: &str, now: Instant, say: &mut impl FnMut(&str, u64)) {
        match &mut self.going_on {
            Some((going, said_at)) if going == cause => {
                self.unsaid = self.unsaid.saturating_add(1);
                if now.saturating_duration_since(*said_at) >= ACCEPT_FAILED_EVERY {
                    say(cause, self.unsaid);
                    *said_at = now;
                    self.unsaid = 0;
                }
            }
            _ => {
                self.ended_saying(say);
                say(cause, 1);
                self.going_on = Some((cause.to_owned(), now));
            }
        }
    }

    fn ended_saying(&mut self, say: &mut impl FnMut(&str, u64)) {
        if let Some((cause, _)) = self.going_on.take()
            && self.unsaid > 0
        {
            say(&cause, self.unsaid);
        }
        self.unsaid = 0;
    }
}

fn say_accept_failed(cause: &str, failures: u64) {
    log::record(
        "accept-failed",
        serde_json::json!({ "error": cause, "failures": failures }),
    );
}

#[cfg(test)]
mod accept_failures_tests {
    use super::*;

    #[test]
    fn a_failure_that_goes_on_is_said_once_then_every_ten_seconds_with_a_count() {
        let mut failures = AcceptFailures::default();
        let mut said = Vec::new();
        let mut say = |cause: &str, n: u64| said.push((cause.to_owned(), n));
        let start = Instant::now();
        // Twenty a second for 25 seconds.
        for tick in 0..500_u64 {
            let now = start + Duration::from_millis(tick * 50);
            failures.failed_saying("Too many open files (os error 24)", now, &mut say);
        }
        failures.ended_saying(&mut say);
        let counts: Vec<u64> = said.iter().map(|(_, n)| *n).collect();
        // At 0 s, at 10 s (200 failures since), at 20 s (200), and the
        // last 99 when it ended.
        assert_eq!(counts, vec![1, 200, 200, 99]);
        assert_eq!(counts.iter().sum::<u64>(), 500);
    }

    #[test]
    fn another_cause_is_said_at_once() {
        let mut failures = AcceptFailures::default();
        let mut said = Vec::new();
        let mut say = |cause: &str, n: u64| said.push((cause.to_owned(), n));
        let now = Instant::now();
        failures.failed_saying("a", now, &mut say);
        failures.failed_saying("a", now, &mut say);
        failures.failed_saying("b", now, &mut say);
        failures.ended_saying(&mut say);
        failures.ended_saying(&mut say);
        assert_eq!(
            said,
            vec![
                ("a".to_owned(), 1),
                ("a".to_owned(), 1),
                ("b".to_owned(), 1)
            ]
        );
    }
}
