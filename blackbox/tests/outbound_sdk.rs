//! Outbound calls through the SDK (CONTRACT.md, "Outbound calls: What
//! Application code does"): with `@vm/sdk` imported a refusal of `fetch`
//! rejects with `Refused` (its `refusal` and `fix`), one that escapes the
//! handler is the `403` with `{ refusal, fix }` as for any operation; a
//! failure rejects with `OperationFailed` (`kind`, `detail`), and escaping
//! the handler it is the platform's fixed `500`. The fixture project is
//! `fixtures/sdk/outbound`, built with the SDK's tooling.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::http::Response;
use blackbox::outbound::{OPEN_LOOPBACK, Origin, Outbound, Reply, Setup, bob};
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, built, install};
use serde_json::Value;

const APP: &str = "sdk-outbound";

/// The program, the fixture project installed, and an origin (not listed).
fn started() -> (Outbound, Origin) {
    started_with(&OPEN_LOOPBACK)
}

/// As [`started`], without `--outbound-private`: for tests of a refusal,
/// which need no call to succeed.
fn started_closed() -> (Outbound, Origin) {
    started_with(&[])
}

fn started_with(args: &[&str]) -> (Outbound, Origin) {
    let built = built("outbound");
    let o = Setup::new(args).start(&[]);
    install(&o.vm, &built);
    let origin = o.origin();
    (o, origin)
}

fn get(o: &Outbound, path: &str) -> Response {
    o.vm.request(APP, path)
        .as_user(&bob())
        .timeout(SDK_REQUEST_TIMEOUT)
        .send()
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

#[test]
fn a_refusal_rejects_with_refused_that_has_its_refusal_and_fix() {
    let (o, origin) = started_closed();
    let response = get(&o, &format!("/caught?url={}", encode(&origin.url("/x"))));
    assert_eq!(response.status, 200, "{}", response.describe());
    let caught = response.json();
    assert_eq!(
        caught["isRefused"], true,
        "expected a `Refused`, got {caught}"
    );
    assert_eq!(caught["name"], "Refused", "{caught}");
    assert_eq!(caught["isError"], true, "{caught}");
    assert_eq!(caught["refusal"]["reason"], "host-not-listed", "{caught}");
    assert_eq!(caught["refusal"]["host"], origin.host(), "{caught}");
    assert!(
        caught["fix"]
            .as_str()
            .is_some_and(|f| f.contains("vibe admin outbound add") && f.contains("vibe outbound")),
        "expected the fix to name the command and `vibe outbound`, got {caught}"
    );
    origin.assert_untouched("a refusal through the SDK");
}

#[test]
fn a_refusal_that_escapes_the_handler_is_the_403_with_refusal_and_fix() {
    let (o, origin) = started_closed();
    let response = get(&o, &format!("/escape?url={}", encode(&origin.url("/x"))));
    assert_eq!(
        response.status,
        403,
        "expected the `403` an escaping refusal is, got\n{}",
        response.describe()
    );
    let body: Value = response.json();
    assert_eq!(body["refusal"]["reason"], "host-not-listed", "{body}");
    assert_eq!(body["refusal"]["host"], origin.host(), "{body}");
    assert!(
        body["fix"].as_str().is_some_and(|f| !f.is_empty()),
        "{body}"
    );
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["fix", "refusal"], "{body}");
}

#[test]
fn a_listed_call_resolves_with_the_answer_whatever_its_status() {
    let (o, origin) = started();
    o.allow(&origin.host());
    origin.always(Reply::new(404).body(b"nothing here"));
    let response = get(
        &o,
        &format!("/caught?url={}", encode(&origin.url("/missing"))),
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    let caught = response.json();
    assert_eq!(caught["status"], 404, "{caught}");
    assert_eq!(caught["text"], "nothing here", "{caught}");
}

#[test]
fn a_failure_rejects_with_operation_failed_that_has_its_kind_and_detail() {
    let (o, _origin) = started();
    let port = blackbox::ports::chosen().unwrap();
    o.allow(&format!("localhost:{port}"));
    let response = get(
        &o,
        &format!(
            "/caught?url={}",
            encode(&format!("https://localhost:{port}/"))
        ),
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    let caught = response.json();
    assert_eq!(
        caught["isFailed"], true,
        "expected an `OperationFailed`, got {caught}"
    );
    assert_eq!(caught["name"], "OperationFailed", "{caught}");
    assert_eq!(caught["kind"], "unreachable", "{caught}");
    assert_eq!(caught["detail"]["kind"], "unreachable", "{caught}");
    assert_eq!(caught["detail"]["why"], "connect", "{caught}");
}

#[test]
fn a_failure_that_escapes_the_handler_is_the_platforms_fixed_500() {
    let (o, _origin) = started();
    let port = blackbox::ports::chosen().unwrap();
    o.allow(&format!("localhost:{port}"));
    let response = get(
        &o,
        &format!(
            "/escape?url={}",
            encode(&format!("https://localhost:{port}/"))
        ),
    );
    assert_eq!(response.status, 500, "{}", response.describe());
    assert!(
        !response.text().contains("unreachable") && !response.text().contains("localhost"),
        "expected the fixed body of a `500`, nothing of the failure, got {}",
        response.describe()
    );
}
