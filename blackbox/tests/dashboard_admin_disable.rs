//! Disabling and enabling from the Dashboard (CONTRACT.md, "The Dashboard
//! for Platform administrators: Disabling", "Enabling" and "When a disable
//! or an enable is refused"), with the stand-in sign-in: the page that
//! confirms a disable (named, its words, its usage, one form, the way
//! back), the same page of one disabled already, `404` for a name no
//! Application has; the `POST` makes the change the API's disable makes
//! (the same `disabled`, its address answers disabled at once, a request in
//! flight is answered within a second, the `application-disabled` record,
//! no Operation log entry), `303` to `?changed=disabled` or `nothing`; the
//! enable's one button, `?changed=enabled` or `nothing`, served again by
//! the Version that serves now; the refusals `404`, `400` (a body over 4
//! KiB), `408` (a body not whole within 10 s) and the `503` of a disable
//! whose answer is lost, each changing nothing.
//!
//! Acceptance criterion (ticket "The Dashboard for Platform
//! administrators"): "A Platform administrator disables an Application and
//! enables it again from the browser, with the same effect as the API."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{
    IN_FLIGHT_ALLOWED, WAITS, assert_disabled_member, assert_disabled_plain, disable_ok, log_page,
    logged, place_usage, usage_of,
};
use blackbox::admin_pages::{
    APPLICATIONS, confirm_disable, disable_target, enable_target, press_enable,
    while_two_readings_run,
};
use blackbox::api::{assert_served_by, marked_bundle, version_body};
use blackbox::dashboard::{
    Dashboard, MAX_FORM_BODY, Page, administrator, assert_changed, assert_refused, error_text,
    stand_in_options, start,
};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, quickly};
use blackbox::records::event;
use blackbox::{Response, User, Vm, fixture_bundle};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

/// `board`, alice's, `fixtures/honest.js`, shared with everyone, used by
/// two Users.
fn with_board() -> Vm {
    let vm = start();
    vm.api_as(&alice())
        .create_with_bundle(APP, &fixture_bundle(blackbox::sharing::TRACED));
    place_usage(&vm, APP, 0, "bob", 9);
    place_usage(&vm, APP, 2, "carol", 1);
    vm
}

fn open(vm: &Vm) -> Response {
    vm.request(APP, "/hello")
        .as_user(&bob())
        .header("accept", "application/json")
        .send()
}

fn opens(vm: &Vm) -> bool {
    let r = open(vm);
    r.status == 200 && r.text().contains("Hello, bob!")
}

fn disabled_now(vm: &Vm) -> serde_json::Value {
    vm.api_as(&alice()).application(APP)["disabled"].clone()
}

