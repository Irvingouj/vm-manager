//! Transactions (blackbox/CONTRACT.md, "Transactions"): one connection held
//! from `tx.begin` to its end, its statements decided, checked, limited and
//! logged as operations, one at a time.
//!
//! Where a transaction lives: each is a task of its own on the program's
//! runtime, owned by the gateway's operations ([`super::Operations`]), which
//! alone holds its connection ([`Held`]). The request that began it holds a
//! [`Handle`] in its [`Transactions`] (crate::runner::InFlight): the id, the
//! sending end of the task's command queue, and a signal that is dropped
//! when the request ends. Nothing global is keyed by an id: an id is looked
//! up only among the open transactions of the request in flight, so an id
//! never begun, already ended, ended by the host, or begun by another
//! request are all the same `no-transaction`.
//!
//! Who ends it, whichever comes first:
//!
//! - Application code, with `tx.commit` or `tx.rollback`;
//! - the request's end ([`Transactions::end`], called by the Request
//!   runner when the request is over, however it ends): rolled back. A
//!   statement running at that moment is cancelled, and the connection is
//!   closed (its backend ended from another connection if it does not come
//!   back within the grace period);
//! - the cap, [`CAP`] after the begin: rolled back. A statement running then
//!   was already cut at the cap, which is its own time limit too;
//! - a statement a limit ended, or a connection that failed: rolled back.
//!
//! The host's own rollback has its `rollback` entry, `allowed`, its `error`
//! saying why. Once ended, the queue is closed before anything is answered,
//! so a later use of the id is `no-transaction`.
//!
//! Bounds: [`PER_REQUEST`] open transactions per request (so at most that
//! many tasks per request in flight); [`QUEUE`] commands waiting per
//! transaction, past which a statement is answered `unavailable`; each wait
//! of the task has its limit (the log's writes, the pool's checkout, each
//! statement, the cap, the grace period).

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use authz::AccessLevel;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::{On, Refusal, Work, actor_name, error_to_wire, ok_to_wire};
use crate::executor::{Failure, Held, Isolation, Label, Level, Pool};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::own_db::OwnDatabases;
use crate::runner::InFlight;

/// At most this many transactions open at once per request.
pub const PER_REQUEST: usize = 4;

/// The longest a transaction may be open.
pub const CAP: Duration = Duration::from_secs(10);

/// Commands waiting for one transaction's task, at most.
pub(super) const QUEUE: usize = 64;

/// The answer's `kind` for an id that is not an open transaction of the
/// request in flight, and the `host-call` record's reason.
pub(super) const NO_TRANSACTION: &str = "no-transaction";

/// The `error` of the host's `rollback` when the request ended.
pub(super) const REQUEST_ENDED: &str = "request-ended";

/// `{ error: { kind: "no-transaction", message } }`: the same whatever the
/// reason, and never the id.
pub(super) fn no_transaction_wire() -> Value {
    error_to_wire(json!({
        "kind": NO_TRANSACTION,
        "message": "this is not an open transaction of the request in flight: a transaction id works only in the request that began it, until it is committed, rolled back or ended by the platform; begin a new one with tx.begin",
    }))
}

/// The open transactions of one request. Held only to look up, add or take
/// away a handle, never across an await.
#[derive(Default)]
pub struct Transactions {
    open: Mutex<Open>,
}

impl fmt::Debug for Transactions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not the ids: they are what a transaction is used with.
        f.write_str("Transactions(..)")
    }
}

#[derive(Default)]
struct Open {
    /// The request is over: nothing more is begun.
    ended: bool,
    /// At most [`PER_REQUEST`] that are still open; ended ones are taken
    /// away when another is begun.
    handles: Vec<Handle>,
}

/// The request's side of one transaction.
struct Handle {
    id: Arc<str>,
    /// The Resource; `None` for the own database.
    resource: Option<Arc<str>>,
    actor: authz::Actor,
    /// The `access` it was begun at.
    access: AccessLevel,
    commands: mpsc::Sender<Command>,
    /// Dropped when the request ends (or the handle is taken away): the
    /// task then rolls back.
    _alive: oneshot::Sender<()>,
}

