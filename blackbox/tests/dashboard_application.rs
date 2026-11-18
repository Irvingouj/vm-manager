//! One Application's page on the Dashboard (CONTRACT.md, "The Dashboard:
//! One Application"), with the stand-in sign-in: who gets it (`403` naming
//! no Contributor, `404`, a Platform administrator), every Contributor and
//! its address, whom it is shared with only to a Contributor or an
//! administrator, the forms only to a Contributor, and the notice a change
//! leads to.
//!
//! Acceptance criteria: "Each Application shows name, description,
//! Contributors and declared Resources"; "A Contributor shares an
//! Application with a person, a Group or everyone from the Dashboard";
//! "A signed-in person sees the Applications they contribute to and those
//! shared with them, and no others".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::dashboard::{Dashboard, Page, administrator, create, deploy, start};
use blackbox::sharing::{
    add, assert_not_shared_page, contributors_ok, everyone, group, share_ok, user,
};
use blackbox::{DOMAIN, User, Vm};

const APP: &str = "board";
const DESCRIPTION: &str = "secret-board-description";

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["staff"])
}

/// `board`, made by alice with a description and a Version, Contributors
/// alice and the Group `staff`, shared with nobody.
fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some(DESCRIPTION));
    deploy(&vm, &alice(), APP, &[]);
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("staff")]));
    vm
}

/// Shares `board` with bob, the Group `finance`, everyone and someone who
/// never signed in, in that order.
fn share_four(vm: &Vm) {
    share_ok(
        &vm.api_as(&alice()),
        vm,
        APP,
        &add(&[
            user("bob"),
            group("finance"),
            everyone(),
            user("never-signed"),
        ]),
    );
}

const FOUR: [&str; 4] = ["user:bob", "group:finance", "everyone", "user:never-signed"];

fn keys(wanted: &[&str]) -> Vec<String> {
    wanted.iter().map(|k| (*k).to_owned()).collect()
}

#[test]
fn someone_it_is_not_shared_with_gets_403_naming_no_contributor() {
    let vm = with_board();
    let response = Dashboard::new(&vm, &bob()).get(&format!("/applications/{APP}"));
    let page = Page::read(
        response.clone(),
        Dashboard::new(&vm, &bob()).url("/"),
        "bob at board's page",
    );
    assert_not_shared_page(
        &response,
        APP,
        &["bob"],
        &["alice", DESCRIPTION, "staff"],
        "bob, who cannot open board, at its page on the Dashboard",
    );
    assert!(
        page.applications().is_empty() && page.forms().is_empty(),
        "expected nothing of the Application on the `403`: no `data-vm-application`, no form\n{}",
        page.describe()
    );
}

#[test]
fn a_name_no_application_has_or_could_have_is_404() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let long = "a".repeat(64);
    for name in [
        "nope",
        "Board",
        "bad_name",
        "xn--board",
        "api",
        "dashboard",
        "-board",
        long.as_str(),
    ] {
        let page = dashboard.page(
            &format!("/applications/{name}"),
            404,
            &format!("/applications/{name}: a name no Application has, or that none could have"),
        );
        assert!(page.applications().is_empty(), "{}", page.describe());
    }
}

#[test]
fn a_contributor_sees_every_contributor_its_address_and_whom_it_is_shared_with_in_order() {
    let vm = with_board();
    share_four(&vm);
    for person in [alice(), carol()] {
        let page = Dashboard::new(&vm, &person).details(APP);
        let board = page.listed(APP);
        assert_eq!(
            board.relation.as_deref(),
            Some("contributor"),
            "{}",
            page.describe()
        );
        assert_eq!(board.serving.as_deref(), Some("1"), "{}", page.describe());
        assert_eq!(board.description(), Some(DESCRIPTION));
        assert_eq!(
            board.contributor_keys(),
            keys(&["user:alice", "group:staff"]),
            "{}: expected every Contributor, in order\n{}",
            person.id,
            page.describe()
        );
        assert!(
            page.text().contains("board.vm.test"),
            "{}: expected the Application's address on its page\n{}",
            person.id,
            page.describe()
        );
        assert_eq!(
            page.shared_keys(),
            keys(&FOUR),
            "{}: expected whom it is shared with, in the order of the list, each `data-vm-shared` with `data-vm-id` for a person or a Group\n{}",
            person.id,
            page.describe()
        );
    }
}

