//! Reading the Change history: the operation `history` (blackbox/CONTRACT.md,
//! "Reading: the operation `history`"; research part D).
//!
//! A read ships the Application's outbox first ([`super::relay`]), so that it
//! holds every change committed before it began. Then:
//!
//! 1. in the own database, as the runtime role, in a read-only transaction
//!    (the request's identity attached, as for any `query`), every name
//!    qualified and the search path the runtime role's: the table as it is now (its
//!    columns, which of them are `json` or `jsonb`, its primary key), or
//!    `42P01`; and the key, if one is given and names exactly the primary
//!    key's columns, read into those columns' types and turned back into
//!    `jsonb` under the settings the capture pins, so that it is compared
//!    as the key columns compare values (research D4);
//! 2. in the platform database: the changes of that table of the
//!    Application in flight, newest first, at most `limit`, older than
//!    `before`; with a key, that row's and every `structure` entry; with a
//!    key that is not the primary key, the `structure` entries only. What it
//!    reads of `old` and `new` is bounded by the answer's byte limit;
//! 3. in the own database again, in the same transaction: each `old` and
//!    `new` read into the table's present row type
//!    (`jsonb_populate_record`), a `json` or `jsonb` column the capture saw
//!    holding JSON `null` given as JSON `null`, each value as `query` gives
//!    a value of that column.
//!
//! The caller bounds the whole of it in time (the shipment goes on by
//! itself past that); the statements in the own database are cut at the
//! same deadline. Nothing is answered unless all of it is.

use serde_json::{Map, Value};
use tokio::time::Instant;
use tokio_postgres::Client;

use super::OwnDatabases;
use crate::executor::{self, Failure, Held, Label, Level, Limit, Param, Rows, Values};
use crate::log;
use crate::platform_db::{DbError, quote_identifier, rfc3339};

/// The most bytes of `old` and `new` (as `jsonb` text) a read takes from
/// the platform database: an answer larger than the operation's byte
/// limit (blackbox/CONTRACT.md, "Limits": 8 MiB) is refused whole, without
/// reading the rest.
const ANSWER_BYTES: i64 = 8 * 1024 * 1024;

/// What a `history` call asks for, its arguments read.
#[derive(Debug)]
pub struct HistoryRead {
    pub schema: String,
    pub table: String,
    /// The key's columns and values, as given.
    pub key: Option<Map<String, Value>>,
    pub limit: i64,
    /// Only changes with a lesser id; `i64::MAX` when not given.
    pub before: i64,
    pub values: Values,
}

/// What a read answered: `{ columns, types?, changes }`, and how many
/// changes.
#[derive(Debug)]
pub struct HistoryAnswer {
    pub answer: Value,
    pub changes: u64,
}

/// The table as it is now.
struct Table {
    /// Its columns in order: name, and whether it is `json` (`Some(false)`)
    /// or `jsonb` (`Some(true)`).
    columns: Vec<(String, Option<bool>)>,
    /// The names of its primary key's columns; empty when it has none.
    key: Vec<String>,
}

/// Which changes of the table a read answers.
enum Which {
    All,
    /// That row's (by its key as the capture recorded it, `jsonb` text) and
    /// every `structure` entry.
    Row(String),
    /// Only the `structure` entries: a key that is not the primary key.
    Structure,
}

/// One change as the platform database holds it, for the answer.
struct Kept {
    id: String,
    at: String,
    kind: String,
    key: Option<String>,
    changed: Option<String>,
    user_id: String,
    actor: String,
    request: Option<String>,
    migration: Option<String>,
    structure: Option<String>,
    /// `{"j": old, "z": [json-null columns]}` as text, if there is an old
    /// row; the same of `new`.
    old: Option<String>,
    new: Option<String>,
}

