//! Transactions through the raw interface (CONTRACT.md, "Transactions:
//! Through the raw interface"; ticket "Transactions through the SDK",
//! criteria 1, 2 and 5): `tx.begin`, `query` and `execute` with `tx`,
//! `tx.commit` and `tx.rollback`, on a shared database at `write` and at
//! `read`, and on the own database as either actor.
//!
//! - Several statements take effect together at the commit; a statement
//!   in the transaction reads its own writes, another transaction does not
//!   until the commit; a rollback keeps nothing.
//! - `read` begins a read-only transaction as the read login (or the
//!   runtime role); a write in it fails in the database or is refused as it
//!   would be outside.
//! - `isolation` reaches Postgres.
//! - A statement Postgres refuses leaves the transaction aborted (`25P02`
//!   after it), and committing it answers `transaction-aborted`.
//! - The statement check still applies inside a transaction.
//! - The Operation log: `begin`, each statement, `commit` / `rollback`,
//!   all carrying the transaction's id; statements outside carry null.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{
    Expected, RESOURCE, assert_database_error, assert_entry, assert_error_kind, assert_refusal,
    assert_statement_refused, lines,
};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, TARGETS, Target, TxEntry, Txs, VIEWER, assert_began, assert_ended, assert_no_transaction,
    assert_tx_entry, begin, begin_with, commit, count_marker, counted, execute_in, insert_marker,
    outside, query_in, rita, rollback, wanda,
};
use serde_json::{Value, json};

fn actor_of(target: Target<'_>) -> &'static str {
    match target {
        Target::Own("application") => "application",
        _ => "user",
    }
}

fn resource_of(target: Target<'_>) -> Option<&str> {
    match target {
        Target::Shared(r) => Some(r),
        Target::Own(_) => None,
    }
}

#[test]
fn statements_in_a_transaction_take_effect_together_at_the_commit() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let context = target.describe();
        let marker = format!("together-{}", token());
        let steps = [
            begin(target, "write", "t"),
            execute_in("t", &insert_marker(target, &marker)),
            execute_in("t", &insert_marker(target, &marker)),
            query_in("t", &count_marker(target, &marker)),
            outside("query", target, &count_marker(target, &marker)),
            commit("t"),
            outside("query", target, &count_marker(target, &marker)),
        ];
        let (response, answers) = t.run(&user, APP, &steps);
        let id = assert_began(&answers[0], &context);
        for (i, answer) in answers[1..3].iter().enumerate() {
            assert!(
                answer["ok"]["rows_affected"] == 1,
                "{context}: expected insert {i} in the transaction to run, got {answer}"
            );
        }
        assert_eq!(
            counted(&answers[3], &context),
            2,
            "{context}: a statement in the transaction must read the transaction's own writes"
        );
        assert_eq!(
            counted(&answers[4], &context),
            0,
            "{context}: an operation outside the transaction must not see its writes before the commit"
        );
        assert_ended(&answers[5], &format!("{context}: tx.commit"));
        assert_eq!(
            counted(&answers[6], &context),
            2,
            "{context}: after the commit both rows are there"
        );
        assert_eq!(
            t.rows_with(target, &marker),
            2,
            "{context}: the harness's own connection must see both rows after the commit"
        );

        // The Operation log: begin, the three statements in the
        // transaction carrying its id, the one outside carrying null,
        // commit, then the last one outside.
        let request = response.request_id();
        let entries = t.entries_of(&response);
        assert_eq!(
            entries.len(),
            7,
            "{context}: expected one Operation log entry per operation, begin and commit included, got:\n{}",
            lines(&entries)
        );
        let resource = resource_of(target);
        let actor = actor_of(target);
        let tx_entry = |operation| TxEntry {
            operation,
            transaction: Some(&id),
            outcome: "allowed",
            reason: None,
            error: None,
            resource,
            actor,
            request: &request,
            user: &user.id,
            application: APP,
        };
        assert_tx_entry(
            &entries[0],
            &tx_entry("begin"),
            &format!("{context}: begin"),
        );
        assert_tx_entry(
            &entries[5],
            &tx_entry("commit"),
            &format!("{context}: commit"),
        );
        for (index, operation, transaction) in [
            (1, "execute", Some(id.as_str())),
            (2, "execute", Some(id.as_str())),
            (3, "query", Some(id.as_str())),
            (4, "query", None),
            (6, "query", None),
        ] {
            let entry = &entries[index];
            let expected = Expected {
                resource,
                actor: Some(actor),
                own_database: Some(resource.is_none()),
                operation,
                ..Expected::allowed(&request, &user.id, APP)
            };
            assert_entry(entry, &expected, &format!("{context}: entry {index}"));
            assert_eq!(
                entry.get("transaction"),
                Some(&json!(transaction)),
                "{context}: entry {index} must carry `transaction` {transaction:?}: {entry}"
            );
        }
        assert_eq!(
            entries[1]["rows"], 1,
            "{context}: an insert's entry records the rows affected"
        );
    }
}

