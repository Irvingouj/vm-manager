//! The limits of "Operations on a shared database" hold whatever settings
//! Application SQL changes (CONTRACT.md, "The login is the boundary"), and
//! a write ended by the time limit is rolled back ("Writing to a shared
//! database": rolled back when a limit ends it).
//!
//! Application SQL lifts every Postgres setting that could stand in for a
//! limit (`statement_timeout`, `lock_timeout`,
//! `idle_in_transaction_session_timeout`, `transaction_timeout` from
//! Postgres 17, `work_mem`), for the session on as many pooled connections
//! as it can reach and inside the very statement that should be ended,
//! always through `set_config` run by `ts_stat`, which a statement check
//! does not see (`docs/research/what-postgres-sees.md`: "A role-level
//! `statement_timeout` was lifted by `set_config`"). The time, row and byte
//! limits still end the statement.
//!
//! These tests wait for the ten-second time limit, so they live together in
//! this file and run beside each other.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, WRITERS, assert_entry, assert_executed, assert_limit,
    lines, op, sql_on,
};
use serde_json::{Value, json};

/// How long a request ended at the time limit may take to be answered.
const SLOW: Duration = Duration::from_secs(40);

/// `set_config(name, value, is_local)` run by `ts_stat`, as a relation of
/// exactly one row; for `transaction_timeout` only where the server has it
/// (Postgres 17 and later).
fn lift(name: &str, value: &str, local: bool) -> String {
    let set = format!("set_config('{name}', '{value}', {local})");
    let set = if name == "transaction_timeout" {
        format!(
            "case when current_setting('server_version_num')::int >= 170000 then {set} else 'unknown' end"
        )
    } else {
        set
    };
    format!(
        "(select * from ts_stat($vm$select to_tsvector('simple', 'set ' || {set})$vm$) limit 1) lifted_{name}"
    )
}

/// Every lift, for the session.
fn lifts() -> Vec<String> {
    [
        ("statement_timeout", "0"),
        ("lock_timeout", "0"),
        ("idle_in_transaction_session_timeout", "0"),
        ("transaction_timeout", "0"),
        ("work_mem", "1GB"),
    ]
    .iter()
    .map(|(name, value)| format!("select 1 as lifted from {}", lift(name, value, false)))
    .collect()
}

/// Lifts every setting for the session through `operation` on many
/// connections at once, failing the test unless each lift ran (the lifts
/// are honest SQL the read and write logins may run).
fn lift_everywhere(s: &Scenario, user: &User, application: &str, operation: &str) {
    let calls: Vec<Value> = lifts()
        .iter()
        .map(|sql| {
            let sql = if operation == "execute" {
                format!("insert into notes (body) select 'lifted' from ({sql}) l returning body")
            } else {
                sql.clone()
            };
            op(operation, &sql_on(RESOURCE, &sql))
        })
        .collect();
    let bodies: Vec<(User, &str, Value)> = (0..4)
        .map(|_| {
            (
                user.clone(),
                application,
                json!({ "calls": calls, "mode": "together" }),
            )
        })
        .collect();
    for (_, answers) in s.at_once(&bodies) {
        for answer in answers {
            assert!(
                answer["ok"].is_object(),
                "expected a lift of a setting to run (it is allowed SQL), got {answer}"
            );
        }
    }
    if operation == "execute" {
        s.db.superuser()
            .batch("delete from notes where body = 'lifted'");
    }
}

