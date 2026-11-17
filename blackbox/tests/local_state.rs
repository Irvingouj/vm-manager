//! The project's local state, `.vm/local/`, as an outsider can see it
//! (CONTRACT.md, "The local loop: one request as a test User": "The
//! command", the paragraph of the three more ways it ends; "What a run
//! does", step 2; ticket "Local loop: run the Application on the Builder's
//! machine and send a request as a test User").
//!
//! A new project declaring nothing; each test warms it with one run first,
//! so that the private Postgres's data folder, `.vm/local/postgres/`, is
//! there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use blackbox::local::{Local, OUTSIDER, local_project};
use blackbox::tool::closed_address;

fn warm(local: &Local, dir: &Path) -> String {
    let requested = local.request_ok(dir, "/", OUTSIDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.request_id()
}

fn assert_nothing_left(local: &Local, context: &str) {
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "{context}: expected nothing the run started left running, found {left:#?}"
    );
}

/// A child of the test's own that does nothing for five minutes; killed
/// and reaped when dropped.
struct Bystander(Child);

impl Bystander {
    fn start() -> Self {
        Self(Command::new("sleep").arg("300").spawn().unwrap())
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn still_running(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn a_stale_postmaster_pid_is_not_believed_and_the_process_it_names_is_left_alone() {
    let local = Local::new();
    let project = local_project(&local.machine, "stale-board", &[]);
    warm(&local, &project.dir);
    let data = project.dir.join(".vm/local/postgres");
    assert!(data.is_dir(), "the harness's own check: {}", data.display());
    let mut bystander = Bystander::start();
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // The lines Postgres writes, naming a process that is not a Postgres
    // and a port nothing listens on: as a Postgres that is gone (the
    // machine switched off) leaves it, its number since given to another.
    let port = closed_address().port();
    fs::write(
        data.join("postmaster.pid"),
        format!(
            "{}\n{}\n{started}\n{port}\n\n127.0.0.1\n  1234567         0\nready   \n",
            bystander.pid(),
            data.display()
        ),
    )
    .unwrap();
    let requested = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(
        requested.status(),
        200,
        "expected the stale file not believed and the run to go on\n{}",
        requested.describe()
    );
    assert!(
        bystander.still_running(),
        "expected the process the file named ({}) left alone: no process signalled on the word of a file",
        bystander.pid()
    );
    assert_nothing_left(&local, "after a stale postmaster.pid");
}

#[test]
fn vm_log_and_postgres_log_are_started_anew_by_every_run() {
    let local = Local::new();
    let project = local_project(&local.machine, "log-board", &[]);
    let vm_log = project.dir.join(".vm/local/vm.log");
    let postgres_log = project.dir.join(".vm/local/postgres.log");
    let first = warm(&local, &project.dir);
    let after_first = fs::read_to_string(&vm_log).unwrap();
    assert!(
        after_first.contains(&first),
        "expected the program's log of the run, with the record of its request ({first}), in .vm/local/vm.log\n{after_first}"
    );
    let second = warm(&local, &project.dir);
    let after_second = fs::read_to_string(&vm_log).unwrap();
    assert!(
        after_second.contains(&second),
        "expected the second run's request ({second}) in vm.log\n{after_second}"
    );
    assert!(
        !after_second.contains(&first),
        "expected vm.log started anew: the first run's request ({first}) no longer in it\n{after_second}"
    );
    let words = fs::read_to_string(&postgres_log).unwrap();
    assert_eq!(
        words.matches("starting PostgreSQL").count(),
        1,
        "expected postgres.log started anew: one start in it after two runs\n{words}"
    );
}

#[test]
fn a_vm_local_that_is_not_a_folder_is_local_state() {
    let local = Local::new();
    let project = local_project(&local.machine, "plain-board", &[]);
    fs::create_dir_all(project.dir.join(".vm")).unwrap();
    fs::write(project.dir.join(".vm/local"), "not a folder\n").unwrap();
    let message = local
        .request(&project.dir, "/", OUTSIDER, &[])
        .json_error(1, "local-state");
    assert!(
        message.contains(".vm/local") && message.to_lowercase().contains("remov"),
        "expected the message to say that removing .vm/local/ starts the local databases anew, got {message:?}"
    );
    assert_nothing_left(&local, "local-state");
}

#[test]
fn a_private_postgres_that_does_not_start_is_postgres_failed_naming_its_log() {
    let local = Local::new();
    let project = local_project(&local.machine, "broken-board", &[]);
    warm(&local, &project.dir);
    let mut conf = fs::OpenOptions::new()
        .append(true)
        .open(project.dir.join(".vm/local/postgres/postgresql.conf"))
        .unwrap();
    conf.write_all(b"\nthis is not a setting at all\n").unwrap();
    drop(conf);
    let message = local
        .request(&project.dir, "/", OUTSIDER, &[])
        .json_error(1, "postgres-failed");
    assert!(
        message.contains(".vm/local/postgres.log"),
        "expected the message to name .vm/local/postgres.log, got {message:?}"
    );
    let words = fs::read_to_string(project.dir.join(".vm/local/postgres.log")).unwrap();
    assert!(
        words.contains("postgresql.conf"),
        "expected postgres.log to hold Postgres's own complaint about its configuration\n{words}"
    );
    assert_nothing_left(&local, "postgres-failed");
}
