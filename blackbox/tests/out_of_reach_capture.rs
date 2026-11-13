//! The capture itself (CONTRACT.md, "The platform's part of an own database
//! is out of a migration's reach: The capture itself"; ticket "The Change
//! history is kept out of a Contributor's reach", seventh acceptance
//! criterion: "A table with a column named `t` is recorded whole; a
//! writer's `extra_float_digits` or `TimeZone` does not change what is
//! recorded").
//!
//! - A row of a table whose columns are named `t`, `vm_row`, `vm_new` and
//!   `vm_old` (text, a composite type, jsonb, timestamptz), and of a table
//!   named `t`, is recorded whole on insert, update and delete: `new` and
//!   `old` are the row as `to_jsonb` gives it, read by the harness under
//!   `TimeZone` UTC.
//! - What is recorded does not depend on the writer's session settings:
//!   the same values written once with Postgres's defaults and once with
//!   `extra_float_digits`, `TimeZone`, `IntervalStyle`, `DateStyle` and
//!   `bytea_output` set otherwise are recorded alike, with every digit and
//!   times at `+00:00`. Once by a migration (`SET LOCAL`), once by the
//!   runtime role through its role settings (`ALTER ROLE ... SET`, made by
//!   the harness's superuser, the program then started again so that its
//!   connections are new).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history_after, history_standard, last_history_id};
use blackbox::out_of_reach::{WRITER, start};
use blackbox::own_database::{Migration, Own, OwnDatabase, assert_ok, ident, own_execute};
use blackbox::scenario::lines;
use serde_json::{Value, json};

/// `0003_odd_columns`: a composite type `pair`, a table `oddcols` whose
/// columns are named like what a capture might call its rows, and a table
/// named `t` with a column `t`.
fn odd_columns() -> Migration {
    Migration::new(
        "0003_odd_columns",
        &[
            r#"CREATE TYPE "pair" AS ("a" integer, "b" text);"#,
            r#"CREATE TABLE "oddcols" (
	"id" integer PRIMARY KEY NOT NULL,
	"t" text,
	"vm_row" "pair",
	"vm_new" jsonb,
	"vm_old" timestamp with time zone
);"#,
            r#"CREATE TABLE "t" (
	"t" integer PRIMARY KEY NOT NULL,
	"vm_row" text,
	"vm_old" integer[]
);"#,
        ],
    )
}

/// `0003_readings`: a table of every kind of value whose printing depends
/// on a session setting.
fn readings() -> Migration {
    Migration::new(
        "0003_readings",
        &[r#"CREATE TABLE "readings" (
	"id" integer PRIMARY KEY NOT NULL,
	"x" double precision,
	"r" real,
	"n" numeric,
	"at" timestamp with time zone,
	"ts" timestamp,
	"d" date,
	"i" interval,
	"b" bytea
);"#],
    )
}

/// The values of a reading, after its id.
const READING: &str = "0.1234567890123, 1.2345678, 1.50, '2026-03-04 05:06:07.123456+00', '2026-03-04 05:06:07.5', '2026-03-04', '1 day 02:03:04.5', '\\x00ff10'";

/// The settings a writer may set otherwise, as `SET LOCAL` statements.
const ODD_SETTINGS: [&str; 5] = [
    "SET LOCAL extra_float_digits = -15",
    "SET LOCAL TimeZone = 'Asia/Tokyo'",
    "SET LOCAL IntervalStyle = 'sql_standard'",
    "SET LOCAL DateStyle = 'SQL, DMY'",
    "SET LOCAL bytea_output = 'escape'",
];

