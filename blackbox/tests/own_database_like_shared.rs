//! On the own database everything else is as on a shared database
//! (CONTRACT.md, "The Application's own database: Operations on the own
//! database"): `query` runs in a read-only transaction, so a write through
//! it fails in the database; `execute` commits when its statement succeeds
//! and rolls back when it fails or a limit ends it; the limits hold, also
//! after Application SQL changes settings; the statement check applies;
//! database errors are answered and logged as on a shared database; and
//! nothing one operation leaves in its session reaches a later one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::User;
use blackbox::own_database::{
    ACTORS, Own, assert_did_not_run, own_args, own_execute, own_query, standard,
};
use blackbox::scenario::{
    Expected, assert_database_error, assert_entry, assert_limit, assert_malformed, assert_rows,
    assert_statement_refused, op,
};
use blackbox::shared_database::{Session, observe_insert, observe_select};
use serde_json::{Value, json};

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

/// SQL text run by `ts_stat`, out of a statement parser's sight.
fn hidden_call(expression: &str) -> String {
    format!(
        "select * from ts_stat($vm$select to_tsvector('simple', 'called ' || coalesce(({expression})::text, ''))$vm$)"
    )
}

#[test]
fn a_write_through_query_fails_in_the_database_as_either_actor() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let before = db.fingerprint();
    let writes = [
        "insert into items (title) values ('through a read') returning id".to_owned(),
        "update seeded set label = 'changed' returning id".to_owned(),
        "delete from seeded returning id".to_owned(),
        "with i as (insert into notes (body) values ('cte') returning id) select id from i"
            .to_owned(),
        "select run_sql('insert into notes (body) values (''through run_sql'')')".to_owned(),
        "select run_sql('delete from seeded')".to_owned(),
        "select nextval('items_id_seq')".to_owned(),
        hidden_call("run_sql('delete from seeded')"),
        hidden_call("set_config('transaction_read_only', 'off', false)"),
        format!(
            "select run_sql('delete from seeded') from ({}) h",
            hidden_call("set_config('transaction_read_only', 'off', true)")
        ),
    ];
    for actor in ACTORS {
        let calls: Vec<Value> = writes.iter().map(|sql| own_query(actor, sql)).collect();
        let (_, answers) = own.run(&ann(), "board", &calls);
        let mut read_only = 0;
        for (sql, answer) in writes.iter().zip(&answers) {
            let code = assert_did_not_run(answer, &format!("{sql} through query as the {actor}"));
            if code.as_deref() == Some("25006") {
                read_only += 1;
            }
        }
        assert!(
            read_only >= 5,
            "expected writes through query to fail in the database as a read-only transaction (25006); answers:\n{answers:#?}"
        );
        // And then, in a later operation, a write through query still fails.
        let (_, answer) = own.one(
            &ann(),
            "board",
            &own_query(
                actor,
                "insert into items (title) values ('later') returning id",
            ),
        );
        assert_database_error(&answer, "25006", "a write through a later query");
    }
    assert_eq!(
        db.fingerprint(),
        before,
        "expected nothing changed in the own database by writes through query"
    );
}

#[test]
fn execute_commits_when_its_statement_succeeds_and_rolls_back_when_it_fails() {
    let own = Own::start();
    let db = own.install("board", &standard());
    for actor in ACTORS {
        let tag = format!("by the {actor}");
        let (_, answers) = own.run(
            &ann(),
            "board",
            &[
                own_execute(
                    actor,
                    &format!(
                        "insert into items (title) select '{tag} ' || n from generate_series(1, 3) n returning title"
                    ),
                ),
                own_query(actor, &format!("select count(*)::int4 as n from items where title like '{tag}%'")),
                // A statement that fails after writing a row: nothing of it stays.
                own_execute(
                    actor,
                    "insert into seeded values (50, 'fifty'), (1, 'a key that is taken') returning id",
                ),
                own_query(actor, "select count(*)::int4 as n from seeded where id = 50"),
                // A write ended at a limit: rolled back.
                own_execute(
                    actor,
                    "insert into notes (body) select 'n' || n from generate_series(1, 10001) n returning body",
                ),
                own_query(actor, "select count(*)::int4 as n from notes"),
            ],
        );
        assert_eq!(
            answers[0]["ok"]["rows_affected"],
            json!(3),
            "{}",
            answers[0]
        );
        assert_rows(
            &answers[1],
            &["n"],
            &json!([[3]]),
            "the committed rows, read in a later operation",
        );
        assert_database_error(
            &answers[2],
            "23505",
            "an insert that fails on its second row",
        );
        assert_rows(
            &answers[3],
            &["n"],
            &json!([[0]]),
            "nothing of the failed insert",
        );
        assert_limit(
            &answers[4],
            "rows",
            "an insert returning more than 10,000 rows",
        );
        assert_rows(
            &answers[5],
            &["n"],
            &json!([[0]]),
            "nothing of the insert ended at the limit",
        );
    }
    assert_eq!(db.column("SELECT count(*) FROM items"), vec!["6"]);
    assert_eq!(db.column("SELECT count(*) FROM seeded"), vec!["1"]);
    assert_eq!(db.column("SELECT count(*) FROM notes"), vec!["0"]);
}

