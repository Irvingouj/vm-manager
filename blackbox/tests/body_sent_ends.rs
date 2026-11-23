//! A body being sent, and what ends it (CONTRACT.md, "What an Application
//! is handed": the third and fourth bullets; "What Platform administrators
//! see and stop: Requests in flight").
//!
//! A body the host itself feeds is sent by the host, and the sending goes on
//! after the code has ended — the request's time and CPU limits bound the
//! code, not the sending. The request's record and the Application's log
//! entry are written when the response's head is decided, not at the body's
//! end. The sending is ended early, the connection closed without a clean
//! end, when the client takes no byte for 30 seconds, when the client goes
//! away, or when the Application is disabled within its second. The limit on
//! one connection's life does not cut a body being sent.
//!
//! Every slow test says how long it takes; none waits longer than 45
//! seconds. A body being sent can only be "closed without a clean end" in a
//! way the client sees if the program had more to send than the two ends'
//! socket buffers hold (about 10 MB on loopback), so the bodies that are cut
//! are 64 MiB.
//!
//! The limit on one connection's life has no number in the contract, so no
//! test pins it by itself: the 30-second test below is the only one that
//! can say a body is not cut by it, and only for limits under that.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::admin::{self, IN_FLIGHT_ALLOWED, administrator};
use blackbox::body::{
    BodyEnd, CHUNK, Checksum, Framing, MIB, Peeked, Raw, pattern_block, sockets_open_on,
};
use blackbox::logs::wait_for_entry;
use blackbox::{INSTALLER, Vm, fixture_bundle};

/// The fixture Application of this file.
const APP: &str = "body-echo";

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

fn installer() -> User {
    User::new(INSTALLER)
}

/// A program with the stand-in sign-in and this file's fixture, shared with
/// everyone.
fn vm() -> Vm {
    Vm::with_stand_in(&[(APP, "body-response.js")])
}

/// The checksum of `blocks` blocks of `block`.
fn checksum_of_blocks(block: &[u8], blocks: usize) -> String {
    let mut checksum = Checksum::new();
    for _ in 0..blocks {
        checksum.take(block);
    }
    checksum.hex()
}

