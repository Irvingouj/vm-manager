//! What the tests of the outbound list's surfaces share (CONTRACT.md,
//! "Outbound calls": "The list", "The tool", "The Dashboard", "The
//! instruction file"): the list's three requests as a named User, the
//! shape of what they answer, a reading of the list, the entries of the
//! program's log that a change leaves, and a tool signed in as a User.
//!
//! What a call does with the list (the decision, the wire, the Operation
//! log's entries) is the core's tests' (`outbound_*` with `src/outbound.rs`).

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::admin::{self, administrator, logged};
use crate::api::{assert_json_content_type, assert_recent_rfc3339};
use crate::http::{Request, Response, User};
use crate::logs::encode;
use crate::tool::Machine;
use crate::vm::Vm;

/// The list.
pub const HOSTS: &str = "/v1/outbound-hosts";

/// The most entries the list holds.
pub const MOST: usize = 1000;

/// A program with the stand-in sign-in and the administrators' Group.
pub fn start() -> Vm {
    admin::start()
}

/// A signed-in User who is not a Platform administrator.
pub fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// A signed-in User with no Group.
pub fn bob() -> User {
    User::new("bob")
}

/// The address of one entry, the entry percent-encoded as a path segment.
pub fn path_of(entry: &str) -> String {
    format!("{HOSTS}/{}", encode(entry))
}

/// A request to `path` as this User, or as nobody.
pub fn request(vm: &Vm, user: Option<&User>, method: &str, path: &str) -> Request {
    let request = vm.api_request(path).method(method);
    match user {
        Some(user) => request.as_user(user),
        None => request,
    }
}

/// `GET /v1/outbound-hosts` as this User.
pub fn list(vm: &Vm, user: &User) -> Response {
    request(vm, Some(user), "GET", HOSTS).send()
}

/// `PUT /v1/outbound-hosts/<entry>` as this User, the entry encoded.
pub fn put(vm: &Vm, user: &User, entry: &str) -> Response {
    request(vm, Some(user), "PUT", &path_of(entry)).send()
}

/// `DELETE /v1/outbound-hosts/<entry>` as this User, the entry encoded.
pub fn delete(vm: &Vm, user: &User, entry: &str) -> Response {
    request(vm, Some(user), "DELETE", &path_of(entry)).send()
}

/// The sorted keys of a JSON object.
pub fn keys_of(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Fails unless this is the answer to a `PUT` that the contract gives:
/// `200`, JSON, exactly `{ host, added_by, added_at, changed }`, `host`
/// this, `changed` this, `added_by` this, `added_at` a recent RFC 3339
/// time. Returns the body.
pub fn assert_put_answer(
    answer: &Response,
    host: &str,
    changed: bool,
    added_by: &str,
    context: &str,
) -> Value {
    assert!(
        answer.status == 200,
        "{context}: expected `200` for the PUT of {host:?}, got\n{}",
        answer.describe()
    );
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["added_at", "added_by", "changed", "host"],
        "{context}: expected exactly {{ host, added_by, added_at, changed }}, got {body}"
    );
    assert_eq!(
        body["host"].as_str(),
        Some(host),
        "{context}: expected `host` {host:?}, got {body}"
    );
    assert_eq!(
        body["changed"].as_bool(),
        Some(changed),
        "{context}: expected `changed` {changed}, got {body}"
    );
    assert_eq!(
        body["added_by"].as_str(),
        Some(added_by),
        "{context}: expected `added_by` {added_by:?}, got {body}"
    );
    assert!(
        crate::api::parse_rfc3339(body["added_at"].as_str().unwrap_or_default()).is_some(),
        "{context}: expected `added_at` an RFC 3339 time, got {body}"
    );
    body
}

/// Fails unless this is the answer to a `PUT` that added `host` just now
/// as `added_by`.
pub fn assert_added(answer: &Response, host: &str, added_by: &str, context: &str) -> Value {
    let body = assert_put_answer(answer, host, true, added_by, context);
    assert_recent_rfc3339(&body["added_at"], 300, &format!("{context}: added_at"));
    body
}

/// Fails unless this is the answer to a `DELETE` that the contract gives:
/// `200`, JSON, exactly `{ host, changed }`.
pub fn assert_deleted(answer: &Response, host: &str, changed: bool, context: &str) {
    assert!(
        answer.status == 200,
        "{context}: expected `200` for the DELETE of {host:?}, got\n{}",
        answer.describe()
    );
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["changed", "host"],
        "{context}: expected exactly {{ host, changed }}, got {body}"
    );
    assert_eq!(
        body["host"].as_str(),
        Some(host),
        "{context}: expected `host` {host:?}, got {body}"
    );
    assert_eq!(
        body["changed"].as_bool(),
        Some(changed),
        "{context}: expected `changed` {changed}, got {body}"
    );
}

/// The list as this User reads it: `200`, JSON, exactly `{ hosts }`, each
/// entry exactly `{ host, added_by, added_at }`. Returns the entries.
pub fn entries_as(vm: &Vm, user: &User) -> Vec<Value> {
    let answer = list(vm, user);
    assert!(
        answer.status == 200,
        "expected `200` reading {HOSTS} as {:?}, got\n{}",
        user.id,
        answer.describe()
    );
    entries_of(&answer)
}

/// The entries of an answer to `GET /v1/outbound-hosts`, its shape
/// checked.
pub fn entries_of(answer: &Response) -> Vec<Value> {
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["hosts"],
        "expected exactly {{ hosts }} from {HOSTS}, got {body}"
    );
    let hosts = body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `hosts` an array, got {body}"));
    for entry in hosts {
        assert_eq!(
            keys_of(entry),
            ["added_at", "added_by", "host"],
            "expected each entry to be exactly {{ host, added_by, added_at }}, got {entry}"
        );
    }
    hosts.clone()
}

/// The list as a Platform administrator reads it.
pub fn entries(vm: &Vm) -> Vec<Value> {
    entries_as(vm, &administrator())
}

/// The `host` of each entry, in the order answered.
pub fn hosts(vm: &Vm) -> Vec<String> {
    entries(vm)
        .iter()
        .map(|e| e["host"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Adds `entry` as the administrator, which must answer `200`; returns the
/// answer's body.
pub fn add(vm: &Vm, entry: &str) -> Value {
    let answer = put(vm, &administrator(), entry);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` adding {entry:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// The records of the program's log of this event (`outbound-host-added`
/// or `outbound-host-removed`) for this host.
pub fn records_of(vm: &Vm, event: &str, host: &str) -> Vec<Value> {
    logged(vm, event)
        .into_iter()
        .filter(|r| r["host"].as_str() == Some(host))
        .collect()
}

/// Waits until the program's log has at least `count` records of this
/// event for this host, at most five seconds; fails the test otherwise.
/// Returns them.
pub fn wait_for_records(vm: &Vm, event: &str, host: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = records_of(vm, event, host);
        if found.len() >= count {
            return found;
        }
        if Instant::now() >= deadline {
            vm.fail(&format!(
                "expected {count} `{event}` record(s) for {host:?} in the program's log within 5 seconds, found {}",
                found.len()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// How many records of `outbound-host-added` and `outbound-host-removed`
/// the program's log has.
pub fn change_records(vm: &Vm) -> usize {
    logged(vm, "outbound-host-added").len() + logged(vm, "outbound-host-removed").len()
}

/// A tool of a machine of its own, signed in as this User.
pub fn signed_in(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

/// How many `request` records the program's log has: what tells whether a
/// command asked the installation anything.
pub fn asked(vm: &Vm) -> usize {
    admin::request_records(vm)
}
