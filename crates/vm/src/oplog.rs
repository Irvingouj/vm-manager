//! The Operation log (ADR 0005; blackbox/CONTRACT.md, "The Operation log"):
//! one entry per operation on a Resource, allowed or refused, kept in the
//! platform database. Written by the Operation gateway, read through the
//! platform's API.
//!
//! Interface: [`OperationLog::refused`] writes a refused operation's entry
//! whole, and [`OperationLog::failed`] the whole entry of an allowed one
//! that failed before its statement was sent; [`OperationLog::begin`] writes an allowed one's as `unfinished`
//! before its statement is sent, and [`OperationLog::finish`] completes it.
//! Each write has [`WRITE_TIMEOUT`]. [`OperationLog::newest`] reads.
//!
//! An entry holds no value from a statement, a parameter or a row: the
//! statement it is given is already without its values (crate::statement).

use std::time::Duration;

use serde_json::{Map, Value};
use tokio_postgres::Client;
use tokio_postgres::types::FromSql;

use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use std::sync::Arc;

/// The longest writing one entry may take; past it the write has failed.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most entries one read gives.
pub const MAX_READ: u16 = 1000;

/// What an entry records about the operation, at its start.
#[derive(Debug)]
pub struct Entry<'a> {
    /// `None` when no request was in flight; then `user` is `None` too.
    pub request: Option<&'a str>,
    pub user: Option<&'a str>,
    pub application: &'a str,
    pub version: u64,
    pub resource: &'a str,
    pub operation: &'a str,
    /// Without its values.
    pub statement: &'a str,
}

/// How an allowed operation ended.
#[derive(Debug)]
pub struct Ending<'a> {
    /// `allowed` (it ran to its end) or `failed`.
    pub outcome: &'a str,
    /// The SQLSTATE, the limit's name or `unavailable`, when failed.
    pub error: Option<&'a str>,
    /// Rows, when it ran to its end.
    pub rows: Option<u64>,
    /// When the statement was sent.
    pub duration_ms: Option<u64>,
}

pub struct OperationLog {
    database: Arc<PlatformDatabase>,
}

impl OperationLog {
    pub fn new(database: Arc<PlatformDatabase>) -> Self {
        Self { database }
    }

    /// Writes the whole entry of a refused operation.
    pub async fn refused(&self, entry: &Entry<'_>, reason: &str) -> Result<(), DbError> {
        let version = version_of(entry.version)?;
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                operation, statement, outcome, reason)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'refused', $8)",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.operation,
                        &entry.statement,
                        &reason,
                    ],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
    }

    /// Writes the whole entry of an allowed operation that failed before
    /// its statement was sent (the statement check refused it, the Resource
    /// has no login for it): `failed`, with `error` saying why.
    pub async fn failed(&self, entry: &Entry<'_>, error: &str) -> Result<(), DbError> {
        let version = version_of(entry.version)?;
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                operation, statement, outcome, error)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'failed', $8)",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.operation,
                        &entry.statement,
                        &error,
                    ],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
    }

    /// Writes the entry of an allowed operation, `unfinished`, and answers
    /// its id.
    pub async fn begin(&self, entry: &Entry<'_>) -> Result<i64, DbError> {
        let version = version_of(entry.version)?;
        self.write(async |client: &mut Client| {
            let row = client
                .query_one(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                operation, statement, outcome)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'unfinished')
                     RETURNING id",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.operation,
                        &entry.statement,
                    ],
                )
                .await
                .map_err(DbError::from)?;
            row.try_get(0).map_err(DbError::from)
        })
        .await
    }

    /// Completes the entry `id`.
    pub async fn finish(&self, id: i64, ending: &Ending<'_>) -> Result<(), DbError> {
        let rows = ending
            .rows
            .map(i64::try_from)
            .transpose()
            .map_err(too_large)?;
        let duration = ending
            .duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(too_large)?;
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "UPDATE operation_log
                     SET outcome = $2, error = $3, rows = $4, duration_ms = $5
                     WHERE id = $1",
                    &[&id, &ending.outcome, &ending.error, &rows, &duration],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
    }

    async fn write<T>(
        &self,
        operation: impl AsyncFnOnce(&mut Client) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        tokio::time::timeout(WRITE_TIMEOUT, self.database.pool().run(operation))
            .await
            .map_err(|_| DbError::timed_out("writing to the Operation log", WRITE_TIMEOUT))?
    }

    /// The newest `limit` entries, oldest first, as the API answers them.
    pub async fn newest(&self, limit: u16) -> Result<Vec<Value>, DbError> {
        let limit = i64::from(limit.min(MAX_READ));
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        concat!(
                            "SELECT id, ",
                            rfc3339!("time"),
                            ", request, user_id, application, version, resource, operation,
                               statement, outcome, reason, error, rows, duration_ms
                             FROM (SELECT * FROM operation_log ORDER BY id DESC LIMIT $1) newest
                             ORDER BY id"
                        ),
                        &[&limit],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut entry = Map::with_capacity(14);
            entry.insert("id".into(), get::<i64>(row, 0)?.into());
            entry.insert("time".into(), get::<String>(row, 1)?.into());
            entry.insert("request".into(), get::<Option<String>>(row, 2)?.into());
            entry.insert("user".into(), get::<Option<String>>(row, 3)?.into());
            entry.insert("application".into(), get::<String>(row, 4)?.into());
            entry.insert("version".into(), get::<i32>(row, 5)?.into());
            entry.insert("resource".into(), get::<String>(row, 6)?.into());
            entry.insert("operation".into(), get::<String>(row, 7)?.into());
            entry.insert("statement".into(), get::<String>(row, 8)?.into());
            entry.insert("outcome".into(), get::<String>(row, 9)?.into());
            entry.insert("reason".into(), get::<Option<String>>(row, 10)?.into());
            entry.insert("error".into(), get::<Option<String>>(row, 11)?.into());
            entry.insert("rows".into(), get::<Option<i64>>(row, 12)?.into());
            entry.insert("duration_ms".into(), get::<Option<i64>>(row, 13)?.into());
            entries.push(Value::Object(entry));
        }
        Ok(entries)
    }
}

fn version_of(version: u64) -> Result<i32, DbError> {
    i32::try_from(version).map_err(too_large)
}

fn too_large<E>(_: E) -> DbError {
    DbError::unexpected("a number too large for the Operation log")
}

fn get<'a, T: FromSql<'a>>(row: &'a tokio_postgres::Row, index: usize) -> Result<T, DbError> {
    row.try_get(index).map_err(DbError::from)
}
