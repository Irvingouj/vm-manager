//! The Operation log (ADR 0005; blackbox/CONTRACT.md, "The Operation log"):
//! one entry per operation on a Resource or on an Application's own
//! database, allowed or refused, kept in the platform database. Written by the Operation gateway, read through the
//! platform's API.
//!
//! Interface: [`OperationLog::refused`] writes a refused operation's entry
//! whole, and [`OperationLog::failed`] the whole entry of an allowed one
//! that failed before its statement was sent ([`OperationLog::whole`] any
//! whole entry, such as the host's own `rollback`); [`OperationLog::begin`] writes an allowed one's as `unfinished`
//! before its statement is sent, and [`OperationLog::finish`] completes it.
//! A pull of a Resource's structure (crate::structure), which is no
//! Application's, has [`OperationLog::begin_structure`] and
//! [`OperationLog::refused_structure`]: `application`, `version` and
//! `statement` null.
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

/// The columns [`entry_of`] reads, in its order.
macro_rules! ENTRY_COLUMNS {
    () => {
        concat!(
            "id, ",
            rfc3339!("time"),
            ", request, user_id, application, version, resource, operation,
               statement, outcome, reason, error, rows, duration_ms,
               actor, own_database, transaction_id, host"
        )
    };
}

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
    /// The Resource; `None` on the Application's own database.
    pub resource: Option<&'a str>,
    /// `user` or `application`.
    pub actor: &'a str,
    pub operation: &'a str,
    /// Without its values. For `begin`, `commit` and `rollback`, that word;
    /// null for a `fetch` (nothing of its address is kept).
    pub statement: Option<&'a str>,
    /// The transaction's id, for a statement in a transaction and for its
    /// `begin`, `commit` and `rollback`; `None` otherwise.
    pub transaction: Option<&'a str>,
    /// The host an outbound call went to, as the list writes it (the next
    /// stage: a `fetch`'s entry); `None` for every other operation.
    pub host: Option<&'a str>,
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
    /// A `begin` that began nothing: its entry's `transaction` becomes null.
    pub began_nothing: bool,
}

pub struct OperationLog {
    database: Arc<PlatformDatabase>,
}

