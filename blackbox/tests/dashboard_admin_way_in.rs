//! The way in to the Dashboard's pages for Platform administrators
//! (CONTRACT.md, "The Dashboard for Platform administrators: Who may" and
//! "The way in"), with the stand-in sign-in and with OIDC: on every page of
//! the Dashboard a Platform administrator's header holds one
//! `a[data-vm-admin]` to `/admin/applications`, named "Administration",
//! with its mark; nobody else's page holds anything of it (no
//! `data-vm-admin`, no `data-vm-admin-sections`, no link under `/admin`,
//! not the word); it follows the person's Groups, decided at each request;
//! each administrator's page under `/admin` but the confirmation of a
//! disable has one `nav[data-vm-admin-sections]` with the four sections,
//! its own `aria-current="page"`, and an Application's page has none.
//!
//! Acceptance criterion (ticket "The Dashboard for Platform
//! administrators"): "A Platform administrator finds the administrator's
//! pages from the Dashboard's header; nobody else sees the way in."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin_pages::{
    APPLICATIONS, assert_admin_link, assert_no_admin_trace, assert_no_sections, assert_sections,
    disable_target, enable_target, pages_with_sections,
};
use blackbox::dashboard::{
    Dashboard, Page, administrator, create, deploy, form_body, oidc_page, oidc_sign_in, start,
};
use blackbox::oidc::Person;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, Installation, grace};
use blackbox::sharing::{add, share_ok, user};
use blackbox::{Response, User, Vm};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// `board` made by alice with a Version, shared with bob; `draft` made by
/// alice with none.
fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some("The incident board"));
    deploy(&vm, &alice(), APP, &[]);
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    create(&vm, &alice(), "draft", None);
    vm
}

/// Every kind of page of the Dashboard as this person gets it, with the
/// status expected: the list, an Application's page, a refusal of a change
/// of sharing, a `404`, a `405`, an Application of no name it has.
fn every_kind_of_page(dashboard: &Dashboard<'_>) -> Vec<(String, u16, Response)> {
    let share = form_body(&[("action", "share"), ("kind", "user"), ("value", "carol")]);
    vec![
        ("the list".into(), 200, dashboard.get("/")),
        (
            "an Application's page".into(),
            200,
            dashboard.get(&format!("/applications/{APP}")),
        ),
        (
            "an Application's page with a notice".into(),
            200,
            dashboard.get(&format!("/applications/{APP}?changed=shared")),
        ),
        (
            "a path the Dashboard has not".into(),
            404,
            dashboard.get("/nope"),
        ),
        (
            "an Application of no name it has".into(),
            404,
            dashboard.get("/applications/nope"),
        ),
        (
            "another method".into(),
            405,
            dashboard.navigation("DELETE", "/").send(),
        ),
        (
            "a refused change of sharing".into(),
            if dashboard.user.id == "alice" {
                404
            } else {
                403
            },
            if dashboard.user.id == "alice" {
                dashboard.post_form("/applications/nope/sharing", &share)
            } else {
                dashboard.post_form(&format!("/applications/{APP}/sharing"), &share)
            },
        ),
    ]
}

#[test]
fn a_platform_administrators_header_holds_the_way_in_on_every_page_of_the_dashboard() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let mut pages = every_kind_of_page(&dashboard);
    for (path, _) in pages_with_sections(APP) {
        pages.push((
            format!("the administrator's page {path}"),
            200,
            dashboard.get(&path),
        ));
    }
    pages.extend([
        (
            "a path under /admin it has not".to_owned(),
            404,
            dashboard.get("/admin/nope"),
        ),
        (
            "another method on an administrator's page".to_owned(),
            405,
            dashboard.navigation("DELETE", APPLICATIONS).send(),
        ),
        (
            "GET of where an enable is posted".to_owned(),
            405,
            dashboard.get(&enable_target(APP)),
        ),
        (
            "the confirmation of a disable of no Application".to_owned(),
            404,
            dashboard.get(&disable_target("nope")),
        ),
        (
            "a malformed place in the list".to_owned(),
            400,
            dashboard.get("/admin/applications?after=Not_A_Name"),
        ),
    ]);
    for (what, status, response) in pages {
        assert_eq!(
            response.status,
            status,
            "{what}: expected `{status}`\n{}",
            response.describe()
        );
        let page = Page::read(response, dashboard.url("/"), &what);
        assert_admin_link(&page, &what);
    }
}

