//! Opening an Application (CONTRACT.md, "Contributors and sharing: Opening
//! an Application", "The two lists"; ticket "Contributors and sharing: who
//! may change and who may open an Application"), with the stand-in
//! sign-in:
//!
//! - a new Application is opened by its Contributors and by nobody else;
//! - after sharing with a person, a Group or everyone, exactly those people
//!   open it, and after unsharing they do not, from the very next request;
//! - a person it is not shared with gets a `403` that says so (a page for a
//!   browser, plain text otherwise), never the Application: none of its
//!   code runs, whatever the path and the method, and the `request` record
//!   says so;
//! - the `403` comes after the `401` and the `404` of an unknown address,
//!   and before the `404` of an Application with no serving Version;
//! - a Platform administrator who is not a Contributor opens it only when
//!   it is shared with them;
//! - being able to open gives nothing on a Resource: a User with no Grant
//!   is refused by it as before, with its Operation log entry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::version_body_declaring;
use blackbox::scenario::{
    ADMINISTRATORS, CONTACT, Expected, READERS, RESOURCE, Scenario, administrator, assert_entry,
    assert_refusal,
};
use blackbox::sharing::{
    TRACED, add, assert_cannot_open, assert_not_shared_page, assert_not_shared_plain,
    assert_not_shared_record, assert_opens, assert_own_page_headers, contributors_ok,
    create_unshared, everyone, group, open, remove, share_ok, user,
};
use blackbox::{INSTALLER, User, Vm, VmOptions, fixture_bundle};
use serde_json::json;

const APP: &str = "board";

fn vm() -> Vm {
    Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    )
}

/// The creator, and so the first Contributor.
fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["finance"])
}

fn dave() -> User {
    User::new("dave").in_groups(&["staff", "finance"])
}

fn erin() -> User {
    User::new("erin").in_groups(&["Finance", "finance-team", " finance"])
}

/// A program with `board` (`fixtures/honest.js`) made by alice and shared
/// with nobody.
fn with_board() -> Vm {
    let vm = vm();
    create_unshared(&vm, &alice(), APP, TRACED);
    vm
}

#[test]
fn a_new_application_is_opened_by_its_contributors_and_by_nobody_else() {
    let vm = with_board();
    assert_opens(&vm, &alice(), APP, "its creator");
    for other in [
        bob(),
        carol(),
        dave(),
        administrator(),
        User::new("ALICE"),
        User::new("alice ").in_groups(&["staff"]),
        User::new(INSTALLER),
    ] {
        assert_cannot_open(&vm, &other, APP, &format!("{other:?} on a new Application"));
    }
}

#[test]
fn a_person_added_as_a_contributor_opens_it() {
    let vm = with_board();
    assert_cannot_open(&vm, &bob(), APP, "bob before");
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, a Contributor");
    assert_cannot_open(&vm, &carol(), APP, "carol, still nobody's");
}

#[test]
fn the_members_of_a_group_that_is_a_contributor_open_it() {
    let vm = with_board();
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("finance")]));
    assert_opens(&vm, &carol(), APP, "carol, in finance");
    assert_opens(&vm, &dave(), APP, "dave, in finance");
    assert_cannot_open(&vm, &bob(), APP, "bob, in no Group");
    assert_cannot_open(&vm, &erin(), APP, "erin, in Groups only like finance");
}

#[test]
fn after_sharing_with_a_person_exactly_that_person_opens_it() {
    let vm = with_board();
    assert_cannot_open(&vm, &bob(), APP, "bob before the sharing");
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, shared with");
    for other in [
        carol(),
        User::new("Bob"),
        User::new("bob2"),
        User::new("carol").in_groups(&["bob"]),
    ] {
        assert_cannot_open(&vm, &other, APP, &format!("{other:?}, not bob"));
    }
    assert_opens(&vm, &alice(), APP, "alice, still a Contributor");
}

#[test]
fn after_sharing_with_a_group_exactly_its_members_open_it() {
    let vm = with_board();
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("finance")]));
    assert_opens(&vm, &carol(), APP, "carol, in finance");
    assert_opens(&vm, &dave(), APP, "dave, in staff and finance");
    for other in [
        bob(),
        erin(),
        User::new("finance"),
        User::new("frank").in_groups(&["FINANCE"]),
    ] {
        assert_cannot_open(
            &vm,
            &other,
            APP,
            &format!("{other:?}, not in finance (compared exactly)"),
        );
    }
}