/// The changes of the table, newest first, with the size of what they are
/// of `old` and `new`, and that size counted from the newest: a read stops
/// taking rows once past [`ANSWER_BYTES`].
macro_rules! changes_query {
    ($filter:literal) => {
        concat!(
            "SELECT y.id::text, y.at_text, y.kind, y.key::text, y.changed::text, y.user_id, y.actor,
                    y.request, y.migration, y.structure::text, y.old_doc, y.new_doc, y.running
               FROM (SELECT x.*, sum(x.size) OVER (ORDER BY x.id DESC) AS running
                       FROM (SELECT h.id, ",
            rfc3339!("h.at"),
            " AS at_text, h.kind, h.key, h.changed, h.user_id, h.actor, h.request,
                                    h.migration, h.structure,
                                    CASE WHEN h.old IS NULL THEN NULL ELSE jsonb_build_object(
                                        'j', h.old, 'z', coalesce(h.json_nulls -> 'old', '[]'::jsonb))::text END AS old_doc,
                                    CASE WHEN h.new IS NULL THEN NULL ELSE jsonb_build_object(
                                        'j', h.new, 'z', coalesce(h.json_nulls -> 'new', '[]'::jsonb))::text END AS new_doc,
                                    coalesce(octet_length(h.old::text), 0)
                                      + coalesce(octet_length(h.new::text), 0) AS size
                               FROM ",
            $filter,
            " ) x) y
              WHERE y.running - y.size <= $6
              ORDER BY y.id DESC"
        )
    };
}

/// Every change of the table.
const ALL: &str = changes_query!(
    "(SELECT * FROM change_history
       WHERE application = $1 AND schema_name = $2 AND table_name = $3 AND id < $4
       ORDER BY id DESC LIMIT $5) h WHERE $7::text IS NULL"
);

/// A row's changes and the `structure` entries.
const ROW: &str = changes_query!(
    "(SELECT * FROM (
        (SELECT * FROM change_history
          WHERE application = $1 AND schema_name = $2 AND table_name = $3 AND id < $4
            AND jsonb_hash_extended(key, 0) = jsonb_hash_extended($7::text::jsonb, 0)
            AND key = $7::text::jsonb
          ORDER BY id DESC LIMIT $5)
        UNION ALL
        (SELECT * FROM change_history
          WHERE application = $1 AND schema_name = $2 AND table_name = $3 AND id < $4
            AND kind = 'structure'
          ORDER BY id DESC LIMIT $5)) u
       ORDER BY id DESC LIMIT $5) h"
);

/// Only the `structure` entries.
const STRUCTURE: &str = changes_query!(
    "(SELECT * FROM change_history
       WHERE application = $1 AND schema_name = $2 AND table_name = $3 AND id < $4
         AND kind = 'structure'
       ORDER BY id DESC LIMIT $5) h WHERE $7::text IS NULL"
);

/// The settings the capture pins (crate::own_db's `own_db_schema.sql`,
/// `vm.capture()`), under which a key is turned back into `jsonb`.
const CAPTURE_SETTINGS: [(&str, &str); 5] = [
    ("TimeZone", "UTC"),
    ("extra_float_digits", "1"),
    ("IntervalStyle", "postgres"),
    ("DateStyle", "ISO, YMD"),
    ("bytea_output", "hex"),
];

impl OwnDatabases {
    /// Reads the history of `read`'s table of `application`'s own database
    /// (whose runtime pool is `pool`), labelled `label`, all of it by
    /// `deadline`; see the module's comment.
    pub async fn read_history(
        &self,
        application: &str,
        pool: &executor::Pool,
        read: &HistoryRead,
        label: Label<'_>,
        deadline: Instant,
    ) -> Result<HistoryAnswer, Failure> {
        let time = || Failure::Limit(Limit::Time);
        let shipped = tokio::time::timeout_at(deadline, self.ship_and_wait(application))
            .await
            .map_err(|_| time())?;
        if !shipped {
            return Err(Failure::Unavailable);
        }
        let mut held = tokio::time::timeout_at(deadline, pool.checkout(Level::Read))
            .await
            .map_err(|_| time())??;
        tokio::time::timeout_at(deadline, held.begin(None, label))
            .await
            .map_err(|_| time())??;
        let read_whole = self.read_in(application, &mut held, read, deadline).await;
        // Read-only: nothing to keep. A connection a limit already ended is
        // given up either way.
        held.end(false).await.ok();
        read_whole
    }