#[test]
fn the_identity_label_on_a_shared_database_holds_for_the_whole_transaction() {
    let t = Txs::start();
    let user = wanda();
    let marker = format!("label-{}", token());
    let target = Target::Shared(RESOURCE);
    let (response, answers) = t.run(
        &user,
        APP,
        &[
            begin(target, "write", "t"),
            query_in(
                "t",
                "select current_setting('vm.user', true) as u, current_setting('vm.application', true) as a, current_setting('vm.request', true) as r",
            ),
            execute_in("t", &insert_marker(target, &marker)),
            execute_in("t", &insert_marker(target, &marker)),
            commit("t"),
        ],
    );
    assert_began(&answers[0], "begin");
    let request = response.request_id();
    assert_eq!(
        answers[1]["ok"]["rows"],
        json!([[user.id, APP, request]]),
        "the label is set when the transaction begins: vm.user, vm.application, vm.request"
    );
    assert_ended(&answers[4], "commit");
    let labels = t.db().superuser().batch(&format!(
        "SELECT label_user, label_application, label_request, written_by FROM notes WHERE body = '{marker}'"
    ));
    assert_eq!(labels.len(), 2, "both rows committed: {labels:?}");
    for row in &labels {
        assert_eq!(
            row[..3],
            [
                Some(user.id.clone()),
                Some(APP.to_owned()),
                Some(request.clone())
            ],
            "every row written in the transaction carries the label of its request, as the owner's trigger read it: {labels:?}"
        );
        assert_eq!(
            row[3].as_deref(),
            Some(t.db().writer.role.as_str()),
            "a transaction begun at `write` runs as the Resource's write login: {labels:?}"
        );
    }
}

#[test]
fn a_rollback_keeps_nothing() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let context = target.describe();
        let marker = format!("rolled-back-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", &insert_marker(target, &marker)),
                execute_in("t", &insert_marker(target, &marker)),
                rollback("t"),
                outside("query", target, &count_marker(target, &marker)),
            ],
        );
        let id = assert_began(&answers[0], &context);
        assert_ended(&answers[3], &format!("{context}: tx.rollback"));
        assert_eq!(counted(&answers[4], &context), 0, "{context}: nothing kept");
        assert_eq!(t.rows_with(target, &marker), 0, "{context}: nothing kept");
        let entries = t.entries_of(&response);
        let rollbacks: Vec<&Value> = entries
            .iter()
            .filter(|e| e["operation"] == "rollback")
            .collect();
        assert_eq!(
            rollbacks.len(),
            1,
            "{context}: expected one `rollback` entry, got:\n{}",
            lines(&entries)
        );
        assert_tx_entry(
            rollbacks[0],
            &TxEntry {
                operation: "rollback",
                transaction: Some(&id),
                outcome: "allowed",
                reason: None,
                error: None,
                resource: resource_of(target),
                actor: actor_of(target),
                request: &response.request_id(),
                user: &user.id,
                application: APP,
            },
            &format!("{context}: a rollback Application code asked for"),
        );
        if let Target::Own(_) = target {
            let history =
                blackbox::history::history_where(&t.own, &format!("h.new->>'title' = '{marker}'"));
            assert!(
                history.is_empty(),
                "{context}: a change rolled back leaves no history row, got {history:?}"
            );
        }
    }
}

