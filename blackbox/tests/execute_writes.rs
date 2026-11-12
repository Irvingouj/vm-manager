//! Writing to a shared database with `execute` (CONTRACT.md, "Writing to a
//! shared database, and the role as the boundary"; ticket "Write access,
//! and the database role as the boundary", first acceptance criterion).
//!
//! With a `write` Grant and a `write` declaration, insert, update and
//! delete run in one read-write transaction as the Resource's write login
//! and are committed: the answer has `rows_affected` and what `RETURNING`
//! gave, the row is there afterwards (read from the harness's own
//! connection), and the Operation log entry says `execute` and the rows
//! affected. A statement that fails, or that a row or byte limit ends,
//! rolls back everything it did. (The time limit is in `limits_hold.rs`,
//! beside the other ten-second tests.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{
    Expected, RESOURCE, Scenario, WRITERS, assert_database_error, assert_entry, assert_executed,
    assert_limit, assert_malformed, execute_sql, lines, op, sql_on,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS])
}

/// The rows of `answer` (an `{ ok: { rows } }`), sorted by their text, for
/// statements whose `RETURNING` order Postgres does not promise.
fn sorted_rows(answer: &Value) -> Vec<Value> {
    let mut rows = answer["ok"]["rows"].as_array().cloned().unwrap_or_default();
    rows.sort_by_key(Value::to_string);
    rows
}

#[test]
fn insert_update_and_delete_succeed_and_are_there_afterwards() {
    let s = Scenario::writable();
    let title = format!("written-{}", token());
    let (response, answers) = s.run(
        &wanda(),
        "board",
        &json!({ "calls": [
            op("execute", &json!({
                "resource": RESOURCE,
                "sql": "insert into incidents (id, title, open, severity, reported_at) \
                        values (10, $1, true, 2, '2026-04-01 10:00:00+00') returning id, title",
                "params": [title],
            })),
            execute_sql("update incidents set open = false where id in (1, 10) returning id, open"),
            execute_sql("delete from incidents where id = 10"),
        ] }),
    );

    assert_executed(
        &answers[0],
        &["id", "title"],
        &json!([[10, title]]),
        1,
        "an insert with RETURNING",
    );
    assert_eq!(
        answers[1]["ok"]["columns"],
        json!(["id", "open"]),
        "an update with RETURNING: {}",
        answers[1]
    );
    assert_eq!(
        sorted_rows(&answers[1]),
        vec![json!([1, false]), json!([10, false])],
        "expected the update to return the two rows it changed, got {}",
        answers[1]
    );
    assert_eq!(
        answers[1]["ok"]["rows_affected"].as_u64(),
        Some(2),
        "an update of two rows: {}",
        answers[1]
    );
    assert_eq!(answers[1]["ok"]["row_count"].as_u64(), Some(2));
    // Without RETURNING there are no rows; rows_affected is still counted.
    assert_eq!(
        answers[2]["ok"]["rows"],
        json!([]),
        "a delete without RETURNING: {}",
        answers[2]
    );
    assert_eq!(answers[2]["ok"]["row_count"].as_u64(), Some(0));
    assert_eq!(
        answers[2]["ok"]["rows_affected"].as_u64(),
        Some(1),
        "a delete of one row: {}",
        answers[2]
    );

    // In the database, from the harness's own connection: the update and
    // the delete were committed.
    let su = s.db.superuser();
    assert_eq!(
        su.value("select string_agg(id || ':' || open, ',' order by id) from incidents")
            .as_deref(),
        Some("1:false,2:false,3:true"),
        "expected incident 1 closed and incident 10 inserted then deleted"
    );

    let id = response.request_id();
    let entries = s.entries_of(&response);
    assert_eq!(
        entries.len(),
        3,
        "expected three entries:\n{}",
        lines(&entries)
    );
    for (entry, rows) in entries.iter().zip([1, 2, 1]) {
        assert_entry(
            entry,
            &Expected::executed(&id, "wanda", "board", rows),
            "an entry of execute",
        );
    }
    let text = serde_json::to_string(&entries).unwrap();
    assert!(
        !text.contains(&title) && !text.contains("2026-04-01"),
        "expected no value of the statements or parameters in the entries, got\n{}",
        lines(&entries)
    );
}

#[test]
fn an_inserted_row_stays_after_the_request_ends() {
    let s = Scenario::writable();
    let body = format!("kept-{}", token());
    let (_, answer) = s.execute(
        &wanda(),
        "board",
        &json!({ "resource": RESOURCE, "sql": "insert into notes (body) values ($1) returning body", "params": [body] }),
    );
    assert_executed(&answer, &["body"], &json!([[body]]), 1, "an insert");
    assert_eq!(
        s.db.superuser()
            .value(&format!("select count(*) from notes where body = '{body}'"))
            .as_deref(),
        Some("1"),
        "expected the inserted row in the shared database, committed"
    );
    // And the Application reads it back with query, in another request.
    let (_, read) = s.one(
        &User::new("wanda").in_groups(&[WRITERS]),
        "board",
        &op(
            "query",
            &json!({ "resource": RESOURCE, "sql": "select body from notes where body = $1", "params": [body] }),
        ),
    );
    blackbox::scenario::assert_rows(&read, &["body"], &json!([[body]]), "reading it back");
}