fn entries_count(vm: &Vm) -> usize {
    log_page(vm, "?limit=1000")["entries"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn the_page_that_confirms_names_it_says_what_it_does_shows_its_usage_and_has_one_form() {
    let vm = with_board();
    let usage = usage_of(&vm, APP);
    let dashboard = Dashboard::new(&vm, &administrator());
    let page = dashboard.page(&disable_target(APP), 200, "the confirmation");
    let root = page.root();
    let h1: Vec<String> = root.named("h1").iter().map(|h| h.text()).collect();
    assert_eq!(
        h1,
        [format!("Disable {APP}?")],
        "its `<h1>` names it\n{}",
        page.describe()
    );
    let text = page.body_text();
    let lower = text.to_lowercase();
    for (words, what) in [
        (
            "in flight",
            "that requests in flight are answered disabled too",
        ),
        ("deleted", "that nothing of it is deleted"),
        ("deploy", "that its Contributors can still deploy"),
        (
            &format!("enable {APP}").to_lowercase(),
            "that \"Enable <name>\" on its page serves it again",
        ),
    ] {
        assert!(
            lower.contains(words),
            "expected the words to say {what}; they say {text:?}"
        );
    }
    for number in [usage["requests"].to_string(), usage["users"].to_string()] {
        assert!(
            text.split(|c: char| !c.is_ascii_digit())
                .any(|w| w == number),
            "expected its usage ({usage}) on the page, it says {text:?}"
        );
    }
    let forms = page.forms_of("disable");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"disable\"]`\n{}",
            page.describe()
        );
    };
    assert_eq!(form.method.as_deref(), Some("post"));
    assert_eq!(form.action.as_deref(), Some(disable_target(APP).as_str()));
    assert!(
        form.inputs.is_empty() && form.selects.is_empty() && form.buttons.len() == 1,
        "one button, no other field: {form:?}"
    );
    let confirm = root.with_attr_value("data-vm-confirm", APP);
    assert_eq!(
        confirm.len(),
        1,
        "one element `data-vm-confirm={APP:?}`\n{}",
        page.describe()
    );
    let holder = confirm[0];
    let buttons: Vec<String> = holder
        .with_attr_value("data-vm-form", "disable")
        .iter()
        .flat_map(|f| f.named("button"))
        .map(|b| b.text())
        .collect();
    assert_eq!(
        buttons,
        [format!("Disable {APP}")],
        "the form, in the element `data-vm-confirm`, with its one button"
    );
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.path() == format!("/applications/{APP}")),
        "expected a quiet link `a[data-vm-back]` to /applications/{APP}, got {back:?}"
    );
    let back_text: Vec<String> = root
        .with_attr("data-vm-back")
        .iter()
        .map(|b| b.text())
        .collect();
    assert_eq!(back_text, ["Keep it serving"], "the way back's words");
    assert!(opens(&vm), "showing the page changes nothing");
    assert!(disabled_now(&vm).is_null());
}

#[test]
fn the_same_page_of_one_disabled_already_says_since_when_and_by_whom_and_has_no_form() {
    let vm = with_board();
    let disabled = disable_ok(&vm, APP)["disabled"].clone();
    let page =
        Dashboard::new(&vm, &administrator()).page(&disable_target(APP), 200, "disabled already");
    let text = page.body_text();
    assert!(
        text.to_lowercase().contains("disabled") && text.contains(disabled["by"].as_str().unwrap()),
        "expected the page to say it is disabled already and by whom, it says {text:?}"
    );
    let since = disabled["since"].as_str().unwrap();
    assert!(
        text.contains(&since.chars().take(16).collect::<String>().replace('T', " "))
            || text.contains(&since.chars().take(16).collect::<String>()),
        "expected since when it is disabled ({since}, in UTC) on the page, it says {text:?}"
    );
    assert!(page.forms().is_empty(), "no form\n{}", page.describe());
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.path() == format!("/applications/{APP}")),
        "expected `a[data-vm-back]` to the Application's page, got {back:?}"
    );
}

#[test]
fn the_page_of_a_name_no_application_has_is_404_naming_it() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for name in ["nope", "Not_A_Name", "-x"] {
        let page = dashboard.page(&disable_target(name), 404, name);
        let errors = page.errors();
        assert!(
            matches!(errors.as_slice(), [(r, t)] if r == "not-found" && t.contains(name)),
            "{name}: expected one `data-vm-error=\"not-found\"` naming it, got {errors:?}"
        );
        let back = page.link("data-vm-back");
        assert!(
            back.as_ref()
                .is_some_and(|b| b.path() == APPLICATIONS && b.query().is_none()),
            "{name}: expected `a[data-vm-back]` to {APPLICATIONS}, got {back:?}"
        );
    }
}

