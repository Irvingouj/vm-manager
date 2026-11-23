//! Who is refused on the list (CONTRACT.md, "Outbound calls: The list",
//! last bullets; the model is "What Platform administrators see and stop:
//! Who may" and "Who is refused"): someone not signed in gets the `401` on
//! all three; a User who is not a Platform administrator, signed in
//! however, gets `403` `not-an-administrator` on `PUT` and `DELETE`, the
//! same answer byte for byte whatever the entry (well formed, malformed,
//! there, not there), before anything else is looked at, with nothing
//! changed and no record in the program's log; any other method is `405`;
//! a Platform administrator by a tool credential may.
//!
//! Acceptance criterion (ticket "Outbound calls"): "Only Platform
//! administrators change the list."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{administrator, logged};
use blackbox::assert_api_error;
use blackbox::outbound_surface::{
    HOSTS, add, alice, bob, change_records, entries, path_of, request, start,
};
use blackbox::scenario::ADMINISTRATORS;
use blackbox::tool::bearer;
use blackbox::{Response, User, Vm};

/// Everyone who is not a Platform administrator, signed in: a Contributor
/// of an Application is alice, someone in a Group of a near name, someone
/// with the administrator's id and not its Group.
fn not_administrators() -> Vec<User> {
    vec![
        alice(),
        bob(),
        User::new("mallory").in_groups(&["Platform-Admins", "platform-admins-x"]),
        User::new(&administrator().id),
    ]
}

/// Entries of every kind, for a `PUT` or a `DELETE`: one that is there, one
/// that is not, malformed ones, long ones.
fn entries_of_every_kind() -> Vec<String> {
    vec![
        "there.example.com".into(),
        "not-there.example.com".into(),
        "*.example.org:8443".into(),
        "*.com".into(),
        "https://api.example.com/path".into(),
        "a b".into(),
        "a".repeat(300),
        "[2001:db8::7]:99999".into(),
        "bücher.example".into(),
    ]
}

fn with_one() -> Vm {
    let vm = start();
    vm.api_as(&alice())
        .create_application("board")
        .expect_status(201);
    add(&vm, "there.example.com");
    vm
}

#[test]
fn someone_not_signed_in_gets_the_401_on_all_three() {
    let vm = with_one();
    let mut bodies = Vec::new();
    for (method, path) in [
        ("GET", HOSTS.to_owned()),
        ("PUT", path_of("api.example.com")),
        ("DELETE", path_of("there.example.com")),
        ("PUT", path_of("*.com")),
        ("DELETE", path_of("a b")),
    ] {
        let answer = request(&vm, None, method, &path).send();
        assert_eq!(answer.status, 401, "{method} {path}\n{}", answer.describe());
        assert!(
            answer
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/plain")),
            "expected a plain `401` for {method} {path}\n{}",
            answer.describe()
        );
        bodies.push(answer.body().to_vec());
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "the same plain `401` everywhere"
    );
    assert_eq!(entries(&vm).len(), 1, "nothing changed");
}

fn assert_refused(answer: &Response, what: &str) {
    assert!(
        answer.status == 403,
        "{what}: expected `403` `not-an-administrator`, got\n{}",
        answer.describe()
    );
    assert_api_error(answer, 403, "not-an-administrator");
}

#[test]
fn everyone_else_is_refused_the_same_byte_for_byte_whatever_the_entry_and_nothing_changes() {
    let vm = with_one();
    let before = entries(&vm);
    for user in not_administrators() {
        let mut first: Option<(String, Vec<u8>)> = None;
        for method in ["PUT", "DELETE"] {
            for entry in entries_of_every_kind() {
                let what = format!("{}: {method} {entry:?}", user.id);
                let answer = request(&vm, Some(&user), method, &path_of(&entry)).send();
                assert_refused(&answer, &what);
                match &first {
                    None => first = Some((what, answer.body().to_vec())),
                    Some((was, body)) => assert_eq!(
                        body.as_slice(),
                        answer.body(),
                        "{what}: the refusal differs from that of {was}: nothing in it may depend on the entry\n{}",
                        answer.describe()
                    ),
                }
            }
            // With a body of any kind: refused before it is looked at.
            let answer = request(&vm, Some(&user), method, &path_of("with-body.example.com"))
                .header("content-type", "application/json")
                .body("not json")
                .send();
            assert_refused(&answer, &format!("{}: {method} with a body", user.id));
        }
    }
    assert_eq!(entries(&vm), before, "nothing changed");
    assert_eq!(
        change_records(&vm),
        1,
        "no record of a change for a refused request (the one is the administrator's own setup)"
    );
    assert!(
        logged(&vm, "outbound-host-removed").is_empty(),
        "nothing was removed"
    );
}

