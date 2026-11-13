//! The Database executor for shared databases (ADR 0013;
//! docs/research/what-postgres-sees.md): runs one Application statement as
//! one transaction on a connection logged in directly as the Access level's
//! login, never switching role.
//!
//! Interface: a [`Pool`] per Resource and [`Level`], or one per
//! Application's own database ([`Levels::Both`]: one runtime role serves
//! both kinds of transaction), made from a [`Target`] (where the database
//! is and the login, which never prints) and [`Limits`]; [`Pool::run`] runs
//! one statement with its [`Param`]s and the identity [`Label`] in one
//! transaction, read-only at [`Level::Read`] and read-write at
//! [`Level::Write`], and answers an [`Outcome`]: the rows, or
//! a [`Failure`] (Postgres refused it, a [`Limit`] ended it, or the
//! database could not be reached), with when the statement was sent and
//! ended. A write is committed only when its statement succeeded; otherwise
//! it is rolled back.
//!
//! A connection serves one pool all its life, so it is never shared
//! between a shared database's levels or between databases, and it goes
//! back to its pool only shown
//! clean: its transaction ended and its session state discarded (`DISCARD
//! ALL`), both answered without error and the connection idle. Otherwise it
//! is closed. The program never sends `SET ROLE` or `SET SESSION
//! AUTHORIZATION`.
//!
//! A transaction that stays open between operations (blackbox/CONTRACT.md,
//! "Transactions") takes a connection with [`Pool::checkout`] and holds it
//! as a [`Held`] until it ends: begun once ([`Held::begin`]: its label, and
//! on an own database its signed identity, set for all of it), then one
//! statement at a time ([`Held::run`]), each under the same limits, its time
//! also cut at the transaction's cap. The same rule holds at its end: back
//! to the pool only shown clean.
//!
//! A pool opens no connection until it is used (registering a Resource
//! does not connect). The statement check (crate::check) is not here: it
//! is hygiene, decided before an operation reaches the executor, and
//! nothing here relies on it.

mod identity;
mod wire;

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;

use crate::log;
pub use identity::{IdentityKey, identity};
use wire::{Connection, Exchange, WireError};

/// A password for a shared database. No `Display`; its `Debug` shows
/// nothing of it.
pub struct Password(String);

impl Password {
    pub fn new(password: String) -> Self {
        Self(password)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<hidden>)")
    }
}

/// Where a shared database is and the login to use. None of it is ever
/// printed: its `Debug` shows nothing, and no error made from it repeats
/// any of it.
pub struct Target {
    host: String,
    port: u16,
    database: String,
    role: String,
    password: Password,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Target(<hidden>)")
    }
}

impl Target {
    pub fn new(
        host: String,
        port: u16,
        database: String,
        role: String,
        password: Password,
    ) -> Self {
        Self {
            host,
            port,
            database,
            role,
            password,
        }
    }
}

/// The pool's bounds and the limits of one statement.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Connections open at once, idle or in use.
    pub size: usize,
    /// The longest an operation waits for a free connection.
    pub checkout: Duration,
    /// The longest opening a connection may take, login included.
    pub connect: Duration,
    /// The longest a statement may run, from when it is sent.
    pub statement: Duration,
    /// After a cancel, how long the connection has to come back before its
    /// backend is ended from another connection; also the time allowed for
    /// ending a transaction.
    pub grace: Duration,
    /// The most rows a result may have.
    pub max_rows: usize,
    /// The most bytes a result's rows may take as they arrive.
    pub max_bytes: usize,
}