#[test]
fn a_transaction_begun_at_read_is_read_only_and_runs_as_the_read_login() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let context = format!("{} at read", target.describe());
        let marker = format!("read-only-{}", token());
        let (_, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "read", "t"),
                query_in(
                    "t",
                    "select current_user::text as login, current_setting('transaction_read_only') as ro",
                ),
                execute_in("t", &insert_marker(target, &marker)),
                rollback("t"),
            ],
        );
        assert_began(&answers[0], &context);
        let login = match target {
            Target::Shared(_) => t.db().reader.role.clone(),
            Target::Own(_) => t.own.runtime.clone(),
        };
        assert_eq!(
            answers[1]["ok"]["rows"],
            json!([[login, "on"]]),
            "{context}: expected a read-only transaction as the read login (the runtime role on the own database)"
        );
        // Outside, `execute` is allowed for this User in this Application:
        // in a read transaction the database refuses the write.
        assert_database_error(&answers[2], "25006", &context);
        assert_ended(&answers[3], &format!("{context}: tx.rollback"));
        assert_eq!(
            t.rows_with(target, &marker),
            0,
            "{context}: nothing written"
        );
    }
}

#[test]
fn execute_in_a_read_transaction_is_refused_as_it_would_be_outside() {
    let t = Txs::start();
    let marker = format!("refused-in-read-{}", token());
    let target = Target::Shared(RESOURCE);
    // Declared at read: declared-lower.
    let (_, answers) = t.run(
        &wanda(),
        VIEWER,
        &[
            begin(target, "read", "t"),
            execute_in("t", &insert_marker(target, &marker)),
            query_in("t", "select 1 as one"),
            commit("t"),
        ],
    );
    assert_began(&answers[0], "viewer, begin at read");
    assert_refusal(
        &answers[1],
        "declared-lower",
        &json!({ "resource": RESOURCE, "declared": "read", "needed": "write" }),
        "execute in a read transaction of an Application that declares the Resource at read",
    );
    assert_eq!(
        answers[2]["ok"]["rows"],
        json!([[1]]),
        "a refused statement is not sent, so the transaction goes on"
    );
    assert_ended(&answers[3], "commit after a refusal");
    // A User whose Groups hold read only: no-grant.
    let (_, answers) = t.run(
        &rita(),
        APP,
        &[
            begin(target, "read", "t"),
            execute_in("t", &insert_marker(target, &marker)),
            commit("t"),
        ],
    );
    assert_began(&answers[0], "rita, begin at read");
    let refusal = &answers[1]["refused"]["refusal"];
    assert_eq!(
        refusal["reason"], "no-grant",
        "execute in a read transaction by a User holding read only, got {}",
        answers[1]
    );
    assert_eq!(refusal["held"], "read", "{}", answers[1]);
    assert_eq!(refusal["needed"], "write", "{}", answers[1]);
    assert_ended(&answers[2], "commit after a refusal");
    assert_eq!(t.shared_notes_with(&marker), 0, "nothing written");
}

#[test]
fn the_isolation_level_and_the_access_reach_postgres() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let mut steps = Vec::new();
        let mut asked = Vec::new();
        for isolation in ["read committed", "repeatable read", "serializable"] {
            for access in ["read", "write"] {
                let mut args = target.args();
                args["access"] = json!(access);
                args["isolation"] = json!(isolation);
                steps.push(begin_with(args, "t"));
                steps.push(query_in(
                    "t",
                    "select current_setting('transaction_isolation') as iso, current_setting('transaction_read_only') as ro",
                ));
                steps.push(commit("t"));
                asked.push((isolation, access));
            }
        }
        let (_, answers) = t.run(&user, APP, &steps);
        for (i, (isolation, access)) in asked.iter().enumerate() {
            let context = format!(
                "{}: isolation {isolation:?}, access {access:?}",
                target.describe()
            );
            assert_began(&answers[i * 3], &context);
            let read_only = if *access == "read" { "on" } else { "off" };
            assert_eq!(
                answers[i * 3 + 1]["ok"]["rows"],
                json!([[isolation, read_only]]),
                "{context}: expected Postgres to report the isolation level and access asked for"
            );
            assert_ended(&answers[i * 3 + 2], &context);
        }
    }
}

