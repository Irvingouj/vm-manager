//! What the tests of "A rewrite records each row it changed" share
//! (CONTRACT.md, "A rewrite records each row it changed"; ticket "A
//! migration that rewrites values records each changed row"):
//!
//! - [`Rewrite`]: a program with [`APP`] installed from
//!   [`history_standard`] and [`rewrite_tables`], every table given history
//!   by a User's writes ([`SEED`]), and later Versions sent with more
//!   migrations;
//! - the row entries of `change_history` a later Version left
//!   ([`row_entries`]) and checks of them ([`assert_row_entries`],
//!   [`assert_between`]);
//! - a refusal's rule, migration and words ([`assert_refused`]).

use serde_json::{Value, json};

use crate::history::{APP, Change, assert_change, history_after, history_standard, history_where};
use crate::history_op::history_call;
use crate::http::{Response, User};
use crate::own_database::{Migration, Own, OwnDatabase, assert_ok, own_execute};
use crate::scenario::lines;

/// The User whose writes give the tables their history.
pub const WRITER: &str = "alice";

/// The name of the migration of the first Version that makes the tables of
/// these tests.
pub const REWRITE_TABLES: &str = "0003_rewrite_tables";

/// The tables `turn_0` to `turn_9`, each with the row `(1, 1)`: a statement
/// that picks one by a sequence's value rewrites another each time it runs.
pub const TURNS: usize = 10;

/// `0003_rewrite_tables`, after [`history_standard`] (which has `keyless (a
/// text, b integer)`, with no primary key):
///
/// - `amounts (id PK, amount integer, note text)`;
/// - `wide (id PK, i integer, v varchar(50), n numeric(10,2), c text)`;
/// - `vals (id PK, s text, ts timestamp, jb jsonb, js json)`;
/// - `keyed (id integer PK, v text)`;
/// - `pk_moved (id PK, code text not null, n integer)`;
/// - `parted (id, day, v)` partitioned by range of `day`, keyed on `(id,
///   day)`: the partition `parted_2026`, and `parted_2027`, itself
///   partitioned by range of `id` with the partition `parted_2027_low`;
/// - `base_t (id PK, v integer)` and `child_t (extra text) INHERITS
///   (base_t)`, keyed on `id` of its own;
/// - `secured (id PK, v integer)` and `other (id PK, v integer)`;
/// - `turn_0` to `turn_9 (id PK, v integer)`;
/// - the enum `mood ('happy', 'sad')` and `moody (id PK, m mood, n
///   integer)`;
/// - the sequence `turns`.
pub fn rewrite_tables() -> Migration {
    let mut statements: Vec<String> = [
        r#"CREATE TABLE "amounts" (
	"id" integer PRIMARY KEY NOT NULL,
	"amount" integer,
	"note" text
);"#,
        r#"CREATE TABLE "wide" (
	"id" integer PRIMARY KEY NOT NULL,
	"i" integer,
	"v" varchar(50),
	"n" numeric(10, 2),
	"c" text
);"#,
        r#"CREATE TABLE "vals" (
	"id" integer PRIMARY KEY NOT NULL,
	"s" text,
	"ts" timestamp,
	"jb" jsonb,
	"js" json
);"#,
        r#"CREATE TABLE "keyed" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" text
);"#,
        r#"CREATE TABLE "pk_moved" (
	"id" integer PRIMARY KEY NOT NULL,
	"code" text NOT NULL,
	"n" integer
);"#,
        r#"CREATE TABLE "parted" (
	"id" integer NOT NULL,
	"day" date NOT NULL,
	"v" integer,
	CONSTRAINT "parted_id_day_pk" PRIMARY KEY("id","day")
) PARTITION BY RANGE ("day");"#,
        r#"CREATE TABLE "parted_2026" PARTITION OF "parted" FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');"#,
        r#"CREATE TABLE "parted_2027" PARTITION OF "parted" FOR VALUES FROM ('2027-01-01') TO ('2028-01-01') PARTITION BY RANGE ("id");"#,
        r#"CREATE TABLE "parted_2027_low" PARTITION OF "parted_2027" FOR VALUES FROM (0) TO (1000);"#,
        r#"CREATE TABLE "base_t" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" integer
);"#,
        r#"CREATE TABLE "child_t" (
	"extra" text
) INHERITS ("base_t");"#,
        r#"ALTER TABLE "child_t" ADD CONSTRAINT "child_t_pk" PRIMARY KEY ("id");"#,
        r#"CREATE TABLE "secured" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" integer
);"#,
        r#"CREATE TABLE "other" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" integer
);"#,
        r#"CREATE TYPE "public"."mood" AS ENUM('happy', 'sad');"#,
        r#"CREATE TABLE "moody" (
	"id" integer PRIMARY KEY NOT NULL,
	"m" "mood",
	"n" integer
);"#,
        r#"CREATE SEQUENCE "turns";"#,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for n in 0..TURNS {
        statements.push(format!(
            "CREATE TABLE \"turn_{n}\" (\n\t\"id\" integer PRIMARY KEY NOT NULL,\n\t\"v\" integer\n);"
        ));
    }
    let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
    Migration::new(REWRITE_TABLES, &refs)
}