#[test]
fn after_sharing_with_everyone_every_signed_in_user_opens_it_and_nobody_signed_out_does() {
    let vm = with_board();
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[everyone()]));
    for other in [
        bob(),
        carol(),
        erin(),
        administrator(),
        User::new("zoë-数据"),
        User::new(&"u".repeat(256)),
    ] {
        assert_opens(&vm, &other, APP, &format!("{other:?}, everyone"));
    }
    let signed_out = vm.request(APP, "/hello").send();
    assert_eq!(
        signed_out.status,
        401,
        "everyone means everyone who can sign in: not signed in is still `401`\n{}",
        signed_out.describe()
    );
}

#[test]
fn unsharing_a_person_takes_effect_at_the_very_next_request() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    share_ok(&api, &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, shared with");
    share_ok(&api, &vm, APP, &remove(&[user("bob")]));
    assert_cannot_open(
        &vm,
        &bob(),
        APP,
        "bob, at the request right after the unsharing",
    );
    share_ok(&api, &vm, APP, &add(&[user("bob")]));
    assert_opens(
        &vm,
        &bob(),
        APP,
        "bob, at the request right after sharing again",
    );
}

#[test]
fn unsharing_a_group_or_everyone_takes_effect_at_the_very_next_request() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    share_ok(&api, &vm, APP, &add(&[group("finance"), everyone()]));
    assert_opens(&vm, &carol(), APP, "carol, in finance");
    assert_opens(&vm, &bob(), APP, "bob, everyone");
    share_ok(&api, &vm, APP, &remove(&[everyone()]));
    assert_cannot_open(&vm, &bob(), APP, "bob, after everyone was unshared");
    assert_opens(&vm, &carol(), APP, "carol, still through finance");
    share_ok(&api, &vm, APP, &remove(&[group("finance")]));
    assert_cannot_open(&vm, &carol(), APP, "carol, after finance was unshared");
    assert_opens(&vm, &alice(), APP, "alice, a Contributor all along");
}

#[test]
fn a_contributor_removed_cannot_open_it_unless_it_is_shared_with_them() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, a Contributor");
    contributors_ok(&api, &vm, APP, &remove(&[user("bob")]));
    assert_cannot_open(&vm, &bob(), APP, "bob, no longer a Contributor");
    share_ok(&api, &vm, APP, &add(&[group("finance")]));
    assert_cannot_open(&vm, &bob(), APP, "bob, in no Group it is shared with");
}

#[test]
fn the_refusal_for_a_request_that_is_not_a_page_is_plain_text_that_says_so() {
    let vm = with_board();
    for accept in [
        None,
        Some("application/json"),
        Some("*/*"),
        Some("text/plain"),
    ] {
        let mut request = vm.request(APP, "/").as_user(&bob());
        if let Some(accept) = accept {
            request = request.header("accept", accept);
        }
        let response = request.send();
        assert_not_shared_plain(&vm, &response, APP, "bob", &format!("Accept {accept:?}"));
        assert!(
            !response.text().contains("alice"),
            "expected the refusal to name no Contributor, got {:?}",
            response.text()
        );
    }
    // A page is asked for only by `GET` and `HEAD`: a `POST` that accepts
    // HTML gets the plain text.
    let posted = vm
        .request(APP, "/")
        .as_user(&bob())
        .method("POST")
        .header("accept", "text/html")
        .body("x")
        .send();
    assert_not_shared_plain(&vm, &posted, APP, "bob", "a POST that accepts text/html");
}

#[test]
fn a_browser_is_answered_with_a_page_of_the_platforms_own_naming_the_person_and_no_contributor() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application_with(&json!({
        "name": APP,
        "description": "Description-only-its-Contributors-know",
    }))
    .expect_status(201);
    api.send_bundle(APP, &fixture_bundle(TRACED))
        .expect_status(201);
    contributors_ok(
        &api,
        &vm,
        APP,
        &add(&[user("contributor-carl"), group("board-team")]),
    );
    share_ok(
        &api,
        &vm,
        APP,
        &add(&[user("someone-else"), group("readers-of-board")]),
    );
    for accept in [
        "text/html",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "application/json, text/html;q=0.1",
    ] {
        let response = vm
            .request(APP, "/some/page?x=1")
            .as_user(&bob())
            .header("accept", accept)
            .send();
        assert_not_shared_page(
            &response,
            APP,
            &["bob"],
            &[
                "alice",
                "contributor-carl",
                "board-team",
                "someone-else",
                "readers-of-board",
                "Description-only-its-Contributors-know",
            ],
            &format!("Accept {accept:?}"),
        );
        assert_not_shared_record(&vm, &response, APP, "bob", "the page");
    }
}

