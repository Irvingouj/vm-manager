//! What the API's refusals of a Version's migrations say about a serving
//! Version (CONTRACT.md, "Deploying: On the platform's API"): the messages
//! of `migration-failed` and `migration-refused` say that a Version still
//! serves only when one does; for an Application with no Version serving
//! they say that nothing was applied and name no Version.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::Response;
use blackbox::assert_api_error;
use blackbox::own_database::{Migration, Own};

/// A migration that parses and fails when run.
fn failing() -> Migration {
    Migration::new(
        "0001_into_nowhere",
        &["INSERT INTO \"nowhere\" VALUES (1);"],
    )
}

/// A migration that breaks the platform's rule `transaction-control`.
fn refused() -> Migration {
    Migration::new("0001_commit_early", &["COMMIT;"])
}

fn first_tables() -> Migration {
    Migration::new("0000_first", &["CREATE TABLE \"kept\" (\"id\" integer);"])
}

/// The answer's `error.message`, the answer being this refusal.
fn refusal_message(answer: &Response, reason: &str) -> String {
    assert_api_error(answer, 422, reason);
    answer.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Fails unless the message says nothing was applied and names no Version
/// that serves.
fn assert_says_no_version_serves(message: &str, context: &str) {
    let lower = message.to_lowercase();
    assert!(
        lower.contains("nothing"),
        "{context}: expected the message to say nothing was applied, got {message:?}"
    );
    for never in ["still serves", "goes on serving", "previous version"] {
        assert!(
            !lower.contains(never),
            "{context}: with no Version serving, expected no word that a Version serves ({never:?}), got {message:?}"
        );
    }
}

/// Fails unless the message says a Version still serves.
fn assert_says_a_version_serves(message: &str, context: &str) {
    let lower = message.to_lowercase();
    assert!(
        lower.contains("serves") || lower.contains("serving"),
        "{context}: with a Version serving, expected the message to say it still serves, got {message:?}"
    );
}

#[test]
fn a_first_versions_refusals_say_nothing_was_applied_and_name_no_serving_version() {
    let own = Own::start();
    own.create("first-failing");
    let message = refusal_message(
        &own.send("first-failing", &[first_tables(), failing()]),
        "migration-failed",
    );
    assert_says_no_version_serves(&message, "migration-failed, first Version");

    own.create("first-refused");
    let message = refusal_message(
        &own.send("first-refused", &[first_tables(), refused()]),
        "migration-refused",
    );
    assert_says_no_version_serves(&message, "migration-refused, first Version");
}

#[test]
fn with_a_version_serving_the_refusals_say_it_still_serves() {
    let own = Own::start();
    own.install("serving-board", &[first_tables()]);
    let message = refusal_message(
        &own.send("serving-board", &[first_tables(), failing()]),
        "migration-failed",
    );
    assert_says_a_version_serves(&message, "migration-failed, a Version serving");
    let message = refusal_message(
        &own.send("serving-board", &[first_tables(), refused()]),
        "migration-refused",
    );
    assert_says_a_version_serves(&message, "migration-refused, a Version serving");
}
