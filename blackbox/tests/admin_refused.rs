//! Who is refused (CONTRACT.md, "What Platform administrators see and
//! stop: Who may", "Who is refused"), with the stand-in sign-in: a
//! signed-in User who is not a Platform administrator, a Contributor of the
//! Application among them, is refused `403` `not-an-administrator` on every
//! request of this section, the same byte for byte whatever the
//! parameters, the body and the names (one an Application has, one it has
//! not, one no Application could have); nothing changes and the program's
//! log has no record of a change; someone not signed in gets the `401`;
//! only `too-large` comes before.
//!
//! Acceptance criterion: "Someone who is not a Platform administrator can
//! do none of this."

use blackbox::admin::{self, LIST, OPERATION_LOG, administrator, disable_ok, list_page, logged};
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sharing::{TRACED, create_unshared};
use blackbox::{Request, Response, User, Vm, assert_api_error};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// Everyone who is not a Platform administrator, signed in: alice (the
/// Contributor of `board`), someone in a Group of a near name, someone of
/// the administrator's id without its Group.
fn not_administrators() -> Vec<User> {
    vec![
        alice(),
        User::new("mallory").in_groups(&["Platform-Admins", "platform-admins-x"]),
        User::new(&administrator().id),
    ]
}

/// Every request of this section, with names, parameters and bodies of
/// every kind: (path, method, body).
fn requests() -> Vec<(String, &'static str, Option<String>)> {
    let mut all: Vec<(String, &'static str, Option<String>)> = Vec::new();
    for query in [
        "",
        "?limit=5",
        "?limit=0",
        "?after=Not_A_Name",
        "?after=board&after=b",
    ] {
        all.push((format!("{LIST}{query}"), "GET", None));
    }
    all.push((LIST.to_owned(), "POST", Some("{}".to_owned())));
    for query in [
        "",
        "?user=alice",
        "?limit=abc",
        "?since=yesterday",
        "?application=board&outcome=x",
    ] {
        all.push((format!("{OPERATION_LOG}{query}"), "GET", None));
    }
    all.push((OPERATION_LOG.to_owned(), "DELETE", None));
    for name in ["board", "stopped", "nothing-here", "Not_A_Name"] {
        for action in ["disable", "enable"] {
            let path = format!("/v1/applications/{name}/{action}");
            for body in ["{}", "", "[1]", "not json"] {
                all.push((path.clone(), "POST", Some(body.to_owned())));
            }
            all.push((path, "GET", None));
        }
    }
    all
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

/// The program with `board` (alice's) and `stopped` (alice's, disabled).
fn with_two() -> Vm {
    let vm = admin::start();
    create_unshared(&vm, &alice(), "board", TRACED);
    create_unshared(&vm, &alice(), "stopped", TRACED);
    disable_ok(&vm, "stopped");
    vm
}

#[test]
fn everyone_else_is_refused_the_same_byte_for_byte_and_nothing_changes() {
    let vm = with_two();
    let before = list_page(&vm, "");
    let records_before = (
        logged(&vm, "application-disabled").len(),
        logged(&vm, "application-enabled").len(),
    );
    for user in not_administrators() {
        let mut bodies: Vec<(String, Vec<u8>)> = Vec::new();
        for (path, method, body) in requests() {
            let answer = send(&vm, Some(&user), &path, method, body.as_deref());
            assert_api_error(&answer, 403, "not-an-administrator");
            bodies.push((format!("{method} {path} {body:?}"), answer.body().to_vec()));
        }
        let first = bodies[0].1.clone();
        for (what, body) in &bodies {
            assert_eq!(
                body,
                &first,
                "{}: the refusal of {what} differs from that of {}: nothing in it may depend on the request\n{}\n{}",
                user.id,
                bodies[0].0,
                String::from_utf8_lossy(body),
                String::from_utf8_lossy(&first)
            );
        }
    }
    assert_eq!(
        list_page(&vm, "")["applications"],
        before["applications"],
        "nothing changed"
    );
    assert_eq!(
        (
            logged(&vm, "application-disabled").len(),
            logged(&vm, "application-enabled").len()
        ),
        records_before,
        "no record of a change for a refused request"
    );
    // The Contributor still cannot enable their own Application, nor
    // disable it.
    let opened = vm
        .request("board", "/hello")
        .as_user(&alice())
        .header("accept", "application/json")
        .send();
    assert_eq!(opened.status, 200, "{}", opened.describe());
}

#[test]
fn someone_not_signed_in_gets_the_401_and_too_large_comes_first() {
    let vm = with_two();
    let mut bodies = Vec::new();
    for (path, method, body) in requests() {
        let answer = send(&vm, None, &path, method, body.as_deref());
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
    for path in [
        "/v1/applications/board/disable",
        "/v1/applications/nothing-here/enable",
    ] {
        let large = vm
            .api_request(path)
            .as_user(&alice())
            .method("POST")
            .header("content-type", "application/json")
            .body(vec![b' '; 16 * 1024 * 1024 + 1])
            .send();
        assert_api_error(&large, 413, "too-large");
    }
}

#[test]
fn an_administrator_by_the_stand_in_group_may_and_the_group_is_compared_exactly() {
    let vm = with_two();
    let in_group = User::new("someone").in_groups(&["staff", ADMINISTRATORS]);
    let listed = admin::list(&vm, &in_group, "");
    assert_eq!(listed.status, 200, "{}", listed.describe());
    admin::operation_log(&vm, &in_group, "").expect_status(200);
    admin::disable(&vm, &in_group, "board").expect_status(200);
    admin::enable(&vm, &in_group, "board").expect_status(200);
    // A program started with another administrators' Group: the same
    // Group name is nobody's there.
    let other = blackbox::Vm::start(
        &blackbox::VmOptions::new()
            .stand_in_sign_in()
            .administrators_group("it-team"),
    );
    assert_api_error(
        &admin::list(&other, &administrator(), ""),
        403,
        "not-an-administrator",
    );
    let it = User::new("ian").in_groups(&["it-team"]);
    admin::list(&other, &it, "").expect_status(200);
}
