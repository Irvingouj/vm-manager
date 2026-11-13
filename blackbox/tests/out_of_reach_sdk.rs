//! `VM004` through the SDK (CONTRACT.md, "What a migration leaves behind:
//! Checked before every commit that could carry a change": "the answer is
//! `{ error: { kind: "database", code: "VM004", message } }`, from `execute`
//! or from `tx.commit` (through the SDK: `OperationFailed` of kind
//! `database` with that code)").
//!
//! `fixtures/sdk/ledger` has a migration (`0001_cascade.sql`) that leaves a
//! `BEFORE DELETE` trigger on `ca`, the child of `pa` under `ON DELETE
//! CASCADE`; each `ca` row's action switches the capture of `victim` off,
//! changes its row and switches it on again. Deleting a `pa` row through
//! Drizzle, in `db.transaction(...)` and outside one, must reject with
//! `OperationFailed` of kind `database` and code `VM004`, keep nothing, and
//! log the `commit` or the `execute` as `failed` with `VM004`. An honest
//! write afterwards is recorded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::{history_after, last_history_id};
use blackbox::left_behind::capture_catalog;
use blackbox::out_of_reach::{differences, untouched};
use blackbox::scenario::lines;
use blackbox::sdk::{Sdk, ann};
use serde_json::Value;

/// Fails unless `body` holds `{ caught }` of an `OperationFailed` of kind
/// `database` and code `VM004`.
fn assert_vm004(body: &Value, context: &str) {
    let caught = &body["caught"];
    assert!(
        caught["isOperationFailed"] == true
            && caught["kind"] == "database"
            && caught["code"] == "VM004",
        "{context}: expected the SDK to reject with OperationFailed of kind `database` and code `VM004`, got {body}"
    );
}

#[test]
fn the_cascade_attack_through_the_sdk_rejects_with_operation_failed_vm004_and_keeps_nothing() {
    let sdk = Sdk::with_shared("ledger");
    let db = &sdk.own;
    let before = untouched(db);
    let catalog = capture_catalog(db);
    for (path, operation) in [
        ("/cascade/tx?id=1", "commit"),
        ("/cascade/write?id=2", "execute"),
    ] {
        let (response, body) = sdk.get_ok(&ann(), path);
        assert_vm004(&body, path);
        let after = untouched(db);
        assert!(
            after == before,
            "{path}: expected nothing kept, but these differ:\n{}",
            differences(&before, &after)
        );
        assert_eq!(
            capture_catalog(db),
            catalog,
            "{path}: expected the capture triggers' catalog rows as before"
        );
        let entries = sdk.entries_of(&response);
        let of: Vec<&Value> = entries
            .iter()
            .filter(|e| e["operation"] == operation)
            .collect();
        assert!(
            of.len() == 1 && of[0]["outcome"] == "failed" && of[0]["error"] == "VM004",
            "{path}: expected one `{operation}` entry `failed` with `error` `VM004`, got:\n{}",
            lines(&entries)
        );
    }
    let last = last_history_id(db);
    let (_, body) = sdk.get_ok(&ann(), "/cascade/honest?id=3");
    assert_eq!(body["done"], true, "{body}");
    let rows = history_after(db, last);
    assert!(
        rows.iter().any(|r| r["table_name"] == "ca"),
        "expected the honest write after the refusals recorded, got:\n{}",
        lines(&rows)
    );
}
