//! What the Postgres server log shows of transactions (CONTRACT.md,
//! "Transactions: What the host does on its own" and "Identity, the
//! Operation log and the Change history"; ticket "Transactions through the
//! SDK", criterion 6, "Two requests in flight never share a transaction's
//! connection").
//!
//! This test binary turns statement logging fully on for its own-database
//! Postgres (`ALTER SYSTEM`) before any program starts; the shared
//! databases log their statements already.
//!
//! - Several requests at once each hold a transaction open (on `devops`
//!   and on the own database) and report `pg_backend_pid()` in each of
//!   their statements, while other requests send marked statements outside
//!   any transaction. Every statement of one transaction reports the same
//!   backend, transactions that overlap report different ones, and on each
//!   transaction's backend, between its first and its last statement, the
//!   server log shows no statement of anyone else (its markers, or the
//!   other requests').
//! - Replaying, as the runtime role in new transactions, what the log
//!   shows the host sent for transactions on the own database attaches
//!   nothing: `vm.identity()` raises `VM001`, and a write to a captured
//!   table fails with `VM001`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::HashMap;

use blackbox::history::{NO_IDENTITY, attempt};
use blackbox::postgres::{for_shared_databases, shared};
use blackbox::scenario::RESOURCE;
use blackbox::server_log::{Entry, entries, log_everything, parse, sent_by, transactions};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use blackbox::transactions::{
    APP, Target, Txs, assert_began, assert_ended, begin, commit, execute_in, in_tx_with, outside,
    query_in, rollback, sleep, together, wanda,
};
use serde_json::{Value, json};

/// A statement reporting its backend, marked, after `ms`.
fn pid_select(marker: &str, ms: u32) -> String {
    format!(
        "select pg_backend_pid() as pid, '{marker}' as m from pg_sleep({})",
        f64::from(ms) / 1000.0
    )
}

/// The log entries of `database`, oldest first, with their index.
fn entries_of(all: &[Entry], database: &str) -> Vec<Entry> {
    all.iter()
        .filter(|e| e.database == database)
        .cloned()
        .collect()
}

#[test]
fn two_requests_in_flight_never_share_a_transactions_connection() {
    log_everything(shared());
    let t = Txs::start();
    let user = wanda();
    let base = format!("mk-{}", token());
    let targets = [
        Target::Shared(RESOURCE),
        Target::Own("user"),
        Target::Shared(RESOURCE),
        Target::Own("application"),
        Target::Shared(RESOURCE),
        Target::Own("user"),
    ];
    let mut bodies: Vec<(blackbox::User, &str, Value)> = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        let mut steps = vec![begin(*target, "write", "t")];
        for k in 0..4 {
            steps.push(query_in(
                "t",
                &pid_select(&format!("{base}-tx{i}-s{k}"), 150),
            ));
            steps.push(sleep(150));
        }
        steps.push(commit("t"));
        bodies.push((user.clone(), APP, json!({ "steps": steps })));
    }
    for j in 0..3 {
        let mut noise = Vec::new();
        for k in 0..6 {
            let marker = format!("{base}-noise{j}-{k}");
            noise.push(outside(
                "query",
                Target::Shared(RESOURCE),
                &pid_select(&marker, 100),
            ));
            noise.push(outside(
                "execute",
                Target::Shared(RESOURCE),
                &format!("insert into notes (body) select '{marker}' from pg_sleep(0.1) returning pg_backend_pid() as pid"),
            ));
            noise.push(outside(
                "query",
                Target::Own("user"),
                &pid_select(&marker, 100),
            ));
            noise.push(outside(
                "execute",
                Target::Own("application"),
                &format!("insert into things (title) select '{marker}' from pg_sleep(0.1) returning pg_backend_pid() as pid"),
            ));
        }
        // Four at a time (one per target and operation), in six rounds
        // while the transactions are open.
        let steps: Vec<Value> = noise.chunks(4).map(together).collect();
        bodies.push((user.clone(), APP, json!({ "steps": steps })));
    }
    let results = t
        .scenario
        .at_once_within(&bodies, blackbox::transactions::TX_REQUEST_TIMEOUT);

    // Every statement of one transaction on one backend.
    let mut pid_of: Vec<i64> = Vec::new();
    for (i, (_, answers)) in results.iter().take(targets.len()).enumerate() {
        let context = format!("transaction {i} on {}", targets[i].describe());
        assert_began(&answers[0], &context);
        let pids: Vec<i64> = (0..4)
            .map(|k| {
                answers[1 + 2 * k]["ok"]["rows"][0][0]
                    .as_i64()
                    .unwrap_or_else(|| {
                        panic!("{context}: statement {k} answered {}", answers[1 + 2 * k])
                    })
            })
            .collect();
        assert!(
            pids.windows(2).all(|w| w[0] == w[1]),
            "{context}: every statement of a transaction runs on its one connection, but they report the backends {pids:?}"
        );
        assert_ended(answers.last().unwrap(), &context);
        pid_of.push(pids[0]);
    }
    for (j, (_, answers)) in results.iter().skip(targets.len()).enumerate() {
        for group in answers.iter().filter(|a| a.is_array()) {
            for answer in group.as_array().unwrap() {
                assert!(
                    answer["ok"].is_object(),
                    "noise request {j}: expected each marked statement to run, got {answer}"
                );
            }
        }
    }

    // What each backend ran, from the server log.
    let shared_log = entries_of(&parse(&for_shared_databases().server_log()), t.db().name());
    let own_log = entries_of(&entries(shared()), &t.own.name);
    let mut overlapping_pairs = 0;
    let mut windows: HashMap<usize, (usize, usize)> = HashMap::new();
    for (i, target) in targets.iter().enumerate() {
        let log = match target {
            Target::Shared(_) => &shared_log,
            Target::Own(_) => &own_log,
        };
        let mine = format!("{base}-tx{i}-");
        let pid = pid_of[i].to_string();
        let at: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.text.starts_with("LOG:  ")
                    && e.text.contains(&mine)
                    && !e.text.contains("duration:")
            })
            .map(|(n, _)| n)
            .collect();
        assert!(
            at.len() >= 4,
            "transaction {i}: the harness's own check: the server log must show its statements (found {} entries with {mine:?})",
            at.len()
        );
        let (first, last) = (at[0], *at.last().unwrap());
        windows.insert(i, (first, last));
        for entry in &log[first..=last] {
            if entry.pid != pid {
                continue;
            }
            assert!(
                !entry.text.contains(&base) || entry.text.contains(&mine),
                "transaction {i} on {}: between its first and last statement its backend {pid} ran someone else's statement:\n{}",
                target.describe(),
                entry.text
            );
        }
        for entry in log.iter().filter(|e| e.text.contains(&mine)) {
            assert_eq!(
                entry.pid, pid,
                "transaction {i}: a statement of the transaction ran on another backend: {}",
                entry.text
            );
        }
    }
    for i in 0..targets.len() {
        for j in (i + 1)..targets.len() {
            let same_database = matches!(
                (targets[i], targets[j]),
                (Target::Shared(_), Target::Shared(_)) | (Target::Own(_), Target::Own(_))
            );
            if !same_database {
                continue;
            }
            let (a, b) = (windows[&i], windows[&j]);
            if a.0 <= b.1 && b.0 <= a.1 {
                overlapping_pairs += 1;
                assert_ne!(
                    pid_of[i], pid_of[j],
                    "transactions {i} and {j} were open at the same time on the same backend"
                );
            }
        }
    }
    assert!(
        overlapping_pairs > 0,
        "the harness's own check: some of the transactions sent at once must have been open at the same time"
    );
}

