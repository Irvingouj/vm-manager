//! Reading the Operation log with filters (CONTRACT.md, "What Platform
//! administrators see and stop: Reading the Operation log"): `user`,
//! `application`, `resource`, `outcome`, `since` (inclusive) and `until`
//! (exclusive), alone and combined; `before` and `earlier` page through
//! every matching entry once, newest page first, each page oldest first;
//! `limit` alone answers what it answered before; an answer stops before
//! 4 MiB with at least one entry; entries with a null `application` or
//! `resource` match no such filter; every malformed parameter is `400`
//! naming it; another method is `405`.
//!
//! Acceptance criterion: "The Operation log can be filtered by User,
//! Application, Resource, outcome and time, and paged."
#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::unwrap_used,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{
    OPERATION_LOG, administrator, every_entry, ids, log_page, micros, operation_log,
};
use blackbox::logs::encode;
use blackbox::scenario::{READERS, RESOURCE, Scenario, lines};
use blackbox::transactions::{APP, GHOST, RO, Target, Txs, VIEWER, nobody, outside, rita, wanda};
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

/// The program of the transaction tests with entries of every kind in its
/// Operation log: allowed, refused and failed; on `devops`, on [`RO`], on
/// [`GHOST`] (never registered) and on the own database (`resource`
/// null); by wanda, rita and nobody; in [`APP`] and [`VIEWER`]; and a pull
/// of `devops`'s structure (`application` null).
fn populated() -> (Txs, Vec<Value>) {
    let t = Txs::start();
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
    let pulled = t
        .vm()
        .api_as(&rita())
        .get(&format!("/v1/resources/{RESOURCE}/structure"));
    pulled.expect_status(200);
    t.run(
        &wanda(),
        APP,
        &[outside("query", devops, "select 5 as five")],
    );
    let all = t.log();
    let kinds = |pred: &dyn Fn(&Value) -> bool| all.iter().filter(|e| pred(e)).count();
    // The harness's own check that every kind is there.
    for (what, n) in [
        ("an allowed entry", kinds(&|e| e["outcome"] == "allowed")),
        ("a refused entry", kinds(&|e| e["outcome"] == "refused")),
        ("a failed entry", kinds(&|e| e["outcome"] == "failed")),
        ("an own database entry", kinds(&|e| e["resource"].is_null())),
        ("a pull", kinds(&|e| e["application"].is_null())),
        ("a ghost entry", kinds(&|e| e["resource"] == GHOST)),
    ] {
        assert!(
            n > 0,
            "the harness's own check: expected {what} in the Operation log:\n{}",
            lines(&all)
        );
    }
    (t, all)
}

/// The entries of `all` that `keep` matches, in order.
fn matching(all: &[Value], keep: impl Fn(&Value) -> bool) -> Vec<Value> {
    all.iter().filter(|e| keep(e)).cloned().collect()
}

/// Fails unless the entries answered with `filters` (read whole with a
/// large limit, and a page of 2 at a time through `before`) are exactly
/// `want`, each once, in order.
fn assert_filtered(t: &Txs, filters: &str, want: &[Value]) {
    let whole = log_page(t.vm(), &format!("?{filters}&limit=1000"));
    assert_eq!(
        whole["entries"].as_array().unwrap().as_slice(),
        want,
        "?{filters}: expected exactly the matching entries, oldest first"
    );
    assert!(
        whole["earlier"].is_null(),
        "?{filters}: nothing older matches: {}",
        whole["earlier"]
    );
    let (paged, pages) = every_entry(t.vm(), filters, 2);
    assert_eq!(
        ids(&paged),
        ids(want),
        "?{filters}: paging by `before` gives every matching entry once (pages, newest first: {pages:?})"
    );
    if want.len() > 2 {
        assert!(
            pages.len() >= 2,
            "?{filters}: expected more than one page: {pages:?}"
        );
        for page in &pages[..pages.len() - 1] {
            assert_eq!(
                page.len(),
                2,
                "?{filters}: pages are full but the oldest: {pages:?}"
            );
        }
    }
}

