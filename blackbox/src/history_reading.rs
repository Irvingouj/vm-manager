//! What the tests of the operation `history` share (CONTRACT.md, "The
//! Change history in the platform database: Reading: the operation
//! `history`"; ticket "The Change history lives in the platform database,
//! shipped from an outbox"):
//!
//! - the migrations the tests send beside [`history_standard`]:
//!   [`reading_tables`] (keys of `numeric`, `timestamptz` and `bigint`, a
//!   row of many types, `json` and `jsonb`, wide rows, a domain whose check
//!   tells who runs it and whether it may write, tables a later migration
//!   reshapes, retypes or drops, and a function that updates one row many
//!   times) and [`reshape`], the later migration;
//! - an Application installed with them ([`reader`]), writes that must run
//!   ([`write`]), and reads of the history through `operations.js`
//!   ([`read`], [`read_ok`]);
//! - what every change of an answer must carry ([`CHANGE_FIELDS`],
//!   [`assert_row_change`]) and their order ([`assert_newest_first`]);
//! - the one Operation log entry of a read ([`the_entry`],
//!   [`assert_history_entry`]);
//! - a lock on `change_history` held by the Postgres administrator in the
//!   platform database ([`hold_change_history`]).

use serde_json::{Value, json};

use crate::api::parse_rfc3339;
use crate::history::{APP, history_standard, platform_sql};
use crate::history_op::history_call;
use crate::http::{Response, User};
use crate::own_database::{Migration, Own, OwnDatabase, own_execute};
use crate::scenario::{Expected, assert_entry, lines};
use crate::sql::Sql;

/// The name of [`reading_tables`].
pub const READING: &str = "0003_reading_tables";

/// The name of [`reshape`].
pub const RESHAPE: &str = "0004_reshape";

/// The User the tests write and read as, as the User.
pub fn alice() -> User {
    User::new("alice")
}

/// The User in flight when the tests act as the Application.
pub fn bob() -> User {
    User::new("bob")
}

/// alice as the User, bob in flight when acting as the Application.
pub fn user_for(actor: &str) -> User {
    if actor == "user" { alice() } else { bob() }
}

/// `0003_reading_tables`:
///
/// - `nums (k numeric PK, note)`, `stamps (t timestamptz PK, note)`,
///   `bigs (id bigint PK, note)`: keys compared by their type;
/// - `typed (id bigint PK, n numeric(10,2), t timestamptz, d date, flag
///   boolean, r real, arr text[], j json, u uuid, s smallint)`: values as
///   `query` gives them;
/// - `docs (id integer PK, jb jsonb, js json)`: JSON `null` and `json`;
/// - `big (id integer PK, body text)`: answers over 8 MiB;
/// - the domain `guarded` over text, whose check `guard_check(v)` raises
///   `guard_check ran as <current_user>` for `'boom'` in a read-only
///   transaction, and for `'write'` inserts a row into `side_effects`
///   every time it runs; `guarded_rows (id integer PK, v guarded)`;
/// - `shapes (id integer PK, a, b, c integer, d text)`, `retyped (id
///   integer PK, v text)` and `gone (id integer PK, v text)`, which
///   [`reshape`] changes;
/// - `bump(k, n)`: updates `things` row `k` `n` times, one statement each.
pub fn reading_tables() -> Migration {
    Migration::new(
        READING,
        &[
            r#"CREATE TABLE "nums" (
	"k" numeric PRIMARY KEY NOT NULL,
	"note" text
);"#,
            r#"CREATE TABLE "stamps" (
	"t" timestamp with time zone PRIMARY KEY NOT NULL,
	"note" text
);"#,
            r#"CREATE TABLE "bigs" (
	"id" bigint PRIMARY KEY NOT NULL,
	"note" text
);"#,
            r#"CREATE TABLE "typed" (
	"id" bigint PRIMARY KEY NOT NULL,
	"n" numeric(10, 2),
	"t" timestamp with time zone,
	"d" date,
	"flag" boolean,
	"r" real,
	"arr" text[],
	"j" json,
	"u" uuid,
	"s" smallint
);"#,
            r#"CREATE TABLE "docs" (
	"id" integer PRIMARY KEY NOT NULL,
	"jb" jsonb,
	"js" json
);"#,
            r#"CREATE TABLE "big" (
	"id" integer PRIMARY KEY NOT NULL,
	"body" text NOT NULL
);"#,
            r#"CREATE TABLE "side_effects" (
	"note" text
);"#,
            r#"CREATE FUNCTION "guard_check"(v text) RETURNS boolean LANGUAGE plpgsql AS $f$
