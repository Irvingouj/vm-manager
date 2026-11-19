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

/// Fails unless `run` ended `local-state` (exit code `1`) as CONTRACT.md
/// says of a `.vm/local/` that cannot be used ("The local loop: one request
/// as a test User", the three more ways it ends; "The local run on a busy
/// machine": only a failure of the private Postgres is `postgres-failed`):
/// the message names `.vm/local` and says that removing it starts the local
/// databases anew, and the `error` has no `step` and no `cause`.
fn assert_local_state_not_postgres(run: &blackbox::tool::Run, context: &str) {
    let message = run.json_error(1, "local-state");
    assert!(
        message.contains(".vm/local") && message.to_lowercase().contains("remov"),
        "{context}: expected the message to say that removing .vm/local/ starts the local databases anew, got {message:?}"
    );
    let error = &run.json()["error"];
    for member in ["step", "cause"] {
        assert!(
            error.get(member).is_none(),
            "{context}: expected no `{member}` in a local-state error (not a failure of the private Postgres), got {error:#}"
        );
    }
}

/// A new project whose `.vm/local/<name>` is made by `make` before its
/// first run.
fn project_with(local: &Local, name: &str, make: impl FnOnce(&Path)) -> blackbox::sdk::TempProject {
    let project = local_project(&local.machine, name, &[]);
    let dir = project.dir.join(".vm/local");
    fs::create_dir_all(&dir).unwrap();
    make(&dir);
    project
}

#[test]
fn a_postgres_log_that_is_a_folder_is_local_state() {
    let local = Local::new();
    let project = project_with(&local, "log-folder-board", |dir| {
        fs::create_dir(dir.join("postgres.log")).unwrap();
    });
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    assert_local_state_not_postgres(&run, "postgres.log a folder");
    assert_nothing_left(&local, "postgres.log a folder");
}

#[test]
fn a_postgres_data_folder_that_is_a_file_is_local_state() {
    let local = Local::new();
    let project = project_with(&local, "data-file-board", |dir| {
        fs::write(dir.join("postgres"), "not a folder\n").unwrap();
    });
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    assert_local_state_not_postgres(&run, "postgres a file");
    assert_nothing_left(&local, "postgres a file");
}

#[test]
fn a_data_folder_without_postgres_data_that_cannot_be_made_anew_is_local_state() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let local = Local::new();
    let project = project_with(&local, "data-kept-board", |dir| {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        let kept = dir.join("postgres").join("kept");
        fs::create_dir_all(&kept).unwrap();
        fs::write(kept.join("file"), "cannot be removed\n").unwrap();
    });
    let kept = project.dir.join(".vm/local/postgres/kept");
    if fs::metadata(&kept).unwrap().uid() == 0 {
        eprintln!(
            "this test runs as root, for whom a folder of mode 0500 does not stop a removal: the case cannot be provoked here"
        );
        return;
    }
    // No PG_VERSION: no Postgres data yet. What `kept/` holds cannot be
    // removed, so the data folder cannot be made anew.
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o500)).unwrap();
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    // Given back before anything is asserted, so that the test's folder can
    // be removed whatever the outcome.
    if kept.exists() {
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_local_state_not_postgres(&run, "a data folder that cannot be made anew");
    assert_nothing_left(&local, "a data folder that cannot be made anew");
}

#[test]
fn a_data_folder_left_without_postgres_data_by_a_failed_initdb_that_cannot_be_made_anew_is_local_state()
 {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let real = blackbox::local::local_postgres_bin();
    let probe = Local::new();
    let failing = blackbox::project::stand_ins(
        &probe.machine.root().join("failing-initdb"),
        &[
            ("postgres", &format!("exec '{real}/postgres' \"$@\"")),
            ("initdb", "echo 'a stand-in initdb that fails' >&2\nexit 1"),
            ("pg_ctl", &format!("exec '{real}/pg_ctl' \"$@\"")),
        ],
    );
    let local = Local::new().with_bin(failing.to_str().unwrap());
    let project = local_project(&local.machine, "initdb-failed-board", &[]);
    // A first run whose initdb fails: what a next run needs is left in
    // .vm/local/, and no Postgres data.
    let first = local.request(&project.dir, "/", OUTSIDER, &[]);
    first.json_error(1, "postgres-failed");
    let error = &first.json()["error"];
    assert_eq!(
        error["step"].as_str(),
        Some("initdb"),
        "{}",
        first.describe()
    );
    assert_eq!(
        error["cause"].as_str(),
        Some("ended"),
        "{}",
        first.describe()
    );
    let local_dir = project.dir.join(".vm/local");
    let data = local_dir.join("postgres");
    if !data.exists() {
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(
        !data.join("PG_VERSION").exists(),
        "the harness's own check: no Postgres data after a failed initdb"
    );
    let kept = data.join("kept");
    fs::create_dir_all(&kept).unwrap();
    fs::write(kept.join("file"), "cannot be removed\n").unwrap();
    if fs::metadata(&kept).unwrap().uid() == 0 {
        eprintln!(
            "this test runs as root, for whom a folder of mode 0500 does not stop a removal: the case cannot be provoked here"
        );
        return;
    }
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o500)).unwrap();
    // The second run with the real programs.
    let run = local.request_with_env(
        &project.dir,
        "/",
        OUTSIDER,
        &[],
        &[("VM_LOCAL_POSTGRES_BIN", real.as_str())],
    );
    // Given back before anything is asserted.
    if kept.exists() {
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_local_state_not_postgres(
        &run,
        "a data folder a failed initdb left, that cannot be made anew",
    );
    assert_nothing_left(
        &local,
        "a data folder a failed initdb left, that cannot be made anew",
    );
}

#[test]
fn vm_dev_with_a_postgres_log_that_is_a_folder_is_local_state() {
    let local = Local::new();
    let project = project_with(&local, "dev-log-folder", |dir| {
        fs::create_dir(dir.join("postgres.log")).unwrap();
    });
    let dev = blackbox::dev::Dev::spawn(
        &local,
        &project.dir,
        "dev-log-folder",
        blackbox::dev::free_port(),
        true,
    );
    let run = dev.wait_end(Duration::from_secs(200));
    assert_local_state_not_postgres(&run, "vm dev, postgres.log a folder");
    assert_nothing_left(&local, "vm dev, postgres.log a folder");
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
