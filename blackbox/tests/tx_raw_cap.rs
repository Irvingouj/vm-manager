//! The cap: a transaction may be open for 10 seconds (CONTRACT.md,
//! "Transactions: What the host does on its own"; ticket "Transactions
//! through the SDK", criterion 4, "A transaction that runs past the cap is
//! ended").
//!
//! One test, because it waits for the cap: three requests at once.
//!
//! - A begins a write transaction on `devops`, writes, then runs
//!   `pg_sleep(30)` in it: the statement is cancelled at the cap (`limit`
//!   `time`), everything is rolled back, later uses of the id answer
//!   `no-transaction`.
//! - B begins a write transaction on the own database, writes, then waits
//!   12 s with no statement running: the host ends it at the cap on its
//!   own. Postgres shows no session of the runtime role still in a
//!   transaction while B's request is still in flight; later uses of the
//!   id answer `no-transaction`.
//! - C begins a read transaction, waits 7 s, then runs `pg_sleep(8)`: the
//!   statement is cut at the transaction's remaining time (about 3 s), not
//!   at the 10 s a statement may otherwise run.
//!
//! Each has a `rollback` entry in the Operation log with `error` `time`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::scenario::{RESOURCE, assert_limit, lines};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, Target, Txs, assert_began, assert_no_transaction, begin, commit, count_marker, execute_in,
    in_transaction, insert_marker, query_in, sleep, slow_select, the_rollback, wanda,
};
use serde_json::Value;

#[test]
fn a_transaction_open_past_the_cap_is_ended_and_its_statements_cut_at_the_remaining_time() {
    let t = Txs::start();
    let user = wanda();
    let a = Target::Shared(RESOURCE);
    let b = Target::Own("user");
    let mark_a = format!("capped-a-{}", token());
    let mark_b = format!("capped-b-{}", token());
    let mark_c = format!("capped-c-{}", token());
    let steps_a = vec![
        begin(a, "write", "t"),
        execute_in("t", &insert_marker(a, &mark_a)),
        query_in("t", &slow_select(30.0, &mark_a)),
        query_in("t", &count_marker(a, &mark_a)),
        execute_in("t", &insert_marker(a, &mark_a)),
        commit("t"),
    ];
    let steps_b = vec![
        begin(b, "write", "t"),
        execute_in("t", &insert_marker(b, &mark_b)),
        sleep(12_000),
        query_in("t", &count_marker(b, &mark_b)),
        execute_in("t", &insert_marker(b, &mark_b)),
        commit("t"),
    ];
    let steps_c = vec![
        begin(a, "read", "t"),
        sleep(7_000),
        query_in("t", &slow_select(8.0, &mark_c)),
        query_in("t", "select 1 as one"),
        commit("t"),
    ];
    let started = Instant::now();
    let (ra, rb, rc, still_in_b) = thread::scope(|scope| {
        let ha = scope.spawn(|| {
            let r = t.run(&user, APP, &steps_a);
            (r, started.elapsed())
        });
        let hb = scope.spawn(|| t.run(&user, APP, &steps_b));
        let hc = scope.spawn(|| {
            let r = t.run(&user, APP, &steps_c);
            (r, started.elapsed())
        });
        // Past the cap, while B's request still waits (until about 12 s):
        // the host must have ended B's transaction on its own.
        thread::sleep(Duration::from_millis(11_000).saturating_sub(started.elapsed()));
        let (database, roles) = t.logins_of(b);
        let sql = t.own.superuser();
        let still = blackbox::transactions::wait_for_none(Duration::from_millis(700), || {
            in_transaction(&sql, &database, &roles)
        });
        (
            ha.join().unwrap(),
            hb.join().unwrap(),
            hc.join().unwrap(),
            still,
        )
    });
    assert!(
        still_in_b.is_empty(),
        "B: past the cap, with B's request still in flight, Postgres still shows its transaction open:\n{still_in_b:#?}"
    );

    // A: the running statement cancelled at the cap.
    let ((response_a, answers_a), elapsed_a) = ra;
    let id_a = assert_began(&answers_a[0], "A");
    assert_limit(
        &answers_a[2],
        "time",
        "A: pg_sleep(30) in a transaction past the cap",
    );
    for (i, what) in [(3, "query"), (4, "execute"), (5, "tx.commit")] {
        assert_no_transaction(
            &answers_a[i],
            &format!("A: {what} after the host ended the transaction"),
        );
    }
    assert!(
        elapsed_a >= Duration::from_millis(9_500) && elapsed_a < Duration::from_secs(14),
        "A: expected the statement cancelled about 10 s after the begin, the request took {elapsed_a:?}"
    );
    assert_eq!(t.rows_with(a, &mark_a), 0, "A: everything was rolled back");

    // B: ended with no statement running.
    let (response_b, answers_b) = rb;
    let id_b = assert_began(&answers_b[0], "B");
    for (i, what) in [(3, "query"), (4, "execute"), (5, "tx.commit")] {
        assert_no_transaction(&answers_b[i], &format!("B: {what} after the cap"));
    }
    assert_eq!(t.rows_with(b, &mark_b), 0, "B: everything was rolled back");

    // C: cut at the transaction's remaining time.
    let ((response_c, answers_c), elapsed_c) = rc;
    let id_c = assert_began(&answers_c[0], "C");
    assert_limit(
        &answers_c[2],
        "time",
        "C: pg_sleep(8) begun 7 s into the transaction",
    );
    assert!(
        elapsed_c >= Duration::from_millis(9_500) && elapsed_c < Duration::from_millis(12_500),
        "C: expected the statement cut at the transaction's remaining time (about 10 s after the begin, 3 s into the statement), the request took {elapsed_c:?}"
    );
    for (i, what) in [(3, "query"), (4, "tx.commit")] {
        assert_no_transaction(
            &answers_c[i],
            &format!("C: {what} after a statement the host ended for a limit"),
        );
    }

    for (name, response, id) in [
        ("A", &response_a, &id_a),
        ("B", &response_b, &id_b),
        ("C", &response_c, &id_c),
    ] {
        let entries = t.entries_eventually(response, 3);
        let rollback = the_rollback(&entries, id, name);
        assert_eq!(
            rollback["error"],
            "time",
            "{name}: the host's rollback at the cap says why, `time`: {rollback}\n{}",
            lines(&entries)
        );
        if name != "B" {
            let cut: Vec<&Value> = entries
                .iter()
                .filter(|e| e["operation"] == "query" && e["error"] == "time")
                .collect();
            assert_eq!(
                cut.len(),
                1,
                "{name}: the cut statement has its entry, `failed` with error `time`:\n{}",
                lines(&entries)
            );
            assert_eq!(cut[0]["outcome"], "failed", "{name}: {}", cut[0]);
            assert_eq!(
                cut[0]["transaction"].as_str(),
                Some(id.as_str()),
                "{name}: {}",
                cut[0]
            );
        }
    }
    let left = t.sessions_in_transaction(a, Duration::from_secs(3));
    assert!(
        left.is_empty(),
        "no shared-database session left in a transaction: {left:#?}"
    );
}