BEGIN
  IF v = 'boom' AND current_setting('transaction_read_only') = 'on' THEN
    RAISE EXCEPTION 'guard_check ran as %', current_user;
  END IF;
  IF v = 'write' THEN
    INSERT INTO "side_effects" ("note") VALUES ('written by guard_check as ' || current_user);
  END IF;
  RETURN true;
END
$f$;"#,
            r#"CREATE DOMAIN "guarded" AS text CHECK ("guard_check"(VALUE));"#,
            r#"CREATE TABLE "guarded_rows" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" "guarded"
);"#,
            r#"CREATE TABLE "shapes" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" text,
	"b" text,
	"c" integer,
	"d" text
);"#,
            r#"CREATE TABLE "retyped" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" text
);"#,
            r#"CREATE TABLE "gone" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" text
);"#,
            r#"CREATE FUNCTION "bump"(k integer, n integer) RETURNS integer LANGUAGE plpgsql AS $f$
BEGIN
  FOR i IN 1..n LOOP
    UPDATE "things" SET "size" = coalesce("size", 0) + 1 WHERE "id" = k;
  END LOOP;
  RETURN n;
END
$f$;"#,
        ],
    )
}

/// `0004_reshape`, a later Version's migration: `shapes` gains `e`, its
/// `b` is renamed `bb`, its `a` dropped and its `d` retyped to integer;
/// `retyped.v` becomes integer (values that are not digits become 0);
/// `gone` is dropped.
pub fn reshape() -> Migration {
    Migration::new(
        RESHAPE,
        &[
            r#"ALTER TABLE "shapes" ADD COLUMN "e" text;"#,
            r#"ALTER TABLE "shapes" RENAME COLUMN "b" TO "bb";"#,
            r#"ALTER TABLE "shapes" DROP COLUMN "a";"#,
            r#"ALTER TABLE "shapes" ALTER COLUMN "d" SET DATA TYPE integer USING "d"::integer;"#,
            r#"ALTER TABLE "retyped" ALTER COLUMN "v" SET DATA TYPE integer USING (CASE WHEN "v" ~ '^[0-9]+$' THEN "v"::integer ELSE 0 END);"#,
            r#"DROP TABLE "gone";"#,
        ],
    )
}

/// [`history_standard`] and [`reading_tables`]: the first Version's list.
pub fn reading_migrations() -> Vec<Migration> {
    let mut all = history_standard();
    all.push(reading_tables());
    all
}

/// A program and [`APP`] installed with [`reading_migrations`].
pub fn reader() -> (Own, OwnDatabase) {
    let own = Own::start();
    let db = own.install(APP, &reading_migrations());
    (own, db)
}

/// One `execute` on `application`'s own database as `actor` for `user`,
/// failing unless it ran (the harness's own check); returns the request's
/// id.
pub fn write(own: &Own, application: &str, user: &User, actor: &str, sql: &str) -> String {
    let (response, answer) = own.one(user, application, &own_execute(actor, sql));
    assert!(
        answer["ok"].is_object(),
        "the harness's own check: expected `{sql}` as the {actor} in {application:?} to run, got {answer}"
    );
    response.request_id()
}

/// One call (a [`history_call`]) in `application` for `user`.
pub fn read(own: &Own, application: &str, user: &User, call: &Value) -> (Response, Value) {
    own.one(user, application, call)
}

/// As [`read`], failing unless it was answered `{ ok: { columns, changes }
/// }`; returns the request's answer, the changes and the whole `ok`.
pub fn read_ok(
    own: &Own,
    application: &str,
    user: &User,
    call: &Value,
) -> (Response, Vec<Value>, Value) {
    let (response, answer) = read(own, application, user, call);
    let changes = ok_changes(&answer, &format!("{call} in {application:?}"));
    (response, changes, answer["ok"].clone())
}

/// A [`history_call`] of `public`.`table` as `actor` with `extra`.
pub fn public(actor: &str, table: &str, extra: &Value) -> Value {
    history_call(actor, "public", table, extra)
}

