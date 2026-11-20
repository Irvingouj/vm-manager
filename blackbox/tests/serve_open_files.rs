//! The program has the open files its bounds need (CONTRACT.md, "Serving":
//! "The open files it needs"): started under a soft limit of 1,024 open
//! files, as a shell, a systemd service or a container under the Docker
//! daemon's default starts it, `vibe serve` raises its own soft limit
//! towards the hard one, says so in an `open-files-limit` record, and then
//! serves its bound of 1,024 connections at once with no `accept-failed`.
//!
//! A file of its own, with one test: it lowers this process's soft limit to
//! 1,024 (with `prlimit`, util-linux) so that the program it starts
//! inherits it, then raises it again for itself, which holds over 1,000
//! sockets. It fails in words when the hard limit here is below 4,096.
//!
//! Linux only: the limits are read from `/proc/<pid>/limits`.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::net::TcpStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::own_tls::{
    ensure_open_files, open_files_limits, open_files_limits_of, start_on_a_port,
};
use blackbox::program::run_to_exit;
use blackbox::vm::VmOptions;

/// The connections the program serves at once.
const BOUND: usize = 1024;

/// The open files the program's bounds need (and this test's own).
const NEEDED: u64 = 4096;

/// The soft limit a program is often started with.
const STARTED_WITH: u64 = 1024;

/// Held connections are opened this many at a time, with [`PAUSE`]
/// between, so that the listening queue is not overfilled.
const BATCH: usize = 100;
const PAUSE: Duration = Duration::from_millis(50);

/// Sets this process's soft limit of open files to `soft`, the hard one
/// left as it is.
fn set_own_soft_limit(soft: u64) {
    let (_, hard) = open_files_limits().unwrap_or_else(|p| panic!("{p}"));
    let pid = std::process::id().to_string();
    let mut command = Command::new("prlimit");
    command.args([
        "--pid",
        &pid,
        &format!(
            "--nofile={soft}:{}",
            hard.map_or("unlimited".to_owned(), |h| h.to_string())
        ),
    ]);
    let finished = run_to_exit(command, Duration::from_secs(10))
        .unwrap_or_else(|p| panic!("this test sets its soft limit of open files with `prlimit` (util-linux), which could not be run: {p}"));
    let (now, _) = open_files_limits().unwrap_or_else(|p| panic!("{p}"));
    assert!(
        finished.status.is_some_and(|s| s.success()) && now == Some(soft),
        "`prlimit --pid {pid} --nofile={soft}:...` did not set the soft limit of open files (now {now:?}): {}",
        finished.stderr.trim()
    );
}

#[test]
fn started_under_a_soft_limit_of_1024_files_the_program_raises_it_and_serves_its_bound() {
    let (_, hard) = open_files_limits().unwrap_or_else(|p| panic!("{p}"));
    assert!(
        hard.is_none_or(|h| h >= NEEDED),
        "this test needs a hard limit of open files of {NEEDED} or more, it is {hard:?} here: raise it (`ulimit -Hn {NEEDED}` as root, or `nofile` in /etc/security/limits.conf) and run it again"
    );

    // Started under a soft limit of 1,024, as an installation often is.
    set_own_soft_limit(STARTED_WITH);
    let vm = start_on_a_port(&VmOptions::new().stand_in_sign_in(), false);
    // This test's own sockets.
    ensure_open_files(NEEDED);

    // It says what it found and what it made of it.
    let records = vm.records();
    let said: Vec<_> = records
        .iter()
        .filter(|r| r["event"] == "open-files-limit")
        .collect();
    assert_eq!(
        said.len(),
        1,
        "expected one `open-files-limit` record, found {said:?}"
    );
    let said = said[0];
    assert_eq!(
        said["raised_from"].as_u64(),
        Some(STARTED_WITH),
        "expected `raised_from: {STARTED_WITH}` in {said}"
    );
    assert!(
        said["soft"].as_u64().is_some_and(|s| s >= NEEDED),
        "expected a soft limit of at least {NEEDED} in {said}"
    );
    assert_eq!(
        said["hard"].as_u64(),
        hard,
        "expected the hard limit in {said}"
    );
    assert!(
        !records.iter().any(|r| r["event"] == "open-files-limit-low"),
        "expected no `open-files-limit-low` with a hard limit of {hard:?}"
    );
    // And it is so.
    let (soft, now_hard) = open_files_limits_of(vm.pid()).unwrap_or_else(|p| panic!("{p}"));
    assert!(
        soft.is_none_or(|s| s >= NEEDED) && now_hard == hard,
        "expected the program's /proc/<pid>/limits to say a soft limit of at least {NEEDED} and the hard limit {hard:?}, it says {soft:?} {now_hard:?}"
    );

    // Its bound of connections at once, all but one held open sending
    // nothing, and a request on the last: more open files than 1,024.
    let address = vm.address();
    let began = Instant::now();
    let mut held = Vec::with_capacity(BOUND - 1);
    for n in 0..BOUND - 1 {
        if n > 0 && n % BATCH == 0 {
            thread::sleep(PAUSE);
        }
        held.push(
            TcpStream::connect_timeout(&address, Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("could not open held connection {n}: {e}")),
        );
    }
    eprintln!(
        "{} connections that send nothing opened in {:?}",
        held.len(),
        began.elapsed()
    );
    let answer = vm
        .api_request("/v1/installation")
        .timeout(Duration::from_secs(5))
        .try_send()
        .unwrap_or_else(|e| {
            vm.fail(&format!(
                "expected `GET /v1/installation` answered while {} connections are held open (the program's bound is {BOUND}): {e}",
                held.len()
            ))
        });
    assert_eq!(answer.status, 200, "{answer:?}");
    eprintln!(
        "`GET /v1/installation` answered {:?} after the held connections began",
        began.elapsed()
    );
    // Anything accept-failed would have said by now.
    thread::sleep(Duration::from_millis(500));
    let failed: Vec<_> = vm
        .records()
        .into_iter()
        .filter(|r| r["event"] == "accept-failed")
        .collect();
    assert!(
        failed.is_empty(),
        "expected no `accept-failed` while serving its bound, found {failed:?}"
    );
    drop(held);
}