#[test]
fn a_contributor_gets_one_share_form_and_an_unshare_form_beside_each_entry() {
    let vm = with_board();
    share_four(&vm);
    for person in [alice(), carol()] {
        let page = Dashboard::new(&vm, &person).details(APP);
        let share = page.share_form();
        let mut choices = share.choices("kind");
        choices.sort();
        assert_eq!(
            choices,
            keys(&["email", "everyone", "group", "user"]),
            "{}: expected the share form to offer the four kinds\n{}",
            person.id,
            page.describe()
        );
        assert!(
            share.has_text_input("value"),
            "expected a text input `value`: {share:?}"
        );
        let unshares = page.forms_of("unshare");
        assert_eq!(
            unshares.len(),
            4,
            "{}: expected one unshare form beside each entry\n{}",
            person.id,
            page.describe()
        );
        for form in page.forms_of("share").iter().chain(unshares.iter()) {
            assert_eq!(form.method.as_deref(), Some("post"), "{form:?}");
            let action = page.action_of(form);
            assert!(
                action.host_str() == Some("dashboard.vm.test")
                    && action.path() == format!("/applications/{APP}/sharing")
                    && action.query().is_none(),
                "expected each form's action to be /applications/{APP}/sharing, got {action}"
            );
        }
        for (kind, value) in [
            ("user", "bob"),
            ("group", "finance"),
            ("everyone", ""),
            ("user", "never-signed"),
        ] {
            let form = page.unshare_form(kind, value);
            assert_eq!(
                form.hidden("action"),
                Some("unshare"),
                "expected the unshare form's fields hidden, `action=unshare`: {form:?}"
            );
        }
    }
}

#[test]
fn someone_it_is_shared_with_sees_who_maintains_it_and_not_whom_else_it_is_shared_with() {
    let vm = with_board();
    share_four(&vm);
    let page = Dashboard::new(&vm, &bob()).details(APP);
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("shared"),
        "{}",
        page.describe()
    );
    assert_eq!(
        board.contributor_keys(),
        keys(&["user:alice", "group:staff"]),
        "expected who maintains it\n{}",
        page.describe()
    );
    assert!(
        page.shared().is_empty(),
        "expected no `data-vm-shared` for someone who is not a Contributor\n{}",
        page.describe()
    );
    assert!(
        page.forms().is_empty(),
        "expected no form for someone who is not a Contributor\n{}",
        page.describe()
    );
    let html = page.response.text();
    for other in ["finance", "never-signed"] {
        assert!(
            !html.contains(other),
            "expected nothing of whom else it is shared with, but the page holds {other:?}\n{html}"
        );
    }
    assert!(
        page.text().to_lowercase().contains("contributors"),
        "expected words saying that only its Contributors see whom it is shared with\n{}",
        page.describe()
    );
}

/// Fails unless `page` shows `board` to a Platform administrator who
/// cannot open it as the contract says: relation `none`, no link to open
/// it, the address as text, words saying they read it as a Platform
/// administrator and that it is not shared with them; and no form.
fn assert_administrator_cannot_open(page: &Page, context: &str) {
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("none"),
        "{context}: expected `data-vm-relation=\"none\"` for a Platform administrator who cannot open it\n{}",
        page.describe()
    );
    assert!(
        board.open.is_empty(),
        "{context}: expected no `a[data-vm-open]` for a Platform administrator who cannot open it, got {:?}\n{}",
        board.open,
        page.describe()
    );
    let address = format!("{APP}.{DOMAIN}");
    assert!(
        page.text().contains(&address),
        "{context}: expected the address {address:?} still shown as text\n{}",
        page.describe()
    );
    let text = page.text().to_lowercase();
    assert!(
        text.contains("administrator") && text.contains("not shared"),
        "{context}: expected words saying they read it as a Platform administrator and that it is not shared with them, got {text:?}"
    );
    assert!(
        page.forms().is_empty(),
        "{context}: expected no form for a Platform administrator who is not a Contributor\n{}",
        page.describe()
    );
    assert_eq!(
        board.contributor_keys(),
        keys(&["user:alice", "group:staff"]),
        "{context}"
    );
}

#[test]
fn a_platform_administrator_gets_the_page_and_whom_it_is_shared_with_and_no_form() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());

    // Shared with nobody: they cannot open it.
    let page = dashboard.details(APP);
    assert!(
        page.shared().is_empty() && page.text().to_lowercase().contains("only"),
        "shared with nobody: expected words saying only its Contributors can open it\n{}",
        page.describe()
    );
    assert_administrator_cannot_open(&page, "shared with nobody");

    // Shared with a person and a Group they are not in: still they cannot
    // open it, and they see whom it is shared with.
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), group("finance")]),
    );
    let page = dashboard.details(APP);
    assert_eq!(
        page.shared_keys(),
        keys(&["user:bob", "group:finance"]),
        "expected a Platform administrator to see whom it is shared with\n{}",
        page.describe()
    );
    assert_administrator_cannot_open(&page, "shared with bob and finance, not with them");

    // Shared with everyone: now they can open it, and are shown it as
    // anyone it is shared with.
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[everyone(), user("never-signed")]),
    );
    let page = dashboard.details(APP);
    assert_eq!(
        page.shared_keys(),
        keys(&FOUR),
        "expected a Platform administrator to see whom it is shared with\n{}",
        page.describe()
    );
    assert!(
        page.forms().is_empty(),
        "expected no form for a Platform administrator who is not a Contributor\n{}",
        page.describe()
    );
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("shared"),
        "shared with everyone: expected a Platform administrator shown it as anyone it is shared with\n{}",
        page.describe()
    );
    assert_eq!(
        board.open_href(),
        Some(dashboard.open_href(APP)),
        "shared with everyone: expected the link to open it\n{}",
        page.describe()
    );
    assert_eq!(
        board.contributor_keys(),
        keys(&["user:alice", "group:staff"])
    );
}

