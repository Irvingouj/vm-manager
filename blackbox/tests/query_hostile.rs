//! Hostile Applications against `query` (CONTRACT.md, "The raw host
//! interface": the host decides who is asking from the request in flight,
//! and nothing the Application passes, sets or overwrites changes that;
//! "Operations on a shared database": one statement, run in a read-only
//! transaction as the read login; "Who is allowed").
//!
//! An Application claims another identity in its arguments and overwrites
//! globals; names the Resource oddly; sends several statements in one
//! string; sends statements that write, create or set the role, also
//! through a careless "read" login that could write; and fires many
//! queries without awaiting them as different Users at once. Never may a
//! User get rows without a Grant, nor an entry name someone else, nor the
//! shared database change.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::HashMap;
use std::sync::Barrier;
use std::thread;

use blackbox::User;
use blackbox::scenario::{
    CONTACT, READERS, RESOURCE, Scenario, assert_refusal, assert_rows, lines,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn nobody() -> User {
    User::new("nobody").in_groups(&["finance"])
}

fn ask(resource: &str, sql: &str) -> Value {
    json!({ "resource": resource, "sql": sql })
}

fn probe(what: &str) -> (String, String) {
    let marker = format!("{what}-{}", token());
    (
        marker.clone(),
        format!("select '{marker}' as probe, title from incidents"),
    )
}

/// Fails if `answer` holds rows; if it is a refusal, it must be `reason`
/// naming `user` where the reason names one.
fn assert_not_allowed(answer: &Value, reason: &str, user: &str, context: &str) {
    assert!(
        answer.get("ok").is_none(),
        "{context}: expected no rows, got {answer}"
    );
    if answer.get("refused").is_some() {
        assert_eq!(
            answer["refused"]["refusal"]["reason"].as_str(),
            Some(reason),
            "{context}: expected the refusal {reason:?}, got {answer}"
        );
        if reason == "no-grant" {
            assert_eq!(
                answer["refused"]["refusal"]["user"].as_str(),
                Some(user),
                "{context}: expected the refusal to name the real User {user:?}, got {answer}"
            );
        }
    }
}

/// Fails unless every Operation log entry of this request names this User
/// and this Application.
fn assert_entries_name(s: &Scenario, response: &blackbox::Response, user: &str, application: &str) {
    let entries = s.entries_of(response);
    let wrong: Vec<Value> = entries
        .iter()
        .filter(|e| {
            e["user"].as_str() != Some(user) || e["application"].as_str() != Some(application)
        })
        .cloned()
        .collect();
    assert!(
        wrong.is_empty(),
        "expected every entry of the request to name the User {user:?} and the Application {application:?}, but:\n{}",
        lines(&wrong)
    );
    assert!(
        !entries.is_empty(),
        "expected the request's queries to have entries"
    );
}

/// The usual scenario with three copies of the hostile fixture: `loud`
/// declaring the Resource at read, `peek` at structure, `sneak` not at all.
fn hostile_scenario() -> Scenario {
    let s = Scenario::ready();
    s.install("loud", "query-hostile.js", &[(RESOURCE, "read")]);
    s.install("peek", "query-hostile.js", &[(RESOURCE, "structure")]);
    s.install("sneak", "query-hostile.js", &[]);
    s
}

#[test]
fn an_identity_claimed_in_the_arguments_changes_nothing() {
    let s = hostile_scenario();
    let (marker, sql) = probe("claim");
    let cases = [
        (nobody(), "loud", "no-grant"),
        (rita(), "peek", "declared-lower"),
        (rita(), "sneak", "not-declared"),
        (nobody(), "sneak", "not-declared"),
    ];
    for (user, application, reason) in cases {
        let (response, body) = s.post(
            &user,
            application,
            "/claim",
            &json!({ "args": ask(RESOURCE, &sql) }),
        );
        let answers = body["answers"].as_array().unwrap();
        assert_eq!(answers.len(), 3, "{body}");
        let context = format!(
            "{} in {application}, claiming to be it-admin in board at write",
            user.id
        );
        // The claims in the arguments themselves: ignored, like any field
        // the operation does not have.
        let expected = match reason {
            "no-grant" => json!({
                "resource": RESOURCE, "user": user.id, "needed": "read", "held": null,
                "contact": CONTACT,
            }),
            "declared-lower" => {
                json!({ "resource": RESOURCE, "declared": "structure", "needed": "read" })
            }
            _ => json!({ "resource": RESOURCE }),
        };
        assert_refusal(&answers[0], reason, &expected, &context);
        for answer in &answers[1..] {
            assert_not_allowed(answer, reason, &user.id, &context);
        }
        assert_entries_name(&s, &response, &user.id, application);
    }
    // The claims do not lower a User who is allowed either.
    let (_, body) = s.post(
        &rita(),
        "loud",
        "/claim",
        &json!({ "args": ask(RESOURCE, "select count(*)::int4 as n from incidents") }),
    );
    assert_rows(
        &body["answers"][0],
        &["n"],
        &json!([[3]]),
        "rita, allowed, with claims in her arguments",
    );
    s.db.assert_not_received(&marker, "queries refused whatever was claimed");
}

#[test]
fn an_identity_claimed_through_the_real_as_argument_gains_nothing() {
    // `as` is an argument the operation has (CONTRACT.md, "Operations on
    // the own database"): `"application"` on a Resource is refused with
    // `not-own-database`, and anything but `"user"` or `"application"` is
    // malformed. Neither sends anything, and the log names the real User.
    let s = hostile_scenario();
    let (marker, sql) = probe("claim-as");
    for (user, application) in [(rita(), "loud"), (nobody(), "loud"), (rita(), "sneak")] {
        let (response, body) = s.post(
            &user,
            application,
            "/claim-as",
            &json!({ "args": ask(RESOURCE, &sql) }),
        );
        let answers = body["answers"].as_array().unwrap();
        assert_eq!(answers.len(), 2, "{body}");
        let context = format!("{} in {application}", user.id);
        assert_refusal(
            &answers[0],
            "not-own-database",
            &json!({ "resource": RESOURCE }),
            &format!("{context}, claiming to act as the Application on a Resource"),
        );
        assert_eq!(
            answers[1]["error"]["kind"].as_str(),
            Some("malformed"),
            "{context}, claiming an identity object as `as`: expected {{ error: {{ kind: \"malformed\" }} }}, got {}",
            answers[1]
        );
        // One entry, for the refusal (a malformed operation has none),
        // naming the real User.
        let entries = s.entries_of(&response);
        assert_eq!(entries.len(), 1, "{}", lines(&entries));
        assert_entries_name(&s, &response, &user.id, application);
        assert_eq!(entries[0]["reason"], "not-own-database", "{}", entries[0]);
        assert_eq!(entries[0]["actor"], "application", "{}", entries[0]);
        // Every host call is recorded as the real User's.
        s.vm.records_of(&response)
            .assert_host_saw_only(&user.id, application);
    }
    s.db.assert_not_received(&marker, "queries claiming through `as`");
}

#[test]
fn overwritten_globals_and_built_ins_change_nothing() {
    let s = hostile_scenario();
    let (marker, sql) = probe("tamper");
    for (user, application, reason) in [
        (nobody(), "loud", "no-grant"),
        (rita(), "peek", "declared-lower"),
        (rita(), "sneak", "not-declared"),
    ] {
        let (response, body) = s.post(
            &user,
            application,
            "/tamper",
            &json!({ "args": ask(RESOURCE, &sql) }),
        );
        assert_not_allowed(
            &body["answer"],
            reason,
            &user.id,
            &format!(
                "{} in {application} after overwriting globals and built-ins ({})",
                user.id, body["tampered"]
            ),
        );
        assert_entries_name(&s, &response, &user.id, application);
    }
    s.db.assert_not_received(&marker, "queries made after tampering");
    // The program still serves the Application and its User afterwards.
    let (_, answer) = s.query(
        &rita(),
        "board",
        &ask(RESOURCE, "select count(*)::int4 as n from incidents"),
    );
    assert_rows(&answer, &["n"], &json!([[3]]), "after the tampering");
}

#[test]
fn a_resource_named_in_odd_case_or_with_whitespace_is_not_the_declared_one() {
    let s = Scenario::ready();
    let (marker, sql) = probe("odd-name");
    for name in [
        "DevOps",
        "DEVOPS",
        " devops",
        "devops ",
        "devops\n",
        "\tdevops",
        "dev ops",
        "devops\u{0}",
        "devоps",
        "devops;",
        "",
    ] {
        let (_, answer) = s.query(&rita(), "board", &ask(name, &sql));
        assert!(
            answer.get("ok").is_none(),
            "expected the Resource name {name:?} not to reach {RESOURCE:?}, got {answer}"
        );
        let refused = answer["refused"]["refusal"]["reason"].as_str();
        let malformed = answer["error"]["kind"] == "malformed";
        assert!(
            refused == Some("not-declared") || malformed,
            "expected the Resource name {name:?} to be refused as not declared (or the arguments as malformed), got {answer}"
        );
    }
    let (_, answer) = s.query(
        &rita(),
        "board",
        &ask(RESOURCE, "select 'odd-control' as c"),
    );
    assert_rows(&answer, &["c"], &json!([["odd-control"]]), "control");
    s.db.assert_received("odd-control", "the control");
    s.db.assert_not_received(&marker, "queries naming the Resource oddly");
}

/// The scenario with a second Resource, `loose`, over the same database
/// with the careless login (which could write), granted to [`READERS`] at
/// read and declared at read by `board2`.
fn careless_scenario() -> Scenario {
    let s = Scenario::ready();
    s.register_with("loose", &s.db.careless);
    s.grant("loose", READERS, "read");
    s.install(
        "board2",
        "query.js",
        &[("loose", "read"), (RESOURCE, "read")],
    );
    s
}

/// The superuser of the harness's instances.
const SUPERUSER: &str = blackbox::postgres::SUPERUSER;

/// Since "Write access, and the database role as the boundary" the
/// statement check refuses many of these before they reach the database;
/// the last ones are hidden from it, so that the database's own refusal is
/// still tested here (the full list is in `boundary_writes_through_query.rs`).
fn writes() -> Vec<String> {
    [
        "insert into incidents values (9, 'injected', true, 1, now())",
        "update incidents set title = 'changed'",
        "delete from incidents",
        "truncate incidents",
        "create table made_here (x int)",
        "create temp table made_here (x int)",
        "drop table kinds",
        "alter table incidents add column extra int",
        "select nextval('ticket_numbers')",
        "select setval('ticket_numbers', 100)",
        "with d as (delete from incidents returning *) select count(*) from d",
        "select * from incidents for update",
        "do $$ begin delete from incidents; end $$",
        "alter role current_user set work_mem = '8MB'",
        "grant select on staff_only to public",
        "copy incidents from program 'true'",
        "set role SUPERUSER",
        "set session authorization SUPERUSER",
        "select set_config('role', 'SUPERUSER', false)",
        "select close_every_incident()",
        "select * from ts_stat($vm$select to_tsvector('simple', 'n ' || nextval('ticket_numbers')::text)$vm$)",
        "select * from ts_stat($vm$select to_tsvector('simple', 'n ' || close_every_incident()::text)$vm$)",
        "select * from ts_stat($vm$select to_tsvector('simple', 'r ' || set_config('role', 'SUPERUSER', false))$vm$)",
    ]
    .iter()
    .map(|w| w.replace("SUPERUSER", SUPERUSER))
    .collect()
}

#[test]
fn statements_that_write_create_or_set_the_role_fail_and_change_nothing() {
    let s = careless_scenario();
    let before = s.db.fingerprint();
    for resource in [RESOURCE, "loose"] {
        for sql in writes() {
            let (_, answer) = s.query(&rita(), "board2", &ask(resource, &sql));
            assert!(
                answer.get("ok").is_none(),
                "expected {sql:?} through {resource:?} (a read) to fail in the database or be refused, got {answer}"
            );
        }
    }
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected the shared database unchanged by statements that write"
    );
}

