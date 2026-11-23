//! A response's body that is a body the host itself feeds (CONTRACT.md,
//! "What an Application is handed": the third and fifth bullets).
//!
//! - The request's own body given back unread, `new Response(request.body)`,
//!   is sent by the host itself: the same bytes, the request's stated
//!   `content-length` whatever the code set, and chunked with no
//!   `content-length` when the request stated none. The bytes never enter
//!   the Application's memory: the program's resident memory, sampled every
//!   20 ms while 200 MiB passes, does not rise by more than 64 MiB over what
//!   it was.
//! - Any other response body — one the code made (a string, bytes, a
//!   `ReadableStream` of its own, a `TransformStream`'s output), or one it
//!   began to read and then gave back — is answered whole, as before: at
//!   most 8 MiB; a larger one is the platform's fixed `500`.
//!
//! The sending that goes on after the code has ended, and what ends it
//! early, are in `body_sent_ends.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use blackbox::User;
use blackbox::Vm;
use blackbox::body::{
    BodyEnd, CHUNK, Checksum, Framing, Head, MIB, Raw, assert_framed_truly, memory_of,
    pattern_block, peak_rss_during,
};

/// The fixture Application of this file.
const APP: &str = "body-echo";

/// What the program's resident memory may rise by while a 200 MiB body
/// passes.
const MEMORY_SLACK: u64 = 64 * MIB;

/// What the program holds in memory for a body the code made or read: 8 MiB.
const WHOLE_BODY_LIMIT: u64 = 8 * MIB;

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// A program with the stand-in sign-in and this file's fixture, shared with
/// everyone.
fn vm() -> Vm {
    Vm::with_stand_in(&[(APP, "body-response.js")])
}

/// Sends `blocks` blocks of `block` on a writer thread (chunked when
/// `chunked`) while `read` runs: the two sides of a body have to overlap, or
/// the program's forwarding stalls. The writing is told to stop once `read`
/// is done; what ended it early (the program closing, say) is returned.
fn write_while_reading<T>(
    raw: &mut Raw,
    block: &[u8],
    blocks: usize,
    chunked: bool,
    read: impl FnOnce(&mut Raw) -> T,
) -> (T, Option<String>) {
    let mut writer = raw.split_writer();
    let stop = AtomicBool::new(false);
    let stop = &stop;
    thread::scope(|scope| {
        let writing = scope.spawn(move || {
            for _ in 0..blocks {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                let sent = if chunked {
                    writer.send_chunk(block)
                } else {
                    writer.write(block)
                };
                if let Err(problem) = sent {
                    return Some(problem);
                }
            }
            if chunked && let Err(problem) = writer.finish_chunks() {
                return Some(problem);
            }
            None
        });
        let outcome = read(raw);
        stop.store(true, Ordering::Relaxed);
        let problem = writing.join().expect("the writer's thread ended");
        (outcome, problem)
    })
}

/// Reads the answer's body, counting and checksumming it.
fn checksum_of(raw: &mut Raw, within: Duration) -> (BodyEnd, u64, String) {
    let mut received: u64 = 0;
    let mut checksum = Checksum::new();
    let end = raw.drain_body(within, |bytes| {
        received += u64::try_from(bytes.len()).expect("a piece below usize");
        checksum.take(bytes);
    });
    (end, received, checksum.hex())
}

/// The checksum of `blocks` blocks of `block`, as the client sent them.
fn checksum_of_blocks(block: &[u8], blocks: usize) -> String {
    let mut checksum = Checksum::new();
    for _ in 0..blocks {
        checksum.take(block);
    }
    checksum.hex()
}

/// A GET of a body the code makes, whole: head, bytes, and how it ended.
fn made(vm: &Vm, query: &str) -> (Head, Vec<u8>) {
    let mut raw = Raw::connect(vm.address());
    let (head, body, end) = raw.exchange(
        "GET",
        &format!("/made?{query}"),
        &vm.host_of(APP),
        Some(&bob()),
        None,
        Duration::from_secs(30),
    );
    assert_eq!(end, BodyEnd::Whole, "/made?{query}: the body whole");
    (head, body)
}

