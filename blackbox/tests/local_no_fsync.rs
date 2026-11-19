//! The local databases do not wait on the disk (CONTRACT.md, "The local run
//! on a busy machine": "The local databases do not wait on the disk", and
//! the bullet on which failures are `postgres-failed`; ticket "The local
//! run's private Postgres passes its time limits under the gate's load, and
//! the refusal does not say which limit").
//!
//! The settings are asked of the running server itself (`SHOW fsync`, `SHOW
//! full_page_writes`), not read from a command line or a file: the harness
//! connects to the private Postgres while a run holds it, on the port its
//! `postmaster.pid` names, as the superuser the run made, with the password
//! the run gave `initdb` (a stand-in `initdb` of `VM_LOCAL_POSTGRES_BIN`
//! keeps a copy of the password file, then runs the real one; `postgres`
//! and `pg_ctl` are the real ones).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use blackbox::dev::Dev;
use blackbox::generate::{Generated, generate};
use blackbox::local::{
    Local, OUTSIDER, Postmaster, Requested, handler, local_postgres_bin, local_project,
    wait_for_postmaster,
};
use blackbox::project::{NOTES_SCHEMA_TS, is_running, stand_ins};
use blackbox::sdk::TempProject;
use blackbox::sql::Sql;

/// How long a test waits for a run's Postgres to listen: a first run makes
/// its data folder first.
const POSTMASTER_WAIT: Duration = Duration::from_secs(120);

/// The role `initdb` is given by the run (`-U`), as the run starts it.
const SUPERUSER: &str = "vm_superuser";

/// A Builder's machine whose runs get the harness's Postgres through a
/// folder of stand-ins, `initdb` keeping a copy of the password it is
/// given.
struct Machine {
    local: Local,
    password_copy: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let local = Local::new();
        let root = local.machine.root().to_path_buf();
        let real = local_postgres_bin();
        let password_copy = root.join("password-given");
        let initdb = format!(
            "for a in \"$@\"; do\n  case \"$a\" in\n    --pwfile=*) cp \"${{a#--pwfile=}}\" '{}' ;;\n  esac\ndone\nexec '{real}/initdb' \"$@\"",
            password_copy.display()
        );
        let postgres = format!("exec '{real}/postgres' \"$@\"");
        let pg_ctl = format!("exec '{real}/pg_ctl' \"$@\"");
        let bin = stand_ins(
            &root.join("bin"),
            &[
                ("postgres", postgres.as_str()),
                ("initdb", initdb.as_str()),
                ("pg_ctl", pg_ctl.as_str()),
            ],
        );
        let local = local.with_bin(bin.to_str().unwrap());
        Self {
            local,
            password_copy,
        }
    }

    fn password(&self) -> String {
        fs::read_to_string(&self.password_copy)
            .unwrap_or_else(|e| {
                panic!(
                    "the harness's own check: the stand-in initdb kept no password file at {}: {e}",
                    self.password_copy.display()
                )
            })
            .trim()
            .to_owned()
    }

    /// `fsync` and `full_page_writes` as the running private Postgres says
    /// them.
    fn settings(&self, postmaster: &Postmaster) -> (String, String) {
        let password = self.password().replace('\\', "\\\\").replace('\'', "\\'");
        let config = format!(
            "host=127.0.0.1 port={} user={SUPERUSER} password='{password}' dbname=postgres connect_timeout=30",
            postmaster.port
        );
        // A Postgres that listens may still be starting up (57P03): asked
        // again, for at most 30 s. The error shown is its SQLSTATE and the
        // server's words, never the connection's settings.
        let deadline = Instant::now() + Duration::from_secs(30);
        let sql = loop {
            match Sql::connect_coded(&config) {
                Ok(sql) => break sql,
                Err(e) if e.code == "57P03" && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => panic!(
                    "the harness's own check: cannot connect to the private Postgres on port {} as {SUPERUSER}: {} ({})",
                    postmaster.port, e.message, e.code
                ),
            }
        };
        let fsync = sql.value("SHOW fsync").unwrap_or_default();
        let full_page_writes = sql.value("SHOW full_page_writes").unwrap_or_default();
        (fsync, full_page_writes)
    }

    fn assert_settings(&self, postmaster: &Postmaster, context: &str) {
        let (fsync, full_page_writes) = self.settings(postmaster);
        assert_eq!(
            fsync, "off",
            "{context}: expected the private Postgres to run with fsync off"
        );
        assert_eq!(
            full_page_writes, "off",
            "{context}: expected the private Postgres to run with full_page_writes off"
        );
    }
}

