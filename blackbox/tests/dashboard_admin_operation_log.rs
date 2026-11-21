//! The Operation log on the Dashboard (CONTRACT.md, "The Dashboard for
//! Platform administrators: The Operation log"), with the stand-in sign-in:
//! the entries `GET /v1/operation-log` answers to the same filters with
//! `limit=100`, newest first; the filter form (`method="get"`, its fields,
//! labels, choices and button, `a[data-vm-clear]`); an empty field is a
//! filter not given, white space is dropped, each field holds its filter; a
//! time typed in a `datetime-local` field is read as UTC, and RFC 3339 with
//! an offset is taken from a typed address; `before`, `a[data-vm-older]` to
//! the end and `a[data-vm-newest]`; each entry's element, what is seen
//! first and what is in its `<details>`; `a[data-vm-filter]` links; words
//! when nothing matches; a malformed filter `400` beside its field with what
//! was typed kept; the 10-second `503` and the `503` of a third reading at
//! once. (The 4 MiB bound cannot be reached by a page: an entry keeps at
//! most about 16 KiB of its statement, so 100 entries stay far below it.)
//!
//! Acceptance criterion (ticket "The Dashboard for Platform
//! administrators"): "A Platform administrator reads the Operation log in
//! the browser, filtered by User, Application, Resource, outcome and time,
//! and paged."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::Vm;
use blackbox::admin::{LIST, OPERATION_LOG as API_LOG, every_entry, log_page};
use blackbox::admin_pages::{
    OPERATION_LOG, as_datetime_local, entries, entry_ids, field_errors, field_value, let_go,
    lock_everything, log_target, query_pairs, to_the_minute, to_the_second, wait_for_lock_waiters,
};
use blackbox::browser::target_of;
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options, start};
use blackbox::logs::encode;
use blackbox::scenario::{RESOURCE, lines};
use blackbox::transactions::{
    APP, GHOST, RO, Target, Txs, VIEWER, nobody, outside, rita, slow_select, wanda,
};
use serde_json::Value;

/// The program of the transaction tests, on the Dashboard's options, with
/// entries of every kind: allowed, refused and failed; on `devops`, on
/// [`RO`], on [`GHOST`] and on the own database; by wanda, rita and nobody;
/// in [`APP`] and [`VIEWER`]; and a pull (`application` null).
fn populated() -> (Txs, Vec<Value>) {
    let t = Txs::start_with(&stand_in_options());
    let devops = Target::Shared(RESOURCE);
    t.run(
        &wanda(),
        APP,
        &[
            outside("query", devops, "select 1 as one"),
            outside("query", Target::Own("user"), "select 1 as one"),
            outside("query", Target::Shared(GHOST), "select 1 as one"),
            outside("query", devops, "select 'x'::int4 as n"),
        ],
    );
    t.run(
        &rita(),
        VIEWER,
        &[outside("query", devops, "select 2 as two")],
    );
    t.run(
        &nobody(),
        APP,
        &[outside("query", devops, "select 3 as three")],
    );
    t.run(
        &rita(),
        APP,
        &[outside("query", Target::Shared(RO), "select 4 as four")],
    );
    t.vm()
        .api_as(&rita())
        .get(&format!("/v1/resources/{RESOURCE}/structure"))
        .expect_status(200);
    t.run(
        &wanda(),
        APP,
        &[outside("query", devops, "select 5 as five")],
    );
    let all = t.log();
    for (what, n) in [
        (
            "allowed",
            all.iter().filter(|e| e["outcome"] == "allowed").count(),
        ),
        (
            "refused",
            all.iter().filter(|e| e["outcome"] == "refused").count(),
        ),
        (
            "failed",
            all.iter().filter(|e| e["outcome"] == "failed").count(),
        ),
        (
            "own database",
            all.iter().filter(|e| e["resource"].is_null()).count(),
        ),
        (
            "a pull",
            all.iter().filter(|e| e["application"].is_null()).count(),
        ),
    ] {
        assert!(
            n > 0,
            "the harness's own check: expected {what} entries:\n{}",
            lines(&all)
        );
    }
    (t, all)
}

fn dashboard(vm: &Vm) -> Dashboard<'_> {
    Dashboard::new(vm, &administrator())
}

/// The Operation log page at `target`, which must be `200`.
fn log_page_at(vm: &Vm, target: &str) -> Page {
    dashboard(vm).page(target, 200, target)
}

/// The ids of the API's answer to `filters` with `limit=100`, newest first.
fn api_newest_first(vm: &Vm, filters: &str) -> Vec<String> {
    let join = if filters.is_empty() { "" } else { "&" };
    let page = log_page(vm, &format!("?{filters}{join}limit=100"));
    let mut ids: Vec<String> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].to_string())
        .collect();
    ids.reverse();
    ids
}