#[test]
fn a_write_through_a_read_login_that_could_write_fails_as_read_only() {
    // The careless login may insert; the read-only transaction stops it.
    let s = careless_scenario();
    let before = s.db.fingerprint();
    let (_, answer) = s.query(
        &rita(),
        "board2",
        &ask(
            "loose",
            "insert into incidents values (9, 'injected', true, 1, now())",
        ),
    );
    blackbox::scenario::assert_database_error(
        &answer,
        "25006",
        "an insert through a read login that has the privilege, in a read-only transaction",
    );
    assert_eq!(s.db.fingerprint(), before);
}

/// `set_config` run by `ts_stat`: SQL text a statement check does not see
/// (docs/research/what-postgres-sees.md).
fn hidden_set(name: &str, value: &str) -> String {
    format!(
        "select * from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('{name}', '{value}', false))$vm$) limit 1"
    )
}

/// Since "Write access, and the database role as the boundary" the
/// statement check refuses these openers before they reach the database, so
/// the ones that change a setting are also sent hidden from the check, to
/// keep this test about what the database allows.
#[test]
fn nothing_a_statement_sets_in_its_session_lets_a_later_one_write() {
    let s = careless_scenario();
    let before = s.db.fingerprint();
    let hidden = [
        hidden_set("default_transaction_read_only", "off"),
        hidden_set("transaction_read_only", "off"),
        hidden_set("default_transaction_isolation", "serializable"),
    ];
    let openers = [
        hidden[0].as_str(),
        hidden[1].as_str(),
        hidden[2].as_str(),
        "set session characteristics as transaction read write",
        "set default_transaction_read_only = off",
        "select set_config('default_transaction_read_only', 'off', false)",
        "set transaction read write",
        "commit",
        "rollback",
        "end",
        "abort",
        "begin",
        "start transaction read write",
        "reset all",
        "discard all",
    ];
    let insert = ask(
        "loose",
        "insert into incidents values (9, 'injected', true, 1, now())",
    );
    for opener in openers {
        // The opener, then inserts in the same request and in later ones,
        // made together so that they spread over the pool's connections.
        let calls: Vec<Value> = std::iter::once(ask("loose", opener))
            .chain(std::iter::repeat_n(insert.clone(), 4))
            .collect();
        let (_, answers) = s.run(&rita(), "board2", &json!({ "calls": calls }));
        let (_, later) = s.run(
            &rita(),
            "board2",
            &json!({ "calls": vec![insert.clone(); 6], "mode": "together" }),
        );
        for answer in answers[1..].iter().chain(&later) {
            assert!(
                answer.get("ok").is_none(),
                "expected an insert after {opener:?} to fail, got {answer}"
            );
        }
    }
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected the shared database unchanged"
    );
}

