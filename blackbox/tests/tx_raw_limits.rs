//! Limits inside a transaction (CONTRACT.md, "Transactions: What the host
//! does on its own", and "Through the raw interface"):
//!
//! - a statement the host ends for a limit (rows, bytes) ends its
//!   transaction too: rolled back, then `no-transaction`, and the host's
//!   `rollback` entry names the limit;
//! - at most 4 transactions open at once per request: a fifth `tx.begin`
//!   answers the `transactions` limit; once one ends, another may begin;
//! - each statement in a transaction is decided again: a Grant lowered
//!   between two statements of a transaction refuses the second.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::User;
use blackbox::scenario::{CONTACT, RESOURCE, WRITERS, assert_limit, assert_refusal, lines};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, Target, Txs, assert_began, assert_ended, assert_no_transaction, begin, commit,
    count_marker, counted, execute_in, insert_marker, query_in, sleep, the_rollback, wanda,
};
use serde_json::json;

#[test]
fn a_statement_ended_for_a_limit_ends_its_transaction() {
    let t = Txs::start();
    let user = wanda();
    let cases = [
        (
            Target::Shared(RESOURCE),
            "rows",
            "select g from generate_series(1, 20000) g",
        ),
        (
            Target::Own("user"),
            "rows",
            "select g from generate_series(1, 20000) g",
        ),
        (
            Target::Shared(RESOURCE),
            "bytes",
            "select repeat('x', 1048576) as big from generate_series(1, 10)",
        ),
        (
            Target::Own("application"),
            "bytes",
            "select repeat('x', 1048576) as big from generate_series(1, 10)",
        ),
    ];
    for (target, limit, sql) in cases {
        let context = format!("{} at the {limit} limit", target.describe());
        let marker = format!("limited-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", &insert_marker(target, &marker)),
                query_in("t", sql),
                query_in("t", &count_marker(target, &marker)),
                commit("t"),
            ],
        );
        let id = assert_began(&answers[0], &context);
        assert_limit(&answers[2], limit, &context);
        assert_no_transaction(
            &answers[3],
            &format!("{context}: a statement after the limit ended the transaction"),
        );
        assert_no_transaction(&answers[4], &format!("{context}: the commit"));
        assert_eq!(t.rows_with(target, &marker), 0, "{context}: rolled back");
        let entries = t.entries_eventually(&response, 4);
        let rollback = the_rollback(&entries, &id, &context);
        assert_eq!(
            rollback["error"],
            limit,
            "{context}: the host's rollback names the limit: {rollback}\n{}",
            lines(&entries)
        );
    }
}

#[test]
fn a_fifth_transaction_open_in_one_request_answers_the_transactions_limit() {
    let t = Txs::start();
    let user = wanda();
    let marker = format!("fifth-{}", token());
    let own = Target::Own("user");
    let (_, answers) = t.run(
        &user,
        APP,
        &[
            begin(Target::Shared(RESOURCE), "write", "a"),
            begin(Target::Shared(RESOURCE), "read", "b"),
            begin(Target::Own("user"), "write", "c"),
            begin(Target::Own("application"), "read", "d"),
            begin(own, "write", "e"),
            commit("a"),
            begin(own, "write", "f"),
            execute_in("f", &insert_marker(own, &marker)),
            commit("f"),
            commit("b"),
            commit("c"),
            commit("d"),
            begin(own, "write", "g"),
            query_in("g", &count_marker(own, &marker)),
            commit("g"),
        ],
    );
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        assert_began(&answers[i], &format!("transaction {name}, one of four"));
    }
    assert_limit(
        &answers[4],
        "transactions",
        "a fifth tx.begin with four open",
    );
    assert_ended(&answers[5], "commit of the first");
    assert_began(&answers[6], "a tx.begin once one of the four ended");
    assert_ended(&answers[8], "commit");
    for answer in &answers[9..12] {
        assert_ended(answer, "commit");
    }
    assert_began(&answers[12], "a tx.begin with none open");
    assert_eq!(counted(&answers[13], "count"), 1);
    assert_ended(&answers[14], "commit");
}

#[test]
fn a_grant_lowered_between_two_statements_of_a_transaction_refuses_the_second() {
    let t = Txs::start();
    let walt = User::new("walt").in_groups(&[WRITERS]);
    let first = format!("before-lowering-{}", token());
    let second = format!("after-lowering-{}", token());
    let target = Target::Shared(RESOURCE);
    let answers = thread::scope(|scope| {
        let request = scope.spawn(|| {
            t.run(
                &walt,
                APP,
                &[
                    begin(target, "write", "t"),
                    execute_in("t", &insert_marker(target, &first)),
                    sleep(3_000),
                    execute_in("t", &insert_marker(target, &second)),
                    commit("t"),
                ],
            )
        });
        assert!(
            t.db().wait_until_received(&first, Duration::from_secs(10)),
            "the harness's own check: the first statement must have reached the database"
        );
        t.scenario.grant(RESOURCE, WRITERS, "read");
        request.join().unwrap().1
    });
    assert_began(&answers[0], "begin while walt's Group held write");
    assert_eq!(answers[1]["ok"]["rows_affected"], 1, "{}", answers[1]);
    assert_refusal(
        &answers[3],
        "no-grant",
        &json!({ "resource": RESOURCE, "user": "walt", "needed": "write", "held": "read", "contact": CONTACT }),
        "a statement of the transaction after the Grant was lowered to read",
    );
    assert_eq!(
        t.shared_notes_with(&second),
        0,
        "the refused statement changed nothing"
    );
    t.db().assert_not_received(&second, "the refused statement");
}
