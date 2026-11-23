//! What the review of "Outbound calls" found (CONTRACT.md, "Outbound calls":
//! "Who may call what", "The list", "Bounds", "Time"): a flood of calls the
//! code does not await, a redirect to something that is not an address, the
//! more address kinds never called, a trailing dot, a client or a code that
//! does not take a body, an upload that stalls, headers past what the glue
//! hands the host, an administrator's request given up, and the Operation
//! log failing in the middle of a redirect chain.
//!
//! Not written here: a call made with arguments made by hand (a stream id
//! that is not the code's, a made-up redirect mode, a `host` header and a
//! `content-length` set by hand). The contract does not name the binding the
//! platform's `fetch` is built on and no fixture reaches it, so nothing in
//! the contract or the harness says how to call it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{self, administrator};
use blackbox::body::{BodyEnd, Framing, MIB, Raw, memory_of};
use blackbox::outbound::{
    APP, Cert, OPEN_LOOPBACK, Origin, Outbound, Reply, assert_failure, assert_fetch_entry,
    assert_refused, assert_resolved, bob, host_call_records, refusal_of,
};
use blackbox::outbound_surface::{DbProxy, hosts, path_of, request};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::storage::sampled;
use blackbox::tls::{Authority, Files, text_of};
use blackbox::vm::DOMAIN;
use serde_json::{Value, json};

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

fn url(target: &str) -> Value {
    json!({ "url": target })
}

/// The program's memory may rise, while a request makes thousands of calls
/// it does not await, by what the Application's own heap may hold (64 MiB
/// for a request in flight, "A host call's arguments are bounded") and a
/// margin for the program's own buffers: 96 MiB. A program that keeps what
/// each of 5,000 calls was handed (32 KiB of headers: 160 MiB) goes over.
const FLOOD_RISE: u64 = 96 * MIB;

const FLOOD: usize = 5000;

/// `/flood` as bob; the response and how far the memory rose.
fn flood(
    o: &Outbound,
    baseline: &str,
    target: &str,
    settle: Option<u64>,
) -> (blackbox::http::Response, u64) {
    // A first small flood, to a place of its own (an origin that answers),
    // so that what the program allocates the first time is in the baseline
    // and nothing of it reaches the origin under test.
    o.vm.request(APP, &format!("/flood?url={baseline}&n=3&pad=1024"))
        .as_user(&bob())
        .timeout(Duration::from_secs(60))
        .send();
    let pid = o.vm.pid();
    let before = memory_of(pid, "VmRSS");
    let settle = settle.map_or(String::new(), |ms| format!("&settle={ms}"));
    let (response, peak) = sampled(pid, || {
        o.vm.request(
            APP,
            &format!("/flood?url={target}&n={FLOOD}&pad=32768{settle}"),
        )
        .as_user(&bob())
        .timeout(Duration::from_secs(120))
        .send()
    });
    let rise = peak.saturating_sub(before);
    eprintln!("MEASURED flood of {FLOOD} calls: the program's memory rose by {rise} bytes");
    (response, rise)
}

fn count(map: &Value, key: &str) -> usize {
    usize::try_from(map[key].as_u64().unwrap_or(0)).unwrap()
}

// --- A flood of calls that are not awaited --------------------------------------

