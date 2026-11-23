//! The Operation log of outbound calls (CONTRACT.md, "Outbound calls: The
//! Operation log"): every call and every redirect followed has one entry,
//! written before a name is resolved or a connection made and completed when
//! the answer's headers have come or the call has failed; `host` is the host
//! called (and its port when not 443) and null for every other operation;
//! nothing else of the call is recorded anywhere (not the path, the query, a
//! header, the body, the answer's status or body), neither in the entries nor
//! in the program's log; `GET /v1/operation-log` takes `host=`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::admin::{administrator, every_entry, operation_log};
use blackbox::api::assert_recent_rfc3339;
use blackbox::outbound::{
    APP, Origin, Outbound, Reply, Wire, assert_fetch_entry, assert_refused, assert_resolved,
    fetch_records,
};
use blackbox::scenario::{READERS, RESOURCE, Scenario};
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

const SOON: Duration = Duration::from_secs(10);

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

#[test]
fn an_allowed_a_refused_and_a_failed_call_each_have_an_entry_with_every_field_the_contract_gives() {
    let (o, origin) = listed();
    // An answer whose headers come after 400 ms and whose body takes much
    // longer: `duration_ms` is the time to the headers.
    origin.always(
        Reply::new(404)
            .after(Duration::from_millis(400))
            .stalls_after(100, b"x", Duration::from_secs(4)),
    );
    let response = o.call_response(&json!({ "url": origin.url("/slow-body"), "read": "none" }));
    assert_resolved(&o.report(&response), 404, "an answer of 404");
    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "allowed",
        Some(&origin.host()),
        None,
        None,
        "an allowed call",
    );
    assert_eq!(entry["user"], "bob", "{entry}");
    assert_eq!(entry["application"], APP, "{entry}");
    assert_eq!(entry["version"], 1, "{entry}");
    assert!(entry["id"].is_u64(), "{entry}");
    assert_eq!(entry["transaction"], Value::Null, "{entry}");
    assert_eq!(
        entry["request"].as_str(),
        Some(response.request_id().as_str()),
        "{entry}"
    );
    assert_recent_rfc3339(&entry["time"], 120, "the entry's time");
    let ms = entry["duration_ms"].as_u64().unwrap_or_else(|| {
        panic!("expected `duration_ms` a number for an allowed call, got {entry}")
    });
    assert!(
        (300..3000).contains(&ms),
        "expected `duration_ms` the time to the answer's headers (about 400 ms, not the body's 4 s), got {ms} in {entry}"
    );

    // Refused.
    let unlisted = o.origin();
    let response = o.call_response(&json!({ "url": unlisted.url("/") }));
    assert_refused(&o.report(&response), "host-not-listed", None, "unlisted");
    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "refused",
        Some(&unlisted.host()),
        Some("host-not-listed"),
        None,
        "a refused call",
    );
    assert_eq!(entry["user"], "bob", "{entry}");
    assert_eq!(entry["application"], APP, "{entry}");
    assert!(
        entry["duration_ms"].is_null(),
        "expected `duration_ms` null for a call that was never made, got {entry}"
    );

    // Failed.
    let port = blackbox::ports::chosen().unwrap();
    o.allow(&format!("localhost:{port}"));
    let response = o.call_response(&json!({ "url": format!("https://localhost:{port}/") }));
    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "failed",
        Some(&format!("localhost:{port}")),
        None,
        Some("unreachable"),
        "a failed call",
    );
}

