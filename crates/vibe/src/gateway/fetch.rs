//! The `fetch` operation: an Application's call to the outside
//! (blackbox/CONTRACT.md, "Outbound calls"; ADR 0020).
//!
//! **Decided in the call, run in a task.** What can be decided from memory
//! is decided inside the Application's own call ([`PlatformGateway::fetch`],
//! in the order the contract gives: no request, disabled, malformed, not
//! `https:`, the platform itself, a host not on the list); the rest is a
//! task on the gateway's [`Operations`], which writes the Operation log entry
//! before a name is resolved, waits for its turn, resolves and connects to
//! an address that was checked, sends the request, and answers when the
//! headers have come. The task owns the call until then; the answer's body
//! is fed by a pump owned by the request's registry of open bodies (or by
//! the response that sends it), which holds the connection and the places
//! the call holds.
//!
//! **Every redirect is a call.** The task decides each hop again, writes an
//! entry for it, and follows at most five.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::header::{
    CONTENT_LENGTH, HOST, HeaderMap, HeaderName, HeaderValue, LOCATION, USER_AGENT,
};
use hyper::{Method, Request};
use openworkers_runtime_v8::runtime::stream_manager::{StreamChunk, StreamId, StreamManager};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::Instant;

use super::transaction::ended_why;
use super::{
    Answer, Decision, MALFORMED, Operations, PlatformGateway, Refusal, error_to_wire, ok_to_wire,
};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::outbound::request::{self, Args, BodyArg, Redirect};
use crate::outbound::target::{self, Target};
use crate::outbound::wire::{self, Failed, NotConnected, Payload};
use crate::outbound::{Itself, Outbound, PrivateRanges, Program};
use crate::runner::InFlight;
use crate::sent::OwnedTask;
use crate::storage::bodies::Bodies;

/// The refusal reasons of a call.
const NOT_HTTPS: &str = "not-https";
const PLATFORM_ITSELF: &str = "platform-itself";
const HOST_NOT_LISTED: &str = "host-not-listed";
const ADDRESS_NOT_ALLOWED: &str = "address-not-allowed";

/// The longest the headers of an answer may take, from the decision.
const HEAD_TIME: Duration = Duration::from_secs(30);

/// Redirects followed at most.
const MOST_REDIRECTS: u32 = 5;

/// The most calls decided and not yet ended, in the whole program: a
/// backstop on the tasks (the places a call needs are 8 for its request and
/// 256 in the program; 72 may be counted for a request).
const MOST_TASKS: usize = 4096;

/// How long the code's call waits for its answer at the most: the
/// task's own steps are all bounded by far less.
const ANSWER_BACKSTOP: Duration = Duration::from_secs(600);

/// How long before its request ends a call that would outlast it ends:
/// the request's own time (30 seconds) is not more than a call's, so a call
/// is cut short of it, and the code is told `limit` `time` while it can
/// still answer.
const BEFORE_THE_END: Duration = Duration::from_secs(1);

/// The pieces an answer's body is passed on in.
const PIECE: usize = 64 * 1024;

/// One call's place among its request's 72 and the program's 4,096 calls
/// not yet ended. It travels with the call: into the task, into the future
/// that gives the code its answer (so a call counts until the code has been
/// given its answer, not only until the task ended), into what the call
/// holds while under way, into the pump of an answer's body; it is given
/// back when the last of them is dropped.
struct Admitted {
    enrolled: crate::outbound::limits::Enrolled,
    _task: OwnedSemaphorePermit,
}

/// What the program holds for outbound calls.
pub(super) struct Calls {
    pub(super) list: Arc<Outbound>,
    pub(super) private: PrivateRanges,
    pub(super) program: Program,
    pub(super) tasks: Arc<Semaphore>,
    pub(super) itself: Itself,
}

impl Calls {
    pub(super) fn new(
        list: Arc<Outbound>,
        private: PrivateRanges,
        itself: Itself,
        mentions: Arc<crate::shares::Mentions>,
    ) -> Self {
        Self {
            list,
            private,
            program: Program::new(mentions),
            tasks: Arc::new(Semaphore::new(MOST_TASKS)),
            itself,
        }
    }
}

/// What the gateway decided about one `fetch`: the decision as for any
/// operation, and the host the call is to, for the `host-call` record.
pub struct FetchDecision {
    pub decision: Decision,
    pub host: Option<String>,
}

fn refusal(reason: &'static str, host: Option<&str>, fix: String) -> Refusal {
    let mut refusal = Refusal::new(reason, fix);
    refusal.details.insert(
        "host".into(),
        host.map_or(Value::Null, |host| Value::String(host.to_owned())),
    );
    refusal
}