impl Default for Limits {
    /// blackbox/CONTRACT.md, "Operations on a shared database": 10,000 rows,
    /// 8 MiB, 10 seconds.
    fn default() -> Self {
        Self {
            size: 8,
            checkout: Duration::from_secs(5),
            connect: Duration::from_secs(5),
            statement: Duration::from_secs(10),
            grace: Duration::from_secs(2),
            max_rows: 10_000,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Which login a pool logs in as, and the transaction its operations run
/// in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// The read login, in a read-only transaction.
    Read,
    /// The write login, in a read-write transaction.
    Write,
}

/// The identity label (blackbox/CONTRACT.md, "The identity label"): set by
/// the host's first statement in each transaction as the transaction-local
/// settings `vm.user`, `vm.application` and `vm.request`, and on an
/// Application's own database `vm.actor` too, sent as parameters. A label
/// only: Application SQL can overwrite it in its own transaction, and
/// nothing on the platform relies on it. On an own database whose pool
/// has the keys ([`Pool::with_identity_key`]) the same values are also
/// attached signed, as `vm.ctx`, which is what `vm.identity()` and the
/// Change history rely on.
#[derive(Clone, Copy, Debug)]
pub struct Label<'a> {
    pub user: &'a str,
    pub application: &'a str,
    pub request: &'a str,
    /// `user` or `application`, on an own database; `None` on a shared one.
    pub actor: Option<&'a str>,
}

/// The isolation level a transaction is begun at (blackbox/CONTRACT.md,
/// "Transactions: Through the raw interface"). Without one, the database's
/// default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Isolation {
    /// The level of one of the three names the contract gives, exactly.
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "read committed" => Some(Self::ReadCommitted),
            "repeatable read" => Some(Self::RepeatableRead),
            "serializable" => Some(Self::Serializable),
            _ => None,
        }
    }
}

/// Which transactions a pool's login runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels {
    /// Only this one: a shared database's login of that Access level.
    Only(Level),
    /// Both: an own database's runtime role, which is the boundary whatever
    /// the transaction (ADR 0013), so one pool serves both.
    Both,
}

impl Levels {
    fn permit(self, level: Level) -> bool {
        match self {
            Self::Only(only) => only == level,
            Self::Both => true,
        }
    }
}

/// What a pool connects to, for the program's log. Never a credential.
#[derive(Clone, Debug)]
pub enum Source {
    /// A Resource, by name.
    Resource(String),
    /// An Application's own database, by the Application's name.
    OwnDatabase(Arc<str>),
}

/// A parameter, sent as text for the server to interpret.
#[derive(Debug)]
pub enum Param {
    Null,
    Text(String),
}

/// How the values of a result reach Application code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Values {
    /// Typed as blackbox/CONTRACT.md, "Operations on a shared database",
    /// says: booleans, some numbers and JSON as themselves, the rest as text.
    #[default]
    Typed,
    /// Every value as the text Postgres printed for it, and the type of
    /// each column, for the SDK to read as Drizzle's columns expect
    /// (blackbox/CONTRACT.md, "The SDK: Values").
    Text,
}

/// A result's columns and rows; each row is a JSON array.
#[derive(Debug)]
pub struct Rows {
    pub columns: Vec<String>,
    /// The type (its OID) of each column, in the order of `columns`.
    pub types: Vec<u32>,
    pub rows: Vec<Value>,
    /// The count in Postgres's command tag (`INSERT 0 3`, `UPDATE 2`,
    /// `SELECT 5`), if it gave one.
    pub affected: Option<u64>,
}

impl Rows {
    /// `{ columns, rows, row_count }`, `rows_affected` when `affected` is
    /// asked for, and `types` when the values are [`Values::Text`], as
    /// Application code receives it.
    pub fn into_wire(self, affected: bool, values: Values) -> Value {
        let mut wire = Map::with_capacity(5);
        wire.insert("row_count".into(), self.rows.len().into());
        if affected {
            let count = self
                .affected
                .unwrap_or_else(|| u64::try_from(self.rows.len()).unwrap_or(u64::MAX));
            wire.insert("rows_affected".into(), count.into());
        }
        wire.insert("columns".into(), self.columns.into());
        if values == Values::Text {
            wire.insert("types".into(), self.types.into());
        }
        wire.insert("rows".into(), Value::Array(self.rows));
        Value::Object(wire)
    }
}

/// Which limit ended a statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Rows,
    Bytes,
    Time,
}

impl Limit {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rows => "rows",
            Self::Bytes => "bytes",
            Self::Time => "time",
        }
    }
}

/// Why a statement gave no rows.
#[derive(Debug)]
pub enum Failure {
    /// Postgres refused or failed it: its SQLSTATE and primary message.
    Database { code: String, message: String },
    /// A limit ended it.
    Limit(Limit),
    /// The database could not be reached, refused the login, or the
    /// connection failed. Nothing about the connection is said.
    Unavailable,
}

