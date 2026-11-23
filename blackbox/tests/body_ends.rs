//! What ends a request's body, and what the program gives back (CONTRACT.md,
//! "What an Application is handed" and "Requests in flight"):
//!
//! - a disable comes before a body's `413` or `408`;
//! - what a request's body held is given back when the request ends;
//! - an answer with no body on the wire (`HEAD`, `204`, `304`) has none
//!   whatever the code answered, and the next request on the connection is
//!   answered as its own;
//! - `100 Continue` is sent only when the body is asked for.
//!
//! Not here: "a request waiting to be given to the code has not had its body
//! asked for". See the note at the end of this file.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::admin::{self, IN_FLIGHT_ALLOWED, administrator};
use blackbox::body::{BodyEnd, Framing, MIB, Raw, memory_of, pattern_block};
use blackbox::logs::wait_for_entry;
use blackbox::{Vm, fixture_bundle};

/// The fixture of the request-body routes.
const APP: &str = "body-request";

/// The fixture of the response-body routes.
const ECHO: &str = "body-echo";

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

fn alice() -> User {
    User::new("alice")
}

// ---------------------------------------------------------------------
// 1. A disable comes before a body's `413` or `408`
// ---------------------------------------------------------------------

/// The code asks for its body (one chunk of 16 bytes arrives) and waits 25
/// seconds; the client goes quiet. `quiet_before_disable` later the
/// Application is disabled. The request is answered as a disabled
/// Application answers, within the second the contract gives (the harness
/// allows `IN_FLIGHT_ALLOWED` on a loaded machine), never `408`, and its
/// entry has status `503`.
fn disabled_while_the_body_is_quiet(quiet_before_disable: Duration) {
    let vm = Vm::start(&admin::options());
    vm.api_as(&alice())
        .create_with_bundle(APP, &fixture_bundle("body-request.js"));
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/read-one-then-sleep?ms=25000",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    raw.send_chunk(&pattern_block(16))
        .expect("a chunk is written");
    thread::sleep(quiet_before_disable);

    let answer = admin::disable(&vm, &administrator(), APP);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    let disabled_at = Instant::now();
    let head = raw.read_head(Duration::from_secs(15));
    let waited = disabled_at.elapsed();
    let mut body = Vec::new();
    raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b));

    // What a disabled Application answers, as a request made now sees it.
    let plain = vm.get_as(&bob(), APP, "/never-reads");
    assert_ne!(plain.status, 200, "the Application is disabled");
    assert!(
        head.status != 408 && head.status != 413,
        "expected the answer of a disabled Application, not the body's {}",
        head.status
    );
    assert_eq!(
        head.status, plain.status,
        "expected a disabled Application's answer ({}), got {}",
        plain.status, head.status
    );
    assert_eq!(
        body,
        plain.body(),
        "expected the same words as a disabled Application's answer"
    );
    assert!(
        waited <= IN_FLIGHT_ALLOWED,
        "expected the answer within 1 s of the disable's 200 ({IN_FLIGHT_ALLOWED:?} allowed on a loaded machine), it took {waited:?}"
    );
    let (entry, _) = wait_for_entry(&vm.api_as(&alice()), APP, &head.request_id());
    assert_eq!(
        entry["status"], 503,
        "expected the entry of a request disabled in flight to say 503, {entry}"
    );
}

#[test]
fn a_disable_before_ten_quiet_seconds_answers_as_disabled_and_not_408() {
    // About 5 seconds.
    disabled_while_the_body_is_quiet(Duration::from_secs(3));
}

#[test]
fn a_disable_after_the_body_has_failed_while_the_code_still_runs_answers_as_disabled_and_not_408() {
    // About 15 seconds: the stream fails at about 10 s (the code is still
    // asleep; it ends at 25 s, inside the request's 30), the disable comes
    // at 12 s, and the `408` that was to take the code's place must not
    // take the disabled answer's.
    disabled_while_the_body_is_quiet(Duration::from_secs(12));
}

// ---------------------------------------------------------------------
// 2. What a request's body held is given back
// ---------------------------------------------------------------------

/// `count` requests of 4 MiB each to `path`, `threads` at a time, each
/// connection of its own. Returns what went wrong (an answer that is not
/// `200`, or none).
fn four_mib_requests(vm: &Vm, path: &str, count: usize, threads: usize) -> Vec<String> {
    let failures = Mutex::new(Vec::new());
    let block = pattern_block(256 * 1024);
    let blocks = 16; // 16 x 256 KiB
    let each = count / threads;
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                for _ in 0..each {
                    let mut raw = Raw::connect(vm.address());
                    raw.send_head(
                        "POST",
                        path,
                        &vm.host_of(APP),
                        Some(&bob()),
                        Framing::Length(4 * MIB),
                        &[],
                    );
                    let mut writer = raw.split_writer();
                    let done = AtomicBool::new(false);
                    let outcome = thread::scope(|inner| {
                        inner.spawn(|| {
                            for _ in 0..blocks {
                                if writer.write(&block).is_err() {
                                    break;
                                }
                            }
                            done.store(true, Ordering::Relaxed);
                        });
                        let head = raw.try_read_head(Duration::from_secs(20));
                        // A program that takes the rest of the body lets the
                        // writer finish; one that does not has it let go.
                        let waited = Instant::now();
                        while !done.load(Ordering::Relaxed)
                            && waited.elapsed() < Duration::from_secs(3)
                        {
                            thread::sleep(Duration::from_millis(10));
                        }
                        raw.go_away();
                        head
                    });
                    match outcome {
                        Ok(head) if head.status == 200 => {}
                        Ok(head) => failures
                            .lock()
                            .expect("not poisoned")
                            .push(format!("{path}: answered {}", head.status)),
                        Err(problem) => failures
                            .lock()
                            .expect("not poisoned")
                            .push(format!("{path}: {}", problem.lines().next().unwrap_or(""))),
                    }
                }
            });
        }
    });
    failures.into_inner().expect("not poisoned")
}

