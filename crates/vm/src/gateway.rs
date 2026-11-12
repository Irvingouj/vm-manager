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
//! Operations: `whoami`, and `query` on a shared database. A `query` goes:
//!
//! 1. its arguments are read (malformed: answered so, nothing logged or
//!    sent);
//! 2. the Authorization decision ([`authz::decide`]), from the request in
//!    flight, the serving Version's Declared access, and the Resources and
//!    Grants in memory (crate::resources);
//! 3. the statement without its values (crate::statement);
//! 4. the rest runs as a task on the program's runtime, owned here: a
//!    refused operation's Operation log entry is written whole and the
//!    refusal answered; an allowed one's entry is written `unfinished`
//!    before anything is sent (if that fails: refused `log-unavailable`),
//!    the statement runs through the Database executor (crate::executor)
//!    and the entry is completed, before the answer.
//!
//! The task does not depend on the request: an operation the Application
//! did not wait for still runs to its end and completes its entry. Tasks
//! are bounded ([`OPERATIONS_AT_ONCE`]); past the bound an operation waits
//! up to [`PERMIT_WAIT`] to start, within the Application's call.
//!
//! Later tickets add write, the own database and outbound calls here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use authz::{AccessLevel, Question};
use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;

use crate::executor::{Failure, Param};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
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

    fn log_unavailable(resource: &str) -> Self {
        let mut refusal = Self::new(
            LOG_UNAVAILABLE,
            "the platform could not write the Operation log entry, so the query was not run: try again shortly".into(),
        );
        refusal.details.insert("resource".into(), resource.into());
        refusal
    }

    /// The refusal of the Authorization decision's `reason`, for a query on
    /// `name` by `user`.
    fn of(reason: authz::Reason, name: &str, user: &str, resource: Option<&Resource>) -> Self {
        let mut refusal = match reason {
            authz::Reason::NoRequest => return Self::no_request(),
            authz::Reason::NotDeclared => Self::new(
                "not-declared",
                format!(
                    "the serving Version does not declare the Resource {name:?}: add it to declared_access.resources (kind \"postgres\", access \"read\" or higher) and send a new Version"
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
    log: Arc<OperationLog>,
    operations: Arc<Operations>,
}

impl PlatformGateway {
    /// `runtime` is where operations run: the program's own, where the
    /// platform database's and the shared databases' connections live.
    pub fn new(resources: Arc<Resources>, log: Arc<OperationLog>, runtime: Handle) -> Self {
        Self {
            resources,
            log,
            operations: Arc::new(Operations {
                runtime,
                tasks: Mutex::new(JoinSet::new()),
                permits: Arc::new(Semaphore::new(OPERATIONS_AT_ONCE)),
            }),
        }
    }

    fn query(&self, in_flight: &Arc<InFlight>, active: bool, args: &Value) -> Decision {
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
        let resource = self.resources.get(call.resource);
        let groups: Vec<&str> = in_flight.user.groups.iter().map(String::as_str).collect();
        let decide = |grants: &[authz::Grant<'_>]| {
            authz::decide(&Question {
                request_in_flight: active,
                groups: &groups,
                declared: in_flight.declared.get(call.resource).copied(),
                registered: resource.is_some(),
                grants,
                needed: AccessLevel::Read,
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
                call.resource,
                &in_flight.user.id,
                resource.as_deref(),
            )),
        };
        let refused = refusal.as_ref().map(|r| r.reason);
        // What is sent is kept only for an operation that will be sent.
        let (sql, params) = match refusal {
            None => (call.sql.to_owned(), call.params),
            Some(_) => (String::new(), Vec::new()),
        };
        let work = Work {
            in_flight: Arc::clone(in_flight),
            active,
            // Copied: the operation runs on another thread, beyond the
            // Application's call that lent these.
            name: call.resource.to_owned(),
            sql,
            params,
            statement: statement::without_values(call.sql),
            resource,
            refusal,
            log: Arc::clone(&self.log),
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
            "query" => self.query(in_flight, active, args),
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

/// The arguments of `query`, read strictly (blackbox/CONTRACT.md,
/// "Operations on a shared database"). Fields it does not have are
/// ignored.
struct Call<'a> {
    resource: &'a str,
    sql: &'a str,
    params: Vec<Param>,
}

impl<'a> Call<'a> {
    fn read(args: &'a Value) -> Result<Self, &'static str> {
        let Some(args) = args.as_object() else {
            return Err("the arguments must be an object: { resource, sql, params }");
        };
        let Some(resource) = args.get("resource").and_then(Value::as_str) else {
            return Err("`resource` must be the name of a Resource, as a string");
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
            resource,
            sql,
            params,
        })
    }
}

/// One `query`, decided, to be logged and run.
struct Work {
    in_flight: Arc<InFlight>,
    active: bool,
    name: String,
    sql: String,
    params: Vec<Param>,
    statement: String,
    resource: Option<Arc<Resource>>,
    refusal: Option<Refusal>,
    log: Arc<OperationLog>,
}

impl Work {
    fn entry(&self) -> Entry<'_> {
        Entry {
            request: self.active.then_some(&*self.in_flight.request),
            user: self.active.then_some(self.in_flight.user.id.as_str()),
            application: &self.in_flight.application,
            version: self.in_flight.version,
            resource: &self.name,
            operation: "query",
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
        let Some(resource) = &self.resource else {
            // Allowed means registered; this is not reached.
            return error_to_wire(json!({ "kind": "unavailable" }));
        };
        let id = match self.log.begin(&self.entry()).await {
            Ok(id) => id,
            Err(error) => {
                self.unlogged(&error);
                return Refusal::log_unavailable(&self.name).to_wire();
            }
        };
        let outcome = resource.read_pool().read(&self.sql, &self.params).await;
        let duration_ms = outcome
            .duration
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let ending = match &outcome.result {
            Ok(rows) => Ending {
                outcome: "allowed",
                error: None,
                rows: u64::try_from(rows.rows.len()).ok(),
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
            Ok(rows) => ok_to_wire(rows.into_wire()),
            Err(Failure::Database { code, message }) => {
                error_to_wire(json!({ "kind": "database", "code": code, "message": message }))
            }
            Err(Failure::Limit(limit)) => {
                error_to_wire(json!({ "kind": "limit", "limit": limit.name() }))
            }
            Err(Failure::Unavailable) => error_to_wire(json!({ "kind": "unavailable" })),
        }
    }

    /// Too many operations under way: a refused one is still refused, an
    /// allowed one is not run.
    fn busy(&self) -> Value {
        log::record(
            "operations-busy",
            json!({ "application": &*self.in_flight.application, "resource": self.name }),
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
                "resource": self.name,
                "error": error.to_string(),
            }),
        );
    }
}
