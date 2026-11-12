//! Hostile callers of the platform's API: someone who tries to send a
//! Version to an Application that is not theirs by path tricks or through
//! another address, to take a platform name through JSON tricks, or to
//! choose fields the platform sets (who created it, who contributes, a
//! Version's number). Whatever they send, nothing of anyone else's changes,
//! and the program goes on answering.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::{assert_served_by, marked_bundle, sha256_hex, version_body};
use blackbox::{Api, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice")
}

fn mallory() -> User {
    User::new("mallory")
}

/// alice owns `theirs` (one Version, marked "alice"); mallory owns `mine`.
fn two_owners(vm: &Vm) {
    vm.api_as(&alice())
        .create_with_bundle("theirs", &marked_bundle("alice"));
    vm.api_as(&mallory())
        .create_with_bundle("mine", &marked_bundle("mallory"));
}

fn state_of(api: &Api<'_>, name: &str) -> (Value, Vec<Value>) {
    (api.application(name), api.versions(name))
}

/// The `reason` of an API error answer, if it is one.
fn reason_of(response: &Response) -> Option<String> {
    serde_json::from_slice::<Value>(response.body())
        .ok()
        .and_then(|b| b["error"]["reason"].as_str().map(str::to_owned))
}

#[test]
fn path_tricks_never_send_a_version_to_someone_elses_application() {
    let vm = vm();
    two_owners(&vm);
    let watcher = vm.api_as(&alice());
    let before = state_of(&watcher, "theirs");
    let body = version_body(&marked_bundle("hijacked"));
    for target in [
        "/v1/applications/theirs/versions",
        "/v1/applications/mine/../theirs/versions",
        "/v1/applications/mine/%2e%2e/theirs/versions",
        "/v1/applications/mine%2F..%2Ftheirs/versions",
        "/v1/applications/mine/versions/../../theirs/versions",
        "/v1/applications/./theirs/versions",
        "/v1/applications//theirs/versions",
        "/v1/applications/theirs/versions/",
        "/v1/applications/theirs/./versions",
        "/v1/applications/THEIRS/versions",
        "/v1/applications/%74heirs/versions",
        "/v1/applications/theirs%00/versions",
        "/v1/applications/theirs/versions?application=mine",
        "/v1/applications/theirs/versions#mine",
        "http://api.vm.test/v1/applications/theirs/versions",
    ] {
        let response = vm
            .api_as(&mallory())
            .request(target)
            .method("POST")
            .json(&body)
            .send();
        assert!(
            !(200..300).contains(&response.status),
            "expected mallory's POST {target} not to succeed, got\n{}",
            response.describe()
        );
    }
    assert_eq!(
        state_of(&watcher, "theirs"),
        before,
        "expected nothing of alice's Application to change"
    );
    assert_served_by(
        &vm,
        &vm.get_as(&alice(), "theirs", "/"),
        "theirs",
        "alice",
        1,
    );
}

#[test]
fn a_post_to_an_applications_own_address_never_reaches_the_api() {
    let vm = vm();
    two_owners(&vm);
    let before = state_of(&vm.api_as(&alice()), "theirs");
    for host in ["theirs.vm.test", "mine.vm.test"] {
        let response = vm
            .request("theirs", "/v1/applications/theirs/versions")
            .host(host)
            .method("POST")
            .as_user(&mallory())
            .json(&version_body(&marked_bundle("hijacked")))
            .send();
        // The request goes to the Application at that address, whose bundle
        // answers it like any other request.
        let application = host.split('.').next().unwrap();
        let marker = if application == "theirs" {
            "alice"
        } else {
            "mallory"
        };
        assert_served_by(&vm, &response, application, marker, 1);
    }
    assert_eq!(state_of(&vm.api_as(&alice()), "theirs"), before);
}