/// Gives 200 MiB (`chunked` or with a stated length) through the echo of
/// the unread body, and checks the bytes, the framing and the memory.
fn echo_200_mib(chunked: bool) {
    // About 5 to 15 seconds when the host forwards it.
    let vm = vm();
    let pid = vm.pid();
    let body_bytes: u64 = 200 * MIB;
    let chunk = u64::try_from(CHUNK).expect("64 KiB fits");
    let blocks = usize::try_from(body_bytes / chunk).expect("fits");
    let block = pattern_block(CHUNK);
    let expected = checksum_of_blocks(&block, blocks);

    // The same route first, with a body that is small: whatever the program
    // allocates the first time (a context, a connection) is in the baseline.
    let mut warm = Raw::connect(vm.address());
    let (head, _, end) = warm.exchange(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        Some(&pattern_block(CHUNK)),
        Duration::from_secs(10),
    );
    assert_eq!((head.status, end), (200, BodyEnd::Whole));
    let rss_before = memory_of(pid, "VmRSS");

    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of(APP),
        Some(&bob()),
        if chunked {
            Framing::Chunked
        } else {
            Framing::Length(body_bytes)
        },
        &[],
    );
    let (((head, (end, received, got)), problem), peak) = peak_rss_during(pid, || {
        write_while_reading(&mut raw, &block, blocks, chunked, |raw| {
            let head = raw.read_head(Duration::from_secs(30));
            let body = checksum_of(raw, Duration::from_secs(45));
            (head, body)
        })
    });
    assert_eq!(
        head.status, 200,
        "expected the request echoed (200), got {} - today a 200 MiB body is refused past the old 8 MiB bound",
        head.status
    );
    if chunked {
        assert_eq!(
            head.content_length(),
            None,
            "expected no content-length when the request stated none"
        );
        assert!(
            head.chunked(),
            "expected the body sent chunked when the request stated no length"
        );
    } else {
        assert_eq!(
            head.content_length(),
            Some(body_bytes),
            "expected the request's stated content-length"
        );
    }
    assert_eq!(end, BodyEnd::Whole, "expected the whole body ({problem:?})");
    assert_eq!(received, body_bytes, "expected 200 MiB, got {received}");
    assert_eq!(got, expected, "expected the same bytes back");

    let rose = peak.saturating_sub(rss_before);
    assert!(
        rose <= MEMORY_SLACK,
        "expected the program's resident memory to rise by at most 64 MiB while 200 MiB passed, it rose by {} MiB (from {} MiB to {} MiB)",
        rose / MIB,
        rss_before / MIB,
        peak / MIB
    );
}

#[test]
fn a_200_mib_body_given_back_unread_is_sent_by_the_host_with_the_stated_content_length_and_bytes() {
    echo_200_mib(false);
}

#[test]
fn a_body_given_back_unread_with_no_stated_length_is_sent_chunked_with_no_content_length() {
    echo_200_mib(true);
}

#[test]
fn the_platform_sets_content_length_itself_whatever_the_code_set() {
    // A second or two. The code sets 5, then 10,000,000, over a 256 KiB
    // body: the answer states the request's length exactly.
    let vm = vm();
    let block = pattern_block(256 * 1024);
    let body_bytes = u64::try_from(block.len()).expect("fits");
    for lie in ["5", "10000000"] {
        let mut raw = Raw::connect(vm.address());
        let (head, body, end) = raw.exchange(
            "POST",
            &format!("/echo-unread?set_length={lie}"),
            &vm.host_of(APP),
            Some(&bob()),
            Some(&block),
            Duration::from_secs(10),
        );
        assert_eq!(head.status, 200, "set_length={lie}: got {}", head.status);
        assert_eq!(
            head.content_length(),
            Some(body_bytes),
            "expected the platform to set the request's stated content-length, whatever the code set (it set {lie})"
        );
        assert_eq!(end, BodyEnd::Whole, "set_length={lie}");
        assert_eq!(
            body, block,
            "set_length={lie}: expected the whole body back"
        );
    }
}