#[test]
fn a_flood_of_calls_to_a_host_not_listed_is_refused_72_at_most_and_unavailable_for_the_rest() {
    let o = Outbound::start();
    let (response, rise) = flood(
        &o,
        "https://unlisted.example.invalid/base",
        "https://unlisted.example.invalid/flood",
        None,
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    let counts = response.json();
    let refused = count(&counts["refused"], "host-not-listed");
    let unavailable = count(&counts["failed"], "unavailable");
    // A refused call stops counting once it is answered, so a few more than
    // 72 may be refused while the loop is still making calls: what is
    // promised is that the bound bites (some are `unavailable`, with no
    // entry) and that every refused call has its entry.
    assert!(
        refused >= 72 && unavailable >= 1,
        "expected the first 72 calls refused `host-not-listed` and the bound to bite on the rest, got {refused} refused and {unavailable} unavailable: {counts}"
    );
    assert_eq!(
        refused + unavailable,
        FLOOD,
        "expected every other call `unavailable`, got {counts}"
    );
    // Each of the 72 has its entry; the others have none.
    let entries = o.entries_of(&response);
    assert_eq!(
        entries.len(),
        refused,
        "expected an Operation log entry for each call decided and none for the `unavailable` ones, got {} entries for {refused} refused and {unavailable} unavailable",
        entries.len()
    );
    for entry in &entries {
        assert_fetch_entry(
            entry,
            "refused",
            Some("unlisted.example.invalid"),
            Some("host-not-listed"),
            None,
            "a call of the flood",
        );
    }
    // The program's log has a `host-call` record `refused` with reason
    // `limit` for each of the rest.
    let limited = host_call_records(&o.vm, &response)
        .into_iter()
        .filter(|r| r["outcome"] == "refused" && r["reason"] == "limit")
        .count();
    assert_eq!(
        limited, unavailable,
        "expected a `host-call` record `refused` with reason `limit` for each call past the bound"
    );
    assert!(
        rise < FLOOD_RISE,
        "expected the program's memory to rise by less than {} MiB while {FLOOD} calls of 32 KiB of headers were made, it rose by {} MiB",
        FLOOD_RISE / MIB,
        rise / MIB
    );
    // The program still serves.
    let after = o.vm.request(APP, "/whoami").as_user(&bob()).send();
    after.expect_status(200);
}

#[test]
fn a_flood_of_calls_to_a_listed_host_that_never_answers_is_72_not_yet_answered_and_the_73rd_has_no_entry()
 {
    let (o, origin) = listed();
    origin.always(Reply::never(Duration::from_secs(120)));
    // The request returns after 3 seconds, the 72 calls still under way
    // (they end with it).
    let quick = o.origin();
    o.allow(&quick.host());
    let (response, rise) = flood(&o, &quick.url("/base"), &origin.url("/flood"), Some(3000));
    assert_eq!(response.status, 200, "{}", response.describe());
    let counts = response.json();
    assert_eq!(
        counts["pending"], 72,
        "expected exactly 72 calls not yet answered (8 under way and 64 waiting), got {counts}"
    );
    assert_eq!(
        count(&counts["failed"], "unavailable"),
        FLOOD - 72,
        "expected every other call `unavailable`, got {counts}"
    );
    let entries = o.entries_of(&response);
    eprintln!("MEASURED entries of the listed flood: {}", entries.len());
    assert!(
        entries.len() == 72,
        "expected the Operation log to grow by exactly 72 for the request (a call that waits has its entry already; none for the 73rd and the rest), got {}",
        entries.len()
    );
    assert!(
        entries.iter().all(|e| e["error"] != "unavailable"),
        "expected no entry `unavailable` for a call past the 72"
    );
    assert_eq!(origin.requests().len(), 8, "8 calls reach the origin");
    let limited = host_call_records(&o.vm, &response)
        .into_iter()
        .filter(|r| r["outcome"] == "refused" && r["reason"] == "limit")
        .count();
    assert_eq!(limited, FLOOD - 72, "{limited} records with reason `limit`");
    assert!(
        rise < FLOOD_RISE,
        "expected the program's memory to rise by less than {} MiB, it rose by {} MiB",
        FLOOD_RISE / MIB,
        rise / MIB
    );
    // The calls ended with their request: the origin sees them go.
    assert!(
        origin
            .wait_peer_closed(Instant::now(), Duration::from_secs(10))
            .is_some(),
        "expected the calls' connections closed when their request ended"
    );
    let after = o.vm.request(APP, "/whoami").as_user(&bob()).send();
    after.expect_status(200);
}

// --- A redirect to what is not an address --------------------------------------

#[test]
fn a_redirect_whose_location_is_not_an_address_is_refused_not_https_with_no_entry_of_its_own() {
    let (o, a) = listed();
    for location in ["https://", "https://[::1"] {
        a.always(Reply::redirect(302, location));
        let response = o.call_response(&url(&a.url("/start")));
        let report = o.report(&response);
        assert_refused(&report, "not-https", None, location);
        let refused = refusal_of(&report);
        assert_eq!(
            refused["refusal"]["redirected_from"],
            json!(a.host()),
            "{location}: {report}"
        );
        assert_eq!(
            refused["refusal"]["host"],
            Value::Null,
            "{location}: expected a null `host`, got {report}"
        );
        let entries = o.entries_of(&response);
        assert_eq!(
            entries.len(),
            1,
            "{location}: expected the first call's entry and none for the hop, got {entries:?}"
        );
        assert_eq!(entries[0]["host"], json!(a.host()), "{}", entries[0]);
    }
}

// --- More address kinds never called ----------------------------------------------

#[test]
fn the_other_forms_that_carry_or_reach_an_ipv4_address_are_never_called_though_listed() {
    let o = Outbound::start_closed();
    let never = [
        ("[::ffff:0:7f00:1]", "https://[::ffff:0:127.0.0.1]/"),
        ("[64:ff9b:1::7f00:1]", "https://[64:ff9b:1::7f00:1]/"),
        ("[2001::1]", "https://[2001::1]/"),
        ("[fec0::1]", "https://[fec0::1]/"),
        ("192.88.99.1", "https://192.88.99.1/"),
    ];
    let mut wrong = Vec::new();
    for (entry, target) in never {
        o.allow(entry);
        let report = o.call(&url(target));
        let reason = refusal_of(&report)["refusal"]["reason"].as_str();
        if reason != Some("address-not-allowed") {
            wrong.push(format!("{target}: {report}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected every one of these listed addresses refused `address-not-allowed`, but:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn a_redirect_to_an_address_that_carries_a_loopback_one_not_opened_is_address_not_allowed() {
    // The program opens 127.0.0.1 and ::1 only. What `--outbound-private`
    // does to an address that carries 127.0.0.1 (opened, or not, as "judged
    // by the address it carries" and "opens them as it opens the others"
    // may be read either way) is not pinned: the hops here carry 127.0.0.2,
    // which no range opens.
    let o = Outbound::start();
    let a = o.origin();
    o.allow(&a.host());
    for (entry, location) in [
        ("[::ffff:7f00:2]:9443", "https://[::ffff:127.0.0.2]:9443/"),
        ("[64:ff9b::7f00:2]", "https://[64:ff9b::7f00:2]/"),
    ] {
        o.allow(entry);
        a.always(Reply::redirect(302, location));
        let report = o.call(&url(&a.url("/start")));
        assert_refused(&report, "address-not-allowed", None, location);
        assert_eq!(
            refusal_of(&report)["refusal"]["redirected_from"],
            json!(a.host()),
            "{location}: {report}"
        );
    }
}

// --- A trailing dot and a long name ---------------------------------------------------

#[test]
fn a_name_with_a_trailing_dot_is_not_the_listed_name_and_the_installations_own_names_still_are_itself()
 {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    let dotted = format!("https://localhost.:{}/x", origin.port());
    let report = o.call(&url(&dotted));
    assert_refused(&report, "host-not-listed", None, &dotted);
    assert!(
        origin.requests().is_empty(),
        "expected nothing to reach the origin through the name with a dot"
    );
    // The installation's own names, with the dot too.
    for target in [
        format!("https://{DOMAIN}./"),
        format!("https://api.{DOMAIN}./v1/me"),
        format!("https://{APP}.{DOMAIN}.:8443/"),
    ] {
        let report = o.call(&url(&target));
        assert_refused(&report, "platform-itself", None, &target);
    }
}

#[test]
fn an_address_whose_host_is_over_253_bytes_is_not_an_address_not_https_with_a_null_host_and_no_entry()
 {
    let o = Outbound::start();
    let label = "a".repeat(63);
    let host = format!("{label}.{label}.{label}.{label}");
    assert_eq!(host.len(), 255);
    let response = o.call_response(&url(&format!("https://{host}/x")));
    let report = o.report(&response);
    assert_refused(&report, "not-https", None, "a host of 255 bytes");
    assert_eq!(
        refusal_of(&report)["refusal"]["host"],
        Value::Null,
        "{report}"
    );
    assert!(
        o.entries_of(&response).is_empty(),
        "expected no entry for an address that is not one"
    );
}

// --- A body nobody takes ------------------------------------------------------------------

#[test]
fn an_answer_passed_on_to_a_client_that_reads_nothing_is_failed_after_30_seconds_and_the_origin_is_let_go()
 {
    // About 30 seconds.
    let (o, origin) = listed();
    origin.always(Reply::new(200).pattern(1024 * MIB));
    let mut raw = Raw::connect(o.vm.address());
    let began = Instant::now();
    raw.send_head(
        "GET",
        &format!("/pass?url={}", origin.url("/endless")),
        &o.vm.host_of(APP),
        Some(&bob()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200);
    // From here the client reads nothing.
    let closed = origin.wait_peer_closed(began, Duration::from_secs(45));
    assert!(
        closed.is_some_and(|after| after < Duration::from_secs(45)),
        "expected the origin to see its connection closed within about 30 seconds of a client that takes nothing, got {closed:?}"
    );
    let end = raw.drain_body(Duration::from_secs(30), |_| ());
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the client's connection ended without a clean end"
    );
}

#[test]
fn an_answers_body_the_code_holds_unread_is_failed_one_second_before_its_requests_time_ends() {
    // About 30 seconds: the code reads nothing for 29.3 seconds.
    let (o, origin) = listed();
    origin.always(Reply::new(200).pattern(200 * MIB));
    let began = Instant::now();
    let response =
        o.vm.request(
            APP,
            &format!("/hold-then-read?url={}&ms=29300", origin.url("/big")),
        )
        .as_user(&bob())
        .timeout(Duration::from_secs(90))
        .send();
    assert_eq!(
        response.status,
        200,
        "expected the request to answer `200` still (took {:?}): {}",
        began.elapsed(),
        response.describe()
    );
    let seen = response.json();
    assert!(
        seen["readFailed"].is_string(),
        "expected the read of a body the code held past one second before its request's time to fail, got {seen}"
    );
    assert!(
        seen["length"].as_u64().unwrap() < 200 * MIB,
        "expected the body never to come whole, got {seen}"
    );
}

#[test]
fn an_upload_that_stalls_ends_the_call_before_the_requests_time_and_the_origin_never_sees_it_whole()
{
    // About 30 seconds. The call's own body gives 1 KiB and then nothing;
    // "a call's own body that stalls fails the call `body-failed`" after 30
    // seconds with no byte, which is after the request's own 30 seconds
    // (the bytes moved at its start). What the code can see in one request is
    // the 30 seconds to the answer's headers, "or until one second before
    // the request's own time ends when that is sooner": `limit` `time`.
    // Either is accepted; what is promised is that the call ends, the
    // origin never has a whole body, and the request is answered.
    let (o, origin) = listed();
    let began = Instant::now();
    let response = o.call_response(&json!({
        "url": origin.url("/up"), "method": "POST",
        "body": { "kind": "stalling" },
    }));
    let took = began.elapsed();
    assert_eq!(
        response.status,
        200,
        "expected the code to be told while it can still answer (took {took:?}): {}",
        response.describe()
    );
    let report = response.json();
    let failure = &report["rejected"]["failure"];
    let kind = failure["kind"].as_str().unwrap_or("");
    assert!(
        kind == "body-failed" || (kind == "limit" && failure["limit"] == "time"),
        "expected `body-failed` or `limit` `time`, got {report}"
    );
    assert!(
        took < Duration::from_secs(31),
        "expected the call to end before its request's 30 seconds, it took {took:?}"
    );
    assert!(
        origin
            .wait_peer_closed(began, Duration::from_secs(10))
            .is_some(),
        "expected the origin to see the connection end"
    );
    for seen in origin.requests() {
        assert!(
            !seen.body_complete,
            "expected no whole body at the origin: {}",
            seen.describe()
        );
    }
}

// --- Headers past what is handed to the host ------------------------------------------------

#[test]
fn headers_of_more_than_8_mib_fail_limit_arguments_before_anything_is_looked_at() {
    // 90 headers of 100 KiB are past `malformed` (64 KiB, 100 headers) and
    // past the 8 MiB the glue hands the host; the contract says the second
    // fails "before anything of it is looked at".
    let (o, origin) = listed();
    let response = o.call_response(&json!({
        "url": origin.url("/x"), "gen": { "count": 90, "size": 100 * 1024 },
    }));
    let report = o.report(&response);
    assert_failure(&report, "limit", "headers of 9 MiB");
    assert_eq!(
        report["rejected"]["failure"]["limit"], "arguments",
        "expected `limit` `arguments`, got {report}"
    );
    assert!(
        o.entries_of(&response).is_empty(),
        "expected no entry: the call was failed before anything of it was looked at"
    );
    assert!(origin.requests().is_empty());
}

// --- An administrator's request given up -------------------------------------------------------

#[test]
fn a_removal_whose_request_is_given_up_leaves_the_entry_answered_again_once_the_connection_fails() {
    const LATE: &str = "given-up.example.com";
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let target = format!("{}:{}", postgres.host(), postgres.port())
        .parse()
        .expect("the Postgres's address");
    let proxy = DbProxy::start(target);
    let name = "vm_platform_outbound_given_up";
    let platform = Platform::on_address(&postgres, name, proxy.address());
    let vm = blackbox::Vm::start(&admin::options().platform(&platform));
    blackbox::outbound_surface::add(&vm, LATE);
    blackbox::outbound_surface::add(&vm, "kept.example.com");

    proxy.arm(LATE);
    // The client gives up after a second, the removal's bytes held.
    let given_up = request(&vm, Some(&administrator()), "DELETE", &path_of(LATE))
        .timeout(Duration::from_secs(1))
        .try_send();
    assert!(
        given_up.is_err(),
        "the harness's own check: expected the DELETE to get no answer in a second"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while proxy.held() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        proxy.held(),
        1,
        "the harness's own check: expected the removal's connection held"
    );
    // The connection fails without the removal being delivered.
    proxy.fail();
    let failed_at = Instant::now();
    let deadline = failed_at + Duration::from_secs(15);
    let mut listed = hosts(&vm);
    while !listed.iter().any(|h| h == LATE) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        listed = hosts(&vm);
    }
    let table: Vec<String> = blackbox::sql::Sql::connect(&postgres.url(name))
        .expect("the harness's own connection")
        .batch("SELECT host FROM outbound_hosts ORDER BY host")
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    eprintln!(
        "MEASURED given up: after {:?} the list answers {listed:?}, the table holds {table:?}",
        failed_at.elapsed()
    );
    assert!(
        table.iter().any(|h| h == LATE),
        "the harness's own check: the removal was never delivered, so the table holds the entry"
    );
    assert!(
        listed.iter().any(|h| h == LATE) && failed_at.elapsed() < Duration::from_secs(8),
        "expected the program to hold what the table holds (the entry) within 5 seconds of the database answering, the list answers {listed:?} after {:?}",
        failed_at.elapsed()
    );
}

// --- The Operation log in the middle of a redirect chain -----------------------------------------

#[test]
fn a_redirect_whose_entry_cannot_be_written_is_refused_log_unavailable_with_redirected_from() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_outbound_log_unavailable_hop");
    let authority = Authority::new("outbound test authority");
    let files = Files::new();
    let ca_file = files.authorities("ca.pem", &[&authority]);
    let options = admin::options()
        .platform(&platform)
        .args_extra(&OPEN_LOOPBACK)
        .env("VIBE_CA_FILE", &text_of(&ca_file));
    let o = Outbound::started(&options, authority, files, ca_file, &[]);
    let a = Origin::start(Cert::localhost(&o.authority).wire());
    let b = Origin::start(Cert::localhost(&o.authority).wire());
    o.allow(&a.host());
    o.allow(&b.host());
    // The first answer is held for 3 seconds; the platform database goes
    // while it is.
    a.always(Reply::redirect(302, &b.url("/second")).after(Duration::from_secs(3)));
    let report = thread::scope(|scope| {
        let call = scope.spawn(|| o.call(&url(&a.url("/first"))));
        a.wait_requests(1, Duration::from_secs(10));
        postgres.stop();
        call.join().unwrap()
    });
    assert_refused(
        &report,
        "log-unavailable",
        None,
        "the hop of a redirect whose entry cannot be written",
    );
    assert_eq!(
        refusal_of(&report)["refusal"]["redirected_from"],
        json!(a.host()),
        "expected `redirected_from` on `log-unavailable` too, got {report}"
    );
    assert!(
        b.requests().is_empty(),
        "expected nothing to leave for the hop"
    );
}

// --- Arguments of its own making, to the way from `fetch` to the host ----------------------
//
// "The way from the platform's `fetch` to the host is not an interface":
// code that reaches it (`globalThis.__nativeBindingInvoke("__vm_fetch",
// "fetch", args, resolve, reject)`) with arguments of its own making gets
// nothing a `fetch` would not have given it. What the arguments are was
// found from outside, by calling it with what is not right and reading the
// messages: `url` (a string), `method`, `headers` (a list of `[name,
// value]`, strings), `redirect` (`follow`, `manual` or `error`), `body` (a
// string, or what the glue makes of the other kinds, which this suite cannot
// make: a `{"$stream": <id>}` is the form an answer's body has, and a
// made-up one is what these tests give).

/// The answers of hand-made calls, in order (`/native-seq`).
fn by_hand(o: &Outbound, calls: &Value) -> Vec<Value> {
    let response =
        o.vm.request(APP, "/native-seq")
            .as_user(&bob())
            .method("POST")
            .json(&json!({ "calls": calls }))
            .timeout(Duration::from_secs(60))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    response.json()["outs"].as_array().unwrap().clone()
}

fn error_kind(out: &Value) -> Option<&str> {
    out["resolved"]["error"]["kind"].as_str()
}

fn still_serves(o: &Outbound) {
    let after = o.vm.request(APP, "/whoami").as_user(&bob()).send();
    after.expect_status(200);
}

#[test]
fn the_arguments_a_fetch_would_make_replayed_by_hand_give_the_same_answer_as_a_fetch() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"same"));
    let report = o.call(&url(&origin.url("/a")));
    let outs = by_hand(&o, &json!([{ "url": origin.url("/a") }]));
    assert_eq!(report["resolved"]["status"], 200, "{report}");
    assert_eq!(outs[0]["resolved"]["ok"]["status"], 200, "{}", outs[0]);
    let seen = origin.wait_requests(2, Duration::from_secs(10));
    assert_eq!(seen[0].method, seen[1].method);
    assert_eq!(seen[0].names(), seen[1].names(), "the same headers sent");
    still_serves(&o);
}