/// Waits until the connection has bytes buffered, taking none.
fn wait_for_buffered(raw: &mut Raw, within: Duration) {
    let began = Instant::now();
    while raw.peek() != Peeked::Data {
        assert!(
            began.elapsed() < within,
            "expected bytes of the body in the socket within {within:?} while the client took none"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_sending_goes_on_after_the_code_has_ended_for_longer_than_the_requests_30_seconds() {
    // This test takes about 35 seconds: the client reads 64 MiB of a body
    // 64 KiB at a time, 30 ms apart, so the sending runs for more than the
    // 30 seconds a request's code gets, while a byte arrives every 30 ms
    // (never the 30 seconds without one that would end it early).
    let vm = vm();
    let body_bytes: u64 = 64 * MIB;
    let chunk = u64::try_from(CHUNK).expect("64 KiB fits");
    let blocks = usize::try_from(body_bytes / chunk).expect("fits");
    let block = pattern_block(CHUNK);
    let expected = checksum_of_blocks(&block, blocks);

    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(body_bytes),
        &[],
    );
    let mut writer = raw.split_writer();
    let stop = AtomicBool::new(false);
    let stop = &stop;
    let (head, took, end, received, got) = thread::scope(|scope| {
        let writing = scope.spawn(move || {
            for _ in 0..blocks {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                if let Err(problem) = writer.write(&block) {
                    return Some(problem);
                }
            }
            None
        });
        let head = raw.read_head(Duration::from_secs(10));
        let id = head.request_id();
        // The request's record and the Application's log entry are written
        // when the head is decided: they are here while the body is still
        // being sent and not one byte of it has been read.
        let records = vm.records_of_id(&id);
        assert_eq!(
            records.request["status"], 200,
            "expected the `request` record written at the head, {:?}",
            records.request
        );
        let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &id);
        assert_eq!(
            entry["status"], 200,
            "expected the log entry written at the head, {entry}"
        );

        let began = Instant::now();
        let mut got = Checksum::new();
        let mut received: u64 = 0;
        let end = raw.drain_body(Duration::from_secs(45), |bytes| {
            received += u64::try_from(bytes.len()).expect("a piece below usize");
            got.take(bytes);
            // Slow, but never a stall: a byte every 30 ms.
            thread::sleep(Duration::from_millis(30));
        });
        let took = began.elapsed();
        stop.store(true, Ordering::Relaxed);
        let problem = writing.join().expect("the writer's thread ended");
        assert!(
            problem.is_none(),
            "expected the request's body written whole, got {problem:?}"
        );
        (head, took, end, received, got.hex())
    });
    assert_eq!(
        head.status, 200,
        "expected the request echoed (200), got {} — today a 64 MiB body is refused past the old 8 MiB bound",
        head.status
    );
    assert_eq!(head.content_length(), Some(body_bytes));
    assert_eq!(end, BodyEnd::Whole, "expected the whole body");
    assert_eq!(received, body_bytes, "expected 64 MiB, got {received}");
    assert_eq!(got, expected, "expected the same bytes back");
    assert!(
        took > Duration::from_secs(30),
        "expected the sending to take longer than the request's 30 seconds, it took {took:?}"
    );
}

#[test]
fn a_client_that_takes_no_byte_for_30_seconds_gets_the_connection_closed_short_of_content_length() {
    // About 31 to 38 seconds: the client takes no byte of a 64 MiB body
    // after the head. The program's closing is seen without taking a byte
    // (`peer_closed`: the kernel's table says the program's end is closed,
    // which holds for a clean end behind buffered bytes and for a reset
    // alike), so both the earliest and the latest time are measured: not
    // before 25 s, not after 38.
    let vm = vm();
    let body_bytes: u64 = 64 * MIB;
    let chunk = u64::try_from(CHUNK).expect("64 KiB fits");
    let blocks = usize::try_from(body_bytes / chunk).expect("fits");
    let block = pattern_block(CHUNK);

    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(body_bytes),
        &[],
    );
    let mut writer = raw.split_writer();
    let stop = AtomicBool::new(false);
    let stop = &stop;
    let (head, closed_after, end, received) = thread::scope(|scope| {
        let writing = scope.spawn(move || {
            // The request's body feeds the response's; the forwarding
            // blocks as soon as the client stops taking bytes, and this
            // thread blocks with it (its write fails after 10 s or when the
            // program closes).
            for _ in 0..blocks {
                if stop.load(Ordering::Relaxed) || writer.write(&block).is_err() {
                    return;
                }
            }
        });
        let head = raw.read_head(Duration::from_secs(10));
        let stalled_from = Instant::now();
        wait_for_buffered(&mut raw, Duration::from_secs(5));
        // From here on the client takes no byte.
        let closed = raw.wait_closed_without_taking(Duration::from_secs(38));
        let closed_after = closed.map(|_| stalled_from.elapsed());
        let mut received: u64 = 0;
        let end = raw.drain_body(Duration::from_secs(5), |bytes| {
            received += u64::try_from(bytes.len()).expect("a piece below usize");
        });
        stop.store(true, Ordering::Relaxed);
        writing.join().expect("the writer's thread ended");
        (head, closed_after, end, received)
    });
    assert_eq!(
        head.status, 200,
        "expected the request echoed (200), got {}",
        head.status
    );
    assert_eq!(head.content_length(), Some(body_bytes));
    let closed_after = closed_after
        .expect("expected the program to close the connection within 38 s of the client taking no byte (the contract says 30)");
    assert!(
        closed_after >= Duration::from_secs(25),
        "the connection was closed {closed_after:?} after the client stopped taking bytes; the contract gives 30 seconds, not fewer than 25"
    );
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the connection closed with the body short of its content-length, got {end:?}"
    );
    assert!(
        received < body_bytes,
        "expected a body short of its {body_bytes} bytes, got {received}"
    );
}

