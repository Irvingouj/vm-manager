//! The operation `history` through the raw host interface (CONTRACT.md,
//! "The Change history in the platform database: Reading: the operation
//! `history`"; ticket "The Change history lives in the platform database,
//! shipped from an outbox", third and sixth acceptance criteria).
//!
//! - Its arguments: `own` exactly `true`, `as` as for `query`, `table` a
//!   string, `schema` a string defaulting to `public`, `key` an object of
//!   scalars, `limit` an integer from 1 to 1000 (default 100), `before` a
//!   string of digits, `values` as for `query`. Anything else, `null` for
//!   any of them included, is `malformed`, has no Operation log entry and is
//!   recorded refused `malformed`. Arguments it does not have are ignored.
//! - It is decided as a `query` on the own database: both actors with a
//!   request in flight, `no-request` without, `log-unavailable` when its
//!   entry cannot be written.
//! - Its answer: `{ ok: { columns, types?, changes } }`, newest first, each
//!   change with exactly the contract's properties, its rows in the order
//!   of `columns` with each value as `query` gives it; `limit` and `before`
//!   page back.
//! - Which changes: those of the table under the key as the key's columns
//!   compare values (a number or a boolean for a `text` key as its text),
//!   of the Application in flight only; a key that is not exactly the
//!   primary key (a column the table does not have included) answers no
//!   row's changes, the `structure` entries only; a value its column's type
//!   refuses is a `database` error.
//! - It ships first, and an uncommitted change is not in it.
//! - It reads as the runtime role in a read-only transaction, in the
//!   table's present shape; a table not there is `42P01`; a value that no
//!   longer fits fails the read whole.
//! - JSON `null` reads as JSON `null`; `json` reads as `jsonb`.
//! - The limits: `time` (the administrator holds a lock on
//!   `change_history`), `bytes`, `unavailable` (the own database out of
//!   reach; the platform database's is `log-unavailable`, in section 2).
//! - One Operation log entry per read, written before anything is read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::expect::assert_refused;
use blackbox::history::{APP, history_standard};
use blackbox::history_op::{history_call, history_with};
use blackbox::history_reading::{
    RESHAPE, ReadEntry, RowChange, alice, assert_change_fields, assert_history_entry,
    assert_newest_first, assert_row_change, bob, column_values, hold_change_history, id_of,
    id_text, ok_changes, public, read, read_ok, reader, reading_migrations, reshape, statement_of,
    the_entry, user_for, write,
};
use blackbox::own_database::{
    ACTORS, Migration, Own, OwnDatabase, ident, literal, version_body_with_migrations,
};
use blackbox::postgres::PrivatePostgres;
use blackbox::scenario::{assert_database_error, assert_limit, assert_malformed, lines};
use blackbox::transactions::{Target, begin, commit, execute_in, rollback, step, tx};
use blackbox::{INSTALLER, Platform, User, VmOptions, fixture_bundle};
use serde_json::{Value, json};

/// A request of these calls, waiting up to `seconds`; fails unless `200`.
fn run_for(
    own: &Own,
    user: &User,
    application: &str,
    calls: &[Value],
    seconds: u64,
) -> (blackbox::Response, Vec<Value>) {
    own.run_body(
        user,
        application,
        &json!({ "calls": calls }),
        Duration::from_secs(seconds),
    )
}

/// The number of rows of `table` in `db`, read by the harness's superuser.
fn count_rows(db: &OwnDatabase, table: &str) -> i64 {
    db.superuser()
        .value(&format!("SELECT count(*)::text FROM {}", ident(table)))
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1)
}

// ---------------------------------------------------------------------------
// 1. The arguments.
// ---------------------------------------------------------------------------

#[test]
fn every_argument_outside_the_rules_is_malformed_has_no_entry_and_is_recorded_refused() {
    let (own, _db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven')",
    );
    let base = json!({ "own": true, "as": "user", "schema": "public", "table": "things" });
    let with = |name: &str, value: Value| {
        let mut arguments = base.clone();
        arguments[name] = value;
        arguments
    };
    let without = |name: &str| {
        let mut arguments = base.clone();
        arguments.as_object_mut().unwrap().remove(name);
        arguments
    };
    let mut cases: Vec<(String, Value)> = vec![
        ("arguments a string".to_owned(), json!("things")),
        ("arguments an array".to_owned(), json!([base.clone()])),
        ("arguments null".to_owned(), Value::Null),
        ("`own` missing".to_owned(), without("own")),
        ("`table` missing".to_owned(), without("table")),
    ];
    let bad: [(&str, Vec<Value>); 8] = [
        (
            "own",
            vec![json!(false), json!("true"), json!(1), Value::Null],
        ),
        (
            "as",
            vec![
                json!("admin"),
                json!("User"),
                json!(""),
                json!(1),
                json!(true),
                Value::Null,
            ],
        ),
        (
            "table",
            vec![json!(1), json!(true), json!([]), json!({}), Value::Null],
        ),
        (
            "schema",
            vec![
                json!(1),
                json!(true),
                json!(["public"]),
                json!({}),
                Value::Null,
            ],
        ),
        (
            "key",
            vec![
                json!([7]),
                json!("7"),
                json!(7),
                json!(true),
                json!({ "id": [7] }),
                json!({ "id": { "value": 7 } }),
                Value::Null,
            ],
        ),
        (
            "limit",
            vec![
                json!(0),
                json!(1001),
                json!(-1),
                json!(1.5),
                json!("10"),
                json!(true),
                Value::Null,
            ],
        ),
        (
            "before",
            vec![
                json!("12a"),
                json!(12),
                json!(""),
                json!("-1"),
                json!("1.5"),
                json!(" 12"),
                json!("12 "),
                json!(["12"]),
                Value::Null,
            ],
        ),
        (
            "values",
            vec![
                json!("json"),
                json!("TEXT"),
                json!(""),
                json!(1),
                json!(true),
                Value::Null,
            ],
        ),
    ];
    for (name, values) in bad {
        for value in values {
            cases.push((format!("`{name}` {value}"), with(name, value)));
        }
    }
    let calls: Vec<Value> = cases.iter().map(|(_, a)| history_with(a)).collect();
    let (response, answers) = run_for(&own, &alice(), APP, &calls, 30);
    assert_eq!(
        answers.len(),
        cases.len(),
        "the harness's own check: one answer per call"
    );
    for ((what, arguments), answer) in cases.iter().zip(&answers) {
        assert_malformed(answer, &format!("`history` with {what} ({arguments})"));
    }
    let entries = own.entries_of(&response);
    assert!(
        entries.is_empty(),
        "expected no Operation log entry for malformed reads (CONTRACT.md: \"A malformed operation has no entry\"), got:\n{}",
        lines(&entries)
    );
    let records = own.vm.records_of(&response);
    let malformed: Vec<&Value> = records
        .host_calls()
        .into_iter()
        .filter(|r| r["outcome"] == "refused" && r["reason"] == "malformed")
        .collect();
    assert_eq!(
        malformed.len(),
        cases.len(),
        "expected each malformed call recorded as a `host-call` refused with reason malformed, got:\n{}",
        blackbox::records::describe(&records.host_calls())
    );
    // A control: the same arguments, well formed, are read.
    let (_, changes, _) = read_ok(&own, APP, &alice(), &history_with(&base));
    assert_eq!(
        changes.len(),
        1,
        "the well-formed read: expected the insert"
    );
}