#[test]
fn a_body_that_names_a_stream_that_is_not_the_codes_own_is_malformed_and_nothing_is_sent() {
    let (o, origin) = listed();
    let u = origin.url("/n");
    let outs = by_hand(
        &o,
        &json!([
            { "url": u },
            { "url": u, "method": "POST", "body": { "$stream": 1 } },
            { "url": u, "method": "POST", "body": { "$stream": 99999 } },
            { "url": u, "method": "POST", "body": { "$stream": -1 } },
            { "url": u, "method": "POST", "body": { "$stream": 1.5 } },
            { "url": u, "method": "POST", "body": { "$stream": "2" } },
            // The id of the first answer's body: a stream, but not a body
            // the code made for a call.
            { "url": u, "method": "POST", "body": { "$stream": 2 } },
            { "url": u, "method": "POST", "body": { "$stream": 2, "extra": 1 } },
        ]),
    );
    assert_eq!(outs[0]["resolved"]["ok"]["status"], 200, "{}", outs[0]);
    for (i, out) in outs.iter().enumerate().skip(1) {
        assert_eq!(
            error_kind(out),
            Some("malformed"),
            "call {i}: expected `malformed`, got {out}"
        );
    }
    let seen = origin.requests();
    assert_eq!(
        seen.len(),
        1,
        "expected only the first call to reach the origin, got {:?}",
        seen.iter().map(|s| s.describe()).collect::<Vec<_>>()
    );
    still_serves(&o);
}