fn assert_same_as_api(vm: &Vm, target: &str, filters: &str) {
    let page = log_page_at(vm, target);
    assert_eq!(
        entry_ids(&page),
        api_newest_first(vm, filters),
        "{target}: expected the entries `GET /v1/operation-log?{filters}&limit=100` answers, newest first\n{}",
        page.describe()
    );
}

#[test]
fn each_filter_and_their_combination_show_what_the_api_answers_newest_first() {
    let (t, all) = populated();
    let vm = t.vm();
    for (query, filters) in [
        (String::new(), String::new()),
        ("user=wanda".into(), "user=wanda".into()),
        ("user=rita".into(), "user=rita".into()),
        (format!("application={APP}"), format!("application={APP}")),
        (
            format!("application={VIEWER}"),
            format!("application={VIEWER}"),
        ),
        (
            format!("resource={RESOURCE}"),
            format!("resource={RESOURCE}"),
        ),
        (format!("resource={GHOST}"), format!("resource={GHOST}")),
        ("outcome=allowed".into(), "outcome=allowed".into()),
        ("outcome=refused".into(), "outcome=refused".into()),
        ("outcome=failed".into(), "outcome=failed".into()),
        (
            format!("user=wanda&application={APP}&outcome=allowed"),
            format!("user=wanda&application={APP}&outcome=allowed"),
        ),
        (
            format!("resource={RESOURCE}&outcome=refused"),
            format!("resource={RESOURCE}&outcome=refused"),
        ),
    ] {
        let target = if query.is_empty() {
            OPERATION_LOG.to_owned()
        } else {
            format!("{OPERATION_LOG}?{query}")
        };
        assert_same_as_api(vm, &target, &filters);
    }
    assert!(all.len() > 5, "the harness's own check");
    // Other parameters, `limit` included, are ignored.
    let page = log_page_at(vm, &format!("{OPERATION_LOG}?limit=1&sort=asc&page=3"));
    assert_eq!(
        entry_ids(&page),
        api_newest_first(vm, ""),
        "other parameters ignored"
    );
}