/// A refusal every operation has, with the call's `host` in it.
fn with_host(mut refusal: Refusal, host: Option<&str>) -> Refusal {
    refusal.details.insert(
        "host".into(),
        host.map_or(Value::Null, |host| Value::String(host.to_owned())),
    );
    refusal
}

/// A refusal at a redirect holds the host the redirect came from.
fn redirected(mut refusal: Refusal, from: Option<&str>) -> Refusal {
    if let Some(from) = from {
        refusal
            .details
            .insert("redirected_from".into(), Value::String(from.to_owned()));
    }
    refusal
}

fn not_https(host: Option<&str>) -> Refusal {
    refusal(
        NOT_HTTPS,
        host,
        "an Application calls the outside over `https:` only, with no user or password in the address: use `https://<host>/...`".into(),
    )
}

fn platform_itself(host: &str) -> Refusal {
    refusal(
        PLATFORM_ITSELF,
        Some(host),
        "an Application does not call the platform itself (its own domain, another Application, or the API) with `fetch`: use the platform's own operations instead".into(),
    )
}

fn not_listed(host: &str, local: bool) -> Refusal {
    let fix = if local {
        "locally the list of hosts a call may go to is `outbound.hosts` in `vibe.dev.json`: add the host there; in an installation the list is the Platform administrators' (`vibe outbound` shows it, and a Platform administrator allows a host with `vibe admin outbound add <host>`)"
    } else {
        "only a Platform administrator can allow a host: ask one to run `vibe admin outbound add <host>`; `vibe outbound` shows what may be called now"
    };
    refusal(HOST_NOT_LISTED, Some(host), fix.into())
}

fn address_not_allowed(host: &str) -> Refusal {
    refusal(
        ADDRESS_NOT_ALLOWED,
        Some(host),
        "the platform does not call addresses inside the company's network or the machine itself, whatever the list says: only a host whose address is on the public Internet can be called".into(),
    )
}

fn failure(value: Value) -> Value {
    error_to_wire(value)
}

fn unavailable() -> Value {
    failure(json!({ "kind": "unavailable" }))
}

fn unreachable_why(why: &str) -> Value {
    failure(json!({ "kind": "unreachable", "why": why }))
}

fn limit(which: &str) -> Value {
    failure(json!({ "kind": "limit", "limit": which }))
}

/// What a call does with its body on a later hop.
enum Payloads {
    None,
    Text(Bytes),
    /// A stream: given to the first hop, and not to another.
    Stream {
        chunks: Option<mpsc::Receiver<StreamChunk>>,
        length: Option<u64>,
    },
}

/// A call decided, and what its task needs.
struct Work {
    in_flight: Arc<InFlight>,
    active: bool,
    log: Arc<OperationLog>,
    calls: Arc<Calls>,
    method: &'static str,
    headers: Vec<(HeaderName, HeaderValue)>,
    redirect: Redirect,
    body: Payloads,
    target: Target,
    /// The refusal the call's decision found, for the first hop.
    refusal: Option<Refusal>,
    /// Failed before it began: no place for it (`unavailable`).
    unavailable: bool,
    admitted: Option<Arc<Admitted>>,
}

/// What a call holds while it is under way: its turn among its request's
/// 8, its place among the program's 256, and its count.
struct Holding {
    _turn: OwnedSemaphorePermit,
    _place: crate::outbound::limits::Held,
    _admitted: Arc<Admitted>,
}

impl PlatformGateway {
    /// The call's place among its request's 72 and the program's 4,096, or
    /// `None` when there is none: the call is answered `unavailable` at
    /// once, with nothing allocated, spawned or written for it.
    fn admit(&self, in_flight: &InFlight) -> Option<Arc<Admitted>> {
        let task = Arc::clone(&self.calls.tasks).try_acquire_owned().ok()?;
        let enrolled = in_flight.fetches.enroll()?;
        Some(Arc::new(Admitted {
            enrolled,
            _task: task,
        }))
    }

