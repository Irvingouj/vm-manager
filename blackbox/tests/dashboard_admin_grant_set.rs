//! Setting a Grant from a Resource's page (CONTRACT.md, "Resources and
//! Grants in full: The Dashboard: a Resource's page and its Grants:
//! Setting"), with the stand-in sign-in: `POST
//! /admin/resources/<name>/grants` makes the change `POST
//! /v1/resources/<name>/grants` makes, by that Platform administrator, with
//! the same record; `303` to `?changed=granted | changed | nothing` with
//! the Group percent-encoded; a field not taken is `400` with the page, an
//! error beside the right field and what was typed kept, nothing changed;
//! white space around a Group is kept; a body over 4 KiB is `400`
//! `malformed`; a body not whole within 10 seconds is `408`; a 1,001st Grant
//! is `409` `too-many`; no such Resource is `404`.
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
    access_chosen, api_grants, assert_changed_resource, assert_error_page, field_errors,
    grant_form, grants_shown, grants_target, group_typed, resource_target, set_access,
};
use blackbox::dashboard::{
    Dashboard, MAX_FORM_BODY, Page, administrator, form_body, stand_in_options,
};
use blackbox::resources::{fill_grants, member_names, records_waited};
use blackbox::scenario::{Scenario, administrator as admin_user};
use blackbox::storage::{MARKETING, Place, Storage};
use blackbox::{Response, VmOptions};
use serde_json::{Value, json};

const NAME: &str = "devops";

fn world() -> Scenario {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    s
}

fn storage_world() -> Storage {
    let place = Place::new("dashboard-grant");
    let root = place.root();
    let s = Storage::start_on(
        place,
        Some(&root),
        &VmOptions::new().args_extra(&["--public-scheme", "http"]),
    );
    s.register(MARKETING);
    s
}

fn api_entry(s: &Scenario, group: &str) -> Option<Value> {
    let answer = s.admin().list_grants(NAME);
    answer.expect_status(200);
    answer.json()["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"] == group)
        .cloned()
}

#[test]
fn giving_a_group_access_is_303_to_the_page_with_granted_and_the_api_has_the_grant_set_by_the_administrator()
 {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let answer = set_access(&dashboard, NAME, "sre", "read");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "granted",
        "sre",
        "a Group that held none",
    );
    let entry = api_entry(&s, "sre").unwrap_or_else(|| panic!("expected a Grant for sre"));
    assert_eq!(entry["access"], "read");
    assert_eq!(entry["granted_by"], json!(admin_user().id), "{entry}");
    blackbox::resources::assert_utc_time(&entry["granted_at"], "when it was set");
    // The page it leads to says so, and shows who set it.
    let page = dashboard.page(
        &blackbox::browser::target_of(
            &dashboard
                .url("/")
                .join(answer.header("location").unwrap())
                .unwrap(),
        ),
        200,
        "after",
    );
    assert_eq!(page.notices(), ["granted"]);
    let shown = grants_shown(&page);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].group, "sre");
    assert_eq!(shown[0].access.as_deref(), Some("read"));
    assert_eq!(shown[0].by.as_deref(), Some(admin_user().id.as_str()));
    assert_eq!(shown[0].at, entry["granted_at"].as_str().map(str::to_owned));
}

#[test]
fn another_level_for_a_group_that_holds_one_is_changed_and_the_same_again_is_nothing() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    assert_changed_resource(
        &set_access(&dashboard, NAME, "sre", "read"),
        &dashboard.url("/"),
        NAME,
        "granted",
        "sre",
        "first",
    );
    let first = api_entry(&s, "sre").unwrap();
    thread::sleep(Duration::from_millis(1100));
    assert_changed_resource(
        &set_access(&dashboard, NAME, "sre", "write"),
        &dashboard.url("/"),
        NAME,
        "changed",
        "sre",
        "another level",
    );
    let second = api_entry(&s, "sre").unwrap();
    assert_eq!(second["access"], "write");
    assert_ne!(
        second["granted_at"], first["granted_at"],
        "set again: a new time"
    );
    thread::sleep(Duration::from_millis(1100));
    assert_changed_resource(
        &set_access(&dashboard, NAME, "sre", "write"),
        &dashboard.url("/"),
        NAME,
        "nothing",
        "sre",
        "the same level again",
    );
    assert_eq!(
        api_entry(&s, "sre").unwrap(),
        second,
        "who set it and when stay as they were"
    );
    let page = dashboard.page(
        &blackbox::admin_pages::changed_target(NAME, "nothing", "sre"),
        200,
        "nothing",
    );
    assert_eq!(page.notices(), ["nothing"]);
    assert_eq!(
        records_waited(&s.vm, "grant-set", 2).len(),
        2,
        "two changes, one that changed nothing"
    );
}

