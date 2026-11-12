//! Refusals through the SDK (CONTRACT.md, "The SDK: Refusals and failures"
//! and "What Application code is given"):
//!
//! - an operation the host refuses, made through Drizzle, rejects with
//!   `Refused`: `error instanceof Refused` in Application code, with
//!   `refusal` as the host gave it, `fix`, `name` `"Refused"` and `message`
//!   `"<reason>: <fix>"`; for `no-grant`, and, forced past the types with a
//!   cast, for `not-declared`, `declared-lower` and `not-own-database`;
//! - `new Refused(...)` at run time throws a `TypeError`;
//! - a `Refused` that escapes `fetch` is the `403` with `{ refusal, fix }`;
//!   any other error is still the fixed `500`;
//! - a handle kept and used after its request is over is refused by the
//!   host with `no-request`, or its code simply never runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::records::{application_of, event, request_of};
use blackbox::scenario::{CONTACT, Expected, RESOURCE, assert_entry, lines};
use blackbox::sdk::{LEFTOVER, Sdk, ann, nobody};
use serde_json::{Value, json};

/// Fails unless `caught` (the fixture's description of a caught error) is a
/// `Refused` carrying exactly `refusal`, a `fix` of at least a few words,
/// `name` `Refused` and `message` `"<reason>: <fix>"`. Returns the fix.
fn assert_refused(caught: &Value, refusal: &Value, context: &str) -> String {
    assert_eq!(
        caught["isRefused"],
        json!(true),
        "{context}: expected `error instanceof Refused` to be true in Application code, got {caught}"
    );
    assert_eq!(
        caught["isError"],
        json!(true),
        "{context}: expected a Refused to be an Error, got {caught}"
    );
    assert_eq!(
        caught["isOperationFailed"],
        json!(false),
        "{context}: a refusal is not an OperationFailed, got {caught}"
    );
    assert_eq!(
        caught["name"], "Refused",
        "{context}: expected error.name \"Refused\", got {caught}"
    );
    assert_eq!(
        &caught["refusal"], refusal,
        "{context}: expected error.refusal to be the refusal as the host gave it, got {caught}"
    );
    let fix = caught["fix"].as_str().unwrap_or_default().to_owned();
    assert!(
        fix.trim().len() >= 10,
        "{context}: expected error.fix, a sentence a Coding agent can act on, got {caught}"
    );
    let reason = refusal["reason"].as_str().unwrap_or_default();
    assert_eq!(
        caught["message"].as_str(),
        Some(format!("{reason}: {fix}").as_str()),
        "{context}: expected error.message \"<reason>: <fix>\", got {caught}"
    );
    fix
}

/// The one Operation log entry of the request this answers, which must be
/// refused with this reason, on this Resource, as this actor.
fn assert_refused_entry(
    sdk: &Sdk,
    response: &blackbox::Response,
    user: &str,
    resource: &str,
    actor: &str,
    operation: &str,
    reason: &str,
) {
    let entries = sdk.entries_of(response);
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one Operation log entry for the refused statement, got:\n{}",
        lines(&entries)
    );
    let request = response.request_id();
    let want = Expected {
        resource: Some(resource),
        actor: Some(actor),
        own_database: Some(false),
        operation,
        ..Expected::refused(&request, user, &sdk.application, reason)
    };
    assert_entry(&entries[0], &want, &format!("the refused {operation}"));
}

#[test]
fn a_select_through_drizzle_by_a_user_without_a_grant_rejects_with_refused_no_grant() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&nobody(), "/refuse/no-grant");
    let fix = assert_refused(
        &body["caught"],
        &json!({
            "reason": "no-grant",
            "resource": RESOURCE,
            "user": "nobody",
            "needed": "read",
            "held": null,
            "contact": CONTACT,
        }),
        "no-grant",
    );
    assert!(
        fix.contains(CONTACT),
        "expected the fix for no-grant to name the Resource's contact {CONTACT:?}, got {fix:?}"
    );
    assert_refused_entry(
        &sdk, &response, "nobody", RESOURCE, "user", "query", "no-grant",
    );
}

#[test]
fn a_resource_not_in_the_manifest_forced_past_the_types_is_refused_not_declared() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/refuse/not-declared");
    assert_refused(
        &body["caught"],
        &json!({ "reason": "not-declared", "resource": "payroll" }),
        "not-declared",
    );
    assert_refused_entry(
        &sdk,
        &response,
        "ann",
        "payroll",
        "user",
        "query",
        "not-declared",
    );
}

#[test]
fn an_insert_through_a_read_declaration_forced_past_the_types_is_refused_declared_lower() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/refuse/declared-lower");
    assert_refused(
        &body["caught"],
        &json!({ "reason": "declared-lower", "resource": RESOURCE, "declared": "read", "needed": "write" }),
        "declared-lower",
    );
    assert_refused_entry(
        &sdk,
        &response,
        "ann",
        RESOURCE,
        "user",
        "execute",
        "declared-lower",
    );
}