/// A new project whose `/notes` writes a note in the own database on
/// `POST` (the body) and lists the notes.
fn notes_project(local: &Local, name: &str) -> TempProject {
    let project = local_project(&local.machine, name, &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    project
}

/// The bodies of the notes `/notes` lists.
fn note_bodies(requested: &Requested) -> Vec<String> {
    requested.json()["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of notes\n{}", requested.describe()))
        .iter()
        .map(|n| n["body"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// While a `vm request` holds the project's private Postgres (a slow
/// request), its settings; then the run is waited for.
fn settings_during_a_request(machine: &Machine, dir: &Path, context: &str) {
    let mut running = machine
        .local
        .spawn_request(dir, "/slow?ms=6000", OUTSIDER, &[]);
    let postmaster = wait_for_postmaster(dir, &mut running, POSTMASTER_WAIT)
        .unwrap_or_else(|| panic!("{context}: expected the run's Postgres to listen"));
    machine.assert_settings(&postmaster, context);
    let run = running.wait(Duration::from_secs(240));
    let requested = Requested::of(&run);
    assert_eq!(requested.text(), "slow", "{context}");
}

#[test]
fn the_private_postgres_of_vm_request_runs_with_fsync_and_full_page_writes_off() {
    let machine = Machine::new();
    let project = local_project(&machine.local.machine, "fsync-request", &[]);
    settings_during_a_request(&machine, &project.dir, "a first run");
    // The private Postgres the run before made is started the same way.
    settings_during_a_request(&machine, &project.dir, "a second run");
}

#[test]
fn the_private_postgres_of_vm_dev_runs_with_fsync_and_full_page_writes_off() {
    let machine = Machine::new();
    let project = local_project(&machine.local.machine, "fsync-dev", &[]);
    let dev = Dev::start(&machine.local, &project.dir, "fsync-dev");
    machine.assert_settings(&dev.postmaster(), "vm dev, a first run");
    dev.stop("INT").expect_code(0);
    // Made by `vm dev` before, started again by `vm dev`.
    let dev = Dev::start(&machine.local, &project.dir, "fsync-dev");
    machine.assert_settings(&dev.postmaster(), "vm dev, again");
    dev.stop("INT").expect_code(0);
}

#[test]
fn rows_written_are_there_after_vm_dev_and_its_postgres_are_killed_outright() {
    let machine = Machine::new();
    let local = &machine.local;
    let project = notes_project(local, "killed-notes");
    let dev = Dev::start(local, &project.dir, "killed-notes");
    let postgres = dev.postmaster();
    // Written through `vm dev` (a `vm request` it answers), so that the
    // Postgres that holds it has not been stopped since.
    let written = Requested::of(&local.request(
        &project.dir,
        "/notes",
        OUTSIDER,
        &["--data", "kept through a kill"],
    ));
    assert_eq!(
        note_bodies(&written),
        ["kept through a kill"],
        "the harness's own check: the note written\n{}",
        written.describe()
    );
    // At once, as a crash would come: a commit that was answered is
    // committed (a kill or a crash of `vm` or of Postgres loses nothing).
    // Postgres first, outright, then `vm dev`: neither stops anything.
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &postgres.pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success(), "the harness's own check: kill -KILL");
    let run = dev.kill();
    assert_eq!(run.code, None, "the harness's own check: killed outright");
    let deadline = Instant::now() + Duration::from_secs(10);
    while is_running(postgres.pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !is_running(postgres.pid),
        "the harness's own check: Postgres killed"
    );
    let read = Requested::of(&local.request(&project.dir, "/notes", OUTSIDER, &[]));
    assert_eq!(
        note_bodies(&read),
        ["kept through a kill"],
        "expected the row written before the kill in the next run: a kill loses nothing\n{}",
        read.describe()
    );
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing running after the next run, found {left:#?}"
    );
}