    /// A `fetch`: decided now, inside the code's call, and run in a task.
    pub(super) fn fetch_call(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
    ) -> FetchDecision {
        // Before anything is read, allocated or spawned: a call that finds
        // no place is answered at once, with no entry and no task.
        let Some(admitted) = self.admit(in_flight) else {
            return FetchDecision {
                decision: Decision {
                    refused: Some(super::LIMIT),
                    answer: Box::pin(std::future::ready(unavailable())),
                },
                host: None,
            };
        };
        if !active {
            let refusal = with_host(Refusal::no_request(), None);
            return self.refused_call(in_flight, active, &refusal, None, admitted);
        }
        if in_flight.stop.is_stopped() {
            let refusal = with_host(Refusal::application_disabled(), None);
            return self.refused_call(in_flight, active, &refusal, None, admitted);
        }
        let args = match request::read(args) {
            Ok(args) => args,
            Err(problem) => {
                return FetchDecision {
                    decision: Decision {
                        refused: Some(MALFORMED),
                        answer: Box::pin(std::future::ready(error_to_wire(
                            json!({ "kind": "malformed", "message": problem }),
                        ))),
                    },
                    host: None,
                };
            }
        };
        let Args {
            url,
            method,
            headers,
            redirect,
            body,
        } = args;
        let target = match target::read(&url) {
            Ok(target) => target,
            Err(host) => {
                let refusal = not_https(host.as_deref());
                return self.refused_call(in_flight, active, &refusal, host, admitted);
            }
        };
        let text = target.text.clone();
        if let Some(refusal) = self.decide_hop(&target, None) {
            return self.refused_call(in_flight, active, &refusal, Some(text), admitted);
        }
        // The body's stream is the call's from here on, inside the code's
        // call, so that the code can neither read it nor race the call for
        // it. Taken only for a call that is allowed.
        let streams = in_flight.streams.get();
        let known = match &body {
            BodyArg::Native(id) => streams.is_some_and(|s| s.is_host_fed(*id)),
            BodyArg::Pumped { id, .. } => streams.is_some_and(|s| s.is_guest_stream(*id)),
            BodyArg::None | BodyArg::Text(_) | BodyArg::Busy => true,
        };
        if !known {
            return FetchDecision {
                decision: Decision {
                    refused: Some(MALFORMED),
                    answer: Box::pin(std::future::ready(error_to_wire(json!({
                        "kind": "malformed",
                        "message": "`body` names a stream that is not one of this request's",
                    })))),
                },
                host: None,
            };
        }
        let unavailable = matches!(body, BodyArg::Busy);
        let payloads = if unavailable {
            Payloads::None
        } else {
            match body {
                BodyArg::None | BodyArg::Busy => Payloads::None,
                BodyArg::Text(text) => Payloads::Text(Bytes::from(text)),
                BodyArg::Native(id) | BodyArg::Pumped { id, .. } => {
                    let length = match &body {
                        BodyArg::Pumped { length, .. } => *length,
                        _ => self.length_of(in_flight, id),
                    };
                    match streams.and_then(|streams| streams.take_receiver(id)) {
                        Some(chunks) => Payloads::Stream {
                            chunks: Some(chunks),
                            length,
                        },
                        None => {
                            return FetchDecision {
                                decision: Decision {
                                    refused: Some(MALFORMED),
                                    answer: Box::pin(std::future::ready(error_to_wire(json!({
                                        "kind": "malformed",
                                        "message": "`body` is a stream that was already given to another call of this request",
                                    })))),
                                },
                                host: None,
                            };
                        }
                    }
                }
            }
        };
        let work = Work {
            in_flight: Arc::clone(in_flight),
            active,
            log: Arc::clone(&self.log),
            calls: Arc::clone(&self.calls),
            method,
            headers,
            redirect,
            body: payloads,
            target,
            refusal: None,
            unavailable,
            admitted: Some(admitted),
        };
        FetchDecision {
            decision: Decision {
                refused: None,
                answer: self.run_work(work),
            },
            host: Some(text),
        }
    }

    /// The length of a native body: the request's own stated one, or a
    /// stored file's size.
    fn length_of(&self, in_flight: &InFlight, id: StreamId) -> Option<u64> {
        let streams = in_flight.streams.get()?;
        if streams.request_body() == Some(id) {
            in_flight.content_length
        } else {
            in_flight.bodies.length_of(id)
        }
    }

    /// `Some(refusal)` when the call to `target` is refused by what memory
    /// knows: the platform itself, a host not on the list. `from` is the
    /// host of the redirect that sent it.
    fn decide_hop(&self, target: &Target, from: Option<&str>) -> Option<Refusal> {
        decide(&self.calls, target, from)
    }