#[test]
fn the_log_has_the_grant_set_record_the_apis_write_makes_with_the_same_by() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    s.admin().grant(NAME, "from-api", "read").expect_status(200);
    set_access(&dashboard, NAME, "from-page", "read");
    set_access(&dashboard, NAME, "from-page", "write");
    set_access(&dashboard, NAME, "from-page", "write");
    let records = records_waited(&s.vm, "grant-set", 3);
    assert_eq!(
        records.len(),
        3,
        "two changes on the page and one through the API, no record for what changed nothing: {records:?}"
    );
    let api = records
        .iter()
        .find(|r| r["group"] == "from-api")
        .unwrap_or_else(|| panic!("a record of the API's write: {records:?}"));
    let page_new = records
        .iter()
        .find(|r| r["group"] == "from-page" && r["was"].is_null())
        .unwrap_or_else(|| panic!("a record of the page's first write: {records:?}"));
    let page_changed = records
        .iter()
        .find(|r| r["group"] == "from-page" && r["was"] == "read")
        .unwrap_or_else(|| panic!("a record of the page's second write: {records:?}"));
    for record in [api, page_new, page_changed] {
        assert_eq!(
            member_names(record),
            member_names(api),
            "the same members as the API's record: {record}"
        );
        assert_eq!(record["event"], "grant-set");
        assert_eq!(record["resource"], NAME);
        assert_eq!(record["by"], json!(admin_user().id), "{record}");
    }
    assert_eq!(page_new["access"], "read");
    assert_eq!(page_changed["access"], "write");
}

#[test]
fn the_group_is_percent_encoded_in_the_location() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for group in [
        "ops/team",
        "two words",
        "a&b=c",
        "say \"hi\"",
        "<b>x</b>",
        "Zoë ünï",
        "plus+sign",
        "100%",
    ] {
        let answer = set_access(&dashboard, NAME, group, "read");
        assert_changed_resource(
            &answer,
            &dashboard.url("/"),
            NAME,
            "granted",
            group,
            &format!("the Group {group:?}"),
        );
        assert!(
            api_grants(&s.vm, NAME).iter().any(|(g, _)| g == group),
            "{group:?}: the API has the Grant for the Group exactly"
        );
    }
}

#[test]
fn white_space_around_a_group_is_kept_it_is_another_group() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    s.grant(NAME, "sre", "read");
    for group in [" sre", "sre ", " sre ", "\u{a0}sre"] {
        let answer = set_access(&dashboard, NAME, group, "write");
        assert_changed_resource(
            &answer,
            &dashboard.url("/"),
            NAME,
            "granted",
            group,
            &format!("the Group {group:?}, not the one without the white space"),
        );
    }
    let held = api_grants(&s.vm, NAME);
    assert_eq!(
        held.iter().filter(|(_, a)| a == "write").count(),
        4,
        "four other Groups: {held:?}"
    );
    assert!(
        held.contains(&("sre".to_owned(), "read".to_owned())),
        "sre is as it was: {held:?}"
    );
    let page = dashboard.page(&resource_target(NAME), 200, "the page");
    let groups: Vec<String> = grants_shown(&page).into_iter().map(|g| g.group).collect();
    for group in [" sre", "sre ", " sre ", "\u{a0}sre", "sre"] {
        assert!(
            groups.iter().any(|g| g == group),
            "{group:?} shown exactly, got {groups:?}"
        );
    }
}

