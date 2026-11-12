//! The Request runner: runs one request of one Application in a context of
//! its own, and answers what the Application answered, or that it failed.
//!
//! Interface: [`RequestRunner::start`] once, then [`RequestRunner::run`]
//! per request with the request in flight ([`InFlight`]: whose request,
//! for which Application), the Application, and the request as the
//! Application should see it. It answers an [`Outcome`].
//!
//! Every request gets a new V8 isolate and context: the Application's
//! script runs from its top level for that request, and everything the
//! context holds is dropped with it when the request is over. Nothing
//! survives from one request to the next.
//!
//! Contexts run on a fixed set of engine threads. Each thread runs many
//! requests at once, interleaved on its event loop: a request waiting (a
//! timer, a host call) gives the thread to the others, as the runtime takes
//! the isolate's lock per turn and lets it go before it waits. So requests
//! for different Users are served at the same time, on one thread and
//! across threads.

mod glue;
mod host;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use openworkers_core::{
    BindingInfo, Event, HttpRequest, HttpResponse, OperationsHandle, ResponseBody, RuntimeLimits,
    Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;

use crate::applications::Application;
use crate::gateway::Gateway;
use crate::user::User;
use host::{APPLICATION_BINDING, HostOps};

/// Whose request is in flight, for which Application. Fixed when the
/// request's context is made; the host speaks for it, and for nothing else.
#[derive(Debug)]
pub struct InFlight {
    /// The request's id, as in `x-vm-request-id` and the log.
    pub request: Arc<str>,
    pub user: User,
    pub application: Arc<str>,
}

/// What the Application answered.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// How a request ended.
#[derive(Debug)]
pub enum Outcome {
    Answered(Answer),
    /// The Application failed to load, threw, rejected, answered with
    /// something that is not a Response, or ran out of time or memory. The
    /// reason is in the log; the User gets the platform's fixed 500.
    Failed,
    /// Every engine thread's queue is full. Nothing of the Application ran.
    Busy,
}

/// The limits of the runner. Each is a bound on memory or time.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Engine threads.
    pub threads: NonZeroUsize,
    /// Requests one engine thread runs at once. Each holds an isolate.
    pub requests_per_thread: usize,
    /// Requests waiting for an engine thread, per thread. When every queue
    /// is full, a request is answered [`Outcome::Busy`] at once.
    pub queue_per_thread: usize,
    /// The longest a request may take, waitUntil work included.
    pub deadline: Duration,
    /// CPU time a request may use.
    pub cpu_ms: u64,
    /// The largest heap of one context.
    pub heap_max_mb: usize,
    /// The largest response body; past it the request fails.
    pub max_body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        let threads = std::thread::available_parallelism()
            .map_or(2, NonZeroUsize::get)
            .clamp(2, 8);
        Self {
            threads: NonZeroUsize::new(threads).unwrap_or(NonZeroUsize::MIN),
            requests_per_thread: 32,
            queue_per_thread: 64,
            deadline: Duration::from_secs(8),
            cpu_ms: 2_000,
            heap_max_mb: 64,
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

/// One request handed to an engine thread.
struct Job {
    in_flight: Arc<InFlight>,
    application: Arc<Application>,
    request: HttpRequest,
    reply: oneshot::Sender<Outcome>,
}

pub struct RequestRunner {
    lanes: Vec<mpsc::Sender<Job>>,
    next: AtomicUsize,
    deadline: Duration,
    /// The engine threads. They run until the program ends; the program is
    /// stopped by killing it (blackbox/CONTRACT.md), so they are never
    /// joined, but they are kept rather than detached.
    _threads: Vec<JoinHandle<()>>,
}

impl RequestRunner {
    /// Starts the engine threads.
    pub fn start(limits: &Limits, gateway: &Arc<dyn Gateway>) -> std::io::Result<Self> {
        let mut lanes = Vec::with_capacity(limits.threads.get());
        let mut threads = Vec::with_capacity(limits.threads.get());
        for index in 0..limits.threads.get() {
            // Bound: `queue_per_thread` requests waiting for this thread.
            // When full, `run` tries the next thread, and answers Busy when
            // every thread's queue is full.
            let (sender, receiver) = mpsc::channel(limits.queue_per_thread.max(1));
            let engine = Engine {
                limits: limits.clone(),
                gateway: Arc::clone(gateway),
            };
            let thread = std::thread::Builder::new()
                .name(format!("vm-engine-{index}"))
                .spawn(move || engine.run(receiver))?;
            lanes.push(sender);
            threads.push(thread);
        }
        Ok(Self {
            lanes,
            next: AtomicUsize::new(0),
            deadline: limits.deadline,
            _threads: threads,
        })
    }

    /// Runs one request to its end.
    pub async fn run(
        &self,
        in_flight: Arc<InFlight>,
        application: Arc<Application>,
        request: HttpRequest,
    ) -> Outcome {
        let (reply, answer) = oneshot::channel();
        let mut job = Job {
            in_flight,
            application,
            request,
            reply,
        };
        let first = self.next.fetch_add(1, Ordering::Relaxed);
        let mut queued = false;
        for offset in 0..self.lanes.len() {
            let Some(lane) = self
                .lanes
                .get(first.wrapping_add(offset) % self.lanes.len())
            else {
                continue;
            };
            match lane.try_send(job) {
                Ok(()) => {
                    queued = true;
                    break;
                }
                Err(
                    mpsc::error::TrySendError::Full(back) | mpsc::error::TrySendError::Closed(back),
                ) => job = back,
            }
        }
        if !queued {
            return Outcome::Busy;
        }
        // The engine enforces the deadline itself; this wait is the
        // backstop, with time for the engine to report.
        match tokio::time::timeout(self.deadline + Duration::from_secs(1), answer).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => Outcome::Failed,
        }
    }
}

/// One engine thread.
struct Engine {
    limits: Limits,
    gateway: Arc<dyn Gateway>,
}

impl Engine {
    fn run(self, mut jobs: mpsc::Receiver<Job>) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                crate::log::record(
                    "engine-failed",
                    serde_json::json!({ "error": error.to_string() }),
                );
                return;
            }
        };
        let local = tokio::task::LocalSet::new();
        let engine = Arc::new(self);
        local.block_on(&runtime, async move {
            // Bound: `requests_per_thread` requests run at once on this
            // thread. When all are taken, the thread stops taking jobs and
            // they wait in its queue.
            let permits = Arc::new(Semaphore::new(engine.limits.requests_per_thread.max(1)));
            // Every running request is owned here; finished ones are
            // reaped as they end.
            let mut running = JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = running.join_next(), if !running.is_empty() => {}
                    job = jobs.recv() => {
                        let Some(job) = job else { break };
                        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else { break };
                        let engine = Arc::clone(&engine);
                        running.spawn_local(async move {
                            let outcome = engine.serve(job.in_flight, &job.application, job.request).await;
                            // The waiter may have given up; then nobody needs it.
                            job.reply.send(outcome).ok();
                            drop(permit);
                        });
                    }
                }
            }
            while running.join_next().await.is_some() {}
        });
    }

    async fn serve(
        &self,
        in_flight: Arc<InFlight>,
        application: &Application,
        request: HttpRequest,
    ) -> Outcome {
        let host = Arc::new(HostOps::new(
            Arc::clone(&in_flight),
            Arc::clone(&self.gateway),
        ));
        let deadline = self.limits.deadline;
        match tokio::time::timeout(deadline, self.serve_in(&host, application, request)).await {
            Ok(outcome) => {
                host.end();
                outcome
            }
            Err(_) => {
                host.end();
                host.uncaught(&format!(
                    "the request did not finish within {} ms",
                    deadline.as_millis()
                ));
                Outcome::Failed
            }
        }
    }

    async fn serve_in(
        &self,
        host: &Arc<HostOps>,
        application: &Application,
        request: HttpRequest,
    ) -> Outcome {
        let script = Script::with_bindings(
            glue::script(&application.script),
            None,
            vec![BindingInfo::invoke(APPLICATION_BINDING)],
        );
        let limits = RuntimeLimits {
            heap_max_mb: self.limits.heap_max_mb,
            max_cpu_time_ms: self.limits.cpu_ms,
            max_wall_clock_time_ms: u64::try_from(self.limits.deadline.as_millis())
                .unwrap_or(u64::MAX),
            ..RuntimeLimits::default()
        };
        let ops: OperationsHandle = Arc::clone(host) as OperationsHandle;
        // The context is made, and the script's top level runs, with no
        // request in flight.
        let mut worker = match Worker::new_with_ops(script, Some(limits), ops).await {
            Ok(worker) => worker,
            Err(reason) => {
                host.uncaught(&describe(&reason));
                return Outcome::Failed;
            }
        };
        let (event, response) = Event::fetch(request);
        host.begin();
        let (ran, answer) = tokio::join!(
            worker.exec(event),
            collect(response, self.limits.max_body_bytes)
        );
        host.end();
        // The context, and all it holds, goes now.
        drop(worker);
        if host.failed() {
            return Outcome::Failed;
        }
        match (ran, answer) {
            (_, Ok(answer)) => Outcome::Answered(answer),
            (Err(reason), Err(_)) => {
                host.uncaught(&describe(&reason));
                Outcome::Failed
            }
            (Ok(()), Err(error)) => {
                host.uncaught(&error);
                Outcome::Failed
            }
        }
    }
}

