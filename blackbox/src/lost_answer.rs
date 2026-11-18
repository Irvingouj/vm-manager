//! What the tests of a write whose answer is lost share (CONTRACT.md, "A
//! write whose answer is lost", and "Contributors and sharing: Opening an
//! Application" for an Application's two lists): a Postgres of the test's
//! own for the platform database, on which the harness can make every
//! commit that wrote something wait, and let commits through again.
//!
//! How commits are held: the harness's Postgres is started with
//! `synchronous_commit=off`, for speed; a commit waits for a standby only
//! with it on, so it is turned on for the role the program connects as (the
//! superuser) before the program opens its first session. Then
//! `synchronous_standby_names` naming a standby that does not exist, and a
//! reload: reads are answered, a commit that wrote waits (`SyncRep` in
//! `pg_stat_activity`), still holding its locks and not yet seen by anyone
//! else. Resetting it and reloading lets the waiting commits land. The
//! program gives up on a write after its limit for an operation on the
//! platform database (20 s) and answers `503` `unavailable`; the commit is
//! still on its way then.
//!
//! The Applications' requests, the API's answers and the host's decisions
//! are seen from outside: the program's `host-call` record says what the
//! host decided at the moment of a call, before its Operation log entry
//! (which waits with every other commit) is written.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::http::{Request, Response};
use crate::postgres::{Platform, PrivatePostgres};
use crate::records::{event, user_of};
use crate::sql::Sql;
use crate::vm::Vm;

/// How long the program may take to answer while the platform database
/// holds commits: what it answers then it answers at once.
pub const QUICK: Duration = Duration::from_secs(2);

/// How long the program has, once commits go through, to have read what
/// became of a write whose answer was lost.
pub const SETTLE: Duration = Duration::from_secs(15);