/// The row of `table` matching `condition`, as `to_jsonb` gives it under
/// `TimeZone` UTC and Postgres's other defaults.
fn row_utc(db: &OwnDatabase, table: &str, condition: &str) -> Value {
    let sql = db.superuser();
    sql.batch(
        "SET TimeZone = 'UTC'; SET extra_float_digits = 1; SET IntervalStyle = 'postgres';
         SET DateStyle = 'ISO, MDY'; SET bytea_output = 'hex'",
    );
    let text = sql
        .value(&format!(
            "SELECT to_jsonb(x)::text FROM {} x WHERE {condition}",
            ident(table)
        ))
        .unwrap_or_else(|| panic!("no row of {table} where {condition}"));
    serde_json::from_str(&text).unwrap()
}

/// One write through the Application as [`WRITER`], which must run.
fn write(own: &Own, actor: &str, sql: &str) -> Value {
    let (_, answer) = own.one(&User::new(WRITER), APP, &own_execute(actor, sql));
    assert_ok(&answer, sql);
    answer
}

/// The history rows after `last`, which must be `count`.
fn recorded(db: &OwnDatabase, last: i64, count: usize, context: &str) -> Vec<Value> {
    let rows = history_after(db, last);
    assert_eq!(
        rows.len(),
        count,
        "{context}: expected {count} history rows, got:\n{}",
        lines(&rows)
    );
    rows
}

#[test]
fn a_row_with_columns_named_t_vm_row_vm_new_and_vm_old_is_recorded_whole() {
    let own = start();
    let mut list = history_standard();
    list.push(odd_columns());
    let db = own.install(APP, &list);
    let last = last_history_id(&db);
    write(
        &own,
        "user",
        "insert into oddcols values (1, 'just t', row(5, 'five')::pair, '{\"k\": [1, 2]}', '2026-01-02 03:04:05+00')",
    );
    let inserted = row_utc(&db, "oddcols", "id = 1");
    write(
        &own,
        "application",
        "update oddcols set t = 'changed t', vm_row = row(6, 'six')::pair where id = 1",
    );
    let updated = row_utc(&db, "oddcols", "id = 1");
    write(&own, "user", "delete from oddcols where id = 1");
    let rows = recorded(&db, last, 3, "oddcols");
    assert_eq!(rows[0]["kind"], "insert");
    assert_eq!(rows[0]["new"], inserted, "the insert's `new`: {}", rows[0]);
    assert_eq!(rows[0]["key"], json!({ "id": 1 }), "{}", rows[0]);
    assert_eq!(rows[1]["kind"], "update");
    assert_eq!(rows[1]["old"], inserted, "the update's `old`: {}", rows[1]);
    assert_eq!(rows[1]["new"], updated, "the update's `new`: {}", rows[1]);
    assert_eq!(rows[1]["changed"], json!(["t", "vm_row"]), "{}", rows[1]);
    assert_eq!(rows[2]["kind"], "delete");
    assert_eq!(rows[2]["old"], updated, "the delete's `old`: {}", rows[2]);
    // A table named `t`, keyed on its column `t`.
    let last = last_history_id(&db);
    write(
        &own,
        "user",
        "insert into t values (7, 'row of t', '{1,2}')",
    );
    let inserted = row_utc(&db, "t", "x.t = 7");
    write(&own, "user", "update t set vm_row = 'changed' where t = 7");
    let updated = row_utc(&db, "t", "x.t = 7");
    write(&own, "user", "delete from t where t = 7");
    let rows = recorded(&db, last, 3, "the table t");
    assert_eq!(rows[0]["new"], inserted, "{}", rows[0]);
    assert_eq!(rows[0]["key"], json!({ "t": 7 }), "{}", rows[0]);
    assert_eq!(rows[1]["old"], inserted, "{}", rows[1]);
    assert_eq!(rows[1]["new"], updated, "{}", rows[1]);
    assert_eq!(rows[1]["key"], json!({ "t": 7 }), "{}", rows[1]);
    assert_eq!(rows[2]["old"], updated, "{}", rows[2]);
}