/// What came of one operation.
#[derive(Debug)]
pub struct Outcome {
    pub result: Result<Rows, Failure>,
    /// From when the statement was sent until it ended; `None` if it was
    /// never sent.
    pub duration: Option<Duration>,
}

/// A bounded pool of connections to one database as one login.
pub struct Pool {
    /// What it connects to, for the program's log. Not a credential.
    source: Source,
    levels: Levels,
    target: Target,
    limits: Limits,
    /// Bound: `limits.size` permits, one per connection open or being
    /// opened. When all are taken an operation waits up to
    /// `limits.checkout`, then is answered unavailable.
    permits: Semaphore,
    /// Connections not in use: at most `limits.size`, since each stands for
    /// a permit given back.
    idle: Mutex<Vec<Connection>>,
    /// An own database's keys: with them, every operation whose label has
    /// an actor gets the signed identity attached, not only the label.
    key: Option<Arc<IdentityKey>>,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("source", &self.source)
            .field("levels", &self.levels)
            .finish_non_exhaustive()
    }
}

impl Pool {
    /// A pool for `source`'s database, running `levels`, whose login
    /// `target` is; it connects on first use.
    pub fn new(source: Source, levels: Levels, target: Target, limits: Limits) -> Self {
        let size = limits.size.max(1);
        Self {
            source,
            levels,
            target,
            limits,
            permits: Semaphore::new(size),
            idle: Mutex::new(Vec::with_capacity(size)),
            key: None,
        }
    }

    /// The same pool, attaching a signed identity with `key` to every
    /// operation on an own database (blackbox/CONTRACT.md, "Identity").
    #[must_use]
    pub fn with_identity_key(mut self, key: Arc<IdentityKey>) -> Self {
        self.key = Some(key);
        self
    }

    /// Runs `sql` in a transaction of `level`, labelled `label`, its
    /// result's values read as `values` says. A level the pool's login is
    /// not for is never run: answered unavailable.
    pub async fn run(
        &self,
        level: Level,
        sql: &str,
        params: &[Param],
        values: Values,
        label: Label<'_>,
    ) -> Outcome {
        if !self.levels.permit(level) {
            self.report("an operation of a level this login is not for");
            return unavailable();
        }
        let checkout = self.limits.checkout;
        let Ok(Ok(_permit)) = tokio::time::timeout(checkout, self.permits.acquire()).await else {
            self.report("no connection was free in time");
            return unavailable();
        };
        let (mut connection, reused) = match self.take_idle() {
            Some(connection) => (connection, true),
            None => match self.open().await {
                Ok(connection) => (connection, false),
                Err(()) => return unavailable(),
            },
        };
        let operation = wire::Operation {
            level,
            sql,
            params,
            values,
            label,
            key: self.key.as_deref(),
        };
        let mut run = connection.operate(&operation, &self.limits).await;
        if run.stale && reused {
            // An idle connection the server had closed: the statement did
            // not reach it (and nothing is committed without the host's own
            // COMMIT), so it goes once more on a new one.
            drop(connection);
            connection = match self.open().await {
                Ok(connection) => connection,
                Err(()) => return unavailable(),
            };
            run = connection.operate(&operation, &self.limits).await;
        }
        if matches!(run.result, Err(Failure::Unavailable)) {
            self.report("the connection failed during the statement");
        }
        if let Some(process_id) = run.stuck {
            drop(connection);
            self.terminate(process_id).await;
        } else if run.reusable {
            self.give_back(connection);
        }
        Outcome {
            duration: run
                .sent
                .map(|sent| run.ended.saturating_duration_since(sent)),
            result: run.result,
        }
    }