#[test]
fn each_filter_alone_answers_exactly_the_entries_that_match_it() {
    let (t, all) = populated();
    for user in ["wanda", "rita", "nobody", "no-such-user"] {
        assert_filtered(
            &t,
            &format!("user={}", encode(user)),
            &matching(&all, |e| e["user"] == user),
        );
    }
    for application in [APP, VIEWER, "never-was"] {
        assert_filtered(
            &t,
            &format!("application={application}"),
            &matching(&all, |e| e["application"] == application),
        );
    }
    for resource in [RESOURCE, RO, GHOST, "never_registered"] {
        assert_filtered(
            &t,
            &format!("resource={resource}"),
            &matching(&all, |e| e["resource"] == resource),
        );
    }
    for outcome in ["allowed", "refused", "failed", "unfinished"] {
        assert_filtered(
            &t,
            &format!("outcome={outcome}"),
            &matching(&all, |e| e["outcome"] == outcome),
        );
    }
    // No filter: every entry, paged.
    let (paged, _) = every_entry(t.vm(), "", 3);
    assert_eq!(ids(&paged), ids(&all));
}

#[test]
fn filters_combine_and_null_application_or_resource_match_no_such_filter() {
    let (t, all) = populated();
    assert_filtered(
        &t,
        &format!("user=wanda&application={APP}&outcome=allowed"),
        &matching(&all, |e| {
            e["user"] == "wanda" && e["application"] == APP && e["outcome"] == "allowed"
        }),
    );
    assert_filtered(
        &t,
        &format!("resource={RESOURCE}&outcome=refused"),
        &matching(&all, |e| {
            e["resource"] == RESOURCE && e["outcome"] == "refused"
        }),
    );
    assert_filtered(
        &t,
        &format!("user=rita&resource={RESOURCE}"),
        &matching(&all, |e| e["user"] == "rita" && e["resource"] == RESOURCE),
    );
    // The pull's entry has a null `application`: no application filter
    // matches it, and the resource filter does.
    let pulls = matching(&all, |e| e["application"].is_null());
    let by_resource = log_page(t.vm(), &format!("?resource={RESOURCE}&limit=1000"));
    let answered = by_resource["entries"].as_array().unwrap();
    for pull in &pulls {
        assert!(
            answered.contains(pull),
            "a pull of {RESOURCE} matches resource={RESOURCE}"
        );
    }
    for application in [APP, VIEWER] {
        let page = log_page(t.vm(), &format!("?application={application}&limit=1000"));
        assert!(
            page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| !e["application"].is_null()),
            "an entry with a null application matches no application filter"
        );
    }
    // Own database entries have a null `resource`: no resource filter
    // matches them.
    for resource in [RESOURCE, RO, GHOST] {
        let page = log_page(t.vm(), &format!("?resource={resource}&limit=1000"));
        assert!(
            page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["resource"] == resource),
            "resource={resource}: only its own entries, never an own database's"
        );
    }
}

#[test]
fn since_is_inclusive_and_until_exclusive_and_an_offset_is_taken() {
    let (t, all) = populated();
    let at = |e: &Value| micros(e["time"].as_str().unwrap()).unwrap();
    let pivot = &all[all.len() / 2];
    let time = pivot["time"].as_str().unwrap().to_owned();
    // `time` is RFC 3339 in UTC with microseconds.
    assert!(
        time.ends_with('Z') && time.len() == "2026-11-21T10:20:30.123456Z".len(),
        "expected `time` in UTC with microseconds, got {time:?}"
    );
    let since = matching(&all, |e| at(e) >= at(pivot));
    assert!(
        since.contains(pivot),
        "given back as `since`, an entry's time matches it"
    );
    assert_filtered(&t, &format!("since={}", encode(&time)), &since);
    let until = matching(&all, |e| at(e) < at(pivot));
    assert!(!until.contains(pivot), "`until` is exclusive");
    assert_filtered(&t, &format!("until={}", encode(&time)), &until);
    // Both: a window holding the pivot alone (or those written at the
    // same microsecond).
    let first = &all[0];
    let last = &all[all.len() - 1];
    assert_filtered(
        &t,
        &format!(
            "since={}&until={}",
            encode(first["time"].as_str().unwrap()),
            encode(last["time"].as_str().unwrap())
        ),
        &matching(&all, |e| at(e) >= at(first) && at(e) < at(last)),
    );
    // The same instant with an offset, `+` written `%2B`.
    let micros_of_pivot = at(pivot);
    let shifted = shift_to_plus_one(&time);
    assert_eq!(
        micros(&shifted),
        Some(micros_of_pivot),
        "the harness's own check"
    );
    assert_filtered(&t, &format!("since={}", encode(&shifted)), &since);
    // A time without fraction and with `Z`.
    let whole_seconds = format!("{}Z", &time[..19]);
    let from_second = matching(&all, |e| at(e) >= micros(&whole_seconds).unwrap());
    assert_filtered(
        &t,
        &format!("since={}", encode(&whole_seconds)),
        &from_second,
    );
    // Long ago and far ahead.
    assert_filtered(&t, "since=2000-01-01T00:00:00Z", &all);
    assert_filtered(&t, "until=2000-01-01T00:00:00Z", &[]);
}

