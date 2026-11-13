//! A request that ends with a transaction open has it rolled back, and its
//! connection returned clean (CONTRACT.md, "Transactions: What the host
//! does on its own"; ticket "Transactions through the SDK", criterion 3).
//!
//! The fixture begins a transaction, writes a marked row in it, and calls
//! `leave_session_state()` in it, which leaves on its session what a
//! rollback alone does not take away (a prepared statement, a session
//! advisory lock), then the request ends: by answering, or by an uncaught
//! error. Afterwards:
//!
//! - the marked row is not there;
//! - Postgres (`pg_stat_activity`, read from the harness's superuser
//!   connection) shows no session of the transaction's login still in a
//!   transaction, and `pg_locks` no advisory lock held by that login: the
//!   connection was cleaned or closed;
//! - the Operation log has the host's `rollback` entry, with `error`
//!   `request-ended` and the transaction's id;
//! - operations of the next request, made together so that they take the
//!   pool's connections, find nothing left on their sessions, and on the
//!   own database `vm.identity()` names the new request.
//!
//! A request may also end while a statement of its transaction is still
//! running (CONTRACT.md: the statement is stopped, its entry `failed` with
//! `error` `request-ended`, then the rollback): see the second test.
//!
//! Not tested: a request that ends because "its time is up". CONTRACT.md
//! gives no time after which a request is ended.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::scenario::{RESOURCE, lines};
use blackbox::shared_database::{OBSERVE_SESSION, Session, token};
use blackbox::transactions::{
    APP, Target, Txs, assert_began, begin, execute_in, fire, insert_marker, query_in, sleep, step,
    the_rollback, throw, together, wanda,
};
use serde_json::{Value, json};

/// How the request ends.
#[derive(Clone, Copy, Debug)]
enum Ending {
    Answers,
    Throws,
}

/// An `execute` on `target` (outside any transaction) that writes a row
/// and answers what its session holds, after `pause_ms`.
fn observe_execute(target: Target<'_>, pause_ms: u32) -> Value {
    let seconds = f64::from(pause_ms) / 1000.0;
    let sql = match target {
        Target::Shared(_) => format!(
            "insert into notes (body) select {OBSERVE_SESSION} from pg_sleep({seconds}) returning body as session"
        ),
        Target::Own(_) => format!(
            "insert into things (title) select {OBSERVE_SESSION} from pg_sleep({seconds}) returning title as session"
        ),
    };
    let mut args = target.args();
    args["sql"] = json!(sql);
    step("execute", args)
}

/// A `query` on `target` that answers what its session holds.
fn observe_query(target: Target<'_>, pause_ms: u32) -> Value {
    let mut args = target.args();
    args["sql"] = json!(blackbox::shared_database::observe_select(pause_ms));
    step("query", args)
}

