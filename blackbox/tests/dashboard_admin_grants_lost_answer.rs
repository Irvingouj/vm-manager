//! A setting or a revoke from the Dashboard whose answer is lost
//! (CONTRACT.md, "Resources and Grants in full: The Dashboard: a Resource's
//! page and its Grants: When the platform database does not answer", and
//! "A write whose answer is lost"): the platform database holds commits,
//! the page is `503` `unavailable` with `retry-after`, words saying it
//! cannot be told whether it was made and a link to the Resource's page;
//! meanwhile that page is its own `503` naming the Resource and shows no
//! Grant on a guess; once commits go through it says what holds; the
//! program's log has one `grant-unsettled` record with who asked. A Resource
//! whose Grants cannot be told because of a write made through the API is
//! the same `503` on its page.
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
    assert_error_page, error_words, grants_shown, grants_target, resource_target, revoke_target,
};
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, SETTLE, quickly};
use blackbox::resources::records_waited;
use blackbox::scenario::{Scenario, administrator as admin_user};
use serde_json::{Value, json};

const NAME: &str = "crewdb";

fn world(postgres: &HeldPostgres, platform: &str) -> Scenario {
    let platform = postgres.platform(platform);
    let s = Scenario::start_with(&stand_in_options().platform(&platform));
    s.register(NAME);
    s.grant(NAME, "crew", "read");
    s
}

/// Sends `body` to `target` as a form of the Dashboard's while commits are
/// held, and returns what it answered and after how long.
fn lost(
    postgres: &HeldPostgres,
    dashboard: &Dashboard<'_>,
    target: &str,
    body: &str,
) -> (blackbox::Response, Duration) {
    let request = dashboard
        .form_request(target, body)
        .timeout(LOST_WRITE_TIMEOUT);
    postgres.hold();
    let began = Instant::now();
    let answer = thread::scope(|scope| {
        let answer = scope.spawn(move || request.try_send());
        postgres.wait_for_waiting(1, "the write");
        answer.join().unwrap()
    });
    let answer = answer.unwrap_or_else(|e| panic!("expected an answer to the write ({e})"));
    (answer, began.elapsed())
}

/// The Resource's page once it answers `200` again, within [`SETTLE`].
fn page_settled(dashboard: &Dashboard<'_>) -> Page {
    let target = resource_target(NAME);
    let deadline = Instant::now() + SETTLE;
    loop {
        let answer = dashboard.get(&target);
        if answer.status == 200 {
            return Page::read(answer, dashboard.url(&target), "settled");
        }
        assert!(
            Instant::now() < deadline,
            "expected the Resource's page again within {SETTLE:?} of commits going through\n{}",
            answer.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
}

fn shown(page: &Page) -> Vec<(String, String)> {
    grants_shown(page)
        .into_iter()
        .map(|g| (g.group, g.access.unwrap_or_default()))
        .collect()
}

fn assert_meanwhile(s: &Scenario, dashboard: &Dashboard<'_>, what: &str) {
    let target = resource_target(NAME);
    let answer = quickly(
        &s.vm,
        &dashboard.navigation("GET", &target),
        &format!("the Resource's page meanwhile ({what})"),
    );
    assert!(
        answer.status == 503 && answer.header("retry-after").is_some(),
        "{what}: expected `503` with `retry-after` while the Grants on {NAME} cannot be told\n{}",
        answer.describe()
    );
    let page = Page::read(answer, dashboard.url(&target), "meanwhile");
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, w)] if r == "unavailable" && w.contains(NAME) && w.to_lowercase().contains("again")),
        "{what}: expected `data-vm-error=\"unavailable\"` naming {NAME} and saying to try again shortly, got {errors:?}"
    );
    assert_eq!(
        page.count("data-vm-grant"),
        0,
        "{what}: no Grant shown on a guess"
    );
    assert_eq!(page.count("data-vm-form"), 0, "{what}: no form on a guess");
}

fn unsettled(s: &Scenario) -> Value {
    let records = records_waited(&s.vm, "grant-unsettled", 1);
    assert_eq!(
        records.len(),
        1,
        "expected one `grant-unsettled` record, got {records:?}"
    );
    records[0].clone()
}

#[test]
fn a_setting_whose_answer_is_lost_is_503_saying_it_cannot_be_told_and_the_page_settles() {
    let postgres = HeldPostgres::start();
    let s = world(&postgres, "vm_platform_dashboard_lost_setting");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let (answer, took) = lost(
        &postgres,
        &dashboard,
        &grants_target(NAME),
        "group=crew2&access=write",
    );
    let page = assert_error_page(
        answer,
        &dashboard.url("/"),
        503,
        "unavailable",
        &resource_target(NAME),
        "a setting whose commit is still on its way",
    );
    assert!(
        took < Duration::from_secs(13),
        "expected the `503` within the platform database's 10 s (and 3 s), it took {took:?}"
    );
    let words = error_words(&page);
    assert!(
        words.contains("whether") && words.contains("resource"),
        "expected the words to say it cannot be told whether it was made and to look at the Resource's page, got {words:?}"
    );
    assert_meanwhile(&s, &dashboard, "a setting");
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let page = page_settled(&dashboard);
    assert_eq!(
        shown(&page),
        [
            ("crew".to_owned(), "read".to_owned()),
            ("crew2".to_owned(), "write".to_owned())
        ],
        "what the database has: the write landed"
    );
    let record = unsettled(&s);
    assert_eq!(record["event"], "grant-unsettled");
    assert_eq!(record["resource"], NAME);
    assert_eq!(record["group"], "crew2");
    assert_eq!(record["asked"], "set");
    assert_eq!(record["access"], "write");
    assert_eq!(record["by"], json!(admin_user().id), "{record}");
    assert!(
        records_waited(&s.vm, "grant-revoked", 0).is_empty(),
        "no `grant-revoked` for a setting"
    );
}

