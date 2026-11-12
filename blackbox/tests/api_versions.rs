//! Sending Versions through the platform's API (CONTRACT.md, "The
//! platform's API: Versions"): only a Contributor may; Versions are numbered
//! 1, 2, 3 in the order they were sent; the Version's record; a body that is
//! malformed or too large; an Application that does not exist.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

use blackbox::api::{
    SOME_GIT_COMMIT, assert_json_content_type, assert_recent_rfc3339, assert_served_by,
    marked_bundle, sha256_hex, version_body,
};
use blackbox::{Api, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

/// 16 MiB, the largest body the API takes.
const LIMIT: usize = 16 * 1024 * 1024;

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// The Application's record and its list of Versions, to compare before
/// and after something that must change nothing.
fn state_of(api: &Api<'_>, name: &str) -> (Value, Vec<Value>) {
    (api.application(name), api.versions(name))
}

#[test]
fn a_contributor_sends_a_version_and_gets_201_with_its_record() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let bundle = marked_bundle("first");
    let declared_access = json!({ "resources": {} });
    let response = api.send_version(
        "board",
        &json!({
            "bundle": bundle,
            "declared_access": declared_access,
            "git_commit": SOME_GIT_COMMIT,
        }),
    );
    response.expect_status(201);
    assert_json_content_type(&response);
    let version = response.json();
    assert_eq!(version["application"], "board", "application in {version}");
    assert_eq!(version["number"], 1, "number in {version}");
    assert_eq!(
        version["declared_access"], declared_access,
        "declared_access in {version}"
    );
    assert_eq!(
        version["git_commit"], SOME_GIT_COMMIT,
        "git_commit in {version}"
    );
    assert_eq!(
        version["bundle_sha256"]
            .as_str()
            .map(str::to_ascii_lowercase),
        Some(sha256_hex(bundle.as_bytes())),
        "expected bundle_sha256 to be the SHA-256 of the bundle's UTF-8 bytes, in {version}"
    );
    assert_eq!(version["created_by"], "alice", "created_by in {version}");
    assert_recent_rfc3339(&version["created_at"], 300, "created_at");
    assert_eq!(
        api.get_version("board", 1).expect_status(200).json(),
        version,
        "expected GET of the Version to answer the object the 201 did"
    );
    assert_eq!(
        api.application("board")["serving_version"],
        1,
        "expected the Version just sent to be the serving one"
    );
}

#[test]
fn declared_access_is_kept_as_sent_for_each_kind_of_access() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let declared_access = json!({
        "resources": {
            "orders": { "kind": "postgres", "access": "read" },
            "schema-owner": { "kind": "postgres", "access": "structure" },
            "ledger": { "kind": "postgres", "access": "write" },
        }
    });
    let response = api.send_version(
        "board",
        &json!({
            "bundle": marked_bundle("access"),
            "declared_access": declared_access,
            "git_commit": "fedcba9876543210fedcba9876543210fedcba98",
        }),
    );
    response.expect_status(201);
    let version = response.json();
    assert_eq!(version["declared_access"], declared_access, "in {version}");
    assert_eq!(
        version["git_commit"],
        "fedcba9876543210fedcba9876543210fedcba98"
    );
    assert_eq!(
        api.get_version("board", 1).json()["declared_access"],
        declared_access
    );
}

#[test]
fn a_bundle_that_is_not_ascii_is_kept_and_hashed_as_utf_8() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let bundle = format!("// Zoë 数据 🚀\n{}", marked_bundle("ünïcödé-数据"));
    let version = api.create_with_bundle("board", &bundle);
    assert_eq!(
        version["bundle_sha256"]
            .as_str()
            .map(str::to_ascii_lowercase),
        Some(sha256_hex(bundle.as_bytes()))
    );
    assert_served_by(
        &vm,
        &vm.get_as(&bob(), "board", "/"),
        "board",
        "ünïcödé-数据",
        1,
    );
}

#[test]
fn versions_are_numbered_1_2_3_in_the_order_they_were_sent() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let mut sent = Vec::new();
    for n in 1..=3_u64 {
        let response = api.send_bundle("board", &marked_bundle(&format!("v{n}")));
        response.expect_status(201);
        let version = response.json();
        assert_eq!(
            version["number"], n,
            "expected Version number {n}, got {version}"
        );
        sent.push(version);
    }
    assert_eq!(
        api.versions("board"),
        sent,
        "expected the list oldest first"
    );
}