#[test]
fn the_entry_is_written_before_the_call_is_made_and_is_unfinished_until_the_headers_come() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"late").after(Duration::from_secs(4)));
    let silent = Origin::start(Wire::Silent);
    o.allow(&silent.host());
    thread::scope(|scope| {
        let held = scope.spawn(|| o.call(&json!({ "url": origin.url("/late") })));
        let connecting = scope.spawn(|| o.call(&json!({ "url": silent.url("/never") })));
        origin.wait_requests(1, SOON);
        // The origin has the request and has not answered: the entry is there,
        // unfinished. The silent one never completed its handshake: its entry
        // was written before any connection was made.
        thread::sleep(Duration::from_millis(500));
        let unfinished = every_entry(&o.vm, "application=outbound&outcome=unfinished", 100).0;
        let hosts: Vec<&str> = unfinished
            .iter()
            .filter_map(|e| e["host"].as_str())
            .collect();
        assert!(
            hosts.contains(&origin.host().as_str()),
            "expected an unfinished entry for the call waiting for headers, got {unfinished:?}"
        );
        assert!(
            hosts.contains(&silent.host().as_str()),
            "expected an unfinished entry for the call still connecting, got {unfinished:?}"
        );
        let report = held.join().unwrap();
        assert_resolved(&report, 200, "the late answer");
        drop(connecting.join());
    });
    let entries = o.entries();
    let late = entries
        .iter()
        .find(|e| e["host"] == json!(origin.host()))
        .unwrap();
    assert_eq!(late["outcome"], "allowed", "{late}");
    assert!(
        late["duration_ms"].as_u64().unwrap_or(0) >= 3500,
        "expected `duration_ms` the time to the headers (4 s), got {late}"
    );
}

#[test]
fn nothing_of_the_path_the_query_a_header_the_body_or_the_answer_is_recorded_anywhere() {
    let (o, origin) = listed();
    let other = o.origin();
    o.allow(&other.host());
    let landing = other.url("/redirect-landing-marker-LLL?q=landing-query-marker-MMM");
    origin.script(move |seen| {
        if seen.path().contains("redirect") {
            Reply::redirect(307, &landing)
        } else {
            Reply::new(418)
                .header("x-answer", "answer-header-marker-XXX")
                .body(b"answer-body-marker-AAA")
        }
    });
    let markers = [
        "path-marker-PPP",
        "query-marker-QQQ",
        "header-marker-HHH",
        "credential-marker-CCC",
        "body-marker-BBB",
        "answer-body-marker-AAA",
        "answer-header-marker-XXX",
        "redirect-landing-marker-LLL",
        "landing-query-marker-MMM",
        "fragment-marker-FFF",
    ];
    // Allowed, with a 418 answer.
    let report = o.call(&json!({
        "url": origin.url("/path-marker-PPP/x?a=query-marker-QQQ#fragment-marker-FFF"),
        "method": "POST",
        "headers": { "x-h": "header-marker-HHH", "authorization": "Bearer credential-marker-CCC" },
        "body": { "kind": "string", "text": "body-marker-BBB" },
    }));
    assert_resolved(&report, 418, "an answer of 418");
    // A redirect that is followed to another host (listed), with its own path.
    let report = o.call(&json!({
        "url": origin.url("/redirect-start?a=query-marker-QQQ"),
        "headers": { "x-h": "header-marker-HHH" },
    }));
    assert_resolved(&report, 418, "a redirect followed to an answer of 418");
    // Refused (not listed), with the same kind of markers.
    let report = o.call(&json!({
        "url": "https://unlisted.example.invalid/path-marker-PPP?a=query-marker-QQQ",
        "method": "POST",
        "headers": { "x-h": "header-marker-HHH" },
        "body": { "kind": "string", "text": "body-marker-BBB" },
    }));
    assert_refused(&report, "host-not-listed", None, "unlisted, with markers");
    // Failed (nobody listens), with markers.
    let port = blackbox::ports::chosen().unwrap();
    o.allow(&format!("localhost:{port}"));
    let report = o.call(&json!({
        "url": format!("https://localhost:{port}/path-marker-PPP?a=query-marker-QQQ"),
        "headers": { "x-h": "header-marker-HHH" },
    }));
    assert_eq!(
        report["rejected"]["failure"]["kind"], "unreachable",
        "{report}"
    );

    // What every surface says: the entries as the API gives them (also a
    // page at a time), and the program's whole log.
    let mut text = String::new();
    for entry in every_entry(&o.vm, "", 3).0 {
        text.push_str(&entry.to_string());
        text.push('\n');
    }
    let whole = operation_log(&o.vm, &administrator(), "?limit=1000");
    text.push_str(&String::from_utf8_lossy(whole.body()));
    for line in o.vm.lines() {
        text.push_str(&format!("{line:?}\n"));
    }
    for record in o.vm.records() {
        text.push_str(&record.to_string());
        text.push('\n');
    }
    for marker in markers {
        assert!(
            !text.contains(marker),
            "expected {marker:?} nowhere in the Operation log or the program's log"
        );
    }
    // And the log has what it should: the host, in the program's record.
    let records = fetch_records(&o.vm);
    assert!(
        records
            .iter()
            .any(|r| r["host"] == json!(origin.host()) && r["outcome"] == "allowed"),
        "expected a `host-call` record of `fetch` with the `host` called, got {records:?}"
    );
}

