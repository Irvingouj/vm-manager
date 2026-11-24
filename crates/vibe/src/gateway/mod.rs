//! The Operation gateway: the single place every operation from Application
//! code enters the host, is decided, logged and run.
//!
//! Interface: [`Gateway::operate`], called by the Request runner's host
//! side synchronously, inside the Application's call, with the request in
//! flight read at that moment, whether it is still in flight, and the
//! call's arguments' size as the native measured them
//! (vendor/VENDORED.md, patch 17). The bounds on what the request and the
//! program hold of calls not yet answered are decided first
//! ([`calls`], blackbox/CONTRACT.md, "A host call's arguments are
//! bounded"), then the operation. It answers a [`Decision`]: the
//! refusal's reason, if refused, for the program's `host-call` record
//! (written at once, by the caller), and the answer Application code will
//! receive, as a future. Who is asking is never read from anything the
//! Application passes.
//!
//! Operations: `whoami`, and `query` (needs `read`; a read-only
//! transaction) and `execute` (needs `write`; a read-write transaction),
//! each either on a shared database (the read or the write login) or on the
//! Application's own database (its runtime role, crate::own_db), as the
//! User or, on the own database only, as the Application. Either goes:
//!
//! 1. its arguments are read (malformed: answered so, nothing logged or
//!    sent);
//! 2. the Authorization decision ([`authz::decide`]), from the request in
//!    flight, who it acts as, its target, and for a Resource the serving
//!    Version's Declared access and the Resources and Grants in memory
//!    (crate::resources), for the level the operation needs. While memory
//!    does not know a Resource's Grants (a write of them whose answer was
//!    lost), it is asked with none, and where it would refuse `no-grant`
//!    the operation is refused `grants-unknown`; that refusal's task, once
//!    it has answered, reads the Grants back ([`Resources::read_back`]);
//! 3. the statement without its values (crate::statement);
//! 4. the rest runs as a task on the program's runtime, owned here: a
//!    refused operation's Operation log entry is written whole and the
//!    refusal answered. An allowed one is then stopped, with a whole
//!    `failed` entry and nothing sent, if it is an `execute` on a Resource
//!    without a write login (`read-only-resource`) or if the statement
//!    check (crate::check) refuses it (`statement`). Otherwise its entry is
//!    written `unfinished` before anything is sent (if that fails: refused
//!    `log-unavailable`), the statement runs through the Database executor
//!    (crate::executor) labelled with the request in flight, and the entry
//!    is completed, before the answer.
//!
//! The task does not depend on the request: an operation the Application
//! did not wait for still runs to its end and completes its entry. Tasks
//! are bounded ([`OPERATIONS_AT_ONCE`]); past the bound an operation waits
//! up to [`PERMIT_WAIT`] to start, within the Application's call.
//!
//! Transactions (blackbox/CONTRACT.md, "Transactions"; [`transaction`]):
//! `tx.begin` is decided like an operation needing the `access` it asks for
//! on its target, then, within the request's bound ([`PER_REQUEST`]), a task
//! of its own takes a connection, logs the `begin` and begins. `query` and
//! `execute` with `tx` are looked up among the request's own open
//! transactions (`no-transaction` otherwise, with no entry), decided again
//! as the transaction's target and actor, and handed to its task in the
//! order they are made; the task checks, logs, limits and runs them one at
//! a time on its connection. `tx.commit` and `tx.rollback` are handed over
//! the same way. The request's end, the cap ([`CAP`]) and a limit end a
//! transaction from the host's side.
//!
//! Storage (blackbox/CONTRACT.md, "Storage"; [`storage`]): `storage.head`,
//! `storage.get`, `storage.put`, `storage.delete` and `storage.list`, each
//! on a Storage Resource as the User, decided by the same Authorization
//! decision after matching kinds (a Storage operation only against names
//! declared and registered `storage`, a database operation only against
//! `postgres` ones), logged the same way, and run on the Resource's backend
//! (crate::storage).
//!
//! Later tickets add outbound calls here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use authz::{AccessLevel, Question};
use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;

mod calls;
mod fetch;
mod storage;
mod transaction;

pub(crate) use calls::{Guard, Took};
pub use calls::{ProgramCalls, RequestCalls, WORD_BYTES, WORD_CALLS, WORD_PROGRAM};
pub use fetch::FetchDecision;
pub use transaction::{CAP, PER_REQUEST, Transactions};

use crate::check;
use crate::executor::{Failure, Held, Isolation, Label, Level, Outcome, Param, Pool, Values};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::own_db::{HistoryRead, OwnDatabases};
use crate::registry::ResourceKind;
use crate::resources::{READ_BACK_WAIT, Resource, Resources, level_name};
use crate::runner::InFlight;
use crate::statement;

/// Why an operation was refused, and how to fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// Which condition failed: a kebab-case word (blackbox/CONTRACT.md,
    /// "Refusal reasons").
    pub reason: &'static str,
    /// What else the refusal says (blackbox/CONTRACT.md, "Who is allowed").
    pub details: Map<String, Value>,
    /// One sentence saying how to fix it.
    pub fix: String,
}

/// A call with no request in flight.
pub const NO_REQUEST: &str = "no-request";
/// An operation the host does not know.
pub const UNKNOWN_OPERATION: &str = "unknown-operation";
/// In place of the check of the Grant: memory does not know the Grants on
/// the Resource now (crate::resources).
pub const GRANTS_UNKNOWN: &str = "grants-unknown";
/// The Operation log entry could not be written.
pub const LOG_UNAVAILABLE: &str = "log-unavailable";
/// A Platform administrator disabled the Application (blackbox/CONTRACT.md,
/// "What Platform administrators see and stop: Requests in flight"): the
/// host's, before every other check but `no-request`; not the
/// Authorization decision's.
pub const APPLICATION_DISABLED: &str = "application-disabled";
/// The `host-call` record's reason for arguments that cannot be read. Not
/// a refusal Application code receives: it gets `{ error: { kind:
/// "malformed" } }`.
pub const MALFORMED: &str = "malformed";
/// The `host-call` record's reason for a call past one of the bounds on
/// a call's arguments (blackbox/CONTRACT.md, "A host call's arguments are
/// bounded"). Not a refusal Application code receives: it gets `{ error:
/// { kind: "limit", limit: ... } }`.
pub const LIMIT: &str = "limit";
/// The record's reason for a call that would pass the program's bound.
/// Application code receives `{ error: { kind: "unavailable" } }`.
pub const UNAVAILABLE: &str = "unavailable";

impl Refusal {
    fn new(reason: &'static str, fix: String) -> Self {
        Self {
            reason,
            details: Map::new(),
            fix,
        }
    }

    pub fn no_request() -> Self {
        Self::new(
            NO_REQUEST,
            "no request in flight: call the platform while handling a request, not at the top level of the script, and not from a timer or a reference kept after the request is over".into(),
        )
    }

    pub fn unknown_operation(operation: &str) -> Self {
        Self::new(
            UNKNOWN_OPERATION,
            format!("the platform has no operation {operation:?} on env.VM"),
        )
    }

    /// The Application was disabled by a Platform administrator.
    pub fn application_disabled() -> Self {
        Self::new(
            APPLICATION_DISABLED,
            "a Platform administrator has disabled this Application, so the platform runs none of its operations: nothing in the code is wrong; tell the Builder, who asks their IT".into(),
        )
    }

    /// The Grants on the Resource `name` are not known now: a write of
    /// them whose answer was lost is being found out.
    fn grants_unknown(name: &str) -> Self {
        let mut refusal = Self::new(
            GRANTS_UNKNOWN,
            format!(
                "the platform is finding out the Grants on the Resource {name:?} after a change of them whose answer was lost, so the operation was not run: try again shortly"
            ),
        );
        refusal.details.insert("resource".into(), name.into());
        refusal
    }

    /// `resource` is `None` on the own database.
    fn log_unavailable(resource: Option<&str>) -> Self {
        let mut refusal = Self::new(
            LOG_UNAVAILABLE,
            "the platform could not write the Operation log entry, so the operation was not run: try again shortly".into(),
        );
        if let Some(resource) = resource {
            refusal.details.insert("resource".into(), resource.into());
        }
        refusal
    }