#[test]
fn a_disable_while_a_body_is_being_sent_closes_the_connection_within_a_second_short_of_content_length()
 {
    // About 3 seconds: the disable comes as soon as the echo's head is read
    // and some of its 64 MiB body is in the socket. After the disable's 200
    // the client drains at full speed, which can only end `ClosedShort`
    // within the time the contract gives: a program that did not cut the
    // body would go on sending it to its end.
    let vm = Vm::start(&admin::options());
    let alice = User::new("alice");
    vm.api_as(&alice)
        .create_with_bundle(APP, &fixture_bundle("body-response.js"));
    let body_bytes: u64 = 64 * MIB;
    let chunk = u64::try_from(CHUNK).expect("64 KiB fits");
    let blocks = usize::try_from(body_bytes / chunk).expect("fits");
    let block = pattern_block(CHUNK);

    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(body_bytes),
        &[],
    );
    let mut writer = raw.split_writer();
    let stop = AtomicBool::new(false);
    let stop = &stop;
    let (head, end, received, took) = thread::scope(|scope| {
        let writing = scope.spawn(move || {
            for _ in 0..blocks {
                if stop.load(Ordering::Relaxed) || writer.write(&block).is_err() {
                    return;
                }
            }
        });
        let head = raw.read_head(Duration::from_secs(10));
        assert_eq!(
            head.status, 200,
            "expected the request echoed (200), got {}",
            head.status
        );
        // Some of the body is in the socket, unread: the sending has begun.
        wait_for_buffered(&mut raw, Duration::from_secs(5));

        let answer = admin::disable(&vm, &administrator(), APP);
        assert_eq!(answer.status, 200, "{}", answer.describe());
        let disabled_at = Instant::now();
        // The close is a reset: a client that takes no byte sees the end as
        // soon as one that reads ("a body being sent is ended early ..."),
        // within the same second.
        let seen_without_taking = raw.wait_closed_without_taking(IN_FLIGHT_ALLOWED);
        assert!(
            seen_without_taking.is_some(),
            "expected a client that takes no byte to see the connection closed within {IN_FLIGHT_ALLOWED:?} of the disable's 200"
        );
        let mut received: u64 = 0;
        let end = raw.drain_body(Duration::from_secs(15), |bytes| {
            received += u64::try_from(bytes.len()).expect("a piece below usize");
        });
        let took = disabled_at.elapsed();
        stop.store(true, Ordering::Relaxed);
        writing.join().expect("the writer's thread ended");
        (head, end, received, took)
    });
    assert_eq!(head.content_length(), Some(body_bytes));
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the connection closed with the body short of its content-length, got {end:?} after {took:?}"
    );
    assert!(
        received < body_bytes,
        "expected a body short of its {body_bytes} bytes, got {received}"
    );
    assert!(
        took <= IN_FLIGHT_ALLOWED,
        "expected the body ended within 1 s of the disable's 200 ({IN_FLIGHT_ALLOWED:?} allowed on a loaded machine), it took {took:?}"
    );

    // "A request whose response's head had already been sent ... keeps the
    // entry and the record written then, with the status its head carried.
    // It is counted in `usage`." (CONTRACT.md, "Requests in flight")
    let id = head.request_id();
    let records = vm.records_of_id(&id);
    assert_eq!(
        records.request["status"], 200,
        "expected the `request` record to keep the head's status, {:?}",
        records.request
    );
    let (entry, _) = wait_for_entry(&vm.api_as(&alice), APP, &id);
    assert_eq!(
        entry["status"], 200,
        "expected the log entry to keep the head's status (not the disabled 503), {entry}"
    );
    admin::wait_for_usage(&vm, APP, 1, 1);
}

#[test]
fn a_client_that_goes_away_ends_the_sending_and_the_program_closes_its_connection() {
    // About 3 seconds. The client reads the head of a 64 MiB echo (more
    // than the socket buffers hold, so the program is mid-send), then goes
    // away. The program must close its end: its sockets on its port are
    // counted from the kernel's table, against what they were before.
    let vm = vm();
    let port = vm.address().port();
    vm.get_as(&bob(), APP, "/text?body=warm").expect_status(200);
    let before = sockets_open_on(port);

    let body_bytes: u64 = 64 * MIB;
    let chunk = u64::try_from(CHUNK).expect("64 KiB fits");
    let blocks = usize::try_from(body_bytes / chunk).expect("fits");
    let block = pattern_block(CHUNK);
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(body_bytes),
        &[],
    );
    let mut writer = raw.split_writer();
    let stop = AtomicBool::new(false);
    let stop = &stop;
    let id = thread::scope(|scope| {
        let writing = scope.spawn(move || {
            for _ in 0..blocks {
                if stop.load(Ordering::Relaxed) || writer.write(&block).is_err() {
                    return;
                }
            }
        });
        let head = raw.read_head(Duration::from_secs(10));
        assert_eq!(
            head.status, 200,
            "expected the request echoed (200), got {}",
            head.status
        );
        wait_for_buffered(&mut raw, Duration::from_secs(5));
        let during = sockets_open_on(port);
        assert!(
            during > before,
            "expected the program to hold the connection while it sends ({during} sockets, {before} before)"
        );
        // The client goes away while its body is being sent.
        raw.go_away();
        stop.store(true, Ordering::Relaxed);
        writing.join().expect("the writer's thread ended");
        head.request_id()
    });

    let began = Instant::now();
    while sockets_open_on(port) > before {
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "expected the program to close the connection of a client that went away within 10 s ({} sockets, {before} before)",
            sockets_open_on(port)
        );
        thread::sleep(Duration::from_millis(100));
    }

    // Its request was recorded when the head was decided, and the program
    // serves the next request.
    let records = vm.records_of_id(&id);
    assert_eq!(
        records.request["status"], 200,
        "expected the `request` record written at the head, {:?}",
        records.request
    );
    let after = vm.get_as(&bob(), APP, "/text?body=after");
    after.expect_status(200);
    assert_eq!(after.body(), b"after");
}
