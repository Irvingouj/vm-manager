//! What the tests of "Changes of structure that change data" share
//! (CONTRACT.md, "The Change history in the platform database: Changes of
//! structure that change data"; ticket "The Change history lives in the
//! platform database, shipped from an outbox"):
//!
//! - [`Structure`]: a program with [`APP`] installed from
//!   [`history_standard`] and [`structure_tables`], every table given
//!   history by a User's writes ([`seed`]), and later Versions sent with
//!   more migrations;
//! - the `structure` entries of `change_history` ([`structure_entries`])
//!   and checks of one ([`assert_structure_row`], [`assert_structure_rows`])
//!   and of one change a `history` read answered
//!   ([`assert_structure_change`]);
//! - a later Version of the SDK fixture `chronicle` ([`send_chronicle`]).

use std::time::Duration;

use serde_json::{Value, json};

use crate::api::SOME_GIT_COMMIT;
use crate::history::{APP, history_standard, history_where};
use crate::history_op::history_call;
use crate::http::{Response, User};
use crate::own_database::{Migration, Own, OwnDatabase, assert_ok, own_execute};
use crate::scenario::lines;
use crate::sdk::{Sdk, built};
use crate::vm::INSTALLER;

/// The User whose writes give the tables their history.
pub const WRITER: &str = "alice";

/// The name of the migration of the first Version that makes the tables of
/// these tests.
pub const STRUCTURE_TABLES: &str = "0003_structure_tables";

/// `0003_structure_tables`, after [`history_standard`]:
///
/// - `priced (id PK, price numeric(10,2), code text)`;
/// - `gone_table (id PK, note)`, to be dropped;
/// - `swapped (id PK, label)` and `swapped_away (id PK, label)`, to be
///   replaced under their names;
/// - the enum `public.mood ('happy', 'sad')`, and `moods (id PK, mood
///   mood)`, `mood_lists (id PK, moods mood[])`, `two_moods (id PK, a mood,
///   note text, b mood[])`, `no_moods (id PK, t text)`;
/// - `measures (id, day, v)` partitioned by range of `day`, keyed on `(id,
///   day)`, with the partition `measures_2026`, and `measures_2027`, a table
///   of the same columns that is no partition;
/// - `plain (id PK, a text, b integer)` and `renamed_t (id PK, a text)`,
///   for changes of structure that change no data.
pub fn structure_tables() -> Migration {
    Migration::new(
        STRUCTURE_TABLES,
        &[
            r#"CREATE TABLE "priced" (
	"id" integer PRIMARY KEY NOT NULL,
	"price" numeric(10, 2),
	"code" text
);"#,
            r#"CREATE TABLE "gone_table" (
	"id" integer PRIMARY KEY NOT NULL,
	"note" text
);"#,
            r#"CREATE TABLE "swapped" (
	"id" integer PRIMARY KEY NOT NULL,
	"label" text
);"#,
            r#"CREATE TABLE "swapped_away" (
	"id" integer PRIMARY KEY NOT NULL,
	"label" text
);"#,
            r#"CREATE TYPE "public"."mood" AS ENUM('happy', 'sad');"#,
            r#"CREATE TABLE "moods" (
	"id" integer PRIMARY KEY NOT NULL,
	"mood" "mood"
);"#,
            r#"CREATE TABLE "mood_lists" (
	"id" integer PRIMARY KEY NOT NULL,
	"moods" "mood"[]
);"#,
            r#"CREATE TABLE "two_moods" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" "mood",
	"note" text,
	"b" "mood"[]
);"#,
            r#"CREATE TABLE "no_moods" (
	"id" integer PRIMARY KEY NOT NULL,
	"t" text
);"#,
            r#"CREATE TABLE "measures" (
	"id" integer NOT NULL,
	"day" date NOT NULL,
	"v" integer,
	CONSTRAINT "measures_id_day_pk" PRIMARY KEY("id","day")
) PARTITION BY RANGE ("day");"#,
            r#"CREATE TABLE "measures_2026" PARTITION OF "measures" FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');"#,
            r#"CREATE TABLE "measures_2027" (
	"id" integer NOT NULL,
	"day" date NOT NULL,
	"v" integer,
	CONSTRAINT "measures_2027_id_day_pk" PRIMARY KEY("id","day")
);"#,
            r#"CREATE TABLE "plain" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" text,
	"b" integer
);"#,
            r#"CREATE TABLE "renamed_t" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" text
);"#,
        ],
    )
}