#[test]
fn the_api_answers_whatever_the_letter_case_or_port_in_host() {
    let vm = vm();
    two_owners(&vm);
    for host in [
        "API.VM.TEST",
        "Api.Vm.Test",
        "api.vm.test:443",
        "API.vm.test:1",
    ] {
        let response = vm
            .api_request("/v1/applications/theirs")
            .host(host)
            .as_user(&mallory())
            .send();
        assert_eq!(
            response.status,
            200,
            "expected Host {host:?} to reach the platform's API, got\n{}",
            response.describe()
        );
        assert_eq!(response.json()["name"], "theirs");
    }
}

#[test]
fn fields_the_platform_sets_cannot_be_chosen_when_creating_an_application() {
    let vm = vm();
    let response = vm.api_as(&mallory()).create_application_with(&json!({
        "name": "smuggled",
        "description": "innocent",
        "created_by": "alice",
        "contributors": [{ "user": "alice" }, { "user": "mallory" }, { "user": "eve" }],
        "serving_version": 7,
        "address": "api.vm.test",
        "created_at": "1999-01-01T00:00:00Z",
    }));
    let reason = reason_of(&response);
    assert!(
        response.status == 201
            || (response.status == 400 && reason.as_deref() == Some("malformed")),
        "expected extra fields to be ignored (201) or refused (400 malformed), got\n{}",
        response.describe()
    );
    if response.status == 201 {
        let application = vm.api_as(&alice()).application("smuggled");
        assert_eq!(application["created_by"], "mallory", "in {application}");
        assert_eq!(
            application["contributors"],
            json!([{ "user": "mallory" }]),
            "in {application}"
        );
        assert!(application["serving_version"].is_null(), "in {application}");
        assert_eq!(
            application["address"], "smuggled.vm.test",
            "in {application}"
        );
        assert_ne!(
            application["created_at"], "1999-01-01T00:00:00Z",
            "in {application}"
        );
        // alice did not become a Contributor by being named.
        assert_api_error(
            &vm.api_as(&User::new("eve"))
                .send_bundle("smuggled", &marked_bundle("eve")),
            403,
            "not-a-contributor",
        );
    }
}

