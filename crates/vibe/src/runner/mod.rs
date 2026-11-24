//! The Request runner: runs one request of one Application in a context of
//! its own, and answers what the Application answered, or that it failed.
//!
//! Interface: [`RequestRunner::start`] once, then [`RequestRunner::run`]
//! per request with the request in flight ([`InFlight`]: whose request,
//! for which Application and Version), the serving Version to run
//! ([`Serving`], shared with the Application registry), and the request as
//! the Application should see it. It answers an [`Outcome`].
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use openworkers_core::{
    BindingInfo, Event, HttpRequest, HttpResponse, OperationsHandle, ResponseBody, RuntimeLimits,
    Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;
use openworkers_runtime_v8::runtime::stream_manager::StreamManager;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::app_logs::Collected;
use crate::gateway::{Gateway, Transactions};
use crate::registry::{Declared, Serving, Stop};
use crate::storage::bodies::Bodies;
use crate::user::User;
use host::{APPLICATION_BINDING, HostOps};

/// Whose request is in flight, for which Application. Fixed when the
/// request's context is made; the host speaks for it, and for nothing else.
pub struct InFlight {
    /// The request's id, as in `x-vm-request-id` and the log.
    pub request: Arc<str>,
    pub user: User,
    pub application: Arc<str>,
    /// The Version running the request.
    pub version: u64,
    /// That Version's Declared access.
    pub declared: Arc<Declared>,
    /// The transactions it holds open (blackbox/CONTRACT.md,
    /// "Transactions"): they belong to it, and are rolled back when it ends.
    pub transactions: Transactions,
    /// What its context printed and did not catch, for its log entry
    /// (crate::app_logs).
    pub collected: Collected,
    /// The bodies this request holds open (blackbox/CONTRACT.md,
    /// "Storage: Bounds"): their places, the pumps and put tasks that feed
    /// them, and the signal of the request's end, on which a put still
    /// running is given up.
    pub bodies: Arc<Bodies>,
    /// The host calls not yet answered of this request
    /// (blackbox/CONTRACT.md, "A host call's arguments are bounded"):
    /// their places, 16 MiB and 1,024 calls, against the program's
    /// 256 MiB.
    pub calls: Arc<crate::gateway::RequestCalls>,
    /// The request's stated `content-length`, when there was one: a put
    /// of the request's own body checks the file's bound against it
    /// before a byte is read.
    pub content_length: Option<u64>,
    /// The streams of this request's context, filled once the context
    /// exists ([`Engine::serve_in`]). A storage get answers a body as a
    /// stream the host feeds through it; a put takes one.
    pub streams: std::sync::OnceLock<Arc<StreamManager>>,
    /// Fired when a Platform administrator disables the Application
    /// (blackbox/CONTRACT.md, "What Platform administrators see and stop:
    /// Requests in flight"): the request is answered so at once, its code
    /// stopped, and its operations refused.
    pub stop: Arc<Stop>,
    /// The outbound calls of this request: its 8 turns and its count of
    /// calls waiting (blackbox/CONTRACT.md, "Outbound calls: Bounds").
    pub fetches: crate::outbound::Turns,
    /// When the request's own time ends, set when the engine starts it: an
    /// outbound call that would outlast it ends a little before, so that
    /// the code is told what happened (`limit` `time`).
    pub ends: std::sync::OnceLock<tokio::time::Instant>,
}

impl std::fmt::Debug for InFlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing below names a body or a stream: what they hold is the
        // request's own.
        f.debug_struct("InFlight")
            .field("request", &self.request)
            .field("user", &self.user)
            .field("application", &self.application)
            .field("version", &self.version)
            .field("declared", &self.declared)
            .finish_non_exhaustive()
    }
}

/// What the Application answered.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: AnswerBody,
}

/// The answer's body.
#[derive(Debug)]
pub enum AnswerBody {
    /// The whole body, at most `Limits::max_body_bytes`: every body the code
    /// made, and one it began to read before giving back.
    Whole(Bytes),
    /// A body the host itself feeds, given back unread: a stream the host
    /// sends itself, after the code has ended if need be
    /// (blackbox/CONTRACT.md, "What an Application is handed"). Never
    /// collected here; at most 16 chunks of 64 KiB are in flight in the
    /// runtime's channel, and the platform sets `content-length`.
    Streamed {
        chunks: mpsc::Receiver<Result<Bytes, String>>,
        /// What feeds the body and must outlive the request's code: the
        /// pump of a Storage file's body, taken over from the request's
        /// open bodies. The request's own body given back holds nothing
        /// here (the connection owns its pump).
        held: Vec<crate::sent::OwnedTask>,
        /// The length the platform states, when it knows one: a file's
        /// size; `None` for the request's own body given back, whose
        /// `content-length` the request stated (or none, and the body is
        /// sent chunked).
        length: Option<u64>,
    },
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
    /// The Application was disabled while the request was in flight: its
    /// code was stopped, or (`given` false) never given it.
    Stopped {
        given: bool,
    },
}