/// The same instant as `utc` (`...Z`), written with the offset `+01:00`.
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
fn limit_alone_answers_as_before_with_earlier_beside_it() {
    let (t, all) = populated();
    let n = all.len();
    for limit in [1, 3, n] {
        let page = log_page(t.vm(), &format!("?limit={limit}"));
        assert_eq!(
            page["entries"].as_array().unwrap().as_slice(),
            &all[n - limit..],
            "?limit={limit}: the newest {limit}, oldest first"
        );
        let want = if limit < n {
            json!(all[n - limit]["id"])
        } else {
            Value::Null
        };
        assert_eq!(page["earlier"], want, "?limit={limit}: `earlier`");
    }
    let without = log_page(t.vm(), "");
    assert_eq!(without["entries"].as_array().unwrap().as_slice(), &all[..]);
    assert!(without["earlier"].is_null());
    // `before` alone: the newest `limit` below it.
    let before = all[n - 2]["id"].as_u64().unwrap();
    let page = log_page(t.vm(), &format!("?before={before}&limit=2"));
    assert_eq!(
        page["entries"].as_array().unwrap().as_slice(),
        &all[n - 4..n - 2]
    );
    // Nothing answered: `earlier` null.
    let first = all[0]["id"].as_u64().unwrap();
    let none = log_page(t.vm(), &format!("?before={first}"));
    assert_eq!(none, json!({ "entries": [], "earlier": null }));
    let none = log_page(t.vm(), "?user=nobody-at-all");
    assert_eq!(none, json!({ "entries": [], "earlier": null }));
}

#[test]
fn a_malformed_parameter_is_refused_naming_it() {
    let (t, _) = populated();
    let user_257 = "u".repeat(257);
    let long_name = "a".repeat(64);
    let cases: Vec<(&str, String)> = vec![
        ("user", "user=".to_owned()),
        ("user", format!("user={user_257}")),
        ("user", "user=a%0Ab".to_owned()),
        ("user", "user=a&user=b".to_owned()),
        ("application", "application=".to_owned()),
        ("application", "application=Board".to_owned()),
        ("application", format!("application={long_name}")),
        ("application", "application=-x".to_owned()),
        ("application", "application=a&application=b".to_owned()),
        ("resource", "resource=".to_owned()),
        ("resource", "resource=Not%20A%20Name".to_owned()),
        ("resource", "resource=a&resource=b".to_owned()),
        ("outcome", "outcome=".to_owned()),
        ("outcome", "outcome=ALLOWED".to_owned()),
        ("outcome", "outcome=denied".to_owned()),
        ("outcome", "outcome=allowed&outcome=refused".to_owned()),
        ("since", "since=".to_owned()),
        ("since", "since=yesterday".to_owned()),
        ("since", "since=2026-11-21".to_owned()),
        ("since", "since=2026-11-21T10:00:00".to_owned()),
        ("since", "since=2026-11-21T10:00:00.1234567Z".to_owned()),
        ("since", "since=2026-11-21T10:00:00+01:00".to_owned()),
        ("since", "since=2026-13-01T10:00:00Z".to_owned()),
        (
            "since",
            "since=2026-11-21T10:00:00Z&since=2026-11-21T10:00:00Z".to_owned(),
        ),
        ("until", "until=".to_owned()),
        ("until", "until=now".to_owned()),
        (
            "until",
            "until=2026-11-21T10:00:00Z&until=2026-11-22T10:00:00Z".to_owned(),
        ),
        (
            "since",
            "since=2026-11-21T10:00:00Z&until=2026-11-21T10:00:00Z".to_owned(),
        ),
        (
            "since",
            "since=2026-11-22T10:00:00Z&until=2026-11-21T10:00:00Z".to_owned(),
        ),
        ("before", "before=".to_owned()),
        ("before", "before=0".to_owned()),
        ("before", "before=-1".to_owned()),
        ("before", "before=01".to_owned()),
        ("before", "before=%2B5".to_owned()),
        ("before", "before=abc".to_owned()),
        ("before", "before=5&before=6".to_owned()),
        ("limit", "limit=".to_owned()),
        ("limit", "limit=0".to_owned()),
        ("limit", "limit=1001".to_owned()),
        ("limit", "limit=05".to_owned()),
        ("limit", "limit=5&limit=5".to_owned()),
    ];
    for (parameter, query) in &cases {
        let answer = operation_log(t.vm(), &administrator(), &format!("?{query}"));
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let named = if *parameter == "since" && query.contains("until=") {
            message.contains("since") || message.contains("until")
        } else {
            message.contains(parameter)
        };
        assert!(
            named,
            "?{query}: expected the message to name the parameter {parameter:?}, got {message:?}"
        );
    }
    // Other parameters are ignored.
    let page = log_page(t.vm(), "?sort=desc&page=2&limit=1");
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    // A raw `+` is read as a space: a time written with one is not taken.
    let raw_plus = operation_log(t.vm(), &administrator(), "?since=2026-11-21T10:00:00+01:00");
    assert_api_error(&raw_plus, 400, "malformed");
    for method in ["POST", "PUT", "DELETE"] {
        let answer = t
            .vm()
            .api_as(&administrator())
            .request(OPERATION_LOG)
            .method(method)
            .json(&json!({}))
            .send();
        assert_eq!(answer.status, 405, "{method}\n{}", answer.describe());
    }
}