    /// A refused call: its answer, and its entry written before it is
    /// given (or at once when no request is in flight).
    fn refused_call(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        refusal: &Refusal,
        host: Option<String>,
        admitted: Arc<Admitted>,
    ) -> FetchDecision {
        let reason = refusal.reason;
        let wire = refusal.to_wire();
        let immediate = wire.clone();
        // A refusal with no address has no entry of its own, but for the
        // ones that are the call's alone: no request, a disabled
        // Application.
        let entry_host = host.clone();
        let has_entry =
            host.is_some() || matches!(reason, super::NO_REQUEST | super::APPLICATION_DISABLED);
        let answer: Answer = if !has_entry {
            drop(admitted);
            Box::pin(std::future::ready(wire))
        } else {
            let log = Arc::clone(&self.log);
            let in_flight = Arc::clone(in_flight);
            let held = Arc::clone(&admitted);
            let write = async move {
                let entry = entry_of(&in_flight, active, entry_host.as_deref());
                if let Err(error) = log.refused(&entry, reason).await {
                    unlogged(&in_flight, &error);
                }
                wire
            };
            if active {
                self.spawn_answer(write, admitted)
            } else {
                // Answered at once: the runtime may not settle a call made
                // while no event runs. Its entry is written meanwhile; past
                // the operations at once it is not, and the program's log
                // says so.
                match Arc::clone(&self.operations.permits).try_acquire_owned() {
                    Ok(permit) => self.operations.spawn(async move {
                        drop(write.await);
                        drop(permit);
                        drop(held);
                    }),
                    Err(_) => log::record(
                        "operations-busy",
                        json!({ "problem": "the entry of a refused fetch was not written" }),
                    ),
                }
                Box::pin(std::future::ready(immediate))
            }
        };
        FetchDecision {
            decision: Decision {
                refused: Some(reason),
                answer,
            },
            host,
        }
    }

    /// Runs `future` as a task of the gateway's and answers what it
    /// answers.
    ///
    /// The call stays counted (`admitted`) until the answer has been given:
    /// the future the code's answer comes from holds it until it resolves.
    fn spawn_answer(
        &self,
        future: impl Future<Output = Value> + Send + 'static,
        admitted: Arc<Admitted>,
    ) -> Answer {
        let operations: Arc<Operations> = Arc::clone(&self.operations);
        let (reply, answer) = oneshot::channel();
        operations.spawn(async move {
            // The Application may have stopped waiting; the entry is
            // complete either way.
            reply.send(future.await).ok();
        });
        Box::pin(async move {
            let answered = match tokio::time::timeout(ANSWER_BACKSTOP, answer).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) | Err(_) => unavailable(),
            };
            drop(admitted);
            answered
        })
    }

    fn run_work(&self, work: Work) -> Answer {
        let Some(admitted) = work.admitted.as_ref().map(Arc::clone) else {
            return Box::pin(std::future::ready(unavailable()));
        };
        self.spawn_answer(work.run(), admitted)
    }
}

use std::future::Future;

/// The decision of memory on a call to `target`.
fn decide(calls: &Calls, target: &Target, from: Option<&str>) -> Option<Refusal> {
    let text = target.text.as_str();
    let found = if target.is_platform(&calls.itself.domain, calls.itself.bare) {
        Some(platform_itself(text))
    } else if !calls.list.allows(target.host_str(), target.port) {
        Some(not_listed(text, calls.itself.local))
    } else {
        None
    };
    found.map(|mut refusal| {
        if let Some(from) = from {
            refusal
                .details
                .insert("redirected_from".into(), Value::String(from.to_owned()));
        }
        refusal
    })
}

fn entry_of<'a>(in_flight: &'a InFlight, active: bool, host: Option<&'a str>) -> Entry<'a> {
    Entry {
        request: active.then_some(&*in_flight.request),
        user: active.then_some(in_flight.user.id.as_str()),
        application: &in_flight.application,
        version: in_flight.version,
        resource: None,
        actor: "user",
        operation: "fetch",
        statement: None,
        transaction: None,
        host,
    }
}

fn unlogged(in_flight: &InFlight, error: &crate::platform_db::DbError) {
    log::record(
        "operation-log-failed",
        json!({
            "application": &*in_flight.application,
            "resource": Value::Null,
            "own_database": false,
            "error": error.to_string(),
        }),
    );
}

fn failed_ending(error: &str, duration_ms: Option<u64>) -> Ending<'_> {
    Ending {
        outcome: "failed",
        error: Some(error),
        rows: None,
        duration_ms,
        began_nothing: false,
    }
}

/// How a hop ended, before its entry is completed.
enum Hop {
    /// The request ended, or its Application was disabled.
    Ended,
    /// Refused after the entry was begun: a name that resolved to an
    /// address never called.
    Refused,
    Failed(&'static str, Value),
    Head(Box<wire::Head>),
}

/// What is done with an answer.
enum Next {
    Answer,
    Follow {
        to: Result<Target, Option<String>>,
        method: &'static str,
        drop_body: bool,
    },
    Fail(&'static str, Value),
}

impl Work {
    fn entry<'a>(&'a self, host: Option<&'a str>) -> Entry<'a> {
        entry_of(&self.in_flight, self.active, host)
    }