#[test]
fn a_field_not_taken_is_400_with_the_page_an_error_beside_the_right_field_and_nothing_changed() {
    let s = world();
    s.grant(NAME, "kept", "read");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let before = api_grants(&s.vm, NAME);
    let too_long = "g".repeat(257);
    let cases: Vec<(&str, String, &str, &str)> = vec![
        ("an empty Group", String::new(), "read", "group"),
        ("a Group of 257 bytes", too_long, "read", "group"),
        (
            "a Group with a control character",
            "a\u{7}b".to_owned(),
            "read",
            "group",
        ),
        (
            "a Group with a newline",
            "a\nb".to_owned(),
            "write",
            "group",
        ),
        (
            "a level that is none",
            "sre".to_owned(),
            "nonsense",
            "access",
        ),
        ("an empty level", "sre".to_owned(), "", "access"),
        ("a level in capitals", "sre".to_owned(), "READ", "access"),
    ];
    for (what, group, access, field) in cases {
        let answer = set_access(&dashboard, NAME, &group, access);
        let page = assert_status_page(answer, &dashboard, 400, what);
        let errors = field_errors(&page);
        assert!(
            matches!(errors.as_slice(), [(f, words)] if f == field && !words.trim().is_empty() && !words.contains("/v1/")),
            "{what}: expected one `data-vm-field-error={field:?}` with words, got {errors:?}\n{}",
            page.describe()
        );
        assert_eq!(
            grants_shown(&page).len(),
            before.len(),
            "{what}: the page shows what holds"
        );
        assert_eq!(api_grants(&s.vm, NAME), before, "{what}: nothing changed");
    }
    assert!(
        records_waited(&s.vm, "grant-set", 0).len() == 1,
        "only the setup's own record"
    );
}

/// The page of an answer that must be `status`, a page of the Dashboard's.
fn assert_status_page(
    answer: Response,
    dashboard: &Dashboard<'_>,
    status: u16,
    what: &str,
) -> Page {
    assert_eq!(
        answer.status,
        status,
        "{what}: expected `{status}`\n{}",
        answer.describe()
    );
    Page::read(answer, dashboard.url("/"), what)
}

#[test]
fn what_was_typed_is_kept_the_group_escaped_in_its_field_and_the_choice_selected() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    // A Group that is not taken, with markup in it, and a level chosen.
    let group = format!("<i>\"q\" & 'a'</i>{}", "x".repeat(260));
    for access in ["structure", "read", "write"] {
        let answer = set_access(&dashboard, NAME, &group, access);
        let page = assert_status_page(answer, &dashboard, 400, "a Group over 256 bytes");
        assert_eq!(
            group_typed(&page).as_deref(),
            Some(group.as_str()),
            "the Group in its field, exactly\n{}",
            page.describe()
        );
        assert_eq!(
            access_chosen(&page).as_deref(),
            Some(access),
            "the choice in the `select`\n{}",
            page.describe()
        );
        let html = page.response.text();
        assert!(
            !html.contains("<i>\"q\""),
            "the Group is escaped in the source\n{html}"
        );
    }
    // A level that is none: the Group is kept.
    let answer = set_access(&dashboard, NAME, "sre ", "nonsense");
    let page = assert_status_page(answer, &dashboard, 400, "a level that is none");
    assert_eq!(
        group_typed(&page).as_deref(),
        Some("sre "),
        "white space around the Group kept in its field"
    );
}

#[test]
fn a_field_missing_or_given_twice_is_400_and_nothing_changes() {
    let s = world();
    s.grant(NAME, "kept", "read");
    let before = api_grants(&s.vm, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for (what, body, field) in [
        ("no group", "access=read", "group"),
        ("no access", "group=sre", "access"),
        ("no field", "", ""),
        ("two groups", "group=a&group=b&access=read", "group"),
        ("two levels", "group=a&access=read&access=write", "access"),
    ] {
        let answer = dashboard.post_form(&grants_target(NAME), body);
        let page = assert_status_page(answer, &dashboard, 400, what);
        let errors = field_errors(&page);
        assert!(
            !errors.is_empty()
                && (field.is_empty() || errors.iter().any(|(f, _)| f == field))
                && errors
                    .iter()
                    .all(|(f, w)| (f == "group" || f == "access") && !w.is_empty()),
            "{what}: expected an error beside {field:?}, got {errors:?}\n{}",
            page.describe()
        );
        assert_eq!(api_grants(&s.vm, NAME), before, "{what}: nothing changed");
    }
}

#[test]
fn a_body_over_4_kib_is_400_malformed_with_a_way_back_and_nothing_changes() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let body = format!("group=sre&access=read&pad={}", "a".repeat(MAX_FORM_BODY));
    assert!(body.len() > MAX_FORM_BODY, "the harness's own count");
    let answer = dashboard.post_form(&grants_target(NAME), &body);
    assert_error_page(
        answer,
        &dashboard.url("/"),
        400,
        "malformed",
        &resource_target(NAME),
        "a body over 4 KiB",
    );
    assert!(api_grants(&s.vm, NAME).is_empty(), "nothing changed");
    // A Group of 256 bytes is taken, in a body well under the bound.
    let group = "g".repeat(256);
    let answer = set_access(&dashboard, NAME, &group, "read");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "granted",
        &group,
        "a Group of 256 bytes",
    );
}