#[test]
fn nobody_else_has_anything_of_the_way_in_on_any_page() {
    let vm = with_board();
    for person in [
        alice(),
        bob(),
        User::new("carol").in_groups(&["platform-admins-not"]),
    ] {
        let dashboard = Dashboard::new(&vm, &person);
        let mut pages = every_kind_of_page(&dashboard);
        pages.push((
            "the refusal of an administrator's page".to_owned(),
            403,
            dashboard.get(APPLICATIONS),
        ));
        for (what, status, response) in pages {
            let what = format!("{what}, as {}", person.id);
            assert_eq!(
                response.status,
                status,
                "{what}: expected `{status}`\n{}",
                response.describe()
            );
            let page = Page::read(response, dashboard.url("/"), &what);
            assert_no_admin_trace(&page, &what);
        }
    }
}

#[test]
fn the_way_in_follows_the_groups_of_each_request_with_the_stand_in() {
    let vm = with_board();
    // The same user id, in the administrators' Group or not, request by
    // request: nothing is remembered between two requests.
    let id = administrator().id;
    let without = User::new(&id).in_groups(&["staff"]);
    for round in 0..2 {
        let page = Dashboard::new(&vm, &administrator()).list();
        assert_admin_link(&page, &format!("round {round}, in the Group"));
        let page = Dashboard::new(&vm, &without).list();
        assert_no_admin_trace(&page, &format!("round {round}, out of the Group"));
    }
}

#[test]
fn each_administrators_page_has_the_four_sections_its_own_current_and_others_have_none() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for (path, section) in pages_with_sections(APP) {
        let page = dashboard.page(&path, 200, &path);
        match section {
            Some(section) => assert_sections(&page, section, &path),
            None => assert_no_sections(&page, &format!("{path}: the confirmation of a disable")),
        }
    }
    // With a query, the same section is current.
    let page = dashboard.page(
        "/admin/operation-log?outcome=refused",
        200,
        "a filtered log",
    );
    assert_sections(&page, "operation-log", "a filtered Operation log");
    let page = dashboard.page(&format!("/admin/applications?after={APP}"), 200, "page two");
    assert_sections(&page, "applications", "a later page of every Application");
    // An Application's page, as a Platform administrator sees it: no
    // sections; the list: none.
    for target in ["/".to_owned(), format!("/applications/{APP}")] {
        let page = dashboard.page(&target, 200, &target);
        assert_no_sections(&page, &target);
    }
}

#[test]
fn get_admin_is_303_to_every_application() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let answer = dashboard.get("/admin");
    let to = answer
        .header("location")
        .and_then(|l| dashboard.url("/admin").join(l).ok());
    assert!(
        answer.status == 303
            && to
                .as_ref()
                .is_some_and(|u| u.path() == APPLICATIONS && u.query().is_none()),
        "expected `303` to {APPLICATIONS}\n{}",
        answer.describe()
    );
}

/// Ada, a Platform administrator or not.
fn ada_in(groups: &[&str]) -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(groups)
}

#[test]
fn with_oidc_a_person_put_into_the_group_has_the_way_in_from_the_next_renewal_on() {
    let at = Installation::start_with(
        &[],
        &[ada_in(&["finance"]), grace()],
        &["--session-renewal", "3"],
    );
    let mut browser = at.browser_as(ADA);
    let (_, page) = oidc_sign_in(&at, &mut browser);
    let signed_in = Instant::now();
    assert_no_admin_trace(&page, "Ada, not a Platform administrator, after signing in");
    at.provider.set_groups(ADA, &["finance", ADMINISTRATORS]);
    let before = oidc_page(&at, &mut browser, "/", 200, "before the renewal");
    assert!(
        signed_in.elapsed() < Duration::from_secs(3),
        "the test was too slow to look before the renewal ({:?})",
        signed_in.elapsed()
    );
    assert_no_admin_trace(&before, "put into the Group, before the session's renewal");
    let until = signed_in + Duration::from_secs(3) + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
    let after = oidc_page(&at, &mut browser, "/", 200, "after the renewal");
    assert_admin_link(&after, "put into the Group, after the session's renewal");
    let admin = oidc_page(&at, &mut browser, APPLICATIONS, 200, "every Application");
    assert_sections(&admin, "applications", "every Application, with OIDC");
    assert_admin_link(&admin, "every Application, with OIDC");
}