#[test]
fn a_head_that_asks_for_a_page_is_the_pages_403_without_a_body() {
    let vm = with_board();
    let response = vm
        .request(APP, "/")
        .as_user(&bob())
        .method("HEAD")
        .header("accept", "text/html")
        .send();
    assert_eq!(response.status, 403, "{}", response.describe());
    assert_own_page_headers(&response, "HEAD of the page");
    assert!(
        response.body().is_empty(),
        "expected no body for HEAD, got\n{}",
        response.describe()
    );
    assert_not_shared_record(&vm, &response, APP, "bob", "HEAD of the page");
}

#[test]
fn the_page_escapes_the_persons_id_as_html() {
    let vm = with_board();
    let tricky = User::new("<b>bob</b>&\"x\"");
    let response = vm
        .request(APP, "/")
        .as_user(&tricky)
        .header("accept", "text/html")
        .send();
    assert_eq!(response.status, 403, "{}", response.describe());
    let html = response.text();
    assert!(
        !html.contains("<b>bob</b>"),
        "expected the id to be written as text of the page (escaped), got\n{html}"
    );
    assert!(
        html.contains("&lt;b&gt;bob&lt;/b&gt;"),
        "expected the page to name the person by their id, escaped for HTML, got\n{html}"
    );
}

#[test]
fn none_of_its_code_runs_whatever_the_path_and_the_method() {
    let vm = with_board();
    let paths = [
        "/",
        "/hello",
        "/print",
        "/index.html",
        "/static/app.js",
        "/favicon.ico",
        "/_vm/anything",
        "/_vm/session",
        "/a/b/c?d=e",
        "/%2e%2e/etc/passwd",
    ];
    let methods = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
    for path in paths {
        for method in methods {
            let mut request = vm.request(APP, path).as_user(&bob()).method(method);
            if !matches!(method, "GET" | "HEAD" | "OPTIONS") {
                request = request
                    .header("content-type", "application/json")
                    .body("{\"calls\":[]}");
            }
            let response = request.send();
            let context = format!("{method} {path}");
            // `/_vm/session` too: with the stand-in it is the
            // Application's own path, under the rule like any other.
            assert_eq!(
                response.status,
                403,
                "{context}: expected `403`, got\n{}",
                response.describe()
            );
            assert!(
                !response.text().contains("honest") && !response.text().contains("Hello"),
                "{context}: expected nothing of the Application in the answer, got\n{}",
                response.describe()
            );
            assert_not_shared_record(&vm, &response, APP, "bob", &context);
        }
    }
    // The Application's code was given no request of bob's at all: its
    // log has no entry of his.
    let control = open(&vm, &alice(), APP);
    assert_eq!(control.status, 200, "{}", control.describe());
    let entries = blackbox::logs::wait_for_entry(&vm.api_as(&alice()), APP, &control.request_id());
    let page = blackbox::logs::page(&vm.api_as(&alice()), APP, "?limit=1000");
    assert!(
        page.entries.iter().all(|e| e["user"] != "bob"),
        "expected no log entry of a request of bob's, got {:?} (control {:?})",
        page.entries,
        entries.0
    );
}

