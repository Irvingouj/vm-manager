//! Operations that fail without being refused (CONTRACT.md, "The SDK:
//! Refusals and failures"): through Drizzle they reject with
//! `OperationFailed`, an `Error` that Application code can check with
//! `instanceof`, whose `kind` is the host's `error.kind`, `code` the
//! SQLSTATE when `kind` is `database`, and `detail` the host's whole error
//! object. Escaping `fetch`, it is the program's fixed `500`.
//!
//! The three kinds shown: a unique violation in the database, a statement
//! the statement check refuses (an `update` whose `sql` fragment calls
//! `set_config`, which Drizzle sends as a non-select), and the row limit.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::scenario::{Expected, assert_entry, lines};
use blackbox::sdk::{Sdk, ann};
use serde_json::{Value, json};

/// Fails unless `caught` is an `OperationFailed` (and an `Error`, and not a
/// `Refused`) of this kind and code, whose `detail` is an object with this
/// `kind`. Returns the detail.
fn assert_failed(caught: &Value, kind: &str, code: Option<&str>, context: &str) -> Value {
    assert_eq!(
        caught["isOperationFailed"],
        json!(true),
        "{context}: expected `error instanceof OperationFailed` to be true in Application code, got {caught}"
    );
    assert_eq!(
        caught["isError"],
        json!(true),
        "{context}: expected an OperationFailed to be an Error, got {caught}"
    );
    assert_eq!(
        caught["isRefused"],
        json!(false),
        "{context}: a failure is not a Refused, got {caught}"
    );
    assert_eq!(
        caught["kind"], kind,
        "{context}: expected error.kind {kind:?}, got {caught}"
    );
    match code {
        Some(code) => assert_eq!(
            caught["code"], code,
            "{context}: expected error.code, the SQLSTATE, {code:?}, got {caught}"
        ),
        None => assert!(
            caught["code"].is_null(),
            "{context}: expected no error.code when the kind is not `database`, got {caught}"
        ),
    }
    let detail = caught["detail"].clone();
    assert_eq!(
        detail["kind"], kind,
        "{context}: expected error.detail to be the host's whole error object, with its `kind`, got {caught}"
    );
    detail
}

/// The Operation log entries of the request this answers.
fn entries(sdk: &Sdk, response: &blackbox::Response, count: usize) -> Vec<Value> {
    let entries = sdk.entries_of(response);
    assert_eq!(
        entries.len(),
        count,
        "expected {count} Operation log entries, one per statement, got:\n{}",
        lines(&entries)
    );
    entries
}

fn a_unique_violation_rejects_with_operation_failed_as(actor: &str) {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), &format!("/fail/unique?as={actor}&tag=twice"));
    let context = format!("a unique violation as {actor}");
    let detail = assert_failed(&body["caught"], "database", Some("23505"), &context);
    assert_eq!(
        detail["code"], "23505",
        "{context}: the host's error carries the SQLSTATE: {body}"
    );
    assert!(
        detail["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{context}: the host's error carries Postgres's message: {body}"
    );
    let found = entries(&sdk, &response, 2);
    let request = response.request_id();
    assert_entry(
        &found[0],
        &Expected::own(&request, "ann", &sdk.application, actor, "execute"),
        &format!("{context}, the first insert"),
    );
    assert_entry(
        &found[1],
        &Expected {
            resource: None,
            actor: Some(actor),
            own_database: Some(true),
            operation: "execute",
            ..Expected::failed(&request, "ann", &sdk.application, "23505")
        },
        &format!("{context}, the second insert"),
    );
}

#[test]
fn a_unique_violation_as_the_user_rejects_with_operation_failed_23505() {
    a_unique_violation_rejects_with_operation_failed_as("user");
}

#[test]
fn a_unique_violation_as_the_application_rejects_with_operation_failed_23505() {
    a_unique_violation_rejects_with_operation_failed_as("application");
}

#[test]
fn a_statement_the_statement_check_refuses_rejects_with_operation_failed_of_kind_statement() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/fail/statement?as=user");
    let sql = body["statements"][0]["sql"].as_str().unwrap_or_default();
    assert!(
        sql.trim_start().to_ascii_lowercase().starts_with("update") && sql.contains("set_config"),
        "the fixture should send an update holding a call of set_config, so that this test shows what it means to: {sql}"
    );
    let detail = assert_failed(&body["caught"], "statement", None, "the statement check");
    assert_eq!(
        detail["reason"], "function-not-allowed",
        "expected the statement check's reason in error.detail: {body}"
    );
    let found = entries(&sdk, &response, 1);
    let request = response.request_id();
    assert_entry(
        &found[0],
        &Expected {
            resource: None,
            actor: Some("user"),
            own_database: Some(true),
            ..Expected::check_refused(&request, "ann", &sdk.application, "execute")
        },
        "the update the statement check refused",
    );
    let counter = sdk
        .own
        .column("SELECT n::text FROM counters WHERE name = 'hits'");
    assert_eq!(
        counter,
        vec!["0"],
        "expected the refused update to change nothing"
    );
}

#[test]
fn a_select_past_the_row_limit_rejects_with_operation_failed_of_kind_limit() {
    let sdk = Sdk::board();
    let response = sdk
        .vm()
        .request(&sdk.application, "/fail/limit?as=application")
        .as_user(&ann())
        .timeout(std::time::Duration::from_secs(60))
        .send();
    response.expect_status(200);
    let body = response.json();
    let detail = assert_failed(&body["caught"], "limit", None, "the row limit");
    assert_eq!(
        detail["limit"], "rows",
        "expected the limit's name in error.detail: {body}"
    );
    let found = entries(&sdk, &response, 1);
    let request = response.request_id();
    assert_entry(
        &found[0],
        &Expected {
            resource: None,
            actor: Some("application"),
            own_database: Some(true),
            operation: "query",
            ..Expected::failed(&request, "ann", &sdk.application, "rows")
        },
        "the select ended at the row limit",
    );
}

#[test]
fn an_operation_failed_that_escapes_fetch_is_the_fixed_500() {
    let sdk = Sdk::board();
    let response = sdk.get(&ann(), "/fail/escape?tag=escaping");
    assert_eq!(
        response.status,
        500,
        "expected an escaped OperationFailed to be the fixed `500`, got\n{}",
        response.describe()
    );
    let ordinary = sdk.get(&ann(), "/throw");
    assert_eq!(
        response.text(),
        ordinary.text(),
        "expected the same fixed body as for any uncaught error"
    );
    let text = response.text();
    assert!(
        !text.contains("23505") && !text.contains("escaping"),
        "expected the fixed body, holding nothing of the error, got {text:?}"
    );
}