/// Fails unless `odd`, a history row written with odd settings, records
/// the same values as `reference`, written with the defaults (all but the
/// id), with every digit and times at `+00:00`.
fn assert_recorded_alike(odd: &Value, reference: &Value, context: &str) {
    let strip = |row: &Value| {
        let mut new = row["new"].clone();
        if let Some(map) = new.as_object_mut() {
            map.remove("id");
        }
        new
    };
    assert_eq!(
        strip(odd),
        strip(reference),
        "{context}: expected the values recorded alike whatever the writer's settings;\nwritten with odd settings: {odd}\nwritten with the defaults: {reference}"
    );
    assert_eq!(
        odd["new"]["x"],
        json!(0.123_456_789_012_3),
        "{context}: expected every digit of the double, got {odd}"
    );
    let at = odd["new"]["at"].as_str().unwrap_or_default();
    assert!(
        at.ends_with("+00:00"),
        "{context}: expected the time with the offset +00:00, got {at:?} in {odd}"
    );
}

#[test]
fn a_migration_s_session_settings_do_not_change_what_is_recorded() {
    let own = start();
    let mut list = history_standard();
    list.push(readings());
    own.install(APP, &list);
    let db = own.own_database(APP);
    // Rows the next Version's migration changes, which an earlier
    // migration made, so that its changes are recorded.
    write(&own, "user", "insert into readings (id) values (1), (2)");
    let last = last_history_id(&db);
    let set = format!("({READING})");
    let mut statements = vec![format!(
        r#"UPDATE "readings" SET ("x", "r", "n", "at", "ts", "d", "i", "b") = {set} WHERE "id" = 2;"#
    )];
    statements.extend(ODD_SETTINGS.iter().map(|s| format!("{s};")));
    statements.push(format!(
        r#"UPDATE "readings" SET ("x", "r", "n", "at", "ts", "d", "i", "b") = {set} WHERE "id" = 1;"#
    ));
    let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
    list.push(Migration::new("0004_odd_settings", &refs));
    let sent = own.send(APP, &list);
    assert_eq!(
        sent.status,
        201,
        "expected the Version applied (`SET LOCAL` is not transaction control), got\n{}",
        sent.describe()
    );
    let rows = recorded(&db, last, 2, "the migration's two updates");
    let (reference, odd) = (&rows[0], &rows[1]);
    assert_eq!(reference["key"], json!({ "id": 2 }));
    assert_eq!(odd["key"], json!({ "id": 1 }));
    assert_recorded_alike(odd, reference, "a migration with SET LOCAL");
}

#[test]
fn the_runtime_role_s_own_settings_do_not_change_what_is_recorded() {
    let own = start();
    let mut list = history_standard();
    list.push(readings());
    own.install(APP, &list);
    let db = own.own_database(APP);
    let last = last_history_id(&db);
    write(
        &own,
        "user",
        &format!("insert into readings values (2, {READING})"),
    );
    // The runtime role's own settings, which it may set itself (`ALTER
    // ROLE ... SET` of itself), set here by the superuser; the program is
    // started again so that it opens new connections, which take them.
    let instance = db.instance();
    for setting in [
        "extra_float_digits = -15",
        "TimeZone = 'Asia/Tokyo'",
        "IntervalStyle = 'sql_standard'",
        "DateStyle = 'SQL, DMY'",
        "bytea_output = 'escape'",
    ] {
        instance.batch(&format!("ALTER ROLE {} SET {setting}", ident(&db.runtime)));
    }
    drop(instance);
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    let answer = write(
        &own,
        "application",
        &format!(
            "insert into readings values (3, {READING}) returning current_setting('extra_float_digits') as efd, current_setting('TimeZone') as tz"
        ),
    );
    assert_eq!(
        answer["ok"]["rows"][0],
        json!(["-15", "Asia/Tokyo"]),
        "the harness's own check: expected the write made with the runtime role's own settings (CONTRACT.md: the runtime role can set them through its own role settings), got {answer}"
    );
    let rows = recorded(&db, last, 2, "the two inserts");
    assert_recorded_alike(&rows[1], &rows[0], "the runtime role with its own settings");
}