    /// The refusal of the Authorization decision's `reason`, for an
    /// operation needing `needed` on the Resource `name` by `user`. On the
    /// own database only `no-request` refuses, and it names no Resource.
    fn of(
        reason: authz::Reason,
        needed: AccessLevel,
        name: &str,
        user: &str,
        resource: Option<&Resource>,
        kinds: Kinds,
    ) -> Self {
        let mut refusal = match reason {
            authz::Reason::NotDeclared if kinds.told(kinds.declared) => {
                let fix = match (kinds.asked, kinds.declared) {
                    (ResourceKind::Storage, Some(_)) => format!(
                        "the serving Version declares {name:?} with kind \"postgres\", a database, and this is an operation on Storage: declare it with kind \"storage\" (access {:?} or higher), or use it as a database with db(...)",
                        level_name(needed)
                    ),
                    (ResourceKind::Storage, None) => format!(
                        "the serving Version does not declare the Storage Resource {name:?}: add it to declared_access.resources with kind \"storage\" (access {:?} or higher) and send a new Version",
                        level_name(needed)
                    ),
                    (ResourceKind::Postgres, _) => format!(
                        "the serving Version declares {name:?} with kind \"storage\", a place of files, and this is an operation on a database: use its files with storage(...), or declare a database with kind \"postgres\""
                    ),
                };
                let mut refusal = Self::new("not-declared", fix);
                kinds.detail(&mut refusal.details, "declared_kind", kinds.declared);
                refusal
            }
            authz::Reason::NotRegistered if kinds.told(kinds.registered) => {
                let fix = match (kinds.asked, kinds.registered) {
                    (ResourceKind::Storage, Some(_)) => format!(
                        "the Resource {name:?} is registered as a database, not as Storage: declare it with kind \"postgres\" and use it as a database, or ask a Platform administrator for the Storage Resource you need"
                    ),
                    (ResourceKind::Storage, None) => format!(
                        "no Storage Resource named {name:?} is registered: check the name, or ask a Platform administrator to register it"
                    ),
                    (ResourceKind::Postgres, _) => format!(
                        "the Resource {name:?} is registered as Storage (a place of files), not as a database: declare it with kind \"storage\" and use its files with storage(...)"
                    ),
                };
                let mut refusal = Self::new("not-registered", fix);
                kinds.detail(&mut refusal.details, "registered_kind", kinds.registered);
                refusal
            }
            authz::Reason::NoRequest => return Self::no_request(),
            authz::Reason::NotOwnDatabase => Self::new(
                "not-own-database",
                format!(
                    "acting as the Application is only for its own database, not the Resource {name:?}: act as the User"
                ),
            ),
            authz::Reason::NotDeclared => Self::new(
                "not-declared",
                format!(
                    "the serving Version does not declare the Resource {name:?}: add it to declared_access.resources (kind \"postgres\", access {:?} or higher) and send a new Version",
                    level_name(needed)
                ),
            ),
            authz::Reason::NotRegistered => Self::new(
                "not-registered",
                format!(
                    "no Resource named {name:?} is registered: check the name, or ask a Platform administrator to register it"
                ),
            ),
            authz::Reason::DeclaredLower { declared, needed } => {
                let mut refusal = Self::new(
                    "declared-lower",
                    format!(
                        "the serving Version declares {name:?} at {:?} and this operation needs {:?}: declare it at {:?} and send a new Version",
                        level_name(declared),
                        level_name(needed),
                        level_name(needed)
                    ),
                );
                refusal
                    .details
                    .insert("declared".into(), level_name(declared).into());
                refusal
                    .details
                    .insert("needed".into(), level_name(needed).into());
                refusal
            }
            authz::Reason::NoGrant { held, needed } => {
                let contact = resource.map(|r| r.contact());
                let contact = contact.as_deref().unwrap_or("");
                let holds = held.map_or_else(
                    || "no Grant".to_owned(),
                    |level| format!("only {:?}", level_name(level)),
                );
                let mut refusal = Self::new(
                    "no-grant",
                    format!(
                        "the User {user:?} holds {holds} on {name:?} through their Groups and this operation needs {:?}: ask {contact} for access",
                        level_name(needed)
                    ),
                );
                refusal.details.insert("user".into(), user.into());
                refusal
                    .details
                    .insert("needed".into(), level_name(needed).into());
                refusal
                    .details
                    .insert("held".into(), held.map(level_name).into());
                refusal.details.insert("contact".into(), contact.into());
                refusal
            }
        };
        refusal.details.insert("resource".into(), name.into());
        refusal
    }

    /// `{ refused: { refusal: { reason, ... }, fix } }`, as Application
    /// code receives it.
    pub fn to_wire(&self) -> Value {
        let mut refusal = self.details.clone();
        refusal.insert("reason".into(), self.reason.into());
        json!({ "refused": { "refusal": refusal, "fix": self.fix } })
    }
}

/// The kinds an operation was decided with: the kind it works on, and the
/// other kind the name is declared or registered as, if it is.
#[derive(Clone, Copy, Debug)]
struct Kinds {
    asked: ResourceKind,
    declared: Option<ResourceKind>,
    registered: Option<ResourceKind>,
}

impl Kinds {
    /// Whether a `not-declared` (with `other` the kind it is declared as)
    /// or a `not-registered` (`other` the kind registered) says the kinds:
    /// always for Storage; for a database only when the name is Storage's
    /// there, so that nothing else of a database's refusals changes.
    fn told(self, other: Option<ResourceKind>) -> bool {
        self.asked == ResourceKind::Storage || other.is_some()
    }

    /// `kind`, and `field` (`declared_kind` or `registered_kind`) when the
    /// name is of the `other` kind.
    fn detail(self, details: &mut Map<String, Value>, field: &str, other: Option<ResourceKind>) {
        details.insert("kind".into(), self.asked.name().into());
        if let Some(other) = other {
            details.insert(field.into(), other.name().into());
        }
    }
}

/// `{ ok: value }`, as Application code receives an allowed answer.
pub fn ok_to_wire(value: Value) -> Value {
    // Moved in, not serialised again as `json!` would.
    let mut wire = Map::with_capacity(1);
    wire.insert("ok".into(), value);
    Value::Object(wire)
}

/// `{ error: { kind, ... } }`.
fn error_to_wire(error: Value) -> Value {
    let mut wire = Map::with_capacity(1);
    wire.insert("error".into(), error);
    Value::Object(wire)
}

/// Arguments that cannot be read: `{ error: { kind: "malformed", message } }`,
/// nothing logged or sent; the `host-call` record says `malformed`.
fn malformed(problem: &str) -> Decision {
    Decision {
        refused: Some(MALFORMED),
        answer: Box::pin(std::future::ready(error_to_wire(
            json!({ "kind": "malformed", "message": problem }),
        ))),
    }
}

/// A call that would pass one of its request's bounds
/// (blackbox/CONTRACT.md, "A host call's arguments are bounded"):
/// `{ error: { kind: "limit", limit: which } }`, answered at once, with
/// no Operation log entry; the `host-call` record says `limit`.
fn call_limit(which: &'static str) -> Decision {
    Decision {
        refused: Some(LIMIT),
        answer: Box::pin(std::future::ready(error_to_wire(
            json!({ "kind": "limit", "limit": which }),
        ))),
    }
}

/// A call that would pass the program's: `{ error: { kind: "unavailable" } }`,
/// answered at once, with no Operation log entry; the record says
/// `unavailable`.
fn call_unavailable() -> Decision {
    Decision {
        refused: Some(UNAVAILABLE),
        answer: Box::pin(std::future::ready(error_to_wire(
            json!({ "kind": "unavailable" }),
        ))),
    }
}

/// An id that is not an open transaction of the request in flight: the same
/// answer whatever the reason, and no Operation log entry. The `host-call`
/// record says `no-transaction`.
fn no_transaction() -> Decision {
    Decision {
        refused: Some(transaction::NO_TRANSACTION),
        answer: Box::pin(std::future::ready(transaction::no_transaction_wire())),
    }
}

