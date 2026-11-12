//! The Operation gateway: the single place every operation from Application
//! code enters the host, is decided, logged and run.
//!
//! Interface: [`Gateway::operate`], called by the Request runner's host
//! side synchronously, inside the Application's call, with the request in
//! flight read at that moment and whether it is still in flight. It answers
//! a [`Decision`]: the refusal's reason, if refused, for the program's
//! `host-call` record (written at once, by the caller), and the answer
//! Application code will receive, as a future. Who is asking is never read
//! from anything the Application passes.
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
//!    (crate::resources), for the level the operation needs;
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
//! Later tickets add outbound calls here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use authz::{AccessLevel, Question};
use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;

use crate::check;
use crate::executor::{Failure, Label, Level, Param, Pool};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::own_db::OwnDatabases;
use crate::resources::{Resource, Resources, level_name};
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
/// The Operation log entry could not be written.
pub const LOG_UNAVAILABLE: &str = "log-unavailable";
/// The `host-call` record's reason for arguments that cannot be read. Not
/// a refusal Application code receives: it gets `{ error: { kind:
/// "malformed" } }`.
pub const MALFORMED: &str = "malformed";

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
    ) -> Self {
        let mut refusal = match reason {
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
                let contact = resource.map_or("", |r| r.contact.as_str());
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
    /// whether it is still in flight. Must decide without waiting.
    fn operate(
        &self,
        in_flight: &Arc<InFlight>,
        active: bool,
        operation: &str,
        args: &Value,
    ) -> Decision;
}

/// Operations under way at once; past it an operation waits to start.
pub const OPERATIONS_AT_ONCE: usize = 256;

/// How long an operation waits to start when [`OPERATIONS_AT_ONCE`] are
/// under way; then it is answered `{ error: { kind: "unavailable" } }`.
pub const PERMIT_WAIT: Duration = Duration::from_secs(5);

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
    own: Arc<OwnDatabases>,
    log: Arc<OperationLog>,
    operations: Arc<Operations>,
}

impl PlatformGateway {
    /// `runtime` is where operations run: the program's own, where the
    /// platform database's and the shared databases' connections live.
    pub fn new(
        resources: Arc<Resources>,
        own: Arc<OwnDatabases>,
        log: Arc<OperationLog>,
        runtime: Handle,
    ) -> Self {
        Self {
            resources,
            own,
            log,
            operations: Arc::new(Operations {
                runtime,
                tasks: Mutex::new(JoinSet::new()),
                permits: Arc::new(Semaphore::new(OPERATIONS_AT_ONCE)),
            }),
        }
    }