#[test]
fn the_row_and_byte_limits_hold_on_the_own_database() {
    let own = Own::start();
    own.install("board", &standard());
    for actor in ACTORS {
        let (_, answers) = own.run(
            &ann(),
            "board",
            &[
                own_query(actor, "select n from generate_series(1, 10001) n"),
                own_query(actor, "select n from generate_series(1, 10000) n"),
                own_query(actor, "select repeat('x', 1048576) as x from generate_series(1, 9)"),
                own_execute(actor, "select n from generate_series(1, 10001) n"),
                own_query(
                    actor,
                    "select n from generate_series(1, 10001) n where run_sql('set work_mem = ''64MB''') = 1",
                ),
            ],
        );
        assert_limit(
            &answers[0],
            "rows",
            &format!("10,001 rows read as the {actor}"),
        );
        assert_eq!(
            answers[1]["ok"]["row_count"],
            json!(10000),
            "10,000 rows are not over the limit"
        );
        assert_limit(&answers[2], "bytes", &format!("9 MiB read as the {actor}"));
        assert_limit(
            &answers[3],
            "rows",
            &format!("10,001 rows through execute as the {actor}"),
        );
        assert_limit(
            &answers[4],
            "rows",
            "10,001 rows after a setting changed in the statement",
        );
    }
}

#[test]
fn the_time_limit_holds_on_the_own_database_whatever_settings_application_sql_changes() {
    let own = Own::start();
    own.install("board", &standard());
    let body = json!({
        "calls": [
            own_query("user", "select pg_sleep(11)"),
            own_execute("application", "select pg_sleep(11)"),
            own_query("application", "select run_sql('set statement_timeout = 0') as a, pg_sleep(11) as b"),
            own_execute("user", "select run_sql('set local statement_timeout = 0') as a, pg_sleep(11) as b"),
        ],
        "mode": "together",
    });
    let (_, answers) = own.run_body(&ann(), "board", &body, Duration::from_secs(40));
    for (i, answer) in answers.iter().enumerate() {
        assert_limit(
            answer,
            "time",
            &format!("statement {i}, sleeping past 10 seconds"),
        );
    }
}

#[test]
fn the_statement_check_applies_on_the_own_database() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let before = db.fingerprint();
    let cases = [
        ("select 1; select 2", "several-statements"),
        (
            "insert into notes (body) values ('a'); delete from seeded",
            "several-statements",
        ),
        ("selec 1", "unparsed"),
        ("create table sneaky (x int)", "kind-not-allowed"),
        ("truncate seeded", "kind-not-allowed"),
        ("drop table items", "kind-not-allowed"),
        ("set role none", "kind-not-allowed"),
        ("", "kind-not-allowed"),
        (
            "select set_config('role', 'none', false)",
            "function-not-allowed",
        ),
        ("select pg_terminate_backend(0)", "function-not-allowed"),
        ("select lo_from_bytea(0, 'x')", "function-not-allowed"),
        ("select query from pg_stat_activity", "relation-not-allowed"),
    ];
    for actor in ACTORS {
        for operation in ["query", "execute"] {
            let calls: Vec<Value> = cases
                .iter()
                .map(|(sql, _)| op(operation, &own_args(actor, sql)))
                .collect();
            let (response, answers) = own.run(&ann(), "board", &calls);
            for ((sql, reason), answer) in cases.iter().zip(&answers) {
                assert_statement_refused(
                    answer,
                    reason,
                    &format!("{sql:?} through {operation} as the {actor}"),
                );
            }
            let entries = own.entries_of(&response);
            assert_eq!(
                entries.len(),
                cases.len(),
                "one entry per refused statement"
            );
            let id = response.request_id();
            for entry in &entries {
                assert_entry(
                    entry,
                    &Expected {
                        ran: Some(false),
                        ..Expected {
                            outcome: "failed",
                            error: Some("statement"),
                            ..Expected::own(&id, "ann", "board", actor, operation)
                        }
                    },
                    "the entry of a statement the check refused on the own database",
                );
            }
        }
    }
    assert_eq!(db.fingerprint(), before);
}

