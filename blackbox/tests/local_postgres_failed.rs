//! What `postgres-failed` says (CONTRACT.md, "The local run on a busy
//! machine": "What `postgres-failed` says"; ticket "The local run's private
//! Postgres passes its time limits under the gate's load, and the refusal
//! does not say which limit": a local run that ends `postgres-failed` on a
//! limit says the limit and the step; on a lost connection or a server
//! error, that).
//!
//! Each test makes the private Postgres of a first local run fail one way,
//! from outside: `VIBE_LOCAL_POSTGRES_BIN` names a folder of the harness's own
//! programs, `initdb` the harness's real one, and `postgres` one that
//! answers `--version` as the real one does and otherwise
//!
//! - ends at once (`ended`);
//! - never starts anything (`sleep`): the run waits for a Postgres that is
//!   never ready (`timed-out`, the step of starting it);
//! - is the real Postgres with `pre_auth_delay=60`: it is ready, and every
//!   connection opened to it waits a minute before it is answered
//!   (`timed-out`, the step of connecting);
//! - is the real Postgres listening on another port than the one it was
//!   given: nothing answers where the run connects (`connection`);
//! - is the real Postgres with `default_transaction_read_only=on`: the first
//!   statement that writes is refused by the server, SQLSTATE `25006`
//!   (`server`).
//!
//! The `initdb` of that folder keeps a copy of the password file the run
//! gives it, so that a test knows the private Postgres's password and can
//! look for it in what the run printed. Each case is run with `--json` and
//! in words, in the same project.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use blackbox::dev::{Dev, free_port};
use blackbox::generate::listed;
use blackbox::local::{Local, OUTSIDER, local_postgres_bin, local_project};
use blackbox::project::stand_ins;
use blackbox::sdk::TempProject;
use blackbox::tool::{Run, closed_address};
use serde_json::Value;

/// The command's own limit for each run: above the program's limits for
/// the private Postgres (60 s to be ready is the longest seen), below the
/// harness's.
const TIMEOUT: [&str; 2] = ["--timeout", "200"];

/// A project whose first local run gets a `postgres` that does `start`
/// (a `/bin/sh` text; `$REAL` is the folder of the harness's own Postgres
/// programs) whenever it is not asked its version.
struct Failing {
    local: Local,
    project: TempProject,
    /// Where the stand-in `initdb` copies the password file it is given.
    password_copy: PathBuf,
}

impl Failing {
    fn new(name: &str, start: &str) -> Self {
        let local = Local::new();
        let root = local.machine.root().to_path_buf();
        let real = local_postgres_bin();
        let password_copy = root.join("password-given");
        let postgres = format!(
            "REAL='{real}'\ncase \"$1\" in\n  --version|-V) exec \"$REAL/postgres\" \"$@\" ;;\nesac\n{start}"
        );
        let initdb = format!(
            "for a in \"$@\"; do\n  case \"$a\" in\n    --pwfile=*) cp \"${{a#--pwfile=}}\" '{}' ;;\n  esac\ndone\nexec '{real}/initdb' \"$@\"",
            password_copy.display()
        );
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
        let project = local_project(&local.machine, name, &[]);
        Self {
            local,
            project,
            password_copy,
        }
    }

    /// The private Postgres's password, as the run gave it to `initdb`.
    fn password(&self) -> String {
        let text = fs::read_to_string(&self.password_copy).unwrap_or_else(|e| {
            panic!(
                "the harness's own check: the stand-in initdb copied no password file to {}: {e}",
                self.password_copy.display()
            )
        });
        let password = text.trim().to_owned();
        assert!(
            password.len() >= 8,
            "the harness's own check: a password of at least 8 characters, got {} characters",
            password.len()
        );
        password
    }

    /// `vibe request / --as outsider --json`, which must end `postgres-failed`;
    /// what it printed and its `error`.
    fn request_json(&self) -> (Run, Value) {
        let run = self
            .local
            .request(&self.project.dir, "/", OUTSIDER, &TIMEOUT);
        run.json_error(1, "postgres-failed");
        let error = run.json()["error"].clone();
        self.assert_nothing_left("with --json");
        (run, error)
    }

    /// `vibe request / --as outsider`, in words, which must end with `1`;
    /// what it printed and its message.
    fn request_words(&self) -> (Run, String) {
        let run = self
            .local
            .request_in_words(&self.project.dir, "/", OUTSIDER, &TIMEOUT);
        let message = run.text_error(1);
        self.assert_nothing_left("in words");
        (run, message)
    }