/// Whether an entry is of the Application's own database: it has no
/// Resource and is not a `fetch`, which has none either.
fn own_database(entry: &Entry<'_>) -> bool {
    entry.resource.is_none() && entry.operation != "fetch"
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
                                                actor, own_database, operation, statement,
                                                transaction_id, outcome, reason, host)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'refused', $11, $12)",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.actor,
                        &own_database(entry),
                        &entry.operation,
                        &entry.statement,
                        &entry.transaction,
                        &reason,
                        &entry.host,
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
        self.whole(entry, "failed", Some(error)).await
    }

    /// Writes the whole entry of an operation with this `outcome` and
    /// `error`, and no rows: what [`OperationLog::failed`] writes, and the
    /// `rollback` the host makes on its own (`allowed`, `error` saying why).
    pub async fn whole(
        &self,
        entry: &Entry<'_>,
        outcome: &str,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        let version = version_of(entry.version)?;
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                actor, own_database, operation, statement,
                                                transaction_id, outcome, error, host)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.actor,
                        &own_database(entry),
                        &entry.operation,
                        &entry.statement,
                        &entry.transaction,
                        &outcome,
                        &error,
                        &entry.host,
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
                                                actor, own_database, operation, statement,
                                                transaction_id, outcome, host)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'unfinished', $11)
                     RETURNING id",
                    &[
                        &entry.request,
                        &entry.user,
                        &entry.application,
                        &version,
                        &entry.resource,
                        &entry.actor,
                        &own_database(entry),
                        &entry.operation,
                        &entry.statement,
                        &entry.transaction,
                        &entry.host,
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
                     SET outcome = $2, error = $3, rows = $4, duration_ms = $5,
                         transaction_id = CASE WHEN $6 THEN NULL ELSE transaction_id END
                     WHERE id = $1",
                    &[
                        &id,
                        &ending.outcome,
                        &ending.error,
                        &rows,
                        &duration,
                        &ending.began_nothing,
                    ],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
    }

    /// Completes the entry `id` as refused for `reason`: a `fetch` whose
    /// name resolved to an address that is never called, found after its
    /// entry was written (blackbox/CONTRACT.md, "Outbound calls").
    pub async fn finish_refused(
        &self,
        id: i64,
        reason: &str,
        duration_ms: Option<u64>,
    ) -> Result<(), DbError> {
        let duration = duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(too_large)?;
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "UPDATE operation_log
                     SET outcome = 'refused', reason = $2, duration_ms = $3
                     WHERE id = $1",
                    &[&id, &reason, &duration],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
    }

    /// Writes the entry of an allowed pull of `resource`'s structure by
    /// `user` in `request`, `unfinished`, before anything is sent to the
    /// shared database, and answers its id. [`OperationLog::finish`]
    /// completes it.
    pub async fn begin_structure(
        &self,
        request: &str,
        user: &str,
        resource: &str,
    ) -> Result<i64, DbError> {
        self.write(async |client: &mut Client| {
            let row = client
                .query_one(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                actor, own_database, operation, statement, outcome)
                     VALUES ($1, $2, NULL, NULL, $3, 'user', false, 'structure', NULL,
                             'unfinished')
                     RETURNING id",
                    &[&request, &user, &resource],
                )
                .await
                .map_err(DbError::from)?;
            row.try_get(0).map_err(DbError::from)
        })
        .await
    }

    /// Writes the whole entry of a refused pull of `resource`'s structure.
    pub async fn refused_structure(
        &self,
        request: &str,
        user: &str,
        resource: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        self.write(async |client: &mut Client| {
            client
                .execute(
                    "INSERT INTO operation_log (request, user_id, application, version, resource,
                                                actor, own_database, operation, statement, outcome,
                                                reason)
                     VALUES ($1, $2, NULL, NULL, $3, 'user', false, 'structure', NULL,
                             'refused', $4)",
                    &[&request, &user, &resource, &reason],
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
                            "SELECT ",
                            ENTRY_COLUMNS!(),
                            " FROM (SELECT * FROM operation_log ORDER BY id DESC LIMIT $1) newest
                             ORDER BY id"
                        ),
                        &[&limit],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        rows.iter().map(entry_of).collect()
    }

    /// One page of the entries that match `filter` (blackbox/CONTRACT.md,
    /// "What Platform administrators see and stop: Reading the Operation
    /// log"): the newest `filter.limit` of them, oldest first, as the API
    /// answers them, stopped before what they take written as JSON (each
    /// with a comma) would pass `max_bytes`, always at least one. Answers
    /// them and whether an older entry matches.
    ///
    /// One statement, built of the filters given (each a bound parameter),
    /// each of which an index serves with `id` (crate::platform_db::steps,
    /// step 16), in a read-only transaction whose statements Postgres ends
    /// after `limit_time`. It reads at most `limit` + 1 rows, and of them
    /// only as many as their statements' bytes let fit.
    pub async fn filtered(
        &self,
        filter: &Filter<'_>,
        max_bytes: usize,
        limit_time: Duration,
    ) -> Result<(Vec<Value>, bool), DbError> {
        let mut conditions: Vec<String> = Vec::new();
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for (column, value) in [
            ("user_id", &filter.user),
            ("application", &filter.application),
            ("resource", &filter.resource),
            ("host", &filter.host),
            ("outcome", &filter.outcome),
        ] {
            if let Some(value) = value {
                params.push(value);
                conditions.push(format!("{column} = ${}", params.len()));
            }
        }
        if let Some(since) = &filter.since {
            params.push(since);
            conditions.push(format!("time >= ${}::text::timestamptz", params.len()));
        }
        if let Some(until) = &filter.until {
            params.push(until);
            conditions.push(format!("time < ${}::text::timestamptz", params.len()));
        }
        if let Some(before) = &filter.before {
            params.push(before);
            conditions.push(format!("id < ${}", params.len()));
        }
        let fetched = filter.limit.saturating_add(1);
        params.push(&fetched);
        let limit_at = params.len();
        let budget = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        params.push(&budget);
        let budget_at = params.len();
        let matching = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        // The rows past what their statements alone would take beyond the
        // budget are not read (the first always is); `n` says how many
        // matched up to `limit` + 1.
        let sql = format!(
            concat!(
                "SELECT ",
                ENTRY_COLUMNS!(),
                ", n FROM (
                    SELECT o.*, row_number() OVER w AS rn,
                           sum(coalesce(octet_length(o.statement), 0)) OVER w AS upto,
                           count(*) OVER () AS n
                    FROM (SELECT * FROM operation_log {matching} ORDER BY id DESC LIMIT ${limit_at}) o
                    WINDOW w AS (ORDER BY o.id DESC ROWS UNBOUNDED PRECEDING)
                 ) t
                 WHERE rn = 1 OR upto <= ${budget_at}
                 ORDER BY id DESC"
            ),
            matching = matching,
            limit_at = limit_at,
            budget_at = budget_at,
        );
        let timeout = crate::registry::statement_timeout(limit_time);
        let read = self.database.pool().run(async |client: &mut Client| {
            let transaction = client
                .build_transaction()
                .read_only(true)
                .start()
                .await
                .map_err(DbError::from)?;
            transaction
                .batch_execute(&timeout)
                .await
                .map_err(DbError::from)?;
            let rows = transaction
                .query(&sql, &params)
                .await
                .map_err(DbError::from)?;
            transaction.commit().await.map_err(DbError::from)?;
            Ok(rows)
        });
        // Postgres ends the statement first; this is the backstop.
        let backstop = limit_time.saturating_add(Duration::from_secs(1));
        let rows = match tokio::time::timeout(backstop, read).await {
            Ok(rows) => rows?,
            Err(_) => return Err(DbError::timed_out("reading the Operation log", limit_time)),
        };
        let matched = match rows.first() {
            Some(row) => get::<i64>(row, 18)?,
            None => 0,
        };
        let limit = usize::try_from(filter.limit).unwrap_or(usize::MAX);
        let mut entries = Vec::with_capacity(rows.len().min(limit));
        let mut taken = 0_usize;
        for row in &rows {
            if entries.len() >= limit {
                break;
            }
            let entry = entry_of(row)?;
            let size = serde_json::to_vec(&entry).map_or(usize::MAX, |b| b.len().saturating_add(1));
            if !entries.is_empty() && taken.saturating_add(size) > max_bytes {
                break;
            }
            taken = taken.saturating_add(size);
            entries.push(entry);
        }
        let more = i64::try_from(entries.len()).map_or(true, |kept| kept < matched);
        entries.reverse();
        Ok((entries, more))
    }

    /// The entries of the request `request`, oldest first, as the API
    /// answers them; at most [`MAX_READ`].
    pub async fn of_request(&self, request: &str) -> Result<Vec<Value>, DbError> {
        let limit = i64::from(MAX_READ);
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        concat!(
                            "SELECT ",
                            ENTRY_COLUMNS!(),
                            " FROM operation_log WHERE request = $1 ORDER BY id LIMIT $2"
                        ),
                        &[&request, &limit],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        rows.iter().map(entry_of).collect()
    }
}