#[test]
fn replaying_what_the_log_shows_of_a_transaction_attaches_nothing() {
    log_everything(shared());
    let t = Txs::start();
    let user = wanda();
    let marker = format!("replayed-{}", token());
    for (actor, access, end) in [
        ("user", "write", "commit"),
        ("application", "write", "commit"),
        ("user", "write", "rollback"),
        ("application", "read", "commit"),
    ] {
        let target = Target::Own(actor);
        let mut steps = vec![
            begin(target, access, "t"),
            query_in("t", "select * from vm.identity()"),
            query_in("t", "select current_setting('vm.ctx') as ctx"),
        ];
        if access == "write" {
            steps.push(in_tx_with(
                "execute",
                "t",
                "insert into things (title) values ($1) returning id",
                &json!([marker]),
            ));
            steps.push(execute_in(
                "t",
                "update things set size = 3 where size is null",
            ));
        }
        steps.push(query_in("t", "select * from vm.identity()"));
        steps.push(if end == "commit" {
            commit("t")
        } else {
            rollback("t")
        });
        let (_, answers) = t.run(&user, APP, &steps);
        assert_began(&answers[0], actor);
        assert_ended(answers.last().unwrap(), actor);
    }
    let log = shared().server_log();
    assert!(
        log.contains(&marker),
        "the harness's own check: the server log must show the parameters of the statements sent"
    );
    let sent = sent_by(&entries(shared()), &t.own.name, &t.own.runtime);
    let groups = transactions(&sent);
    assert!(
        groups.iter().any(|g| g.len() >= 3),
        "the harness's own check: the log must show the transactions as groups of statements, got {groups:#?}"
    );
    let password = t.own.set_known_password(&t.own.runtime);
    let runtime = Sql::connect(&shared().url_as(&t.own.runtime, &password, &t.own.name))
        .unwrap_or_else(|e| panic!("cannot log in as the runtime role: {e}"));
    let mut wrong = Vec::new();
    for group in &groups {
        runtime.batch("BEGIN");
        for statement in group {
            drop(attempt(&runtime, statement));
        }
        let identity = attempt(&runtime, "SELECT * FROM vm.identity()");
        if !matches!(&identity, Err(e) if e.code == NO_IDENTITY) {
            wrong.push(format!(
                "  after replaying {group:#?}\n  vm.identity() gave {identity:?}"
            ));
        }
        let write = attempt(
            &runtime,
            "INSERT INTO things (title) VALUES ('written after replaying a transaction')",
        );
        if !matches!(&write, Err(e) if e.code == NO_IDENTITY) {
            wrong.push(format!(
                "  after replaying {group:#?}\n  a write gave {write:?}"
            ));
        }
        runtime.batch("ROLLBACK");
    }
    assert!(
        wrong.is_empty(),
        "expected nothing replayed from the server log of a transaction to attach an identity, but:\n{}",
        wrong.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
}