#[test]
fn execute_runs_as_the_write_login_in_a_read_write_transaction() {
    let s = Scenario::writable();
    let marker = format!("as-writer-{}", token());
    let (_, answer) = s.execute(
        &wanda(),
        "board",
        &sql_on(
            RESOURCE,
            &format!(
                "insert into notes (body) values ('{marker}') \
                 returning written_by, session_login, current_setting('transaction_read_only') as read_only"
            ),
        ),
    );
    assert_executed(
        &answer,
        &["written_by", "session_login", "read_only"],
        &json!([[s.db.writer.role, s.db.writer.role, "off"]]),
        1,
        "who wrote, and in what kind of transaction",
    );
    s.db.assert_marker_came_from(&marker, &s.db.writer, "the statement of an execute");
}

#[test]
fn a_statement_that_fails_rolls_back_everything_it_did() {
    let s = Scenario::writable();
    let before = s.db.fingerprint();
    let cases = [
        // Rows 1 and 2 are updated before row 3 divides by zero.
        (
            "update incidents set severity = severity + 1 returning 10 / (3 - id) as q",
            "22012",
        ),
        // The first row goes in before the second collides.
        (
            "insert into incidents values (11, 'first', true, 1, now()), (1, 'collides', true, 1, now())",
            "23505",
        ),
        ("insert into notes (body) values ('a'), (null)", "23502"),
        // IT's function writes with its owner's rights, then the
        // statement fails.
        (
            "with c as (select close_every_incident() as n) \
             insert into notes (body) select (1 / (n - n))::text from c",
            "22012",
        ),
        ("insert into staff_only values ('not allowed')", "42501"),
    ];
    let calls: Vec<Value> = cases.iter().map(|(sql, _)| execute_sql(sql)).collect();
    let (response, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
    for ((sql, code), answer) in cases.iter().zip(&answers) {
        assert_database_error(answer, code, sql);
    }
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected every statement that failed to be rolled back, leaving the shared database as it was"
    );
    let id = response.request_id();
    let entries = s.entries_of(&response);
    assert_eq!(entries.len(), cases.len(), "{}", lines(&entries));
    for ((sql, code), entry) in cases.iter().zip(&entries) {
        assert_entry(
            entry,
            &Expected {
                operation: "execute",
                ..Expected::failed(&id, "wanda", "board", code)
            },
            &format!("the entry of {sql:?}"),
        );
    }
    // The connection is fit for the next write.
    let (_, answer) = s.execute(
        &wanda(),
        "board",
        &sql_on(
            RESOURCE,
            "insert into notes (body) values ('after') returning body",
        ),
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([["after"]]),
        1,
        "after the failures",
    );
}

#[test]
fn a_statement_ended_at_the_row_or_byte_limit_rolls_back() {
    let s = Scenario::writable();
    let before = s.db.fingerprint();
    let (response, answers) = s.run(
        &wanda(),
        "board",
        &json!({
            "calls": [
                execute_sql("insert into notes (body) select 'n' || g from generate_series(1, 10001) g returning id"),
                execute_sql("insert into notes (body) select 'big' from generate_series(1, 12) returning repeat('x', 1048576) as big"),
            ],
            "summary": true,
        }),
    );
    assert_limit(&answers[0], "rows", "an insert returning 10,001 rows");
    assert_limit(&answers[1], "bytes", "an insert returning 12 MiB");
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected the writes of statements ended at a limit rolled back"
    );
    let id = response.request_id();
    let entries = s.entries_of(&response);
    assert_eq!(entries.len(), 2, "{}", lines(&entries));
    for (entry, limit) in entries.iter().zip(["rows", "bytes"]) {
        assert_entry(
            entry,
            &Expected {
                operation: "execute",
                ..Expected::failed(&id, "wanda", "board", limit)
            },
            &format!("the entry of an execute ended at the {limit} limit"),
        );
    }
    // Exactly 10,000 rows returned is within the limit, and committed.
    let (_, answers) = s.run(
        &wanda(),
        "board",
        &json!({
            "calls": [execute_sql("insert into notes (body) select 'm' || g from generate_series(1, 10000) g returning id")],
            "summary": true,
        }),
    );
    assert_eq!(
        answers[0]["ok_summary"]["row_count"].as_u64(),
        Some(10_000),
        "expected 10,000 rows returned, got {}",
        answers[0]
    );
    assert_eq!(
        answers[0]["ok_summary"]["rows_affected"].as_u64(),
        Some(10_000)
    );
    assert_eq!(
        s.db.superuser()
            .value("select count(*) from notes where body like 'm%'")
            .as_deref(),
        Some("10000")
    );
}

#[test]
fn malformed_arguments_to_execute_reach_no_database_and_have_no_entry() {
    let s = Scenario::writable();
    let marker = format!("malformed-{}", token());
    let sql = format!("insert into notes (body) values ('{marker}')");
    let cases = [
        json!("not an object"),
        json!(7),
        json!(null),
        json!([RESOURCE, sql]),
        json!({ "sql": sql }),
        json!({ "resource": RESOURCE }),
        json!({ "resource": 7, "sql": sql }),
        json!({ "resource": RESOURCE, "sql": ["select 1"] }),
        json!({ "resource": RESOURCE, "sql": sql, "params": "x" }),
        json!({ "resource": RESOURCE, "sql": format!("{sql} returning $1"), "params": [{ "a": 1 }] }),
        json!({ "resource": RESOURCE, "sql": format!("{sql} returning $1"), "params": [[1]] }),
    ];
    let calls: Vec<Value> = cases.iter().map(|args| op("execute", args)).collect();
    let (response, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
    for (args, answer) in cases.iter().zip(&answers) {
        assert_malformed(answer, &format!("execute with {args}"));
    }
    s.db.assert_not_received(&marker, "malformed calls of execute");
    let entries = s.entries_of(&response);
    assert!(
        entries.is_empty(),
        "expected no Operation log entry for a malformed operation, got\n{}",
        lines(&entries)
    );
}
