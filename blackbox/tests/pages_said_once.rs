//! What can be pinned of the product owner's four rules for every page
//! (CONTRACT.md, "How the pages look: Said once, and without words where
//! that can be done", with "The Dashboard: The list" and "One
//! Application"): the Dashboard's list holds no Contributor, no Resource
//! and no one it is shared with, and the person's id once, in the header;
//! an Application's own page holds what the list leaves out; a page of the
//! sign-in address says which installation it is; the local run's
//! test-User page says which Application it is for. (That every `<svg>`
//! has a name or is hidden beside its words is measured on the pages of
//! the walk, `browser_look.rs`.)
//!
//! Acceptance criteria (ticket "The pages follow the product owner's four
//! UI/UX rules"): "On no page is a piece of information said twice"; "The
//! Dashboard's list shows what is needed to choose and open an
//! Application; the rest is on the Application's page or behind a
//! disclosure".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::browser::url;
use blackbox::dashboard::{
    Dashboard, Page, assert_attention, assert_attention_attribute,
    assert_list_holds_nothing_of_an_applications_own_page, create, deploy, start,
};
use blackbox::dev::Dev;
use blackbox::local::{Local, local_project};
use blackbox::page::Document;
use blackbox::sessions::{ADA, Installation, ada};
use blackbox::sharing::{add, contributors_ok, everyone, group, share_ok, user};
use blackbox::tool_sign_ins::Challenge;
use blackbox::{DOMAIN, Response, User};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// Fails unless the person's id is in the text of the list once, and that
/// once inside the element `data-vm-user`.
fn assert_id_once_in_the_header(page: &Page, id: &str, context: &str) {
    let text = page.body_text();
    assert_eq!(
        text.matches(id).count(),
        1,
        "{context}: expected the signed-in person's id {id:?} on the list once, in the header; its text is {text:?}"
    );
    let (_, header) = page.user();
    assert!(
        header.contains(id),
        "{context}: expected the one {id:?} in the element `data-vm-user`, whose text is {header:?}"
    );
}

#[test]
fn the_list_holds_no_contributor_no_resource_no_sharing_and_the_persons_id_once_in_the_header() {
    let vm = start();
    create(&vm, &alice(), "ledger", Some("The quarterly ledger"));
    deploy(&vm, &alice(), "ledger", &[("payments", "read")]);
    contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        "ledger",
        &add(&[group("staff"), user("carol")]),
    );
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        "ledger",
        &add(&[user("bob"), everyone()]),
    );
    create(&vm, &alice(), "draft", None);

    for (person, relation) in [(alice(), "contributor"), (bob(), "shared")] {
        let context = format!("{}'s list", person.id);
        let list = Dashboard::new(&vm, &person).list();
        assert_eq!(
            list.listed("ledger").relation.as_deref(),
            Some(relation),
            "the harness's own check\n{}",
            list.describe()
        );
        assert_list_holds_nothing_of_an_applications_own_page(&list, &context);
        assert_id_once_in_the_header(&list, &person.id, &context);
        let text = list.body_text();
        for absent in ["carol", "payments"] {
            assert!(
                !text.contains(absent),
                "{context}: expected no Contributor, Group or Resource named on the list, but it names {absent:?}\n{}",
                list.describe()
            );
        }
        // The details are a page away: the name leads there.
        let ledger = list.listed("ledger");
        assert!(
            ledger.details_href().is_some(),
            "{context}: expected the name to be the link `a[data-vm-details]`\n{}",
            list.describe()
        );
        assert_attention(&ledger, "not-registered", &context);
    }
}