    async fn ended_answer(&self) -> Value {
        if self.in_flight.stop.is_stopped() {
            with_host(Refusal::application_disabled(), None).to_wire()
        } else {
            unavailable()
        }
    }

    async fn run(mut self) -> Value {
        let mut from: Option<String> = None;
        let mut redirects = 0_u32;
        let mut holding: Option<Holding> = None;
        let mut first = true;
        loop {
            let decided = Instant::now();
            let text = self.target.text.clone();
            if self.unavailable {
                // No place for the call: its whole entry, failed, and the
                // answer at once.
                let entry = self.entry(Some(&text));
                return match self.log.failed(&entry, "unavailable").await {
                    Ok(()) => unavailable(),
                    Err(error) => {
                        unlogged(&self.in_flight, &error);
                        redirected(
                            with_host(Refusal::log_unavailable(None), Some(&text)),
                            from.as_deref(),
                        )
                        .to_wire()
                    }
                };
            }
            let decision = if first {
                first = false;
                self.refusal.take()
            } else {
                decide(&self.calls, &self.target, from.as_deref())
            };
            if let Some(refusal) = decision {
                let entry = self.entry(Some(&text));
                if let Err(error) = self.log.refused(&entry, refusal.reason).await {
                    unlogged(&self.in_flight, &error);
                }
                return refusal.to_wire();
            }
            let entry = self.entry(Some(&text));
            let id = match self.log.begin(&entry).await {
                Ok(id) => id,
                Err(error) => {
                    unlogged(&self.in_flight, &error);
                    return redirected(
                        with_host(Refusal::log_unavailable(None), Some(&text)),
                        from.as_deref(),
                    )
                    .to_wire();
                }
            };
            let elapsed = || Some(u64::try_from(decided.elapsed().as_millis()).unwrap_or(u64::MAX));
            if holding.is_none() {
                match self.take_places().await {
                    Ok(places) => holding = Some(places),
                    Err(error) => {
                        self.finish(id, &failed_ending(error, elapsed())).await;
                        return if error == "unavailable" {
                            unavailable()
                        } else {
                            self.ended_answer().await
                        };
                    }
                }
            }
            let hop = self.hop(decided).await;
            let head = match hop {
                Hop::Ended => {
                    let error = ended_why(&self.in_flight);
                    self.finish(id, &failed_ending(error, elapsed())).await;
                    return self.ended_answer().await;
                }
                Hop::Refused => {
                    if let Err(error) = self
                        .log
                        .finish_refused(id, ADDRESS_NOT_ALLOWED, elapsed())
                        .await
                    {
                        unlogged(&self.in_flight, &error);
                    }
                    let mut refusal = address_not_allowed(&text);
                    if let Some(from) = &from {
                        refusal
                            .details
                            .insert("redirected_from".into(), Value::String(from.clone()));
                    }
                    return refusal.to_wire();
                }
                Hop::Failed(error, answer) => {
                    self.finish(id, &failed_ending(error, elapsed())).await;
                    return answer;
                }
                Hop::Head(head) => head,
            };
            match self.next(&head, redirects) {
                Next::Fail(error, answer) => {
                    self.finish(id, &failed_ending(error, elapsed())).await;
                    return answer;
                }
                Next::Answer => {
                    let ended = Ending {
                        outcome: "allowed",
                        error: None,
                        rows: None,
                        duration_ms: elapsed(),
                        began_nothing: false,
                    };
                    self.finish(id, &ended).await;
                    let Some(holding) = holding.take() else {
                        return unavailable();
                    };
                    return self.answer(*head, holding, redirects > 0);
                }
                Next::Follow {
                    to,
                    method,
                    drop_body,
                } => {
                    let ended = Ending {
                        outcome: "allowed",
                        error: None,
                        rows: None,
                        duration_ms: elapsed(),
                        began_nothing: false,
                    };
                    self.finish(id, &ended).await;
                    // The redirect's own body is not read: its connection
                    // goes with the head.
                    drop(head);
                    redirects += 1;
                    from = Some(text);
                    self.method = method;
                    if drop_body {
                        self.body = Payloads::None;
                        self.headers.retain(|(name, _)| {
                            !matches!(
                                name.as_str(),
                                "content-type"
                                    | "content-encoding"
                                    | "content-language"
                                    | "content-location"
                            )
                        });
                    }
                    match to {
                        Ok(next) => {
                            if !self.target.same_origin(&next) {
                                self.headers.retain(|(name, _)| !request::credential(name));
                            }
                            self.target = next;
                        }
                        Err(host) => {
                            // Not `https:`: a call of its own, refused.
                            let mut refusal = not_https(host.as_deref());
                            if let Some(from) = &from {
                                refusal
                                    .details
                                    .insert("redirected_from".into(), Value::String(from.clone()));
                            }
                            if host.is_some() {
                                let entry = self.entry(host.as_deref());
                                if let Err(error) = self.log.refused(&entry, refusal.reason).await {
                                    unlogged(&self.in_flight, &error);
                                }
                            }
                            return refusal.to_wire();
                        }
                    }
                }
            }
        }
    }