#[test]
fn every_form_the_rules_allow_is_read() {
    let (own, _db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven'), (8, 'eight')",
    );
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into counters (name, n) values ('7', 1), ('true', 2), ('plain', 3)",
    );
    // (what, arguments, how many changes)
    let cases: Vec<(&str, Value, usize)> = vec![
        (
            "`schema` left out: public",
            json!({ "own": true, "as": "user", "table": "things" }),
            2,
        ),
        (
            "`as` left out: the User",
            json!({ "own": true, "table": "things" }),
            2,
        ),
        (
            "`as` the Application",
            json!({ "own": true, "as": "application", "table": "things" }),
            2,
        ),
        (
            "`limit` 1",
            json!({ "own": true, "table": "things", "limit": 1 }),
            1,
        ),
        (
            "`limit` 1000",
            json!({ "own": true, "table": "things", "limit": 1000 }),
            2,
        ),
        (
            "`before` \"0\"",
            json!({ "own": true, "table": "things", "before": "0" }),
            0,
        ),
        (
            "`values` \"text\"",
            json!({ "own": true, "table": "things", "values": "text" }),
            2,
        ),
        (
            "`key` with a number",
            json!({ "own": true, "table": "things", "key": { "id": 7 } }),
            1,
        ),
        (
            "`key` with a string",
            json!({ "own": true, "table": "counters", "key": { "name": "plain" } }),
            1,
        ),
        (
            "`key` with a number for a text key",
            json!({ "own": true, "table": "counters", "key": { "name": 7 } }),
            1,
        ),
        (
            "`key` with a boolean for a text key",
            json!({ "own": true, "table": "counters", "key": { "name": true } }),
            1,
        ),
        (
            "`key` with null: no row's key is null",
            json!({ "own": true, "table": "counters", "key": { "name": null } }),
            0,
        ),
        (
            "`key` empty: not the primary key, so no row's changes",
            json!({ "own": true, "table": "things", "key": {} }),
            0,
        ),
    ];
    // Arguments `history` does not have are ignored, each alone and all
    // together.
    let unknown: Vec<(&str, Value)> = vec![
        ("sql", json!("delete from things")),
        ("params", json!([7])),
        ("resource", json!("devops")),
        ("tx", json!("t-1")),
        ("application", json!("twin")),
        ("access", json!("write")),
        ("whatever", json!({ "x": [1, null] })),
        ("nothing", Value::Null),
    ];
    let mut cases = cases;
    let mut all = json!({ "own": true, "table": "things" });
    for (name, value) in &unknown {
        let mut one = json!({ "own": true, "table": "things" });
        one[*name] = value.clone();
        all[*name] = value.clone();
        cases.push(("an argument `history` does not have: ignored", one, 2));
    }
    cases.push((
        "every argument `history` does not have at once: ignored",
        all,
        2,
    ));
    let mut failures = Vec::new();
    for (what, arguments, want) in cases {
        let (_, answer) = read(&own, APP, &alice(), &history_with(&arguments));
        let got = answer["ok"]["changes"].as_array().map(Vec::len);
        if got != Some(want) {
            failures.push(format!(
                "  {what}: {arguments}\n    expected {{ ok }} with {want} change(s), got {answer}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "reads inside the rules were not answered as the contract says:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// 2. Who may read.
// ---------------------------------------------------------------------------

#[test]
fn both_actors_read_with_a_request_in_flight_and_none_reads_without_one() {
    let (own, _db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven')",
    );
    for actor in ACTORS {
        let user = user_for(actor);
        let (response, changes, _) = read_ok(
            &own,
            APP,
            &user,
            &public(actor, "things", &json!({ "key": { "id": 7 } })),
        );
        assert_eq!(changes.len(), 1, "a read as the {actor}");
        assert_history_entry(
            &the_entry(&own, &response),
            &response.request_id(),
            &user.id,
            APP,
            &ReadEntry::allowed(actor, "public", "things", 1),
            &format!("the entry of a read as the {actor}"),
        );
    }
    // At load, with no request in flight: own-at-load.js reads `notes`.
    own.create("loader");
    let sent = own.send_body(
        "loader",
        &version_body_with_migrations(
            &fixture_bundle("own-at-load.js"),
            &[],
            &[Migration::new(
                "0000_notes",
                &[r#"CREATE TABLE "notes" (
	"id" serial PRIMARY KEY NOT NULL,
	"body" text NOT NULL
);"#],
            )],
        ),
    );
    assert_eq!(
        sent.status,
        201,
        "the harness's own check: sending own-at-load.js\n{}",
        sent.describe()
    );
    let response = own.vm.get_as(&alice(), "loader", "/");
    response.expect_status(200);
    let body = response.json();
    for name in ["own_history_as_user", "own_history_as_application"] {
        let answered = &body["atLoad"][name]["answered"];
        assert!(
            !answered.is_null(),
            "expected the call {name} at load to resolve with a refusal (not reject or throw), got {body}"
        );
        assert_refused(answered, "no-request", &format!("{name} at the top level"));
    }
}

#[test]
fn a_read_whose_entry_cannot_be_written_is_refused_log_unavailable() {
    // The platform database (and so the own database) on a Postgres of the
    // test's own, stopped once all is in place.
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_history_log_unavailable");
    let own = Own::start_with(&VmOptions::new().platform(&platform));
    own.create(APP);
    own.send_ok(APP, &history_standard());
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven')",
    );
    let call = public("user", "things", &json!({ "key": { "id": 7 } }));
    let (_, changes, _) = read_ok(&own, APP, &alice(), &call);
    assert_eq!(changes.len(), 1, "a read while all is well");

    postgres.stop();
    for actor in ACTORS {
        let (_, answers) = run_for(
            &own,
            &user_for(actor),
            APP,
            &[public(actor, "things", &json!({ "key": { "id": 7 } }))],
            40,
        );
        assert_refused(
            &answers[0],
            "log-unavailable",
            &format!("a read as the {actor} whose Operation log entry cannot be written"),
        );
    }
}

// ---------------------------------------------------------------------------
// 3. The answer.
// ---------------------------------------------------------------------------

#[test]
fn the_answer_is_every_change_newest_first_with_every_property() {
    let (own, _db) = reader();
    let inserted = write(
        &own,
        APP,
        &alice(),
        "user",
        r#"insert into things (id, title, size, tags) values (7, 'first', 1, '{"a": 1}')"#,
    );
    let updated = write(
        &own,
        APP,
        &bob(),
        "application",
        "update things set title = 'second', size = 2 where id = 7",
    );
    let deleted = write(
        &own,
        APP,
        &alice(),
        "user",
        "delete from things where id = 7",
    );
    let (_, changes, ok) = read_ok(
        &own,
        APP,
        &alice(),
        &public("user", "things", &json!({ "key": { "id": 7 } })),
    );
    assert_eq!(
        ok["columns"],
        json!(["id", "title", "size", "tags"]),
        "expected `columns` the table's columns in its order, got {ok}"
    );
    assert!(
        ok.get("types").is_none(),
        "expected no `types` without `values: \"text\"`, got {ok}"
    );
    let keys: Vec<&String> = ok.as_object().unwrap().keys().collect();
    assert!(
        keys.iter().all(|k| *k == "columns" || *k == "changes"),
        "expected `ok` to hold `columns` and `changes`, got {ok}"
    );
    assert_eq!(changes.len(), 3, "expected three changes, got {changes:?}");
    assert_newest_first(&changes, "a row inserted, updated and deleted");
    let first = json!([7, "first", 1, { "a": 1 }]);
    let second = json!([7, "second", 2, { "a": 1 }]);
    let key = json!({ "id": 7 });
    assert_row_change(
        &changes[0],
        &RowChange::delete("alice", "user", &deleted, key.clone(), second.clone()),
        "the newest, the delete",
    );
    assert_row_change(
        &changes[1],
        &RowChange::update(
            "bob",
            "application",
            &updated,
            key.clone(),
            first.clone(),
            second,
            json!(["title", "size"]),
        ),
        "the update, as the Application",
    );
    assert_row_change(
        &changes[2],
        &RowChange::insert("alice", "user", &inserted, key, first),
        "the oldest, the insert",
    );
    // Without a key: every row of the table, newest first.
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (8, 'eight'), (9, 'nine')",
    );
    let (_, all, _) = read_ok(&own, APP, &alice(), &public("user", "things", &json!({})));
    assert_eq!(all.len(), 5, "the whole table's history");
    assert_newest_first(&all, "the whole table's history");
    for change in &all {
        assert_change_fields(change, "a change of the whole table's history");
    }
}

#[test]
fn each_value_is_as_query_gives_it_and_values_text_gives_types_and_text() {
    let (own, _db) = reader();
    let insert = r#"insert into typed (id, n, t, d, flag, r, arr, j, u, s) values
        (9007199254740993, 12.30, '2026-03-01 12:00:00.123456+00', '2026-03-01', true, 1.5,
         ARRAY['a,b', '"q"', '', NULL, 'back\slash', '{brace}'], '{"a": [1, 2], "z": 1}',
         'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 7),
        (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"#;
    write(&own, APP, &alice(), "user", insert);
    for (key, id) in [("9007199254740993", 9_007_199_254_740_993_u64), ("2", 2)] {
        for values in [None, Some("text")] {
            let mut extra = json!({ "key": { "id": key } });
            let mut select = json!({ "own": true, "as": "user", "sql": format!("select * from typed where id = {id}") });
            if let Some(v) = values {
                extra["values"] = json!(v);
                select["values"] = json!(v);
            }
            let context = format!("the row {id} with values {values:?}");
            let (_, changes, ok) = read_ok(&own, APP, &alice(), &public("user", "typed", &extra));
            let (_, queried) = own.one(&alice(), APP, &blackbox::scenario::op("query", &select));
            assert!(
                queried["ok"]["rows"].is_array(),
                "the harness's own check: {context}: the query must run, got {queried}"
            );
            assert_eq!(
                ok["columns"], queried["ok"]["columns"],
                "{context}: expected the history's `columns` as the query's"
            );
            match values {
                Some(_) => assert_eq!(
                    ok["types"], queried["ok"]["types"],
                    "{context}: expected `types`, the columns' type OIDs, as the query gives them"
                ),
                None => assert!(
                    ok.get("types").is_none(),
                    "{context}: expected no `types`, got {ok}"
                ),
            }
            assert_eq!(changes.len(), 1, "{context}: one insert");
            assert_eq!(
                changes[0]["new"], queried["ok"]["rows"][0],
                "{context}: expected the change's `new` to be the row as `query` gives it"
            );
        }
    }
    // The key itself, as `query` would give the value of its column.
    let (_, changes, _) = read_ok(
        &own,
        APP,
        &alice(),
        &public(
            "user",
            "typed",
            &json!({ "key": { "id": "9007199254740993" }, "values": "text" }),
        ),
    );
    assert_eq!(
        changes[0]["new"][0],
        json!("9007199254740993"),
        "a bigint beyond 2^53 kept whole, as text"
    );
}

#[test]
fn limit_and_before_page_back_through_a_long_history() {
    let (own, _db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title, size) select g, 'row ' || g, g from generate_series(1, 250) g",
    );
    let call = |extra: Value| public("user", "things", &extra);
    let (_, by_default, _) = read_ok(&own, APP, &alice(), &call(json!({})));
    assert_eq!(by_default.len(), 100, "expected 100 changes by default");
    let mut seen: Vec<Value> = Vec::new();
    let mut before: Option<String> = None;
    let mut pages = Vec::new();
    loop {
        let extra = match &before {
            Some(b) => json!({ "before": b }),
            None => json!({}),
        };
        let (_, page, _) = read_ok(&own, APP, &alice(), &call(extra));
        pages.push(page.len());
        let Some(last) = page.last() else { break };
        before = Some(id_text(last));
        seen.extend(page);
        assert!(pages.len() <= 5, "too many pages: {pages:?}");
    }
    assert_eq!(
        pages,
        vec![100, 100, 50, 0],
        "expected pages of 100, 100, 50 and then none"
    );
    assert_newest_first(&seen, "the pages one after another");
    assert_eq!(
        column_values(&seen, 2),
        (1..=250).rev().map(|g| json!(g)).collect::<Vec<_>>(),
        "expected every insert once, newest (size 250) first"
    );
    // One row's long history, paged with a smaller limit.
    write(&own, APP, &alice(), "user", "select bump(1, 149)");
    let mut sizes = Vec::new();
    let mut before: Option<String> = None;
    let mut pages = Vec::new();
    loop {
        let mut extra = json!({ "key": { "id": 1 }, "limit": 60 });
        if let Some(b) = &before {
            extra["before"] = json!(b);
        }
        let (_, page, _) = read_ok(&own, APP, &alice(), &call(extra));
        pages.push(page.len());
        let Some(last) = page.last() else { break };
        before = Some(id_text(last));
        sizes.extend(column_values(&page, 2));
        assert!(pages.len() <= 5, "too many pages: {pages:?}");
    }
    assert_eq!(
        pages,
        vec![60, 60, 30, 0],
        "row 1: one insert and 149 updates"
    );
    assert_eq!(
        sizes,
        (1..=150).rev().map(|g| json!(g)).collect::<Vec<_>>(),
        "row 1's sizes, newest first"
    );
    let (_, everything, _) = read_ok(&own, APP, &alice(), &call(json!({ "limit": 1000 })));
    assert_eq!(
        everything.len(),
        399,
        "250 inserts and 149 updates, at most 1000"
    );
    let (_, one, _) = read_ok(&own, APP, &alice(), &call(json!({ "limit": 1 })));
    assert_eq!(one.len(), 1);
    assert_eq!(
        id_of(&one[0]),
        id_of(&everything[0]),
        "`limit` 1: the newest"
    );
}

// ---------------------------------------------------------------------------
// 4. The key.
// ---------------------------------------------------------------------------

#[test]
fn the_key_is_compared_as_the_key_columns_compare_values() {
    let (own, _db) = reader();
    for sql in [
        "insert into things (id, title) values (7, 'seven'), (70, 'seventy')",
        "insert into bigs (id, note) values (7, 'seven'), (9007199254740993, 'big'), (9007199254740992, 'big minus one')",
        "insert into nums (k, note) values (1.50, 'one and a half'), (15, 'fifteen'), (2, 'two')",
        "insert into stamps (t, note) values ('2026-03-01 12:00:00+00', 'noon'), ('2026-03-01 13:00:00+00', 'one')",
        "insert into inventory.stock (sku, warehouse, qty) values ('sku-1', 'north', 5), ('sku-1', 'south', 3), ('sku-2', 'north', 1)",
    ] {
        write(&own, APP, &alice(), "user", sql);
    }
    // (schema, table, key, the note or title of the one row expected, the
    // index of that column)
    let cases: Vec<(&str, &str, Value, Value, usize)> = vec![
        ("public", "things", json!({ "id": 7 }), json!("seven"), 1),
        ("public", "things", json!({ "id": "7" }), json!("seven"), 1),
        (
            "public",
            "things",
            json!({ "id": "007" }),
            json!("seven"),
            1,
        ),
        ("public", "bigs", json!({ "id": 7 }), json!("seven"), 1),
        ("public", "bigs", json!({ "id": "7" }), json!("seven"), 1),
        (
            "public",
            "bigs",
            json!({ "id": "9007199254740993" }),
            json!("big"),
            1,
        ),
        (
            "public",
            "bigs",
            json!({ "id": "9007199254740992" }),
            json!("big minus one"),
            1,
        ),
        (
            "public",
            "nums",
            json!({ "k": "1.5" }),
            json!("one and a half"),
            1,
        ),
        (
            "public",
            "nums",
            json!({ "k": "1.50" }),
            json!("one and a half"),
            1,
        ),
        (
            "public",
            "nums",
            json!({ "k": "1.500" }),
            json!("one and a half"),
            1,
        ),
        (
            "public",
            "nums",
            json!({ "k": 1.5 }),
            json!("one and a half"),
            1,
        ),
        ("public", "nums", json!({ "k": 2 }), json!("two"), 1),
        ("public", "nums", json!({ "k": "2.0" }), json!("two"), 1),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01T12:00:00Z" }),
            json!("noon"),
            1,
        ),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01 12:00:00+00" }),
            json!("noon"),
            1,
        ),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01T13:00:00+01:00" }),
            json!("noon"),
            1,
        ),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01 07:00:00-05:00" }),
            json!("noon"),
            1,
        ),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01T12:00:00.000000Z" }),
            json!("noon"),
            1,
        ),
        (
            "public",
            "stamps",
            json!({ "t": "2026-03-01T13:00:00Z" }),
            json!("one"),
            1,
        ),
        (
            "inventory",
            "stock",
            json!({ "sku": "sku-1", "warehouse": "north" }),
            json!(5),
            2,
        ),
        (
            "inventory",
            "stock",
            json!({ "warehouse": "south", "sku": "sku-1" }),
            json!(3),
            2,
        ),
    ];
    let mut failures = Vec::new();
    for (schema, table, key, want, index) in cases {
        let (_, answer) = read(
            &own,
            APP,
            &alice(),
            &history_call("user", schema, table, &json!({ "key": key })),
        );
        let got = answer["ok"]["changes"]
            .as_array()
            .map(|c| column_values(c, index));
        if got.as_deref() != Some(std::slice::from_ref(&want)) {
            failures.push(format!(
                "  {schema}.{table} key {key}: expected the one change of the row whose value is {want}, got {answer}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "keys not compared as their columns compare values:\n{}",
        failures.join("\n")
    );
}

#[test]
fn a_key_that_is_not_exactly_the_primary_key_answers_no_row_and_a_value_its_type_refuses_is_a_database_error()
 {
    let (own, _db) = reader();
    for sql in [
        "insert into things (id, title) values (7, 'seven')",
        "insert into nums (k, note) values (1.5, 'one and a half')",
        "insert into stamps (t, note) values ('2026-03-01 12:00:00+00', 'noon')",
        "insert into bigs (id, note) values (7, 'seven')",
        "insert into inventory.stock (sku, warehouse, qty) values ('sku-1', 'north', 5)",
        "insert into keyless (a, b) values ('a', 1)",
    ] {
        write(&own, APP, &alice(), "user", sql);
    }
    // Not exactly the primary key's columns, a column the table does not
    // have included: no row's changes (and no structure entry, as none was
    // recorded for these tables; with them, see the next test). Not a
    // database error.
    let none: Vec<(&str, &str, Value)> = vec![
        ("public", "things", json!({ "nope": 7 })),
        ("public", "things", json!({ "id": 7, "nope": 7 })),
        ("public", "keyless", json!({ "nope": "a" })),
        ("inventory", "stock", json!({ "sku": "sku-1" })),
        ("inventory", "stock", json!({ "warehouse": "north" })),
        (
            "inventory",
            "stock",
            json!({ "sku": "sku-1", "warehouse": "north", "qty": 5 }),
        ),
        ("public", "things", json!({ "title": "seven" })),
        ("public", "things", json!({ "id": 7, "title": "seven" })),
        ("public", "keyless", json!({ "a": "a" })),
        ("public", "things", json!({ "id": 8 })),
    ];
    let mut failures = Vec::new();
    for (schema, table, key) in none {
        let (_, answer) = read(
            &own,
            APP,
            &alice(),
            &history_call("user", schema, table, &json!({ "key": key })),
        );
        if answer["ok"]["changes"].as_array().map(Vec::len) != Some(0) {
            failures.push(format!(
                "  {schema}.{table} key {key}: expected {{ ok }} with no change, got {answer}"
            ));
        }
    }
    // A value the column's type refuses: (table, key, SQLSTATE)
    for (table, key, code) in [
        ("things", json!({ "id": "x" }), "22P02"),
        ("things", json!({ "id": "7.5" }), "22P02"),
        ("bigs", json!({ "id": "99999999999999999999" }), "22003"),
        ("nums", json!({ "k": "one" }), "22P02"),
        ("stamps", json!({ "t": "not a time" }), "22007"),
    ] {
        let (_, answer) = read(
            &own,
            APP,
            &alice(),
            &public("user", table, &json!({ "key": key })),
        );
        if answer["error"]["kind"] != "database" || answer["error"]["code"] != code {
            failures.push(format!(
                "  {table} key {key}: expected {{ error: {{ kind: \"database\", code: {code:?} }} }}, got {answer}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "keys answered otherwise than the contract says:\n{}",
        failures.join("\n")
    );
}

/// CONTRACT.md: "A `key` that names something other than exactly the
/// table's primary key columns (one missing, another column, a name the
/// table does not have, any key for a table without a primary key) answers
/// no row's changes (the `structure` entries only)". On `shapes` after
/// [`RESHAPE`], which has two `structure` entries and two changes of row 1.
#[test]
fn a_key_that_is_not_exactly_the_primary_key_answers_the_structure_entries_only() {
    let (own, _db, _, _) = reshaped();
    let (_, all, _) = read_ok(&own, APP, &alice(), &public("user", "shapes", &json!({})));
    let structure_ids: Vec<u128> = all
        .iter()
        .filter(|c| c["kind"] == "structure")
        .map(id_of)
        .collect();
    assert_eq!(
        (all.len(), structure_ids.len()),
        (4, 2),
        "the harness's own check: shapes has two structure entries and two changes of row 1, got {all:?}"
    );
    let (_, of_row, _) = read_ok(
        &own,
        APP,
        &alice(),
        &public("user", "shapes", &json!({ "key": { "id": 1 } })),
    );
    assert_eq!(
        of_row.len(),
        4,
        "the control: the primary key answers the row's changes and the structure entries, got {of_row:?}"
    );
    let mut failures = Vec::new();
    for key in [
        json!({}),
        json!({ "nope": 1 }),
        json!({ "id": 1, "nope": 1 }),
        json!({ "c": 6 }),
        json!({ "id": 1, "c": 6 }),
        json!({ "a": "x" }),
        json!({ "b": "y" }),
    ] {
        let (_, answer) = read(
            &own,
            APP,
            &alice(),
            &public("user", "shapes", &json!({ "key": key })),
        );
        let ids: Option<Vec<u128>> = answer["ok"]["changes"]
            .as_array()
            .map(|cs| cs.iter().map(id_of).collect());
        if ids.as_ref() != Some(&structure_ids) {
            failures.push(format!(
                "  shapes key {key}: expected {{ ok }} with the structure entries only, ids {structure_ids:?}, got {answer}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "keys that are not exactly the primary key answered otherwise than the contract says:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// 5. Only the Application in flight's.
// ---------------------------------------------------------------------------

#[test]
fn each_application_reads_only_its_own_changes_whatever_is_passed() {
    let own = Own::start();
    own.install(APP, &history_standard());
    own.install("twin", &history_standard());
    for (application, title) in [(APP, "of board"), ("twin", "of twin")] {
        write(
            &own,
            application,
            &alice(),
            "user",
            &format!("insert into things (id, title) values (7, '{title}')"),
        );
        write(
            &own,
            application,
            &bob(),
            "application",
            &format!("update things set size = 1 where id = 7 and title = '{title}'"),
        );
    }
    for (application, title) in [(APP, "of board"), ("twin", "of twin")] {
        for extra in [json!({ "key": { "id": 7 } }), json!({})] {
            for actor in ACTORS {
                let (_, changes, _) = read_ok(
                    &own,
                    application,
                    &user_for(actor),
                    &public(actor, "things", &extra),
                );
                let context = format!("{application} reading as the {actor} with {extra}");
                assert_eq!(changes.len(), 2, "{context}: its insert and its update");
                for change in &changes {
                    assert_eq!(
                        change["application"], application,
                        "{context}: expected only its own changes, got {change}"
                    );
                    assert_eq!(change["new"][1], title, "{context}: {change}");
                }
            }
        }
        // Naming the other Application changes nothing: `application` is
        // not an argument of `history`, and is ignored.
        let other = if application == APP { "twin" } else { APP };
        let (_, answer) = read(
            &own,
            application,
            &alice(),
            &public(
                "user",
                "things",
                &json!({ "key": { "id": 7 }, "application": other }),
            ),
        );
        let changes = ok_changes(&answer, "a read naming another Application");
        assert!(
            changes.len() == 2 && changes.iter().all(|c| c["application"] == application),
            "{application} naming {other}: expected only its own two changes, got {answer}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. It ships first; uncommitted changes are not in it.
// ---------------------------------------------------------------------------

#[test]
fn a_change_is_read_in_the_same_request_and_in_the_next() {
    let (own, _db) = reader();
    for round in 0..6_i64 {
        let actor = ACTORS[usize::try_from(round % 2).unwrap()];
        let user = user_for(actor);
        let id = 100 + round;
        let key = json!({ "key": { "id": id } });
        let (response, answers) = run_for(
            &own,
            &user,
            APP,
            &[
                blackbox::history::own_call(
                    "execute",
                    actor,
                    "insert into things (id, title) values ($1, 'written')",
                    &json!([id]),
                ),
                public(actor, "things", &key),
            ],
            20,
        );
        assert!(
            answers[0]["ok"].is_object(),
            "the harness's own check: the insert ran, got {}",
            answers[0]
        );
        let same = ok_changes(
            &answers[1],
            &format!("round {round}: the read after the write, same request"),
        );
        assert_eq!(
            same.len(),
            1,
            "round {round}: the insert, read in its own request: {same:?}"
        );
        assert_eq!(same[0]["kind"], "insert");
        assert_eq!(
            same[0]["request"].as_str(),
            Some(response.request_id().as_str())
        );
        let updated = write(
            &own,
            APP,
            &user,
            actor,
            &format!("update things set title = 'changed' where id = {id}"),
        );
        let (_, next, _) = read_ok(&own, APP, &user, &public(actor, "things", &key));
        assert_eq!(
            next.len(),
            2,
            "round {round}: the update, read in the next request: {next:?}"
        );
        assert_eq!(next[0]["kind"], "update");
        assert_eq!(next[0]["request"].as_str(), Some(updated.as_str()));
    }
    // Many rows in one statement, read in the same request.
    let (_, answers) = run_for(
        &own,
        &alice(),
        APP,
        &[
            blackbox::own_database::own_execute(
                "user",
                "insert into things (id, title) select g, 'many' from generate_series(1000, 1499) g",
            ),
            public("user", "things", &json!({ "limit": 1000 })),
        ],
        30,
    );
    let changes = ok_changes(&answers[1], "500 inserts read in their own request");
    assert_eq!(
        changes.len(),
        500 + 12,
        "expected the 500 inserts of the request and the 12 changes before"
    );
}

#[test]
fn an_uncommitted_change_is_not_read_and_a_committed_one_is() {
    let own = Own::start();
    let ledger = "ledger";
    own.create(ledger);
    let sent = own.send_body(
        ledger,
        &version_body_with_migrations(&fixture_bundle("transactions.js"), &[], &history_standard()),
    );
    assert_eq!(
        sent.status,
        201,
        "the harness's own check: sending transactions.js\n{}",
        sent.describe()
    );
    let read_of = |id: i64| {
        step(
            "history",
            json!({ "own": true, "as": "user", "table": "things", "key": { "id": id } }),
        )
    };
    let read_in_tx = |id: i64, name: &str| {
        step(
            "history",
            json!({ "own": true, "as": "user", "table": "things", "key": { "id": id }, "tx": tx(name) }),
        )
    };
    for (round, actor) in (0_i64..).zip(ACTORS) {
        let user = user_for(actor);
        let (kept, dropped) = (500 + 10 * round, 501 + 10 * round);
        let steps = [
            begin(Target::Own(actor), "write", "t"),
            execute_in(
                "t",
                &format!("insert into things (id, title) values ({kept}, 'committed later')"),
            ),
            read_of(kept),
            read_in_tx(kept, "t"),
            commit("t"),
            read_of(kept),
            begin(Target::Own(actor), "write", "r"),
            execute_in(
                "r",
                &format!("insert into things (id, title) values ({dropped}, 'rolled back')"),
            ),
            read_of(dropped),
            rollback("r"),
            read_of(dropped),
        ];
        let (_, answers) = own.run_body(
            &user,
            ledger,
            &json!({ "steps": steps }),
            Duration::from_secs(40),
        );
        let context = |i: usize| format!("as the {actor}, step {i} ({})", steps[i]);
        for i in [0, 1, 4, 6, 7, 9] {
            assert!(
                answers[i]["ok"].is_object(),
                "the harness's own check: {}: expected it to run, got {}",
                context(i),
                answers[i]
            );
        }
        assert_eq!(
            ok_changes(&answers[2], &context(2)).len(),
            0,
            "{}: a change of an open transaction is not in a read",
            context(2)
        );
        assert_eq!(
            ok_changes(&answers[3], &context(3)).len(),
            0,
            "{}: a read never takes a transaction: `tx` is not an argument of `history`, and is ignored",
            context(3)
        );
        let committed = ok_changes(&answers[5], &context(5));
        assert_eq!(committed.len(), 1, "{}: the committed insert", context(5));
        assert_eq!(committed[0]["new"][1], "committed later");
        assert_eq!(
            ok_changes(&answers[8], &context(8)).len(),
            0,
            "{}",
            context(8)
        );
        assert_eq!(
            ok_changes(&answers[10], &context(10)).len(),
            0,
            "{}: a change rolled back is never read",
            context(10)
        );
    }
}

// ---------------------------------------------------------------------------
// 7. Read as the runtime role, read-only.
// ---------------------------------------------------------------------------

#[test]
fn the_read_runs_the_applications_code_as_the_runtime_role_and_writes_nothing() {
    let (own, db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into guarded_rows (id, v) values (1, 'boom'), (2, 'write'), (3, 'fine')",
    );
    assert_eq!(
        count_rows(&db, "side_effects"),
        1,
        "the harness's own check: guard_check wrote once when the row 'write' was inserted"
    );
    let (response, answer) = read(
        &own,
        APP,
        &alice(),
        &public("user", "guarded_rows", &json!({ "key": { "id": 1 } })),
    );
    assert_database_error(&answer, "P0001", "a read whose domain check raises");
    let message = answer["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!("guard_check ran as {}", db.runtime)),
        "expected the domain's check, run in a read-only transaction, to have run as the runtime role {:?}, got {answer}",
        db.runtime
    );
    assert_history_entry(
        &the_entry(&own, &response),
        &response.request_id(),
        "alice",
        APP,
        &ReadEntry::failed("user", "public", "guarded_rows", "P0001"),
        "the entry of the read that failed in the domain's check",
    );
    for actor in ACTORS {
        let (_, answer) = read(
            &own,
            APP,
            &user_for(actor),
            &public(actor, "guarded_rows", &json!({ "key": { "id": 2 } })),
        );
        assert_database_error(
            &answer,
            "25006",
            &format!("a read as the {actor} whose domain check writes: the read is read-only"),
        );
    }
    assert_eq!(
        count_rows(&db, "side_effects"),
        1,
        "expected nothing written by the Application's code during the reads"
    );
    let (_, changes, _) = read_ok(
        &own,
        APP,
        &alice(),
        &public("user", "guarded_rows", &json!({ "key": { "id": 3 } })),
    );
    assert_eq!(
        column_values(&changes, 1),
        vec![json!("fine")],
        "a row whose check passes is read"
    );
}

// ---------------------------------------------------------------------------
// 8. The table's present shape.
// ---------------------------------------------------------------------------

/// [`reader`], rows written into `shapes`, `retyped` and `gone`, then
/// [`reshape`] sent as Version 2. Returns the insert's and the update's
/// request ids.
fn reshaped() -> (Own, OwnDatabase, String, String) {
    let (own, db) = reader();
    let inserted = write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into shapes (id, a, b, c, d) values (1, 'x', 'y', 5, '12')",
    );
    let updated = write(
        &own,
        APP,
        &bob(),
        "application",
        "update shapes set c = 6 where id = 1",
    );
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into retyped (id, v) values (1, 'abc')",
    );
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into retyped (id, v) values (2, '42')",
    );
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into gone (id, v) values (1, 'v')",
    );
    let mut all = reading_migrations();
    all.push(reshape());
    own.send_ok(APP, &all);
    (own, db, inserted, updated)
}

/// Fails unless `change` is a `structure` entry of [`RESHAPE`] saying
/// `structure`.
fn assert_structure(change: &Value, structure: &Value, context: &str) {
    assert_change_fields(change, context);
    let mut got = change.clone();
    let o = got.as_object_mut().unwrap();
    o.remove("id");
    o.remove("at");
    assert_eq!(
        got,
        json!({
            "kind": "structure", "key": null, "old": null, "new": null, "changed": null,
            "user_id": INSTALLER, "application": APP, "actor": "migration",
            "request": null, "migration": RESHAPE, "structure": structure,
        }),
        "{context}"
    );
}

#[test]
fn the_answer_is_in_the_tables_present_shape() {
    let (own, _db, inserted, updated) = reshaped();
    for extra in [json!({ "key": { "id": 1 } }), json!({})] {
        let (_, changes, ok) = read_ok(&own, APP, &alice(), &public("user", "shapes", &extra));
        let context = format!("shapes read with {extra} after {RESHAPE}");
        assert_eq!(
            ok["columns"],
            json!(["id", "bb", "c", "d", "e"]),
            "{context}: expected the present columns: `a` dropped, `b` renamed `bb`, `e` added"
        );
        assert_eq!(
            changes.len(),
            4,
            "{context}: two structure entries, the update, the insert; got {changes:?}"
        );
        assert_newest_first(&changes, &context);
        let mut structures: Vec<Value> = changes[..2]
            .iter()
            .map(|c| c["structure"].clone())
            .collect();
        structures.sort_by_key(Value::to_string);
        let mut want = vec![
            json!({ "change": "column-dropped", "column": "a" }),
            json!({ "change": "column-type", "column": "d", "from": "text", "to": "integer" }),
        ];
        want.sort_by_key(Value::to_string);
        assert_eq!(structures, want, "{context}: the structure entries");
        for change in &changes[..2] {
            let s = change["structure"].clone();
            assert_structure(change, &s, &context);
        }
        let key = json!({ "id": 1 });
        assert_row_change(
            &changes[2],
            &RowChange::update(
                "bob",
                "application",
                &updated,
                key.clone(),
                json!([1, null, 5, 12, null]),
                json!([1, null, 6, 12, null]),
                json!(["c"]),
            ),
            &format!(
                "{context}: the update, `bb` null (renamed since), `d` cast to integer, `e` null"
            ),
        );
        assert_row_change(
            &changes[3],
            &RowChange::insert(
                "alice",
                "user",
                &inserted,
                key,
                json!([1, null, 5, 12, null]),
            ),
            &format!("{context}: the insert"),
        );
    }
    // A table that is not there now: dropped, never made, or in another
    // schema.
    for (schema, table) in [
        ("public", "gone"),
        ("public", "nothing_here"),
        ("inventory", "things"),
        ("public", "Things"),
    ] {
        let (response, answer) = read(
            &own,
            APP,
            &alice(),
            &history_call("user", schema, table, &json!({})),
        );
        assert_database_error(
            &answer,
            "42P01",
            &format!("{schema}.{table}, not there now"),
        );
        let mut want = ReadEntry::failed("user", schema, table, "42P01");
        want.version = 2;
        assert_history_entry(
            &the_entry(&own, &response),
            &response.request_id(),
            "alice",
            APP,
            &want,
            &format!("the entry of the read of {schema}.{table}"),
        );
    }
}

#[test]
fn a_value_that_no_longer_fits_its_column_fails_the_read_whole() {
    let (own, _db, _, _) = reshaped();
    // `retyped`, newest first: the column-type entry, row 2 ('42'), row 1
    // ('abc', which no integer is).
    for (extra, code) in [
        (json!({}), Some("22P02")),
        (json!({ "key": { "id": 1 } }), Some("22P02")),
        (json!({ "limit": 3 }), Some("22P02")),
        (json!({ "limit": 2 }), None),
        (json!({ "key": { "id": 2 } }), None),
    ] {
        let (response, answer) = read(&own, APP, &alice(), &public("user", "retyped", &extra));
        let context = format!("retyped read with {extra}");
        match code {
            Some(code) => {
                assert_database_error(
                    &answer,
                    code,
                    &format!("{context}: the answer would hold 'abc'"),
                );
                let mut want = ReadEntry::failed("user", "public", "retyped", code);
                want.version = 2;
                assert_history_entry(
                    &the_entry(&own, &response),
                    &response.request_id(),
                    "alice",
                    APP,
                    &want,
                    &context,
                );
            }
            None => {
                let changes = ok_changes(&answer, &context);
                assert_eq!(changes.len(), 2, "{context}: {changes:?}");
                assert_structure(
                    &changes[0],
                    &json!({ "change": "column-type", "column": "v", "from": "text", "to": "integer" }),
                    &context,
                );
                assert_eq!(
                    changes[1]["new"],
                    json!([2, 42]),
                    "{context}: '42' cast to integer"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 9. JSON null, and json as jsonb.
// ---------------------------------------------------------------------------

#[test]
fn json_null_reads_as_json_null_and_json_reads_as_jsonb() {
    let (own, db) = reader();
    let messy = r#"{"b": 1,  "a": 2, "a": 3}"#;
    write(
        &own,
        APP,
        &alice(),
        "user",
        &format!(
            "insert into docs (id, jb, js) values (1, 'null'::jsonb, 'null'::json), (2, NULL, NULL), (3, {m}, {m})",
            m = literal(messy)
        ),
    );
    let as_jsonb = db
        .superuser()
        .value(&format!("SELECT {}::jsonb::text", literal(messy)))
        .unwrap();
    let as_value: Value = serde_json::from_str(&as_jsonb).unwrap();
    // (id, values, the row's `new`)
    let cases = [
        (1, None, json!([1, null, null])),
        (1, Some("text"), json!(["1", "null", "null"])),
        (2, None, json!([2, null, null])),
        (2, Some("text"), json!(["2", null, null])),
        (3, None, json!([3, as_value, as_value])),
        (3, Some("text"), json!(["3", as_jsonb, as_jsonb])),
    ];
    for (id, values, want) in cases {
        let mut extra = json!({ "key": { "id": id } });
        if let Some(v) = values {
            extra["values"] = json!(v);
        }
        let (_, changes, _) = read_ok(&own, APP, &alice(), &public("user", "docs", &extra));
        assert_eq!(changes.len(), 1, "docs row {id}");
        assert_eq!(
            changes[0]["new"], want,
            "docs row {id} with values {values:?}: JSON null reads as JSON null (in text, \"null\"), SQL null as null, `json` as `jsonb` prints it"
        );
    }
    // A control: what `query` gives the jsonb column of row 1 in text.
    let (_, queried) = own.one(
        &alice(),
        APP,
        &blackbox::scenario::op(
            "query",
            &json!({ "own": true, "as": "user", "sql": "select jb from docs where id = 1", "values": "text" }),
        ),
    );
    assert_eq!(
        queried["ok"]["rows"],
        json!([["null"]]),
        "the harness's own check: query's text of JSON null"
    );
}

// ---------------------------------------------------------------------------
// 10. The limits.
// ---------------------------------------------------------------------------

#[test]
fn a_read_held_up_past_ten_seconds_is_ended_at_the_time_limit_with_its_entry_written_first() {
    let (own, db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven')",
    );
    let call = public("user", "things", &json!({ "key": { "id": 7 } }));
    let (_, changes, _) = read_ok(&own, APP, &alice(), &call);
    assert_eq!(
        changes.len(),
        1,
        "the harness's own check: shipped before the lock"
    );

    let lock = hold_change_history(&db);
    let started = Instant::now();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "update things set title = 'held' where id = 7",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "expected a write answered without waiting for its shipment, took {:?}",
        started.elapsed()
    );
    let statement = statement_of("public", "things");
    let (outcome, elapsed, unfinished) = thread::scope(|scope| {
        let reading = scope.spawn(|| {
            let started = Instant::now();
            let got = run_for(&own, &alice(), APP, std::slice::from_ref(&call), 40);
            (got, started.elapsed())
        });
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut unfinished = None;
        while Instant::now() < deadline && unfinished.is_none() {
            unfinished = own.log().into_iter().find(|e| {
                e["operation"] == "history"
                    && e["outcome"] == "unfinished"
                    && e["statement"] == statement.as_str()
            });
            thread::sleep(Duration::from_millis(200));
        }
        let (got, elapsed) = reading.join().unwrap();
        (got, elapsed, unfinished)
    });
    let ((response, answers), elapsed) = (outcome, elapsed);
    lock.batch("COMMIT");
    let unfinished = unfinished.unwrap_or_else(|| {
        panic!("expected the read's entry in the Operation log, `unfinished`, while the read was held up")
    });
    assert_eq!(
        unfinished["request"].as_str(),
        Some(response.request_id().as_str()),
        "the unfinished entry is the held read's: {unfinished}"
    );
    assert_limit(
        &answers[0],
        "time",
        "a read held up by a lock on change_history",
    );
    assert!(
        elapsed >= Duration::from_millis(9500) && elapsed <= Duration::from_secs(15),
        "expected the read ended after about 10 s, took {elapsed:?}"
    );
    let entry = the_entry(&own, &response);
    assert_eq!(
        entry["id"], unfinished["id"],
        "one entry, completed: {entry}"
    );
    assert_history_entry(
        &entry,
        &response.request_id(),
        "alice",
        APP,
        &ReadEntry::failed("user", "public", "things", "time"),
        "the entry of the read ended at the time limit",
    );
    let (_, changes, _) = read_ok(&own, APP, &alice(), &call);
    assert_eq!(
        column_values(&changes, 1),
        vec![json!("held"), json!("seven")],
        "after the lock, a later read holds the change shipped meanwhile"
    );
}

#[test]
fn an_answer_over_8_mib_is_ended_at_the_bytes_limit() {
    let (own, _db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into big (id, body) select g, repeat('x', 100000) from generate_series(1, 100) g",
    );
    for (extra, fits) in [
        (json!({}), false),
        (json!({ "values": "text" }), false),
        (json!({ "limit": 50 }), true),
    ] {
        let (response, answers) =
            run_for(&own, &alice(), APP, &[public("user", "big", &extra)], 60);
        let context = format!("100 changes of 100,000 bytes each, read with {extra}");
        if fits {
            let changes = ok_changes(&answers[0], &context);
            assert_eq!(changes.len(), 50, "{context}");
            assert!(
                changes
                    .iter()
                    .all(|c| c["new"][1].as_str().map(str::len) == Some(100_000)),
                "{context}: every body whole"
            );
            assert_history_entry(
                &the_entry(&own, &response),
                &response.request_id(),
                "alice",
                APP,
                &ReadEntry::allowed("user", "public", "big", 50),
                &context,
            );
        } else {
            assert_limit(&answers[0], "bytes", &context);
            assert_history_entry(
                &the_entry(&own, &response),
                &response.request_id(),
                "alice",
                APP,
                &ReadEntry::failed("user", "public", "big", "bytes"),
                &context,
            );
        }
    }
}

#[test]
fn a_read_when_the_own_database_cannot_be_reached_is_unavailable() {
    let (own, db) = reader();
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into things (id, title) values (7, 'seven')",
    );
    let call = public("user", "things", &json!({ "key": { "id": 7 } }));
    read_ok(&own, APP, &alice(), &call);
    let instance = db.instance();
    instance.batch(&format!(
        "ALTER DATABASE {} WITH ALLOW_CONNECTIONS false",
        ident(&db.name)
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left: i64 = instance
            .value(&format!(
                "SELECT count(pg_terminate_backend(pid))::text FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()",
                literal(&db.name)
            ))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if left == 0 || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let (response, answers) = run_for(&own, &alice(), APP, std::slice::from_ref(&call), 40);
    instance.batch(&format!(
        "ALTER DATABASE {} WITH ALLOW_CONNECTIONS true",
        ident(&db.name)
    ));
    assert_eq!(
        answers[0],
        json!({ "error": { "kind": "unavailable" } }),
        "a read while the own database refuses every connection"
    );
    assert_history_entry(
        &the_entry(&own, &response),
        &response.request_id(),
        "alice",
        APP,
        &ReadEntry::failed("user", "public", "things", "unavailable"),
        "the entry of the read that could not reach the own database",
    );
    // Reachable again: a later read is answered.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, answer) = read(&own, APP, &alice(), &call);
        if answer["ok"]["changes"].as_array().map(Vec::len) == Some(1) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected a read answered within 10 s of the own database being reachable again, last got {answer}"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

// ---------------------------------------------------------------------------
// 11. The Operation log.
// ---------------------------------------------------------------------------

#[test]
fn each_read_has_one_entry_naming_the_table_as_given_and_no_value() {
    let (own, _db) = reader();
    let marker = format!("marker-{}", blackbox::shared_database::token());
    write(
        &own,
        APP,
        &alice(),
        "user",
        &format!(
            "insert into counters (name, n) values ({}, 1)",
            literal(&marker)
        ),
    );
    write(
        &own,
        APP,
        &alice(),
        "user",
        "insert into inventory.stock (sku, warehouse, qty) values ('sku-1', 'north', 5), ('sku-2', 'north', 6)",
    );
    // (user, actor, call, statement's schema and table, changes answered)
    let cases: Vec<(User, &str, Value, &str, &str, u64)> = vec![
        (
            alice(),
            "user",
            public(
                "user",
                "counters",
                &json!({ "key": { "name": marker }, "limit": 37, "before": "987654321" }),
            ),
            "public",
            "counters",
            1,
        ),
        (
            bob(),
            "application",
            history_call(
                "application",
                "inventory",
                "stock",
                &json!({ "values": "text" }),
            ),
            "inventory",
            "stock",
            2,
        ),
        (
            alice(),
            "user",
            history_with(&json!({ "own": true, "table": "counters" })),
            "public",
            "counters",
            1,
        ),
    ];
    for (user, actor, call, schema, table, rows) in cases {
        let (response, changes, _) = read_ok(&own, APP, &user, &call);
        assert_eq!(u64::try_from(changes.len()).unwrap(), rows, "{call}");
        let entry = the_entry(&own, &response);
        assert_history_entry(
            &entry,
            &response.request_id(),
            &user.id,
            APP,
            &ReadEntry::allowed(actor, schema, table, rows),
            &format!("the entry of {call}"),
        );
        let text = entry.to_string();
        for value in [marker.as_str(), "987654321"] {
            assert!(
                !text.contains(value),
                "expected no value of the call in its entry ({value:?}): {entry}"
            );
        }
    }
    // A write and a read in one request: two entries, the shipment none.
    let (response, answers) = run_for(
        &own,
        &alice(),
        APP,
        &[
            blackbox::own_database::own_execute(
                "user",
                "insert into things (id, title) values (7, 'seven')",
            ),
            public("user", "things", &json!({})),
        ],
        20,
    );
    assert_eq!(ok_changes(&answers[1], "the read after the write").len(), 1);
    let entries = own.entries_of(&response);
    let operations: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["operation"].as_str())
        .collect();
    assert_eq!(
        operations,
        vec!["execute", "history"],
        "expected one entry for the write and one for the read, none for any shipment:\n{}",
        lines(&entries)
    );
    // Three reads in one request: three entries.
    let call = public("user", "things", &json!({}));
    let (response, _) = run_for(&own, &alice(), APP, &[call.clone(), call.clone(), call], 20);
    let entries = own.entries_of(&response);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["operation"] == "history")
            .count(),
        3,
        "expected one entry per read:\n{}",
        lines(&entries)
    );
}