/// Hands a command to a transaction's task, at once, so that commands run
/// in the order they were made; answers what the task answers. A task that
/// has ended (or ends before it gets to the command) is `no-transaction`. A
/// transaction whose queue is full ([`transaction::QUEUE`] commands waiting)
/// answers `unavailable`.
fn hand_over(
    commands: &mpsc::Sender<transaction::Command>,
    command: impl FnOnce(oneshot::Sender<Value>) -> transaction::Command,
) -> Decision {
    let (reply, answer) = oneshot::channel();
    match commands.try_send(command(reply)) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Closed(_)) => return no_transaction(),
        Err(mpsc::error::TrySendError::Full(_)) => {
            log::record(
                "operations-busy",
                json!({ "problem": "too many operations waiting in one transaction" }),
            );
            return Decision::allowed(error_to_wire(json!({ "kind": "unavailable" })));
        }
    }
    let answer = async move {
        match tokio::time::timeout(ANSWER_WAIT, answer).await {
            Ok(Ok(answer)) => answer,
            // The task ended without getting to it.
            Ok(Err(_)) => transaction::no_transaction_wire(),
            Err(_) => error_to_wire(json!({ "kind": "unavailable" })),
        }
    };
    Decision {
        refused: None,
        answer: Box::pin(answer),
    }
}

/// What Application code will receive.
pub type Answer = Pin<Box<dyn Future<Output = Value> + Send>>;

/// What the gateway decided about one call.
pub struct Decision {
    /// The reason, when refused, for the `host-call` record.
    pub refused: Option<&'static str>,
    pub answer: Answer,
}

impl Decision {
    fn allowed(answer: Value) -> Self {
        Self {
            refused: None,
            answer: Box::pin(std::future::ready(answer)),
        }
    }

    fn refused(refusal: &Refusal) -> Self {
        Self {
            refused: Some(refusal.reason),
            answer: Box::pin(std::future::ready(refusal.to_wire())),
        }
    }
}

/// Decides and runs the operations Application code asks for.
pub trait Gateway: Send + Sync {
    /// One operation, for the request that `in_flight` is; `active` says
    /// whether it is still in flight, and `size` is the call's
    /// arguments' bytes as the native measured them
    /// (vendor/VENDORED.md, patch 17) — 0 for arguments not given. Must
    /// decide without waiting, and first against the request's and the
    /// program's bounds on calls not yet answered
    /// (blackbox/CONTRACT.md, "A host call's arguments are bounded").
    fn operate(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        operation: &str,
        args: Value,
        size: u64,
    ) -> Decision;

    /// The program's count of host calls not yet answered, when this
    /// gateway keeps one: asked once per request by the engine, to be
    /// asked before a call's arguments are copied or parsed
    /// (vendor/VENDORED.md, patch 17). `None` when the gateway holds
    /// nothing, and then no call is refused before it is read.
    fn held_calls(&self) -> Option<Arc<ProgramCalls>> {
        None
    }

    /// A `fetch` of Application code (blackbox/CONTRACT.md, "Outbound
    /// calls"), decided now and run in a task. A gateway with no outbound
    /// calls refuses it as an operation it does not know.
    fn fetch(&self, _in_flight: &Arc<InFlight>, active: bool, _args: &Value) -> FetchDecision {
        let refusal = if active {
            Refusal::unknown_operation("fetch")
        } else {
            Refusal::no_request()
        };
        FetchDecision {
            decision: Decision::refused(&refusal),
            host: None,
        }
    }
}

/// Operations under way at once; past it an operation waits to start.
pub const OPERATIONS_AT_ONCE: usize = 256;

/// How long an operation waits to start when [`OPERATIONS_AT_ONCE`] are
/// under way; then it is answered `{ error: { kind: "unavailable" } }`.
pub const PERMIT_WAIT: Duration = Duration::from_secs(5);

/// The longest a `history` read may take, the shipment before it included
/// (blackbox/CONTRACT.md, "Reading: the operation `history`: Limits").
const HISTORY_TIME: Duration = Duration::from_secs(10);

/// The backstop on waiting for an operation's answer. Every step of the
/// task has its own limit (the log's writes, the pool's checkout and
/// connect, the statement and its grace period), which together stay
/// below this.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// The tasks running operations, on the program's runtime.
struct Operations {
    runtime: Handle,
    /// Owned here, reaped as new ones start; at most
    /// [`OPERATIONS_AT_ONCE`] running, each holding a permit.
    tasks: Mutex<JoinSet<()>>,
    permits: Arc<Semaphore>,
}

impl Operations {
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn_on(task, &self.runtime);
    }
}

/// The platform's operations.
pub struct PlatformGateway {
    resources: Arc<Resources>,
    /// The file threads, the budget of bodies and the cursors' key.
    storage: Arc<crate::storage::Storage>,
    own: Arc<OwnDatabases>,
    log: Arc<OperationLog>,
    /// What outbound calls are decided with: the list of hosts, from
    /// memory, the ranges the operator opened, and the program's places.
    calls: Arc<fetch::Calls>,
    operations: Arc<Operations>,
    /// The bytes of the host calls not yet answered of all requests
    /// (blackbox/CONTRACT.md, "A host call's arguments are bounded"): the
    /// program's 256 MiB and each Application's share.
    held: Arc<ProgramCalls>,
}