    async fn finish(&self, id: i64, ending: &Ending<'_>) {
        if let Err(error) = self.log.finish(id, ending).await {
            unlogged(&self.in_flight, &error);
        }
    }

    /// Its turn among its request's, then its place among the program's.
    async fn take_places(&mut self) -> Result<Holding, &'static str> {
        let Some(admitted) = self.admitted.take() else {
            return Err("unavailable");
        };
        let enrolled = &admitted.enrolled;
        let bodies = Arc::clone(&self.in_flight.bodies);
        let turn = tokio::select! {
            biased;
            () = bodies.ended() => return Err(ended_why(&self.in_flight)),
            turn = enrolled.turn() => turn,
        };
        let Some(turn) = turn else {
            return Err("unavailable");
        };
        let place = tokio::select! {
            biased;
            () = bodies.ended() => return Err(ended_why(&self.in_flight)),
            place = self.calls.program.place(&self.in_flight.application) => place,
        };
        let Some(place) = place else {
            return Err("unavailable");
        };
        Ok(Holding {
            _turn: turn,
            _place: place,
            _admitted: admitted,
        })
    }

    /// One call: the connection, the request and the answer's headers.
    async fn hop(&mut self, decided: Instant) -> Hop {
        let overall = self.cut_short(decided + HEAD_TIME);
        let bodies = Arc::clone(&self.in_flight.bodies);
        let Ok(tls) = crate::trust::client_config() else {
            return Hop::Failed("unreachable", unreachable_why("tls"));
        };
        let connected = tokio::select! {
            biased;
            () = bodies.ended() => return Hop::Ended,
            connected = wire::connect(&self.target, &self.calls.private, &self.calls.program, &self.in_flight.application, &tls, overall) => connected,
        };
        let stream = match connected {
            Ok(stream) => stream,
            Err(NotConnected::NotAllowed) => return Hop::Refused,
            Err(NotConnected::Dns) => return Hop::Failed("unreachable", unreachable_why("dns")),
            Err(NotConnected::Connect) => {
                return Hop::Failed("unreachable", unreachable_why("connect"));
            }
            Err(NotConnected::Tls) => return Hop::Failed("unreachable", unreachable_why("tls")),
            Err(NotConnected::Time) => return Hop::Failed("time", limit("time")),
        };
        let payload = match &mut self.body {
            Payloads::None => Payload::empty(),
            Payloads::Text(bytes) => Payload::whole(bytes.clone()),
            Payloads::Stream { chunks, length } => match chunks.take() {
                Some(chunks) => Payload::stream(chunks, *length),
                None => Payload::empty(),
            },
        };
        let uploaded = payload.failed();
        let Some(request) = self.build(payload) else {
            return Hop::Failed("unreachable", unreachable_why("protocol"));
        };
        let exchange = tokio::time::timeout_at(overall, wire::exchange(stream, request, &uploaded));
        let exchanged = tokio::select! {
            biased;
            () = bodies.ended() => return Hop::Ended,
            exchanged = exchange => exchanged,
        };
        match exchanged {
            Err(_) => Hop::Failed("time", limit("time")),
            Ok(Err(Failed::Protocol)) => Hop::Failed("unreachable", unreachable_why("protocol")),
            Ok(Err(Failed::Headers)) => Hop::Failed("headers", limit("headers")),
            Ok(Err(Failed::BodyTooLarge)) => Hop::Failed("bytes", limit("bytes")),
            Ok(Err(Failed::BodyFailed)) => {
                Hop::Failed("body-failed", failure(json!({ "kind": "body-failed" })))
            }
            Ok(Ok(head)) => Hop::Head(Box::new(head)),
        }
    }

    /// `at`, or a little before the request ends when that is sooner.
    fn cut_short(&self, at: Instant) -> Instant {
        match self.in_flight.ends.get() {
            Some(ends) => at.min(ends.checked_sub(BEFORE_THE_END).unwrap_or(*ends)),
            None => at,
        }
    }