#[test]
fn the_application_on_a_shared_resource_forced_past_the_types_is_refused_not_own_database() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/refuse/not-own-database");
    assert_refused(
        &body["caught"],
        &json!({ "reason": "not-own-database", "resource": RESOURCE }),
        "not-own-database",
    );
    assert_refused_entry(
        &sdk,
        &response,
        "ann",
        RESOURCE,
        "application",
        "query",
        "not-own-database",
    );
}

#[test]
fn a_refused_that_escapes_fetch_is_a_403_with_the_refusal_and_the_fix() {
    let sdk = Sdk::board();
    let response = sdk.get(&nobody(), "/refuse/escape");
    assert_eq!(
        response.status,
        403,
        "expected an escaped Refused to become `403`, got\n{}",
        response.describe()
    );
    let content_type = response.header("content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "expected content-type application/json, got {content_type:?}\n{}",
        response.describe()
    );
    let body = response.json();
    let keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        {
            let mut k = keys.clone();
            k.sort_unstable();
            k
        },
        vec!["fix", "refusal"],
        "expected the body to be exactly {{ refusal, fix }}, got {body}"
    );
    assert_eq!(
        body["refusal"],
        json!({
            "reason": "no-grant",
            "resource": RESOURCE,
            "user": "nobody",
            "needed": "read",
            "held": null,
            "contact": CONTACT,
        }),
        "expected the refusal as the host gave it, got {body}"
    );
    assert!(
        body["fix"].as_str().is_some_and(|f| f.contains(CONTACT)),
        "expected the fix naming the contact, got {body}"
    );
}

#[test]
fn application_code_cannot_make_a_refused_at_run_time() {
    let sdk = Sdk::board();
    let (_, body) = sdk.get_ok(&ann(), "/refuse/construct");
    assert_eq!(
        body["constructed"],
        json!(false),
        "expected `new Refused(...)` to throw in Application code, got {body}"
    );
    assert_eq!(
        body["caught"]["isTypeError"],
        json!(true),
        "expected `new Refused(...)` to throw a TypeError, got {body}"
    );
}

#[test]
fn any_other_error_that_escapes_fetch_is_still_the_fixed_500() {
    let sdk = Sdk::board();
    let response = sdk.get(&ann(), "/throw");
    assert_eq!(
        response.status,
        500,
        "expected the fixed `500` for an ordinary Error, got\n{}",
        response.describe()
    );
    let text = response.text();
    assert!(
        !text.contains("marker-uncaught-7f3a") && !text.contains("refusal"),
        "expected the fixed body, holding neither the error's message nor a refusal, got {text:?}"
    );
    let other = sdk.get(&ann(), "/throw");
    assert_eq!(
        other.text(),
        text,
        "expected the same fixed body every time"
    );
}

/// Every Operation log entry of `application` with no request: each must
/// be refused with `no-request`. Returns them.
fn entries_without_request(sdk: &Sdk) -> Vec<Value> {
    let entries: Vec<Value> = sdk
        .log()
        .into_iter()
        .filter(|e| e["application"] == sdk.application.as_str() && e["request"].is_null())
        .collect();
    for entry in &entries {
        assert!(
            entry["outcome"] == "refused" && entry["reason"] == "no-request",
            "expected every operation of {} with no request in flight to be refused with no-request, got {entry}",
            sdk.application
        );
    }
    entries
}

#[test]
fn handles_used_after_the_response_are_refused_with_no_request() {
    let sdk = Sdk::only("leftover");
    let (response, body) = sdk.get_ok(&ann(), "/after");
    assert_eq!(body, json!({ "left": true }));
    // Long enough for every timer the fixture arms (the last at 300 ms), if
    // it ever runs. Code left running may also never run.
    std::thread::sleep(Duration::from_millis(1500));
    let late: Vec<Value> = sdk
        .vm()
        .records()
        .into_iter()
        .filter(|r| {
            event(r) == "printed"
                && application_of(r) == Some(LEFTOVER)
                && r["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("LATE "))
        })
        .collect();
    for record in &late {
        assert!(
            request_of(record).is_none(),
            "a print from code left running after its request must carry no request: {record}"
        );
        let message = record["message"].as_str().unwrap_or_default();
        let said: Value =
            serde_json::from_str(message.trim_start_matches("LATE ")).unwrap_or_else(|e| {
                panic!("the fixture printed something that is not JSON ({e}): {message}")
            });
        for actor in ["asUser", "asApplication"] {
            let caught = &said[actor]["caught"];
            assert!(
                caught["isRefused"] == json!(true)
                    && caught["refusal"] == json!({ "reason": "no-request" }),
                "expected the kept handle ({actor}) used after the response to reject with Refused no-request, got {said}"
            );
        }
    }
    entries_without_request(&sdk);
    let allowed_after: Vec<Value> = sdk
        .log()
        .into_iter()
        .filter(|e| {
            e["application"] == LEFTOVER
                && e["outcome"] == "allowed"
                && e["request"].as_str() != Some(response.request_id().as_str())
        })
        .collect();
    assert!(
        allowed_after.is_empty(),
        "expected no allowed operation outside the request:\n{}",
        lines(&allowed_after)
    );
    let notes = sdk.own.column("SELECT body FROM notes ORDER BY id");
    assert_eq!(
        notes,
        vec!["seeded"],
        "expected nothing written by the handles kept after the response"
    );
}
