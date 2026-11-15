//! The Version's new field `structures` (CONTRACT.md, "Deploying: On the
//! platform's API"): `POST /v1/applications/<name>/versions` takes,
//! optionally, `"structures": { "<resource>": "sha256:<64 hex>" }`, the
//! fingerprint of each shared structure the Version was built against.
//! Every key must be a Resource the Version declares and every value a
//! fingerprint in that form, else `400 malformed`; left out, it is an empty
//! object; the Version's record holds it, as sent, for ever. The order of
//! the API's errors ("The platform's API") holds with it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::{marked_bundle, version_body_declaring};
use blackbox::own_database::{Migration, migrations_json};
use blackbox::{Api, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// A fingerprint made of 64 times this hex digit.
fn fp(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

/// A Version of `board` declaring `devops` at `read` and `inventory` at
/// `write`, with these `structures` (left out when `None`).
fn body(structures: Option<Value>) -> Value {
    let mut body = version_body_declaring(
        &marked_bundle("structures"),
        &[("devops", "read"), ("inventory", "write")],
    );
    if let Some(structures) = structures {
        body["structures"] = structures;
    }
    body
}

/// `board` created by alice, with one Version that has no `structures`.
fn board(vm: &Vm) -> Api<'_> {
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    api.send_version("board", &body(None)).expect_status(201);
    api
}

#[test]
fn structures_are_recorded_as_sent_and_answered_for_ever() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let structures = json!({ "devops": fp('a'), "inventory": fp('0') });
    let sent = api.send_version("board", &body(Some(structures.clone())));
    sent.expect_status(201);
    assert_eq!(
        sent.json()["structures"],
        structures,
        "expected the 201's Version record to hold `structures` as sent"
    );
    assert_eq!(
        api.get_version("board", 1).expect_status(200).json()["structures"],
        structures
    );

    // A later Version, which leaves it out: an empty object, and the first
    // is as it was.
    let later = api.send_version("board", &body(None));
    later.expect_status(201);
    assert_eq!(
        later.json()["structures"],
        json!({}),
        "expected `structures` left out to be an empty object"
    );
    assert_eq!(
        api.get_version("board", 2).expect_status(200).json()["structures"],
        json!({})
    );
    assert_eq!(
        api.get_version("board", 1).expect_status(200).json()["structures"],
        structures,
        "expected Version 1's `structures` as sent, whatever is sent later"
    );
    let listed = api.versions("board");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["structures"], structures);
    assert_eq!(listed[1]["structures"], json!({}));

    // After a restart, the same.
    let vm = vm.restart();
    let api = vm.api_as(&alice());
    assert_eq!(
        api.get_version("board", 1).expect_status(200).json()["structures"],
        structures
    );
}

#[test]
fn some_of_the_declared_resources_or_none_and_an_empty_object_are_taken() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    for structures in [json!({ "devops": fp('f') }), json!({})] {
        let sent = api.send_version("board", &body(Some(structures.clone())));
        sent.expect_status(201);
        assert_eq!(sent.json()["structures"], structures);
    }
    // A Resource declared at `structure` may have its fingerprint too.
    let mut at_structure = version_body_declaring(&marked_bundle("s"), &[("catalog", "structure")]);
    at_structure["structures"] = json!({ "catalog": fp('9') });
    api.send_version("board", &at_structure).expect_status(201);
}

/// Bodies whose `structures` is malformed, each with what is wrong.
fn malformed() -> Vec<(&'static str, Value)> {
    let hex64 = "a".repeat(64);
    vec![
        (
            "a key the Version does not declare",
            json!({ "devops": fp('a'), "elsewhere": fp('b') }),
        ),
        (
            "only a key it does not declare",
            json!({ "payroll": fp('a') }),
        ),
        ("a value with no `sha256:`", json!({ "devops": hex64 })),
        (
            "a value of another digest",
            json!({ "devops": format!("sha1:{}", "a".repeat(40)) }),
        ),
        (
            "a value of 63 hex digits",
            json!({ "devops": format!("sha256:{}", "a".repeat(63)) }),
        ),
        (
            "a value of 65 hex digits",
            json!({ "devops": format!("sha256:{}", "a".repeat(65)) }),
        ),
        (
            "a value with a digit that is not hex",
            json!({ "devops": format!("sha256:{}g", "a".repeat(63)) }),
        ),
        ("an empty value", json!({ "devops": "" })),
        ("a number for a value", json!({ "devops": 7 })),
        ("null for a value", json!({ "devops": null })),
        (
            "an object for a value",
            json!({ "devops": { "fp": fp('a') } }),
        ),
        ("an array for a value", json!({ "devops": [fp('a')] })),
        ("an array", json!([fp('a')])),
        ("a string", json!(fp('a'))),
        ("a number", json!(1)),
        ("a boolean", json!(true)),
    ]
}

#[test]
fn malformed_structures_are_400_malformed_and_nothing_is_kept() {
    let vm = vm();
    let api = board(&vm);
    let before = (api.application("board"), api.versions("board"));
    for (case, structures) in malformed() {
        let answer = api.send_version("board", &body(Some(structures)));
        assert!(
            answer.status == 400,
            "expected `400 malformed` for `structures` with {case}, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    // A key given twice.
    let text = format!(
        "{{\"bundle\":\"x\",\"declared_access\":{{\"resources\":{{\"devops\":{{\"kind\":\"postgres\",\"access\":\"read\"}}}}}},\"git_commit\":\"{}\",\"structures\":{{\"devops\":\"{}\",\"devops\":\"{}\"}}}}",
        blackbox::api::SOME_GIT_COMMIT,
        fp('a'),
        fp('b')
    );
    let answer = api.post_raw("/v1/applications/board/versions", text.as_bytes());
    assert_api_error(&answer, 400, "malformed");
    assert_eq!(
        (api.application("board"), api.versions("board")),
        before,
        "expected nothing kept: the Application and its Versions as they were"
    );
}

#[test]
fn not_a_contributor_and_not_found_come_before_malformed_structures() {
    let vm = vm();
    let _api = board(&vm);
    let bad = body(Some(json!({ "payroll": "nope" })));
    // bob is not a Contributor of board: he learns nothing of his body.
    let answer = vm.api_as(&bob()).send_version("board", &bad);
    assert_api_error(&answer, 403, "not-a-contributor");
    // No such Application.
    let answer = vm.api_as(&alice()).send_version("no-such-board", &bad);
    assert_api_error(&answer, 404, "not-found");
}

#[test]
fn malformed_structures_come_before_migrations_diverged() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let first = Migration::new("0000_first", &["CREATE TABLE first_table (id integer);"]);
    let mut v1 = body(None);
    v1["migrations"] = migrations_json(std::slice::from_ref(&first));
    api.send_version("board", &v1).expect_status(201);

    // Migrations that do not continue Version 1's, and malformed
    // `structures`: malformed first.
    let other = Migration::new("0000_other", &["CREATE TABLE other_table (id integer);"]);
    let mut diverging = body(Some(json!({ "devops": "sha256:short" })));
    diverging["migrations"] = migrations_json(std::slice::from_ref(&other));
    assert_api_error(&api.send_version("board", &diverging), 400, "malformed");
    // The control: well-formed `structures`, the same migrations: diverged.
    diverging["structures"] = json!({ "devops": fp('c') });
    assert_api_error(
        &api.send_version("board", &diverging),
        409,
        "migrations-diverged",
    );
    assert_eq!(api.versions("board").len(), 1);
}