/// What a reading of the Operation log by a Platform administrator asks
/// for ([`OperationLog::filtered`]): each filter given matches exactly.
#[derive(Debug, Default)]
pub struct Filter<'a> {
    pub user: Option<&'a str>,
    pub application: Option<&'a str>,
    pub resource: Option<&'a str>,
    /// The host of a `fetch`, exactly as the list writes it.
    pub host: Option<&'a str>,
    pub outcome: Option<&'a str>,
    /// RFC 3339, checked: entries at or after it.
    pub since: Option<&'a str>,
    /// RFC 3339, checked: entries before it.
    pub until: Option<&'a str>,
    /// Entries whose `id` is below it.
    pub before: Option<i64>,
    /// How many at most: 1 to [`MAX_READ`].
    pub limit: i64,
}

/// One entry as the API answers it, from a row of [`ENTRY_COLUMNS`].
fn entry_of(row: &tokio_postgres::Row) -> Result<Value, DbError> {
    let mut entry = Map::with_capacity(18);
    entry.insert("id".into(), get::<i64>(row, 0)?.into());
    entry.insert("time".into(), get::<String>(row, 1)?.into());
    entry.insert("request".into(), get::<Option<String>>(row, 2)?.into());
    entry.insert("user".into(), get::<Option<String>>(row, 3)?.into());
    entry.insert("application".into(), get::<Option<String>>(row, 4)?.into());
    entry.insert("version".into(), get::<Option<i32>>(row, 5)?.into());
    entry.insert("resource".into(), get::<Option<String>>(row, 6)?.into());
    entry.insert("actor".into(), get::<String>(row, 14)?.into());
    entry.insert("own_database".into(), get::<bool>(row, 15)?.into());
    entry.insert("operation".into(), get::<String>(row, 7)?.into());
    entry.insert("statement".into(), get::<Option<String>>(row, 8)?.into());
    entry.insert("outcome".into(), get::<String>(row, 9)?.into());
    entry.insert("reason".into(), get::<Option<String>>(row, 10)?.into());
    entry.insert("error".into(), get::<Option<String>>(row, 11)?.into());
    entry.insert("rows".into(), get::<Option<i64>>(row, 12)?.into());
    entry.insert("duration_ms".into(), get::<Option<i64>>(row, 13)?.into());
    entry.insert("transaction".into(), get::<Option<String>>(row, 16)?.into());
    entry.insert("host".into(), get::<Option<String>>(row, 17)?.into());
    Ok(Value::Object(entry))
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