impl PlatformGateway {
    /// `runtime` is where operations run: the program's own, where the
    /// platform database's and the shared databases' connections live.
    /// `mentions` is the program's one-a-minute `share-full` memory, shared
    /// with the file threads and the storage bodies.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of one gateway, each named: what it works on and what outbound calls are decided with"
    )]
    pub fn new(
        resources: Arc<Resources>,
        storage: Arc<crate::storage::Storage>,
        own: Arc<OwnDatabases>,
        log: Arc<OperationLog>,
        outbound: Arc<crate::outbound::Outbound>,
        outbound_private: crate::outbound::PrivateRanges,
        itself: crate::outbound::Itself,
        mentions: Arc<crate::shares::Mentions>,
        runtime: Handle,
    ) -> Self {
        Self {
            resources,
            storage,
            own,
            log,
            calls: Arc::new(fetch::Calls::new(
                outbound,
                outbound_private,
                itself,
                Arc::clone(&mentions),
            )),
            operations: Arc::new(Operations {
                runtime,
                tasks: Mutex::new(JoinSet::new()),
                permits: Arc::new(Semaphore::new(OPERATIONS_AT_ONCE)),
            }),
            held: Arc::new(ProgramCalls::new(mentions)),
        }
    }

    fn on_database(
        &self,
        kind: Kind,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
        held: Option<Guard>,
    ) -> Decision {
        let call = match Call::read(args) {
            Ok(call) => call,
            // With no request in flight nothing is answered but that.
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => return malformed(problem),
        };
        let target = match call.target {
            Target::Tx(id) => return self.in_transaction(kind, in_flight, active, id, call, held),
            target => target,
        };
        let actor = call.actor;
        let (on, refusal) = self.decide(kind.needed(), actor, target, in_flight, active);
        let work = self.work(kind, in_flight, active, actor, on, call, refusal, held);
        if !active {
            // Refused (`no-request` comes first). Answered at once: the
            // runtime may not settle a call made while no event runs, as
            // at the script's top level. Its entry is written meanwhile.
            return self.refused_unawaited(work);
        }
        let refused = work.refusal.as_ref().map(|r| r.reason);
        self.dispatch(work, refused, Work::run)
    }

    /// `query` or `execute` with `tx`: in the transaction `id` of the
    /// request in flight, as its target and actor, decided again. An id
    /// that is not one of its open transactions answers `no-transaction`,
    /// with no Operation log entry. An allowed statement is handed to the
    /// transaction's task at once, so statements run in the order they were
    /// made.
    fn in_transaction(
        &self,
        kind: Kind,
        in_flight: &Arc<InFlight>,
        active: bool,
        id: &str,
        call: Call<'_>,
        held: Option<Guard>,
    ) -> Decision {
        // A transaction belongs to the request in flight; with none, there
        // is nothing to look it up in.
        if !active {
            return Decision::refused(&Refusal::no_request());
        }
        let Some(found) = in_flight.transactions.find(id) else {
            // Its transactions were ended by the disable.
            if in_flight.stop.is_stopped() {
                return Decision::refused(&Refusal::application_disabled());
            }
            return no_transaction();
        };
        let target = match &found.resource {
            None => Target::Own,
            Some(name) => Target::Resource(name),
        };
        // In a `write` transaction a `query` runs as the write login in a
        // read-write transaction, where it could write: it needs `write`.
        let needed = kind.needed().max(found.access);
        let (on, refusal) = self.decide(needed, found.actor, target, in_flight, active);
        let work = Work {
            transaction: Some(Arc::clone(&found.id)),
            ..self.work(
                kind,
                in_flight,
                active,
                found.actor,
                on,
                call,
                refusal,
                held,
            )
        };
        if let Some(refusal) = &work.refusal {
            let refused = Some(refusal.reason);
            return self.dispatch(work, refused, Work::run);
        }
        hand_over(&found.commands, |reply| transaction::Command::Statement {
            work: Box::new(work),
            reply,
        })
    }

    /// `tx.begin`: decided like an operation needing its `access` on its
    /// target; then, within the request's bound on open transactions, a
    /// task of its own takes a connection and begins it.
    fn begin(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
        held: Option<Guard>,
    ) -> Decision {
        let begin = match Begin::read(args) {
            Ok(begin) => begin,
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => return malformed(problem),
        };
        let (on, refusal) = self.decide(begin.access, begin.actor, begin.target, in_flight, active);
        let work = Work {
            kind: Kind::Begin(begin.access),
            in_flight: Arc::clone(in_flight),
            active,
            actor: begin.actor,
            target: on,
            sql: String::new(),
            params: Vec::new(),
            values: Values::Typed,
            statement: "begin".to_owned(),
            refusal,
            transaction: None,
            history: None,
            _held: held,
            log: Arc::clone(&self.log),
            own: Arc::clone(&self.own),
        };
        if !active {
            return self.refused_unawaited(work);
        }
        if let Some(refusal) = &work.refusal {
            let refused = Some(refusal.reason);
            return self.dispatch(work, refused, Work::run);
        }
        let reserved =
            in_flight
                .transactions
                .reserve(work.resource_name(), begin.actor, begin.access);
        let made = match reserved {
            transaction::Reserved::Made(made) => made,
            // The request ended meanwhile.
            transaction::Reserved::Ended => return Decision::refused(&Refusal::no_request()),
            transaction::Reserved::Full => {
                return self.dispatch(work, None, |work: Work| async move {
                    let answer = error_to_wire(json!({ "kind": "limit", "limit": "transactions" }));
                    work.failed_unsent("transactions", answer).await
                });
            }
        };
        let level = match begin.access {
            AccessLevel::Write => Level::Write,
            AccessLevel::Read | AccessLevel::Structure => Level::Read,
        };
        let opened = transaction::Opened {
            id: made.id,
            in_flight: work.in_flight,
            actor: begin.actor,
            target: work.target,
            level,
            isolation: begin.isolation,
            log: work.log,
            own: work.own,
        };
        let (began, answer) = oneshot::channel();
        self.operations
            .spawn(transaction::run(opened, made.commands, made.alive, began));
        let answer = async move {
            match tokio::time::timeout(ANSWER_WAIT, answer).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) | Err(_) => error_to_wire(json!({ "kind": "unavailable" })),
            }
        };
        Decision {
            refused: None,
            answer: Box::pin(answer),
        }
    }

    /// `tx.commit` (`commit`) or `tx.rollback`: handed to the transaction's
    /// task, after the statements made before it. The call's guard does
    /// not travel: only the transaction's id does, so the host has let
    /// go of the call's arguments when this returns, and the call stops
    /// counting then, as the contract has it.
    fn end(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
        // Dropped when this returns, unread: the call stops counting then.
        _held: Option<Guard>,
        commit: bool,
    ) -> Decision {
        let id = match args.as_object().and_then(|args| args.get("tx")) {
            Some(Value::String(id)) => id,
            _ if !active => return Decision::refused(&Refusal::no_request()),
            _ => return malformed("`tx` must be the id of a transaction, as a string"),
        };
        if !active {
            return Decision::refused(&Refusal::no_request());
        }
        if in_flight.stop.is_stopped() {
            return Decision::refused(&Refusal::application_disabled());
        }
        let Some(found) = in_flight.transactions.find(id) else {
            return no_transaction();
        };
        hand_over(&found.commands, |reply| transaction::Command::End {
            commit,
            reply,
        })
    }

    /// `history` (blackbox/CONTRACT.md, "Reading: the operation `history`"):
    /// decided as a `query` on the own database is; then, in a task of its
    /// own, logged, and the history read ([`OwnDatabases::read_history`]).
    fn history(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
        held: Option<Guard>,
    ) -> Decision {
        let (actor, read) = match read_history(args) {
            Ok(read) => read,
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => return malformed(problem),
        };
        let (on, refusal) = self.decide(AccessLevel::Read, actor, Target::Own, in_flight, active);
        let statement = format!("history \"{}\".\"{}\"", read.schema, read.table);
        let work = Work {
            kind: Kind::History,
            in_flight: Arc::clone(in_flight),
            active,
            actor,
            target: on,
            sql: String::new(),
            params: Vec::new(),
            values: read.values,
            statement,
            refusal,
            transaction: None,
            history: Some(Box::new(read)),
            _held: held,
            log: Arc::clone(&self.log),
            own: Arc::clone(&self.own),
        };
        if !active {
            return self.refused_unawaited(work);
        }
        let refused = work.refusal.as_ref().map(|r| r.reason);
        self.dispatch(work, refused, Work::run)
    }

    /// The Authorization decision for an operation needing `needed` on
    /// `target`, acting as `actor`, for the request in flight: what it is
    /// on, and the refusal if refused.
    fn decide(
        &self,
        needed: AccessLevel,
        actor: authz::Actor,
        target: Target<'_>,
        in_flight: &InFlight,
        active: bool,
    ) -> (On, Option<Refusal>) {
        self.decide_kind(
            needed,
            actor,
            target,
            ResourceKind::Postgres,
            in_flight,
            active,
        )
    }

    /// [`PlatformGateway::decide`] for an operation on Resources of `kind`.
    fn decide_kind(
        &self,
        needed: AccessLevel,
        actor: authz::Actor,
        target: Target<'_>,
        kind: ResourceKind,
        in_flight: &InFlight,
        active: bool,
    ) -> (On, Option<Refusal>) {
        let (on, refusal) = self.decide_access(needed, actor, target, kind, in_flight, active);
        // Disabled: refused before every other check but `no-request`.
        if active && in_flight.stop.is_stopped() {
            return (on, Some(Refusal::application_disabled()));
        }
        (on, refusal)
    }

    /// The Authorization decision of [`PlatformGateway::decide`].
    fn decide_access(
        &self,
        needed: AccessLevel,
        actor: authz::Actor,
        target: Target<'_>,
        kind: ResourceKind,
        in_flight: &InFlight,
        active: bool,
    ) -> (On, Option<Refusal>) {
        let groups: Vec<&str> = in_flight.user.groups.iter().map(String::as_str).collect();
        match target {
            Target::Own | Target::Tx(_) => {
                let decision = authz::decide(&Question {
                    request_in_flight: active,
                    actor,
                    own_database: true,
                    groups: &groups,
                    declared: None,
                    registered: false,
                    grants: &[],
                    needed,
                });
                let refusal = match decision {
                    authz::Decision::Allow => None,
                    // Only `no-request` refuses on the own database.
                    authz::Decision::Refuse(_) => Some(Refusal::no_request()),
                };
                (On::Own, refusal)
            }
            Target::Resource(name) => {
                // Only Resources of the operation's kind are its: a name
                // declared or registered as the other kind is as if not
                // declared or not registered, and the refusal says so.
                let found = self.resources.get(name);
                let declaration = in_flight.declared.get(name).copied();
                let kinds = Kinds {
                    asked: kind,
                    declared: declaration.map(|d| d.kind).filter(|k| *k != kind),
                    registered: found.as_ref().map(|r| r.kind).filter(|k| *k != kind),
                };
                let declared = declaration.filter(|d| d.kind == kind).map(|d| d.level);
                let resource = found.filter(|r| r.kind == kind);
                let decide = |grants: &[authz::Grant<'_>]| {
                    authz::decide(&Question {
                        request_in_flight: active,
                        actor,
                        own_database: false,
                        groups: &groups,
                        declared,
                        registered: resource.is_some(),
                        grants,
                        needed,
                    })
                };
                // Grants memory does not know now: asked with none, so that
                // the refusals checked before the Grant are given as ever,
                // and `grants-unknown` in place of the Grant's.
                let (decision, known) = match &resource {
                    Some(resource) => match resource.with_grants(decide) {
                        Some(decision) => (decision, true),
                        None => (decide(&[]), false),
                    },
                    None => (decide(&[]), true),
                };
                let refusal = match decision {
                    authz::Decision::Allow
                    | authz::Decision::Refuse(authz::Reason::NoGrant { .. })
                        if !known =>
                    {
                        Some(Refusal::grants_unknown(name))
                    }
                    authz::Decision::Allow => None,
                    authz::Decision::Refuse(reason) => Some(Refusal::of(
                        reason,
                        needed,
                        name,
                        &in_flight.user.id,
                        resource.as_deref(),
                        kinds,
                    )),
                };
                // Copied: the operation runs on another thread, beyond the
                // Application's call that lent the name.
                let on = On::Resource {
                    name: name.to_owned(),
                    resource,
                };
                (on, refusal)
            }
        }
    }

    /// The operation `call` asks for, decided: what is sent is kept only
    /// for an operation that will be sent.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of one decided operation, each named"
    )]
    fn work(
        &self,
        kind: Kind,
        in_flight: &Arc<InFlight>,
        active: bool,
        actor: authz::Actor,
        target: On,
        call: Call<'_>,
        refusal: Option<Refusal>,
        held: Option<Guard>,
    ) -> Work {
        let (sql, params) = match refusal {
            // Copied: sent from another thread, after the call.
            None => (call.sql.to_owned(), call.params),
            Some(_) => (String::new(), Vec::new()),
        };
        Work {
            kind,
            in_flight: Arc::clone(in_flight),
            active,
            actor,
            target,
            sql,
            params,
            values: call.values,
            statement: statement::without_values(call.sql),
            refusal,
            transaction: None,
            history: None,
            _held: held,
            log: Arc::clone(&self.log),
            own: Arc::clone(&self.own),
        }
    }

    /// Runs `task` on `work` in a task of its own, owned here, once one of
    /// [`OPERATIONS_AT_ONCE`] is free (waiting up to [`PERMIT_WAIT`]), and
    /// answers what it answers.
    fn dispatch<W, F, Fut>(&self, work: W, refused: Option<&'static str>, task: F) -> Decision
    where
        W: Dispatched,
        F: FnOnce(W) -> Fut + Send + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        let operations = Arc::clone(&self.operations);
        // A refusal `grants-unknown`: once it is answered, its task reads
        // the Resource's Grants back, so that the operations after it are
        // decided by the Grants the database has once the write is
        // settled.
        let read_back = match (work.refusal(), work.on()) {
            (
                Some(refusal),
                On::Resource {
                    resource: Some(resource),
                    ..
                },
            ) if refusal.reason == GRANTS_UNKNOWN => {
                Some((Arc::clone(&self.resources), Arc::clone(resource)))
            }
            _ => None,
        };
        let answer = async move {
            let permits = Arc::clone(&operations.permits);
            let Ok(Ok(permit)) = tokio::time::timeout(PERMIT_WAIT, permits.acquire_owned()).await
            else {
                return work.busy();
            };
            let (reply, answer) = oneshot::channel();
            operations.spawn(async move {
                // The Application may have stopped waiting; the entry is
                // complete either way.
                reply.send(task(work).await).ok();
                if let Some((resources, resource)) = read_back {
                    let deadline = tokio::time::Instant::now() + READ_BACK_WAIT;
                    resources.read_back(&resource, deadline).await;
                }
                drop(permit);
            });
            match tokio::time::timeout(ANSWER_WAIT, answer).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) | Err(_) => error_to_wire(json!({ "kind": "unavailable" })),
            }
        };
        Decision {
            refused,
            answer: Box::pin(answer),
        }
    }
}