#[test]
fn an_applications_page_holds_its_version_its_address_and_every_contributor_changed_with_the_tool()
{
    let vm = start();
    create(&vm, &alice(), "ledger", Some("The quarterly ledger"));
    for _ in 0..3 {
        deploy(&vm, &alice(), "ledger", &[]);
    }
    contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        "ledger",
        &add(&[group("staff"), user("carol")]),
    );
    create(&vm, &alice(), "draft", None);
    let dashboard = Dashboard::new(&vm, &alice());

    let page = dashboard.details("ledger");
    let ledger = page.listed("ledger");
    assert_eq!(
        (
            ledger.relation.as_deref(),
            ledger.serving.as_deref(),
            ledger.attention.as_deref()
        ),
        (Some("contributor"), Some("3"), Some("none")),
        "expected the Application in an element with the list's attributes: `data-vm-relation`, `data-vm-serving`, `data-vm-attention`\n{}",
        page.describe()
    );
    assert_eq!(ledger.description(), Some("The quarterly ledger"));
    let words: Vec<String> = ledger
        .text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    assert!(
        words.iter().any(|w| w == "3" || w == "v3"),
        "expected the serving Version's number (3) as text in the Application's element, got {:?}",
        ledger.text
    );
    let host = format!("ledger.{DOMAIN}");
    assert!(
        ledger.text.contains(&host),
        "expected the Application's address ({host}) as text in its element, got {:?}",
        ledger.text
    );
    assert_eq!(
        ledger.open_href(),
        Some(dashboard.open_href("ledger")),
        "expected `a[data-vm-open]` when a Version serves\n{}",
        page.describe()
    );
    assert!(
        ledger.open_names.iter().all(|n| n.contains("ledger")),
        "expected the name of `a[data-vm-open]`, as a screen reader says it, to hold the Application's name, got {:?}",
        ledger.open_names
    );
    assert_eq!(
        page.contributor_keys(),
        vec![
            "user:alice".to_owned(),
            "group:staff".to_owned(),
            "user:carol".to_owned()
        ],
        "expected every Contributor, in the order of the list\n{}",
        page.describe()
    );
    let text = page.body_text().to_lowercase();
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    assert!(
        tokens.contains(&"vibe") || tokens.contains(&"tool"),
        "expected words saying that Contributors are changed with the tool, got {text:?}"
    );

    let draft = dashboard.details("draft").listed("draft");
    assert_eq!(draft.serving.as_deref(), Some("none"));
    assert_eq!(draft.open_href(), None, "no Version serves draft");
    assert!(
        draft.text.to_lowercase().contains("deployed"),
        "expected words saying that nothing is deployed yet, got {:?}",
        draft.text
    );
    assert_attention_attribute(&draft, "none", "the page of draft");
}

/// The texts of the page's `<header>` elements.
fn headers(html: &str) -> Vec<String> {
    Document::parse(html)
        .root()
        .named("header")
        .iter()
        .map(blackbox::page::Element::text)
        .collect()
}

fn assert_says_the_installation(response: &Response, status: u16, context: &str) {
    assert!(
        response.status == status
            && response
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/html")),
        "{context}: expected `{status}` and a page\n{}",
        response.describe()
    );
    let found = headers(&response.text());
    assert!(
        found.iter().any(|h| h.contains(DOMAIN)),
        "{context}: expected its header (`<header>`) to hold the installation's domain {DOMAIN:?}, its headers say {found:?}\n{}",
        response.describe()
    );
}

#[test]
fn a_page_of_the_sign_in_address_says_which_installation_it_is_in_its_header() {
    let at = Installation::start_with(&[("honest", "honest.js")], &[ada()], &[]);
    assert!(
        at.auth_host().ends_with(DOMAIN),
        "the harness's own check: the sign-in address is on {DOMAIN}"
    );
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");

    let sign_out = browser.open(&at.auth_url("/sign-out"));
    assert_says_the_installation(&sign_out.response, 200, "the sign-out page");

    let challenge = Challenge::fresh();
    let steps = at.walk_tool_to_page(
        &mut browser,
        &url(&format!(
            "{}/tool?challenge={}&machine=harness-machine",
            at.auth_origin(),
            challenge.challenge
        )),
    );
    let tool = &steps.last().unwrap().response;
    assert_says_the_installation(tool, 200, "the tool's sign-in page");

    let failed = browser.open(&at.auth_url("/callback?state=not-one-of-ours&code=x"));
    assert_says_the_installation(&failed.response, 400, "a page of a failed sign-in");
}

#[test]
fn the_local_runs_test_user_page_names_the_application_outside_its_title() {
    const NAME: &str = "said-once-board";
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let page = dev.browser().get("/_vm/test-user");
    assert_eq!(page.status, 200, "{}", page.describe());
    let document = Document::parse(&page.text());
    let text = document
        .root()
        .named("body")
        .first()
        .map(blackbox::page::Element::text)
        .unwrap_or_default();
    assert!(
        text.contains(NAME),
        "expected the Application's name {NAME:?} in the text of the test-User page, outside its <title>; its <body> says {text:?}"
    );
    dev.stop("INT").expect_code(0);
}
