//! A small client of the harness's own for its Postgres instances: to set up
//! the shared databases the tests register as Resources, and to look at
//! them from outside the program (their rows, their sessions). It is never
//! handed to the program.
//!
//! Synchronous for the tests: each call runs on a single-threaded runtime
//! owned by the client, with a timeout. Values come back as the text
//! Postgres prints for them (the simple query protocol).

use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};

/// How long connecting, or one call, may take.
pub const SQL_TIMEOUT: Duration = Duration::from_secs(30);

/// One connection.
pub struct Sql {
    client: Option<Client>,
    /// Drives the connection; aborted when the client is dropped.
    connection: Option<JoinHandle<()>>,
    runtime: Runtime,
}

/// An error Postgres answered, by its SQLSTATE and message; `code` is
/// empty when the error did not come from the server (a timeout, a closed
/// connection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbError {
    pub code: String,
    pub message: String,
}

impl DbError {
    fn of(e: &tokio_postgres::Error) -> Self {
        match e.as_db_error() {
            Some(d) => Self {
                code: d.code().code().to_owned(),
                message: d.message().to_owned(),
            },
            None => Self {
                code: String::new(),
                message: e.to_string(),
            },
        }
    }

    fn other(message: String) -> Self {
        Self {
            code: String::new(),
            message,
        }
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl Sql {
    /// Connects to this address (`postgres://role:password@host:port/db`).
    pub fn connect(url: &str) -> Result<Self, String> {
        Self::connect_coded(url).map_err(|e| format!("cannot connect: {e}"))
    }

    /// As [`Sql::connect`], with Postgres's SQLSTATE when it refused: to
    /// tell "permission denied for database" (`42501`) from a wrong
    /// password (`28P01`).
    pub fn connect_coded(url: &str) -> Result<Self, DbError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                DbError::other(format!(
                    "cannot start a runtime for the harness's SQL client: {e}"
                ))
            })?;
        let (client, connection) = runtime
            .block_on(async {
                tokio::time::timeout(SQL_TIMEOUT, tokio_postgres::connect(url, NoTls)).await
            })
            .map_err(|_| DbError::other(format!("connecting took longer than {SQL_TIMEOUT:?}")))?
            .map_err(|e| DbError::of(&e))?;
        let connection = runtime.spawn(async move {
            drop(connection.await);
        });
        Ok(Self {
            client: Some(client),
            connection: Some(connection),
            runtime,
        })
    }

    fn client(&self) -> &Client {
        self.client
            .as_ref()
            .expect("the client is there until dropped")
    }

    /// Runs one or more statements; an error says which and why.
    pub fn try_batch(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let client = self.client();
        let messages = self
            .runtime
            .block_on(async { tokio::time::timeout(SQL_TIMEOUT, client.simple_query(sql)).await })
            .map_err(|_| format!("`{sql}` took longer than {SQL_TIMEOUT:?}"))?
            .map_err(|e| {
                let detail = e
                    .as_db_error()
                    .map(|d| format!("{} ({})", d.message(), d.code().code()))
                    .unwrap_or_else(|| e.to_string());
                format!("`{sql}` failed: {detail}")
            })?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(row) => Some(
                    (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect(),
                ),
                _ => None,
            })
            .collect())
    }

    /// Runs one or more statements; an error is Postgres's SQLSTATE and
    /// message.
    pub fn try_coded(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
        let client = self.client();
        let messages = self
            .runtime
            .block_on(async { tokio::time::timeout(SQL_TIMEOUT, client.simple_query(sql)).await })
            .map_err(|_| DbError::other(format!("`{sql}` took longer than {SQL_TIMEOUT:?}")))?
            .map_err(|e| DbError::of(&e))?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(row) => Some(
                    (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect(),
                ),
                _ => None,
            })
            .collect())
    }

    /// Runs one or more statements, failing the test if any fails. Returns
    /// the rows, each value as Postgres prints it (`None` for null).
    pub fn batch(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        self.try_batch(sql)
            .unwrap_or_else(|problem| panic!("the harness's SQL: {problem}"))
    }

    /// The first value of the first row, or `None` when there is no row or
    /// the value is null.
    pub fn value(&self, sql: &str) -> Option<String> {
        self.batch(sql)
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next().flatten())
    }
}

impl Drop for Sql {
    fn drop(&mut self) {
        // Closing the client ends the connection's task; abort it in case
        // it has not noticed yet, then let the runtime finish it.
        drop(self.client.take());
        if let Some(connection) = self.connection.take() {
            connection.abort();
            drop(
                self.runtime
                    .block_on(async { tokio::time::timeout(SQL_TIMEOUT, connection).await }),
            );
        }
    }
}