#[test]
fn several_statements_in_one_string_do_not_run() {
    let s = careless_scenario();
    let before = s.db.fingerprint();
    let (marker, _) = probe("second-statement");
    let strings = [
        format!("select 1; select '{marker}' as m"),
        "select 1; delete from incidents".to_owned(),
        "delete from incidents; select 1".to_owned(),
        "select 1;; update incidents set title = 'x'".to_owned(),
        "commit; delete from incidents".to_owned(),
        "commit; insert into incidents values (9, 'x', true, 1, now())".to_owned(),
        format!("select 1; set role {SUPERUSER}"),
        "select 1;\ninsert into incidents values (9, 'x', true, 1, now())".to_owned(),
    ];
    for resource in [RESOURCE, "loose"] {
        for sql in &strings {
            let (_, answer) = s.query(&rita(), "board2", &ask(resource, sql));
            assert!(
                answer.get("ok").is_none(),
                "expected several statements in one string ({sql:?}) to fail or be refused, got {answer}"
            );
        }
    }
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected the shared database unchanged"
    );
    // The second statement never ran on its own.
    let ran_alone =
        s.db.statements()
            .into_iter()
            .filter(|e| e.contains(&marker) && !e.contains("select 1;"))
            .collect::<Vec<_>>();
    assert!(
        ran_alone.is_empty(),
        "expected the second statement never to be sent on its own, got:\n{}",
        ran_alone.join("\n")
    );
}