#[test]
fn the_filters_are_one_get_form_with_labelled_fields_choices_and_one_button() {
    let (t, _) = populated();
    let page = log_page_at(t.vm(), OPERATION_LOG);
    let root = page.root();
    let forms = root.with_attr_value("data-vm-form", "operation-log");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"operation-log\"]`\n{}",
            page.describe()
        );
    };
    assert_eq!(
        form.attr("method").map(str::to_ascii_lowercase).as_deref(),
        Some("get")
    );
    assert_eq!(form.attr("action"), Some(OPERATION_LOG));
    let fields: Vec<(String, String, Option<String>)> = form
        .named("input")
        .iter()
        .filter(|i| {
            i.attr("type")
                .is_none_or(|t| !t.eq_ignore_ascii_case("submit"))
        })
        .map(|i| {
            (
                i.attr("name").unwrap_or_default().to_owned(),
                i.attr("type").unwrap_or("text").to_ascii_lowercase(),
                i.attr("step").map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        fields,
        vec![
            ("user".to_owned(), "text".to_owned(), None),
            ("application".to_owned(), "text".to_owned(), None),
            ("resource".to_owned(), "text".to_owned(), None),
            (
                "since".to_owned(),
                "datetime-local".to_owned(),
                Some("1".to_owned())
            ),
            (
                "until".to_owned(),
                "datetime-local".to_owned(),
                Some("1".to_owned())
            ),
        ],
        "expected the fields user, application, resource (text), since and until (datetime-local, step 1), in that order\n{}",
        page.describe()
    );
    let selects = form.named("select");
    let [select] = selects.as_slice() else {
        panic!("expected one `select` (outcome)\n{}", page.describe());
    };
    assert_eq!(select.attr("name"), Some("outcome"));
    let options: Vec<(String, String)> = select
        .named("option")
        .iter()
        .map(|o| (o.attr("value").unwrap_or("?").to_owned(), o.text()))
        .collect();
    assert_eq!(
        options,
        [
            ("", "Any"),
            ("allowed", "Allowed"),
            ("refused", "Refused"),
            ("failed", "Failed"),
            ("unfinished", "Unfinished")
        ]
        .map(|(v, t)| (v.to_owned(), t.to_owned())),
        "the outcome's choices"
    );
    // Each field has a visible label naming it; the two times say UTC.
    let labels: Vec<(Option<String>, String)> = form
        .named("label")
        .iter()
        .map(|l| (l.attr("for").map(str::to_owned), l.text()))
        .collect();
    for words in [
        "User id",
        "Application",
        "Resource",
        "From (UTC)",
        "Before (UTC)",
    ] {
        assert!(
            labels.iter().any(|(_, t)| t.contains(words)),
            "expected a label {words:?}, the labels are {labels:?}"
        );
    }
    let buttons: Vec<String> = form.named("button").iter().map(|b| b.text()).collect();
    assert_eq!(
        buttons,
        ["Show entries"],
        "one submit button, \"Show entries\""
    );
    assert!(
        page.link("data-vm-clear").is_none(),
        "no `a[data-vm-clear]` with no filter given"
    );
    let filtered = log_page_at(t.vm(), &format!("{OPERATION_LOG}?user=wanda"));
    let clear = filtered.link("data-vm-clear");
    assert!(
        clear
            .as_ref()
            .is_some_and(|c| c.path() == OPERATION_LOG && c.query().is_none()),
        "expected `a[data-vm-clear]` to {OPERATION_LOG} when a filter is given, got {clear:?}"
    );
}

#[test]
fn an_empty_field_is_a_filter_not_given_white_space_is_dropped_and_each_field_holds_its_filter() {
    let (t, all) = populated();
    let vm = t.vm();
    // The form as a browser sends it with nothing typed.
    let empty = format!("{OPERATION_LOG}?user=&application=&resource=&outcome=&since=&until=");
    assert_same_as_api(vm, &empty, "");
    let page = log_page_at(vm, &empty);
    assert!(
        page.link("data-vm-clear").is_none(),
        "no filter given: no `a[data-vm-clear]`"
    );
    // Only some typed.
    let some =
        format!("{OPERATION_LOG}?user=wanda&application=&resource=&outcome=refused&since=&until=");
    assert_same_as_api(vm, &some, "user=wanda&outcome=refused");
    // White space around a value is dropped.
    let spaced = format!(
        "{OPERATION_LOG}?{}",
        blackbox::dashboard::form_body(&[
            ("user", "  wanda "),
            ("application", &format!(" {APP}\t")),
            ("resource", ""),
            ("outcome", " allowed "),
        ])
    );
    assert_same_as_api(
        vm,
        &spaced,
        &format!("user=wanda&application={APP}&outcome=allowed"),
    );
    // Each field holds the filter it was given.
    let time = all[1]["time"].as_str().unwrap();
    let target = log_target(&[
        ("user", "wanda"),
        ("application", APP),
        ("resource", RESOURCE),
        ("outcome", "allowed"),
        ("since", &as_datetime_local(time)),
    ]);
    let page = log_page_at(vm, &target);
    for (field, value) in [
        ("user", "wanda".to_owned()),
        ("application", APP.to_owned()),
        ("resource", RESOURCE.to_owned()),
        ("outcome", "allowed".to_owned()),
    ] {
        assert_eq!(
            field_value(&page, field).as_deref(),
            Some(value.as_str()),
            "{target}: expected the field {field} to hold {value:?}\n{}",
            page.describe()
        );
    }
    let since = field_value(&page, "since");
    assert_eq!(
        since.as_deref(),
        Some(to_the_second(time).as_str()),
        "{target}: expected the `since` field to hold the time given to the second, its fraction cut (`29b90b1`)"
    );
    let until = field_value(&page, "until");
    assert_eq!(until.as_deref(), Some(""), "a field not given is empty");
}

/// `utc` (`...Z`) written with the offset `+01:00`.
fn shift_to_plus_one(utc: &str) -> String {
    let hour: u32 = utc[11..13].parse().unwrap();
    assert!(
        hour < 23,
        "the harness's own bound: run this test before 23:00 UTC"
    );
    format!(
        "{}{:02}{}+01:00",
        &utc[..11],
        hour + 1,
        &utc[13..utc.len() - 1]
    )
}

#[test]
fn a_time_from_a_datetime_local_field_is_read_as_utc_and_rfc_3339_is_taken_from_an_address() {
    let (t, all) = populated();
    let vm = t.vm();
    let pivot = all[all.len() / 2]["time"].as_str().unwrap().to_owned();
    let api_since = format!("since={}", encode(&pivot));
    // As `datetime-local` sends it, with the fraction: the same as the API's
    // `since` with `Z`.
    for (what, typed) in [
        ("with a fraction of six digits", as_datetime_local(&pivot)),
        ("to the second", to_the_second(&pivot)),
        ("to the minute", to_the_minute(&pivot)),
    ] {
        let target = log_target(&[("since", &typed)]);
        let api = if typed.len() == pivot.len() - 1 {
            api_since.clone()
        } else {
            let zeroes = if typed.len() == 16 { ":00Z" } else { "Z" };
            format!("since={}", encode(&format!("{typed}{zeroes}")))
        };
        let page = log_page_at(vm, &target);
        assert_eq!(
            entry_ids(&page),
            api_newest_first(vm, &api),
            "{what}: `since={typed}` read as that time in UTC, as the API's {api}"
        );
        let until_target = log_target(&[("until", &typed)]);
        let api_until = api.replacen("since=", "until=", 1);
        assert_eq!(
            entry_ids(&log_page_at(vm, &until_target)),
            api_newest_first(vm, &api_until),
            "{what}: `until={typed}` read as that time in UTC"
        );
    }
    // RFC 3339 with an offset, as an address written by hand or by a
    // Coding agent: `Z` (either case) and `+01:00` (written `%2B`).
    for written in [
        pivot.clone(),
        pivot.replace('T', "t").replace('Z', "z"),
        shift_to_plus_one(&pivot),
    ] {
        let target = format!("{OPERATION_LOG}?since={}", encode(&written));
        assert_eq!(
            entry_ids(&log_page_at(vm, &target)),
            api_newest_first(vm, &api_since),
            "since={written}: RFC 3339 with an offset, as the API takes it"
        );
    }
    // A window: since and until together, the entries of the API's window.
    let first = all[0]["time"].as_str().unwrap();
    let last = all[all.len() - 1]["time"].as_str().unwrap();
    let target = log_target(&[
        ("since", &as_datetime_local(first)),
        ("until", &as_datetime_local(last)),
    ]);
    assert_eq!(
        entry_ids(&log_page_at(vm, &target)),
        api_newest_first(
            vm,
            &format!("since={}&until={}", encode(first), encode(last))
        ),
        "a window typed in both fields"
    );
}

#[test]
fn a_time_typed_with_microseconds_is_held_by_its_field_to_the_second_its_fraction_cut_and_the_entries_are_the_apis_for_it()
 {
    let (t, all) = populated();
    let vm = t.vm();
    // An entry's time, with its microseconds, as an address typed by hand
    // (no `Z`: a `datetime-local` value, read as UTC). One whose fraction
    // is half a second or more where there is one, so that a fraction
    // rounded would show as another second than one cut.
    let times: Vec<&str> = all
        .iter()
        .filter_map(|e| e["time"].as_str())
        .filter(|time| {
            time.split_once('.')
                .is_some_and(|(_, f)| f.trim_end_matches('Z').len() == 6)
        })
        .collect();
    let pivot = times
        .iter()
        .find(|time| {
            time.split_once('.')
                .is_some_and(|(_, f)| f.starts_with(['5', '6', '7', '8', '9']))
        })
        .or_else(|| times.first())
        .unwrap_or_else(|| {
            panic!("the harness's own check: an entry's time with six digits of a second")
        })
        .to_string();
    let typed = as_datetime_local(&pivot);
    let held = to_the_second(&pivot);
    for field in ["since", "until"] {
        let page = log_page_at(vm, &log_target(&[(field, &typed)]));
        // The entries first: those the API answers for the time as typed,
        // to the microsecond, whatever the field shows.
        assert_eq!(
            entry_ids(&page),
            api_newest_first(vm, &format!("{field}={}", encode(&pivot))),
            "`{field}={typed}`: the entries the API answers for the time as typed (to the microsecond)"
        );
        assert_eq!(
            field_value(&page, field).as_deref(),
            Some(held.as_str()),
            "`{field}={typed}`: expected the field to hold the time to the second, its fraction cut, not rounded (`29b90b1`)"
        );
    }
}

#[test]
fn following_older_to_the_end_shows_every_matching_entry_once_and_newest_leads_back() {
    let t = Txs::start_with(&stand_in_options());
    let devops = Target::Shared(RESOURCE);
    let steps: Vec<Value> = (0..130)
        .map(|i| outside("query", devops, &format!("select {i} as n")))
        .collect();
    t.run(&wanda(), APP, &steps);
    t.run(
        &rita(),
        VIEWER,
        &[outside("query", devops, "select 1 as one")],
    );
    let vm = t.vm();
    let filters = format!("user=wanda&application={APP}&resource=&outcome=allowed");
    let (all, _) = every_entry(
        vm,
        &format!("user=wanda&application={APP}&outcome=allowed"),
        100,
    );
    assert!(
        all.len() > 100,
        "the harness's own check: {} entries",
        all.len()
    );
    let mut wanted: Vec<String> = all.iter().map(|e| e["id"].to_string()).collect();
    wanted.reverse();
    let dashboard = dashboard(vm);
    let mut page = dashboard.page(&format!("{OPERATION_LOG}?{filters}"), 200, "the newest");
    assert!(
        page.link("data-vm-newest").is_none(),
        "no `before`: no `a[data-vm-newest]`"
    );
    let mut seen: Vec<String> = Vec::new();
    for round in 0..20 {
        let shown = entry_ids(&page);
        assert!(shown.len() <= 100, "round {round}: at most 100 to a page");
        seen.extend(shown);
        let Some(older) = page.link("data-vm-older") else {
            break;
        };
        let pairs = query_pairs(&older);
        let names: BTreeSet<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["application", "before", "outcome", "user"]),
            "round {round}: `a[data-vm-older]` keeps the filters, leaves out the empty ones, adds `before`: {older}"
        );
        page = dashboard.page(&target_of(&older), 200, "an older page");
        let newest = page.link("data-vm-newest");
        assert!(
            newest.as_ref().is_some_and(|n| {
                let p = query_pairs(n);
                n.path() == OPERATION_LOG
                    && p.iter().all(|(k, _)| k != "before")
                    && p.contains(&("user".to_owned(), "wanda".to_owned()))
                    && p.contains(&("outcome".to_owned(), "allowed".to_owned()))
            }),
            "round {round}: with `before`, `a[data-vm-newest]` leads to the same filters without it, got {newest:?}"
        );
    }
    assert_eq!(
        seen, wanted,
        "following `a[data-vm-older]` until there is none shows every matching entry once"
    );
    // `before` typed in the address is taken as the API takes it.
    let before = &all[all.len() - 5]["id"];
    let page = log_page_at(vm, &format!("{OPERATION_LOG}?before={before}"));
    assert_eq!(
        entry_ids(&page),
        api_newest_first(vm, &format!("before={before}"))
    );
}