    async fn read_in(
        &self,
        application: &str,
        held: &mut Held<'_>,
        read: &HistoryRead,
        deadline: Instant,
    ) -> Result<HistoryAnswer, Failure> {
        // The search path is the runtime role's, as for any `query`: the
        // Application's code this runs (a domain's check, a type's input)
        // finds its own objects by it. Every name these statements use is
        // qualified, and none of them runs with more than the runtime
        // role's rights.
        let table = describe(held, read, deadline).await?;
        let relation = format!(
            "{}.{}",
            quote_identifier(&read.schema),
            quote_identifier(&read.table)
        );
        let which = match &read.key {
            None => Which::All,
            Some(key) => {
                let names_the_key = !table.key.is_empty()
                    && key.len() == table.key.len()
                    && table.key.iter().all(|c| key.contains_key(c));
                if names_the_key {
                    Which::Row(as_captured(held, &relation, &table, key, deadline).await?)
                } else {
                    Which::Structure
                }
            }
        };
        let kept = tokio::time::timeout_at(deadline, self.changes(application, read, &which))
            .await
            .map_err(|_| Failure::Limit(Limit::Time))?
            .map_err(|_| Failure::Unavailable)?
            .ok_or(Failure::Limit(Limit::Bytes))?;
        // The rows of the changes, `old` and `new` in turn, as one JSON
        // array: the text the platform database gave, put together once
        // (copied: it is sent on as one parameter).
        let mut docs = String::with_capacity(
            kept.iter()
                .map(|k| {
                    k.old
                        .as_ref()
                        .map_or(0, String::len)
                        .saturating_add(k.new.as_ref().map_or(0, String::len))
                        .saturating_add(2)
                })
                .sum::<usize>()
                .saturating_add(2),
        );
        docs.push('[');
        for doc in kept.iter().flat_map(|k| [&k.old, &k.new]).flatten() {
            if docs.len() > 1 {
                docs.push(',');
            }
            docs.push_str(doc);
        }
        docs.push(']');
        let shaped = run(
            held,
            &shape_statement(&relation, &table),
            vec![Param::Text(docs)],
            read.values,
            deadline,
        )
        .await?;
        let count = kept.len();
        let answered = answer(application, kept, shaped, read.values).ok_or_else(|| {
            // Never a row's values left out: the read fails whole.
            log::record(
                "history-not-read",
                serde_json::json!({
                    "application": application,
                    "problem": "the own database gave back another number of rows than the changes hold",
                }),
            );
            Failure::Unavailable
        })?;
        Ok(HistoryAnswer::of(answered, count))
    }

