//! What a local run starts, it stops, however it ends (CONTRACT.md, "The
//! local loop: one request as a test User": "The command", "What a run
//! does", steps 1 to 3; ticket "Local loop: run the Application on the
//! Builder's machine and send a request as a test User": nothing needs
//! Docker, administrator rights or anything installed by hand).
//!
//! The Postgres of a run is seen from outside through the `postmaster.pid`
//! under the project's `.vm/local/` (its process, its data folder, its port
//! and address), and every process a run started is found again from
//! `/proc` (`blackbox/src/local.rs`, `Local::processes`). The project is a
//! new one declaring nothing, whose `/slow?ms=<n>` answers after `n`
//! milliseconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::generate::{Generated, generate, listed};
use blackbox::local::{
    Local, OUTSIDER, Postmaster, Requested, handler, local_project, port_open, wait_for_postmaster,
};
use blackbox::project::{NOTES_SCHEMA_TS, Tree, is_running};
use blackbox::tool::{Run, mode_of};

/// How long a test waits for a run's Postgres to listen: a first run makes
/// its data folder first.
const POSTMASTER_WAIT: Duration = Duration::from_secs(120);

/// Fails unless nothing a run started is running, its Postgres is gone,
/// its port is closed and its folder in `TMPDIR` is removed.
fn assert_nothing_left(local: &Local, postmaster: Option<&Postmaster>, context: &str) {
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "{context}: expected no process the run started still running, found {left:#?}"
    );
    if let Some(postmaster) = postmaster {
        assert!(
            !is_running(postmaster.pid),
            "{context}: expected the run's Postgres ({}) stopped",
            postmaster.pid
        );
        assert!(
            !port_open(postmaster.port),
            "{context}: expected the port {} closed",
            postmaster.port
        );
    }
    assert_eq!(
        listed(&local.tmp()),
        Vec::<String>::new(),
        "{context}: expected the run's own folder in TMPDIR removed"
    );
}

/// A run of `/` as outsider that must answer 200: so that the project's
/// local state is made before a test times something.
fn warm(local: &Local, dir: &Path) {
    let requested = local.request_ok(dir, "/", OUTSIDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
}

#[test]
fn a_run_listens_on_loopback_only_and_stops_everything_it_started_when_it_ends() {
    let local = Local::new();
    let project = local_project(&local.machine, "slow-board", &[]);
    let mut running = local.spawn_request(&project.dir, "/slow?ms=3000", OUTSIDER, &[]);
    let postmaster = wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT)
        .unwrap_or_else(|| panic!("expected the run's Postgres to listen, under .vm/local/"));
    assert!(
        matches!(
            postmaster.listen.as_str(),
            "127.0.0.1" | "::1" | "localhost"
        ),
        "expected the private Postgres to listen on a loopback address only, got {:?}",
        postmaster.listen
    );
    let data = fs::canonicalize(&postmaster.data).unwrap();
    assert!(
        data.starts_with(fs::canonicalize(project.dir.join(".vm/local")).unwrap()),
        "expected the data of the private Postgres in the project's .vm/local/, got {}",
        data.display()
    );
    assert_eq!(
        mode_of(&data),
        0o700,
        "expected the data folder of mode 0700"
    );
    let inside = listed(&local.tmp());
    assert_eq!(
        inside.len(),
        1,
        "expected one folder of the run's own in TMPDIR while it runs, got {inside:?}"
    );
    assert_eq!(mode_of(&local.tmp().join(&inside[0])), 0o700);
    let run = running.wait(Duration::from_secs(120));
    let requested = Requested::of(&run);
    assert_eq!(requested.text(), "slow");
    assert_nothing_left(&local, Some(&postmaster), "a run that answered");
    // .vm/local/ is git's to ignore.
    let ignored = fs::read_to_string(project.dir.join(".gitignore")).unwrap();
    assert!(
        ignored
            .lines()
            .any(|l| l.trim() == ".vm/*" || l.trim() == ".vm/local/" || l.trim() == ".vm/"),
        "expected .vm/local/ ignored by git\n{ignored}"
    );
}

#[test]
fn a_run_that_fails_after_starting_its_postgres_leaves_nothing_running() {
    let local = Local::new();
    let project = local_project(&local.machine, "failing-board", &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    let made = Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    fs::write(project.path(&made.file), "SELECT 1 / 0;\n").unwrap();
    let run = local.request(&project.dir, "/notes", OUTSIDER, &[]);
    run.json_error(1, "migration-failed");
    assert_nothing_left(&local, None, "a run that failed");
}

#[test]
fn out_of_time_the_run_ends_request_timed_out_and_stops_what_it_started() {
    let local = Local::new();
    let project = local_project(&local.machine, "late-board", &[]);
    warm(&local, &project.dir);
    let began = Instant::now();
    let mut running = local.spawn_request(
        &project.dir,
        "/slow?ms=60000",
        OUTSIDER,
        &["--timeout", "15"],
    );
    let postmaster = wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT);
    let run = running.wait(Duration::from_secs(90));
    let message = run.json_error(1, "request-timed-out");
    assert!(!message.trim().is_empty());
    assert!(
        began.elapsed() < Duration::from_secs(45),
        "expected the run to end soon after its 15 seconds, took {:?}",
        began.elapsed()
    );
    assert_nothing_left(&local, postmaster.as_ref(), "a run out of time");
}