#[test]
fn a_redirect_mode_that_is_no_mode_is_malformed() {
    let (o, origin) = listed();
    let u = origin.url("/n");
    let outs = by_hand(
        &o,
        &json!([
            { "url": u, "redirect": "bogus" },
            { "url": u, "redirect": "FOLLOW" },
            { "url": u, "redirect": 3 },
            { "url": u, "redirect": ["manual"] },
        ]),
    );
    for (i, out) in outs.iter().enumerate() {
        assert_eq!(
            error_kind(out),
            Some("malformed"),
            "call {i}: expected `malformed`, got {out}"
        );
    }
    assert!(origin.requests().is_empty(), "nothing reaches the origin");
    still_serves(&o);
}

#[test]
fn the_headers_the_platform_sets_itself_given_by_hand_are_dropped_and_nothing_is_smuggled() {
    let (o, origin) = listed();
    let outs = by_hand(
        &o,
        &json!([{
            "url": origin.url("/h"),
            "method": "POST",
            "body": "four",
            "headers": [
                ["host", "evil.example"],
                ["content-length", "0"],
                ["transfer-encoding", "chunked"],
                ["connection", "upgrade"],
                ["upgrade", "websocket"],
                ["x-keep", "yes\r\nGET /smuggled HTTP/1.1"],
                ["x-ok", "kept"],
            ],
        }]),
    );
    // A value that cannot be sent is `malformed`; the same without it is
    // served.
    assert_eq!(error_kind(&outs[0]), Some("malformed"), "{}", outs[0]);
    let outs = by_hand(
        &o,
        &json!([{
            "url": origin.url("/h"),
            "method": "POST",
            "body": "four",
            "headers": [
                ["host", "evil.example"],
                ["content-length", "0"],
                ["transfer-encoding", "chunked"],
                ["connection", "upgrade"],
                ["upgrade", "websocket"],
                ["x-ok", "kept"],
            ],
        }]),
    );
    assert_eq!(outs[0]["resolved"]["ok"]["status"], 200, "{}", outs[0]);
    let seen = origin.requests();
    assert_eq!(
        seen.len(),
        1,
        "expected one request, no second one smuggled"
    );
    let seen = &seen[0];
    assert_eq!(
        seen.header_all("host"),
        [origin.host()],
        "{}",
        seen.describe()
    );
    assert_eq!(seen.content_length, Some(4), "{}", seen.describe());
    assert!(!seen.chunked, "{}", seen.describe());
    assert_eq!(seen.body_len, 4, "{}", seen.describe());
    assert_eq!(seen.header("upgrade"), None, "{}", seen.describe());
    assert_ne!(seen.header("connection"), Some("upgrade"));
    assert_eq!(seen.header("x-ok"), Some("kept"));
    still_serves(&o);
}