    /// Fails unless nothing the runs started is running and the run's own
    /// folder in `TMPDIR` is removed.
    fn assert_nothing_left(&self, context: &str) {
        let left = self.local.processes_after(Duration::from_secs(5));
        assert!(
            left.is_empty(),
            "{context}: expected nothing the run started left running after postgres-failed, found {left:#?}"
        );
        assert_eq!(
            listed(&self.local.tmp()),
            Vec::<String>::new(),
            "{context}: expected the run's own folder in TMPDIR removed"
        );
    }

    /// Fails if the private Postgres's password or a connection string is in
    /// any of these texts.
    fn assert_no_secret(&self, runs: &[&Run]) {
        let password = self.password();
        for run in runs {
            for (whence, text) in run.texts() {
                assert!(
                    !text.contains(&password),
                    "expected the private Postgres's password in no output; {whence} holds it"
                );
                let lower = text.to_lowercase();
                for needle in ["postgres://", "postgresql://", "password="] {
                    assert!(
                        !lower.contains(needle),
                        "expected no connection string in the output; {whence} holds {needle:?}:\n{text}"
                    );
                }
            }
        }
    }
}

/// Fails unless `step` is a short fixed name: lower-case words joined by
/// `-`.
fn assert_step_shape(error: &Value) -> String {
    let step = error["step"].as_str().unwrap_or_else(|| {
        panic!("expected `error.step`, a short fixed name of what the run was doing, got {error:#}")
    });
    assert!(
        !step.is_empty()
            && step.len() <= 64
            && step
                .split('-')
                .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase())),
        "expected `error.step` of lower-case words joined by `-`, got {step:?}"
    );
    step.to_owned()
}

/// Fails unless `error.cause` is `cause`, `limit_seconds` is there exactly
/// when it is `timed-out` and `sqlstate` exactly when it is `server`.
fn assert_cause(error: &Value, cause: &str) {
    assert_eq!(
        error["cause"].as_str(),
        Some(cause),
        "expected `error.cause` {cause:?}, got {error:#}"
    );
    let limit = &error["limit_seconds"];
    if cause == "timed-out" {
        let seconds = limit.as_f64().unwrap_or_else(|| {
            panic!("expected `error.limit_seconds`, a number, with timed-out, got {error:#}")
        });
        assert!(
            seconds > 0.0 && seconds <= 300.0,
            "expected a limit of the program's own, in seconds, got {seconds}"
        );
    } else {
        assert!(
            limit.is_null(),
            "expected no `limit_seconds` with cause {cause:?}, got {error:#}"
        );
    }
    let sqlstate = &error["sqlstate"];
    if cause == "server" {
        let code = sqlstate.as_str().unwrap_or_else(|| {
            panic!("expected `error.sqlstate`, five characters, with server, got {error:#}")
        });
        assert!(
            code.len() == 5
                && code
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()),
            "expected an SQLSTATE of five characters, got {code:?}"
        );
    } else {
        assert!(
            sqlstate.is_null(),
            "expected no `sqlstate` with cause {cause:?}, got {error:#}"
        );
    }
}

/// Fails unless `message` names `.vibe/local/postgres.log`, as it did
/// before.
fn assert_names_log(message: &str, context: &str) {
    assert!(
        message.contains(".vibe/local/postgres.log"),
        "{context}: expected the message to keep naming .vibe/local/postgres.log, got {message:?}"
    );
}

/// The limit as the words would write it: `5`, `2.5`.
fn seconds_text(seconds: f64) -> String {
    if seconds.fract() == 0.0 {
        format!("{seconds:.0}")
    } else {
        format!("{seconds}")
    }
}

/// Fails unless `message` says the step did not end within `seconds`
/// seconds.
fn assert_says_limit(message: &str, seconds: f64, context: &str) {
    let n = seconds_text(seconds);
    assert!(
        [
            format!("{n} s "),
            format!("{n} s)"),
            format!("{n} s."),
            format!("{n} s,"),
            format!("{n} s:"),
            format!("{n} s;"),
            format!("{n}s"),
            format!("{n} second"),
        ]
        .iter()
        .any(|said| message.contains(said.as_str())),
        "{context}: expected the message to say the limit, {n} seconds, got {message:?}"
    );
}

