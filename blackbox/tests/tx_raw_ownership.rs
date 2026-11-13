//! A transaction belongs to the request that began it (CONTRACT.md,
//! "Transactions: Through the raw interface"; ticket "Transactions through
//! the SDK", criterion 3, "a transaction handle kept past its request is
//! unusable"):
//!
//! - an id that is not an open transaction of the request in flight
//!   answers `no-transaction` for `query`, `execute`, `tx.commit` and
//!   `tx.rollback`: never begun, already committed, already rolled back,
//!   begun by another request (while that request is still in flight, and
//!   after it ended; by the same User, by another, in another
//!   Application), and the answer does not say which;
//! - nothing a second request sends with a stolen id runs in the first's
//!   transaction, and the first commits what it did, unharmed;
//! - code left running after its request (a timer holding the id) gets no
//!   allowed operation, and the transaction it held was rolled back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use blackbox::User;
use blackbox::records::{event, request_of};
use blackbox::scenario::RESOURCE;
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, Target, Txs, VIEWER, assert_began, assert_ended, assert_no_transaction, begin, commit,
    count_marker, counted, execute_in, insert_marker, later, query_in, rollback, stash_value, step,
    tx, wanda,
};
use serde_json::{Value, json};

/// The four uses of a transaction id, with this literal id.
fn every_use(id: &str, target: Target<'_>, marker: &str) -> Vec<Value> {
    vec![
        step(
            "query",
            json!({ "tx": id, "sql": count_marker(target, marker) }),
        ),
        step(
            "execute",
            json!({ "tx": id, "sql": insert_marker(target, marker) }),
        ),
        step("tx.commit", json!({ "tx": id })),
        step("tx.rollback", json!({ "tx": id })),
    ]
}

/// The four uses of the id kept under `name`.
fn every_use_of(name: &str, target: Target<'_>, marker: &str) -> Vec<Value> {
    vec![
        query_in(name, &count_marker(target, marker)),
        execute_in(name, &insert_marker(target, marker)),
        commit(name),
        rollback(name),
    ]
}

const USES: [&str; 4] = ["query", "execute", "tx.commit", "tx.rollback"];

/// The `error` of a `no-transaction` answer with every id in `ids`
/// replaced by `<id>`: what a caller learns from it.
fn what_it_says(answer: &Value, ids: &[String]) -> String {
    let mut text = answer["error"].to_string();
    for id in ids {
        if !id.is_empty() {
            text = text.replace(id.as_str(), "<id>");
        }
    }
    text
}