#[test]
fn a_body_not_whole_within_10_seconds_is_408_timed_out_and_changes_nothing() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let request = dashboard
        .form_request(&grants_target(NAME), "group=sre&access=read")
        .declare_length(64)
        .timeout(Duration::from_secs(30));
    let began = Instant::now();
    let answer = request.send();
    let waited = began.elapsed();
    assert_error_page(
        answer,
        &dashboard.url("/"),
        408,
        "timed-out",
        &resource_target(NAME),
        "a body that does not come whole",
    );
    assert!(
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(20),
        "expected the program to wait about 10 s for the body, it answered after {waited:?}"
    );
    assert!(api_grants(&s.vm, NAME).is_empty(), "nothing changed");
}

#[test]
fn a_1001st_grant_is_409_too_many_and_a_group_that_holds_one_is_changed_all_the_same() {
    let s = world();
    fill_grants(&s.vm, NAME, 0, 1000, "read");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "a Resource with 1,000 Grants");
    assert_eq!(
        grants_shown(&page).len(),
        1000,
        "a Resource's page holds at most 1,000 Grants"
    );
    assert_eq!(page.count("data-vm-grants-more"), 0);
    let answer = set_access(&dashboard, NAME, "one-too-many", "read");
    let page = assert_error_page(
        answer,
        &dashboard.url("/"),
        409,
        "too-many",
        &resource_target(NAME),
        "a 1,001st Grant",
    );
    let words = blackbox::admin_pages::error_words(&page);
    assert!(
        words.contains("1,000") || words.contains("1000"),
        "expected words saying how many a Resource holds, got {words:?}"
    );
    assert_eq!(api_grants(&s.vm, NAME).len(), 1000, "nothing written");
    // A Group that holds one is changed also when the list is full.
    let answer = set_access(&dashboard, NAME, "group0000", "write");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "changed",
        "group0000",
        "a Group that holds a Grant, on a full Resource",
    );
}

#[test]
fn no_such_resource_is_404_naming_it_and_nothing_is_made() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for name in ["nope", "Not_A_Name", "-x"] {
        let answer = dashboard.post_form(&grants_target(name), "group=sre&access=read");
        let page = assert_error_page(
            answer,
            &dashboard.url("/"),
            404,
            "not-found",
            "/admin/resources",
            &format!("a Grant on {name:?}"),
        );
        assert!(
            blackbox::dashboard::error_text(&page).contains(name),
            "naming it"
        );
    }
    s.admin().get_resource("nope").expect_status(404);
    assert!(api_grants(&s.vm, NAME).is_empty());
}

#[test]
fn a_storage_resource_takes_read_and_write_and_structure_is_a_field_error_of_access() {
    let s = storage_world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    assert_changed_resource(
        &set_access(&dashboard, MARKETING, "designers", "read"),
        &dashboard.url("/"),
        MARKETING,
        "granted",
        "designers",
        "read on a Storage Resource",
    );
    assert_changed_resource(
        &set_access(&dashboard, MARKETING, "designers", "write"),
        &dashboard.url("/"),
        MARKETING,
        "changed",
        "designers",
        "write on a Storage Resource",
    );
    let page = dashboard.page(&resource_target(MARKETING), 200, "the page");
    let form = grant_form(&page);
    // A person cannot choose `structure`, but a form sent by hand can.
    let answer = dashboard
        .form_request(
            &grants_target(MARKETING),
            &form_body(&[("group", "designers"), ("access", "structure")]),
        )
        .send();
    let page = assert_status_page(answer, &dashboard, 400, "structure on a Storage Resource");
    let errors = field_errors(&page);
    assert!(
        matches!(errors.as_slice(), [(f, w)] if f == "access" && !w.is_empty()),
        "expected one `data-vm-field-error=\"access\"`, got {errors:?}"
    );
    assert_eq!(
        group_typed(&page).as_deref(),
        Some("designers"),
        "the Group kept"
    );
    let held = s.admin().list_grants(MARKETING).json();
    assert_eq!(
        held["grants"][0]["access"], "write",
        "nothing changed: {held}"
    );
    assert_eq!(
        form.choices("access"),
        ["read", "write"],
        "the form offers two levels"
    );
}