#[test]
fn fields_the_platform_sets_cannot_be_chosen_when_sending_a_version() {
    let vm = vm();
    two_owners(&vm);
    let bundle = marked_bundle("smuggled");
    let mut body = version_body(&bundle);
    let extra = json!({
        "number": 99,
        "application": "theirs",
        "created_by": "alice",
        "bundle_sha256": "0".repeat(64),
        "created_at": "1999-01-01T00:00:00Z",
        "serving": true,
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let before = state_of(&vm.api_as(&alice()), "theirs");
    let response = vm.api_as(&mallory()).send_version("mine", &body);
    let reason = reason_of(&response);
    assert!(
        response.status == 201
            || (response.status == 400 && reason.as_deref() == Some("malformed")),
        "expected extra fields to be ignored (201) or refused (400 malformed), got\n{}",
        response.describe()
    );
    if response.status == 201 {
        let version = response.json();
        assert_eq!(version["number"], 2, "in {version}");
        assert_eq!(version["application"], "mine", "in {version}");
        assert_eq!(version["created_by"], "mallory", "in {version}");
        assert_eq!(
            version["bundle_sha256"]
                .as_str()
                .map(str::to_ascii_lowercase),
            Some(sha256_hex(bundle.as_bytes())),
            "in {version}"
        );
        assert_ne!(
            version["created_at"], "1999-01-01T00:00:00Z",
            "in {version}"
        );
        assert_api_error(
            &vm.api_as(&alice()).get_version("mine", 99),
            404,
            "not-found",
        );
    }
    assert_eq!(state_of(&vm.api_as(&alice()), "theirs"), before);
}

#[test]
fn duplicate_keys_never_let_a_platform_name_through() {
    let vm = vm();
    let api = vm.api_as(&mallory());
    for body in [
        r#"{"name":"api","name":"dup-first-reserved"}"#,
        r#"{"name":"dup-last-reserved","name":"api"}"#,
        r#"{"name":"Not_Valid","name":"dup-first-invalid"}"#,
        r#"{"name":"dup-last-invalid","name":"Not_Valid"}"#,
    ] {
        let response = api.post_raw("/v1/applications", body);
        assert!(
            response.status == 400 || response.status == 201,
            "expected a body with a duplicate key to be refused (400) or taken as one name, got\n{}",
            response.describe()
        );
        if response.status == 201 {
            let name = response.json()["name"].as_str().unwrap().to_owned();
            assert!(
                name.starts_with("dup-"),
                "expected only a valid, free name to be created from {body}, got {name:?}"
            );
        }
    }
    for name in ["api", "Not_Valid", "not-valid"] {
        let response = api.get(&format!("/v1/applications/{name}"));
        assert_eq!(
            response.status,
            404,
            "expected no Application {name:?}, got\n{}",
            response.describe()
        );
    }
    assert_api_error(&api.get_application("nothing-here"), 404, "not-found");
}

#[test]
fn wrong_types_huge_numbers_and_broken_json_are_400_malformed() {
    let vm = vm();
    let api = vm.api_as(&mallory());
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let deep_object = format!(
        r#"{{"name":"deep","description":{}"x"{}}}"#,
        "[".repeat(10_000),
        "]".repeat(10_000)
    );
    let bodies: Vec<(&str, Vec<u8>)> = vec![
        ("an empty body", Vec::new()),
        ("not JSON", b"name=board".to_vec()),
        ("cut-off JSON", br#"{"name":"board""#.to_vec()),
        (
            "trailing garbage",
            br#"{"name":"board"} {"name":"other"}"#.to_vec(),
        ),
        ("a JSON array", br#"["board"]"#.to_vec()),
        ("a JSON string", br#""board""#.to_vec()),
        ("JSON null", b"null".to_vec()),
        ("an empty object", b"{}".to_vec()),
        ("a name that is a number", br#"{"name":5}"#.to_vec()),
        (
            "a name that is a huge number",
            br#"{"name":1e999}"#.to_vec(),
        ),
        (
            "a name that is 2^64",
            br#"{"name":18446744073709551616}"#.to_vec(),
        ),
        ("a name that is null", br#"{"name":null}"#.to_vec()),
        ("a name that is true", br#"{"name":true}"#.to_vec()),
        ("a name that is a list", br#"{"name":["board"]}"#.to_vec()),
        (
            "a name that is an object",
            br#"{"name":{"name":"board"}}"#.to_vec(),
        ),
        (
            "a description that is a number",
            br#"{"name":"board","description":5}"#.to_vec(),
        ),
        (
            "a description that is a list",
            br#"{"name":"board","description":["a"]}"#.to_vec(),
        ),
        (
            "bytes that are not UTF-8",
            b"{\"name\":\"bo\xffard\"}".to_vec(),
        ),
        ("deeply nested arrays", deep.into_bytes()),
        ("a deeply nested description", deep_object.into_bytes()),
    ];
    for (case, body) in &bodies {
        let response = api.post_raw("/v1/applications", body);
        assert!(
            response.status == 400,
            "expected `400 malformed` for {case}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 400, "malformed");
    }
    assert_api_error(&api.get_application("board"), 404, "not-found");
    assert_api_error(&api.get_application("deep"), 404, "not-found");
    // And the program is still answering.
    api.create_application("still-here").expect_status(201);
}

#[test]
fn a_lone_surrogate_in_a_name_is_refused() {
    let vm = vm();
    let api = vm.api_as(&mallory());
    let response = api.post_raw("/v1/applications", br#"{"name":"a\ud800b"}"#);
    let reason = reason_of(&response);
    assert!(
        response.status == 400 && matches!(reason.as_deref(), Some("malformed" | "invalid-name")),
        "expected `400` (malformed or invalid-name) for a name holding a lone surrogate, got\n{}",
        response.describe()
    );
}