#[test]
fn a_postgres_that_ends_at_once_is_cause_ended_with_the_same_step_in_every_run() {
    let failing = Failing::new("pg-ends", "echo 'a stand-in that ends at once' >&2\nexit 1");
    let (first, error) = failing.request_json();
    let step = assert_step_shape(&error);
    assert_cause(&error, "ended");
    assert_names_log(error["message"].as_str().unwrap(), "with --json");
    let (again, error_again) = failing.request_json();
    assert_cause(&error_again, "ended");
    assert_eq!(
        error_again["step"].as_str(),
        Some(step.as_str()),
        "expected the same `step` for the same step in every run"
    );
    let (words, message) = failing.request_words();
    assert_names_log(&message, "in words");
    let lower = message.to_lowercase();
    assert!(
        lower.contains("did not start") || lower.contains("ended") || lower.contains("exited"),
        "in words: expected the message to say that the private Postgres did not start or ended, got {message:?}"
    );
    failing.assert_no_secret(&[&first, &again, &words]);
}

#[test]
fn a_postgres_never_ready_is_cause_timed_out_with_its_limit_in_seconds() {
    let failing = Failing::new("pg-never-ready", "exec sleep 600");
    let (json, error) = failing.request_json();
    assert_step_shape(&error);
    assert_cause(&error, "timed-out");
    let seconds = error["limit_seconds"].as_f64().unwrap();
    let message = error["message"].as_str().unwrap();
    assert_names_log(message, "with --json");
    assert_says_limit(message, seconds, "with --json");
    let (words, said) = failing.request_words();
    assert_names_log(&said, "in words");
    assert_says_limit(&said, seconds, "in words");
    failing.assert_no_secret(&[&json, &words]);
}

#[test]
fn a_connection_never_answered_is_cause_timed_out_with_its_limit_in_seconds() {
    let failing = Failing::new(
        "pg-silent",
        "exec \"$REAL/postgres\" \"$@\" -c pre_auth_delay=60",
    );
    let (json, error) = failing.request_json();
    assert_step_shape(&error);
    assert_cause(&error, "timed-out");
    let seconds = error["limit_seconds"].as_f64().unwrap();
    let message = error["message"].as_str().unwrap();
    assert_names_log(message, "with --json");
    assert_says_limit(message, seconds, "with --json");
    let (words, said) = failing.request_words();
    assert_names_log(&said, "in words");
    assert_says_limit(&said, seconds, "in words");
    failing.assert_no_secret(&[&json, &words]);
}

#[test]
fn a_connection_that_cannot_be_opened_is_cause_connection() {
    let elsewhere = closed_address().port();
    let failing = Failing::new(
        "pg-elsewhere",
        &format!("exec \"$REAL/postgres\" \"$@\" -p {elsewhere}"),
    );
    let (json, error) = failing.request_json();
    assert_step_shape(&error);
    assert_cause(&error, "connection");
    let message = error["message"].as_str().unwrap();
    assert_names_log(message, "with --json");
    let (words, said) = failing.request_words();
    assert_names_log(&said, "in words");
    for (context, text) in [("with --json", message), ("in words", said.as_str())] {
        assert!(
            text.to_lowercase().contains("connect"),
            "{context}: expected the message to say the connection to the private Postgres could not be opened, got {text:?}"
        );
    }
    failing.assert_no_secret(&[&json, &words]);
}

#[test]
fn a_statement_the_server_refuses_is_cause_server_with_its_sqlstate() {
    let failing = Failing::new(
        "pg-read-only",
        "exec \"$REAL/postgres\" \"$@\" -c default_transaction_read_only=on",
    );
    let (json, error) = failing.request_json();
    assert_step_shape(&error);
    assert_cause(&error, "server");
    assert_eq!(
        error["sqlstate"].as_str(),
        Some("25006"),
        "expected the server's own SQLSTATE, read_only_sql_transaction"
    );
    let message = error["message"].as_str().unwrap();
    assert_names_log(message, "with --json");
    let (words, said) = failing.request_words();
    assert_names_log(&said, "in words");
    for (context, text) in [("with --json", message), ("in words", said.as_str())] {
        assert!(
            text.contains("25006"),
            "{context}: expected the message to give the server's SQLSTATE 25006, got {text:?}"
        );
    }
    failing.assert_no_secret(&[&json, &words]);
}

#[test]
fn vm_dev_that_fails_before_it_listens_says_the_same() {
    let failing = Failing::new(
        "pg-dev-read-only",
        "exec \"$REAL/postgres\" \"$@\" -c default_transaction_read_only=on",
    );
    let dev = Dev::spawn(
        &failing.local,
        &failing.project.dir,
        "pg-dev-read-only",
        free_port(),
        true,
    );
    let run = dev.wait_end(Duration::from_secs(200));
    run.json_error(1, "postgres-failed");
    let error = run.json()["error"].clone();
    assert_step_shape(&error);
    assert_cause(&error, "server");
    assert_eq!(error["sqlstate"].as_str(), Some("25006"));
    failing.assert_nothing_left("vibe dev");
    failing.assert_no_secret(&[&run]);
}
