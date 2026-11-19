//! The platform database: where the program keeps everything it must
//! remember (ADR 0015), on the Postgres `VIBE_DATABASE_URL` names.
//!
//! Interface:
//!
//! - [`DatabaseAddress`]: `VIBE_DATABASE_URL` read, a type that never prints
//!   its password.
//! - [`PlatformDatabase::open`]: at start, connects (creating the database
//!   if it does not exist), brings its structure up to date ([`steps`]), and
//!   answers a pool, or a message naming the problem.
//! - [`Pool::run`]: one operation on one connection, within time limits.
//!   The modules that own tables (the Application registry, later Resources,
//!   Grants, the Operation log) write their own queries against it.
//! - [`DbError`]: what went wrong, with nothing secret in it.

mod address;
mod pool;
pub mod steps;

use std::fmt;
use std::time::Duration;

use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

pub use address::{DatabaseAddress, FORM, Password, VARIABLE};
pub(crate) use pool::Connection;
pub use pool::{Pool, PoolLimits};

use crate::log;

/// `created_at` and the like as RFC 3339 in UTC, with microseconds, for
/// the tables' queries.
macro_rules! rfc3339 {
    ($column:literal) => {
        concat!(
            "to_char(",
            $column,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}
pub(crate) use rfc3339;

/// Why a step on Postgres failed, as a value kept from where the failure
/// was seen (not read back out of a message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A time limit of the program's own passed: the whole time the step
    /// was given.
    TimedOut(Duration),
    /// The connection could not be opened, or was lost.
    Connection,
    /// The server answered with an error: its SQLSTATE, or `None` for an
    /// answer the program did not expect.
    Server(Option<SqlState>),
    /// The server (or the program that makes it) did not start, or ended.
    Ended,
}

impl Cause {
    /// Its fixed name: `timed-out`, `connection`, `server` or `ended`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::TimedOut(_) => "timed-out",
            Self::Connection => "connection",
            Self::Server(_) => "server",
            Self::Ended => "ended",
        }
    }

    /// The limit that passed, with [`Cause::TimedOut`] only.
    pub fn limit(&self) -> Option<Duration> {
        match self {
            Self::TimedOut(limit) => Some(*limit),
            _ => None,
        }
    }

    /// The server's SQLSTATE, with [`Cause::Server`] only.
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Server(Some(code)) => Some(code.code()),
            _ => None,
        }
    }

    /// The cause of `error`: the server's answer when it is one; a lost or
    /// refused connection when the error is about the connection (it was
    /// closed, or its socket failed); else an answer the program did not
    /// expect (a row of another form, a parameter it could not send).
    pub fn of(error: &tokio_postgres::Error) -> Self {
        if let Some(db) = error.as_db_error() {
            return Self::Server(Some(db.code().clone()));
        }
        let socket = std::error::Error::source(error)
            .is_some_and(|source| source.downcast_ref::<std::io::Error>().is_some());
        if error.is_closed() || socket {
            Self::Connection
        } else {
            Self::Server(None)
        }
    }
}