#[test]
fn arguments_that_are_not_what_a_fetch_is_given_are_malformed_and_the_program_goes_on() {
    let (o, origin) = listed();
    let u = origin.url("/n");
    let outs = by_hand(
        &o,
        &json!([
            { "url": 5 },
            { "url": null },
            { "url": ["https://x.example/"] },
            { "url": u, "headers": { "a": "b" } },
            { "url": u, "headers": [["x-a", 1]] },
            { "url": u, "headers": [["x-a"]] },
            { "url": u, "headers": ["x-a"] },
            { "url": u, "method": 7 },
            { "url": u, "method": "TRACE" },
            { "url": u, "body": 5 },
            { "url": u, "body": { "$stream": 1, "extra": 1 } },
            null,
            "https://localhost/",
            [],
            5,
        ]),
    );
    for (i, out) in outs.iter().enumerate() {
        assert_eq!(
            error_kind(out),
            Some("malformed"),
            "call {i}: expected `malformed`, got {out}"
        );
    }
    assert!(origin.requests().is_empty(), "nothing reaches the origin");
    // A command that is not `fetch`: nothing is served by it.
    let response =
        o.vm.request(APP, "/native")
            .as_user(&bob())
            .method("POST")
            .json(&json!({ "command": "not-fetch", "args": { "url": u }, "waitMs": 3000 }))
            .timeout(Duration::from_secs(30))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json();
    assert!(
        answer["resolved"]["ok"].is_null(),
        "expected a command that is not `fetch` to get no answer of a call, got {answer}"
    );
    assert!(origin.requests().is_empty(), "nothing reaches the origin");
    still_serves(&o);
}

#[test]
fn a_host_not_listed_is_refused_by_hand_with_its_entry_as_through_fetch() {
    let o = Outbound::start();
    let response =
        o.vm.request(APP, "/native-seq")
            .as_user(&bob())
            .method("POST")
            .json(&json!({ "calls": [{ "url": "https://unlisted.example.invalid/x?q=1" }] }))
            .timeout(Duration::from_secs(60))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let out = response.json()["outs"][0].clone();
    assert!(
        out.to_string().contains("host-not-listed"),
        "expected the refusal `host-not-listed`, got {out}"
    );
    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "refused",
        Some("unlisted.example.invalid"),
        Some("host-not-listed"),
        None,
        "by hand",
    );
    still_serves(&o);
}

#[test]
fn five_thousand_calls_by_hand_that_are_not_awaited_have_the_same_bound_as_fetch() {
    let (o, origin) = listed();
    origin.always(Reply::never(Duration::from_secs(120)));
    let quick = o.origin();
    o.allow(&quick.host());
    o.vm.request(
        APP,
        &format!("/native-flood?url={}&n=3&pad=1024", quick.url("/base")),
    )
    .as_user(&bob())
    .timeout(Duration::from_secs(60))
    .send();
    let pid = o.vm.pid();
    let before = memory_of(pid, "VmRSS");
    let (response, peak) = sampled(pid, || {
        o.vm.request(
            APP,
            &format!(
                "/native-flood?url={}&n={FLOOD}&pad=32768&settle=3000",
                origin.url("/flood")
            ),
        )
        .as_user(&bob())
        .timeout(Duration::from_secs(120))
        .send()
    });
    let rise = peak.saturating_sub(before);
    eprintln!("MEASURED native flood: the program's memory rose by {rise} bytes");
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json();
    assert_eq!(
        answer["pending"], 72,
        "expected at most 72 calls not yet answered, got {answer}"
    );
    assert_eq!(
        answer["counts"]["error:unavailable"],
        FLOOD - 72,
        "expected every other call `unavailable`, got {answer}"
    );
    assert_eq!(
        o.entries_of(&response).len(),
        72,
        "expected no entry for a call past the 72"
    );
    assert!(
        rise < FLOOD_RISE,
        "expected the memory to rise by less than {} MiB, it rose by {} MiB",
        FLOOD_RISE / MIB,
        rise / MIB
    );
    still_serves(&o);
}