/// An open transaction of the request, found by its id.
pub(super) struct Found {
    pub(super) id: Arc<str>,
    pub(super) resource: Option<Arc<str>>,
    pub(super) actor: authz::Actor,
    /// The `access` it was begun at: in a `write` transaction every
    /// statement needs `write`, `query` included (blackbox/CONTRACT.md,
    /// "Transactions: Through the raw interface").
    pub(super) access: AccessLevel,
    pub(super) commands: mpsc::Sender<Command>,
}

/// What [`Transactions::reserve`] gave.
pub(super) enum Reserved {
    Made(Made),
    /// [`PER_REQUEST`] are open.
    Full,
    /// The request is over.
    Ended,
}

/// A transaction just added to the request: what its task is given.
pub(super) struct Made {
    pub(super) id: Arc<str>,
    pub(super) commands: mpsc::Receiver<Command>,
    pub(super) alive: oneshot::Receiver<()>,
}

impl Transactions {
    /// The request is over: every transaction it left open is rolled back
    /// by its own task, which sees its handle go.
    pub fn end(&self) {
        let handles = {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            open.ended = true;
            std::mem::take(&mut open.handles)
        };
        drop(handles);
    }

    /// The open transaction `id` of this request.
    pub(super) fn find(&self, id: &str) -> Option<Found> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let handle = open
            .handles
            .iter()
            .find(|h| !h.commands.is_closed() && *h.id == *id)?;
        Some(Found {
            id: Arc::clone(&handle.id),
            resource: handle.resource.as_ref().map(Arc::clone),
            actor: handle.actor,
            access: handle.access,
            commands: handle.commands.clone(),
        })
    }

    /// Adds a transaction on `resource` (`None`: the own database) acting
    /// as `actor`, unless [`PER_REQUEST`] are open. Its id is made here:
    /// 64 hex digits from the operating system's random source (two version
    /// 4 UUIDs, 244 random bits), which cannot be guessed.
    pub(super) fn reserve(
        &self,
        resource: Option<&str>,
        actor: authz::Actor,
        access: AccessLevel,
    ) -> Reserved {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if open.ended {
            return Reserved::Ended;
        }
        open.handles.retain(|h| !h.commands.is_closed());
        if open.handles.len() >= PER_REQUEST {
            return Reserved::Full;
        }
        let id: Arc<str> = Arc::from(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ));
        let (commands, receiver) = mpsc::channel(QUEUE);
        let (alive, signal) = oneshot::channel();
        open.handles.push(Handle {
            id: Arc::clone(&id),
            resource: resource.map(Arc::from),
            actor,
            access,
            commands,
            _alive: alive,
        });
        Reserved::Made(Made {
            id,
            commands: receiver,
            alive: signal,
        })
    }
}

/// What the transaction's task is asked to do.
pub(super) enum Command {
    /// A statement, decided and allowed, to check, log and run.
    Statement {
        work: Box<Work>,
        reply: oneshot::Sender<Value>,
    },
    /// `tx.commit` (`commit`) or `tx.rollback`.
    End {
        commit: bool,
        reply: oneshot::Sender<Value>,
    },
}

