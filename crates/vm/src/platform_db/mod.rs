//! The platform database: where the program keeps everything it must
//! remember (ADR 0015), on the Postgres `VM_DATABASE_URL` names.
//!
//! Interface:
//!
//! - [`DatabaseAddress`]: `VM_DATABASE_URL` read, a type that never prints
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

use tokio_postgres::error::SqlState;

pub use address::{DatabaseAddress, FORM, Password, VARIABLE};
use pool::Connection;
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

/// What went wrong talking to Postgres. Its text comes from Postgres's
/// own message or from this program; it never holds the password.
#[derive(Debug)]
pub struct DbError {
    code: Option<SqlState>,
    timed_out: bool,
    message: String,
}

impl DbError {
    pub fn timed_out(what: &str, limit: Duration) -> Self {
        Self {
            code: None,
            timed_out: true,
            message: format!("{what} took longer than {} ms", limit.as_millis()),
        }
    }

    pub fn closed() -> Self {
        Self {
            code: None,
            timed_out: false,
            message: "the pool is closed".into(),
        }
    }

    /// A malformed or unexpected answer from the database.
    pub fn unexpected(what: &str) -> Self {
        Self {
            code: None,
            timed_out: false,
            message: format!("unexpected answer from the platform database: {what}"),
        }
    }

    #[must_use]
    pub fn context(mut self, context: &str) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    pub fn code(&self) -> Option<&SqlState> {
        self.code.as_ref()
    }

    pub fn is_timeout(&self) -> bool {
        self.timed_out
    }
}

/// Postgres's error, kept as its code and its message.
impl From<tokio_postgres::Error> for DbError {
    fn from(error: tokio_postgres::Error) -> Self {
        match error.as_db_error() {
            Some(db) => Self {
                code: Some(db.code().clone()),
                timed_out: false,
                message: format!("{}: {} ({})", db.severity(), db.message(), db.code().code()),
            },
            None => Self {
                code: error.code().cloned(),
                timed_out: false,
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

/// The platform database, open.
pub struct PlatformDatabase {
    pool: Pool,
}

impl PlatformDatabase {
    /// Connects, creating the database if it does not exist, and brings its
    /// structure up to date. The error is a message for the operator; it
    /// names the database, the server and the role, never the password.
    pub async fn open(address: &DatabaseAddress, limits: &PoolLimits) -> Result<Self, String> {
        let database = Self::connect(address, limits).await?;
        let applied = steps::bring_up_to_date(&database.pool, steps::STEPS)
            .await
            .map_err(|e| format!("the platform database \"{}\": {e}", address.database()))?;
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
    pub async fn connect(address: &DatabaseAddress, limits: &PoolLimits) -> Result<Self, String> {
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
async fn create_database(address: &DatabaseAddress, limits: &PoolLimits) -> Result<(), String> {
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
        Err(error) => Err(format!(
            "cannot create the platform database \"{}\" on Postgres at {} as the role \"{}\": {error}",
            address.database(),
            address.server(),
            address.role()
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

fn cannot_connect(address: &DatabaseAddress, error: &DbError) -> String {
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
fn quote_identifier(name: &str) -> String {
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