// --- The body markers, by hand -------------------------------------------------------------
//
// `args.body` as an object: `{"$native": id}` names a stream the host itself
// feeds (the request's body, a stored file's body); `{"$pumped": id,
// "$length": n}` one the code's side pumps bytes into
// (`__responseStreamCreate`, `__responseStreamWrite`, `__responseStreamEnd`);
// `{"$busy": ...}` says the code had no stream left; an answer's body is
// `{"$stream": id}`. What the code can reach: `__nativeStreamRead`,
// `__nativeStreamCancel`, `__createNativeStream`, `__responseStreamCreate`,
// `__responseStreamWrite`, `__responseStreamEnd`, `__responseStreamError`,
// `__responseStreamIsClosed`, `__responseFromMeta`, `__bufferBody`,
// `__encodeFetchBody`, `__nativeBindingInvoke` (and the other
// `__nativeBinding*`, timers and console).

const SURPLUS: &str = "GET /smuggled HTTP/1.1\r\nHost: x\r\n\r\n";

/// `/native-hand` with these steps and this request body; the answers of the
/// steps by name.
fn hand(o: &Outbound, steps: &Value, body: &[u8]) -> Value {
    let response =
        o.vm.request(
            APP,
            &format!(
                "/native-hand?steps={}",
                blackbox::outbound::segment(&steps.to_string())
            ),
        )
        .as_user(&bob())
        .method("POST")
        .body(body)
        .timeout(Duration::from_secs(90))
        .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let outs = response.json()["outs"].clone();
    eprintln!("OBSERVED {steps} -> {outs}");
    outs
}

fn assert_failed_call(out: &Value, context: &str) {
    assert!(
        out["resolved"]["ok"].is_null() && !out["timeout"].as_bool().unwrap_or(false),
        "{context}: expected a refusal or `malformed` (not an answer, not a hang), got {out}"
    );
}

/// The origin received nothing but what is allowed: no request to the
/// smuggled path, and no body that is not one of `only`.
fn assert_nothing_foreign(origin: &Origin, only: &[&str], context: &str) {
    for seen in origin.requests() {
        assert_ne!(seen.path(), "/smuggled", "{context}: {}", seen.describe());
        assert!(
            seen.body_len == 0 || only.contains(&seen.body_sha256.as_str()),
            "{context}: expected no body but what the code could have sent, got {}",
            seen.describe()
        );
    }
}

#[test]
fn a_native_marker_with_a_made_up_id_or_two_members_or_a_busy_one_is_refused_and_nothing_is_sent() {
    let (o, origin) = listed();
    let u = origin.url("/n");
    let outs = hand(
        &o,
        &json!([
            { "create": "p" },
            { "reqbody": "rb" },
            { "call": "native-made-up", "args": { "url": u, "method": "POST", "body": { "$native": 424242 } } },
            { "call": "pumped-made-up", "args": { "url": u, "method": "POST", "body": { "$pumped": 424242 } } },
            { "call": "busy-true", "args": { "url": u, "method": "POST", "body": { "$busy": true } } },
            { "call": "busy-none", "args": { "url": u, "method": "POST", "body": { "$busy": null } } },
            { "call": "busy-object", "args": { "url": u, "method": "POST", "body": { "$busy": { "a": 1 } } } },
            { "call": "two-members", "args": { "url": u, "method": "POST", "body": { "$native": "@rb", "$pumped": "@p" } } },
            { "call": "native-and-stream", "args": { "url": u, "method": "POST", "body": { "$native": "@rb", "$stream": 2 } } },
            { "call": "native-negative", "args": { "url": u, "method": "POST", "body": { "$native": -1 } } },
            { "call": "native-string", "args": { "url": u, "method": "POST", "body": { "$native": "7" } } },
        ]),
        b"the request's own body",
    );
    for name in [
        "native-made-up",
        "pumped-made-up",
        "busy-true",
        "busy-none",
        "busy-object",
        "native-negative",
        "native-string",
    ] {
        assert_failed_call(&outs[name], name);
    }
    // A marker with two members at once is read as one of them (the
    // request's own body, which a `fetch` could send): what the origin may
    // have received is that body whole, or nothing.
    let own = blackbox::outbound::sha256(b"the request's own body");
    assert_nothing_foreign(&origin, &[own.as_str()], "made-up and mixed markers");
    still_serves(&o);
}

#[test]
fn a_native_marker_naming_an_answers_body_or_the_requests_body_sends_only_those_bytes() {
    let o = Outbound::start();
    let a = o.origin();
    let b = o.origin();
    o.allow(&a.host());
    o.allow(&b.host());
    a.always(Reply::ok(b"the bytes of an answer of A"));
    let body = b"the request's own body";
    let outs = hand(
        &o,
        &json!([
            { "reqbody": "rb" },
            { "call": "a", "args": { "url": a.url("/answer") } },
            // The answer of A sent to B without being read, as
            // `fetch(urlB, { body: response.body })` would.
            { "call": "relay", "args": { "url": b.url("/relay"), "method": "POST", "body": { "$native": "@aBody" } } },
            // The request's body, as `fetch(urlB, { body: request.body })`.
            { "call": "request", "args": { "url": b.url("/request"), "method": "POST", "body": { "$native": "@rb" } } },
        ]),
        body,
    );
    eprintln!(
        "OBSERVED relay of A's answer to B: {}; the request's body to B: {}",
        outs["relay"], outs["request"]
    );
    let allowed = [
        blackbox::outbound::sha256(b"the bytes of an answer of A"),
        blackbox::outbound::sha256(body),
    ];
    let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
    assert_nothing_foreign(&b, &allowed, "B");
    // Each lands exactly as its own bytes, or is refused: never the other's.
    for seen in b.requests() {
        match seen.path() {
            "/relay" => assert!(
                seen.body_len == 0
                    || seen.body_sha256
                        == blackbox::outbound::sha256(b"the bytes of an answer of A"),
                "{}",
                seen.describe()
            ),
            "/request" => assert!(
                seen.body_len == 0 || seen.body_sha256 == blackbox::outbound::sha256(body),
                "{}",
                seen.describe()
            ),
            other => panic!("unexpected path {other}"),
        }
    }
    still_serves(&o);
}