#[test]
fn what_a_requests_body_held_is_given_back_when_the_request_ends() {
    // About 20 to 40 seconds: 16 warm-up requests of 4 MiB per route, then
    // 200 requests of 4 MiB per route (400 in all, 1.6 GB through loopback),
    // 8 at a time. Memory is the program's resident memory after the
    // requests and a short rest, against what it was after the warm-up.
    let vm = Vm::with_stand_in(&[(APP, "body-request.js")]);
    let pid = vm.pid();
    let routes = ["/never-reads", "/read-one"];
    for route in routes {
        let failures = four_mib_requests(&vm, route, 16, 8);
        assert!(failures.is_empty(), "warm-up: {failures:?}");
    }
    thread::sleep(Duration::from_secs(1));
    let before = memory_of(pid, "VmRSS");
    for route in routes {
        let failures = four_mib_requests(&vm, route, 200, 8);
        assert!(
            failures.is_empty(),
            "{} request(s) of 200 to {route} failed, the first: {:?}",
            failures.len(),
            failures.first()
        );
        thread::sleep(Duration::from_secs(2));
        let after = memory_of(pid, "VmRSS");
        let grew = after.saturating_sub(before);
        assert!(
            grew < 64 * MIB,
            "expected the program's resident memory less than 64 MiB above where it was after 200 requests of 4 MiB to {route}, it is {} MiB above ({} MiB to {} MiB)",
            grew / MIB,
            before / MIB,
            after / MIB
        );
    }
}

// ---------------------------------------------------------------------
// 3. An answer with no body on the wire
// ---------------------------------------------------------------------

fn encoded_marker(marker: &str) -> String {
    marker.replace('-', "%2D")
}

/// Runs `case`, noting in `failures` what it panicked with.
fn collect(failures: &mut Vec<String>, what: &str, case: impl FnOnce()) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(case)) {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default();
        let first: String = message.lines().take(3).collect::<Vec<_>>().join(" | ");
        failures.push(format!("{what}: {first}"));
    }
}

/// One request whose answer has no body on the wire. `body` is what the
/// request carries: `Some(sent)` states its length and sends it, `None`
/// sends none; `stated_unsent` states a length and sends nothing (the
/// connection is then not asked for another request).
struct NoBody<'a> {
    method: &'a str,
    target: &'a str,
    body: Option<&'a [u8]>,
    stated_unsent: bool,
    expect_status: Option<u16>,
    no_content_length: bool,
}

fn no_body_case(vm: &Vm, case: &NoBody<'_>) {
    let mut raw = Raw::connect_keep_alive(vm.address());
    let framing = match (case.body, case.stated_unsent) {
        (Some(body), _) => Framing::Length(u64::try_from(body.len()).expect("fits")),
        (None, true) => Framing::Length(MIB),
        (None, false) => Framing::None,
    };
    raw.send_head(
        case.method,
        case.target,
        &vm.host_of(ECHO),
        Some(&bob()),
        framing,
        &[],
    );
    if let Some(body) = case.body {
        raw.write(body).expect("the body is written");
    }
    let head = raw.read_head(Duration::from_secs(10));
    if let Some(expected) = case.expect_status {
        assert_eq!(head.status, expected, "status of {}", case.target);
    }
    if case.no_content_length {
        assert_eq!(
            head.header("content-length"),
            None,
            "expected no content-length on a {}",
            head.status
        );
        assert_eq!(
            head.header("transfer-encoding"),
            None,
            "no framing of a body"
        );
    }
    // Nothing of a body follows the head.
    let stray = raw.bytes_within(Duration::from_millis(400));
    assert_eq!(
        stray, 0,
        "expected no body byte after the head of {} {}, got {stray}",
        case.method, case.target
    );
    if case.stated_unsent {
        return;
    }
    // The next request on the connection is answered as its own.
    let marker = format!("next-after-{}-{}", case.method, case.target.len());
    let (next, body, end) = raw.exchange(
        "GET",
        &format!("/text?body={}", encoded_marker(&marker)),
        &vm.host_of(ECHO),
        Some(&bob()),
        None,
        Duration::from_secs(10),
    );
    assert_eq!(next.status, 200, "the next request: {}", next.status);
    assert_eq!(end, BodyEnd::Whole);
    assert_eq!(
        String::from_utf8_lossy(&body),
        marker,
        "expected the next request answered as its own"
    );
}