#[test]
fn an_interrupted_run_ends_interrupted_and_stops_what_it_started() {
    let local = Local::new();
    let project = local_project(&local.machine, "stopped-board", &[]);
    warm(&local, &project.dir);
    for signal in ["INT", "TERM"] {
        let mut running = local.spawn_request(&project.dir, "/slow?ms=60000", OUTSIDER, &[]);
        let postmaster = wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT)
            .unwrap_or_else(|| panic!("{signal}: expected the run's Postgres to listen"));
        thread::sleep(Duration::from_millis(1000));
        let sent = Instant::now();
        running.signal(signal);
        let run = running.wait(Duration::from_secs(60));
        run.json_error(1, "interrupted");
        assert!(
            sent.elapsed() < Duration::from_secs(20),
            "{signal}: expected the run to end within seconds of the signal, took {:?}",
            sent.elapsed()
        );
        assert_nothing_left(&local, Some(&postmaster), signal);
    }
}

#[test]
fn a_postgres_left_by_a_run_killed_outright_is_stopped_and_started_anew_by_the_next_run() {
    let local = Local::new();
    let project = local_project(&local.machine, "killed-board", &[]);
    warm(&local, &project.dir);
    let mut running = local.spawn_request(&project.dir, "/slow?ms=60000", OUTSIDER, &[]);
    let left = wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT)
        .unwrap_or_else(|| panic!("expected the run's Postgres to listen"));
    thread::sleep(Duration::from_millis(500));
    running.signal("KILL");
    let killed: Run = running.wait(Duration::from_secs(30));
    assert_eq!(
        killed.code, None,
        "the harness's own check: killed outright"
    );
    // Whatever it left, the next run stops and starts anew.
    let next = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(next.status(), 200, "{}", next.describe());
    assert!(
        !is_running(left.pid),
        "expected the Postgres left by the killed run ({}) stopped by the next run",
        left.pid
    );
    let still = local.processes_after(Duration::from_secs(5));
    assert!(
        still.is_empty(),
        "expected nothing running after the next run, found {still:#?}"
    );
}

#[test]
fn a_second_local_command_in_the_same_project_waits_for_the_first_and_both_succeed() {
    let local = Local::new();
    let project = local_project(&local.machine, "shared-board", &[]);
    warm(&local, &project.dir);
    let mut running = local.spawn_request(&project.dir, "/slow?ms=6000", OUTSIDER, &[]);
    wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT)
        .unwrap_or_else(|| panic!("expected the first run's Postgres to listen"));
    thread::sleep(Duration::from_millis(500));
    let second = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(second.status(), 200, "{}", second.describe());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !running.has_ended() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        running.has_ended(),
        "expected the second command to have waited for the first to end"
    );
    let first = Requested::of(&running.wait(Duration::from_secs(10)));
    assert_eq!(first.text(), "slow");
    assert_nothing_left(&local, None, "two runs, one after the other");
}

#[test]
fn a_second_command_that_waits_past_its_own_timeout_says_another_local_command_is_running() {
    let local = Local::new();
    let project = local_project(&local.machine, "busy-board", &[]);
    warm(&local, &project.dir);
    let mut running = local.spawn_request(&project.dir, "/slow?ms=60000", OUTSIDER, &[]);
    wait_for_postmaster(&project.dir, &mut running, POSTMASTER_WAIT)
        .unwrap_or_else(|| panic!("expected the first run's Postgres to listen"));
    let began = Instant::now();
    let second = local.request(&project.dir, "/", OUTSIDER, &["--timeout", "3"]);
    let message = second.json_error(1, "request-timed-out");
    assert!(
        message.to_lowercase().contains("another"),
        "expected the message to say another local command of this project is running, got {message:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "expected the second command to give up at its own timeout"
    );
    running.signal("TERM");
    running.wait(Duration::from_secs(60));
    assert_nothing_left(&local, None, "after both");
}

#[test]
fn nothing_is_written_outside_the_users_folder_the_project_and_tmpdir() {
    let local = Local::new();
    let project = local_project(&local.machine, "tidy-board", &[]);
    let before = Tree::of(local.machine.root());
    warm(&local, &project.dir);
    let outside: Vec<String> = before
        .differences(&Tree::of(local.machine.root()))
        .into_iter()
        .filter(|line| {
            !["data", "tmp"].iter().any(|kept| {
                line.starts_with(&format!("{kept}/")) || line.starts_with(&format!("{kept}:"))
            })
        })
        .collect();
    assert_eq!(
        outside,
        Vec::<String>::new(),
        "expected nothing written on the machine but in the user's data folder and TMPDIR (the project is elsewhere)"
    );
}