#[test]
fn an_answer_stops_before_4_mib_with_at_least_one_entry_and_earlier_goes_on() {
    let s = Scenario::ready();
    let rita = User::new("rita").in_groups(&[READERS]);
    // Long statements (their constants become placeholders, their column
    // names stay), five to a request, until the entries hold well over
    // 4 MiB as JSON. How long an entry is depends on how the statement is
    // kept, so it is measured on the first ones.
    let statement = |n: usize| {
        let columns: Vec<String> = (0..1500)
            .map(|c| format!("1 as c{n:03}_{c:04}_{}", "x".repeat(50)))
            .collect();
        format!("select {}", columns.join(", "))
    };
    let mut made = 0;
    let send_five = |made: &mut usize| {
        let calls: Vec<Value> = (0..5)
            .map(|i| json!({ "resource": RESOURCE, "sql": statement(*made + i) }))
            .collect();
        s.run(&rita, "board", &json!({ "calls": calls, "summary": true }));
        *made += 5;
    };
    send_five(&mut made);
    // Read as before this ticket (`?limit` alone), to measure.
    let first = s.admin().operation_log(Some("5")).json();
    let entry_bytes = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.to_string().len())
        .min()
        .unwrap_or(0);
    assert!(
        entry_bytes >= 4_000,
        "the harness's own check: expected each entry to hold a long statement, the shortest is {entry_bytes} bytes as JSON"
    );
    let wanted = (5 * 1024 * 1024 / entry_bytes + 1).min(500);
    while made < wanted {
        send_five(&mut made);
    }
    let page = s
        .admin()
        .request("/v1/operation-log?limit=1000")
        .timeout(std::time::Duration::from_secs(30))
        .try_send()
        .unwrap_or_else(|e| {
            panic!(
                "expected an answer whose body stops before 4 MiB ({made} entries of about {entry_bytes} bytes match), got none the harness can read: {e}"
            )
        });
    assert_eq!(page.status, 200, "status {}", page.status);
    let body_bytes = page.body().len();
    assert!(
        body_bytes <= 4 * 1024 * 1024,
        "expected the answer to stop before its body passes 4 MiB, it is {body_bytes} bytes"
    );
    let body = page.json();
    let entries = body["entries"].as_array().unwrap();
    assert!(
        !entries.is_empty() && entries.len() < made,
        "expected fewer than {made} entries (about {entry_bytes} bytes each) and at least one, got {} in {body_bytes} bytes",
        entries.len()
    );
    assert_eq!(
        body["earlier"], entries[0]["id"],
        "`earlier` goes on from the first answered"
    );
    let (all, pages) = every_entry(&s.vm, "", 1000);
    assert_eq!(
        all.len(),
        made,
        "every entry once through `before` (pages {pages:?})"
    );
    let mut seen = ids(&all);
    seen.dedup();
    assert_eq!(seen.len(), made);
    // One entry is always answered, however large.
    let one = log_page(&s.vm, "?limit=1");
    assert_eq!(one["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn entries_of_a_disabled_application_are_read_as_any_other() {
    let (t, all) = populated();
    blackbox::admin::disable_ok(t.vm(), APP);
    assert_filtered(
        &t,
        &format!("application={APP}"),
        &matching(&all, |e| e["application"] == APP),
    );
}