#[test]
fn versions_sent_at_once_get_distinct_numbers_listed_in_order() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    const SENDERS: usize = 8;
    let start = Barrier::new(SENDERS);
    let numbers: Vec<u64> = thread::scope(|scope| {
        let handles: Vec<_> = (0..SENDERS)
            .map(|i| {
                let (api, start) = (&api, &start);
                scope.spawn(move || {
                    let request = api
                        .request("/v1/applications/board/versions")
                        .method("POST")
                        .json(&version_body(&marked_bundle(&format!("sender-{i}"))));
                    start.wait();
                    let response = request.send();
                    response.expect_status(201);
                    response.json()["number"].as_u64().unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let distinct: BTreeSet<u64> = numbers.iter().copied().collect();
    let expected: BTreeSet<u64> = (1..=8).collect();
    assert_eq!(
        distinct, expected,
        "expected {SENDERS} Versions sent at once to be numbered 1 to {SENDERS}, each once, got {numbers:?}"
    );
    let listed: Vec<u64> = api
        .versions("board")
        .iter()
        .map(|v| v["number"].as_u64().unwrap())
        .collect();
    assert_eq!(
        listed,
        (1..=8).collect::<Vec<_>>(),
        "expected the list oldest first"
    );
}

#[test]
fn a_user_who_is_not_a_contributor_gets_403_not_a_contributor_and_nothing_changes() {
    let vm = vm();
    let owner = vm.api_as(&alice());
    owner.create_with_bundle("board", &marked_bundle("alices"));
    let before = state_of(&owner, "board");
    for intruder in [
        bob(),
        User::new("mallory").in_groups(&["alice", "board", "contributors"]),
        User::new("Alice"),
        User::new("ALICE"),
        User::new("\u{0430}lice"),
        User::new("alice\u{200b}"),
        User::new("alice,bob"),
        User::new(&format!("alice{}", " x")),
        User::new(super_long_alice().as_str()),
    ] {
        let response = vm
            .api_as(&intruder)
            .send_bundle("board", &marked_bundle("intruder"));
        assert!(
            response.status == 403,
            "expected `403 not-a-contributor` for {:?} sending a Version to alice's Application, got\n{}",
            intruder.id,
            response.describe()
        );
        assert_api_error(&response, 403, "not-a-contributor");
    }
    assert_eq!(
        state_of(&owner, "board"),
        before,
        "expected nothing to change"
    );
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "alices", 1);
}

fn super_long_alice() -> String {
    format!("alice{}", "e".repeat(251))
}

#[test]
fn a_user_who_created_another_application_is_not_a_contributor_of_this_one() {
    let vm = vm();
    vm.api_as(&alice())
        .create_with_bundle("theirs", &marked_bundle("theirs"));
    let mallory = vm.api_as(&User::new("mallory"));
    mallory.create_with_bundle("mine", &marked_bundle("mine"));
    let before = state_of(&mallory, "theirs");
    assert_api_error(
        &mallory.send_bundle("theirs", &marked_bundle("hijack")),
        403,
        "not-a-contributor",
    );
    assert_eq!(state_of(&mallory, "theirs"), before);
    // And bob's attempt to take the name does not make him a Contributor.
    assert_api_error(
        &vm.api_as(&bob()).create_application("theirs"),
        409,
        "name-taken",
    );
    assert_api_error(
        &vm.api_as(&bob())
            .send_bundle("theirs", &marked_bundle("bob")),
        403,
        "not-a-contributor",
    );
    assert_eq!(state_of(&mallory, "theirs"), before);
}

#[test]
fn sending_a_version_to_an_application_that_does_not_exist_is_404_not_found() {
    let vm = vm();
    let api = vm.api_as(&alice());
    assert_api_error(
        &api.send_bundle("nowhere", &marked_bundle("x")),
        404,
        "not-found",
    );
    assert_api_error(&api.get_application("nowhere"), 404, "not-found");
    assert_api_error(&api.list_versions("nowhere"), 404, "not-found");
    assert_api_error(&api.get_version("nowhere", 1), 404, "not-found");
}

/// Version bodies that are not the JSON asked for.
fn malformed_version_bodies() -> Vec<(&'static str, Vec<u8>)> {
    let good = || version_body("globalThis.default = { fetch() { return new Response('x'); } };");
    let with = |pointer: &str, value: Value| {
        let mut body = good();
        *body.pointer_mut(pointer).unwrap() = value;
        body.to_string().into_bytes()
    };
    let without = |key: &str| {
        let mut body = good();
        body.as_object_mut().unwrap().remove(key);
        body.to_string().into_bytes()
    };
    let resource = |value: Value| {
        let mut body = good();
        body["declared_access"]["resources"] = json!({ "orders": value });
        body.to_string().into_bytes()
    };
    vec![
        ("an empty body", Vec::new()),
        ("not JSON", b"bundle=hello".to_vec()),
        ("cut-off JSON", b"{\"bundle\": \"x\"".to_vec()),
        ("a JSON array", b"[]".to_vec()),
        ("a JSON string", b"\"globalThis.default = {}\"".to_vec()),
        ("JSON null", b"null".to_vec()),
        (
            "bytes that are not UTF-8",
            b"{\"bundle\": \"\xff\xfe\"}".to_vec(),
        ),
        ("no bundle", without("bundle")),
        ("no declared_access", without("declared_access")),
        ("no git_commit", without("git_commit")),
        ("a bundle that is a number", with("/bundle", json!(5))),
        ("a bundle that is null", with("/bundle", Value::Null)),
        (
            "a bundle that is an object",
            with("/bundle", json!({ "code": "x" })),
        ),
        (
            "declared_access that is a list",
            with("/declared_access", json!([])),
        ),
        (
            "resources that are a list",
            with("/declared_access/resources", json!([])),
        ),
        (
            "a resource of another kind",
            resource(json!({ "kind": "mysql", "access": "read" })),
        ),
        (
            "a resource with no kind",
            resource(json!({ "access": "read" })),
        ),
        (
            "an access that is not structure, read or write",
            resource(json!({ "kind": "postgres", "access": "admin" })),
        ),
        (
            "an access in upper case",
            resource(json!({ "kind": "postgres", "access": "READ" })),
        ),
        (
            "a resource with no access",
            resource(json!({ "kind": "postgres" })),
        ),
        (
            "a resource that is a string",
            resource(json!("postgres:read")),
        ),
        (
            "a git_commit of 39 hex characters",
            with(
                "/git_commit",
                json!(SOME_GIT_COMMIT.chars().take(39).collect::<String>()),
            ),
        ),
        (
            "a git_commit of 41 hex characters",
            with("/git_commit", json!(format!("{SOME_GIT_COMMIT}0"))),
        ),
        (
            "a git_commit that is not hex",
            with("/git_commit", json!("g".repeat(40))),
        ),
        (
            "a git_commit that is a number",
            with("/git_commit", json!(1234567890)),
        ),
        ("an empty git_commit", with("/git_commit", json!(""))),
        (
            "a huge number for git_commit",
            b"{\"bundle\":\"x\",\"declared_access\":{\"resources\":{}},\"git_commit\":1e999}"
                .to_vec(),
        ),
    ]
}

#[test]
fn a_malformed_version_body_is_400_malformed_and_sends_nothing() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let before = state_of(&api, "board");
    for (case, body) in malformed_version_bodies() {
        let response = api.post_raw("/v1/applications/board/versions", &body);
        assert!(
            response.status == 400,
            "expected `400 malformed` for a Version body with {case}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 400, "malformed");
    }
    assert_eq!(
        state_of(&api, "board"),
        before,
        "expected no Version to have been made"
    );
    vm.get_as(&alice(), "board", "/").expect_status(404);
}

/// A Version body of exactly `size` bytes: a valid one whose bundle is
/// padded with a JavaScript comment.
fn version_body_of_size(size: usize) -> Vec<u8> {
    let empty = version_body("").to_string();
    let padding = size.checked_sub(empty.len()).expect("size too small");
    let body = version_body(&"/".repeat(padding)).to_string().into_bytes();
    assert_eq!(body.len(), size);
    body
}

fn big(api: &Api<'_>, target: &str, body: Vec<u8>, chunked: bool) -> blackbox::Response {
    let request = api
        .request(target)
        .method("POST")
        .header("content-type", "application/json")
        .body(body)
        .timeout(Duration::from_secs(60));
    if chunked { request.chunked() } else { request }.send()
}

#[test]
fn a_body_larger_than_16_mib_is_413_too_large_and_changes_nothing() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("small"));
    let before = state_of(&api, "board");
    for chunked in [false, true] {
        let how = if chunked {
            "chunked"
        } else {
            "with a Content-Length"
        };
        let response = big(
            &api,
            "/v1/applications/board/versions",
            version_body_of_size(LIMIT + 1),
            chunked,
        );
        assert!(
            response.status == 413,
            "expected `413 too-large` for a Version body of 16 MiB + 1 byte sent {how}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 413, "too-large");
        let mut name_body = br#"{"name":"huge","description":""#.to_vec();
        name_body.extend(std::iter::repeat_n(b'd', LIMIT));
        name_body.extend_from_slice(br#""}"#);
        let response = big(&api, "/v1/applications", name_body, chunked);
        assert_api_error(&response, 413, "too-large");
    }
    assert_eq!(state_of(&api, "board"), before);
    assert_api_error(&api.get_application("huge"), 404, "not-found");
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "small", 1);
}

#[test]
fn a_body_of_exactly_16_mib_is_not_too_large() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    let body = version_body_of_size(LIMIT);
    let digest = sha256_hex(
        serde_json::from_slice::<Value>(&body).unwrap()["bundle"]
            .as_str()
            .unwrap()
            .as_bytes(),
    );
    let response = big(&api, "/v1/applications/board/versions", body, false);
    response.expect_status(201);
    let version = response.json();
    assert_eq!(version["number"], 1);
    assert_eq!(
        version["bundle_sha256"]
            .as_str()
            .map(str::to_ascii_lowercase),
        Some(digest)
    );
}