    /// The request for this hop.
    fn build(&self, payload: Payload) -> Option<Request<Payload>> {
        let mut builder = Request::builder()
            .method(Method::from_bytes(self.method.as_bytes()).ok()?)
            .uri(self.target.target())
            .header(HOST, self.target.authority());
        let mut sets_agent = false;
        for (name, value) in &self.headers {
            if request::platform_sets(name) {
                continue;
            }
            if name == USER_AGENT {
                sets_agent = true;
            }
            builder = builder.header(name, value);
        }
        if !sets_agent {
            builder = builder.header(USER_AGENT, "vibe");
        }
        builder.body(payload).ok()
    }

    /// What the answer's head says to do.
    fn next(&self, head: &wire::Head, redirects: u32) -> Next {
        let response = &head.response;
        let status = response.status().as_u16();
        if status < 200 {
            return Next::Fail("unreachable", unreachable_why("protocol"));
        }
        let headers = response.headers();
        let bytes: usize = headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum();
        if headers.len() > wire::MOST_HEADERS || bytes > wire::MOST_HEADER_BYTES {
            return Next::Fail("headers", limit("headers"));
        }
        let Some(location) = redirect_location(status, headers) else {
            return Next::Answer;
        };
        match self.redirect {
            Redirect::Manual => Next::Answer,
            Redirect::Error => Next::Fail("redirect", failure(json!({ "kind": "redirect" }))),
            Redirect::Follow => {
                if redirects >= MOST_REDIRECTS {
                    return Next::Fail("redirects", limit("redirects"));
                }
                let (method, drop_body) = request::after_redirect(status, self.method);
                if !drop_body && matches!(self.body, Payloads::Stream { .. }) {
                    return Next::Fail(
                        "body-not-replayable",
                        failure(json!({ "kind": "body-not-replayable" })),
                    );
                }
                let to = match self.target.url.join(location) {
                    Ok(next) => target::read(next.as_str()),
                    Err(_) => Err(None),
                };
                Next::Follow {
                    to,
                    method,
                    drop_body,
                }
            }
        }
    }

    /// The answer the code gets: the status and headers, and the body as a
    /// stream the host feeds, passed on by a pump that owns the
    /// connection and what the call holds.
    fn answer(&self, head: wire::Head, holding: Holding, redirected: bool) -> Value {
        let wire::Head {
            response,
            connection,
        } = head;
        let status = response.status();
        let (parts, body) = response.into_parts();
        let no_body = self.method == "HEAD" || matches!(status.as_u16(), 204 | 205 | 304);
        let length = parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<u64>().ok());
        let stream = if no_body {
            None
        } else {
            let Some(streams) = self.in_flight.streams.get() else {
                return unavailable();
            };
            let id = streams.create_stream("fetch".to_owned());
            streams.mark_host_fed(id);
            let pump = pump(
                Arc::clone(streams),
                id,
                body,
                connection,
                holding,
                Arc::clone(&self.in_flight.bodies),
                self.in_flight
                    .ends
                    .get()
                    .map(|ends| ends.checked_sub(BEFORE_THE_END).unwrap_or(*ends)),
            );
            self.in_flight.bodies.hold(id, pump, length);
            Some(id)
        };
        let headers: Vec<Value> = parts
            .headers
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "connection" | "transfer-encoding" | "keep-alive"
                )
            })
            .map(|(name, value)| json!([name.as_str(), latin1(value.as_bytes())]))
            .collect();
        ok_to_wire(json!({
            "status": status.as_u16(),
            "statusText": status.canonical_reason().unwrap_or(""),
            "headers": headers,
            "url": self.target.url.as_str(),
            "redirected": redirected,
            "body": stream.map(|id| json!({ "$stream": id })),
        }))
    }
}

/// A header value's bytes as text, one character a byte: what a
/// `Headers` of Application code holds.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// The `location` of a redirect that is followed or handed over.
fn redirect_location(status: u16, headers: &HeaderMap) -> Option<&str> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    headers.get(LOCATION)?.to_str().ok()
}

