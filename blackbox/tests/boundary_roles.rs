//! No statement lets Application SQL reach a more powerful database role
//! (CONTRACT.md, "The login is the boundary": each Access level has its own
//! connections, logged in directly as that level's login; `current_user`
//! and `session_user` are the level's login in every operation; ticket
//! "Write access, and the database role as the boundary", fourth acceptance
//! criterion).
//!
//! `SET ROLE`, `RESET ROLE`, `SET SESSION AUTHORIZATION` and
//! `set_config('role', ...)` are tried directly and hidden from a parser in
//! `ts_stat` (which is how the spike escaped a one-login-and-`SET ROLE`
//! design, `docs/research/what-postgres-sees.md`), through `query` and
//! through `execute`. Then many operations, made together so that they
//! take every pooled connection, report who they run as; and the shared
//! database's own server log shows which login each statement came from.
//! The assertions are on what Postgres says, not on the statement check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;

use blackbox::User;
use blackbox::postgres::SUPERUSER;
use blackbox::scenario::{READERS, RESOURCE, Scenario, WRITERS, op, sql_on};
use blackbox::shared_database::{Session, observe_insert, observe_select, token};
use serde_json::{Value, json};

fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS])
}

fn walt() -> User {
    User::new("walt").in_groups(&[WRITERS, READERS])
}

/// `set_config(name, value, false)` run by `ts_stat`, out of a parser's
/// sight.
fn hidden_set(name: &str, value: &str) -> String {
    format!(
        "select * from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('{name}', '{value}', false))$vm$) limit 1"
    )
}

/// The roles a statement might try to become: the instance's superuser,
/// the database's own write login (stronger than the read login), and
/// `none` (what `RESET ROLE` sets).
fn targets(s: &Scenario) -> Vec<String> {
    vec![
        SUPERUSER.to_owned(),
        s.db.writer.role.clone(),
        "none".to_owned(),
    ]
}

/// The attempts, as SQL; each is also expected to leave the session as it
/// found it.
fn attempts(s: &Scenario) -> Vec<String> {
    let mut sql = vec![
        "reset role".to_owned(),
        "reset session authorization".to_owned(),
        "set session authorization default".to_owned(),
        "set role none".to_owned(),
        "reset all".to_owned(),
    ];
    for role in targets(s) {
        sql.push(format!("set role {role}"));
        sql.push(format!("set session authorization {role}"));
        sql.push(format!("select set_config('role', '{role}', false)"));
        sql.push(format!(
            "select set_config('session_authorization', '{role}', false)"
        ));
        sql.push(hidden_set("role", &role));
        sql.push(hidden_set("session_authorization", &role));
        sql.push(format!(
            "with h as ({}) select current_user::text from h",
            hidden_set("role", &role)
        ));
        sql.push(format!(
            "select title from incidents where exists ({})",
            hidden_set("role", &role)
        ));
    }
    sql
}

/// In the statement that tries it: who the statement runs as, right after
/// the hidden `set_config('role', 'none')` (a `RESET ROLE`).
fn reset_then_who() -> String {
    "select current_user::text as cu, session_user::text as su \
       from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('role', 'none', false))$vm$) limit 1"
        .to_owned()
}

/// As [`reset_then_who`], in a write: the row records who wrote it.
fn reset_then_who_writes() -> String {
    "insert into notes (body) \
       select current_user || '/' || session_user \
         from (select * from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('role', 'none', false))$vm$) limit 1) h \
     returning body, written_by, session_login"
        .to_owned()
}

/// Fails unless every session ran as `login`, both `current_user` and
/// `session_user`.
fn assert_all_ran_as(sessions: &[Session], login: &str, context: &str) {
    let wrong: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.current_user != login || s.session_user != login)
        .collect();
    assert!(
        wrong.is_empty(),
        "{context}: expected every operation to run as {login} (current_user and session_user), but these did not:\n{wrong:#?}"
    );
    let pids: BTreeSet<i64> = sessions.iter().map(|s| s.pid).collect();
    eprintln!(
        "{context}: {} operations on {} connections, all as {login}",
        sessions.len(),
        pids.len()
    );
}