#[test]
fn a_body_the_code_made_is_answered_whole_with_the_length_it_has() {
    // A second or two.
    let vm = vm();
    for kind in ["string", "bytes", "stream", "transform"] {
        let (head, body) = made(&vm, &format!("kind={kind}&size=1048576&chunk=65536"));
        assert_eq!(head.status, 200, "{kind}: got {}", head.status);
        assert_eq!(body.len(), 1_048_576, "{kind}: a body of 1 MiB");
        assert!(
            body.iter().all(|byte| *byte == b'x'),
            "{kind}: expected the body's bytes"
        );
        assert_framed_truly(&head, 1_048_576, kind);
    }
}

#[test]
fn a_body_the_code_made_is_taken_up_to_8_mib_and_a_larger_one_is_the_fixed_500() {
    // A few seconds: eight bodies of 8 MiB and a byte (or less).
    let vm = vm();
    for kind in ["string", "bytes", "stream", "transform"] {
        let (head, body) = made(&vm, &format!("kind={kind}&size={WHOLE_BODY_LIMIT}"));
        assert_eq!(
            head.status, 200,
            "{kind}, exactly 8 MiB: got {}",
            head.status
        );
        assert_eq!(
            u64::try_from(body.len()).expect("fits"),
            WHOLE_BODY_LIMIT,
            "{kind}: exactly 8 MiB is taken whole"
        );

        let (head, body) = made(&vm, &format!("kind={kind}&size={}", WHOLE_BODY_LIMIT + 1));
        assert_eq!(
            head.status, 500,
            "{kind}, 8 MiB and a byte: expected the platform's fixed 500, got {}",
            head.status
        );
        assert!(
            body.len() < 4096 && !body.windows(8).any(|w| w == b"xxxxxxxx"),
            "{kind}: expected the fixed 500's body, not the made body ({} bytes)",
            body.len()
        );
        let records = vm.records_of_id(&head.request_id());
        assert_eq!(
            records.request["status"], 500,
            "{kind}: {}",
            records.request
        );
    }
}

#[test]
fn a_body_the_code_began_to_read_and_gave_back_is_the_rest_whole_and_over_8_mib_the_fixed_500() {
    // A few seconds.
    let vm = vm();

    // 256 KiB: the buffered path gives the rest of the body whole, and the
    // code's own read is no longer part of it.
    let block = pattern_block(256 * 1024);
    let mut raw = Raw::connect(vm.address());
    let (head, got, end) = raw.exchange(
        "POST",
        "/echo-read-then-give-back",
        &vm.host_of(APP),
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
    assert!(first <= block.len(), "the code read {first} bytes");
    assert_eq!(
        got.as_slice(),
        block.get(first..).expect("first within the body"),
        "expected the whole rest of the body, as before"
    );
    assert_framed_truly(&head, u64::try_from(got.len()).expect("fits"), "the rest");

    // 10 MiB: the rest is over 8 MiB, which the platform's `500` answers.
    let big = pattern_block(CHUNK);
    let blocks = usize::try_from(10 * MIB / u64::try_from(CHUNK).expect("fits")).expect("fits");
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-read-then-give-back",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(10 * MIB),
        &[],
    );
    let (head, _) = write_while_reading(&mut raw, &big, blocks, false, |raw| {
        raw.read_head(Duration::from_secs(30))
    });
    assert_eq!(
        head.status, 500,
        "expected the fixed 500 for a body of 10 MiB the code began to read and gave back, got {}",
        head.status
    );
}