/// The writes [`seed`] makes as [`WRITER`], each one `execute`; with the
/// rows of `turn_0` to `turn_9` ([`turn_writes`]) they change
/// [`SEEDED_ROWS`] rows.
pub const SEED: [&str; 14] = [
    "insert into amounts (id, amount, note) values (1, 5, 'five'), (2, 0, 'zero'), (3, null, 'none')",
    "insert into wide (id, i, v, n, c) values (1, 12, 'vee', 1.50, 'c1'), (2, null, null, null, null)",
    r#"insert into vals (id, s, ts, jb, js) values (1, '5', '2026-03-01 10:00:00', '{"a": 1}', '{"a": 1}'), (2, null, null, 'null', 'null'), (3, '7', null, null, null)"#,
    "insert into keyed (id, v) values (1, 'a'), (2, 'b')",
    "insert into pk_moved (id, code, n) values (1, 'p1', 1), (2, 'p2', 2)",
    "insert into parted (id, day, v) values (1, '2026-03-01', 5), (2, '2027-03-01', 7)",
    "insert into base_t (id, v) values (1, 1)",
    "insert into child_t (id, v, extra) values (1, 2, 'x')",
    "insert into secured (id, v) values (1, 1), (2, 2)",
    "insert into other (id, v) values (1, 1)",
    "insert into moody (id, m, n) values (1, 'happy', 1)",
    "insert into keyless (a, b) values ('d', 1), ('d', 1), ('e', 1)",
    "insert into things (title, size) values ('t1', 1)",
    "update amounts set note = 'five!' where id = 1",
];

/// The rows [`SEED`] and [`turn_writes`] change.
pub const SEEDED_ROWS: usize = 3 + 2 + 3 + 2 + 2 + 2 + 1 + 1 + 2 + 1 + 1 + 3 + 1 + 1 + TURNS;

/// One insert of `(1, 1)` into each of `turn_0` to `turn_9`.
pub fn turn_writes() -> Vec<String> {
    (0..TURNS)
        .map(|n| format!("insert into turn_{n} (id, v) values (1, 1)"))
        .collect()
}

/// Gives every table of [`rewrite_tables`] history: the writes of [`SEED`]
/// and [`turn_writes`] as [`WRITER`], acting as the User, each answered
/// `ok`.
pub fn seed(own: &Own) {
    let mut writes: Vec<String> = SEED.iter().map(|s| (*s).to_owned()).collect();
    writes.extend(turn_writes());
    let calls: Vec<Value> = writes.iter().map(|sql| own_execute("user", sql)).collect();
    let (_, answers) = own.run(&User::new(WRITER), APP, &calls);
    for (sql, answer) in writes.iter().zip(&answers) {
        assert_ok(answer, &format!("the harness's own check: seeding {sql:?}"));
    }
}

/// A program with [`APP`] installed and seeded, and the migrations its
/// Versions sent so far.
pub struct Rewrite {
    pub own: Own,
    pub db: OwnDatabase,
    pub sent: Vec<Migration>,
}

impl Rewrite {
    /// A fresh program; [`APP`]'s first Version with [`history_standard`]
    /// and [`rewrite_tables`]; [`seed`]. Fails unless the seed is in
    /// `change_history` (the harness's own check).
    pub fn start() -> Self {
        Self::start_with(&[])
    }