#[test]
fn confirming_makes_the_apis_disable_and_its_address_answers_disabled_at_once() {
    let vm = with_board();
    assert!(opens(&vm));
    let dashboard = Dashboard::new(&vm, &administrator());
    let entries = entries_count(&vm);
    let answer = confirm_disable(&dashboard, APP);
    assert_changed(
        &answer,
        &dashboard.url("/"),
        APP,
        "disabled",
        "the disable confirmed",
    );
    let disabled = disabled_now(&vm);
    let since = assert_disabled_member(
        &disabled,
        &administrator().id,
        "after the Dashboard's disable",
    );
    assert_disabled_plain(&open(&vm), APP, "the Application's address after the `303`");
    let records = logged(&vm, "application-disabled");
    assert!(
        matches!(records.as_slice(), [r] if r["application"] == APP && r["by"] == administrator().id.as_str()),
        "expected one `application-disabled` record by the administrator, got {records:?}"
    );
    assert_eq!(
        entries_count(&vm),
        entries,
        "the Operation log is not written"
    );
    // The page it leads to says so, when and by whom.
    let page = dashboard.page(
        &format!("/applications/{APP}?changed=disabled"),
        200,
        "after",
    );
    assert_eq!(page.notices(), ["disabled"]);
    assert_eq!(
        page.root()
            .with_attr_value("data-vm-disabled", &since)
            .len(),
        1
    );

    // Again: `?changed=nothing`, since and by as they were, no record.
    let again = blackbox::admin_pages::post(&dashboard, &disable_target(APP), "");
    assert_changed(
        &again,
        &dashboard.url("/"),
        APP,
        "nothing",
        "a second disable",
    );
    assert_eq!(disabled_now(&vm), disabled, "since and by as they were");
    assert_eq!(
        logged(&vm, "application-disabled").len(),
        1,
        "no record when nothing changed"
    );
}