fn advisory_locks(t: &Txs, target: Target<'_>) -> Vec<String> {
    let (database, roles) = t.logins_of(target);
    let roles = roles
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
    t.superuser_of(target)
        .batch(&format!(
            "SELECT a.pid || ' ' || a.usename || ' ' || l.classid || '/' || l.objid
               FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
              WHERE l.locktype = 'advisory' AND a.datname = '{database}' AND a.usename IN ({roles})"
        ))
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

fn wait_for_no_locks(t: &Txs, target: Target<'_>) -> Vec<String> {
    blackbox::transactions::wait_for_none(Duration::from_secs(5), || advisory_locks(t, target))
}

#[test]
fn a_request_that_ends_with_a_transaction_open_has_it_rolled_back_and_its_connection_clean() {
    let t = Txs::start();
    let user = wanda();
    for ending in [Ending::Answers, Ending::Throws] {
        for (target, access) in [
            (Target::Shared(RESOURCE), "write"),
            (Target::Shared(RESOURCE), "read"),
            (Target::Own("user"), "write"),
            (Target::Own("application"), "read"),
        ] {
            let context = format!("{} at {access}, the request {ending:?}", target.describe());
            let marker = format!("left-open-{}", token());
            let mut steps = vec![begin(target, access, "t")];
            if access == "write" {
                steps.push(execute_in("t", &insert_marker(target, &marker)));
            }
            steps.push(query_in("t", "select leave_session_state() as pid"));
            if let Ending::Throws = ending {
                steps.push(throw("an uncaught error with a transaction open"));
            }
            let response = t.send(&user, APP, &steps);
            let expected_status = match ending {
                Ending::Answers => 200,
                Ending::Throws => 500,
            };
            assert_eq!(
                response.status,
                expected_status,
                "{context}: {}",
                response.describe()
            );
            let id = if let Ending::Answers = ending {
                let answers = response.json()["answers"].as_array().cloned().unwrap();
                let id = assert_began(&answers[0], &context);
                assert!(
                    answers.last().unwrap()["ok"]["rows"][0][0].is_number(),
                    "{context}: leave_session_state() ran in the transaction: {}",
                    answers.last().unwrap()
                );
                Some(id)
            } else {
                None
            };
            assert_eq!(
                t.rows_with(target, &marker),
                0,
                "{context}: nothing the request did in its transaction is kept"
            );
            let still = t.sessions_in_transaction(target, Duration::from_secs(5));
            assert!(
                still.is_empty(),
                "{context}: expected no session of the transaction's login left in a transaction once the request ended, but Postgres shows:\n{still:#?}"
            );
            let locks = wait_for_no_locks(&t, target);
            assert!(
                locks.is_empty(),
                "{context}: the connection must go back clean or be closed, but its login's sessions still hold advisory locks: {locks:?}"
            );

            // The host's rollback, in the Operation log.
            let entries = t.entries_eventually(&response, steps.len());
            let begun = entries
                .iter()
                .find(|e| e["operation"] == "begin")
                .unwrap_or_else(|| panic!("{context}: no begin entry:\n{}", lines(&entries)));
            let begun_id = begun["transaction"].as_str().unwrap_or_else(|| {
                panic!("{context}: the begin entry carries the transaction's id: {begun}")
            });
            if let Some(id) = &id {
                assert_eq!(
                    begun_id, id,
                    "{context}: the id in the Operation log is the id Application code holds"
                );
            }
            let rollback = the_rollback(&entries, begun_id, &context);
            assert_eq!(
                rollback["error"], "request-ended",
                "{context}: the host's rollback says why: {rollback}"
            );
            assert_eq!(rollback["statement"], "rollback", "{context}: {rollback}");
            assert!(rollback["rows"].is_null(), "{context}: {rollback}");
            assert_eq!(
                rollback["request"].as_str(),
                Some(response.request_id().as_str()),
                "{context}: the host's rollback belongs to the request: {rollback}"
            );

            // The next request's operations find nothing left.
            let observe = if access == "write" {
                observe_execute(target, 300)
            } else {
                observe_query(target, 300)
            };
            let (next, answers) = t.run(&user, APP, &[together(&vec![observe; 6])]);
            for answer in answers[0].as_array().unwrap() {
                let session = Session::from_answer(answer).unwrap_or_else(|| {
                    panic!("{context}: expected an observation of the session, got {answer}")
                });
                assert!(
                    session.leftovers().is_empty(),
                    "{context}: a later operation found what the ended transaction left: {:?} in {session:?}",
                    session.leftovers()
                );
                assert_eq!(
                    session.vm_request.as_deref(),
                    Some(next.request_id().as_str()),
                    "{context}: a later operation's label is its own request's: {session:?}"
                );
            }
            if let Target::Own(actor) = target {
                let mut args = target.args();
                args["sql"] = json!("select request, actor from vm.identity()");
                let (next, answers) = t.run(&user, APP, &[step("query", args)]);
                assert_eq!(
                    answers[0]["ok"]["rows"],
                    json!([[next.request_id(), actor]]),
                    "{context}: the identity a later operation finds is its own"
                );
            }
        }
    }
}

/// The next request's operations on `target`, made together, find nothing
/// left on their sessions and carry their own request's label.
fn next_operations_are_clean(t: &Txs, target: Target<'_>, context: &str) {
    let (next, answers) = t.run(
        &wanda(),
        APP,
        &[together(&vec![observe_execute(target, 300); 6])],
    );
    for answer in answers[0].as_array().unwrap() {
        let session = Session::from_answer(answer).unwrap_or_else(|| {
            panic!("{context}: expected an observation of the session, got {answer}")
        });
        assert!(
            session.leftovers().is_empty(),
            "{context}: a later operation found what the ended transaction left: {:?} in {session:?}",
            session.leftovers()
        );
        assert_eq!(
            session.vm_request.as_deref(),
            Some(next.request_id().as_str()),
            "{context}: a later operation's label is its own request's: {session:?}"
        );
    }
}

#[test]
fn a_request_that_ends_while_a_statement_of_its_transaction_runs_has_it_stopped_and_rolled_back() {
    let t = Txs::start();
    let user = wanda();
    for target in [Target::Shared(RESOURCE), Target::Own("user")] {
        let context = format!(
            "{}: the request answers while `select pg_sleep(30)` runs in its transaction",
            target.describe()
        );
        let marker = format!("mid-statement-{}", token());
        let steps = [
            begin(target, "write", "t"),
            execute_in("t", &insert_marker(target, &marker)),
            fire(&query_in("t", "select pg_sleep(30) as slept")),
            // Long enough for the statement to reach Postgres; the request
            // then answers with it still running.
            sleep(500),
        ];
        let started = std::time::Instant::now();
        let response = t.send(&user, APP, &steps);
        assert_eq!(response.status, 200, "{context}: {}", response.describe());
        let answers = response.json()["answers"].as_array().cloned().unwrap();
        let id = assert_began(&answers[0], &context);
        assert_eq!(answers[2], json!({ "fired": true }), "{context}");

        let still = t.sessions_in_transaction(target, Duration::from_secs(5));
        assert!(
            still.is_empty(),
            "{context}: expected no session of the transaction's login in a transaction or running the sleep a few seconds after the request ended, but Postgres shows:\n{still:#?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{context}: the sleep was stopped, not waited out ({:?})",
            started.elapsed()
        );
        assert_eq!(
            t.rows_with(target, &marker),
            0,
            "{context}: the insert made in the transaction is gone"
        );

        // begin, execute, the sleep's query, the host's rollback.
        let entries = t.entries_eventually(&response, 4);
        let slept: Vec<&Value> = entries
            .iter()
            .filter(|e| {
                e["operation"] == "query"
                    && e["statement"]
                        .as_str()
                        .is_some_and(|s| s.contains("pg_sleep("))
            })
            .collect();
        let [slept] = slept.as_slice() else {
            panic!(
                "{context}: expected one entry of the sleep, got {}:\n{}",
                slept.len(),
                lines(&entries)
            )
        };
        assert_eq!(slept["outcome"], "failed", "{context}: {slept}");
        assert_eq!(slept["error"], "request-ended", "{context}: {slept}");
        assert_eq!(
            slept["transaction"].as_str(),
            Some(id.as_str()),
            "{context}: {slept}"
        );
        let rollback = the_rollback(&entries, &id, &context);
        assert_eq!(
            rollback["error"], "request-ended",
            "{context}: the host's rollback says why: {rollback}"
        );

        next_operations_are_clean(&t, target, &context);
    }
}