#[test]
fn the_same_native_id_in_two_calls_at_once_goes_to_one_of_them_whole_and_never_to_both() {
    let o = Outbound::start();
    let a = o.origin();
    let b = o.origin();
    o.allow(&a.host());
    o.allow(&b.host());
    let body = vec![b'R'; 100 * 1024];
    let outs = hand(
        &o,
        &json!([
            { "reqbody": "rb" },
            { "start": "one", "args": { "url": a.url("/one"), "method": "POST", "body": { "$native": "@rb" } } },
            { "start": "two", "args": { "url": b.url("/two"), "method": "POST", "body": { "$native": "@rb" } } },
            { "await": "one" },
            { "await": "two" },
        ]),
        &body,
    );
    let whole = blackbox::outbound::sha256(&body);
    let mut got = Vec::new();
    for origin in [&a, &b] {
        for seen in origin.requests() {
            assert!(
                seen.body_len == 0 || seen.body_sha256 == whole,
                "expected a body whole or none, got {}",
                seen.describe()
            );
            if seen.body_len > 0 {
                got.push(seen.path().to_owned());
            }
        }
    }
    eprintln!(
        "OBSERVED bodies received at {got:?}; one: {}, two: {}",
        outs["one"], outs["two"]
    );
    assert!(
        got.len() <= 1,
        "expected the bytes of one stream to go to one call at most, they went to {got:?}"
    );
    still_serves(&o);
}

#[test]
fn a_pumped_marker_with_the_id_of_a_native_stream_and_the_reverse_sends_nothing_foreign() {
    let (o, origin) = listed();
    let u = origin.url("/n");
    let outs = hand(
        &o,
        &json!([
            { "create": "p" },
            { "reqbody": "rb" },
            { "call": "pumped-is-native", "args": { "url": u, "method": "POST", "body": { "$pumped": "@rb" } } },
            { "call": "native-is-pumped", "args": { "url": u, "method": "POST", "body": { "$native": "@p" } } },
        ]),
        b"the request's own body",
    );
    for name in ["pumped-is-native", "native-is-pumped"] {
        assert_failed_call(&outs[name], name);
    }
    assert_nothing_foreign(&origin, &[], "mixed ids");
    still_serves(&o);
}

/// A pumped stream of the code's own, with a `$length` (the JSON of the
/// member, or none), `written` pumped into it and ended.
fn pumped(o: &Outbound, origin: &Origin, length: Option<&str>, written: &str) -> Value {
    let length: Value = length.map_or(Value::Null, |l| serde_json::from_str(l).unwrap());
    let mut marker = json!({ "$pumped": "@p" });
    if !length.is_null() {
        marker["$length"] = length;
    }
    let outs = hand(
        o,
        &json!([
            { "create": "p" },
            { "start": "c", "args": { "url": origin.url("/pumped"), "method": "POST", "body": marker } },
            { "write": "@p", "text": written, "nowait": true },
            { "sleep": 300 },
            { "end": "@p" },
            { "await": "c" },
        ]),
        b"",
    );
    outs["c"].clone()
}

#[test]
fn a_pumped_stream_whose_length_lies_never_makes_a_second_request_of_the_surplus() {
    let (o, origin) = listed();
    let written = format!("abc{SURPLUS}");
    // The control: the length that is true.
    let out = pumped(&o, &origin, Some(&written.len().to_string()), &written);
    assert!(!out["timeout"].as_bool().unwrap_or(false), "{out}");
    let seen = origin.requests();
    assert_eq!(seen.len(), 1, "the control: {out}");
    assert_eq!(seen[0].body_len, u64::try_from(written.len()).unwrap());
    // A length shorter than what is then written: three bytes stated.
    let before = origin.requests().len();
    let out = pumped(&o, &origin, Some("3"), &written);
    assert!(!out["timeout"].as_bool().unwrap_or(false), "a hang: {out}");
    let seen = origin.requests();
    assert!(
        seen.len() <= before + 1,
        "expected at most one more request, got {:?}",
        seen.iter().map(|s| s.describe()).collect::<Vec<_>>()
    );
    for s in &seen {
        assert_ne!(s.path(), "/smuggled", "{}", s.describe());
        assert!(s.body_len <= u64::try_from(written.len()).unwrap());
    }
    // A length longer than what is written (the stream then ends): a short
    // body is a failed call, in the bounds and not a hang.
    let before = origin.requests().len();
    let out = pumped(&o, &origin, Some("100000"), "abc");
    assert!(!out["timeout"].as_bool().unwrap_or(false), "a hang: {out}");
    let seen = origin.requests();
    for s in &seen[before..] {
        assert!(
            !s.body_complete || s.body_len == 100_000,
            "expected a short body to be no complete request: {}",
            s.describe()
        );
    }
    assert!(
        out["resolved"]["ok"].is_null() || seen[before..].iter().all(|s| s.body_len == 3),
        "a body of 3 bytes under a length of 100000 was answered as whole: {out}"
    );
    still_serves(&o);
}