    fn on_database(
        &self,
        kind: Kind,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: &Value,
    ) -> Decision {
        let call = match Call::read(args) {
            Ok(call) => call,
            // With no request in flight nothing is answered but that.
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => {
                return Decision {
                    refused: Some(MALFORMED),
                    answer: Box::pin(std::future::ready(error_to_wire(
                        json!({ "kind": "malformed", "message": problem }),
                    ))),
                };
            }
        };
        let groups: Vec<&str> = in_flight.user.groups.iter().map(String::as_str).collect();
        let (target, refusal) = match call.target {
            Target::Own => {
                let decision = authz::decide(&Question {
                    request_in_flight: active,
                    actor: call.actor,
                    own_database: true,
                    groups: &groups,
                    declared: None,
                    registered: false,
                    grants: &[],
                    needed: kind.needed(),
                });
                let refusal = match decision {
                    authz::Decision::Allow => None,
                    // Only `no-request` refuses on the own database.
                    authz::Decision::Refuse(_) => Some(Refusal::no_request()),
                };
                (On::Own, refusal)
            }
            Target::Resource(name) => {
                let resource = self.resources.get(name);
                let decide = |grants: &[authz::Grant<'_>]| {
                    authz::decide(&Question {
                        request_in_flight: active,
                        actor: call.actor,
                        own_database: false,
                        groups: &groups,
                        declared: in_flight.declared.get(name).copied(),
                        registered: resource.is_some(),
                        grants,
                        needed: kind.needed(),
                    })
                };
                let decision = match &resource {
                    Some(resource) => resource.with_grants(decide),
                    None => decide(&[]),
                };
                let refusal = match decision {
                    authz::Decision::Allow => None,
                    authz::Decision::Refuse(reason) => Some(Refusal::of(
                        reason,
                        kind.needed(),
                        name,
                        &in_flight.user.id,
                        resource.as_deref(),
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
        };
        let refused = refusal.as_ref().map(|r| r.reason);
        // What is sent is kept only for an operation that will be sent.
        let (sql, params) = match refusal {
            None => (call.sql.to_owned(), call.params),
            Some(_) => (String::new(), Vec::new()),
        };
        let work = Work {
            kind,
            in_flight: Arc::clone(in_flight),
            active,
            actor: call.actor,
            target,
            sql,
            params,
            statement: statement::without_values(call.sql),
            refusal,
            log: Arc::clone(&self.log),
            own: Arc::clone(&self.own),
        };
        if !active {
            // Refused (`no-request` comes first). Answered at once: the
            // runtime may not settle a call made while no event runs, as
            // at the script's top level. Its entry is written meanwhile.
            return self.refused_unawaited(work);
        }
        let operations = Arc::clone(&self.operations);
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
                reply.send(work.run().await).ok();
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
    /// Answers `work`'s refusal now and writes its entry in a task of its
    /// own. Past [`OPERATIONS_AT_ONCE`] the entry is not written, and the
    /// program's log says so.
    fn refused_unawaited(&self, work: Work) -> Decision {
        let Some(refusal) = &work.refusal else {
            return Decision::refused(&Refusal::no_request());
        };
        let decision = Decision::refused(refusal);
        match Arc::clone(&self.operations.permits).try_acquire_owned() {
            Ok(permit) => self.operations.spawn(async move {
                work.run().await;
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
        args: &Value,
    ) -> Decision {
        match operation {
            "query" => self.on_database(Kind::Query, in_flight, active, args),
            "execute" => self.on_database(Kind::Execute, in_flight, active, args),
            _ if !active => Decision::refused(&Refusal::no_request()),
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
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Execute => "execute",
        }
    }

    fn needed(self) -> AccessLevel {
        match self {
            Self::Query => AccessLevel::Read,
            Self::Execute => AccessLevel::Write,
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
}

/// The arguments of `query` and `execute`, read strictly (blackbox/CONTRACT.md,
/// "Operations on a shared database" and "Operations on the own database"):
/// exactly one of `own: true` and `resource`; `as` absent, `"user"` or
/// `"application"`. Fields it does not have are ignored.
struct Call<'a> {
    target: Target<'a>,
    actor: authz::Actor,
    sql: &'a str,
    params: Vec<Param>,
}

impl<'a> Call<'a> {
    fn read(args: &'a Value) -> Result<Self, &'static str> {
        const TARGET: &str = "name exactly one target: `own: true` for the Application's own database, or `resource` with the name of a Resource";
        let Some(args) = args.as_object() else {
            return Err(
                "the arguments must be an object: { own: true, as, sql, params } or { resource, as, sql, params }",
            );
        };
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
        })
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
    statement: String,
    refusal: Option<Refusal>,
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
            statement: &self.statement,
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
        let pool: &Pool = match &self.target {
            On::Resource { resource: None, .. } => {
                // Allowed means registered; this is not reached.
                return error_to_wire(json!({ "kind": "unavailable" }));
            }
            On::Resource {
                resource: Some(resource),
                ..
            } => match self.kind {
                Kind::Query => resource.read_pool(),
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
            },
            On::Own => {
                own_pool = self.own.pool(&self.in_flight.application);
                match &own_pool {
                    Some(pool) => pool,
                    // A serving Version means a ready own database; this is
                    // not reached.
                    None => {
                        let answer = error_to_wire(json!({ "kind": "unavailable" }));
                        return self.failed_unsent("unavailable", answer).await;
                    }
                }
            }
        };
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
            Kind::Query => Level::Read,
            Kind::Execute => Level::Write,
        };
        let outcome = pool.run(level, &self.sql, &self.params, label).await;
        let duration_ms = outcome
            .duration
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let ending = match &outcome.result {
            Ok(rows) => Ending {
                outcome: "allowed",
                error: None,
                rows: match self.kind {
                    // The rows affected, as Postgres counted them.
                    Kind::Execute => rows
                        .affected
                        .or_else(|| u64::try_from(rows.rows.len()).ok()),
                    Kind::Query => u64::try_from(rows.rows.len()).ok(),
                },
                duration_ms,
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
            },
        };
        if let Err(error) = self.log.finish(id, &ending).await {
            self.unlogged(&error);
        }
        match outcome.result {
            Ok(rows) => ok_to_wire(rows.into_wire(self.kind == Kind::Execute)),
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