/// The writes [`seed`] makes as [`WRITER`], each one `execute`.
pub const SEED: [&str; 19] = [
    "insert into things (title, size, tags) values ('one', 1, '[\"a\"]')",
    "insert into things (title, size, tags) values ('two', 2, '[\"b\"]')",
    "insert into things (title, size, tags) values ('three', 3, null)",
    "update things set size = 20 where id = 2",
    "insert into priced (id, price, code) values (1, 1.50, '10'), (2, 2.25, '20')",
    "insert into gone_table (id, note) values (1, 'a'), (2, 'b')",
    "insert into swapped (id, label) values (1, 's1')",
    "insert into swapped_away (id, label) values (1, 'a1')",
    "insert into moods (id, mood) values (1, 'happy'), (2, 'sad')",
    "insert into mood_lists (id, moods) values (1, '{sad,happy}')",
    "insert into two_moods (id, a, note, b) values (1, 'sad', 'n', '{happy}')",
    "insert into no_moods (id, t) values (1, 't')",
    "insert into measures (id, day, v) values (1, '2026-03-01', 5)",
    "insert into measures_2027 (id, day, v) values (1, '2027-03-01', 7)",
    "insert into plain (id, a, b) values (1, 'a', 1)",
    "update plain set b = 2 where id = 1",
    "insert into renamed_t (id, a) values (1, 'r')",
    "update renamed_t set a = 'r2' where id = 1",
    "update moods set mood = 'happy' where id = 2",
];

/// Gives every table of [`structure_tables`] (and `things`) history: the
/// writes of [`SEED`] as [`WRITER`], acting as the User, each answered
/// `ok`.
pub fn seed(own: &Own) {
    let calls: Vec<Value> = SEED.iter().map(|sql| own_execute("user", sql)).collect();
    let (_, answers) = own.run(&User::new(WRITER), APP, &calls);
    for (sql, answer) in SEED.iter().zip(&answers) {
        assert_ok(answer, &format!("the harness's own check: seeding {sql:?}"));
    }
}

/// A program with [`APP`] installed and seeded, and the migrations its
/// Versions sent so far.
pub struct Structure {
    pub own: Own,
    pub db: OwnDatabase,
    pub sent: Vec<Migration>,
}

impl Structure {
    /// A fresh program; [`APP`]'s first Version with [`history_standard`]
    /// and [`structure_tables`]; [`seed`]. Fails unless the seed is in
    /// `change_history` (the harness's own check).
    pub fn start() -> Self {
        let own = Own::start();
        let mut sent = history_standard();
        sent.push(structure_tables());
        let db = own.install(APP, &sent);
        seed(&own);
        let seeded = history_where(&db, "true");
        assert!(
            seeded.len() >= 20,
            "the harness's own check: the seed's changes are in change_history before a later Version; got {}:\n{}",
            seeded.len(),
            lines(&seeded)
        );
        Self { own, db, sent }
    }

    /// Sends a later Version of [`APP`]: every migration sent so far, then
    /// `more`. When it is accepted (`201`), `more` is counted as sent.
    pub fn send(&mut self, more: &[Migration]) -> Response {
        let mut list = self.sent.clone();
        list.extend_from_slice(more);
        let response = self.own.send(APP, &list);
        if response.status == 201 {
            self.sent = list;
        }
        response
    }

    /// As [`Structure::send`], failing unless `201`.
    pub fn send_ok(&mut self, more: &[Migration]) {
        let response = self.send(more);
        if response.status != 201 {
            self.own.vm.fail(&format!(
                "expected `201` for a later Version with {:?}, got\n{}",
                more.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                response.describe()
            ));
        }
    }

    /// The operation `history` of `public.<table>` as [`WRITER`], acting as
    /// `actor`, with `extra` (`key`, `limit`, ...); what the host answered.
    pub fn read(&self, actor: &str, table: &str, extra: &Value) -> Value {
        let (_, answer) = self.own.one(
            &User::new(WRITER),
            APP,
            &history_call(actor, "public", table, extra),
        );
        answer
    }
}

/// The `structure` entries of `db`'s Application in `change_history` with
/// an `id` greater than `after`, oldest first, once shipped.
pub fn structure_entries(db: &OwnDatabase, after: i64) -> Vec<Value> {
    history_where(db, &format!("h.id > {after} AND h.kind = 'structure'"))
}

/// Fails unless `row` of `change_history` is a `structure` entry of
/// `public.<table>` of [`APP`] made by `migration` of a Version
/// [`INSTALLER`] sent, saying `structure`: `actor` `migration`, `request`,
/// `key`, `old`, `new` and `changed` null, an `id` and an `at`.
pub fn assert_structure_row(
    row: &Value,
    table: &str,
    migration: &str,
    structure: &Value,
    context: &str,
) {
    let mut wrong = Vec::new();
    let mut check = |column: &str, expected: Value| {
        if row.get(column) != Some(&expected) {
            wrong.push(format!(
                "  `{column}`: expected {expected}, got {}",
                row.get(column)
                    .map_or("(no such column)".to_owned(), Value::to_string)
            ));
        }
    };
    check("application", json!(APP));
    check("schema_name", json!("public"));
    check("table_name", json!(table));
    check("kind", json!("structure"));
    check("key", Value::Null);
    check("old", Value::Null);
    check("new", Value::Null);
    check("changed", Value::Null);
    check("user_id", json!(INSTALLER));
    check("actor", json!("migration"));
    check("request", Value::Null);
    check("migration", json!(migration));
    check("structure", structure.clone());
    if !row["id"].is_i64() {
        wrong.push(format!("  `id`: expected a number, got {}", row["id"]));
    }
    if !row["at"].is_string() {
        wrong.push(format!("  `at`: expected a time, got {}", row["at"]));
    }
    assert!(
        wrong.is_empty(),
        "{context}: the structure entry is not the one expected:\n{}\nthe row: {row}",
        wrong.join("\n")
    );
}