/// Waits until one second before the request's own end, while the code
/// still holds the body (a body that was handed to the response to be sent,
/// or that the request's end ended, is not cut by it).
async fn cut_at(cap: Option<Instant>, bodies: &Bodies, id: StreamId) {
    match cap {
        Some(cap) => {
            tokio::time::sleep_until(cap).await;
            if bodies.is_ended() || !bodies.holds(id) {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// A wait of the pump on whoever takes the body, bounded as every wait is:
/// `Err(())` when `wait` has not come in [`wire::STALL`] or the request's
/// end drew near.
async fn held_up<T>(
    wait: impl Future<Output = T>,
    cap: Option<Instant>,
    bodies: &Bodies,
    id: StreamId,
) -> Result<T, ()> {
    tokio::select! {
        biased;
        done = wait => Ok(done),
        () = tokio::time::sleep(wire::STALL) => Err(()),
        () = cut_at(cap, bodies, id) => Err(()),
    }
}

/// Waits until what was written into the stream has been taken, as long as
/// bytes keep being taken: each further piece taken has [`wire::STALL`] to
/// come, so that a slow taker is waited for and one that takes nothing is
/// not.
async fn drained(streams: &StreamManager, id: StreamId, bodies: &Bodies) {
    let Some(sender) = streams.sender(id) else {
        return;
    };
    loop {
        let free = sender.capacity();
        if free >= sender.max_capacity() {
            return;
        }
        let wanted = free.saturating_add(1);
        match held_up(sender.reserve_many(wanted), None, bodies, id).await {
            Ok(Ok(_)) => {}
            // Closed (the taker went), or nothing taken in 30 seconds.
            Ok(Err(_)) | Err(()) => return,
        }
    }
}

/// Feeds an answer's body into the stream the code reads, in pieces of at
/// most 64 KiB, as the other side sends it. It holds the connection and the
/// places the call holds, and is owned by the request's registry (or, for a
/// body given back as the response, by the response that sends it): ended
/// with its owner, the connection is closed and the places are given back.
///
/// A body that stops before its end (the other side went away, less than
/// its `content-length`, no byte for 30 seconds, more than 5 GiB) fails the
/// stream: never ended as if whole.
fn pump(
    streams: Arc<StreamManager>,
    id: StreamId,
    mut body: hyper::body::Incoming,
    mut connection: wire::Connection,
    holding: Holding,
    bodies: Arc<Bodies>,
    cap: Option<Instant>,
) -> OwnedTask {
    OwnedTask::new(tokio::spawn(async move {
        let mut total: u64 = 0;
        let mut over = false;
        let outcome: Result<(), &'static str> = 'read: loop {
            // No byte for 30 seconds fails the body; so does the request's
            // own end drawing near while the code is reading it (a body
            // being sent after the request ended is not cut by it).
            let cut = cut_at(cap, &bodies, id);
            let frame = tokio::select! {
                biased;
                frame = body.frame() => Some(frame),
                ended = &mut connection, if !over => {
                    // Whatever the connection's end, the body says what
                    // came of it.
                    drop(ended);
                    over = true;
                    continue;
                }
                () = tokio::time::sleep(wire::STALL) => None,
                () = cut => None,
            };
            match frame {
                None => break Err("no byte of the answer's body for 30 seconds"),
                Some(None) => break Ok(()),
                Some(Some(Err(_))) => break Err("the answer's body ended before its end"),
                Some(Some(Ok(frame))) => {
                    let Ok(data) = frame.into_data() else {
                        // Trailers are nothing the code is given.
                        continue;
                    };
                    let mut at = 0;
                    while at < data.len() {
                        let end = (at + PIECE).min(data.len());
                        let piece = data.slice(at..end);
                        total = total.saturating_add(u64::try_from(piece.len()).unwrap_or(0));
                        if total > wire::MOST_BODY_BYTES {
                            break 'read Err("limit bytes: the answer's body is larger than 5 GiB");
                        }
                        // Whoever takes the body (the code, or the client it is
                        // sent to) has 30 seconds for each piece: a body on
                        // which no byte moves is failed in either direction.
                        match held_up(
                            streams.write_chunk(id, StreamChunk::Data(piece)),
                            cap,
                            &bodies,
                            id,
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            // Cancelled, or the request over.
                            Ok(Err(_)) => break 'read Ok(()),
                            Err(()) => {
                                break 'read Err(
                                    "no byte of the answer's body taken for 30 seconds",
                                );
                            }
                        }
                        at = end;
                    }
                }
            }
        };
        match outcome {
            Ok(()) => {
                drop(
                    held_up(
                        streams.write_chunk(id, StreamChunk::Done),
                        None,
                        &bodies,
                        id,
                    )
                    .await,
                );
            }
            Err(reason) => {
                // What was passed before the failure is taken first, so
                // that a client being sent the body has had it, head
                // included, when the connection ends short.
                drained(&streams, id, &bodies).await;
                // The connection that sends them flushes them before it
                // gives the failure (`SentStream` is pending once first).
                drop(
                    held_up(
                        streams.write_chunk(id, StreamChunk::Error(reason.to_owned())),
                        None,
                        &bodies,
                        id,
                    )
                    .await,
                );
            }
        }
        // The connection is let go as soon as the body is in the stream.
        drop(connection);
        drop(body);
        // The body's place is given back when it was read to its end, or at
        // once when the stream is closed here (cancelled, or the request
        // over); a body being sent empties as the client takes it.
        drained(&streams, id, &bodies).await;
        streams.close_stream(id);
        bodies.release(id);
        drop(holding);
    }))
}