#[test]
fn errors_and_malformed_arguments_are_answered_as_on_a_shared_database() {
    let own = Own::start();
    own.install("board", &standard());
    for actor in ACTORS {
        let (response, answer) = own.one(
            &ann(),
            "board",
            &own_query(actor, "select * from missing_table"),
        );
        assert_database_error(&answer, "42P01", "a table that does not exist");
        let id = response.request_id();
        assert_entry(
            &own.entry_of(&response),
            &Expected {
                outcome: "failed",
                error: Some("42P01"),
                ran: None,
                ..Expected::own(&id, "ann", "board", actor, "query")
            },
            "the entry of a statement that failed in the own database",
        );
        let mut args = own_args(
            actor,
            "select $1::text as a, $2::int4 as b, $3::bool as c, $4::text as d",
        );
        args["params"] = json!(["x", 7, true, null]);
        let (_, answer) = own.one(&ann(), "board", &op("query", &args));
        assert_rows(
            &answer,
            &["a", "b", "c", "d"],
            &json!([["x", 7, true, null]]),
            "params on the own database",
        );
        for bad in [json!([{ "x": 1 }]), json!("not an array"), json!([[1]])] {
            let mut args = own_args(actor, "select $1::text as a");
            args["params"] = bad.clone();
            let (response, answer) = own.one(&ann(), "board", &op("execute", &args));
            assert_malformed(&answer, &format!("params {bad}"));
            assert!(
                own.entries_of(&response).is_empty(),
                "no entry for a malformed operation"
            );
        }
        let (_, answer) = own.one(
            &ann(),
            "board",
            &op("query", &json!({ "own": true, "as": actor })),
        );
        assert_malformed(&answer, "no sql");
    }
}

#[test]
fn nothing_one_operation_leaves_in_its_session_reaches_a_later_one() {
    let own = Own::start();
    own.install("board", &standard());
    // Leave state from many sessions at once, as both actors, through query
    // and execute.
    let leave: Vec<(User, &str, Value)> = (0..4)
        .map(|i| {
            let actor = ACTORS[i % 2];
            let call = if i < 2 {
                own_execute(actor, "select leave_session_state()")
            } else {
                own_query(actor, "select leave_session_state(), pg_sleep(0.1)")
            };
            (
                User::new(&format!("leaver-{i}")),
                "board",
                json!({ "calls": vec![call; 6], "mode": "together" }),
            )
        })
        .collect();
    for (_, answers) in own.at_once(&leave) {
        for answer in answers {
            assert!(answer["ok"].is_object(), "leaving session state: {answer}");
        }
    }
    let observe: Vec<(User, &str, Value)> = (0..4)
        .map(|i| {
            let actor = ACTORS[i % 2];
            let call = if i < 2 {
                op("query", &own_args(actor, &observe_select(100)))
            } else {
                op("execute", &own_args(actor, &observe_insert(100)))
            };
            (
                User::new(&format!("observer-{i}")),
                "board",
                json!({ "calls": vec![call; 6], "mode": "together" }),
            )
        })
        .collect();
    let mut leftovers = Vec::new();
    for (_, answers) in own.at_once(&observe) {
        for answer in answers {
            let session = Session::from_answer(&answer)
                .unwrap_or_else(|| panic!("expected an observation of the session, got {answer}"));
            let left = session.leftovers();
            if !left.is_empty() {
                leftovers.push(format!("  session {}: {}", session.pid, left.join(", ")));
            }
        }
    }
    assert!(
        leftovers.is_empty(),
        "expected no session state to survive from one own-database operation to the next, but:\n{}",
        leftovers.join("\n")
    );
}