#[test]
fn the_entry_of_a_query_has_host_null_and_the_filter_takes_a_fetchs_host_exactly() {
    // A query entry on a program of its own: `host` is a member, null.
    let s = Scenario::ready();
    let rita = User::new("rita").in_groups(&[READERS]);
    let (_, answer) = s.query(
        &rita,
        "board",
        &json!({ "resource": RESOURCE, "sql": "select 1 as one" }),
    );
    assert!(answer["ok"].is_object(), "{answer}");
    let entries = s.log();
    let query = entries.iter().find(|e| e["operation"] == "query").unwrap();
    assert!(
        query.as_object().unwrap().contains_key("host"),
        "expected every entry to have a `host` member, null for a query, got {query}"
    );
    assert_eq!(query["host"], Value::Null, "{query}");

    // The filter, on the outbound program.
    let (o, a) = listed();
    let b = o.origin();
    o.allow(&b.host());
    let unlisted = o.origin();
    for target in [a.url("/1"), a.url("/2"), b.url("/3"), unlisted.url("/4")] {
        o.call(&json!({ "url": target }));
    }
    let hosts_of = |query: &str| -> Vec<(String, String)> {
        let answer = operation_log(&o.vm, &administrator(), query);
        assert_eq!(answer.status, 200, "{query}: {}", answer.describe());
        answer.json()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["host"].as_str().unwrap_or("<null>").to_owned(),
                    e["outcome"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    };
    let only_a = hosts_of(&format!("?host={}", encode(&a.host())));
    assert_eq!(
        only_a,
        vec![
            (a.host(), "allowed".to_owned()),
            (a.host(), "allowed".to_owned())
        ],
        "expected exactly the entries whose host is this one"
    );
    let refused_unlisted = hosts_of(&format!(
        "?host={}&outcome=refused&application={APP}",
        encode(&unlisted.host())
    ));
    assert_eq!(
        refused_unlisted,
        vec![(unlisted.host(), "refused".to_owned())],
        "expected the filters combined"
    );
    assert!(
        hosts_of("?host=localhost").is_empty(),
        "expected an entry matched by the whole host and port, not a part of it"
    );
    assert!(
        hosts_of("?host=nobody.example").is_empty(),
        "expected a host nobody called to match nothing"
    );
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

#[test]
fn the_host_filter_takes_1_to_260_bytes_once_and_a_malformed_one_is_400_naming_it() {
    let o = Outbound::start_closed();
    let ok = operation_log(
        &o.vm,
        &administrator(),
        &format!("?host={}", "a".repeat(260)),
    );
    assert_eq!(ok.status, 200, "260 bytes: {}", ok.describe());
    assert!(ok.json()["entries"].as_array().unwrap().is_empty());
    for (what, query) in [
        ("empty", "?host=".to_owned()),
        ("261 bytes", format!("?host={}", "a".repeat(261))),
        ("twice", "?host=a.example&host=b.example".to_owned()),
    ] {
        let answer = operation_log(&o.vm, &administrator(), &query);
        assert_api_error(&answer, 400, "malformed");
        assert!(
            answer.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("host"),
            "{what}: expected the message to name the parameter `host`, got {}",
            answer.describe()
        );
    }
}