/// The changes of an answer `{ ok: { columns, changes } }`; fails
/// otherwise.
pub fn ok_changes(answer: &Value, context: &str) -> Vec<Value> {
    assert!(
        answer["ok"]["columns"].is_array() && answer["ok"]["changes"].is_array(),
        "{context}: expected `history` answered {{ ok: {{ columns, changes }} }}, got {answer}"
    );
    answer["ok"]["changes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The properties of a change, exactly (CONTRACT.md, "Reading: the
/// operation `history`": the change's JSON).
pub const CHANGE_FIELDS: [&str; 13] = [
    "id",
    "at",
    "kind",
    "key",
    "old",
    "new",
    "changed",
    "user_id",
    "application",
    "actor",
    "request",
    "migration",
    "structure",
];

/// The change's `id` as a number; fails unless it is a string of decimal
/// digits.
pub fn id_of(change: &Value) -> u128 {
    let text = change["id"].as_str().unwrap_or_default();
    assert!(
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        "expected a change's `id` to be a string of decimal digits, got {change}"
    );
    text.parse()
        .unwrap_or_else(|e| panic!("a change's `id` {text:?} is not a number: {e}"))
}

/// The `id` of a change as the text to send as `before`.
pub fn id_text(change: &Value) -> String {
    id_of(change).to_string()
}

/// Fails unless the ids strictly decrease: newest first.
pub fn assert_newest_first(changes: &[Value], context: &str) {
    let ids: Vec<u128> = changes.iter().map(id_of).collect();
    assert!(
        ids.windows(2).all(|w| w.first() > w.get(1)),
        "{context}: expected the changes newest first (ids decreasing), got the ids {ids:?}"
    );
}

/// Fails unless `change` has exactly [`CHANGE_FIELDS`], an `id` of
/// digits, and an `at` that is RFC 3339 in UTC within two minutes of now.
pub fn assert_change_fields(change: &Value, context: &str) {
    let mut have: Vec<&str> = change
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    have.sort_unstable();
    let mut want = CHANGE_FIELDS.to_vec();
    want.sort_unstable();
    assert_eq!(
        have, want,
        "{context}: expected a change to have exactly the properties {CHANGE_FIELDS:?}, got {change}"
    );
    id_of(change);
    let at = change["at"].as_str().unwrap_or_default();
    assert!(
        parse_rfc3339(at).is_some() && (at.ends_with('Z') || at.ends_with("+00:00")),
        "{context}: expected `at` as RFC 3339 in UTC, got {change}"
    );
    crate::api::assert_recent_rfc3339(&change["at"], 120, &format!("{context}, `at`"));
}

/// What a change of a row must say.
#[derive(Clone, Debug)]
pub struct RowChange<'a> {
    pub kind: &'a str,
    pub key: Value,
    pub old: Value,
    pub new: Value,
    pub changed: Value,
    pub user: &'a str,
    pub application: &'a str,
    pub actor: &'a str,
    /// The request's id; `None` for a migration's change.
    pub request: Option<&'a str>,
}

impl<'a> RowChange<'a> {
    /// An insert made in [`APP`] by `user` as `actor` in `request`.
    pub fn insert(user: &'a str, actor: &'a str, request: &'a str, key: Value, new: Value) -> Self {
        Self {
            kind: "insert",
            key,
            old: Value::Null,
            new,
            changed: Value::Null,
            user,
            application: APP,
            actor,
            request: Some(request),
        }
    }

    /// An update.
    pub fn update(
        user: &'a str,
        actor: &'a str,
        request: &'a str,
        key: Value,
        old: Value,
        new: Value,
        changed: Value,
    ) -> Self {
        Self {
            kind: "update",
            old,
            changed,
            ..Self::insert(user, actor, request, key, new)
        }
    }

    /// A delete.
    pub fn delete(user: &'a str, actor: &'a str, request: &'a str, key: Value, old: Value) -> Self {
        Self {
            kind: "delete",
            old,
            new: Value::Null,
            ..Self::insert(user, actor, request, key, Value::Null)
        }
    }
}

/// Fails unless `change` is the change of a row `want` describes, with
/// every property of the contract (and `migration` and `structure` null).
pub fn assert_row_change(change: &Value, want: &RowChange<'_>, context: &str) {
    assert_change_fields(change, context);
    let expected = json!({
        "kind": want.kind,
        "key": want.key,
        "old": want.old,
        "new": want.new,
        "changed": want.changed,
        "user_id": want.user,
        "application": want.application,
        "actor": want.actor,
        "request": want.request,
        "migration": null,
        "structure": null,
    });
    let mut got = change.clone();
    if let Some(o) = got.as_object_mut() {
        o.remove("id");
        o.remove("at");
    }
    assert_eq!(
        got, expected,
        "{context}: expected the change {expected}, got {change}"
    );
}

/// `history "<schema>"."<table>"`: the `statement` of a read's entry.
pub fn statement_of(schema: &str, table: &str) -> String {
    format!("history \"{schema}\".\"{table}\"")
}

/// The one Operation log entry of the request `response` answers; fails
/// unless there is exactly one.
pub fn the_entry(own: &Own, response: &Response) -> Value {
    let entries = own.entries_of(response);
    match entries.as_slice() {
        [entry] => entry.clone(),
        _ => panic!(
            "expected exactly one Operation log entry for request {}, got {}:\n{}",
            response.request_id(),
            entries.len(),
            lines(&entries)
        ),
    }
}

/// What the entry of a read must say beyond its request.
#[derive(Clone, Debug)]
pub struct ReadEntry<'a> {
    pub actor: &'a str,
    pub statement: String,
    /// `allowed` or `failed`.
    pub outcome: &'a str,
    /// The number of changes answered, for an allowed read.
    pub rows: Option<u64>,
    /// The SQLSTATE, the limit's name or `unavailable`, for a failed read.
    pub error: Option<&'a str>,
    pub version: u64,
}