#[test]
fn an_answer_with_no_body_on_the_wire_has_none_and_the_next_request_is_its_own() {
    // About 6 seconds (a 400 ms look for stray bytes after each head).
    // `204` and `304` are reached with `Object.defineProperty(response,
    // "status", ...)` on a Response with a body (the fixture's `/forced`):
    // a Response cannot be built with such a status and a body.
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let sent = pattern_block(4096);
    let mut failures = Vec::new();

    for (what, target, stated_unsent, body) in [
        (
            "HEAD echo, body sent",
            "/echo-unread",
            false,
            Some(sent.as_slice()),
        ),
        (
            "HEAD echo, body stated and not sent",
            "/echo-unread",
            true,
            None,
        ),
        (
            "HEAD made, no body",
            "/made?kind=string&size=100",
            false,
            None,
        ),
        (
            "HEAD made, body sent",
            "/made?kind=string&size=100",
            false,
            Some(sent.as_slice()),
        ),
        (
            "HEAD made, body stated and not sent",
            "/made?kind=string&size=100",
            true,
            None,
        ),
    ] {
        collect(&mut failures, what, || {
            no_body_case(
                &vm,
                &NoBody {
                    method: "HEAD",
                    target,
                    body,
                    stated_unsent,
                    expect_status: Some(200),
                    no_content_length: false,
                },
            );
        });
    }

    for status in [204_u16, 304] {
        for kind in ["made", "echo"] {
            for (sent_label, stated_unsent, body) in [
                ("body sent", false, Some(sent.as_slice())),
                ("body stated and not sent", true, None),
            ] {
                let target = format!("/forced?status={status}&kind={kind}");
                collect(
                    &mut failures,
                    &format!("{status} with a {kind} body, {sent_label}"),
                    || {
                        no_body_case(
                            &vm,
                            &NoBody {
                                method: "POST",
                                target: &target,
                                body,
                                stated_unsent,
                                expect_status: Some(status),
                                no_content_length: status == 204,
                            },
                        );
                    },
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------
// 4. `100 Continue` only when asked for
// ---------------------------------------------------------------------

#[test]
fn a_client_that_expects_100_continue_is_not_told_when_nobody_asks_for_the_body() {
    // About 1 second. The code never reads the body: the answer comes, and
    // no `100 Continue` before it.
    let vm = vm_request();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/never-reads",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(MIB),
        &[("Expect", "100-continue")],
    );
    let head = raw.read_head(Duration::from_secs(5));
    assert_eq!(head.status, 200, "got {}", head.status);
    assert_eq!(
        raw.interims(),
        0,
        "expected no `100 Continue` for a body nobody asked for"
    );
}

#[test]
fn a_client_that_expects_100_continue_is_told_only_after_the_code_ran_and_asked_for_the_body() {
    // About 3 seconds. The code sleeps 1 second before it reads: no interim
    // response in the first 500 ms; one arrives once the code asks; the
    // body then goes and is read whole.
    let vm = vm_request();
    let mut raw = Raw::connect(vm.address());
    let began = Instant::now();
    raw.send_head(
        "POST",
        "/sleep-then-read-all?ms=1000",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(16),
        &[("Expect", "100-continue")],
    );
    let early = raw.bytes_within(Duration::from_millis(500));
    assert_eq!(
        early, 0,
        "expected nothing on the wire in the first 500 ms: the code has not asked for the body, so no `100 Continue` yet"
    );
    let interim = raw.wait_interim(Duration::from_secs(8));
    let at = interim.map(|_| began.elapsed());
    assert!(
        interim.is_some(),
        "expected a `100 Continue` once the code asked for the body"
    );
    assert!(
        at.expect("some") >= Duration::from_millis(900),
        "the `100 Continue` came after {at:?}, before the code (asleep for 1 s) could have asked"
    );
    raw.write(&pattern_block(16)).expect("the body is written");
    let head = raw.read_head(Duration::from_secs(10));
    assert_eq!(head.status, 200, "got {}", head.status);
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b)),
        BodyEnd::Whole
    );
    let answer: serde_json::Value = serde_json::from_slice(&body).expect("the code's JSON");
    assert_eq!(answer["bytes"], 16, "{answer}");
}

fn vm_request() -> Vm {
    Vm::with_stand_in(&[(APP, "body-request.js")])
}

// ---------------------------------------------------------------------
// 5. A request waiting to be given to the code has not had its body asked
//    for: not pinned.
//
// The contract states no number for how many requests the program gives to
// Application code at once, and no black-box test in `blackbox/tests` fills
// the runner to make a request wait (the only `503` answers of the tests are
// for other limits: pulls, readings for administrators, a platform database
// that cannot be reached). A test would have to guess the limit and run an
// unknown number of sleeping requests to find one that waits, and what it
// found would depend on the machine. It is left until the contract says how
// many requests run at once.
// ---------------------------------------------------------------------