    /// As [`Rewrite::start`], with `more` migrations in the first Version
    /// after [`rewrite_tables`].
    pub fn start_with(more: &[Migration]) -> Self {
        let own = Own::start();
        let mut sent = history_standard();
        sent.push(rewrite_tables());
        sent.extend_from_slice(more);
        let db = own.install(APP, &sent);
        seed(&own);
        let seeded = history_where(&db, "true");
        assert!(
            seeded.len() == SEEDED_ROWS,
            "the harness's own check: the seed's {SEEDED_ROWS} changes are in change_history before a later Version; got {}:\n{}",
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

    /// As [`Rewrite::send`], failing unless `201`.
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

    /// Every row of `public.<table>` as the superuser reads it with
    /// `to_jsonb`, ordered by its text: to show what a table holds now.
    pub fn rows_of(&self, table: &str) -> Vec<String> {
        self.db.column(&format!(
            "SELECT to_jsonb(t)::text FROM ONLY public.{table} t ORDER BY 1"
        ))
    }

    /// The file node of `public.<table>`: Postgres gives a table a new one
    /// when it writes its rows anew.
    pub fn filenode(&self, table: &str) -> Option<String> {
        self.db.superuser().value(&format!(
            "SELECT pg_relation_filenode('public.{table}')::text"
        ))
    }
}

/// The entries of `db`'s Application in `change_history` with an `id`
/// greater than `after` that are not `structure` entries, oldest first,
/// once shipped.
pub fn row_entries(db: &OwnDatabase, after: i64) -> Vec<Value> {
    history_after(db, after)
        .into_iter()
        .filter(|r| r["kind"] != "structure")
        .collect()
}

/// The `structure` entries of `db`'s Application with an `id` greater than
/// `after`, oldest first, once shipped.
pub fn structure_after(db: &OwnDatabase, after: i64) -> Vec<Value> {
    history_after(db, after)
        .into_iter()
        .filter(|r| r["kind"] == "structure")
        .collect()
}

/// Fails unless `rows` are exactly one entry per `want`, in any order (the
/// order of the entries of one rewrite is not given by CONTRACT.md), each
/// found by its table, `kind` and `key` and then compared whole with
/// [`assert_change`].
pub fn assert_row_entries(rows: &[Value], wants: &[Change<'_>], context: &str) {
    let describe = |w: &Change<'_>| format!("{} {} {} key {}", w.kind, w.schema, w.table, w.key);
    assert_eq!(
        rows.len(),
        wants.len(),
        "{context}: expected exactly {} entries ({}), got {}:\n{}",
        wants.len(),
        wants.iter().map(describe).collect::<Vec<_>>().join("; "),
        rows.len(),
        lines(rows)
    );
    for want in wants {
        let found: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                r["table_name"] == want.table
                    && r["schema_name"] == want.schema
                    && r["kind"] == want.kind
                    && r["key"] == want.key
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{context}: expected one entry {}, got:\n{}",
            describe(want),
            lines(rows)
        );
        if let Some(row) = found.first() {
            assert_change(row, want, &format!("{context}: {}", describe(want)));
        }
    }
}

/// Fails unless every one of `rows` has an `id` greater than `low` (when
/// given) and less than every `id` of `high`.
pub fn assert_between(rows: &[Value], low: Option<i64>, high: &[Value], context: &str) {
    let high_ids: Vec<i64> = high.iter().filter_map(|r| r["id"].as_i64()).collect();
    for row in rows {
        let id = row["id"].as_i64().unwrap_or(i64::MIN);
        if let Some(low) = low {
            assert!(
                id > low,
                "{context}: expected the entry's id greater than {low}, the last row the migration changed before the statement: {row}"
            );
        }
        assert!(
            high_ids.iter().all(|h| id < *h),
            "{context}: expected the entry's id less than those of the migration's structure entries {high_ids:?}: {row}"
        );
    }
}

/// Fails unless `sent` is `422 migration-refused` with `rule`, naming
/// `migration`, with a `message` that contains each of `words` (compared
/// without case).
pub fn assert_refused(sent: &Response, rule: &str, migration: &str, words: &[&str], context: &str) {
    let body = sent.json();
    let error = &body["error"];
    assert!(
        sent.status == 422
            && error["reason"] == "migration-refused"
            && error["rule"] == rule
            && error["migration"] == migration,
        "{context}: expected 422 migration-refused, rule {rule:?}, migration {migration:?}; got {}:\n{}",
        sent.status,
        sent.describe()
    );
    let message = error["message"].as_str().unwrap_or_default().to_lowercase();
    let missing: Vec<&&str> = words
        .iter()
        .filter(|w| !message.contains(&w.to_lowercase()))
        .collect();
    assert!(
        missing.is_empty(),
        "{context}: expected the message to mention {missing:?}, got {error}"
    );
}

/// An update by `migration` of a Version [`crate::vm::INSTALLER`] sent, of
/// `public.<table>`.
pub fn migration_update<'a>(
    migration: &'a str,
    table: &'a str,
    key: Value,
    old: Value,
    new: Value,
    changed: Value,
) -> Change<'a> {
    Change::by_migration(crate::vm::INSTALLER, migration)
        .on("public", table)
        .update(key, old, new, changed)
}

/// The `{ "id": n }` key.
pub fn id_key(id: i64) -> Value {
    json!({ "id": id })
}
