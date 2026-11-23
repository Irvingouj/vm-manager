//! The API's refusals that name the fields they take (CONTRACT.md,
//! "Storage: The tool and the Dashboard", last bullet but one): the
//! refusal of a registration, and of a Version, names both kinds,
//! `"postgres" | "storage"`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use blackbox::User;
use blackbox::assert_api_error;
use blackbox::storage::Storage;
use serde_json::json;

const BOTH_KINDS: &str = "\"postgres\" | \"storage\"";

#[test]
fn a_malformed_registration_says_that_a_resource_is_a_postgres_or_a_storage() {
    let s = Storage::start();
    let refused = s.admin().create_resource(&json!({}));
    assert_api_error(&refused, 400, "malformed");
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains(BOTH_KINDS),
        "expected the form of a registration to name both kinds ({BOTH_KINDS}), got {message:?}"
    );
    // What each kind takes: `connection` for `postgres`, `location` with
    // its `backend` and `directory` for `storage`.
    for word in ["connection", "location", "backend", "directory"] {
        assert!(
            message.contains(&format!("\"{word}\"")),
            "expected the form of a registration to name {word:?}, got {message:?}"
        );
    }
}

#[test]
fn a_malformed_version_says_that_a_declared_resource_is_a_postgres_or_a_storage() {
    let s = Storage::start();
    let api = s.vm.api_as(&User::new("alice").in_groups(&["staff"]));
    api.create_application("board").expect_status(201);
    let refused = api.send_version("board", &json!({ "bundle": "x" }));
    assert_api_error(&refused, 400, "malformed");
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains(BOTH_KINDS),
        "expected the form of a Version to name both kinds ({BOTH_KINDS}), got {message:?}"
    );
}