#[test]
fn a_pumped_length_that_is_no_length_is_refused_or_ignored_and_never_a_second_request() {
    let (o, origin) = listed();
    let written = format!("abc{SURPLUS}");
    for length in [
        "-1",
        "1.5",
        "\"5\"",
        "9007199254740992",
        "18446744073709551616",
        "null",
        "[3]",
        "true",
    ] {
        let before = origin.requests().len();
        let out = pumped(&o, &origin, Some(length), &written);
        assert!(
            !out["timeout"].as_bool().unwrap_or(false),
            "$length {length}: a hang: {out}"
        );
        let seen = origin.requests();
        assert!(seen.len() <= before + 1, "$length {length}: {out}");
        for s in &seen {
            assert_ne!(s.path(), "/smuggled", "$length {length}: {}", s.describe());
        }
        for s in &seen[before..] {
            assert!(
                s.body_len == 0 || s.body_len <= u64::try_from(written.len()).unwrap(),
                "$length {length}: {}",
                s.describe()
            );
        }
        eprintln!("OBSERVED $length {length}: {out}");
    }
    still_serves(&o);
}

// --- The mapped address -------------------------------------------------------------------------

#[test]
fn a_mapped_loopback_address_is_called_when_the_range_it_carries_is_opened_and_not_otherwise() {
    use std::net::{IpAddr, Ipv4Addr};
    let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
    // Opened: 127.0.0.1/32 (and ::1, as the origin listens on it too).
    for (args, answered) in [
        (&["--outbound-private", "127.0.0.1/32"][..], true),
        (&["--outbound-private", "10.0.0.0/8"][..], false),
    ] {
        let o = Outbound::start_with(args, &[]);
        let cert = blackbox::outbound::Cert::for_names_and_addresses(
            &o.authority,
            &["localhost"],
            &[IpAddr::V4(Ipv4Addr::LOCALHOST), mapped],
        );
        let origin = Origin::start(cert.wire());
        let entry = format!("[::ffff:7f00:1]:{}", origin.port());
        o.allow(&entry);
        let target = format!("https://[::ffff:127.0.0.1]:{}/mapped", origin.port());
        let report = o.call(&url(&target));
        if answered {
            assert_resolved(&report, 200, "a mapped loopback with 127.0.0.1/32 opened");
        } else {
            assert_refused(
                &report,
                "address-not-allowed",
                None,
                "a mapped loopback with only 10.0.0.0/8 opened",
            );
            assert!(origin.requests().is_empty());
        }
    }
}

// --- A body that stops before its end, with no content-length -----------------------------------

#[test]
fn a_chunked_answer_that_stops_before_its_end_passed_on_is_a_connection_that_ends_without_a_clean_end()
 {
    let (o, origin) = listed();
    origin.always(Reply::new(200).chunked_pieces_then_close(
        &[b"first piece ", b"second piece ", b"third piece"],
        Duration::from_millis(800),
    ));
    let mut raw = Raw::connect(o.vm.address());
    raw.send_head(
        "GET",
        &format!("/pass?url={}", origin.url("/cut")),
        &o.vm.host_of(APP),
        Some(&bob()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200, "{head:?}");
    let mut got = Vec::new();
    let end = raw.drain_body(Duration::from_secs(30), |piece| {
        got.extend_from_slice(piece)
    });
    eprintln!(
        "OBSERVED pass-through of a cut chunked body: end {end:?}, {} bytes {:?}",
        got.len(),
        String::from_utf8_lossy(&got)
    );
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the client's connection to end without the body's terminating chunk, it saw {end:?} after {:?}",
        String::from_utf8_lossy(&got)
    );
    assert_eq!(
        String::from_utf8_lossy(&got),
        "first piece second piece third piece",
        "the bytes sent arrive"
    );
}

#[test]
fn a_chunked_answer_that_stops_before_its_end_rejects_the_code_that_reads_it() {
    let (o, origin) = listed();
    origin.always(Reply::new(200).chunked_pieces_then_close(
        &[b"first piece ", b"second piece"],
        Duration::from_millis(500),
    ));
    let response =
        o.vm.request(APP, &format!("/text?url={}", origin.url("/cut")))
            .as_user(&bob())
            .timeout(Duration::from_secs(60))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let seen = response.json();
    assert!(
        seen["text"].is_null() && !seen["rejected"].is_null(),
        "expected `text()` to reject, not to resolve with the short text, got {seen}"
    );
}

#[test]
fn a_chunked_answer_closed_right_behind_its_last_data_is_never_ended_cleanly_for_the_client() {
    // The origin sends the head and the chunks and closes at once, with no
    // pause and no terminating chunk; the failure arrives right behind the
    // last data. Each round is a fresh request. The promise: zero clean ends.
    const ROUNDS: usize = 30;
    let (o, origin) = listed();
    let small: [&[u8]; 3] = [b"first piece ", b"second piece ", b"third piece"];
    let block = vec![b'z'; 64 * 1024];
    let large: [&[u8]; 3] = [&block, &block, &block];
    let mut clean = [0_usize; 2];
    for (i, pieces) in [small, large].into_iter().enumerate() {
        origin.always(Reply::new(200).chunked_pieces_then_close(&pieces, Duration::ZERO));
        for _ in 0..ROUNDS {
            let mut raw = Raw::connect(o.vm.address());
            raw.send_head(
                "GET",
                &format!("/pass?url={}", origin.url("/cut")),
                &o.vm.host_of(APP),
                Some(&bob()),
                Framing::None,
                &[],
            );
            let head = raw.read_head(Duration::from_secs(30));
            assert_eq!(head.status, 200, "{head:?}");
            let end = raw.drain_body(Duration::from_secs(30), |_| ());
            assert_ne!(end, BodyEnd::StillOpen, "the connection stayed open");
            if end == BodyEnd::Whole {
                clean[i] += 1;
            }
        }
    }
    eprintln!(
        "MEASURED clean ends out of {ROUNDS}: small pieces {}, 64 KiB pieces {}",
        clean[0], clean[1]
    );
    assert_eq!(
        clean,
        [0, 0],
        "expected no body ended as if whole, got {} of {ROUNDS} with small pieces and {} of {ROUNDS} with 64 KiB pieces",
        clean[0],
        clean[1]
    );
}