/// How long the harness waits for the answer of a write the program gives
/// up on (it does after 20 s).
pub const LOST_WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the harness waits for something it has caused to be seen (a
/// commit waiting, a record written).
const SEEN_WITHIN: Duration = Duration::from_secs(10);

/// A Postgres of the test's own on which commits can be held.
pub struct HeldPostgres {
    postgres: PrivatePostgres,
}

impl HeldPostgres {
    /// Starts it and turns `synchronous_commit` on for the role the program
    /// will connect as. Commits go through until [`HeldPostgres::hold`].
    pub fn start() -> Self {
        let postgres = PrivatePostgres::start()
            .unwrap_or_else(|e| panic!("the harness could not start a Postgres of its own: {e}"));
        let held = Self { postgres };
        held.superuser()
            .batch("ALTER ROLE CURRENT_USER SET synchronous_commit = on");
        held
    }

    /// The Postgres itself.
    pub fn postgres(&self) -> &PrivatePostgres {
        &self.postgres
    }

    /// A platform database of this name on it, as its superuser (the role
    /// whose commits are held).
    pub fn platform(&self, name: &str) -> Platform {
        Platform::on(&self.postgres, name)
    }

    /// A new connection of the harness's superuser.
    pub fn superuser(&self) -> Sql {
        Sql::connect(&self.postgres.url("postgres"))
            .unwrap_or_else(|e| panic!("the harness cannot reach its own Postgres: {e}"))
    }

    /// Makes every commit that wrote something wait.
    pub fn hold(&self) {
        self.set_standby(Some("nobody"));
    }

    /// Lets commits through again, those waiting included.
    pub fn let_through(&self) {
        self.set_standby(None);
    }

    /// How many commits are waiting.
    pub fn waiting(&self) -> usize {
        self.superuser()
            .value("SELECT count(*) FROM pg_stat_activity WHERE wait_event = 'SyncRep'")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    /// Waits until at least `at_least` commits are waiting; the harness's
    /// own check fails otherwise.
    pub fn wait_for_waiting(&self, at_least: usize, context: &str) {
        let deadline = Instant::now() + SEEN_WITHIN;
        loop {
            let now = self.waiting();
            if now >= at_least {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the harness's own check, {context}: expected {at_least} commits waiting at the test Postgres within {SEEN_WITHIN:?}, {now} are"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until no commit is waiting any more: every write that was on
    /// its way has landed. The harness's own check fails otherwise.
    pub fn wait_until_none_waiting(&self, context: &str) {
        let deadline = Instant::now() + SEEN_WITHIN;
        loop {
            let now = self.waiting();
            if now == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the harness's own check, {context}: expected no commit waiting at the test Postgres within {SEEN_WITHIN:?} once commits go through, {now} are"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Fails (as the harness's own check) unless at least `at_least`
    /// commits are waiting now.
    pub fn assert_waiting(&self, at_least: usize, context: &str) {
        let now = self.waiting();
        assert!(
            now >= at_least,
            "the harness's own check, {context}: expected {at_least} commits still on their way at the test Postgres, {now} are"
        );
    }

    /// How many lines of the server's log name `synchronous_standby_names`.
    fn reloads_seen(&self) -> usize {
        self.postgres
            .server_log()
            .lines()
            .filter(|l| l.contains("synchronous_standby_names"))
            .count()
    }

    /// Sets `synchronous_standby_names` (or resets it with `None`) and
    /// reloads, waiting until the server's log shows the server took it,
    /// and a little more for every server process to have taken it before
    /// its next command.
    fn set_standby(&self, value: Option<&str>) {
        let sql = self.superuser();
        let before = self.reloads_seen();
        match value {
            Some(value) => sql.batch(&format!(
                "ALTER SYSTEM SET synchronous_standby_names = '{value}'"
            )),
            None => sql.batch("ALTER SYSTEM RESET synchronous_standby_names"),
        };
        sql.batch("SELECT pg_reload_conf()");
        let deadline = Instant::now() + SEEN_WITHIN;
        while self.reloads_seen() == before {
            assert!(
                Instant::now() < deadline,
                "the harness's own check: expected the test Postgres to take the new configuration\n{}",
                self.postgres.server_log()
            );
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// Sends `request`, which must be answered within [`QUICK`]: what the
/// program answers while it cannot tell what became of a write.
pub fn quickly(vm: &Vm, request: &Request, context: &str) -> Response {
    let began = Instant::now();
    let response = request
        .try_send()
        .unwrap_or_else(|e| vm.fail(&format!("{context}: no answer ({e})")));
    let took = began.elapsed();
    assert!(
        took < QUICK,
        "{context}: expected an answer at once, without waiting for the platform database, it took {took:?}\n{}",
        response.describe()
    );
    response
}

/// Fails unless this is the `503` with `retry-after` of an Application the
/// program cannot tell how to serve now.
pub fn assert_503_retry(response: &Response, context: &str) {
    assert!(
        response.status == 503 && response.header("retry-after").is_some(),
        "{context}: expected `503` with `retry-after`, nothing served on a guess, got\n{}",
        response.describe()
    );
}

/// The `host-call` record of this User's call, waiting up to a few
/// seconds; `None` when none is written by then. (Each call a test makes
/// while commits are held is made by a User of its own.)
pub fn host_call_of(vm: &Vm, user: &str) -> Option<Value> {
    let deadline = Instant::now() + SEEN_WITHIN;
    loop {
        let found = vm
            .records()
            .into_iter()
            .find(|r| event(r) == "host-call" && user_of(r) == Some(user));
        if found.is_some() || Instant::now() >= deadline {
            return found;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// `Ok` when the `host-call` record of this User's call says the host
/// refused it with `reason`, else what it says instead, for the failure.
pub fn host_call_refused(vm: &Vm, user: &str, reason: &str) -> Result<(), String> {
    match host_call_of(vm, user) {
        Some(record)
            if record["outcome"].as_str() == Some("refused")
                && record["reason"].as_str() == Some(reason) =>
        {
            Ok(())
        }
        Some(record) => Err(format!(
            "expected the host to decide {user:?}'s call `refused` {reason:?} at the moment of the call, its `host-call` record says {record}"
        )),
        None => Err(format!(
            "expected a `host-call` record for {user:?}'s call within {SEEN_WITHIN:?}, there is none"
        )),
    }
}