#[test]
fn a_user_who_is_not_an_administrator_still_reads_the_list() {
    let vm = with_one();
    for user in not_administrators() {
        let answer = blackbox::outbound_surface::list(&vm, &user);
        assert!(
            answer.status == 200,
            "{}: expected `200` reading the list, got\n{}",
            user.id,
            answer.describe()
        );
    }
}

#[test]
fn an_entry_path_is_the_administrators_whatever_the_method_and_the_collection_takes_get_only() {
    let vm = with_one();
    let admin = administrator();
    let methods = ["GET", "POST", "PATCH", "OPTIONS"];
    for entry in ["there.example.com", "*.com", "x.example.net"] {
        for method in methods {
            // An administrator: 405 for what is not PUT or DELETE.
            let answer = request(&vm, Some(&admin), method, &path_of(entry)).send();
            assert_eq!(
                answer.status,
                405,
                "{method} {entry:?} as a Platform administrator: expected `405`\n{}",
                answer.describe()
            );
            // Anyone else: the refusal, before `405` and `400`.
            for user in [alice(), bob()] {
                let answer = request(&vm, Some(&user), method, &path_of(entry)).send();
                assert_refused(&answer, &format!("{}: {method} {entry:?}", user.id));
            }
        }
    }
    // The collection takes GET only: 405 for anyone signed in.
    for user in [admin, alice(), bob()] {
        for method in ["POST", "PATCH", "PUT", "DELETE"] {
            let answer = request(&vm, Some(&user), method, HOSTS).send();
            assert_eq!(
                answer.status,
                405,
                "{}: {method} {HOSTS}: expected `405`\n{}",
                user.id,
                answer.describe()
            );
        }
    }
    assert_eq!(entries(&vm).len(), 1, "nothing changed");
}

#[test]
fn an_empty_entry_is_malformed_for_put_and_delete_by_an_administrator() {
    let vm = with_one();
    for method in ["PUT", "DELETE"] {
        let answer = request(&vm, Some(&administrator()), method, &format!("{HOSTS}/")).send();
        assert_api_error(&answer, 400, "malformed");
    }
    assert_eq!(entries(&vm).len(), 1, "nothing changed");
}

#[test]
fn not_an_administrator_comes_before_malformed() {
    let vm = with_one();
    let answer = request(&vm, Some(&alice()), "PUT", &path_of("*.com")).send();
    assert_refused(&answer, "alice PUT *.com");
    let answer = request(&vm, Some(&administrator()), "PUT", &path_of("*.com")).send();
    assert_api_error(&answer, 400, "malformed");
}

#[test]
fn a_tool_credential_is_an_administrator_when_its_user_is_and_not_when_it_is_not() {
    let vm = with_one();
    let (admins, _) = vm.api_as(&administrator()).tool_credential();
    let (others, _) = vm.api_as(&alice()).tool_credential();
    let put = bearer(&vm, &path_of("by-credential.example.com"), &admins)
        .method("PUT")
        .send();
    assert!(
        put.status == 200 && put.json()["changed"] == true,
        "expected an administrator's tool credential to add an entry, got\n{}",
        put.describe()
    );
    let refused = bearer(&vm, &path_of("by-other.example.com"), &others)
        .method("PUT")
        .send();
    assert_refused(&refused, "alice's tool credential");
    let refused = bearer(&vm, &path_of("by-credential.example.com"), &others)
        .method("DELETE")
        .send();
    assert_refused(&refused, "alice's tool credential, DELETE");
    let hosts: Vec<String> = entries(&vm)
        .iter()
        .map(|e| e["host"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(hosts, ["by-credential.example.com", "there.example.com"]);
}

#[test]
fn the_administrators_group_is_compared_exactly() {
    let vm = with_one();
    let in_group = User::new("someone").in_groups(&["staff", ADMINISTRATORS]);
    let answer = request(
        &vm,
        Some(&in_group),
        "PUT",
        &path_of("by-group.example.com"),
    )
    .send();
    assert!(
        answer.status == 200,
        "expected a member of the administrators' Group to add an entry, got\n{}",
        answer.describe()
    );
}
