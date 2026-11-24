//! Who may do what with a Resource (CONTRACT.md, "Resources and Grants in
//! full: Who may"): every path of stage two that writes a Resource, and
//! everything under `/v1/admin/resources`, is for Platform administrators;
//! anyone else signed in is `403` `not-an-administrator`, the same bytes for
//! every request, before `404`, `405`, `400` and `413`; someone not signed
//! in is the plain `401`; `/v1/resources/<name>` takes `GET` for every
//! User, `PATCH` and `DELETE` for administrators, and is `405` for every
//! other method; `.../connection` and `.../location` take `PUT` only, and
//! `/v1/admin/resources[/<name>]` `GET` only (the `405`s, after the check
//! of administrators).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::resources::admin_resource;
use blackbox::scenario::{ADMINISTRATORS, RESOURCE, Scenario, administrator};
use blackbox::{Request, Response, User, Vm, assert_api_error};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn not_administrators() -> Vec<User> {
    vec![
        alice(),
        User::new("mallory").in_groups(&["Platform-Admins", "platform-admins-x"]),
        User::new(&administrator().id),
        // The Resource's owner, as it is written in the registration.
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

const BODIES: [&str; 4] = ["{}", "", "not json", r#"{"contact":"x"}"#];

/// The paths that are the administrators' whatever the method, with every
/// method, name and body: (path, method, body).
fn administrators_requests() -> Vec<(String, &'static str, Option<String>)> {
    let mut all = Vec::new();
    for name in [RESOURCE, "ghost", "Not_A_Name"] {
        for suffix in ["/connection", "/location"] {
            for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
                for body in BODIES {
                    all.push((
                        format!("/v1/resources/{name}{suffix}"),
                        method,
                        Some(body.to_owned()),
                    ));
                }
            }
        }
        for method in ["PATCH", "DELETE"] {
            for body in BODIES {
                all.push((
                    format!("/v1/resources/{name}"),
                    method,
                    Some(body.to_owned()),
                ));
            }
        }
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            all.push((format!("/v1/admin/resources/{name}"), method, None));
        }
    }
    for query in [
        "",
        "?limit=5",
        "?limit=0",
        "?after=Not_A_Name",
        "?after=a&after=b",
    ] {
        all.push((format!("/v1/admin/resources{query}"), "GET", None));
    }
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        all.push((
            "/v1/admin/resources".to_owned(),
            method,
            Some("{}".to_owned()),
        ));
    }
    all
}

fn world() -> Scenario {
    let s = Scenario::ready();
    s.install("peek", "query.js", &[(RESOURCE, "read")]);
    s
}