impl PlatformGateway {
    /// Waits, up to `limit`, for every operation's task under way to end
    /// (its Operation log entry written): for a caller that reads the log
    /// right after its request (the local run). Answers whether all ended;
    /// those still running at `limit` are ended with the set that held
    /// them.
    pub async fn drain(&self, limit: Duration) -> bool {
        let mut taken = {
            let mut tasks = self
                .operations
                .tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        tokio::time::timeout(limit, async { while taken.join_next().await.is_some() {} })
            .await
            .is_ok()
    }

    /// Answers `work`'s refusal now and writes its entry in a task of its
    /// own. Past [`OPERATIONS_AT_ONCE`] the entry is not written, and the
    /// program's log says so.
    fn refused_unawaited(&self, work: Work) -> Decision {
        self.refused_unawaited_with(work, Work::run)
    }

    /// [`PlatformGateway::refused_unawaited`], its entry written by `run`.
    fn refused_unawaited_with<W, F, Fut>(&self, work: W, run: F) -> Decision
    where
        W: Dispatched,
        F: FnOnce(W) -> Fut + Send + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        let Some(refusal) = work.refusal() else {
            return Decision::refused(&Refusal::no_request());
        };
        let decision = Decision::refused(refusal);
        match Arc::clone(&self.operations.permits).try_acquire_owned() {
            Ok(permit) => self.operations.spawn(async move {
                run(work).await;
                drop(permit);
            }),
            Err(_) => {
                work.busy();
            }
        }
        decision
    }
}

impl Gateway for PlatformGateway {
    fn operate(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        operation: &str,
        args: Value,
        size: u64,
    ) -> Decision {
        // The bounds on what the request and the program hold of calls not
        // yet answered (blackbox/CONTRACT.md, "A host call's arguments are
        // bounded"), decided first, before anything else about the call:
        // whatever the operation's name, known to the host or not. The
        // call's own bound the native decided before anything of it was
        // copied (vendor/VENDORED.md, patch 17). With no request in flight
        // too: code that runs while its script loads can make as many calls
        // as code in a request can, and a refusal that has an Operation log
        // entry keeps its arguments until the entry is written.
        let held = match in_flight
            .calls
            .take(&self.held, &in_flight.application, size)
        {
            // The guard travels with the call's arguments: into the work
            // below, and so into the future that waits for a permit and
            // the task that runs the operation, which outlives a request
            // that ended. A call answered at once drops it here, holding
            // nothing after it returns.
            Took::Taken(guard) => Some(guard),
            Took::Calls => return call_limit("calls"),
            Took::Bytes => return call_limit("arguments"),
            // The Application's share is answered as the program's bound
            // is; the share wrote its own `share-full` record when it
            // refused.
            Took::Share | Took::Program => return call_unavailable(),
        };
        if let Some(op) = storage::Op::of(operation) {
            // Owned: a put's body is moved on, never copied.
            return self.storage(op, in_flight, active, args, held);
        }
        let args = &args;
        match operation {
            "query" => self.on_database(Kind::Query, in_flight, active, args, held),
            "execute" => self.on_database(Kind::Execute, in_flight, active, args, held),
            "history" => self.history(in_flight, active, args, held),
            "tx.begin" => self.begin(in_flight, active, args, held),
            "tx.commit" => self.end(in_flight, active, args, held, true),
            "tx.rollback" => self.end(in_flight, active, args, held, false),
            _ if !active => Decision::refused(&Refusal::no_request()),
            _ if in_flight.stop.is_stopped() => Decision::refused(&Refusal::application_disabled()),
            // Nothing the Application passes is read: who is asking is the
            // request in flight.
            "whoami" => Decision::allowed(ok_to_wire(json!({
                "user": in_flight.user.to_json(),
                "application": &*in_flight.application,
                "version": in_flight.version,
                "request": &*in_flight.request,
            }))),
            other => Decision::refused(&Refusal::unknown_operation(other)),
        }
    }

    /// The program's count of host calls not yet answered
    /// (blackbox/CONTRACT.md, "A host call's arguments are bounded"),
    /// for the engine to ask before a call's arguments are copied or
    /// parsed (vendor/VENDORED.md, patch 17).
    fn held_calls(&self) -> Option<Arc<ProgramCalls>> {
        Some(Arc::clone(&self.held))
    }