const USERS: usize = 16;

#[test]
fn many_queries_fired_without_awaiting_as_different_users_at_once_stay_with_their_user() {
    let s = Scenario::ready();
    let user = |i: usize| -> User {
        if i.is_multiple_of(2) {
            User::new(&format!("granted-{i}")).in_groups(&[READERS])
        } else {
            User::new(&format!("refused-{i}")).in_groups(&["finance"])
        }
    };
    let calls = |i: usize| -> Vec<Value> {
        (0..6)
            .map(|k| {
                json!({
                    "resource": RESOURCE,
                    "sql": "select id, title from incidents where id = $1",
                    "params": [(i + k) % 3 + 1],
                })
            })
            .collect()
    };
    let start = Barrier::new(USERS);
    let results: Vec<(User, String, Option<Vec<Value>>)> = thread::scope(|scope| {
        let handles: Vec<_> = (0..USERS)
            .map(|i| {
                let (s, start) = (&s, &start);
                scope.spawn(move || {
                    let user = user(i);
                    // A third of the requests fire their queries and answer
                    // without awaiting any.
                    let mode = if i % 3 == 0 { "unawaited" } else { "together" };
                    let request =
                        s.vm.request("board", "/")
                            .as_user(&user)
                            .method("POST")
                            .json(&json!({ "calls": calls(i), "mode": mode, "pauseMs": i % 4 }));
                    start.wait();
                    let response = request.send();
                    response.expect_status(200);
                    let answers = response.json()["answers"].as_array().cloned();
                    (user, response.request_id(), answers)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let titles = [
        "Disk full on db-7",
        "Login page slow",
        s.db.row_marker.as_str(),
    ];
    for (i, (user, _, answers)) in results.iter().enumerate() {
        let Some(answers) = answers else { continue };
        for (k, answer) in answers.iter().enumerate() {
            let id = (i + k) % 3 + 1;
            if user.groups == [READERS] {
                assert_rows(
                    answer,
                    &["id", "title"],
                    &json!([[id, titles[id - 1]]]),
                    &format!("{}'s query {k}", user.id),
                );
            } else {
                assert_refusal(
                    answer,
                    "no-grant",
                    &json!({
                        "resource": RESOURCE, "user": user.id, "needed": "read",
                        "held": null, "contact": CONTACT,
                    }),
                    &format!("{}'s query {k}", user.id),
                );
            }
        }
    }
    // Every entry of every request names that request's User, and only a
    // granted User's queries are allowed.
    let users: HashMap<&str, &User> = results
        .iter()
        .map(|(user, id, _)| (id.as_str(), user))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let entries = s.log();
    let mut per_request: HashMap<&str, usize> = HashMap::new();
    for entry in &entries {
        let Some(request) = entry["request"].as_str() else {
            continue;
        };
        let Some(user) = users.get(request) else {
            continue;
        };
        *per_request.entry(request).or_default() += 1;
        assert_eq!(
            entry["user"].as_str(),
            Some(user.id.as_str()),
            "expected the entry of request {request} to name its User {:?}, got {entry}",
            user.id
        );
        let granted = user.groups == [READERS];
        let expected_outcome = if granted { "allowed" } else { "refused" };
        assert_eq!(
            entry["outcome"].as_str(),
            Some(expected_outcome),
            "expected {:?}'s query {expected_outcome}, got {entry}",
            user.id
        );
        if !granted {
            assert_eq!(entry["reason"], "no-grant", "{entry}");
        }
    }
    for (user, id, answers) in &results {
        let count = per_request.get(id.as_str()).copied().unwrap_or(0);
        if answers.is_some() {
            assert_eq!(
                count, 6,
                "expected six entries for {}'s six awaited queries, got {count}",
                user.id
            );
        } else {
            assert!(
                count <= 6,
                "expected at most six entries for {}'s six unawaited queries, got {count}",
                user.id
            );
        }
    }
    s.vm.assert_log_keeps_each_request_to_itself();
}