/// Fails unless `rows` are exactly one `structure` entry per `(table,
/// structure)` of `wants`, in any order, each as [`assert_structure_row`]
/// says, all made by `migration`. The order among the entries of one
/// migration is not given by CONTRACT.md.
pub fn assert_structure_rows(
    rows: &[Value],
    wants: &[(&str, Value)],
    migration: &str,
    context: &str,
) {
    assert_eq!(
        rows.len(),
        wants.len(),
        "{context}: expected exactly {} structure entries ({}), got {}:\n{}",
        wants.len(),
        wants
            .iter()
            .map(|(t, s)| format!("{t}: {s}"))
            .collect::<Vec<_>>()
            .join("; "),
        rows.len(),
        lines(rows)
    );
    for (table, structure) in wants {
        let found: Vec<&Value> = rows
            .iter()
            .filter(|r| r["table_name"] == *table && r["structure"] == *structure)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{context}: expected one structure entry of public.{table} saying {structure}, got:\n{}",
            lines(rows)
        );
        assert_structure_row(found[0], table, migration, structure, context);
    }
}

/// Fails unless `change`, one change of a `history` answer, is the
/// `structure` entry of [`APP`] made by `migration` of a Version
/// [`INSTALLER`] sent, saying `structure` (CONTRACT.md, "Reading: the
/// operation `history`": `key`, `old`, `new`, `changed` null).
pub fn assert_structure_change(change: &Value, migration: &str, structure: &Value, context: &str) {
    let mut wrong = Vec::new();
    let mut check = |property: &str, expected: Value| {
        if change.get(property) != Some(&expected) {
            wrong.push(format!(
                "  `{property}`: expected {expected}, got {}",
                change
                    .get(property)
                    .map_or("(missing)".to_owned(), Value::to_string)
            ));
        }
    };
    check("kind", json!("structure"));
    check("key", Value::Null);
    check("old", Value::Null);
    check("new", Value::Null);
    check("changed", Value::Null);
    check("user_id", json!(INSTALLER));
    check("application", json!(APP));
    check("actor", json!("migration"));
    check("request", Value::Null);
    check("migration", json!(migration));
    check("structure", structure.clone());
    if !change["id"]
        .as_str()
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        wrong.push(format!(
            "  `id`: expected a string of decimal digits, got {}",
            change["id"]
        ));
    }
    if !change["at"].is_string() {
        wrong.push(format!("  `at`: expected a time, got {}", change["at"]));
    }
    assert!(
        wrong.is_empty(),
        "{context}: the change is not the structure entry expected:\n{}\nthe change: {change}",
        wrong.join("\n")
    );
}

/// The ids of the changes of a `history` answer, as numbers, in the order
/// answered.
pub fn answered_ids(changes: &[Value]) -> Vec<i64> {
    changes
        .iter()
        .map(|c| {
            c["id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .unwrap_or_else(|| {
                    panic!(
                        "expected a change's `id` as a string of decimal digits, got {}",
                        c["id"]
                    )
                })
        })
        .collect()
}

/// The ids of these rows of `change_history`, newest first.
pub fn newest_first(rows: &[Value]) -> Vec<i64> {
    let mut ids: Vec<i64> = rows.iter().filter_map(|r| r["id"].as_i64()).collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids
}

/// Sends a later Version of the `chronicle` fixture installed in `sdk`:
/// its bundle and declared access as built, and its migrations followed by
/// `more`. Fails unless `201`.
pub fn send_chronicle(sdk: &Sdk, more: &[Migration]) {
    let chronicle = built("chronicle");
    let mut migrations = chronicle.migrations.clone();
    migrations.extend_from_slice(more);
    let body = json!({
        "bundle": chronicle.bundle,
        "declared_access": chronicle.declared_access,
        "git_commit": SOME_GIT_COMMIT,
        "migrations": crate::own_database::migrations_json(&migrations),
    });
    let sent = sdk
        .vm()
        .api_as(&User::new(INSTALLER))
        .request(&format!("/v1/applications/{}/versions", chronicle.name))
        .method("POST")
        .json(&body)
        .timeout(Duration::from_secs(60))
        .send();
    if sent.status != 201 {
        sdk.vm().fail(&format!(
            "expected `201` for a later Version of {:?} with {:?}, got\n{}",
            chronicle.name,
            more.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            sent.describe()
        ));
    }
}