#[test]
fn a_platform_administrator_who_can_open_it_is_shown_it_as_anyone() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let admin_id = dashboard.user.id.clone();
    // Shared with the administrator: as anyone it is shared with.
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user(&admin_id)]));
    let page = dashboard.details(APP);
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("shared"),
        "expected a Platform administrator it is shared with to be shown it as anyone it is shared with\n{}",
        page.describe()
    );
    assert_eq!(
        board.open_href(),
        Some(dashboard.open_href(APP)),
        "expected the link to open it\n{}",
        page.describe()
    );
    assert!(
        !page.text().to_lowercase().contains("not shared"),
        "expected no words saying it is not shared with them\n{}",
        page.describe()
    );
    // A Contributor as well: as any Contributor, with the forms.
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user(&admin_id)]));
    let page = dashboard.details(APP);
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("contributor"),
        "expected a Platform administrator who is a Contributor to be shown it as a Contributor\n{}",
        page.describe()
    );
    assert_eq!(board.open_href(), Some(dashboard.open_href(APP)));
    page.share_form();
}

#[test]
fn with_the_stand_in_nobody_is_marked_as_never_seen() {
    let vm = with_board();
    // `never-signed` and `ghost` never made a request here; the stand-in
    // keeps no record of people, so neither is marked.
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("ghost")]));
    share_four(&vm);
    let dashboard = Dashboard::new(&vm, &alice());
    for page in [dashboard.list(), dashboard.details(APP)] {
        assert!(
            page.root().with_attr("data-vm-unseen").is_empty(),
            "expected no element `data-vm-unseen` with the stand-in\n{}",
            page.describe()
        );
        let board = page.listed(APP);
        for shown in board.contributors.iter().chain(page.shared().iter()) {
            let text = shown.text.to_lowercase();
            assert!(
                !(text.contains("seen") || text.contains("check")),
                "expected no words saying someone was not seen signing in, with the stand-in, got {shown:?}"
            );
        }
    }
}

#[test]
fn shared_with_nobody_a_contributor_is_told_only_its_contributors_can_open_it() {
    let vm = with_board();
    let page = Dashboard::new(&vm, &alice()).details(APP);
    assert!(page.shared().is_empty(), "{}", page.describe());
    assert!(
        page.forms_of("unshare").is_empty(),
        "expected no unshare form when it is shared with nobody\n{}",
        page.describe()
    );
    page.share_form();
    let text = page.text().to_lowercase();
    assert!(
        text.contains("only") && text.contains("contributors"),
        "expected words saying that only its Contributors can open it, got {text:?}"
    );
}

#[test]
fn a_notice_says_what_a_change_did() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    for changed in ["shared", "unshared", "nothing"] {
        let page = dashboard.page(
            &format!("/applications/{APP}?changed={changed}"),
            200,
            &format!("?changed={changed}"),
        );
        let notices = page.notices();
        assert_eq!(
            notices,
            vec![changed.to_owned()],
            "expected one element `data-vm-notice={changed:?}`\n{}",
            page.describe()
        );
        let text: String = page
            .root()
            .with_attr("data-vm-notice")
            .first()
            .map(|e| e.text())
            .unwrap_or_default();
        assert!(!text.is_empty(), "expected the notice to say so in words");
    }
    for target in [
        format!("/applications/{APP}?changed=bogus"),
        format!("/applications/{APP}?changed="),
        format!("/applications/{APP}"),
    ] {
        let page = dashboard.page(&target, 200, &target);
        assert!(
            page.notices().is_empty(),
            "{target}: expected no `data-vm-notice`\n{}",
            page.describe()
        );
    }
}

#[test]
fn nothing_of_an_application_the_person_cannot_open_is_on_any_page_of_theirs() {
    let vm = with_board();
    create(&vm, &bob(), "bobs", None);
    let dashboard = Dashboard::new(&vm, &User::new("dave"));
    let mut pages = vec![
        (200, dashboard.get("/")),
        (403, dashboard.get(&format!("/applications/{APP}"))),
        (
            403,
            dashboard.get(&format!("/applications/{APP}?changed=shared")),
        ),
        (403, dashboard.get("/applications/bobs")),
        (404, dashboard.get("/no-such-page")),
    ];
    pages.push((
        403,
        dashboard.post_form(
            &Dashboard::sharing_target(APP),
            "action=share&kind=user&value=dave",
        ),
    ));
    for (status, response) in &pages {
        assert_eq!(
            response.status,
            *status,
            "expected `{status}` for {}\n{}",
            response.request(),
            response.describe()
        );
        Page::read(response.clone(), dashboard.url("/"), response.request());
    }
    let pages: Vec<_> = pages.into_iter().map(|(_, r)| r).collect();
    for response in &pages {
        let html = response.text();
        for hidden in ["alice", DESCRIPTION, "staff", "\"bob\""] {
            assert!(
                !html.contains(hidden),
                "expected nothing of an Application dave cannot open on any page of his, but the answer to {} holds {hidden:?}\n{html}",
                response.request()
            );
        }
    }
}