/// A whole number of seconds as a number, else with its fraction: `5`,
/// `2.5`.
pub fn seconds_text(limit: Duration) -> String {
    if limit.subsec_nanos() == 0 {
        limit.as_secs().to_string()
    } else {
        let text = format!("{:.3}", limit.as_secs_f64());
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// What went wrong talking to Postgres. Its text comes from Postgres's
/// own message or from this program; it never holds the password.
#[derive(Debug)]
pub struct DbError {
    cause: Cause,
    message: String,
}

impl DbError {
    pub fn timed_out(what: &str, limit: Duration) -> Self {
        Self {
            cause: Cause::TimedOut(limit),
            message: format!("{what} did not end within {} s", seconds_text(limit)),
        }
    }

    pub fn closed() -> Self {
        Self {
            cause: Cause::Connection,
            message: "the pool is closed".into(),
        }
    }

    /// A malformed or unexpected answer from the database.
    pub fn unexpected(what: &str) -> Self {
        Self {
            cause: Cause::Server(None),
            message: format!("unexpected answer from the platform database: {what}"),
        }
    }

    #[must_use]
    pub fn context(mut self, context: &str) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    pub fn code(&self) -> Option<&SqlState> {
        match &self.cause {
            Cause::Server(code) => code.as_ref(),
            _ => None,
        }
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self.cause, Cause::TimedOut(_))
    }

    /// Why it went wrong, as a value.
    pub fn cause(&self) -> &Cause {
        &self.cause
    }
}

/// Postgres's error, kept as its cause and its message.
impl From<tokio_postgres::Error> for DbError {
    fn from(error: tokio_postgres::Error) -> Self {
        let cause = Cause::of(&error);
        match error.as_db_error() {
            Some(db) => Self {
                cause,
                message: format!("{}: {} ({})", db.severity(), db.message(), db.code().code()),
            },
            None => Self {
                cause,
                message: error.to_string(),
            },
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why the platform database (or the parts on it) could not be opened: a
/// message for the operator, and its cause when it is Postgres's (`None`:
/// something else of the program's, the platform database's structure
/// written by a newer program, the engine threads).
#[derive(Debug)]
pub struct OpenError {
    pub cause: Option<Cause>,
    pub message: String,
}

impl OpenError {
    pub fn new(cause: Option<Cause>, message: String) -> Self {
        Self { cause, message }
    }

    /// `message`, with the cause of `error`.
    pub fn of(error: &DbError, message: String) -> Self {
        Self {
            cause: Some(error.cause().clone()),
            message,
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<OpenError> for String {
    fn from(error: OpenError) -> Self {
        error.message
    }
}

/// The platform database, open.
pub struct PlatformDatabase {
    pool: Pool,
}

impl PlatformDatabase {
    /// Connects, creating the database if it does not exist, and brings its
    /// structure up to date. The error is a message for the operator; it
    /// names the database, the server and the role, never the password.
    pub async fn open(address: &DatabaseAddress, limits: &PoolLimits) -> Result<Self, OpenError> {
        let database = Self::connect(address, limits).await?;
        let applied = steps::bring_up_to_date(&database.pool, steps::STEPS)
            .await
            .map_err(|e| {
                let cause = match &e {
                    steps::StepsError::Database(error) => Some(error.cause().clone()),
                    steps::StepsError::Newer { .. } => None,
                };
                OpenError::new(
                    cause,
                    format!("the platform database \"{}\": {e}", address.database()),
                )
            })?;
        database.close_to_public(address).await?;
        log::record(
            "platform-database",
            serde_json::json!({
                "database": address.database(),
                "server": address.server(),
                "steps_applied": applied,
                "structure_step": steps::STEPS.len(),
            }),
        );
        Ok(database)
    }

    /// Connects, creating the database if it does not exist; does not
    /// touch its structure.
    pub async fn connect(
        address: &DatabaseAddress,
        limits: &PoolLimits,
    ) -> Result<Self, OpenError> {
        let config = config(address, limits);
        let first = match Connection::open(&config, limits.connect).await {
            Ok(connection) => connection,
            Err(error) if error.code() == Some(&SqlState::INVALID_CATALOG_NAME) => {
                create_database(address, limits).await?;
                Connection::open(&config, limits.connect)
                    .await
                    .map_err(|e| cannot_connect(address, &e))?
            }
            Err(error) => return Err(cannot_connect(address, &error)),
        };
        Ok(Self {
            pool: Pool::new(config, limits.clone(), Some(first)),
        })
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Takes `CONNECT` on the platform database from `PUBLIC`, so that no
    /// role of an own database (which the platform's role creates, and
    /// which have it from `PUBLIC` alone) can connect to it and reach the
    /// Change history (blackbox/CONTRACT.md, "Where it is kept"). When the
    /// platform's role may not (IT made the database and owns it) and
    /// `PUBLIC` still has it, the program does not start: the
    /// `platform-database-open` record and the message say what to run.
    async fn close_to_public(&self, address: &DatabaseAddress) -> Result<(), OpenError> {
        let failed = |e: DbError| {
            OpenError::of(
                &e,
                format!(
                    "the platform database \"{}\": cannot take CONNECT from PUBLIC: {e}",
                    address.database()
                ),
            )
        };
        let still_open = self
            .pool
            .run(async |client: &mut Client| {
                if !public_may_connect(client).await? {
                    return Ok(false);
                }
                // Only a warning when the platform's role does not own the
                // database: then nothing is taken, and the second look says so.
                client
                    .batch_execute(
                        "DO $$ BEGIN
                             EXECUTE pg_catalog.format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC',
                                                       pg_catalog.current_database());
                         END $$",
                    )
                    .await
                    .map_err(DbError::from)?;
                public_may_connect(client).await
            })
            .await
            .map_err(failed)?;
        if !still_open {
            return Ok(());
        }
        let statement = format!(
            "REVOKE CONNECT ON DATABASE {} FROM PUBLIC",
            quote_identifier(address.database())
        );
        log::record(
            "platform-database-open",
            serde_json::json!({
                "database": address.database(),
                "server": address.server(),
                "statement": statement,
            }),
        );
        Err(OpenError::new(
            None,
            format!(
                "the platform database \"{}\" lets every role connect (PUBLIC has CONNECT on it), and the role \"{}\" may not take that away: the roles of Applications' own databases could read the Change history there. Have its owner run: {statement}",
                address.database(),
                address.role()
            ),
        ))
    }
}

/// Whether `PUBLIC` has `CONNECT` on the database `client` is connected to.
async fn public_may_connect(client: &Client) -> Result<bool, DbError> {
    client
        .query_one(
            "SELECT pg_catalog.has_database_privilege('public', pg_catalog.current_database(), 'CONNECT')",
            &[],
        )
        .await
        .and_then(|row| row.try_get::<_, bool>(0))
        .map_err(DbError::from)
}

fn config(address: &DatabaseAddress, limits: &PoolLimits) -> tokio_postgres::Config {
    let mut config = address.config(limits.connect);
    // The server ends a statement the program has stopped waiting for.
    let ms = limits.operation.as_millis();
    config.options(format!("-c statement_timeout={ms} -c lock_timeout={ms}"));
    config
}

/// Creates the platform database, through the maintenance database
/// `postgres` on the same server as the same role. Another program
/// creating it at the same moment is fine.
async fn create_database(address: &DatabaseAddress, limits: &PoolLimits) -> Result<(), OpenError> {
    let maintenance = address.with_database("postgres");
    let connection = Connection::open(&config(&maintenance, limits), limits.connect)
        .await
        .map_err(|e| cannot_connect(&maintenance, &e))?;
    let statement = format!("CREATE DATABASE {}", quote_identifier(address.database()));
    let created = tokio::time::timeout(
        limits.operation,
        connection_execute(&connection, &statement),
    )
    .await
    .map_err(|_| DbError::timed_out("creating the database", limits.operation))
    .and_then(|r| r);
    match created {
        Ok(()) => {
            log::record(
                "platform-database-created",
                serde_json::json!({ "database": address.database(), "server": address.server() }),
            );
            Ok(())
        }
        Err(error)
            if error.code() == Some(&SqlState::DUPLICATE_DATABASE)
                || error.code() == Some(&SqlState::UNIQUE_VIOLATION) =>
        {
            Ok(())
        }
        Err(error) => Err(OpenError::of(
            &error,
            format!(
                "cannot create the platform database \"{}\" on Postgres at {} as the role \"{}\": {error}",
                address.database(),
                address.server(),
                address.role()
            ),
        )),
    }
}

async fn connection_execute(connection: &Connection, statement: &str) -> Result<(), DbError> {
    connection
        .client()
        .batch_execute(statement)
        .await
        .map_err(DbError::from)
}

fn cannot_connect(address: &DatabaseAddress, error: &DbError) -> OpenError {
    OpenError::of(error, cannot_connect_words(address, error))
}

fn cannot_connect_words(address: &DatabaseAddress, error: &DbError) -> String {
    let refused = matches!(
        error.code(),
        Some(c) if *c == SqlState::INVALID_PASSWORD || *c == SqlState::INVALID_AUTHORIZATION_SPECIFICATION
    );
    if refused {
        format!(
            "Postgres at {} refused the role \"{}\" for the platform database \"{}\": {error}",
            address.server(),
            address.role(),
            address.database()
        )
    } else if error.is_timeout() {
        format!(
            "cannot reach Postgres at {} (role \"{}\", platform database \"{}\"): it did not answer: {error}",
            address.server(),
            address.role(),
            address.database()
        )
    } else {
        format!(
            "cannot reach Postgres at {} (role \"{}\", platform database \"{}\"): {error}",
            address.server(),
            address.role(),
            address.database()
        )
    }
}

/// `name` as a Postgres identifier in double quotes.
pub(crate) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{DatabaseAddress, PoolLimits};

    /// A platform database on the harness's private Postgres that no other
    /// test of this binary uses. It does not exist until it is connected to.
    pub fn fresh_address() -> DatabaseAddress {
        DatabaseAddress::parse(blackbox::postgres::Platform::fresh().url())
            .unwrap_or_else(|e| panic!("the harness's address cannot be read: {e}"))
    }

    pub fn limits() -> PoolLimits {
        PoolLimits::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_identifier("Vm-Platform"), "\"Vm-Platform\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
    }
}