    /// The changes from the platform database, newest first; `None` when
    /// what they are of `old` and `new` passes the answer's byte limit.
    async fn changes(
        &self,
        application: &str,
        read: &HistoryRead,
        which: &Which,
    ) -> Result<Option<Vec<Kept>>, DbError> {
        let (sql, key) = match which {
            Which::All => (ALL, None),
            Which::Row(key) => (ROW, Some(key.as_str())),
            Which::Structure => (STRUCTURE, None),
        };
        let rows = self
            .platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        sql,
                        &[
                            &application,
                            &read.schema,
                            &read.table,
                            &read.before,
                            &read.limit,
                            &ANSWER_BYTES,
                            &key,
                        ],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut kept = Vec::with_capacity(rows.len());
        for row in &rows {
            let running: i64 = row.try_get(12).map_err(DbError::from)?;
            if running > ANSWER_BYTES {
                return Ok(None);
            }
            kept.push(Kept {
                id: row.try_get(0).map_err(DbError::from)?,
                at: row.try_get(1).map_err(DbError::from)?,
                kind: row.try_get(2).map_err(DbError::from)?,
                key: row.try_get(3).map_err(DbError::from)?,
                changed: row.try_get(4).map_err(DbError::from)?,
                user_id: row.try_get(5).map_err(DbError::from)?,
                actor: row.try_get(6).map_err(DbError::from)?,
                request: row.try_get(7).map_err(DbError::from)?,
                migration: row.try_get(8).map_err(DbError::from)?,
                structure: row.try_get(9).map_err(DbError::from)?,
                old: row.try_get(10).map_err(DbError::from)?,
                new: row.try_get(11).map_err(DbError::from)?,
            });
        }
        Ok(Some(kept))
    }
}

/// One statement in the read's transaction, cut at `deadline`.
async fn run(
    held: &mut Held<'_>,
    sql: &str,
    params: Vec<Param>,
    values: Values,
    deadline: Instant,
) -> Result<Rows, Failure> {
    held.run(sql, &params, values, deadline).await.result
}

/// The table `read` names as it is now, or `42P01` when there is no table
/// of that name.
async fn describe(
    held: &mut Held<'_>,
    read: &HistoryRead,
    deadline: Instant,
) -> Result<Table, Failure> {
    let rows = run(
        held,
        "SELECT (c.oid IS NOT NULL)::text, a.attname::text,
                CASE a.atttypid WHEN 'pg_catalog.json'::pg_catalog.regtype THEN 'json'
                                WHEN 'pg_catalog.jsonb'::pg_catalog.regtype THEN 'jsonb' END,
                coalesce(a.attnum = ANY (i.indkey::pg_catalog.int2[]), false)::text
           FROM (SELECT pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1::pg_catalog.text,
                                                                 $2::pg_catalog.text)) AS oid) t
           LEFT JOIN pg_catalog.pg_class c ON c.oid = t.oid AND c.relkind IN ('r', 'p')
           LEFT JOIN pg_catalog.pg_attribute a
                  ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
           LEFT JOIN pg_catalog.pg_index i ON i.indrelid = c.oid AND i.indisprimary
          ORDER BY a.attnum",
        // Copied: the parameters are the statement's own.
        vec![
            Param::Text(read.schema.clone()),
            Param::Text(read.table.clone()),
        ],
        Values::Text,
        deadline,
    )
    .await?;
    let text = |row: &Value, i: usize| row.get(i).and_then(Value::as_str).map(str::to_owned);
    let exists = rows
        .rows
        .first()
        .and_then(|row| text(row, 0))
        .is_some_and(|e| e == "true");
    if !exists {
        return Err(Failure::Database {
            code: "42P01".to_owned(),
            message: format!(
                "relation {}.{} does not exist",
                quote_identifier(&read.schema),
                quote_identifier(&read.table)
            ),
        });
    }
    let mut table = Table {
        columns: Vec::with_capacity(rows.rows.len()),
        key: Vec::new(),
    };
    for row in &rows.rows {
        let Some(name) = text(row, 1) else {
            continue;
        };
        let json = match text(row, 2).as_deref() {
            Some("json") => Some(false),
            Some("jsonb") => Some(true),
            _ => None,
        };
        if text(row, 3).is_some_and(|k| k == "true") {
            table.key.push(name.clone());
        }
        table.columns.push((name, json));
    }
    Ok(table)
}