#[test]
fn a_revoke_whose_answer_is_lost_is_503_saying_it_cannot_be_told_and_the_page_settles() {
    let postgres = HeldPostgres::start();
    let s = world(&postgres, "vm_platform_dashboard_lost_revoke");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let (answer, took) = lost(&postgres, &dashboard, &revoke_target(NAME), "group=crew");
    let page = assert_error_page(
        answer,
        &dashboard.url("/"),
        503,
        "unavailable",
        &resource_target(NAME),
        "a revoke whose commit is still on its way",
    );
    assert!(
        took < Duration::from_secs(13),
        "expected the `503` within the platform database's 10 s (and 3 s), it took {took:?}"
    );
    let words = error_words(&page);
    assert!(
        words.contains("whether") && words.contains("resource"),
        "expected the words to say it cannot be told whether it was made and to look at the Resource's page, got {words:?}"
    );
    assert_meanwhile(&s, &dashboard, "a revoke");
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let page = page_settled(&dashboard);
    assert!(
        shown(&page).is_empty(),
        "what the database has: the revoke landed, got {:?}",
        shown(&page)
    );
    assert_eq!(page.count("data-vm-empty"), 1);
    let record = unsettled(&s);
    assert_eq!(record["resource"], NAME);
    assert_eq!(record["group"], "crew");
    assert_eq!(record["asked"], "revoke");
    assert_eq!(record["access"], Value::Null);
    assert_eq!(record["by"], json!(admin_user().id), "{record}");
    assert!(
        records_waited(&s.vm, "grant-set", 0).len() == 1,
        "no `grant-set` but the setup's"
    );
}

#[test]
fn the_resources_page_is_503_naming_it_while_a_grant_written_through_the_api_cannot_be_told() {
    let postgres = HeldPostgres::start();
    let s = world(&postgres, "vm_platform_dashboard_lost_page");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    assert_eq!(
        dashboard
            .page(&resource_target(NAME), 200, "before")
            .count("data-vm-grant"),
        1,
        "the harness's own check: the page before"
    );
    postgres.hold();
    let lower = s
        .admin()
        .request(&format!("/v1/resources/{NAME}/grants"))
        .method("POST")
        .json(&json!({ "group": "crew", "access": "structure" }))
        .timeout(LOST_WRITE_TIMEOUT);
    let lowered = thread::scope(|scope| {
        let lowered = scope.spawn(move || lower.try_send());
        postgres.wait_for_waiting(1, "the Grant lowered");
        lowered.join().unwrap()
    });
    let lowered = lowered.unwrap_or_else(|e| panic!("expected `503` for the Grant lowered ({e})"));
    blackbox::assert_api_error(&lowered, 503, "unavailable");
    assert_meanwhile(&s, &dashboard, "an API write");
    // The confirmation and the form are not offered on a guess either: the
    // list is the same `503`.
    let list = quickly(
        &s.vm,
        &dashboard.navigation("GET", "/admin/resources"),
        "the list meanwhile",
    );
    assert!(
        list.status == 503 && list.header("retry-after").is_some(),
        "{}",
        list.describe()
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let page = page_settled(&dashboard);
    assert_eq!(
        shown(&page),
        [("crew".to_owned(), "structure".to_owned())],
        "the Grant the database has once settled"
    );
}

#[test]
fn while_the_resource_is_not_known_a_setting_and_a_revoke_are_503_saying_nothing_was_changed() {
    let postgres = HeldPostgres::start();
    let s = world(&postgres, "vm_platform_dashboard_lost_second");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let second_setting = dashboard.form_request(&grants_target(NAME), "group=crew3&access=read");
    let revoke = dashboard.form_request(&revoke_target(NAME), "group=crew");
    postgres.hold();
    let first = s
        .admin()
        .request(&format!("/v1/resources/{NAME}/grants"))
        .method("POST")
        .json(&json!({ "group": "crew2", "access": "write" }))
        .timeout(LOST_WRITE_TIMEOUT);
    let first = thread::scope(|scope| {
        let first = scope.spawn(move || first.try_send());
        postgres.wait_for_waiting(1, "the first setting");
        first.join().unwrap()
    });
    let first = first.unwrap_or_else(|e| panic!("expected `503` for the first setting ({e})"));
    blackbox::assert_api_error(&first, 503, "unavailable");
    assert_eq!(unsettled(&s)["group"], "crew2");
    for (what, request) in [("a second setting", second_setting), ("a revoke", revoke)] {
        let answer = quickly(&s.vm, &request, what);
        let page = assert_error_page(
            answer,
            &dashboard.url("/"),
            503,
            "unavailable",
            &resource_target(NAME),
            what,
        );
        let words = error_words(&page);
        assert!(
            words.contains("nothing")
                && words.contains("changed")
                && words.contains("try again")
                && !words.contains("cannot"),
            "{what}: expected words that nothing was changed and to try again shortly, not that it cannot be told; got {words:?}"
        );
    }
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let page = page_settled(&dashboard);
    assert_eq!(
        shown(&page),
        [
            ("crew".to_owned(), "read".to_owned()),
            ("crew2".to_owned(), "write".to_owned())
        ],
        "what the first write made, nothing of the second or of the revoke"
    );
    assert_eq!(
        records_waited(&s.vm, "grant-unsettled", 0).len(),
        1,
        "no second `grant-unsettled` record"
    );
}
