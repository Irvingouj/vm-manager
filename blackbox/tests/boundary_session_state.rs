//! Nothing one operation leaves in its session reaches a later operation:
//! settings, the search path, temporary tables, prepared statements,
//! cursors, session advisory locks, `LISTEN` (CONTRACT.md, "The login is
//! the boundary"; ticket "Write access, and the database role as the
//! boundary", fifth acceptance criterion).
//!
//! A hostile Application leaves state through `query` and through
//! `execute`: by IT's function that leaves everything a session can hold
//! (`leave_session_state()`, which a statement check cannot see into), and
//! by plain SQL that passes any statement check (`pg_try_advisory_lock`,
//! `set_config` run by `ts_stat`). It does so many times at once, so that
//! as many pooled connections as possible are dirtied. Then later
//! operations, by the same User and by another, made together so that they
//! take every pooled connection, report what their session holds; and the
//! harness's own connection looks for advisory locks still held by the
//! login's sessions. Each connection that was left dirty must be found
//! clean by a later operation, or be gone.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::scenario::{READERS, RESOURCE, Scenario, WRITERS, op, sql_on};
use blackbox::shared_database::{
    LEFT_SEARCH_PATH, LEFT_SETTING, LEFT_STATEMENT_TIMEOUT, LEFT_WORK_MEM, Login, Session,
    observe_insert, observe_select,
};
use serde_json::{Value, json};

/// How many operations are made at once, over this many requests: more
/// than a pool of a shared database's connections is expected to hold.
const REQUESTS: usize = 4;
const PER_REQUEST: usize = 8;

/// Statements that leave state behind and answer their backend's pid as
/// `pid`; `select` for `query`, wrapped in an insert for `execute`.
fn leavers() -> Vec<String> {
    let set = |name: &str, value: &str| {
        format!(
            "select pg_backend_pid() as pid \
               from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('{name}', '{value}', false))$vm$) limit 1"
        )
    };
    vec![
        "select leave_session_state() as pid".to_owned(),
        "select pg_backend_pid() as pid from (select pg_try_advisory_lock(71, 44)) l".to_owned(),
        set("search_path", LEFT_SEARCH_PATH),
        set("statement_timeout", LEFT_STATEMENT_TIMEOUT),
        set("work_mem", LEFT_WORK_MEM),
        set("leftover.setting", LEFT_SETTING),
    ]
}

/// `sql` (a `select ... as pid`) as a write: the pid comes back through
/// `RETURNING`.
fn as_insert(sql: &str) -> String {
    format!("insert into notes (body) select pid::text from ({sql}) l returning body as pid")
}

/// The pid an answer to a leaver reports, if it ran.
fn pid_of(answer: &Value) -> Option<i64> {
    let value = &answer["ok"]["rows"][0][0];
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
}

/// What must be equal between a clean session and any other of the same
/// level: everything but the backend and the label.
fn shape(session: &Session) -> Session {
    Session {
        pid: 0,
        vm_user: None,
        vm_application: None,
        vm_request: None,
        ..session.clone()
    }
}