/// Fails unless the statement was ended at the time limit after about ten
/// seconds, and is no longer running in the shared database.
fn assert_ended_in_time(s: &Scenario, answer: &Value, took: Duration, context: &str) {
    assert_limit(answer, "time", context);
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(20),
        "{context}: expected the statement ended at about 10 seconds, the answer came after {took:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let running: Vec<String> =
            s.db.running()
                .into_iter()
                .filter(|q| q.contains("pg_sleep(60)"))
                .collect();
        if running.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: expected the statement ended in the shared database too, but it still runs: {running:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Sends the time, row and byte cases at once, each in its own request,
/// and returns each answer, how long its request took, and the request.
fn three_at_once(
    s: &Scenario,
    user: &User,
    application: &str,
    calls: &[Value; 3],
) -> Vec<(Value, Duration, blackbox::Response)> {
    let began = Instant::now();
    let bodies: Vec<(User, &str, Value)> = calls
        .iter()
        .map(|call| {
            (
                user.clone(),
                application,
                json!({ "calls": [call], "summary": true }),
            )
        })
        .collect();
    s.at_once_within(&bodies, SLOW)
        .into_iter()
        .map(|(response, answers)| {
            let took = began.elapsed();
            (
                answers.into_iter().next().unwrap_or(Value::Null),
                took,
                response,
            )
        })
        .collect()
}

#[test]
fn the_limits_end_a_read_whatever_settings_its_sql_changed() {
    let s = Scenario::writable();
    let rita = User::new("rita").in_groups(&[READERS]);
    lift_everywhere(&s, &rita, "viewer", "query");
    let results = three_at_once(
        &s,
        &rita,
        "viewer",
        &[
            op(
                "query",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "select pg_sleep(60) as slept from {}, {}",
                        lift("statement_timeout", "0", true),
                        lift("transaction_timeout", "0", true)
                    ),
                ),
            ),
            op(
                "query",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "select g from generate_series(1, 10001) g, {}",
                        lift("work_mem", "1GB", true)
                    ),
                ),
            ),
            op(
                "query",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "select repeat('x', 1048576) as x from generate_series(1, 12), {}",
                        lift("work_mem", "1GB", true)
                    ),
                ),
            ),
        ],
    );
    assert_ended_in_time(
        &s,
        &results[0].0,
        results[0].1,
        "a read sleeping 60 seconds after lifting the timeouts",
    );
    assert_limit(
        &results[1].0,
        "rows",
        "a read of 10,001 rows after lifting work_mem",
    );
    assert_limit(
        &results[2].0,
        "bytes",
        "a read of 12 MiB after lifting work_mem",
    );
    for ((_, _, response), limit) in results.iter().zip(["time", "rows", "bytes"]) {
        let id = response.request_id();
        assert_entry(
            &s.entry_of(response),
            &Expected::failed(&id, "rita", "viewer", limit),
            &format!("the entry of a read ended at the {limit} limit"),
        );
    }
}

#[test]
fn the_limits_end_a_write_whatever_settings_its_sql_changed_and_it_is_rolled_back() {
    let s = Scenario::writable();
    let wanda = User::new("wanda").in_groups(&[WRITERS]);
    lift_everywhere(&s, &wanda, "board", "execute");
    let before = s.db.fingerprint();
    let results = three_at_once(
        &s,
        &wanda,
        "board",
        &[
            op(
                "execute",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "update incidents set title = 'slow' || pg_sleep(60)::text from {}, {} where id = 1",
                        lift("statement_timeout", "0", true),
                        lift("lock_timeout", "0", true)
                    ),
                ),
            ),
            op(
                "execute",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "insert into notes (body) select 'n' || g from generate_series(1, 10001) g, {} returning id",
                        lift("work_mem", "1GB", true)
                    ),
                ),
            ),
            op(
                "execute",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "insert into notes (body) select 'big' from generate_series(1, 12), {} returning repeat('x', 1048576) as big",
                        lift("work_mem", "1GB", true)
                    ),
                ),
            ),
        ],
    );
    assert_ended_in_time(
        &s,
        &results[0].0,
        results[0].1,
        "a write sleeping 60 seconds after lifting the timeouts",
    );
    assert_limit(
        &results[1].0,
        "rows",
        "a write returning 10,001 rows after lifting work_mem",
    );
    assert_limit(
        &results[2].0,
        "bytes",
        "a write returning 12 MiB after lifting work_mem",
    );
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected every write ended at a limit rolled back"
    );
    for ((_, _, response), limit) in results.iter().zip(["time", "rows", "bytes"]) {
        let id = response.request_id();
        let entries = s.entries_of(response);
        assert_eq!(entries.len(), 1, "{}", lines(&entries));
        assert_entry(
            &entries[0],
            &Expected {
                operation: "execute",
                ..Expected::failed(&id, "wanda", "board", limit)
            },
            &format!("the entry of a write ended at the {limit} limit"),
        );
    }
    // The row the slow update held is free again: another write to it
    // goes through at once.
    let began = Instant::now();
    let (_, answer) = s.execute(
        &wanda,
        "board",
        &sql_on(
            RESOURCE,
            "update incidents set severity = 5 where id = 1 returning id",
        ),
    );
    assert_executed(
        &answer,
        &["id"],
        &json!([[1]]),
        1,
        "a write to the row the ended update held",
    );
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "expected the row free at once after the slow update was ended, the write took {:?}",
        began.elapsed()
    );
}