    fn fetch(&self, in_flight: &Arc<InFlight>, active: bool, args: &Value) -> FetchDecision {
        self.fetch_call(in_flight, active, args)
    }
}

/// An operation on a database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `query`: needs `read`; a read-only transaction, as the read login of
    /// a shared database or the own database's runtime role.
    Query,
    /// `execute`: needs `write`; a read-write transaction, as the write
    /// login or the runtime role.
    Execute,
    /// `tx.begin`: needs the `access` it asks for. Only its refusals and
    /// failures before anything is sent run as a [`Work`]; the rest is the
    /// transaction's own task.
    Begin(AccessLevel),
    /// `history`: needs `read`, on the own database only.
    History,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Execute => "execute",
            Self::Begin(_) => "begin",
            Self::History => "history",
        }
    }

    fn needed(self) -> AccessLevel {
        match self {
            Self::Query | Self::History => AccessLevel::Read,
            Self::Execute => AccessLevel::Write,
            Self::Begin(access) => access,
        }
    }
}

/// What an operation is on.
#[derive(Clone, Copy, Debug)]
enum Target<'a> {
    /// The Application's own database.
    Own,
    /// A Resource, by name.
    Resource(&'a str),
    /// A transaction of the request in flight, by its id: its target and
    /// actor are the transaction's.
    Tx(&'a str),
}

/// What a call's arguments name: `own: true` or `resource`, and `as`.
fn read_target(args: &Map<String, Value>) -> Result<(Target<'_>, authz::Actor), &'static str> {
    const TARGET: &str = "name exactly one target: `own: true` for the Application's own database, or `resource` with the name of a Resource";
    let own = match args.get("own") {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return Err("`own` must be exactly true, and then no `resource` is given"),
    };
    let target = match (own, args.get("resource")) {
        (true, None) => Target::Own,
        (false, Some(Value::String(name))) => Target::Resource(name),
        (false, Some(_)) => {
            return Err("`resource` must be the name of a Resource, as a string");
        }
        (true, Some(_)) | (false, None) => return Err(TARGET),
    };
    let actor = match args.get("as") {
        None => authz::Actor::User,
        Some(Value::String(actor)) if actor == "user" => authz::Actor::User,
        Some(Value::String(actor)) if actor == "application" => authz::Actor::Application,
        Some(_) => return Err("`as` must be \"user\" (the default) or \"application\""),
    };
    Ok((target, actor))
}

/// The arguments of `tx.begin` (blackbox/CONTRACT.md, "Transactions: Through
/// the raw interface"): a target and actor as for `query`, `access`
/// (`"read"` or `"write"`) and, optionally, `isolation`.
struct Begin<'a> {
    target: Target<'a>,
    actor: authz::Actor,
    access: AccessLevel,
    isolation: Option<Isolation>,
}

impl<'a> Begin<'a> {
    fn read(args: &'a Value) -> Result<Self, &'static str> {
        let Some(args) = args.as_object() else {
            return Err(
                "the arguments must be an object: { own: true, as, access, isolation } or { resource, as, access, isolation }",
            );
        };
        let (target, actor) = read_target(args)?;
        let access = match args.get("access") {
            Some(Value::String(access)) if access == "read" => AccessLevel::Read,
            Some(Value::String(access)) if access == "write" => AccessLevel::Write,
            _ => return Err("`access` must be \"read\" or \"write\""),
        };
        let isolation = match args.get("isolation") {
            None => None,
            Some(Value::String(name)) => Some(Isolation::of(name).ok_or(ISOLATION)?),
            Some(_) => return Err(ISOLATION),
        };
        Ok(Self {
            target,
            actor,
            access,
            isolation,
        })
    }
}

const ISOLATION: &str =
    "`isolation` must be \"read committed\", \"repeatable read\" or \"serializable\", or not given";

/// The arguments of `query` and `execute`, read strictly (blackbox/CONTRACT.md,
/// "Operations on a shared database" and "Operations on the own database"):
/// exactly one of `own: true` and `resource`; `as` absent, `"user"` or
/// `"application"`; or instead of all three `tx`, a transaction's id
/// ("Transactions"); `values` absent or `"text"` ("The SDK: Values").
/// Fields it does not have are ignored.
struct Call<'a> {
    target: Target<'a>,
    actor: authz::Actor,
    sql: &'a str,
    params: Vec<Param>,
    values: Values,
}

impl<'a> Call<'a> {
    fn read(args: &'a Value) -> Result<Self, &'static str> {
        let Some(args) = args.as_object() else {
            return Err(
                "the arguments must be an object: { own: true, as, sql, params }, { resource, as, sql, params } or { tx, sql, params }",
            );
        };
        let (target, actor) = match args.get("tx") {
            Some(Value::String(id)) => {
                if ["resource", "own", "as"]
                    .iter()
                    .any(|key| args.contains_key(*key))
                {
                    return Err(
                        "`tx` names the transaction, and with it the target and the actor: give no `resource`, `own` or `as` beside it",
                    );
                }
                // The transaction's own actor is used, not this.
                (Target::Tx(id), authz::Actor::User)
            }
            Some(_) => return Err("`tx` must be the id of a transaction, as a string"),
            None => read_target(args)?,
        };
        let values = match args.get("values") {
            None => Values::Typed,
            Some(Value::String(values)) if values == "text" => Values::Text,
            Some(_) => return Err("`values` must be \"text\", or not given"),
        };
        let Some(sql) = args.get("sql").and_then(Value::as_str) else {
            return Err("`sql` must be one statement, as a string");
        };
        if sql.contains('\0') {
            return Err("`sql` must not hold a NUL character");
        }
        let params = match args.get("params") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(params)) => params
                .iter()
                .map(|param| match param {
                    Value::Null => Ok(Param::Null),
                    // Copied: sent from another thread, after the call.
                    Value::String(text) => Ok(Param::Text(text.clone())),
                    Value::Number(number) => Ok(Param::Text(number.to_string())),
                    Value::Bool(true) => Ok(Param::Text("true".into())),
                    Value::Bool(false) => Ok(Param::Text("false".into())),
                    Value::Array(_) | Value::Object(_) => {
                        Err("each of `params` must be null, a string, a number or a boolean")
                    }
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("`params` must be an array"),
        };
        Ok(Self {
            target,
            actor,
            sql,
            params,
            values,
        })
    }
}

/// The most changes one `history` answers; and how many when `limit` is
/// not given (blackbox/CONTRACT.md, "Reading: the operation `history`").
const HISTORY_LIMIT: i64 = 1000;
const HISTORY_DEFAULT_LIMIT: i64 = 100;

/// The longest name of a schema or table `history` takes, in bytes.
const HISTORY_NAME_BYTES: usize = 63;

/// The arguments of `history`, read strictly: `own` exactly `true`; `as`
/// as for `query`; `table` a string; `schema` a string, `public` when not
/// given, neither longer than [`HISTORY_NAME_BYTES`]; `key` an object whose
/// values are each null, a string, a number or a boolean; `limit` an
/// integer from 1 to 1000; `before` a string of decimal digits; `values` as
/// for `query`. Arguments it does not have are ignored (`resource`, `tx`,
/// `sql`, ...).
fn read_history(args: &Value) -> Result<(authz::Actor, HistoryRead), &'static str> {
    let Some(args) = args.as_object() else {
        return Err(
            "the arguments must be an object: { own: true, as, schema, table, key, limit, before, values }",
        );
    };
    if args.get("own") != Some(&Value::Bool(true)) {
        return Err(
            "`own` must be exactly true: the Change history is kept only for the own database",
        );
    }
    let actor = match args.get("as") {
        None => authz::Actor::User,
        Some(Value::String(actor)) if actor == "user" => authz::Actor::User,
        Some(Value::String(actor)) if actor == "application" => authz::Actor::Application,
        Some(_) => return Err("`as` must be \"user\" (the default) or \"application\""),
    };
    let Some(Value::String(table)) = args.get("table") else {
        return Err("`table` must be the table's name, as a string");
    };
    let schema = match args.get("schema") {
        None => "public",
        Some(Value::String(schema)) => schema.as_str(),
        Some(_) => {
            return Err("`schema` must be the schema's name, as a string (public when not given)");
        }
    };
    // No name in Postgres is longer (NAMEDATALEN - 1); this also bounds
    // what of the caller's goes into the entry's `statement`.
    if table.len() > HISTORY_NAME_BYTES || schema.len() > HISTORY_NAME_BYTES {
        return Err("`schema` and `table` are names of at most 63 bytes");
    }
    let key = match args.get("key") {
        None => None,
        Some(Value::Object(key))
            if key.values().all(|v| {
                matches!(
                    v,
                    Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_)
                )
            }) =>
        {
            // Copied: read on another thread, after the call.
            Some(key.clone())
        }
        Some(_) => {
            return Err(
                "`key` must be an object naming the row by its primary key's columns, each value null, a string, a number or a boolean",
            );
        }
    };
    let limit = match args.get("limit") {
        None => HISTORY_DEFAULT_LIMIT,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(limit) if (1..=HISTORY_LIMIT).contains(&limit) => limit,
            _ => return Err("`limit` must be an integer from 1 to 1000 (100 when not given)"),
        },
        Some(_) => return Err("`limit` must be an integer from 1 to 1000 (100 when not given)"),
    };
    let before = match args.get("before") {
        None => i64::MAX,
        Some(Value::String(id)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
            // An id past the greatest there can be leaves nothing out.
            id.parse::<i64>().unwrap_or(i64::MAX)
        }
        Some(_) => return Err("`before` must be a change's id: a string of decimal digits"),
    };
    let values = match args.get("values") {
        None => Values::Typed,
        Some(Value::String(values)) if values == "text" => Values::Text,
        Some(_) => return Err("`values` must be \"text\", or not given"),
    };
    Ok((
        actor,
        HistoryRead {
            // Copied: read on another thread, after the call.
            schema: schema.to_owned(),
            table: table.clone(),
            key,
            limit,
            before,
            values,
        },
    ))
}