/// The whole round for one level: observe a clean pool, leave state from
/// many operations at once, observe again by the same and by another User,
/// and look from outside.
fn assert_nothing_is_left(
    s: &Scenario,
    operation: &str,
    login: &Login,
    leaver: &User,
    other: &User,
    application: &str,
) {
    let observe = |pause: u32| {
        let sql = if operation == "execute" {
            observe_insert(pause)
        } else {
            observe_select(pause)
        };
        op(operation, &sql_on(RESOURCE, &sql))
    };
    let observers: Vec<(User, &str)> = (0..REQUESTS)
        .map(|i| {
            let user = if i % 2 == 0 { leaver } else { other };
            (user.clone(), application)
        })
        .collect();

    let clean = s.observe_spread(&observers, &observe(100), PER_REQUEST);
    let baseline = shape(&clean[0]);
    assert!(
        baseline.leftovers().is_empty(),
        "expected a fresh pool to hold nothing of the test's, got {baseline:?}"
    );

    // Leave state from many operations at once.
    let calls: Vec<Value> = leavers()
        .iter()
        .map(|sql| {
            let sql = if operation == "execute" {
                as_insert(sql)
            } else {
                sql.clone()
            };
            op(operation, &sql_on(RESOURCE, &sql))
        })
        .collect();
    let bodies: Vec<(User, &str, Value)> = (0..REQUESTS)
        .map(|_| {
            (
                leaver.clone(),
                application,
                json!({ "calls": calls, "mode": "together" }),
            )
        })
        .collect();
    let mut dirtied = BTreeSet::new();
    for (_, answers) in s.at_once(&bodies) {
        for (sql, answer) in leavers().iter().cycle().zip(&answers) {
            match pid_of(answer) {
                Some(pid) => {
                    dirtied.insert(pid);
                }
                None => eprintln!("a leaver did not run ({sql}): {answer}"),
            }
        }
    }
    eprintln!(
        "{operation}: state left on the connections {dirtied:?}; before, the pool showed {:?}",
        clean.iter().map(|c| c.pid).collect::<BTreeSet<_>>()
    );

    // Later operations, by the same User and by another.
    let mut later: Vec<Session> = Vec::new();
    for _round in 0..2 {
        later.extend(s.observe_spread(&observers, &observe(100), PER_REQUEST));
        later.extend(s.observe_spread(&observers[..1], &observe(0), 4));
    }
    let dirty: Vec<String> = later
        .iter()
        .filter(|session| !session.leftovers().is_empty())
        .map(|session| {
            format!(
                "  connection {} still holds {:?}",
                session.pid,
                session.leftovers()
            )
        })
        .collect();
    assert!(
        dirty.is_empty(),
        "{operation}: expected no later operation to find what an earlier one left in its session, but:\n{}",
        dirty.join("\n")
    );
    let different: Vec<&Session> = later
        .iter()
        .filter(|session| shape(session) != baseline)
        .collect();
    assert!(
        different.is_empty(),
        "{operation}: expected every later session as the fresh pool showed it ({baseline:?}), but:\n{different:#?}"
    );
    let visited: BTreeSet<i64> = later.iter().map(|session| session.pid).collect();
    eprintln!("{operation}: later operations ran on the connections {visited:?}");

    // From outside: no advisory lock still held by the login's sessions,
    // once the program has had a moment to put its connections back.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let held = s.db.advisory_locks_of(&[&login.role]);
        if held.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{operation}: expected no advisory lock still held by the sessions of {}, but the harness's own connection sees:\n{}",
            login.role,
            held.join("\n")
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Each dirtied connection was found clean by a later operation, or is
    // gone.
    let alive: BTreeSet<i64> = s.db.sessions_of(&login.role).into_iter().collect();
    let unaccounted: Vec<i64> = dirtied
        .iter()
        .filter(|pid| !visited.contains(pid) && alive.contains(pid))
        .copied()
        .collect();
    assert!(
        unaccounted.is_empty(),
        "{operation}: the connections {unaccounted:?} had state left in them and are still open as {}, but no later operation ran on them, so they cannot have been shown clean (the later operations ran on {visited:?})",
        login.role
    );
}

#[test]
fn state_left_by_a_read_does_not_reach_a_later_read() {
    let s = Scenario::writable();
    assert_nothing_is_left(
        &s,
        "query",
        &s.db.reader,
        &User::new("rita").in_groups(&[READERS]),
        &User::new("rex").in_groups(&[READERS]),
        "viewer",
    );
}

#[test]
fn state_left_by_a_write_does_not_reach_a_later_write() {
    let s = Scenario::writable();
    assert_nothing_is_left(
        &s,
        "execute",
        &s.db.writer,
        &User::new("wanda").in_groups(&[WRITERS]),
        &User::new("walt").in_groups(&[WRITERS]),
        "board",
    );
    // The reads of the same program find nothing of it either.
    let reads = s.observe_spread(
        &[(User::new("rita").in_groups(&[READERS]), "viewer")],
        &op("query", &sql_on(RESOURCE, &observe_select(100))),
        PER_REQUEST,
    );
    for session in reads {
        assert!(
            session.leftovers().is_empty() && session.current_user == s.db.reader.role,
            "expected a read after the writes to find nothing they left, got {session:?}"
        );
    }
}