#[test]
fn the_403_comes_after_the_401_and_the_404_of_an_unknown_address_and_before_the_404_of_no_version()
{
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("empty").expect_status(201);
    create_unshared(&vm, &alice(), APP, TRACED);
    // Not signed in: 401, whatever the Application.
    for name in [APP, "empty", "nothing-here"] {
        let response = vm.request(name, "/").send();
        assert_eq!(response.status, 401, "{name}: {}", response.describe());
    }
    // Signed in, an address that is no Application's: 404.
    let unknown = open(&vm, &bob(), "nothing-here");
    assert_eq!(unknown.status, 404, "{}", unknown.describe());
    let record = vm.records_of(&unknown).request;
    assert!(
        record["application"].is_null(),
        "an unknown address names no Application, got {record}"
    );
    // A known Application with no Version, not shared with bob: 403
    // before the 404 of no Version.
    let no_version = open(&vm, &bob(), "empty");
    assert_not_shared_plain(
        &vm,
        &no_version,
        "empty",
        "bob",
        "an Application with no Version",
    );
    // Its Contributor gets the 404 of no Version.
    let contributor = open(&vm, &alice(), "empty");
    assert_eq!(
        contributor.status,
        404,
        "its Contributor gets the 404 of no Version\n{}",
        contributor.describe()
    );
    // A malformed sign-in is 400 before anything.
    let malformed = vm
        .request(APP, "/")
        .header("x-vm-stand-in-user", "bob")
        .header("x-vm-stand-in-user", "carol")
        .send();
    assert_eq!(malformed.status, 400, "{}", malformed.describe());
}

#[test]
fn a_platform_administrator_who_is_not_a_contributor_opens_it_only_when_it_is_shared_with_them() {
    let vm = with_board();
    let admin = administrator();
    assert_cannot_open(
        &vm,
        &admin,
        APP,
        "a Platform administrator, not shared with",
    );
    let api = vm.api_as(&alice());
    share_ok(&api, &vm, APP, &add(&[group(ADMINISTRATORS)]));
    assert_opens(
        &vm,
        &admin,
        APP,
        "a Platform administrator, shared with by their Group",
    );
    share_ok(&api, &vm, APP, &remove(&[group(ADMINISTRATORS)]));
    assert_cannot_open(&vm, &admin, APP, "a Platform administrator, unshared again");
    share_ok(&api, &vm, APP, &add(&[user(&admin.id)]));
    assert_opens(
        &vm,
        &admin,
        APP,
        "a Platform administrator, shared with by id",
    );
}

#[test]
fn being_able_to_open_gives_nothing_on_a_resource_held_by_no_grant() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, READERS, "read");
    let installer = s.vm.api_as(&User::new(INSTALLER));
    installer.create_application(APP).expect_status(201);
    installer
        .send_version(
            APP,
            &version_body_declaring(&fixture_bundle("query.js"), &[(RESOURCE, "read")]),
        )
        .expect_status(201);
    share_ok(
        &installer,
        &s.vm,
        APP,
        &add(&[user("nobody"), group("finance")]),
    );
    let ask = json!({ "resource": RESOURCE, "sql": "select id, title from incidents" });
    for opener in [
        User::new("nobody"),
        User::new("fin").in_groups(&["finance"]),
    ] {
        let (response, answer) = s.query(&opener, APP, &ask);
        assert_refusal(
            &answer,
            "no-grant",
            &json!({
                "resource": RESOURCE, "user": opener.id, "needed": "read", "held": null,
                "contact": CONTACT,
            }),
            &format!("{} opens {APP} and holds no Grant", opener.id),
        );
        assert_entry(
            &s.entry_of(&response),
            &Expected::refused(&response.request_id(), &opener.id, APP, "no-grant"),
            "the Operation log entry of the refusal",
        );
    }
    // Someone it is shared with who holds the Grant reads.
    share_ok(&installer, &s.vm, APP, &add(&[group(READERS)]));
    let (_, answer) = s.query(&User::new("rita").in_groups(&[READERS]), APP, &ask);
    assert!(
        answer.get("ok").is_some(),
        "expected a reader it is shared with to read, got {answer}"
    );
}

#[test]
fn someone_who_cannot_open_it_reaches_no_resource_through_it() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, READERS, "read");
    let installer = s.vm.api_as(&User::new(INSTALLER));
    installer.create_application(APP).expect_status(201);
    installer
        .send_version(
            APP,
            &version_body_declaring(&fixture_bundle("query.js"), &[(RESOURCE, "read")]),
        )
        .expect_status(201);
    let before = s.log().len();
    let rita = User::new("rita").in_groups(&[READERS]);
    let response =
        s.vm.request(APP, "/")
            .as_user(&rita)
            .method("POST")
            .json(&json!({ "calls": [ { "resource": RESOURCE, "sql": "select 1" } ] }))
            .send();
    assert_not_shared_plain(
        &s.vm,
        &response,
        APP,
        "rita",
        "a reader it is not shared with",
    );
    assert_eq!(
        s.log().len(),
        before,
        "expected no Operation log entry for a request the Application was never given"
    );
}