    /// Takes a connection for a transaction that stays open until it is
    /// ended ([`Held`]), within the checkout wait, as [`Pool::run`] does. It
    /// holds one of the pool's permits until then. Nothing is sent yet.
    pub async fn checkout(&self, level: Level) -> Result<Held<'_>, Failure> {
        if !self.levels.permit(level) {
            self.report("a transaction of a level this login is not for");
            return Err(Failure::Unavailable);
        }
        let checkout = self.limits.checkout;
        let Ok(Ok(permit)) = tokio::time::timeout(checkout, self.permits.acquire()).await else {
            self.report("no connection was free in time");
            return Err(Failure::Unavailable);
        };
        let (connection, reused) = match self.take_idle() {
            Some(connection) => (connection, true),
            None => (self.open().await.map_err(|()| Failure::Unavailable)?, false),
        };
        Ok(Held {
            pool: self,
            _permit: permit,
            level,
            connection: Some(connection),
            reused,
            begun: false,
            aborted: false,
        })
    }

    async fn open(&self) -> Result<Connection, ()> {
        Connection::open(&self.target, self.limits.connect)
            .await
            .map_err(|error| self.report(error.describe()))
    }

    /// Ends the backend `process_id` from a connection of its own, as the
    /// same login (a role may end its own backends), within the connect
    /// limit and the grace period.
    async fn terminate(&self, process_id: i32) {
        let ended = async {
            let mut connection = Connection::open(&self.target, self.limits.connect).await?;
            let sql = format!("SELECT pg_terminate_backend({process_id})");
            tokio::time::timeout(self.limits.grace, connection.simple(&sql))
                .await
                .map_err(|_| WireError::TimedOut)?
        };
        if !matches!(ended.await, Ok(true)) {
            self.report("a statement past its limit could not be ended");
        }
    }

    /// A record in the program's log. Names the Resource or the
    /// Application and a fixed description, never anything of its
    /// connection.
    fn report(&self, problem: &str) {
        match &self.source {
            Source::Resource(resource) => log::record(
                "shared-database-unavailable",
                serde_json::json!({ "resource": resource, "problem": problem }),
            ),
            Source::OwnDatabase(application) => log::record(
                "own-database-unavailable",
                serde_json::json!({ "application": &**application, "problem": problem }),
            ),
        }
    }

    fn take_idle(&self) -> Option<Connection> {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
    }

    fn give_back(&self, connection: Connection) {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(connection);
    }
}

/// A connection held by one transaction, from its begin to its end
/// (blackbox/CONTRACT.md, "Transactions"), with one of its pool's permits:
/// between them it runs nothing but that transaction's statements, one at a
/// time (each takes `&mut self`). Whoever holds it ends it: [`Held::end`]
/// (the connection goes back to its pool only shown clean, as after any
/// operation), [`Held::halt`] when it was left in the middle of a
/// statement, or [`Held::release`] before it was begun. Dropped otherwise,
/// the connection is closed, which rolls its transaction back.
pub struct Held<'p> {
    pool: &'p Pool,
    _permit: SemaphorePermit<'p>,
    level: Level,
    /// `None` once given up: the transaction is over.
    connection: Option<Connection>,
    /// Taken from the idle ones: the server may have closed it meanwhile.
    reused: bool,
    begun: bool,
    /// A statement Postgres refused left the transaction aborted.
    aborted: bool,
}

impl fmt::Debug for Held<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Held")
            .field("pool", &self.pool)
            .field("level", &self.level)
            .field("open", &self.connection.is_some())
            .field("aborted", &self.aborted)
            .finish_non_exhaustive()
    }
}