#[test]
fn each_entry_says_first_its_outcome_time_user_application_resource_operation_and_reason() {
    let (t, all) = populated();
    let vm = t.vm();
    let page = log_page_at(vm, OPERATION_LOG);
    let shown = entries(&page);
    let mut by_id = std::collections::HashMap::new();
    for e in &all {
        by_id.insert(e["id"].to_string(), e.clone());
    }
    let marks = [
        ("allowed", "allowed"),
        ("refused", "refused"),
        ("failed", "failed"),
        ("unfinished", "unfinished"),
    ];
    assert_eq!(
        shown.len(),
        all.len(),
        "every entry shown\n{}",
        page.describe()
    );
    for s in &shown {
        let entry = by_id
            .get(&s.id)
            .unwrap_or_else(|| panic!("an entry shown that the API has not: {}", s.id));
        let context = format!("entry {}", s.id);
        assert_eq!(
            s.outcome.as_deref(),
            entry["outcome"].as_str(),
            "{context}: data-vm-outcome"
        );
        assert_eq!(
            s.time.as_deref(),
            entry["time"].as_str(),
            "{context}: data-vm-time"
        );
        let outcome = entry["outcome"].as_str().unwrap();
        let word = marks.iter().find(|(o, _)| *o == outcome).unwrap().1;
        assert!(
            s.marks.iter().flatten().any(|l| l.to_lowercase() == word),
            "{context}: expected the outcome as a mark named {word:?}, its marks are named {:?}",
            s.marks
        );
        let time = entry["time"].as_str().unwrap();
        let second = to_the_second(time);
        assert!(
            s.first.contains(&second) || s.first.contains(&second.replace('T', " ")),
            "{context}: expected its time, in UTC, to the second ({second}), seen first; it says {:?}",
            s.first
        );
        match entry["user"].as_str() {
            Some(u) => assert!(s.first.contains(u), "{context}: the User {u}"),
            None => assert!(
                s.first.contains('—') || s.first.contains('-'),
                "{context}: a dash for no User"
            ),
        }
        match entry["application"].as_str() {
            Some(a) => assert!(s.first.contains(a), "{context}: the Application {a}"),
            None => assert!(
                s.first.contains('—') || s.first.contains('-'),
                "{context}: a dash for no Application"
            ),
        }
        match entry["resource"].as_str() {
            Some(r) => assert!(s.first.contains(r), "{context}: the Resource {r}"),
            None => assert!(
                s.first.to_lowercase().contains("own database"),
                "{context}: the words \"own database\""
            ),
        }
        let operation = entry["operation"].as_str().unwrap();
        assert!(
            s.first.contains(operation),
            "{context}: the operation {operation}"
        );
        match outcome {
            "refused" => assert_eq!(
                s.reason.as_deref(),
                entry["reason"].as_str(),
                "{context}: `data-vm-reason`"
            ),
            "failed" => assert_eq!(
                s.error_code.as_deref(),
                entry["error"].as_str(),
                "{context}: `data-vm-error-code`"
            ),
            _ => assert!(
                s.reason.is_none() && s.error_code.is_none(),
                "{context}: no reason or error"
            ),
        }
        // One step away: the statement, rows, duration, request,
        // transaction, actor and id.
        let details = s.details.clone().unwrap_or_else(|| {
            panic!(
                "{context}: expected a `<details>` in the entry\n{}",
                page.describe()
            )
        });
        if let Some(statement) = entry["statement"].as_str() {
            let squashed = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                details.contains(&squashed),
                "{context}: the statement in its details, got {details:?}"
            );
            assert!(
                !s.first.contains(&squashed),
                "{context}: the statement is one step away, not seen first"
            );
        }
        for member in ["request", "transaction", "actor"] {
            if let Some(v) = entry[member].as_str() {
                assert!(
                    details.contains(v),
                    "{context}: `{member}` {v} in its details"
                );
            }
        }
        for member in ["rows", "duration_ms"] {
            if let Some(v) = entry[member].as_u64() {
                assert!(
                    details
                        .split(|c: char| !c.is_ascii_digit())
                        .any(|w| w == v.to_string()),
                    "{context}: `{member}` {v} in its details, got {details:?}"
                );
            }
        }
        assert!(details.contains(&s.id), "{context}: its id in its details");
        assert!(
            !s.has_user_mark,
            "{context}: an entry carries no `data-vm-user` (said once)"
        );
    }
    // The column's head says UTC once.
    let text = page.body_text();
    assert!(
        text.contains("UTC"),
        "expected the words UTC on the page\n{}",
        page.describe()
    );
    let in_entries: usize = shown.iter().map(|s| s.text.matches("UTC").count()).sum();
    assert_eq!(
        in_entries, 0,
        "UTC is said once, at the head, not in each entry"
    );
}

