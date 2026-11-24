//! Who may revoke a Grant, and with what (CONTRACT.md, "Resources and
//! Grants in full: Who may", "Revoking a Grant"):
//! `/v1/resources/<name>/grants/<group>` is a Platform administrator's path
//! whatever the method, the names and the body: anyone else signed in is
//! `403` `not-an-administrator`, the same bytes for every request, before
//! `404`, `405` and `400`, and nothing changes; someone not signed in is the
//! plain `401`; the path takes `DELETE` only (`405` for an administrator
//! otherwise); past 64 KiB a body is `413` for everyone, and no body is
//! looked at under it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::resources::{admin_resource, records_waited};
use blackbox::scenario::{RESOURCE, Scenario, administrator};
use blackbox::{Request, Response, User, Vm, assert_api_error};
use serde_json::json;

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn not_administrators() -> Vec<User> {
    vec![
        alice(),
        User::new("mallory").in_groups(&["Platform-Admins", "platform-admins-x"]),
        User::new(&administrator().id),
        User::new("The DevOps team"),
    ]
}

fn send(vm: &Vm, user: Option<&User>, path: &str, method: &str, body: Option<&str>) -> Response {
    let mut request: Request = vm.api_request(path).method(method);
    if let Some(user) = user {
        request = request.as_user(user);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    request.send()
}

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
const BODIES: [&str; 4] = ["{}", "", "not json", r#"{"group":"ops"}"#];

/// Every request of stage three's revoke path: (path, method, body).
fn requests() -> Vec<(String, &'static str, Option<String>)> {
    let long = "g".repeat(300);
    let mut all = Vec::new();
    for name in [RESOURCE, "ghost", "Not_A_Name"] {
        for segment in ["ops", "%FF", "%01", long.as_str(), "a%2Fb"] {
            for method in METHODS {
                for body in BODIES {
                    all.push((
                        format!("/v1/resources/{name}/grants/{segment}"),
                        method,
                        Some(body.to_owned()),
                    ));
                }
            }
        }
    }
    all
}

fn world() -> Scenario {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "read");
    s.grant(RESOURCE, "a/b", "write");
    s
}

#[test]
fn everyone_else_is_refused_the_same_byte_for_byte_and_nothing_changes() {
    let s = world();
    let before = admin_resource(&s.admin(), RESOURCE).json();
    let grants = s.admin().list_grants(RESOURCE).json();
    records_waited(&s.vm, "grant-set", 2);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let records_before = s.vm.records().len();
    for user in not_administrators() {
        let mut bodies: Vec<(String, Vec<u8>)> = Vec::new();
        for (path, method, body) in requests() {
            let answer = send(&s.vm, Some(&user), &path, method, body.as_deref());
            assert_api_error(&answer, 403, "not-an-administrator");
            bodies.push((format!("{method} {path} {body:?}"), answer.body().to_vec()));
        }
        let first = bodies[0].1.clone();
        for (what, body) in &bodies {
            assert_eq!(
                body, &first,
                "{}: the refusal of {what} differs from that of {}: nothing in it may depend on the request",
                user.id, bodies[0].0
            );
        }
    }
    assert_eq!(admin_resource(&s.admin(), RESOURCE).json(), before);
    assert_eq!(s.admin().list_grants(RESOURCE).json(), grants);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let changed: Vec<String> = s.vm.records()[records_before..]
        .iter()
        .filter_map(|r| r["event"].as_str())
        .filter(|e| e.starts_with("grant-"))
        .map(str::to_owned)
        .collect();
    assert!(
        changed.is_empty(),
        "no record of a change for a refused request: {changed:?}"
    );
}

#[test]
fn someone_not_signed_in_gets_the_plain_401() {
    let s = world();
    let mut bodies = Vec::new();
    for (path, method, body) in requests() {
        let answer = send(&s.vm, None, &path, method, body.as_deref());
        assert_eq!(answer.status, 401, "{method} {path}\n{}", answer.describe());
        bodies.push(answer.body().to_vec());
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "the same plain `401` everywhere"
    );
}

#[test]
fn an_administrator_is_405_on_every_method_but_delete() {
    let s = world();
    let admin = administrator();
    for name in [RESOURCE, "ghost"] {
        for method in ["GET", "POST", "PUT", "PATCH"] {
            let answer = send(
                &s.vm,
                Some(&admin),
                &format!("/v1/resources/{name}/grants/ops"),
                method,
                Some("{}"),
            );
            assert_api_error(&answer, 405, "method-not-allowed");
        }
    }
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "nothing changed"
    );
}

#[test]
fn no_body_is_looked_at_under_64_kib_and_past_it_is_413_for_everyone() {
    let s = world();
    // A body that is not JSON, or says something else, changes nothing.
    let answer = send(
        &s.vm,
        Some(&administrator()),
        &format!("/v1/resources/{RESOURCE}/grants/ops"),
        "DELETE",
        Some("not json at all"),
    );
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({ "resource": RESOURCE, "group": "ops", "changed": true, "was": "read" })
    );
    let answer = send(
        &s.vm,
        Some(&administrator()),
        &format!("/v1/resources/{RESOURCE}/grants/a%2Fb"),
        "DELETE",
        Some(r#"{"group":"someone-else"}"#),
    );
    assert_eq!(
        answer.json()["group"],
        json!("a/b"),
        "the path says the Group"
    );

    s.grant(RESOURCE, "ops", "read");
    let path = format!("/v1/resources/{RESOURCE}/grants/ops");
    let at = " ".repeat(64 * 1024);
    let over = " ".repeat(64 * 1024 + 1);
    for user in [administrator(), alice()] {
        assert_api_error(
            &send(&s.vm, Some(&user), &path, "DELETE", Some(&over)),
            413,
            "too-large",
        );
    }
    assert_api_error(
        &send(&s.vm, Some(&alice()), &path, "DELETE", Some(&at)),
        403,
        "not-an-administrator",
    );
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"][0]["group"],
        json!("ops"),
        "nothing was revoked by a request that was refused"
    );
}