impl Held<'_> {
    /// Begins the transaction, read-only or read-write as the checkout's
    /// level says, at `isolation`, labelled `label` (and on an own database
    /// with the signed identity attached, once, for all of it). A reused
    /// connection the server had closed is replaced once by a new one.
    /// Answers how long it took; on a failure the connection is given up.
    pub async fn begin(
        &mut self,
        isolation: Option<Isolation>,
        label: Label<'_>,
    ) -> Result<Duration, Failure> {
        let begin = wire::Begin {
            level: self.level,
            isolation,
            label,
            key: self.pool.key.as_deref(),
        };
        let limit = self.pool.limits.statement;
        let started = Instant::now();
        let Some(connection) = self.connection.as_mut() else {
            return Err(Failure::Unavailable);
        };
        self.begun = true;
        let mut began = connection.begin_transaction(&begin, limit).await;
        if matches!(began, Err(Exchange::Stale)) && self.reused {
            self.connection = None;
            let mut fresh = self.pool.open().await.map_err(|()| Failure::Unavailable)?;
            began = fresh.begin_transaction(&begin, limit).await;
            self.connection = Some(fresh);
        }
        self.reused = false;
        match began {
            Ok(()) => Ok(started.elapsed()),
            Err(_) => {
                self.connection = None;
                self.pool.report("a transaction could not be begun");
                Err(Failure::Unavailable)
            }
        }
    }

    /// Whether the transaction is still open on its connection.
    pub fn is_open(&self) -> bool {
        self.connection.is_some()
    }

    /// Whether Postgres has the transaction aborted: a statement failed.
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// Runs `sql` in the transaction, under the pool's limits, its time cut
    /// at `cap` if that comes first. A statement Postgres refuses leaves the
    /// transaction open and aborted. A statement a limit ended, or a
    /// connection that failed, ends the transaction: rolled back, and the
    /// connection given up (its backend ended from another connection if
    /// it does not come back from the cancel).
    pub async fn run(
        &mut self,
        sql: &str,
        params: &[Param],
        values: Values,
        cap: Instant,
    ) -> Outcome {
        let Some(connection) = self.connection.as_mut() else {
            return unavailable();
        };
        let statement = wire::Statement {
            sql,
            params,
            values,
        };
        let run = connection
            .statement(&statement, &self.pool.limits, cap)
            .await;
        if matches!(run.result, Err(Failure::Unavailable)) {
            self.pool
                .report("the connection failed during a statement in a transaction");
        }
        if run.reusable {
            self.aborted = run.aborted;
        } else {
            self.connection = None;
            if let Some(process_id) = run.stuck {
                self.pool.terminate(process_id).await;
            }
        }
        Outcome {
            duration: run
                .sent
                .map(|sent| run.ended.saturating_duration_since(sent)),
            result: run.result,
        }
    }

    /// Ends the transaction: `COMMIT` when `commit`, else `ROLLBACK`, with
    /// `DISCARD ALL`, within the grace period. The connection goes back to
    /// its pool only if both were answered without error and it is idle;
    /// otherwise it is closed. A `COMMIT` Postgres refused is its failure.
    pub async fn end(mut self, commit: bool) -> Result<(), Failure> {
        let Some(mut connection) = self.connection.take() else {
            return Err(Failure::Unavailable);
        };
        // A read-write transaction on an own database is committed only
        // after the check (blackbox/CONTRACT.md, "What a migration leaves
        // behind").
        let checked = self.pool.key.is_some() && self.level == Level::Write;
        let (ending, clean) = connection
            .end_transaction(commit, checked, self.pool.limits.grace)
            .await;
        if clean {
            self.pool.give_back(connection);
        }
        ending
    }

    /// Ends a transaction whose connection was left in an unknown state (a
    /// statement given up halfway): cancels what runs, waits up to the
    /// grace period for it, rolls back, and closes the connection; if it
    /// did not come back, its backend is ended from another connection.
    pub async fn halt(mut self) {
        let Some(mut connection) = self.connection.take() else {
            return;
        };
        let stuck = connection.halt(self.pool.limits.grace).await;
        drop(connection);
        if let Some(process_id) = stuck {
            self.pool.terminate(process_id).await;
        }
    }

    /// Lets go of a connection on which nothing was begun: back to its pool.
    pub fn release(mut self) {
        if let Some(connection) = self.connection.take()
            && !self.begun
        {
            self.pool.give_back(connection);
        }
    }
}

fn unavailable() -> Outcome {
    Outcome {
        result: Err(Failure::Unavailable),
        duration: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_of_a_target_is_printed() {
        let target = Target::new(
            "db.example".into(),
            6543,
            "secret_db".into(),
            "secret_role".into(),
            Password::new("secret-password".into()),
        );
        let printed = format!("{target:?}");
        for hidden in [
            "db.example",
            "6543",
            "secret_db",
            "secret_role",
            "secret-password",
        ] {
            assert!(!printed.contains(hidden), "{printed}");
        }
        let pool = Pool::new(
            Source::Resource("devops".into()),
            Levels::Only(Level::Write),
            target,
            Limits::default(),
        );
        assert!(!format!("{pool:?}").contains("secret"));
    }
}