#[test]
fn an_unfinished_entry_is_shown_with_its_own_mark() {
    let t = Txs::start_with(&stand_in_options());
    let marker = format!("slow-{}", blackbox::shared_database::token());
    let vm = t.vm();
    thread::scope(|scope| {
        let running = scope.spawn(|| {
            t.send(
                &wanda(),
                APP,
                &[outside(
                    "query",
                    Target::Shared(RESOURCE),
                    &slow_select(6.0, &marker),
                )],
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if t.log().iter().any(|e| e["outcome"] == "unfinished") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the harness's own wait: an unfinished entry"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let page = log_page_at(vm, &format!("{OPERATION_LOG}?outcome=unfinished"));
        let shown = entries(&page);
        assert!(
            matches!(shown.as_slice(), [s] if s.outcome.as_deref() == Some("unfinished")
                && s.marks.iter().flatten().any(|l| l.eq_ignore_ascii_case("unfinished"))),
            "expected the unfinished entry with its mark named \"Unfinished\": {shown:?}\n{}",
            page.describe()
        );
        running.join().unwrap();
    });
}

#[test]
fn the_user_application_and_resource_of_an_entry_are_links_narrowing_to_them() {
    let (t, all) = populated();
    let vm = t.vm();
    let before = all[all.len() - 1]["id"].to_string();
    let target = format!("{OPERATION_LOG}?outcome=allowed&before={before}&since=");
    let page = log_page_at(vm, &target);
    let shown = entries(&page);
    assert!(!shown.is_empty(), "the harness's own check");
    for s in &shown {
        let entry = all
            .iter()
            .find(|e| e["id"].as_u64() == s.id.parse::<u64>().ok())
            .unwrap();
        for kind in ["user", "application", "resource"] {
            let links: Vec<&url::Url> = s
                .filters
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, u)| u)
                .collect();
            match entry[kind].as_str() {
                Some(value) => {
                    let [link] = links.as_slice() else {
                        panic!(
                            "entry {}: expected one `a[data-vm-filter={kind:?}]`, got {links:?}",
                            s.id
                        );
                    };
                    let mut pairs = query_pairs(link);
                    pairs.sort();
                    let mut wanted = vec![
                        ("outcome".to_owned(), "allowed".to_owned()),
                        (kind.to_owned(), value.to_owned()),
                    ];
                    wanted.sort();
                    assert_eq!(
                        (link.path(), pairs),
                        (OPERATION_LOG, wanted),
                        "entry {}: the {kind} filter link keeps the other filters, sets {kind}, drops `before`",
                        s.id
                    );
                }
                None => assert!(
                    links.is_empty(),
                    "entry {}: no {kind} to narrow to, no link",
                    s.id
                ),
            }
        }
    }
    // Following one narrows to it.
    let wandas = shown
        .iter()
        .find_map(|s| {
            s.filters.iter().find(|(k, u)| {
                k == "user" && query_pairs(u).contains(&("user".to_owned(), "wanda".to_owned()))
            })
        })
        .unwrap();
    let narrowed = log_page_at(vm, &target_of(&wandas.1));
    assert_eq!(
        entry_ids(&narrowed),
        api_newest_first(vm, "outcome=allowed&user=wanda")
    );
}

#[test]
fn nothing_matching_is_said_in_words_naming_each_filter_and_an_empty_log_says_it_has_no_entry_yet()
{
    let (t, _) = populated();
    let vm = t.vm();
    let hostile = "<b>nobody</b> & co";
    let target = log_target(&[("user", hostile), ("outcome", "unfinished")]);
    let page = log_page_at(vm, &target);
    assert!(entries(&page).is_empty(), "nothing matches");
    let empty: Vec<String> = page
        .root()
        .with_attr("data-vm-empty")
        .iter()
        .map(|e| e.text())
        .collect();
    let [words] = empty.as_slice() else {
        panic!(
            "expected one element `data-vm-empty`, got {empty:?}\n{}",
            page.describe()
        );
    };
    assert!(
        words.contains(hostile)
            && words.contains("unfinished")
            && words.to_lowercase().contains("no entry"),
        "expected words that no entry matches, naming each filter and its value, got {words:?}"
    );
    assert!(
        !page.response.text().contains("<b>nobody</b>"),
        "the value escaped as HTML"
    );
    let fresh = start();
    let page = log_page_at(&fresh, OPERATION_LOG);
    let empty: Vec<String> = page
        .root()
        .with_attr("data-vm-empty")
        .iter()
        .map(|e| e.text())
        .collect();
    assert!(
        matches!(empty.as_slice(), [w] if w.to_lowercase().contains("no entry yet") && w.to_lowercase().contains("operation")),
        "expected words that the Operation log has no entry yet and that each operation writes one, got {empty:?}"
    );
}

#[test]
fn a_malformed_filter_is_400_beside_its_field_with_what_was_typed_kept() {
    let (t, _) = populated();
    let vm = t.vm();
    let user_257 = "u".repeat(257);
    let cases: Vec<(&str, Vec<(&str, String)>)> = vec![
        ("user", vec![("user", user_257)]),
        ("user", vec![("user", "a\u{1}b".to_owned())]),
        (
            "application",
            vec![("application", "Not_A_Name".to_owned())],
        ),
        ("application", vec![("application", "a".repeat(64))]),
        ("resource", vec![("resource", "Not A Name".to_owned())]),
        ("outcome", vec![("outcome", "maybe".to_owned())]),
        ("since", vec![("since", "yesterday".to_owned())]),
        ("since", vec![("since", "2026-13-01T10:00".to_owned())]),
        (
            "since",
            vec![("since", "2026-11-21T10:00:00.1234567".to_owned())],
        ),
        ("until", vec![("until", "2026-11-21".to_owned())]),
        (
            "until",
            vec![
                ("since", "2026-11-22T10:00".to_owned()),
                ("until", "2026-11-21T10:00".to_owned()),
            ],
        ),
        (
            "until",
            vec![
                ("since", "2026-11-21T10:00".to_owned()),
                ("until", "2026-11-21T10:00".to_owned()),
            ],
        ),
        (
            "user",
            vec![("user", "wanda".to_owned()), ("user", "rita".to_owned())],
        ),
    ];
    for (field, filters) in &cases {
        let fields: Vec<(&str, &str)> = filters.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let target = log_target(&fields);
        let page = dashboard(vm).page(&target, 400, &target);
        assert!(
            entries(&page).is_empty(),
            "{target}: no entry on a malformed filter"
        );
        assert_eq!(
            page.root()
                .with_attr_value("data-vm-form", "operation-log")
                .len(),
            1,
            "{target}: the page with the form"
        );
        let errors = field_errors(&page);
        assert!(
            errors
                .iter()
                .any(|(f, words)| f == field && !words.is_empty()),
            "{target}: expected `data-vm-field-error={field:?}` with words, got {errors:?}\n{}",
            page.describe()
        );
        for (name, value) in filters {
            if ["user", "application", "resource"].contains(name)
                && filters.iter().filter(|(n, _)| n == name).count() == 1
            {
                assert_eq!(
                    field_value(&page, name).as_deref(),
                    Some(value.as_str()),
                    "{target}: what was typed in {name} is kept"
                );
            }
        }
    }
    // A time the browser's field cannot hold is quoted in the error's
    // words.
    let target = log_target(&[("since", "yesterday")]);
    let page = dashboard(vm).page(&target, 400, &target);
    assert!(
        field_errors(&page)
            .iter()
            .any(|(f, w)| f == "since" && w.contains("yesterday")),
        "expected the time typed quoted in the error's words, got {:?}",
        field_errors(&page)
    );
    // The select keeps the choice made.
    let target = log_target(&[("outcome", "refused"), ("application", "Not_A_Name")]);
    let page = dashboard(vm).page(&target, 400, &target);
    assert_eq!(
        field_value(&page, "outcome").as_deref(),
        Some("refused"),
        "the choice made is kept"
    );
    // `before` that is no positive whole number: over the entries' place,
    // with `a[data-vm-newest]`.
    for before in ["0", "-1", "x", "01"] {
        let target = format!("{OPERATION_LOG}?user=wanda&before={before}");
        let page = dashboard(vm).page(&target, 400, &target);
        assert!(
            field_errors(&page)
                .iter()
                .any(|(f, w)| f == "before" && !w.is_empty()),
            "{target}: expected `data-vm-field-error=\"before\"`, got {:?}",
            field_errors(&page)
        );
        let newest = page.link("data-vm-newest");
        assert!(
            newest.as_ref().is_some_and(|n| n.path() == OPERATION_LOG
                && query_pairs(n) == vec![("user".to_owned(), "wanda".to_owned())]),
            "{target}: expected `a[data-vm-newest]` to the same filters without `before`, got {newest:?}"
        );
    }
}

#[test]
fn a_reading_past_10_seconds_is_503_without_retry_after_saying_to_narrow_it() {
    let vm = start();
    let dashboard = dashboard(&vm);
    log_page(&vm, "");
    let lock = lock_everything(&vm);
    let began = Instant::now();
    let target = log_target(&[("user", "wanda"), ("outcome", "failed")]);
    let answer = dashboard
        .navigation("GET", &target)
        .timeout(Duration::from_secs(40))
        .send();
    let took = began.elapsed();
    let_go(&lock);
    assert!(
        answer.status == 503 && answer.header("retry-after").is_none(),
        "expected `503` with no `retry-after` for a reading past 10 s\n{}",
        answer.describe()
    );
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(16),
        "expected the reading ended after its 10 seconds, it took {took:?}"
    );
    let page = Page::read(answer, dashboard.url(&target), "past 10 s");
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, w)] if r == "unavailable" && w.to_lowercase().contains("too long")),
        "expected `data-vm-error=\"unavailable\"` saying the reading took too long, got {errors:?}"
    );
    assert!(
        page.body_text().to_lowercase().contains("narrow"),
        "and to narrow it"
    );
    assert_eq!(
        field_value(&page, "user").as_deref(),
        Some("wanda"),
        "the filters kept"
    );
    assert_eq!(
        field_value(&page, "outcome").as_deref(),
        Some("failed"),
        "the filters kept"
    );
}