#[test]
fn a_request_in_flight_is_answered_disabled_within_a_second_of_the_303() {
    let vm = start();
    vm.api_as(&alice())
        .create_with_bundle(APP, &fixture_bundle(WAITS));
    let dashboard = Dashboard::new(&vm, &administrator());
    let held = vm
        .request(APP, "/plain?ms=8000")
        .as_user(&bob())
        .header("accept", "application/json")
        .timeout(Duration::from_secs(40));
    let started = Instant::now();
    let ((response, answered_at), disabled_at) =
        thread::scope(|scope| {
            let running = scope.spawn(|| {
                let r = held.send();
                (r, Instant::now())
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !vm.records().iter().any(|r| {
                event(r) == "printed" && r["message"].as_str() == Some("waits: began /plain")
            }) {
                assert!(
                    Instant::now() < deadline,
                    "the harness's own wait: the request is in its code"
                );
                thread::sleep(Duration::from_millis(20));
            }
            let answer = confirm_disable(&dashboard, APP);
            let at = Instant::now();
            assert_changed(&answer, &dashboard.url("/"), APP, "disabled", "the disable");
            (running.join().unwrap(), at)
        });
    let after = answered_at.saturating_duration_since(disabled_at);
    assert!(
        after <= IN_FLIGHT_ALLOWED && answered_at.duration_since(started) < Duration::from_secs(8),
        "expected the request in flight answered within 1 s of the disable's `303` ({IN_FLIGHT_ALLOWED:?} allowed), it was {after:?} after it\n{}",
        response.describe()
    );
    assert_disabled_plain(&response, APP, "the request in flight");
}

#[test]
fn enabling_by_its_button_serves_it_again_by_the_version_that_serves_now() {
    let vm = start();
    let api = vm.api_as(&alice());
    api.create_with_bundle(APP, &marked_bundle("v1"));
    let dashboard = Dashboard::new(&vm, &administrator());
    assert_changed(
        &confirm_disable(&dashboard, APP),
        &dashboard.url("/"),
        APP,
        "disabled",
        "disable",
    );
    api.send_version(APP, &version_body(&marked_bundle("v2")))
        .expect_status(201);
    let answer = press_enable(&dashboard, APP);
    assert_changed(
        &answer,
        &dashboard.url("/"),
        APP,
        "enabled",
        "the enable's button",
    );
    assert!(disabled_now(&vm).is_null(), "enabled");
    assert_served_by(&vm, &vm.get_as(&bob(), APP, "/"), APP, "v2", 2);
    let records = logged(&vm, "application-enabled");
    assert!(
        matches!(records.as_slice(), [r] if r["application"] == APP && r["by"] == administrator().id.as_str()),
        "expected one `application-enabled` record by the administrator, got {records:?}"
    );
    let page = dashboard.page(
        &format!("/applications/{APP}?changed=enabled"),
        200,
        "after",
    );
    assert_eq!(page.notices(), ["enabled"]);
    // Enabling one that is not disabled: `?changed=nothing`, no record.
    let again = blackbox::admin_pages::post(&dashboard, &enable_target(APP), "");
    assert_changed(
        &again,
        &dashboard.url("/"),
        APP,
        "nothing",
        "an enable of one not disabled",
    );
    assert_eq!(logged(&vm, "application-enabled").len(), 1);
}

#[test]
fn nothing_in_the_body_is_looked_at_up_to_4_kib_and_a_longer_one_is_400_malformed() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let padded = |to: usize| format!("x={}", "a".repeat(to - 2));
    for (action, target, changed) in [
        ("disable", disable_target(APP), "disabled"),
        ("enable", enable_target(APP), "enabled"),
    ] {
        let longer = padded(MAX_FORM_BODY + 1);
        let page = assert_refused(
            dashboard.post_form(&target, &longer),
            &dashboard.url("/"),
            400,
            "malformed",
            APP,
            &format!("a {action} with a body of 4 KiB and one byte"),
        );
        assert!(!error_text(&page).is_empty());
        assert_eq!(
            disabled_now(&vm).is_null(),
            action == "disable",
            "a refused {action} changes nothing"
        );
        let exact = padded(MAX_FORM_BODY);
        assert_eq!(exact.len(), MAX_FORM_BODY, "the harness's own count");
        assert_changed(
            &dashboard.post_form(&target, &exact),
            &dashboard.url("/"),
            APP,
            changed,
            &format!("a {action} with a body of exactly 4 KiB, not looked at"),
        );
    }
    for body in ["action=enable", "%%%", "changed=nothing&x=1&x=2"] {
        assert_changed(
            &dashboard.post_form(&disable_target(APP), body),
            &dashboard.url("/"),
            APP,
            "disabled",
            &format!("a disable with the body {body:?}, not looked at"),
        );
        press_enable(&dashboard, APP);
    }
}

#[test]
fn a_body_not_whole_within_10_seconds_is_408_timed_out_and_changes_nothing() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for (action, target) in [
        ("disable", disable_target(APP)),
        ("enable", enable_target(APP)),
    ] {
        let request = dashboard
            .form_request(&target, "x=1")
            .declare_length(64)
            .timeout(Duration::from_secs(30));
        let started = Instant::now();
        let answer = request.send();
        let waited = started.elapsed();
        assert_refused(
            answer,
            &dashboard.url("/"),
            408,
            "timed-out",
            APP,
            &format!("a {action} whose body does not come whole"),
        );
        assert!(
            waited >= Duration::from_secs(9),
            "expected the program to wait about 10 s for the body, it answered after {waited:?}"
        );
        assert!(
            disabled_now(&vm).is_null(),
            "nothing changed by the {action}"
        );
    }
    assert!(opens(&vm));
    assert!(logged(&vm, "application-disabled").is_empty());
}

