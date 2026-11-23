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
use blackbox::body::{BodyEnd, Framing, MIB, Raw, assert_framed_truly, memory_of, pattern_block};
use blackbox::logs::wait_for_entry;
use blackbox::{INSTALLER, Vm, fixture_bundle};

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
                            && waited.elapsed() < Duration::from_secs(1)
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
    // About 2 minutes. Six routes, each with 200 requests of 4 MiB to warm
    // it and then 200 more of 4 MiB, 8 at a time (the routes that
    // answer at once) or 20 at a time (the three that hold the request 1.5
    // seconds, so that the platform's read-ahead fills while the code waits:
    // 10 rounds of 1.5 s each). That is 2,400 requests of 4 MiB, 10 GB
    // through loopback. The routes:
    //
    // - `/never-reads`, `/read-one`: answer at once (the read-ahead never
    //   fills);
    // - `/read-one-wait-made`: reads one chunk, waits 1.5 s, answers a
    //   string it made;
    // - `/read-one-wait-echo`: reads one chunk, waits 1.5 s, gives the
    //   request's body back (collected whole: 4 MiB is under the 8 MiB);
    // - `/read-unawaited`: starts a read of the body, does not wait for it
    //   and answers a string it made at once (the request may end with that
    //   read still pending);
    // - `/sleep`: never reads, waits 1.5 s, answers; the client keeps
    //   writing meanwhile, from a thread, and lets go when the answer is in.
    //
    // Memory is the program's resident memory: for each route, 200
    // requests warm it to its working set, the level after a 2-second rest
    // is the baseline, and after 200 more and a rest it must be less than
    // 64 MiB above that baseline.
    let vm = Vm::with_stand_in(&[(APP, "body-request.js")]);
    let pid = vm.pid();
    let routes: [(&str, usize); 6] = [
        ("/read-unawaited", 8),
        ("/never-reads", 8),
        ("/read-one", 8),
        ("/read-one-wait-made?ms=1500", 20),
        ("/read-one-wait-echo?ms=1500", 20),
        ("/sleep?ms=1500", 20),
    ];
    for (route, threads) in routes {
        // The first 200 take the route to its working set (20 collected
        // bodies of 4 MiB at once are more than 80 MiB held, and the
        // allocator keeps what it took); the baseline is taken after them
        // and a rest, and the second 200 are what is measured.
        let failures = four_mib_requests(&vm, route, 200, threads);
        assert!(
            failures.is_empty(),
            "{} request(s) of the first 200 to {route} failed, the first: {:?}",
            failures.len(),
            failures.first()
        );
        thread::sleep(Duration::from_secs(2));
        let before = memory_of(pid, "VmRSS");
        let failures = four_mib_requests(&vm, route, 200, threads);
        assert!(
            failures.is_empty(),
            "{} request(s) of the second 200 to {route} failed, the first: {:?}",
            failures.len(),
            failures.first()
        );
        thread::sleep(Duration::from_secs(2));
        let after = memory_of(pid, "VmRSS");
        let grew = after.saturating_sub(before);
        assert!(
            grew < 64 * MIB,
            "expected the program's resident memory less than 64 MiB above where it was after 200 more requests of 4 MiB to {route}, it is {} MiB above ({} MiB to {} MiB)",
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

// ---------------------------------------------------------------------
// 6. The code cannot write into a stream the host feeds
// ---------------------------------------------------------------------

/// What `/write-into-host-stream` says it found, from the answer's head: it
/// calls the runtime's own `__responseStreamWrite`, `__responseStreamEnd`,
/// `__responseStreamError` and `__nativeScheduleWritable` (the last with the
/// id alone and with the id and a function: its arguments are not known to
/// this test) with a made-up id (999999) and with the request body's own id.
fn assert_writes_threw(head: &blackbox::body::Head) {
    assert_eq!(
        head.header("x-had-id"),
        Some("true"),
        "`request.body._nativeStreamId` is not a number in this runtime: the test cannot name the host's stream"
    );
    assert_eq!(
        head.header("x-missing"),
        Some(""),
        "a runtime call the test means to make is not there"
    );
    let mut threw: Vec<&str> = head
        .header("x-threw")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .collect();
    threw.sort_unstable();
    let mut expected: Vec<String> = Vec::new();
    for which in ["made-up", "real"] {
        for name in ["write", "end", "error", "schedule", "schedule-fn"] {
            expected.push(format!("{which}:{name}"));
        }
    }
    expected.sort_unstable();
    assert_eq!(
        threw, expected,
        "expected every call on a stream the code did not make to throw (the request's body, a made-up id): these did"
    );
}

#[test]
fn the_code_cannot_write_into_the_stream_the_host_feeds_and_the_body_arrives_as_sent() {
    // About 1 second. The code writes 1,000 bytes into the request body's
    // own stream id and into a made-up id (999999), both must throw, and the
    // body given back is exactly what the client sent, with the length the
    // platform stated; on the same connection the next request is its own.
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let sent: Vec<u8> = (0..4096_u32)
        .map(|i| u8::try_from((i * 7 + 3) % 256).expect("below 256"))
        .collect();
    let mut raw = Raw::connect_keep_alive(vm.address());
    let (head, body, end) = raw.exchange(
        "POST",
        "/write-into-host-stream",
        &vm.host_of(ECHO),
        Some(&bob()),
        Some(&sent),
        Duration::from_secs(10),
    );
    assert_eq!(head.status, 200, "got {}", head.status);
    assert_writes_threw(&head);
    assert_eq!(end, BodyEnd::Whole);
    assert_eq!(body, sent, "expected exactly the bytes the client sent");
    assert_eq!(head.content_length(), Some(4096), "the stated length");

    let (next, text, end) = raw.exchange(
        "GET",
        "/text?body=next",
        &vm.host_of(ECHO),
        Some(&bob()),
        None,
        Duration::from_secs(10),
    );
    assert_eq!((next.status, end), (200, BodyEnd::Whole));
    assert_eq!(
        text, b"next",
        "expected the next request answered as its own"
    );
}

#[test]
fn the_code_cannot_write_into_the_stream_the_host_feeds_when_no_length_is_stated() {
    // About 1 second. The same for a chunked request: the body given back is
    // exactly what was sent and ends cleanly (chunked, with its last chunk).
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let sent: Vec<u8> = (0..4096_u32)
        .map(|i| u8::try_from((i * 7 + 3) % 256).expect("below 256"))
        .collect();
    let mut raw = Raw::connect_keep_alive(vm.address());
    raw.send_head(
        "POST",
        "/write-into-host-stream",
        &vm.host_of(ECHO),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    raw.send_chunk(&sent).expect("a chunk is written");
    raw.finish_chunks().expect("the end is written");
    let head = raw.read_head(Duration::from_secs(10));
    assert_eq!(head.status, 200, "got {}", head.status);
    assert_writes_threw(&head);
    assert_eq!(head.content_length(), None);
    assert!(head.chunked(), "expected the body chunked");
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(10), |b| body.extend_from_slice(b)),
        BodyEnd::Whole,
        "expected the body to end cleanly"
    );
    assert_eq!(body, sent, "expected exactly the bytes the client sent");
}

// ---------------------------------------------------------------------
// 7. A body the code cancelled
// ---------------------------------------------------------------------

#[test]
fn a_body_the_code_cancelled_is_never_sent_as_the_clients_bytes() {
    // About 3 seconds. The code cancels the request's body (on the stream,
    // or through a reader it releases) and gives it back, or answers a
    // string it made. Allowed for a body given back: `200` with an empty
    // body, or the fixed `500`; never the client's bytes; the framing is
    // true; where the client sent its whole body the next request on the
    // connection is answered as its own. Each outcome is printed (see them
    // with `--nocapture`).
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let sent: Vec<u8> = (0..4096_u32)
        .map(|i| u8::try_from((i * 7 + 3) % 256).expect("below 256"))
        .collect();
    let mut failures = Vec::new();
    for route in ["/cancel-a", "/cancel-b", "/cancel-made"] {
        for whole in [true, false] {
            let what = format!(
                "{route}, client {}",
                if whole {
                    "sent all 4096 bytes"
                } else {
                    "stated 1 MiB and sent none"
                }
            );
            collect(&mut failures, &what, || {
                let mut raw = Raw::connect_keep_alive(vm.address());
                let framing = if whole {
                    Framing::Length(4096)
                } else {
                    Framing::Length(MIB)
                };
                raw.send_head("POST", route, &vm.host_of(ECHO), Some(&bob()), framing, &[]);
                if whole {
                    raw.write(&sent).expect("the body is written");
                }
                let head = raw.read_head(Duration::from_secs(10));
                let mut body = Vec::new();
                let end = raw.drain_body(Duration::from_secs(10), |b| body.extend_from_slice(b));
                eprintln!(
                    "OUTCOME {what}: status {}, content-length {:?}, chunked {}, {} body bytes, end {end:?}",
                    head.status,
                    head.content_length(),
                    head.chunked(),
                    body.len()
                );
                // Framing is the platform's: the answer is whole as it states,
                // or the connection is closed without a clean end short of a
                // stated length (the client can tell); never a clean end
                // short of it.
                match end {
                    BodyEnd::Whole => {
                        assert_framed_truly(&head, u64::try_from(body.len()).expect("fits"), &what)
                    }
                    BodyEnd::ClosedShort => assert!(
                        head.content_length()
                            .is_some_and(|l| l > u64::try_from(body.len()).expect("fits")),
                        "closed short of nothing stated"
                    ),
                    BodyEnd::StillOpen => panic!("the answer never ended"),
                }
                assert!(
                    !body.windows(64).any(|w| sent.windows(64).any(|s| s == w)),
                    "the client's own bytes were sent back after the code cancelled them"
                );
                if route == "/cancel-made" {
                    assert_eq!(head.status, 200, "got {}", head.status);
                    assert_eq!(body, b"made after cancel");
                } else if head.status == 200 {
                    assert!(body.is_empty(), "a 200 with {} body bytes", body.len());
                } else {
                    assert_eq!(head.status, 500, "expected 200 empty or the fixed 500");
                    assert!(body.len() < 1024, "the fixed 500's body");
                }
                if whole && end == BodyEnd::Whole {
                    let (next, text, end) = raw.exchange(
                        "GET",
                        "/text?body=next",
                        &vm.host_of(ECHO),
                        Some(&bob()),
                        None,
                        Duration::from_secs(10),
                    );
                    assert_eq!((next.status, end), (200, BodyEnd::Whole));
                    assert_eq!(text, b"next", "the next request answered as its own");
                }
            });
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
// 8. A locked body cannot be given back
// ---------------------------------------------------------------------

#[test]
fn a_body_the_code_still_holds_a_reader_on_is_the_fixed_500_and_a_released_one_is_given_back() {
    // About 2 seconds. The client sends 1 MiB whole. A read started and not
    // awaited, or one chunk read with the reader still held, leaves the
    // stream locked: the request fails as code that throws does (the fixed
    // `500`, as `errors.rs` recognises it: a short body that reveals
    // nothing), with the entry's status `500`; never a `200` with an empty or
    // partial body. The control reads one chunk and releases the lock: the
    // rest comes whole.
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let block = pattern_block(usize::try_from(MIB).expect("fits"));
    let mut failures = Vec::new();
    for route in ["/locked-a", "/locked-b"] {
        collect(&mut failures, route, || {
            let mut raw = Raw::connect_keep_alive(vm.address());
            let (head, body, end) = raw.exchange(
                "POST",
                route,
                &vm.host_of(ECHO),
                Some(&bob()),
                Some(&block),
                Duration::from_secs(10),
            );
            assert_eq!(
                head.status,
                500,
                "{route}: expected the fixed 500 for a locked body given back, got {} with {} body bytes",
                head.status,
                body.len()
            );
            assert_eq!(end, BodyEnd::Whole, "{route}");
            assert_framed_truly(&head, u64::try_from(body.len()).expect("fits"), route);
            assert!(
                body.len() < 1024
                    && !body
                        .windows(64)
                        .any(|w| block.windows(64).next() == Some(w)),
                "{route}: expected the fixed 500's body, got {} bytes",
                body.len()
            );
            let (entry, _) =
                wait_for_entry(&vm.api_as(&User::new(INSTALLER)), ECHO, &head.request_id());
            assert_eq!(entry["status"], 500, "{route}: {entry}");
        });
    }
    collect(
        &mut failures,
        "control: one chunk read, lock released",
        || {
            let mut raw = Raw::connect_keep_alive(vm.address());
            let (head, body, end) = raw.exchange(
                "POST",
                "/echo-read-then-give-back",
                &vm.host_of(ECHO),
                Some(&bob()),
                Some(&block),
                Duration::from_secs(10),
            );
            assert_eq!(head.status, 200, "got {}", head.status);
            assert_eq!(end, BodyEnd::Whole);
            let first: usize = head
                .header("x-first-chunk")
                .expect("the code says how much it read")
                .parse()
                .expect("a number");
            assert_eq!(
                body.as_slice(),
                block.get(first..).expect("within the body"),
                "expected the rest of the body whole"
            );
            assert_framed_truly(&head, u64::try_from(body.len()).expect("fits"), "control");
        },
    );
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------
// 9. Cancelling does not ask for the body, and neither does a read after
// ---------------------------------------------------------------------

#[test]
fn cancelling_the_body_and_reading_after_it_do_not_ask_the_client_to_send_it() {
    // About 2 seconds. The client sent `Expect: 100-continue` and holds its
    // body back. The code cancels (and, in the second route, then reads
    // `request.text()` inside a try/catch) and answers a string it made: no
    // `100 Continue` at all, the string with `200`, and the connection
    // closed after the answer (the client had not sent its body).
    let vm = Vm::with_stand_in(&[(ECHO, "body-response.js")]);
    let mut failures = Vec::new();
    for route in ["/cancel-made", "/cancel-then-text"] {
        collect(&mut failures, route, || {
            let mut raw = Raw::connect_keep_alive(vm.address());
            raw.send_head(
                "POST",
                route,
                &vm.host_of(ECHO),
                Some(&bob()),
                Framing::Length(MIB),
                &[("Expect", "100-continue")],
            );
            let head = raw.read_head(Duration::from_secs(5));
            assert_eq!(head.status, 200, "{route}: got {}", head.status);
            let mut body = Vec::new();
            assert_eq!(
                raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b)),
                BodyEnd::Whole,
                "{route}"
            );
            assert_eq!(body, b"made after cancel", "{route}");
            assert_eq!(
                raw.interims(),
                0,
                "{route}: expected no `100 Continue`: cancelling does not ask for the body"
            );
            assert!(
                raw.wait_closed_without_taking(Duration::from_secs(5))
                    .is_some(),
                "{route}: expected the connection closed after the answer: the client had not sent its body"
            );
        });
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