#[test]
fn everyone_else_is_refused_the_same_byte_for_byte_and_nothing_changes() {
    let s = world();
    let before = admin_resource(&s.admin(), RESOURCE).json();
    let grants = s.admin().list_grants(RESOURCE).json();
    // The registration's own record may still be on its way to the harness.
    blackbox::resources::records_waited(&s.vm, "resource-registered", 1);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let records_before = s.vm.records().len();
    for user in not_administrators() {
        let mut bodies: Vec<(String, Vec<u8>)> = Vec::new();
        for (path, method, body) in administrators_requests() {
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
    assert_eq!(
        admin_resource(&s.admin(), RESOURCE).json(),
        before,
        "nothing changed"
    );
    assert_eq!(s.admin().list_grants(RESOURCE).json(), grants);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let changed: Vec<String> = s.vm.records()[records_before..]
        .iter()
        .filter_map(|r| r["event"].as_str())
        .filter(|e| e.starts_with("resource-"))
        .map(str::to_owned)
        .collect();
    assert!(
        changed.is_empty(),
        "no record of a change for a refused request: {changed:?}"
    );
}

#[test]
fn past_64_kib_is_413_for_everyone_and_under_it_403_whatever_the_body() {
    // Only `too-large` comes before `not-an-administrator`; on these paths
    // it is past 64 KiB, for everyone.
    let s = world();
    let paths = [
        (format!("/v1/resources/{RESOURCE}/connection"), "PUT"),
        (format!("/v1/resources/{RESOURCE}/location"), "PUT"),
        (format!("/v1/resources/{RESOURCE}"), "PATCH"),
    ];
    let over = " ".repeat(64 * 1024 + 1);
    let at = format!("{}{}", r#"{"contact":"x"}"#, " ".repeat(64 * 1024 - 15));
    assert_eq!(at.len(), 64 * 1024);
    for (path, method) in &paths {
        for user in [alice(), administrator()] {
            let answer = send(&s.vm, Some(&user), path, method, Some(&over));
            assert_api_error(&answer, 413, "too-large");
        }
        let answer = send(&s.vm, Some(&alice()), path, method, Some(&at));
        assert_api_error(&answer, 403, "not-an-administrator");
        let answer = send(&s.vm, Some(&alice()), path, method, Some("not json"));
        assert_api_error(&answer, 403, "not-an-administrator");
    }
}

#[test]
fn a_name_no_resource_could_have_is_404_for_an_administrator_on_every_new_path() {
    let s = world();
    let admin = administrator();
    for (path, method, body) in [
        (
            "/v1/resources/Not_A_Name/connection",
            "PUT",
            r#"{"write":null}"#,
        ),
        (
            "/v1/resources/Not_A_Name/location",
            "PUT",
            r#"{"backend":"directory","directory":"/x"}"#,
        ),
        ("/v1/resources/Not_A_Name", "PATCH", r#"{"contact":"x"}"#),
        ("/v1/resources/Not_A_Name", "DELETE", ""),
        ("/v1/admin/resources/Not_A_Name", "GET", ""),
    ] {
        let body = (!body.is_empty()).then_some(body);
        assert_api_error(
            &send(&s.vm, Some(&admin), path, method, body),
            404,
            "not-found",
        );
        assert_api_error(
            &send(&s.vm, Some(&alice()), path, method, body),
            403,
            "not-an-administrator",
        );
    }
}

#[test]
fn someone_not_signed_in_gets_the_plain_401_everywhere() {
    let s = world();
    let mut bodies = Vec::new();
    for (path, method, body) in administrators_requests() {
        let answer = send(&s.vm, None, &path, method, body.as_deref());
        assert_eq!(answer.status, 401, "{method} {path}\n{}", answer.describe());
        assert!(
            answer
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/plain")),
            "{}",
            answer.describe()
        );
        bodies.push(answer.body().to_vec());
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "the same plain `401` everywhere"
    );
}

#[test]
fn one_path_two_kinds_of_request_get_stays_open_and_the_rest_is_405_for_anyone() {
    let s = world();
    for user in [alice(), administrator()] {
        // `GET` is for every signed-in User, as before.
        let answer = send(
            &s.vm,
            Some(&user),
            &format!("/v1/resources/{RESOURCE}"),
            "GET",
            None,
        );
        answer.expect_status(200);
        assert!(
            answer.json().get("logins").is_none(),
            "{}",
            answer.describe()
        );
        // Every other method that is not the administrators' is `405`.
        for method in ["POST", "PUT"] {
            let answer = send(
                &s.vm,
                Some(&user),
                &format!("/v1/resources/{RESOURCE}"),
                method,
                Some("{}"),
            );
            assert_api_error(&answer, 405, "method-not-allowed");
            assert!(
                answer.header("allow").is_none_or(|a| a.contains("GET")),
                "an `allow` header, when there is one, names GET: {}",
                answer.describe()
            );
        }
    }
    // Someone who is not an administrator is `403`, not `405`, on the two
    // methods that are the administrators'.
    for method in ["PATCH", "DELETE"] {
        let answer = send(
            &s.vm,
            Some(&alice()),
            &format!("/v1/resources/{RESOURCE}"),
            method,
            Some("{}"),
        );
        assert_api_error(&answer, 403, "not-an-administrator");
    }
}

#[test]
fn an_administrator_is_405_on_the_methods_a_path_does_not_take() {
    let s = world();
    let admin = administrator();
    let not_put = ["GET", "POST", "PATCH", "DELETE"];
    for suffix in ["/connection", "/location"] {
        for method in not_put {
            let answer = send(
                &s.vm,
                Some(&admin),
                &format!("/v1/resources/{RESOURCE}{suffix}"),
                method,
                Some("{}"),
            );
            assert_api_error(&answer, 405, "method-not-allowed");
        }
    }
    for path in ["/v1/admin/resources", "/v1/admin/resources/devops"] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let answer = send(&s.vm, Some(&admin), path, method, Some("{}"));
            assert_api_error(&answer, 405, "method-not-allowed");
        }
    }
    for method in ["POST", "PUT"] {
        let answer = send(
            &s.vm,
            Some(&admin),
            &format!("/v1/resources/{RESOURCE}"),
            method,
            Some("{}"),
        );
        assert_api_error(&answer, 405, "method-not-allowed");
    }
    // A name that nothing has is `404` on the methods the path takes, and
    // `405` still on the others (the method comes first).
    assert_api_error(
        &send(
            &s.vm,
            Some(&admin),
            "/v1/resources/ghost/connection",
            "PUT",
            Some(r#"{"write":null}"#),
        ),
        404,
        "not-found",
    );
    assert_api_error(
        &send(
            &s.vm,
            Some(&admin),
            "/v1/resources/ghost/connection",
            "GET",
            None,
        ),
        405,
        "method-not-allowed",
    );
    // The new administrators' paths are answered by the administrators only
    // when the group is exactly the program's.
    let in_group = User::new("someone").in_groups(&["staff", ADMINISTRATORS]);
    send(&s.vm, Some(&in_group), "/v1/admin/resources", "GET", None).expect_status(200);
}

#[test]
fn past_64_kib_is_413_for_everyone_on_the_administrators_listings_too() {
    let s = world();
    let over = " ".repeat(64 * 1024 + 1);
    let at = " ".repeat(64 * 1024);
    for path in [
        "/v1/admin/resources".to_owned(),
        format!("/v1/admin/resources/{RESOURCE}"),
    ] {
        for user in [alice(), administrator()] {
            let answer = send(&s.vm, Some(&user), &path, "GET", Some(&over));
            assert_api_error(&answer, 413, "too-large");
        }
        assert_api_error(
            &send(&s.vm, Some(&alice()), &path, "GET", Some(&at)),
            403,
            "not-an-administrator",
        );
        let answer = send(&s.vm, Some(&administrator()), &path, "GET", Some(&at));
        assert_ne!(
            answer.status,
            413,
            "a body of exactly 64 KiB is not too large\n{}",
            answer.describe()
        );
    }
}