#[test]
fn a_disable_or_enable_of_a_name_no_application_has_is_404_naming_it_and_leading_to_every_application()
 {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for name in ["nope", "Not_A_Name"] {
        for target in [disable_target(name), enable_target(name)] {
            let answer = dashboard.post_form(&target, "");
            assert_eq!(answer.status, 404, "{target}\n{}", answer.describe());
            let page = Page::read(answer, dashboard.url(&target), &target);
            let errors = page.errors();
            assert!(
                matches!(errors.as_slice(), [(r, t)] if r == "not-found" && t.contains(name) && !t.contains("/v1/")),
                "{target}: expected `data-vm-error=\"not-found\"` naming {name:?} for a person at a browser, got {errors:?}"
            );
            let back = page.link("data-vm-back");
            assert!(
                back.as_ref().is_some_and(|b| b.path() == APPLICATIONS),
                "{target}: expected `a[data-vm-back]` to {APPLICATIONS}, got {back:?}"
            );
        }
    }
    assert!(opens(&vm));
}

#[test]
fn a_disable_whose_answer_is_lost_is_503_saying_it_cannot_be_told_and_settles_as_the_database_says()
{
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_dashboard_lost_disable");
    let vm = Vm::start(&stand_in_options().platform(&platform));
    vm.api_as(&alice())
        .create_with_bundle(APP, &fixture_bundle(blackbox::sharing::TRACED));
    assert!(opens(&vm));
    let dashboard = Dashboard::new(&vm, &administrator());
    let request = dashboard
        .form_request(&disable_target(APP), "")
        .timeout(LOST_WRITE_TIMEOUT);
    postgres.hold();
    let began = Instant::now();
    let answer = thread::scope(|scope| {
        let answer = scope.spawn(move || request.try_send());
        postgres.wait_for_waiting(1, "the disable");
        answer.join().unwrap()
    });
    let took = began.elapsed();
    let answer = answer.unwrap_or_else(|e| panic!("expected an answer to the disable ({e})"));
    let page = assert_refused(
        answer,
        &dashboard.url("/"),
        503,
        "unavailable",
        APP,
        "a disable whose commit is still on its way",
    );
    assert!(
        took < Duration::from_secs(13),
        "expected the disable's `503` within its own 10 s (and 3 s), it took {took:?}"
    );
    let words = error_text(&page).to_lowercase();
    assert!(
        words.contains("whether") && !words.contains("/v1/"),
        "expected the words to say it cannot be told whether it was made and to look at the Application's page, got {words:?}"
    );
    // Meanwhile its address answers `503` with `retry-after`, at once.
    let meanwhile = quickly(
        &vm,
        &vm.request(APP, "/hello")
            .as_user(&bob())
            .header("accept", "application/json"),
        "meanwhile",
    );
    blackbox::lost_answer::assert_503_retry(&meanwhile, "the Application's address meanwhile");
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let deadline = Instant::now() + blackbox::lost_answer::SETTLE;
    loop {
        let response = open(&vm);
        if response.header("retry-after").is_none() {
            assert_disabled_plain(&response, APP, "settled as the database says");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected it settled\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn while_two_readings_run_the_page_that_confirms_is_503_at_once_with_retry_after() {
    let vm = with_board();
    let target = disable_target(APP);
    let dashboard = Dashboard::new(&vm, &administrator());
    dashboard.page(&target, 200, "before");
    let (answer, took) = while_two_readings_run(&vm, || {
        let began = Instant::now();
        let answer = dashboard
            .navigation("GET", &target)
            .timeout(Duration::from_secs(40))
            .send();
        (answer, began.elapsed())
    });
    assert!(
        answer.status == 503
            && answer.header("retry-after").is_some()
            && took < Duration::from_secs(2),
        "the page that confirms a disable is a reading (its usage): expected `503` with `retry-after` at once while two others run, got after {took:?}\n{}",
        answer.describe()
    );
    let page = Page::read(answer, dashboard.url(&target), "the third reading");
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, w)] if r == "unavailable" && w.to_lowercase().contains("again")),
        "expected `data-vm-error=\"unavailable\"` saying to try again shortly, got {errors:?}"
    );
    assert!(disabled_now(&vm).is_null(), "nothing changed");
    dashboard.page(&target, 200, "once they are done");
}