#[test]
fn without_isolation_the_transaction_has_the_databases_default() {
    let t = Txs::start();
    let user = wanda();
    let ask = "select current_setting('transaction_isolation') as iso";
    // A database left at its default.
    for target in TARGETS {
        let (_, answers) = t.run(
            &user,
            APP,
            &[begin(target, "write", "t"), query_in("t", ask), commit("t")],
        );
        assert_began(&answers[0], &target.describe());
        assert_eq!(
            answers[1]["ok"]["rows"],
            json!([["read committed"]]),
            "{}: without `isolation`, the database's default",
            target.describe()
        );
    }
    // The viewer's own database, set to repeatable read before any
    // request opened a connection to it.
    let viewer = blackbox::own_database::OwnDatabase::find(t.vm().platform(), VIEWER).unwrap();
    viewer.instance().batch(&format!(
        "ALTER DATABASE {} SET default_transaction_isolation = 'repeatable read'",
        blackbox::own_database::ident(&viewer.name)
    ));
    for access in ["write", "read"] {
        let (_, answers) = t.run(
            &user,
            VIEWER,
            &[
                begin(Target::Own("user"), access, "t"),
                query_in("t", ask),
                commit("t"),
            ],
        );
        assert_began(&answers[0], access);
        assert_eq!(
            answers[1]["ok"]["rows"],
            json!([["repeatable read"]]),
            "at {access}, without `isolation`, on a database whose default_transaction_isolation is repeatable read"
        );
    }
}