/// How long a request stopped while its code runs waits for the engine to
/// say it has stopped it, before it is answered anyway (code that holds its
/// thread is ended by its CPU limit).
const STOPPED_WAIT: Duration = Duration::from_millis(500);

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
            // Longer than a query may run (10 s, crate::executor) and be
            // logged, so that an Application waiting on one gets its answer.
            deadline: Duration::from_secs(30),
            cpu_ms: 2_000,
            heap_max_mb: 64,
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

/// One request handed to an engine thread.
struct Job {
    in_flight: Arc<InFlight>,
    serving: Arc<Serving>,
    request: HttpRequest,
    /// Says that the request's body has been asked for: the runtime sets it
    /// when the code first reads the body, or the response takes it to send
    /// (vendor/VENDORED.md, patch 9). Its sender gone means nobody was ever
    /// given the request.
    asked: watch::Sender<bool>,
    reply: oneshot::Sender<Outcome>,
    /// Set by whichever comes first: the engine taking the job, or the
    /// request being stopped while it waits (then the engine drops it).
    taken: Arc<AtomicBool>,
}

pub struct RequestRunner {
    lanes: Vec<mpsc::Sender<Job>>,
    next: AtomicUsize,
    deadline: Duration,
    /// The engine threads, kept rather than detached. `vibe serve` runs them
    /// until the program is killed (blackbox/CONTRACT.md); the local run
    /// ends them with [`RequestRunner::stop`].
    threads: Vec<JoinHandle<()>>,
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
                .name(format!("vibe-engine-{index}"))
                .spawn(move || engine.run(receiver))?;
            lanes.push(sender);
            threads.push(thread);
        }
        Ok(Self {
            lanes,
            next: AtomicUsize::new(0),
            deadline: limits.deadline,
            threads,
        })
    }

    /// Stops the engine threads: no request is taken any more, and each
    /// thread ends once the requests it runs have ended (each within its
    /// deadline). Waits up to `limit`, polling, and answers whether every
    /// thread ended; a thread still running then is left to end by itself.
    /// For a caller that must know the threads have let go of the gateway
    /// (the local run, before it stops its Postgres).
    pub async fn stop(self, limit: Duration) -> bool {
        let Self { lanes, threads, .. } = self;
        // Closing every lane ends each thread's loop once it is idle.
        drop(lanes);
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if threads.iter().all(JoinHandle::is_finished) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Runs one request to its end. `asked` says when its body is first
    /// asked for (see [`Job::asked`]).
    pub async fn run(
        &self,
        in_flight: Arc<InFlight>,
        serving: Arc<Serving>,
        request: HttpRequest,
        asked: watch::Sender<bool>,
    ) -> Outcome {
        let (reply, mut answer) = oneshot::channel();
        let taken = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&in_flight.stop);
        let stop_transactions = Arc::clone(&in_flight);
        let mut job = Job {
            in_flight,
            serving,
            request,
            asked,
            reply,
            taken: Arc::clone(&taken),
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
        let backstop = tokio::time::sleep(self.deadline + Duration::from_secs(1));
        tokio::select! {
            biased;
            outcome = &mut answer => return outcome.unwrap_or(Outcome::Failed),
            () = backstop => return Outcome::Failed,
            () = stop.stopped() => {}
        }
        // Disabled while it waited or ran.
        if !taken.swap(true, Ordering::AcqRel) {
            // Never given to an engine: it never will be.
            return Outcome::Stopped { given: false };
        }
        // Its transactions are rolled back now, whatever its code is doing
        // (the engine ends them too once it has stopped the code).
        stop_transactions.transactions.end();
        match tokio::time::timeout(STOPPED_WAIT, answer).await {
            Ok(Ok(outcome @ Outcome::Stopped { .. })) => outcome,
            _ => Outcome::Stopped { given: true },
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
                        // Stopped while it waited: never given to the code.
                        if job.taken.swap(true, Ordering::AcqRel) {
                            continue;
                        }
                        let engine = Arc::clone(&engine);
                        running.spawn_local(async move {
                            let outcome = engine
                                .serve(job.in_flight, &job.serving, job.request, job.asked)
                                .await;
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
        serving: &Serving,
        request: HttpRequest,
        asked: watch::Sender<bool>,
    ) -> Outcome {
        if in_flight.stop.is_stopped() {
            return Outcome::Stopped { given: false };
        }
        let stop = Arc::clone(&in_flight.stop);
        let host = Arc::new(HostOps::new(
            Arc::clone(&in_flight),
            Arc::clone(&self.gateway),
        ));
        let deadline = self.limits.deadline;
        in_flight
            .ends
            .set(tokio::time::Instant::now() + deadline)
            .ok();
        let served = tokio::select! {
            biased;
            // Disabled: the context, and all it holds, goes with the
            // future; its transactions are rolled back.
            () = stop.stopped() => {
                host.end();
                return Outcome::Stopped { given: true };
            }
            served = tokio::time::timeout(deadline, self.serve_in(&host, serving, request, asked)) => served,
        };
        match served {
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
        serving: &Serving,
        request: HttpRequest,
        asked: watch::Sender<bool>,
    ) -> Outcome {
        let script = Script::with_bindings(
            glue::script(&serving.bundle),
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
        // The runtime's streams, read after the event for the one thing the
        // host needs to know of them: which host-fed stream became the
        // response's body, and whether the code read it
        // (vendor/VENDORED.md, patch 6). Named for the host's operations
        // (a storage get answers a body as a stream through them, a put
        // takes one) before the event, so a call made in it finds them.
        let stream_manager = worker.stream_manager();
        drop(host.in_flight.streams.set(Arc::clone(&stream_manager)));
        // The request's precheck of a call's arguments
        // (vendor/VENDORED.md, patch 17), attached before the event like
        // the `asked` sender: asked by the invoke native with the call's
        // binding and the UTF-8 length of its JSON text, within the call's
        // own bound, before anything of the call is copied or parsed. The
        // words it answers are the host's (crate::gateway::calls), read
        // again by the host inside the call. Calls on the glue's own
        // binding it says nothing of: the host answers them at once and
        // keeps nothing of them, whoever made them (code that names that
        // binding itself is held to the call's own bound by the native,
        // as every call is).
        {
            let calls = Arc::clone(&host.in_flight.calls);
            let program = self.gateway.held_calls();
            let application = Arc::clone(&host.in_flight.application);
            stream_manager.set_call_precheck(Arc::new(move |binding, size| {
                if binding == crate::runner::host::PLATFORM_BINDING
                    || binding == crate::runner::host::FETCH_BINDING
                {
                    return None;
                }
                calls.would_refuse(program.as_ref(), &application, size)
            }));
        }
        // The request's body is read only when the code (or the response)
        // asks for it (vendor/VENDORED.md, patch 9). Attached before the
        // event: a body asked for at its first read is seen.
        stream_manager.watch_asked(asked);
        let (event, response) = Event::fetch(request);
        host.begin();
        let in_flight = Arc::clone(&host.in_flight);
        let (ran, answer) = tokio::join!(
            worker.exec(event),
            collect(
                &in_flight,
                stream_manager,
                response,
                self.limits.max_body_bytes
            )
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
///
/// The one body not read here is a stream the runtime says the host itself
/// feeds and the Application's code did not read (the request's own body
/// given back, a Storage file's): it is handed on as it is, to be sent by
/// the connection. A body the code began to read is no longer the host's
/// to vouch for, and is read whole as every other body is
/// (blackbox/CONTRACT.md, "What an Application is handed").
async fn collect(
    in_flight: &Arc<InFlight>,
    stream_manager: Arc<StreamManager>,
    response: oneshot::Receiver<HttpResponse>,
    max_body_bytes: usize,
) -> Result<Answer, String> {
    let response = response
        .await
        .map_err(|_| "the Application gave no response".to_owned())?;
    // Told by the runtime before the response was sent (vendor/VENDORED.md,
    // patch 6): `Some(read)` when the response's body is a host-fed
    // stream, and whether the code had read from it.
    let host_fed = stream_manager.host_response();
    let body = match response.body {
        ResponseBody::None => AnswerBody::Whole(Bytes::new()),
        ResponseBody::Bytes(bytes) => {
            if bytes.len() > max_body_bytes {
                return Err(format!(
                    "the response body is larger than {max_body_bytes} bytes"
                ));
            }
            AnswerBody::Whole(bytes)
        }
        ResponseBody::Stream(chunks) => {
            if let Some(host_fed) = host_fed
                && !host_fed.read
            {
                // A body the host sends itself. A Storage file's body is
                // fed by the request's own pump: the sending goes on
                // after the code has ended, so the answer takes the pump
                // over (with the file's size, the `content-length` the
                // platform states); the request's own body given back is
                // fed by the connection's own pump.
                let (held, length) = match in_flight.bodies.take_for_sending(host_fed.id) {
                    Some((pump, length)) => (vec![pump], length),
                    None => (Vec::new(), None),
                };
                return Ok(Answer {
                    status: response.status,
                    headers: response.headers,
                    body: AnswerBody::Streamed {
                        chunks,
                        held,
                        length,
                    },
                });
            }
            let mut body = BytesMut::new();
            let mut chunks = chunks;
            while let Some(chunk) = chunks.recv().await {
                let chunk = chunk.map_err(|e| format!("the response body failed: {e}"))?;
                if body.len() + chunk.len() > max_body_bytes {
                    return Err(format!(
                        "the response body is larger than {max_body_bytes} bytes"
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            AnswerBody::Whole(body.freeze())
        }
    };
    Ok(Answer {
        status: response.status,
        headers: response.headers,
        body,
    })
}