#[test]
fn an_id_never_begun_or_already_ended_answers_no_transaction_for_every_use() {
    let t = Txs::start();
    let user = wanda();
    let shared = Target::Shared(RESOURCE);
    let own = Target::Own("user");
    let marker = format!("ended-{}", token());
    let made_up = ["0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "made-up-id"];
    let mut steps = Vec::new();
    for id in made_up {
        steps.extend(every_use(id, shared, &marker));
    }
    steps.push(begin(shared, "write", "committed"));
    steps.push(commit("committed"));
    steps.extend(every_use_of("committed", shared, &marker));
    steps.push(begin(own, "write", "rolled"));
    steps.push(rollback("rolled"));
    steps.extend(every_use_of("rolled", own, &marker));
    let (_, body) = t.run_body(&user, APP, &steps);
    let answers = body["answers"].as_array().unwrap();
    let mut ids: Vec<String> = made_up.iter().map(|s| (*s).to_owned()).collect();
    for name in ["committed", "rolled"] {
        if let Some(id) = body["kept"][name].as_str() {
            ids.push(id.to_owned());
        }
    }
    // What each use answered, by use: (use, what it says).
    let mut said: Vec<(&str, String)> = Vec::new();
    for (i, id) in made_up.iter().enumerate() {
        for (j, use_) in USES.iter().enumerate() {
            let answer = &answers[i * 4 + j];
            assert_no_transaction(answer, &format!("{use_} with the id {id:?}, never begun"));
            said.push((use_, what_it_says(answer, &ids)));
        }
    }
    assert_began(&answers[8], "begin");
    assert_ended(&answers[9], "commit");
    for (j, use_) in USES.iter().enumerate() {
        let answer = &answers[10 + j];
        assert_no_transaction(answer, &format!("{use_} of a committed transaction"));
        said.push((use_, what_it_says(answer, &ids)));
    }
    assert_began(&answers[14], "begin");
    assert_ended(&answers[15], "rollback");
    for (j, use_) in USES.iter().enumerate() {
        let answer = &answers[16 + j];
        assert_no_transaction(answer, &format!("{use_} of a rolled-back transaction"));
        said.push((use_, what_it_says(answer, &ids)));
    }
    said.sort();
    said.dedup();
    assert_eq!(
        said.len(),
        USES.len(),
        "the answer does not say why an id is not an open transaction of the request: each use must answer the same whatever the reason, but these differ:\n{said:#?}"
    );
    assert_eq!(
        t.rows_with(shared, &marker),
        0,
        "nothing ran on the shared database"
    );
    assert_eq!(
        t.rows_with(own, &marker),
        0,
        "nothing ran on the own database"
    );
    t.db()
        .assert_not_received(&marker, "uses of ids that are not open transactions");
}

#[test]
fn an_id_begun_by_another_request_answers_no_transaction_while_it_runs_and_after() {
    let t = Txs::start();
    let user = wanda();
    for target in [Target::Shared(RESOURCE), Target::Own("user")] {
        let context = target.describe();
        let mine = format!("first-{}", token());
        let stolen = format!("stolen-{}", token());
        let key = format!("tx-{}", token());
        let first_done = AtomicBool::new(false);
        let (first, during, id) = thread::scope(|scope| {
            let first = scope.spawn(|| {
                let result = t.run(
                    &user,
                    APP,
                    &[
                        begin(target, "write", "t"),
                        execute_in("t", &insert_marker(target, &mine)),
                        stash_value(&key, &tx("t")),
                        query_in("t", "select pg_sleep(3) is null as slept"),
                        execute_in("t", &insert_marker(target, &mine)),
                        query_in("t", &count_marker(target, &stolen)),
                        commit("t"),
                    ],
                );
                first_done.store(true, Ordering::SeqCst);
                result
            });
            let id = t.stashed(&key, Duration::from_secs(10)).unwrap_or_else(|| {
                panic!("{context}: the first request never stashed its transaction id")
            });
            let mut during = Vec::new();
            for (who, application) in [
                (user.clone(), APP),
                (User::new("mallory").in_groups(&["writers", "readers"]), APP),
                (user.clone(), VIEWER),
            ] {
                let (_, answers) = t.run(&who, application, &every_use(&id, target, &stolen));
                during.push((who.id.clone(), application, answers));
            }
            assert!(
                !first_done.load(Ordering::SeqCst),
                "{context}: the harness's own check: the second requests must be made while the first is in flight (its pg_sleep(3)); make the sleep longer if this fails"
            );
            (first.join().unwrap(), during, id)
        });
        for (who, application, answers) in &during {
            for (use_, answer) in USES.iter().zip(answers) {
                assert_no_transaction(
                    answer,
                    &format!(
                        "{context}: {use_} by {who:?} in {application:?} with the id of a transaction another request holds open"
                    ),
                );
            }
        }
        let (_, answers) = first;
        assert_began(&answers[0], &context);
        assert_eq!(
            counted(&answers[5], &context),
            0,
            "{context}: nothing the other requests sent ran in the first's transaction"
        );
        assert_ended(
            &answers[6],
            &format!("{context}: the first request's commit, after the others tried its id"),
        );
        assert_eq!(
            t.rows_with(target, &mine),
            2,
            "{context}: the first request's transaction is unharmed"
        );
        assert_eq!(
            t.rows_with(target, &stolen),
            0,
            "{context}: nothing stolen ran"
        );
        if let Target::Shared(_) = target {
            t.db().assert_not_received(&stolen, &context);
        }
        // After the first request ended.
        let (_, answers) = t.run(&user, APP, &every_use(&id, target, &stolen));
        for (use_, answer) in USES.iter().zip(&answers) {
            assert_no_transaction(
                answer,
                &format!(
                    "{context}: {use_} with the id of another request's transaction, after it ended"
                ),
            );
        }
        assert_eq!(
            t.rows_with(target, &stolen),
            0,
            "{context}: nothing stolen ran"
        );
    }
}

#[test]
fn a_transaction_kept_past_its_request_is_unusable_and_was_rolled_back() {
    let t = Txs::start();
    let user = wanda();
    for target in [Target::Shared(RESOURCE), Target::Own("application")] {
        let context = target.describe();
        let during = format!("kept-during-{}", token());
        let after = format!("kept-after-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", &insert_marker(target, &during)),
                later(
                    300,
                    &[
                        execute_in("t", &insert_marker(target, &after)),
                        query_in("t", &count_marker(target, &during)),
                        commit("t"),
                    ],
                ),
            ],
        );
        assert_began(&answers[0], &context);
        assert_eq!(answers[2], json!({ "armed": true }));
        // Long enough for the timer to have fired, if it ever runs.
        thread::sleep(Duration::from_millis(1500));
        let records = t.vm().records();
        let leftover: Vec<&Value> = records
            .iter()
            .filter(|r| {
                event(r) == "printed"
                    && r["message"]
                        .as_str()
                        .is_some_and(|m| m.starts_with("LEFTOVER"))
            })
            .collect();
        for record in &leftover {
            let text = record["message"]
                .as_str()
                .unwrap()
                .trim_start_matches("LEFTOVER ");
            let answers: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            for answer in answers.as_array().into_iter().flatten() {
                let refused_no_request = answer["refused"]["refusal"]["reason"] == "no-request";
                let no_transaction = answer["error"]["kind"] == "no-transaction";
                assert!(
                    refused_no_request || no_transaction,
                    "{context}: code left running after its request must get no-request or no-transaction for every use of the id it kept, got {answer} in {record}"
                );
            }
        }
        let allowed_late: Vec<&Value> = records
            .iter()
            .filter(|r| {
                event(r) == "host-call" && r["outcome"] == "allowed" && request_of(r).is_none()
            })
            .collect();
        assert!(
            allowed_late.is_empty(),
            "{context}: no host call is allowed with no request in flight, got {allowed_late:?}"
        );
        assert_eq!(
            t.rows_with(target, &during),
            0,
            "{context}: the transaction the request left open was rolled back when it ended"
        );
        assert_eq!(
            t.rows_with(target, &after),
            0,
            "{context}: nothing the leftover code sent ran"
        );
        if let Target::Shared(_) = target {
            t.db().assert_not_received(&after, &context);
        }
        let entries = t.entries_eventually(&response, 3);
        let late: Vec<Value> = t
            .log()
            .into_iter()
            .filter(|e| e["outcome"] == "allowed" && e["request"].is_null())
            .collect();
        assert!(
            late.is_empty(),
            "{context}: no Operation log entry is allowed with no request in flight, got {late:?}"
        );
        assert!(
            entries
                .iter()
                .all(|e| e["outcome"] != "allowed" || e["operation"] != "commit"),
            "{context}: the kept transaction was never committed:\n{}",
            blackbox::scenario::lines(&entries)
        );
    }
}