/// A decided operation, as [`PlatformGateway::dispatch`] runs it.
trait Dispatched: Send + 'static {
    fn refusal(&self) -> Option<&Refusal>;
    fn on(&self) -> &On;
    /// Too many operations under way: a refused one is still refused, an
    /// allowed one is not run.
    fn busy(&self) -> Value;
}

impl Dispatched for Work {
    fn refusal(&self) -> Option<&Refusal> {
        self.refusal.as_ref()
    }

    fn on(&self) -> &On {
        &self.target
    }

    fn busy(&self) -> Value {
        Work::busy(self)
    }
}

/// What a decided operation is on.
enum On {
    Own,
    Resource {
        name: String,
        /// `None` when not registered (then it was refused).
        resource: Option<Arc<Resource>>,
    },
}

fn actor_name(actor: authz::Actor) -> &'static str {
    match actor {
        authz::Actor::User => "user",
        authz::Actor::Application => "application",
    }
}

/// One `query` or `execute`, decided, to be logged and run.
struct Work {
    kind: Kind,
    in_flight: Arc<InFlight>,
    active: bool,
    actor: authz::Actor,
    target: On,
    sql: String,
    params: Vec<Param>,
    values: Values,
    statement: String,
    refusal: Option<Refusal>,
    /// The transaction it runs in (blackbox/CONTRACT.md, "Transactions"),
    /// by its id; `None` outside one.
    transaction: Option<Arc<str>>,
    /// What a `history` reads; `None` for every other operation.
    history: Option<Box<HistoryRead>>,
    /// The call's place among what its request and the program hold of
    /// calls not yet answered (blackbox/CONTRACT.md, "A host call's
    /// arguments are bounded"): read by no one, only held — given back
    /// when this work, with the call's arguments, is let go of, here or
    /// in the task that runs it.
    _held: Option<Guard>,
    log: Arc<OperationLog>,
    own: Arc<OwnDatabases>,
}

impl Work {
    /// The Resource's name; `None` on the own database.
    fn resource_name(&self) -> Option<&str> {
        match &self.target {
            On::Own => None,
            On::Resource { name, .. } => Some(name),
        }
    }