/// The key as the capture recorded it: the given values read into the key
/// columns' types (`jsonb_populate_record` of the table's row type), and
/// turned back into `jsonb` (`jsonb_build_object` of each column, as
/// `to_jsonb` of the row gives it) under the settings the capture pins,
/// which are put back as they were afterwards. A value a column's type
/// refuses fails the read with that SQLSTATE.
async fn as_captured(
    held: &mut Held<'_>,
    relation: &str,
    table: &Table,
    key: &Map<String, Value>,
    deadline: Instant,
) -> Result<String, Failure> {
    let saved = run(
        held,
        &format!(
            "SELECT {}",
            CAPTURE_SETTINGS
                .iter()
                .map(|(name, _)| format!("pg_catalog.current_setting({})", literal(name)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Vec::new(),
        Values::Text,
        deadline,
    )
    .await?;
    let saved: Vec<String> = saved
        .rows
        .first()
        .and_then(Value::as_array)
        .map(|row| {
            row.iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let set = |values: &mut dyn Iterator<Item = (&str, &str)>| {
        format!(
            "SELECT {}",
            values
                .map(|(name, value)| format!(
                    "pg_catalog.set_config({}, {}, true)",
                    literal(name),
                    literal(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let pin = set(&mut CAPTURE_SETTINGS.iter().copied());
    run(held, &pin, Vec::new(), Values::Text, deadline).await?;
    let built = table
        .key
        .iter()
        .map(|c| format!("{}, vm_r.{}", literal(c), quote_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let key_text = Value::Object(key.clone()).to_string();
    let captured = run(
        held,
        &format!(
            "SELECT pg_catalog.jsonb_build_object({built})::pg_catalog.text
               FROM pg_catalog.jsonb_populate_record(NULL::{relation}, $1::pg_catalog.jsonb) AS vm_r"
        ),
        vec![Param::Text(key_text)],
        Values::Text,
        deadline,
    )
    .await?;
    if saved.len() == CAPTURE_SETTINGS.len() {
        let back = set(&mut CAPTURE_SETTINGS
            .iter()
            .zip(&saved)
            .map(|((name, _), value)| (*name, value.as_str())));
        run(held, &back, Vec::new(), Values::Text, deadline).await?;
    }
    captured
        .rows
        .first()
        .and_then(|row| row.get(0))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(Failure::Unavailable)
}

/// The statement that reads `$1`, an array of `{"j": row, "z": [json-null
/// columns]}`, into the table's present row type, one row each, in order.
fn shape_statement(relation: &str, table: &Table) -> String {
    let columns = table
        .columns
        .iter()
        .map(|(name, json)| {
            let column = quote_identifier(name);
            match json {
                None => format!("vm_r.{column}"),
                Some(binary) => format!(
                    "CASE WHEN (vm_u.e OPERATOR(pg_catalog.->) 'z') OPERATOR(pg_catalog.?) {name} \
                     THEN 'null'::pg_catalog.{kind} ELSE vm_r.{column} END AS {column}",
                    name = literal(name),
                    kind = if *binary { "jsonb" } else { "json" },
                ),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT {columns}
           FROM pg_catalog.jsonb_array_elements($1::pg_catalog.jsonb) WITH ORDINALITY AS vm_u(e, n)
          CROSS JOIN LATERAL pg_catalog.jsonb_populate_record(
                NULL::{relation}, vm_u.e OPERATOR(pg_catalog.->) 'j') AS vm_r
          ORDER BY vm_u.n"
    )
}

/// The answer, from the changes kept and their rows shaped (in the order
/// of the changes, `old` before `new`); `None` unless there is exactly one
/// shaped row for each `old` and `new` there is.
fn answer(application: &str, kept: Vec<Kept>, shaped: Rows, values: Values) -> Option<Value> {
    let mut rows = shaped.rows.into_iter();
    let count = kept.len();
    let parse = |text: Option<String>| {
        text.and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .unwrap_or(Value::Null)
    };
    let mut changes = Vec::with_capacity(count);
    for k in kept {
        let old = match k.old {
            Some(_) => rows.next()?,
            None => Value::Null,
        };
        let new = match k.new {
            Some(_) => rows.next()?,
            None => Value::Null,
        };
        let mut change = Map::with_capacity(13);
        change.insert("id".into(), k.id.into());
        change.insert("at".into(), k.at.into());
        change.insert("kind".into(), k.kind.into());
        change.insert("key".into(), parse(k.key));
        change.insert("old".into(), old);
        change.insert("new".into(), new);
        change.insert("changed".into(), parse(k.changed));
        change.insert("user_id".into(), k.user_id.into());
        change.insert("application".into(), application.into());
        change.insert("actor".into(), k.actor.into());
        change.insert("request".into(), k.request.into());
        change.insert("migration".into(), k.migration.into());
        change.insert("structure".into(), parse(k.structure));
        changes.push(Value::Object(change));
    }
    if rows.next().is_some() {
        return None;
    }
    let mut wire = Map::with_capacity(3);
    wire.insert("columns".into(), shaped.columns.into());
    if values == Values::Text {
        wire.insert("types".into(), shaped.types.into());
    }
    wire.insert("changes".into(), Value::Array(changes));
    Some(Value::Object(wire))
}

impl HistoryAnswer {
    fn of(answer: Value, changes: usize) -> Self {
        Self {
            answer,
            changes: u64::try_from(changes).unwrap_or(u64::MAX),
        }
    }
}

/// `text` as a Postgres string literal.
fn literal(text: &str) -> String {
    super::literal(text)
}

/// The newest `limit` changes of the Change history, newest first, of every
/// Application and table, or of the table `table` (`(schema, table)`): each
/// as it is recorded, `old` and `new` the rows by column name, `id` as
/// text. For the local run's `vibe local change-history`, which reads the
/// private platform database whole.
pub async fn recorded(
    platform: &crate::platform_db::PlatformDatabase,
    table: Option<(&str, &str)>,
    limit: u16,
) -> Result<Vec<Value>, DbError> {
    let limit = i64::from(limit);
    let (schema, name) = table.unzip();
    let rows = platform
        .pool()
        .run(async |client: &mut Client| {
            client
                .query(
                    concat!(
                        "SELECT id::text, ",
                        rfc3339!("at"),
                        ", schema_name, table_name, kind, key, old, new, changed, user_id,
                           actor, request, migration, structure
                         FROM change_history
                         WHERE ($1::text IS NULL OR (schema_name = $1 AND table_name = $2))
                         ORDER BY at DESC, id DESC
                         LIMIT $3"
                    ),
                    &[&schema, &name, &limit],
                )
                .await
                .map_err(DbError::from)
        })
        .await?;
    let mut changes = Vec::with_capacity(rows.len());
    for row in &rows {
        let get_text = |at: usize| -> Result<Value, DbError> {
            Ok(row
                .try_get::<_, Option<String>>(at)
                .map_err(DbError::from)?
                .map_or(Value::Null, Value::String))
        };
        let get_json = |at: usize| -> Result<Value, DbError> {
            Ok(row
                .try_get::<_, Option<Value>>(at)
                .map_err(DbError::from)?
                .unwrap_or(Value::Null))
        };
        let mut change = Map::with_capacity(14);
        change.insert("id".into(), get_text(0)?);
        change.insert("at".into(), get_text(1)?);
        change.insert("schema".into(), get_text(2)?);
        change.insert("table".into(), get_text(3)?);
        change.insert("kind".into(), get_text(4)?);
        change.insert("key".into(), get_json(5)?);
        change.insert("old".into(), get_json(6)?);
        change.insert("new".into(), get_json(7)?);
        change.insert("changed".into(), get_json(8)?);
        change.insert("user_id".into(), get_text(9)?);
        change.insert("actor".into(), get_text(10)?);
        change.insert("request".into(), get_text(11)?);
        change.insert("migration".into(), get_text(12)?);
        change.insert("structure".into(), get_json(13)?);
        changes.push(Value::Object(change));
    }
    Ok(changes)
}
