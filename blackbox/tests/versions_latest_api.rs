//! The latest Version, and the list of Versions (CONTRACT.md, "Versions,
//! going back, and logs: The latest Version, and the list of Versions"):
//! the Application's object names its highest-numbered Version, which a
//! go-back leaves above the serving one; the list of Versions is bounded
//! and takes `?limit=<n>`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::marked_bundle;
use blackbox::logs::{from_to, go_back};
use blackbox::{User, Vm, VmOptions, assert_api_error};
use serde_json::Value;

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice")
}

fn numbers(versions: &Value) -> Vec<u64> {
    versions["versions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected {{ versions: [...] }}, got {versions}"))
        .iter()
        .map(|v| v["number"].as_u64().unwrap())
        .collect()
}

#[test]
fn latest_version_is_null_before_a_version_the_highest_number_after_and_above_serving_after_a_go_back()
 {
    let vm = vm();
    let api = vm.api_as(&alice());
    let created = api.create_application("board");
    created.expect_status(201);
    let created = created.json();
    assert!(
        created.get("latest_version").is_some_and(Value::is_null),
        "expected `latest_version` null in the created Application, got {created}"
    );
    assert!(
        api.application("board")
            .get("latest_version")
            .is_some_and(Value::is_null),
        "expected `latest_version` null with no Version"
    );
    for n in 1..=3 {
        api.send_bundle("board", &marked_bundle(&format!("v{n}")))
            .expect_status(201);
        assert_eq!(api.application("board")["latest_version"], n);
    }
    go_back(&api, "board", &from_to(3, Some(1))).expect_status(200);
    let after = api.application("board");
    assert_eq!(after["latest_version"], 3, "{after}");
    assert_eq!(after["serving_version"], 1, "{after}");
    api.send_bundle("board", &marked_bundle("v4"))
        .expect_status(201);
    let after = api.application("board");
    assert_eq!(after["latest_version"], 4, "{after}");
    assert_eq!(after["serving_version"], 4, "{after}");
    // Someone it is shared with reads the same Versions (CONTRACT.md,
    // "Contributors and sharing": only who can open it reads it; to them
    // `contributor` is false and `shared_with` null).
    api.share_with_everyone("board");
    let read = vm.api_as(&User::new("bob")).application("board");
    assert_eq!(read["latest_version"], 4, "{read}");
    assert_eq!(read["serving_version"], 4, "{read}");
    assert_eq!(read["contributor"], false, "{read}");
    assert!(read["shared_with"].is_null(), "{read}");
}

#[test]
fn the_list_of_versions_with_limit_is_the_newest_n_oldest_first() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    for n in 1..=3 {
        api.send_bundle("board", &marked_bundle(&format!("v{n}")))
            .expect_status(201);
    }
    let list = |query: &str| {
        let answer = api.get(&format!("/v1/applications/board/versions{query}"));
        answer.expect_status(200);
        numbers(&answer.json())
    };
    assert_eq!(list("?limit=2"), [2, 3]);
    assert_eq!(list("?limit=1"), [3]);
    assert_eq!(list("?limit=3"), [1, 2, 3]);
    assert_eq!(list("?limit=1000"), [1, 2, 3]);
    assert_eq!(list(""), [1, 2, 3], "without `limit`: the newest 1,000");
    assert_eq!(
        api.get("/v1/applications/board/versions?limit=2").json()["versions"],
        Value::Array(api.versions("board")[1..].to_vec()),
        "expected the same Version records as the whole list"
    );
}

#[test]
fn a_limit_outside_1_to_1000_or_given_twice_is_malformed() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    api.send_bundle("board", &marked_bundle("v1"))
        .expect_status(201);
    for query in [
        "?limit=0",
        "?limit=1001",
        "?limit=-1",
        "?limit=1.5",
        "?limit=two",
        "?limit=",
        "?limit=1&limit=1",
    ] {
        assert_api_error(
            &api.get(&format!("/v1/applications/board/versions{query}")),
            400,
            "malformed",
        );
    }
}