/// What became of the transaction after a statement.
pub(super) enum After {
    Open,
    /// Over, for this reason; its connection is given up already, or (the
    /// request ended before the statement was sent) still clean in it.
    Ended(&'static str),
    /// The request ended while the statement ran: the connection's state is
    /// not known.
    Abandoned,
}

/// Whether the request has ended, without waiting.
pub(super) fn has_ended(alive: &mut oneshot::Receiver<()>) -> bool {
    matches!(alive.try_recv(), Err(oneshot::error::TryRecvError::Closed))
}

/// A transaction about to begin: everything its task needs.
pub(super) struct Opened {
    pub(super) id: Arc<str>,
    pub(super) in_flight: Arc<InFlight>,
    pub(super) actor: authz::Actor,
    pub(super) target: On,
    pub(super) level: Level,
    pub(super) isolation: Option<Isolation>,
    pub(super) log: Arc<OperationLog>,
    pub(super) own: Arc<OwnDatabases>,
}

/// The task of one transaction, from its begin to its end. `began` gets the
/// answer to `tx.begin`.
pub(super) async fn run(
    opened: Opened,
    mut commands: mpsc::Receiver<Command>,
    mut alive: oneshot::Receiver<()>,
    began: oneshot::Sender<Value>,
) {
    let own_pool;
    let pool: &Pool = match &opened.target {
        On::Resource {
            resource: Some(resource),
            ..
        } => match opened.level {
            Level::Read => resource.read_pool(),
            Level::Write => {
                if let Some(pool) = resource.write_pool() {
                    pool
                } else {
                    let answer = error_to_wire(json!({
                        "kind": "read-only-resource",
                        "message": "the Resource was registered without a write login, so nothing can be written to it: ask a Platform administrator to register its write login, or begin with access \"read\"",
                    }));
                    began
                        .send(opened.failed("read-only-resource", answer).await)
                        .ok();
                    return;
                }
            }
        },
        // Allowed means registered; this is not reached.
        On::Resource { resource: None, .. } => {
            began.send(unavailable()).ok();
            return;
        }
        On::Own => {
            own_pool = opened
                .own
                .pool(&opened.in_flight.application, opened.level == Level::Write)
                .await;
            if let Some(pool) = &own_pool {
                pool
            } else {
                began
                    .send(opened.failed("unavailable", unavailable()).await)
                    .ok();
                return;
            }
        }
    };
    let mut held = match opened.begin(pool).await {
        Ok(held) => held,
        Err(answer) => {
            began.send(answer).ok();
            return;
        }
    };
    began.send(ok_to_wire(json!({ "tx": &*opened.id }))).ok();
    let cap = Instant::now() + CAP;
    let why = loop {
        let command = tokio::select! {
            biased;
            _ = &mut alive => break REQUEST_ENDED,
            () = tokio::time::sleep_until(cap) => break "time",
            command = commands.recv() => match command {
                Some(command) => command,
                None => break REQUEST_ENDED,
            },
        };
        match command {
            Command::Statement { work, reply } => {
                let (answer, after) = work.run_in_transaction(&mut held, cap, &mut alive).await;
                let why = match after {
                    After::Open => {
                        reply.send(answer).ok();
                        continue;
                    }
                    After::Ended(why) => why,
                    After::Abandoned => REQUEST_ENDED,
                };
                // Closed before anything is answered: a use of the id
                // after this answer is `no-transaction`.
                commands.close();
                opened.rolled_back_by_host(why).await;
                match after {
                    After::Abandoned => held.halt().await,
                    _ if held.is_open() => {
                        held.end(false).await.ok();
                    }
                    _ => {}
                }
                reply.send(answer).ok();
                return;
            }
            Command::End { commit, reply } => {
                commands.close();
                reply.send(opened.end(held, commit).await).ok();
                return;
            }
        }
    };
    // Ended by the host while no statement ran.
    commands.close();
    opened.rolled_back_by_host(why).await;
    held.end(false).await.ok();
}

impl Opened {
    /// The Resource's name; `None` on the own database.
    fn resource_name(&self) -> Option<&str> {
        match &self.target {
            On::Own => None,
            On::Resource { name, .. } => Some(name),
        }
    }