    fn entry(&self) -> Entry<'_> {
        Entry {
            request: self.active.then_some(&*self.in_flight.request),
            user: self.active.then_some(self.in_flight.user.id.as_str()),
            application: &self.in_flight.application,
            version: self.in_flight.version,
            resource: self.resource_name(),
            actor: actor_name(self.actor),
            operation: self.kind.name(),
            statement: Some(&self.statement),
            transaction: self.transaction.as_deref(),
            host: None,
        }
    }

    async fn run(self) -> Value {
        if let Some(refusal) = &self.refusal {
            if let Err(error) = self.log.refused(&self.entry(), refusal.reason).await {
                self.unlogged(&error);
            }
            return refusal.to_wire();
        }
        let own_pool;
        let pool: Arc<Pool> = match &self.target {
            On::Resource { resource: None, .. } => {
                // Allowed means registered; this is not reached.
                return error_to_wire(json!({ "kind": "unavailable" }));
            }
            On::Resource {
                resource: Some(resource),
                ..
            } => {
                // A database Resource that is not served (no key, a value
                // that does not open): an allowed operation fails
                // `unavailable`, with its entry, and nothing is sent to any
                // database (blackbox/CONTRACT.md, "Resources and Grants in
                // full: At start").
                if resource.unserved().is_some() {
                    resource.mention_unserved(false);
                    let answer = error_to_wire(json!({ "kind": "unavailable" }));
                    return self.failed_unsent("unavailable", answer).await;
                }
                match self.kind {
                    Kind::Query | Kind::Begin(_) | Kind::History => match resource.read_pool() {
                        Some(pool) => pool,
                        // Allowed means registered as a database: not reached.
                        None => return error_to_wire(json!({ "kind": "unavailable" })),
                    },
                    Kind::Execute => match resource.write_pool() {
                        Some(pool) => pool,
                        None => {
                            let answer = error_to_wire(json!({
                                "kind": "read-only-resource",
                                "message": "the Resource was registered without a write login, so nothing can be written to it: ask a Platform administrator to register its write login, or use query",
                            }));
                            return self.failed_unsent("read-only-resource", answer).await;
                        }
                    },
                }
            }
            On::Own => {
                own_pool = self
                    .own
                    .pool(
                        &self.in_flight.application,
                        matches!(self.kind, Kind::Execute),
                    )
                    .await;
                match &own_pool {
                    Some(pool) => Arc::clone(pool),
                    // A serving Version means a ready own database: reached
                    // only when its keys could not be read (logged).
                    None => {
                        let answer = error_to_wire(json!({ "kind": "unavailable" }));
                        return self.failed_unsent("unavailable", answer).await;
                    }
                }
            }
        };
        if let Some(read) = &self.history {
            return self.read_history(&pool, read).await;
        }
        if let Err(refused) = check::check(&self.sql) {
            let answer = error_to_wire(json!({
                "kind": "statement",
                "reason": refused.reason.name(),
                "message": refused.message,
            }));
            return self.failed_unsent("statement", answer).await;
        }
        let id = match self.log.begin(&self.entry()).await {
            Ok(id) => id,
            Err(error) => {
                self.unlogged(&error);
                return Refusal::log_unavailable(self.resource_name()).to_wire();
            }
        };
        let label = Label {
            user: &self.in_flight.user.id,
            application: &self.in_flight.application,
            request: &self.in_flight.request,
            actor: match self.target {
                On::Own => Some(actor_name(self.actor)),
                On::Resource { .. } => None,
            },
        };
        let level = match self.kind {
            Kind::Query | Kind::Begin(_) | Kind::History => Level::Read,
            Kind::Execute => Level::Write,
        };
        // Given up when the Application is disabled meanwhile: what runs is
        // cancelled and the entry says why.
        let stop = Arc::clone(&self.in_flight.stop);
        let active = self.active;
        let stopped = async move {
            if active {
                stop.stopped().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let ran = pool
            .run_until(level, &self.sql, &self.params, self.values, label, stopped)
            .await;
        let Some(outcome) = ran else {
            let ending = Ending {
                outcome: "failed",
                error: Some(APPLICATION_DISABLED),
                rows: None,
                duration_ms: None,
                began_nothing: false,
            };
            if let Err(error) = self.log.finish(id, &ending).await {
                self.unlogged(&error);
            }
            return Refusal::application_disabled().to_wire();
        };
        // What a write committed on the own database is shipped to the
        // Change history once it is answered: asked for now, not waited for
        // (blackbox/CONTRACT.md, "Shipping: When").
        if self.kind == Kind::Execute && matches!(self.target, On::Own) && outcome.result.is_ok() {
            self.own.ship(&self.in_flight.application);
        }
        let ending = self.ending_of(&outcome);
        if let Err(error) = self.log.finish(id, &ending).await {
            self.unlogged(&error);
        }
        self.answer_of(outcome)
    }

    /// `history`, allowed: its entry written before anything is read, the
    /// read ([`OwnDatabases::read_history`]) within [`HISTORY_TIME`],
    /// shipment included, and the entry completed with the number of
    /// changes answered.
    async fn read_history(&self, pool: &Pool, read: &HistoryRead) -> Value {
        let id = match self.log.begin(&self.entry()).await {
            Ok(id) => id,
            Err(error) => {
                self.unlogged(&error);
                return Refusal::log_unavailable(None).to_wire();
            }
        };
        let label = Label {
            user: &self.in_flight.user.id,
            application: &self.in_flight.application,
            request: &self.in_flight.request,
            actor: Some(actor_name(self.actor)),
        };
        let started = tokio::time::Instant::now();
        let deadline = started + HISTORY_TIME;
        let read = tokio::time::timeout_at(
            deadline,
            self.own
                .read_history(&self.in_flight.application, pool, read, label, deadline),
        )
        .await
        .unwrap_or(Err(Failure::Limit(crate::executor::Limit::Time)));
        let duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let (changes, answer, error) = match read {
            Ok(read) => (Some(read.changes), ok_to_wire(read.answer), None),
            Err(failure) => {
                let error = match &failure {
                    Failure::Database { code, .. } => code.clone(),
                    Failure::Limit(limit) => limit.name().to_owned(),
                    Failure::Unavailable => "unavailable".to_owned(),
                };
                let answer = self.answer_of(crate::executor::Outcome {
                    result: Err(failure),
                    duration: None,
                });
                (None, answer, Some(error))
            }
        };
        let ending = Ending {
            outcome: if error.is_none() { "allowed" } else { "failed" },
            error: error.as_deref(),
            rows: changes,
            duration_ms,
            began_nothing: false,
        };
        if let Err(error) = self.log.finish(id, &ending).await {
            self.unlogged(&error);
        }
        answer
    }

    /// A statement in its transaction, on the transaction's connection
    /// `held`: checked, logged and run as any statement is, its time cut at
    /// the transaction's `cap`. If the request ends (`alive` answers) before
    /// or while it runs, it is given up. Answers what Application code
    /// receives, and what became of the transaction.
    async fn run_in_transaction(
        self,
        held: &mut Held<'_>,
        cap: tokio::time::Instant,
        alive: &mut oneshot::Receiver<()>,
    ) -> (Value, transaction::After) {
        if let Err(refused) = check::check(&self.sql) {
            let answer = error_to_wire(json!({
                "kind": "statement",
                "reason": refused.reason.name(),
                "message": refused.message,
            }));
            let answer = self.failed_unsent("statement", answer).await;
            return (answer, transaction::After::Open);
        }
        let id = match self.log.begin(&self.entry()).await {
            Ok(id) => id,
            Err(error) => {
                self.unlogged(&error);
                let answer = Refusal::log_unavailable(self.resource_name()).to_wire();
                return (answer, transaction::After::Open);
            }
        };
        let request_ended = Ending {
            outcome: "failed",
            error: Some(transaction::ended_why(&self.in_flight)),
            rows: None,
            duration_ms: None,
            began_nothing: false,
        };
        // The request may have ended while the entry was written: then
        // nothing is sent.
        if transaction::has_ended(alive) {
            if let Err(error) = self.log.finish(id, &request_ended).await {
                self.unlogged(&error);
            }
            let after = transaction::After::Ended(transaction::ended_why(&self.in_flight));
            return (transaction::no_transaction_wire(), after);
        }
        let ran = tokio::select! {
            biased;
            _ = &mut *alive => None,
            outcome = held.run(&self.sql, &self.params, self.values, cap) => Some(outcome),
        };
        let Some(outcome) = ran else {
            // The request ended while the statement ran: what the
            // connection is doing is not known, so it is stopped.
            if let Err(error) = self.log.finish(id, &request_ended).await {
                self.unlogged(&error);
            }
            return (
                transaction::no_transaction_wire(),
                transaction::After::Abandoned,
            );
        };
        let ending = self.ending_of(&outcome);
        if let Err(error) = self.log.finish(id, &ending).await {
            self.unlogged(&error);
        }
        let after = if held.is_open() {
            transaction::After::Open
        } else {
            transaction::After::Ended(match &outcome.result {
                Err(Failure::Limit(limit)) => limit.name(),
                _ => "unavailable",
            })
        };
        (self.answer_of(outcome), after)
    }

    /// How the operation's entry is completed, from its outcome.
    fn ending_of<'o>(&self, outcome: &'o Outcome) -> Ending<'o> {
        let duration_ms = outcome
            .duration
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        match &outcome.result {
            Ok(rows) => Ending {
                outcome: "allowed",
                error: None,
                rows: match self.kind {
                    // The rows affected, as Postgres counted them.
                    Kind::Execute => rows
                        .affected
                        .or_else(|| u64::try_from(rows.rows.len()).ok()),
                    Kind::Query | Kind::Begin(_) | Kind::History => {
                        u64::try_from(rows.rows.len()).ok()
                    }
                },
                duration_ms,
                began_nothing: false,
            },
            Err(failure) => Ending {
                outcome: "failed",
                error: Some(match failure {
                    Failure::Database { code, .. } => code.as_str(),
                    Failure::Limit(limit) => limit.name(),
                    Failure::Unavailable => "unavailable",
                }),
                rows: None,
                duration_ms,
                began_nothing: false,
            },
        }
    }

    /// What Application code receives, from the operation's outcome.
    fn answer_of(&self, outcome: Outcome) -> Value {
        match outcome.result {
            Ok(rows) => ok_to_wire(rows.into_wire(self.kind == Kind::Execute, self.values)),
            Err(Failure::Database { code, message }) => {
                error_to_wire(json!({ "kind": "database", "code": code, "message": message }))
            }
            Err(Failure::Limit(limit)) => {
                error_to_wire(json!({ "kind": "limit", "limit": limit.name() }))
            }
            Err(Failure::Unavailable) => error_to_wire(json!({ "kind": "unavailable" })),
        }
    }

    /// An allowed operation that ends before anything is sent: its whole
    /// entry is written, `failed` with `error`, and `answer` given. If the
    /// entry cannot be written, refused `log-unavailable` instead, as every
    /// operation that cannot be logged is.
    async fn failed_unsent(&self, error: &str, answer: Value) -> Value {
        match self.log.failed(&self.entry(), error).await {
            Ok(()) => answer,
            Err(failure) => {
                self.unlogged(&failure);
                Refusal::log_unavailable(self.resource_name()).to_wire()
            }
        }
    }

    /// Too many operations under way: a refused one is still refused, an
    /// allowed one is not run.
    fn busy(&self) -> Value {
        log::record(
            "operations-busy",
            json!({
                "application": &*self.in_flight.application,
                "resource": self.resource_name(),
                "own_database": self.resource_name().is_none(),
            }),
        );
        match &self.refusal {
            Some(refusal) => refusal.to_wire(),
            None => error_to_wire(json!({ "kind": "unavailable" })),
        }
    }

    /// A record of an entry that could not be written or completed. It
    /// carries no request id: it may come after its request ended.
    fn unlogged(&self, error: &crate::platform_db::DbError) {
        log::record(
            "operation-log-failed",
            json!({
                "application": &*self.in_flight.application,
                "resource": self.resource_name(),
                "own_database": self.resource_name().is_none(),
                "error": error.to_string(),
            }),
        );
    }
}
