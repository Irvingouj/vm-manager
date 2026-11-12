//! When the statement runs and fails (CONTRACT.md, "Operations on a shared
//! database"): Postgres's error as `{ error: { kind: "database", code,
//! message } }`; a result over 10,000 rows or 8 MiB, or a statement
//! running longer than 10 seconds, ended as `{ error: { kind: "limit",
//! limit } }`; and the Operation log entry `failed` with the SQLSTATE or the
//! limit.
//!
//! The time limit cannot be shown in less than its 10 seconds; that test
//! runs beside the others.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, assert_database_error, assert_entry, assert_limit,
};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn ask(sql: &str) -> Value {
    json!({ "resource": RESOURCE, "sql": sql })
}

/// Changed by "Write access, and the database role as the boundary": a
/// statement that cannot be parsed used to be sent for Postgres to refuse
/// (SQLSTATE 42601); the statement check now refuses it before it is sent
/// (CONTRACT.md, "The statement check"). Still logged as `<unparsed>`.
#[test]
fn a_statement_with_a_syntax_error_is_refused_unsent_and_logged_unparsed() {
    let s = Scenario::ready();
    let marker = format!("syntax-{}", blackbox::shared_database::token());
    let (response, answer) = s.query(
        &rita(),
        "board",
        &ask(&format!("selec title, '{marker}' from incidents")),
    );
    blackbox::scenario::assert_statement_refused(&answer, "unparsed", "a syntax error");
    s.db.assert_not_received(&marker, "a statement that cannot be parsed");
    let id = response.request_id();
    assert_entry(
        &s.entry_of(&response),
        &Expected {
            statement: Some("<unparsed>"),
            ran: Some(false),
            ..Expected::failed(&id, "rita", "board", "statement")
        },
        "the entry of a statement that cannot be parsed",
    );
}

#[test]
fn errors_postgres_reports_reach_the_application_with_their_sqlstate() {
    let s = Scenario::ready();
    for (sql, code) in [
        ("select * from no_such_table", "42P01"),
        ("select no_such_column from incidents", "42703"),
        ("select 1 / 0", "22012"),
        ("select 'not a number'::int4", "22P02"),
    ] {
        let (response, answer) = s.query(&rita(), "board", &ask(sql));
        assert_database_error(&answer, code, sql);
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected::failed(&id, "rita", "board", code),
            &format!("the entry of {sql:?}"),
        );
    }
    // The program goes on serving queries.
    let (_, answer) = s.query(&rita(), "board", &ask("select 1 as one"));
    blackbox::scenario::assert_rows(&answer, &["one"], &json!([[1]]), "after the errors");
}

#[test]
fn a_result_of_more_than_10000_rows_is_ended_at_the_row_limit() {
    let s = Scenario::ready();
    let (response, answers) = s.run(
        &rita(),
        "board",
        &json!({
            "calls": [
                ask("select g from generate_series(1, 10000) g"),
                ask("select g from generate_series(1, 10001) g"),
            ],
            "summary": true,
        }),
    );
    assert_eq!(
        answers[0]["ok_summary"]["row_count"].as_u64(),
        Some(10_000),
        "expected exactly 10,000 rows to be answered, got {}",
        answers[0]
    );
    assert_limit(&answers[1], "rows", "10,001 rows");
    let id = response.request_id();
    let entries = s.entries_of(&response);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_entry(
        &entries[0],
        &Expected {
            rows: Some(10_000),
            ..Expected::allowed(&id, "rita", "board")
        },
        "10,000 rows",
    );
    assert_entry(
        &entries[1],
        &Expected::failed(&id, "rita", "board", "rows"),
        "10,001 rows",
    );
}

#[test]
fn a_result_of_more_than_8_mib_is_ended_at_the_byte_limit() {
    let s = Scenario::ready();
    let (response, answers) = s.run(
        &rita(),
        "board",
        &json!({
            "calls": [
                ask("select repeat('x', 1048576) as x from generate_series(1, 4)"),
                ask("select repeat('x', 1048576) as x from generate_series(1, 12)"),
            ],
            "summary": true,
        }),
    );
    let bytes = answers[0]["ok_summary"]["bytes"].as_u64().unwrap_or(0);
    assert!(
        bytes >= 4 * 1024 * 1024,
        "expected 4 MiB of rows to be answered, got {}",
        answers[0]
    );
    assert_limit(&answers[1], "bytes", "12 MiB of rows");
    let id = response.request_id();
    let entries = s.entries_of(&response);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_entry(
        &entries[1],
        &Expected::failed(&id, "rita", "board", "bytes"),
        "12 MiB of rows",
    );
}

#[test]
fn a_statement_running_longer_than_10_seconds_is_ended() {
    let s = Scenario::ready();
    let began = Instant::now();
    let response =
        s.vm.request("board", "/")
            .as_user(&rita())
            .method("POST")
            .json(&json!({ "calls": [ask("select pg_sleep(60) as slept")] }))
            .timeout(Duration::from_secs(40))
            .send();
    let took = began.elapsed();
    response.expect_status(200);
    let answer = &response.json()["answers"][0];
    assert_limit(answer, "time", "a statement sleeping 60 seconds");
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(20),
        "expected the statement ended at about 10 seconds, the answer came after {took:?}"
    );
    let id = response.request_id();
    assert_entry(
        &s.entry_of(&response),
        &Expected::failed(&id, "rita", "board", "time"),
        "the entry of a statement ended at the time limit",
    );
    // Ended in the database too, not left running.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let running: Vec<String> =
            s.db.running()
                .into_iter()
                .filter(|q| q.contains("pg_sleep"))
                .collect();
        if running.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the statement to be ended in the shared database, but it still runs: {running:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