    /// The entry of `begin`, `commit` or `rollback`, of this transaction
    /// (`transaction` its id) or of none.
    fn entry<'a>(&'a self, operation: &'a str, transaction: Option<&'a str>) -> Entry<'a> {
        Entry {
            request: Some(&self.in_flight.request),
            user: Some(&self.in_flight.user.id),
            application: &self.in_flight.application,
            version: self.in_flight.version,
            resource: self.resource_name(),
            actor: actor_name(self.actor),
            operation,
            statement: operation,
            transaction,
        }
    }

    /// A `begin` that failed before anything was sent: its whole entry,
    /// `failed` with `error` and no transaction, and `answer` (or
    /// `log-unavailable` if the entry cannot be written).
    async fn failed(&self, error: &str, answer: Value) -> Value {
        match self.log.failed(&self.entry("begin", None), error).await {
            Ok(()) => answer,
            Err(failure) => {
                self.unlogged(&failure);
                Refusal::log_unavailable(self.resource_name()).to_wire()
            }
        }
    }

    /// Takes a connection from `pool`, writes the `begin` entry, and begins
    /// the transaction on it. If any of it fails, the answer to `tx.begin`.
    async fn begin<'p>(&self, pool: &'p Pool) -> Result<Held<'p>, Value> {
        let Ok(mut held) = pool.checkout(self.level).await else {
            return Err(self.failed("unavailable", unavailable()).await);
        };
        let entry = match self.log.begin(&self.entry("begin", Some(&self.id))).await {
            Ok(entry) => entry,
            Err(failure) => {
                self.unlogged(&failure);
                held.release();
                return Err(Refusal::log_unavailable(self.resource_name()).to_wire());
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
        let began_in = held.begin(self.isolation, label).await;
        let ending = Ending {
            outcome: if began_in.is_ok() {
                "allowed"
            } else {
                "failed"
            },
            error: began_in.as_ref().err().map(|_| "unavailable"),
            rows: None,
            duration_ms: began_in.as_ref().ok().map(|d| millis(*d)),
            began_nothing: began_in.is_err(),
        };
        if let Err(failure) = self.log.finish(entry, &ending).await {
            self.unlogged(&failure);
        }
        match began_in {
            Ok(_) => Ok(held),
            Err(_) => Err(unavailable()),
        }
    }

    /// Ends the transaction as Application code asked: `COMMIT` or
    /// `ROLLBACK`, logged as `commit` or `rollback`. Committing a
    /// transaction Postgres has aborted rolls it back and answers
    /// `transaction-aborted`.
    async fn end(&self, held: Held<'_>, commit: bool) -> Value {
        let operation = if commit { "commit" } else { "rollback" };
        let entry = match self.log.begin(&self.entry(operation, Some(&self.id))).await {
            Ok(entry) => entry,
            Err(failure) => {
                self.unlogged(&failure);
                // Not committed: nothing is done that cannot be logged.
                held.end(false).await.ok();
                return Refusal::log_unavailable(self.resource_name()).to_wire();
            }
        };
        let aborted = commit && held.aborted();
        let started = Instant::now();
        let ended = held.end(commit && !aborted).await;
        let duration_ms = Some(millis(started.elapsed()));
        let (outcome, error, answer) = match &ended {
            Ok(()) if aborted => (
                "failed",
                Some("transaction-aborted"),
                error_to_wire(json!({
                    "kind": "transaction-aborted",
                    "message": "a statement in the transaction had failed, so it was rolled back instead of committed: nothing it did was kept",
                })),
            ),
            Ok(()) => ("allowed", None, ok_to_wire(json!({}))),
            Err(Failure::Database { code, message }) => (
                "failed",
                Some(code.as_str()),
                error_to_wire(json!({ "kind": "database", "code": code, "message": message })),
            ),
            Err(Failure::Limit(limit)) => (
                "failed",
                Some(limit.name()),
                error_to_wire(json!({ "kind": "limit", "limit": limit.name() })),
            ),
            Err(Failure::Unavailable) => ("failed", Some("unavailable"), unavailable()),
        };
        let ending = Ending {
            outcome,
            error,
            rows: None,
            duration_ms,
            began_nothing: false,
        };
        if let Err(failure) = self.log.finish(entry, &ending).await {
            self.unlogged(&failure);
        }
        answer
    }

    /// The entry of a rollback the host makes on its own: `allowed`, its
    /// `error` saying why.
    async fn rolled_back_by_host(&self, why: &str) {
        let entry = self.entry("rollback", Some(&self.id));
        if let Err(failure) = self.log.whole(&entry, "allowed", Some(why)).await {
            self.unlogged(&failure);
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

fn unavailable() -> Value {
    error_to_wire(json!({ "kind": "unavailable" }))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