#[test]
fn no_role_is_reached_from_a_read() {
    let s = Scenario::writable();
    let reader = s.db.reader.role.clone();
    // In the statement itself.
    let (_, answer) = s.one(
        &walt(),
        "board",
        &op("query", &sql_on(RESOURCE, &reset_then_who())),
    );
    assert_eq!(
        answer["ok"]["rows"],
        json!([[reader, reader]]),
        "expected a read to stay its login right after a hidden RESET ROLE, got {answer}"
    );
    // Every attempt, then many operations at once.
    let calls: Vec<Value> = attempts(&s)
        .iter()
        .map(|sql| op("query", &sql_on(RESOURCE, sql)))
        .collect();
    let (_, answers) = s.run(&walt(), "board", &json!({ "calls": calls }));
    for answer in &answers {
        if let Some(rows) = answer["ok"]["rows"].as_array() {
            let text = json!(rows).to_string();
            assert!(
                !text.contains(SUPERUSER) && !text.contains(&s.db.writer.role),
                "expected no read to report a stronger role, got {answer}"
            );
        }
    }
    let marker = format!("read-after-{}", token());
    let sessions = s.observe_spread(
        &[
            (walt(), "board"),
            (User::new("rita").in_groups(&[READERS]), "viewer"),
            (walt(), "viewer"),
            (User::new("rex").in_groups(&[READERS]), "board"),
        ],
        &op("query", &sql_on(RESOURCE, &observe_select(100))),
        8,
    );
    assert_all_ran_as(&sessions, &reader, "the reads after the attempts");
    // The server's log: every statement of a read came from the read login.
    let (_, answer) = s.one(
        &walt(),
        "board",
        &op(
            "query",
            &sql_on(RESOURCE, &format!("select '{marker}' as m")),
        ),
    );
    assert!(answer["ok"].is_object(), "{answer}");
    s.db.assert_marker_came_from(&marker, &s.db.reader, "a read after the attempts");
    assert_statements_came_from_the_logins(&s);
}

#[test]
fn no_role_is_reached_from_a_write() {
    let s = Scenario::writable();
    let writer = s.db.writer.role.clone();
    let (_, answer) = s.execute(
        &wanda(),
        "board",
        &sql_on(RESOURCE, &reset_then_who_writes()),
    );
    assert_eq!(
        answer["ok"]["rows"],
        json!([[format!("{writer}/{writer}"), writer, writer]]),
        "expected a write to stay its login right after a hidden RESET ROLE, got {answer}"
    );
    let calls: Vec<Value> = attempts(&s)
        .iter()
        .map(|sql| op("execute", &sql_on(RESOURCE, sql)))
        .chain(
            targets(&s)
                .iter()
                .map(|role| {
                    format!(
                        "insert into notes (body) select 'after {role}' from ({}) h returning written_by, session_login",
                        hidden_set("role", role)
                    )
                })
                .map(|sql| op("execute", &sql_on(RESOURCE, &sql))),
        )
        .collect();
    let (_, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
    for answer in &answers {
        if let Some(rows) = answer["ok"]["rows"].as_array() {
            let text = json!(rows).to_string();
            assert!(
                !text.contains(&format!("\"{SUPERUSER}\"")),
                "expected no write to report the superuser, got {answer}"
            );
        }
    }
    let sessions = s.observe_spread(
        &[
            (wanda(), "board"),
            (walt(), "board"),
            (wanda(), "board"),
            (walt(), "board"),
        ],
        &op("execute", &sql_on(RESOURCE, &observe_insert(100))),
        8,
    );
    assert_all_ran_as(&sessions, &writer, "the writes after the attempts");
    // Who wrote every row of notes, as Postgres recorded it.
    let writers =
        s.db.superuser()
            .batch("select distinct written_by || '/' || session_login from notes");
    assert_eq!(
        writers,
        vec![vec![Some(format!("{writer}/{writer}"))]],
        "expected every row written by the write login, logged in as itself"
    );
    // Reads in the same program still run as the read login.
    let reads = s.observe_spread(
        &[(walt(), "board"), (walt(), "viewer")],
        &op("query", &sql_on(RESOURCE, &observe_select(100))),
        8,
    );
    assert_all_ran_as(&reads, &s.db.reader.role, "the reads after the writes");
    assert_statements_came_from_the_logins(&s);
}

/// Fails unless every statement the shared database received came from its
/// read or write login, or from the harness's own superuser (which sets the
/// database up and looks at it).
fn assert_statements_came_from_the_logins(s: &Scenario) {
    let allowed = [
        s.db.reader.role.as_str(),
        s.db.writer.role.as_str(),
        SUPERUSER,
    ];
    let wrong: Vec<String> =
        s.db.statements_by_login()
            .into_iter()
            .filter(|(by, _)| !allowed.contains(&by.as_str()))
            .map(|(by, entry)| format!("  from {by:?}: {entry}"))
            .collect();
    assert!(
        wrong.is_empty(),
        "expected every statement in the shared database's log to come from the Resource's logins, but:\n{}",
        wrong.join("\n")
    );
}