#[test]
fn a_statement_postgres_refuses_aborts_the_transaction_and_the_commit_says_so() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let context = target.describe();
        let marker = format!("aborted-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", &insert_marker(target, &marker)),
                query_in("t", "select 1 / 0 as boom"),
                query_in("t", "select 1 as after"),
                commit("t"),
                query_in("t", "select 1 as after_the_end"),
            ],
        );
        let id = assert_began(&answers[0], &context);
        assert_database_error(
            &answers[2],
            "22012",
            &format!("{context}: division by zero"),
        );
        assert_database_error(
            &answers[3],
            "25P02",
            &format!("{context}: a statement after the failed one"),
        );
        assert_error_kind(
            &answers[4],
            "transaction-aborted",
            &format!("{context}: commit"),
        );
        assert!(
            answers[4]["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.trim().is_empty()),
            "{context}: expected a message with transaction-aborted, got {}",
            answers[4]
        );
        assert_no_transaction(
            &answers[5],
            &format!("{context}: the commit of an aborted transaction ended it"),
        );
        assert_eq!(
            t.rows_with(target, &marker),
            0,
            "{context}: everything was rolled back"
        );
        let entries = t.entries_of(&response);
        assert_eq!(
            entries
                .iter()
                .map(|e| e["operation"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["begin", "execute", "query", "query", "commit"],
            "{context}: one entry per operation; the use after the end, answered `no-transaction`, has none:\n{}",
            lines(&entries)
        );
        let commits: Vec<&Value> = entries
            .iter()
            .filter(|e| e["operation"] == "commit")
            .collect();
        assert_eq!(
            commits.len(),
            1,
            "{context}: expected one `commit` entry, got:\n{}",
            lines(&entries)
        );
        assert_tx_entry(
            commits[0],
            &TxEntry {
                operation: "commit",
                transaction: Some(&id),
                outcome: "failed",
                reason: None,
                error: Some("transaction-aborted"),
                resource: resource_of(target),
                actor: actor_of(target),
                request: &response.request_id(),
                user: &user.id,
                application: APP,
            },
            &format!("{context}: the commit of an aborted transaction"),
        );
        let failed: Vec<&Value> = entries
            .iter()
            .filter(|e| e["operation"] == "query" && e["outcome"] == "failed")
            .collect();
        assert_eq!(
            failed
                .iter()
                .map(|e| e["error"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["22012", "25P02"],
            "{context}: each failed statement has its own entry with its SQLSTATE:\n{}",
            lines(&entries)
        );
        for entry in failed {
            assert_eq!(
                entry["transaction"].as_str(),
                Some(id.as_str()),
                "{context}: {entry}"
            );
        }
    }
}

#[test]
fn the_statement_check_applies_inside_a_transaction() {
    let t = Txs::start();
    let user = wanda();
    for target in TARGETS {
        let context = target.describe();
        let marker = format!("checked-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", "set statement_timeout = 0"),
                execute_in("t", "commit"),
                query_in("t", "select pg_terminate_backend(pg_backend_pid())"),
                execute_in(
                    "t",
                    &format!("{}; select 1", insert_marker(target, &marker)),
                ),
                execute_in("t", &insert_marker(target, &marker)),
                commit("t"),
            ],
        );
        let id = assert_began(&answers[0], &context);
        assert_statement_refused(&answers[1], "kind-not-allowed", &format!("{context}: SET"));
        assert_statement_refused(
            &answers[2],
            "kind-not-allowed",
            &format!("{context}: COMMIT as a statement"),
        );
        assert_statement_refused(
            &answers[3],
            "function-not-allowed",
            &format!("{context}: pg_terminate_backend"),
        );
        assert_statement_refused(
            &answers[4],
            "several-statements",
            &format!("{context}: two statements"),
        );
        assert_eq!(
            answers[5]["ok"]["rows_affected"], 1,
            "{context}: statements the check refused were never sent, so the transaction goes on: {}",
            answers[5]
        );
        assert_ended(&answers[6], &format!("{context}: commit"));
        assert_eq!(
            t.rows_with(target, &marker),
            1,
            "{context}: the one allowed insert is kept"
        );
        let entries = t.entries_of(&response);
        let checked: Vec<&Value> = entries
            .iter()
            .filter(|e| e["error"] == "statement")
            .collect();
        assert_eq!(
            checked.len(),
            4,
            "{context}: each refused statement has its entry, `failed` with error `statement`:\n{}",
            lines(&entries)
        );
        for entry in checked {
            assert_eq!(entry["outcome"], "failed", "{context}: {entry}");
            assert_eq!(
                entry["transaction"].as_str(),
                Some(id.as_str()),
                "{context}: {entry}"
            );
        }
    }
}

#[test]
fn a_read_only_resource_answers_read_only_resource_at_write_and_begins_at_read() {
    let t = Txs::start();
    let target = Target::Shared(blackbox::transactions::RO);
    let (response, answers) = t.run(
        &wanda(),
        APP,
        &[
            begin(target, "write", "w"),
            begin(target, "read", "r"),
            query_in("r", "select current_user::text as login"),
            commit("r"),
        ],
    );
    assert_error_kind(
        &answers[0],
        "read-only-resource",
        "tx.begin at write on a Resource registered without a write login",
    );
    assert_began(&answers[1], "tx.begin at read on it");
    assert_eq!(
        answers[2]["ok"]["rows"],
        json!([[t.db().reader.role]]),
        "a read transaction runs as the read login"
    );
    assert_ended(&answers[3], "commit");
    let entries = t.entries_of(&response);
    let begins: Vec<&Value> = entries
        .iter()
        .filter(|e| e["operation"] == "begin")
        .collect();
    assert_eq!(begins.len(), 2, "{}", lines(&entries));
    assert_eq!(begins[0]["outcome"], "failed", "{}", begins[0]);
    assert_eq!(begins[0]["error"], "read-only-resource", "{}", begins[0]);
    assert!(
        begins[0]["transaction"].is_null(),
        "a begin that began nothing has `transaction` null: {}",
        begins[0]
    );
}

#[test]
fn the_own_database_needs_no_grant_for_either_actor() {
    let t = Txs::start();
    let stranger = User::new("stranger");
    for target in [Target::Own("user"), Target::Own("application")] {
        let marker = format!("own-no-grant-{}", token());
        let (_, answers) = t.run(
            &stranger,
            APP,
            &[
                begin(target, "write", "t"),
                execute_in("t", &insert_marker(target, &marker)),
                commit("t"),
            ],
        );
        assert_began(&answers[0], &target.describe());
        assert_ended(&answers[2], &target.describe());
        assert_eq!(t.own_things_with(&marker), 1, "{}", target.describe());
    }
}