fn describe(reason: &TerminationReason) -> String {
    match reason {
        TerminationReason::Exception(message) => message.clone(),
        TerminationReason::CpuTimeLimit => "the request used more CPU time than it may".to_owned(),
        TerminationReason::WallClockTimeout => "the request took longer than it may".to_owned(),
        TerminationReason::MemoryLimit => "the request used more memory than it may".to_owned(),
        other => format!("the request could not run: {other:?}"),
    }
}

/// The Application's response, its body read whole up to `max_body_bytes`.
async fn collect(
    response: oneshot::Receiver<HttpResponse>,
    max_body_bytes: usize,
) -> Result<Answer, String> {
    let response = response
        .await
        .map_err(|_| "the Application gave no response".to_owned())?;
    let body = match response.body {
        ResponseBody::None => Bytes::new(),
        ResponseBody::Bytes(bytes) => {
            if bytes.len() > max_body_bytes {
                return Err(format!(
                    "the response body is larger than {max_body_bytes} bytes"
                ));
            }
            bytes
        }
        ResponseBody::Stream(mut chunks) => {
            let mut body = BytesMut::new();
            while let Some(chunk) = chunks.recv().await {
                let chunk = chunk.map_err(|e| format!("the response body failed: {e}"))?;
                if body.len() + chunk.len() > max_body_bytes {
                    return Err(format!(
                        "the response body is larger than {max_body_bytes} bytes"
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            body.freeze()
        }
    };
    Ok(Answer {
        status: response.status,
        headers: response.headers,
        body,
    })
}