#[test]
fn a_third_reading_at_once_is_503_at_once_with_retry_after_on_the_log_and_on_every_application() {
    let vm = start();
    let dashboard = dashboard(&vm);
    log_page(&vm, "");
    let lock = lock_everything(&vm);
    let read = |target: &str| {
        vm.api_as(&administrator())
            .request(target)
            .timeout(Duration::from_secs(40))
            .send()
    };
    let (third, took, applications, took_applications) = thread::scope(|scope| {
        let first = scope.spawn(|| read(LIST));
        let second = scope.spawn(|| read(API_LOG));
        wait_for_lock_waiters(&vm, 2, "two readings");
        let began = Instant::now();
        let target = log_target(&[("user", "wanda")]);
        let third = dashboard
            .navigation("GET", &target)
            .timeout(Duration::from_secs(40))
            .send();
        let took = began.elapsed();
        let began = Instant::now();
        let applications = dashboard
            .navigation("GET", "/admin/applications")
            .timeout(Duration::from_secs(40))
            .send();
        let took_applications = began.elapsed();
        let_go(&lock);
        first.join().unwrap();
        second.join().unwrap();
        (third, took, applications, took_applications)
    });
    for (what, answer, took) in [
        ("the Operation log", &third, took),
        ("every Application", &applications, took_applications),
    ] {
        assert!(
            answer.status == 503
                && answer.header("retry-after").is_some()
                && took < Duration::from_secs(2),
            "{what}: expected `503` with `retry-after` at once while two readings run, got after {took:?}\n{}",
            answer.describe()
        );
    }
    let page = Page::read(third, dashboard.url(OPERATION_LOG), "the third reading");
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, w)] if r == "unavailable" && w.to_lowercase().contains("again")),
        "expected `data-vm-error=\"unavailable\"` saying to try again shortly, got {errors:?}"
    );
    assert_eq!(
        field_value(&page, "user").as_deref(),
        Some("wanda"),
        "the form and its filters kept"
    );
    // Once they are done, the pages are read again.
    dashboard.page(OPERATION_LOG, 200, "after");
}