impl<'a> ReadEntry<'a> {
    /// An allowed read of `schema`.`table` as `actor` that answered `rows`
    /// changes, under Version 1.
    pub fn allowed(actor: &'a str, schema: &str, table: &str, rows: u64) -> Self {
        Self {
            actor,
            statement: statement_of(schema, table),
            outcome: "allowed",
            rows: Some(rows),
            error: None,
            version: 1,
        }
    }

    /// A failed read, with this `error`.
    pub fn failed(actor: &'a str, schema: &str, table: &str, error: &'a str) -> Self {
        Self {
            outcome: "failed",
            rows: None,
            error: Some(error),
            ..Self::allowed(actor, schema, table, 0)
        }
    }
}

/// Fails unless `entry` is the entry of a read in `application` by `user`
/// in `request`: `operation` `history`, `own_database` true, `resource`
/// null, `transaction` null, and what `want` says.
pub fn assert_history_entry(
    entry: &Value,
    request: &str,
    user: &str,
    application: &str,
    want: &ReadEntry<'_>,
    context: &str,
) {
    let base = Expected::own(request, user, application, want.actor, "history");
    let expected = if want.outcome == "failed" {
        Expected {
            outcome: "failed",
            error: want.error,
            ran: None,
            ..base
        }
    } else {
        Expected {
            rows: want.rows,
            ..base
        }
    };
    let expected = Expected {
        statement: Some(&want.statement),
        version: want.version,
        ..expected
    };
    assert_entry(entry, &expected, context);
    assert!(
        entry.get("transaction").is_some_and(Value::is_null),
        "{context}: expected `transaction` null (a read of the history is never in a transaction), got {entry}"
    );
    if want.outcome == "failed" {
        assert!(
            entry["rows"].is_null(),
            "{context}: expected `rows` null for a read that did not run to its end, got {entry}"
        );
    }
}

/// The Postgres administrator's connection to the platform database of
/// `db`'s program, holding `LOCK TABLE change_history IN ACCESS EXCLUSIVE
/// MODE` in an open transaction until `COMMIT` is sent on it (or it is
/// dropped).
pub fn hold_change_history(db: &OwnDatabase) -> Sql {
    let sql = platform_sql(db);
    sql.batch("BEGIN; LOCK TABLE change_history IN ACCESS EXCLUSIVE MODE");
    sql
}

/// The values of column `index` of each change's `new` (or `old` when
/// `new` is null), in the answer's order.
pub fn column_values(changes: &[Value], index: usize) -> Vec<Value> {
    changes
        .iter()
        .map(|c| {
            let row = if c["new"].is_array() {
                &c["new"]
            } else {
                &c["old"]
            };
            row.get(index).cloned().unwrap_or(Value::Null)
        })
        .collect()
}
