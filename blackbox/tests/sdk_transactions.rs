//! Transactions through the SDK (CONTRACT.md, "Transactions: Through the
//! SDK" and "Identity, the Operation log and the Change history"; ticket
//! "Transactions through the SDK", criteria 1, 2, 4 and 5), with the
//! fixture project `fixtures/sdk/ledger`:
//!
//! - several statements in `db.transaction(...)` take effect together at
//!   the commit, on the own database as the User and as the Application and
//!   on a shared database declared at `write`; `transaction` resolves to
//!   the callback's value; the callback reads its own writes, and another
//!   transaction (in the same request, or another request) does not see
//!   them until the commit;
//! - a thrown error rolls everything back and is the very error
//!   `transaction` rejects with; `tx.rollback()` rejects with Drizzle's
//!   `TransactionRollbackError`;
//! - through a handle declared at `read` the transaction is read-only, and
//!   a write forced past the types is refused;
//! - `isolationLevel` and `accessMode` reach Postgres;
//! - `tx.transaction(...)` rejects with an `Error` saying `Transactions
//!   cannot be nested`, nothing of it reaches the host, and the transaction
//!   around it goes on;
//! - a statement that failed and was caught makes the commit reject with
//!   `OperationFailed` of kind `transaction-aborted`, and nothing is kept;
//! - each statement has its own Operation log entry carrying the
//!   transaction's id, with `begin` and `commit` / `rollback` entries;
//!   `vm.identity()` answers the same in every statement; every change is
//!   in the Change history with the User, the Application, the actor and
//!   the request.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::history::history_where;
use blackbox::own_database::literal;
use blackbox::records::event;
use blackbox::scenario::{RESOURCE, lines};
use blackbox::sdk::{LEDGER, OPS, Sdk, ann, has_constants};
use blackbox::shared_database::token;
use blackbox::transactions::{TxEntry, assert_tx_entry, in_transaction, wait_for_none};
use serde_json::{Value, json};

const ACTORS: [&str; 2] = ["user", "application"];

fn ledger() -> Sdk {
    Sdk::with_shared("ledger")
}

fn own_notes_with(sdk: &Sdk, marker: &str) -> Vec<String> {
    sdk.own.column(&format!(
        "SELECT body FROM notes WHERE body LIKE {} ORDER BY id",
        literal(&format!("{marker}-%"))
    ))
}

fn shared_notes_with(sdk: &Sdk, marker: &str) -> Vec<Vec<Option<String>>> {
    sdk.scenario.db.superuser().batch(&format!(
        "SELECT body, written_by, label_user, label_request FROM notes WHERE body LIKE {} ORDER BY id",
        literal(&format!("{marker}-%"))
    ))
}

fn hits(sdk: &Sdk) -> i64 {
    sdk.own
        .column("SELECT n::text FROM counters WHERE name = 'hits'")
        .first()
        .and_then(|n| n.parse().ok())
        .unwrap()
}

fn operations(entries: &[Value]) -> Vec<&str> {
    entries
        .iter()
        .map(|e| e["operation"].as_str().unwrap_or_default())
        .collect()
}

/// The `transaction` id the first entry (the `begin`) carries.
fn id_of_begin(entries: &[Value], context: &str) -> String {
    assert_eq!(
        entries.first().map(|e| e["operation"].clone()),
        Some(json!("begin")),
        "{context}: the first entry of a transaction is its `begin`:\n{}",
        lines(entries)
    );
    entries[0]["transaction"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{context}: the begin entry carries the transaction's id: {}",
                entries[0]
            )
        })
        .to_owned()
}

#[test]
fn statements_in_a_transaction_on_the_own_database_commit_together() {
    let sdk = ledger();
    let user = ann();
    for actor in ACTORS {
        let context = format!("the own database as the {actor}");
        let marker = format!("commit-{actor}-{}", token());
        let before = hits(&sdk);
        let (response, body) = sdk.get_ok(&user, &format!("/commit?as={actor}&marker={marker}"));
        let request = response.request_id();
        let result = &body["result"];
        assert!(
            result["ids"][0].is_number() && result["ids"][1].is_number(),
            "{context}: transaction resolves to what the callback returned, got {body}"
        );
        assert_eq!(
            result["inside"],
            json!([format!("{marker}-a"), format!("{marker}-b")]),
            "{context}: a select in the transaction reads its own writes"
        );
        assert_eq!(
            result["outside"], 0,
            "{context}: a select outside the transaction does not see its writes before the commit"
        );
        let identity = json!([{ "userId": user.id, "application": LEDGER, "actor": actor, "request": request }]);
        assert_eq!(
            result["identities"],
            json!([identity, identity]),
            "{context}: vm.identity() answers the same in every statement of the transaction"
        );
        assert_eq!(
            own_notes_with(&sdk, &marker),
            vec![format!("{marker}-a"), format!("{marker}-b")],
            "{context}: both notes are kept"
        );
        assert_eq!(
            hits(&sdk),
            before + 1,
            "{context}: the counter's change is kept"
        );

        // The Operation log: begin, one entry per statement, commit.
        let entries = sdk.entries_of(&response);
        assert_eq!(
            operations(&entries),
            vec![
                "begin", "query", "execute", "execute", "execute", "query", "query", "query",
                "commit"
            ],
            "{context}: expected begin, one entry per statement Drizzle sent, and commit:\n{}",
            lines(&entries)
        );
        let id = id_of_begin(&entries, &context);
        let tx_entry = |operation| TxEntry {
            operation,
            transaction: Some(&id),
            outcome: "allowed",
            reason: None,
            error: None,
            resource: None,
            actor,
            request: &request,
            user: &user.id,
            application: LEDGER,
        };
        assert_tx_entry(
            &entries[0],
            &tx_entry("begin"),
            &format!("{context}: begin"),
        );
        assert_tx_entry(
            &entries[8],
            &tx_entry("commit"),
            &format!("{context}: commit"),
        );
        let statements = body["statements"].as_array().unwrap();
        assert_eq!(statements.len(), 7, "{context}: {body}");
        for (i, (entry, statement)) in entries[1..8].iter().zip(statements).enumerate() {
            let outside = i == 5;
            assert_eq!(
                entry["transaction"].as_str(),
                if outside { None } else { Some(id.as_str()) },
                "{context}: statement {i} carries {}: {entry}",
                if outside {
                    "`transaction` null (it ran outside)"
                } else {
                    "the transaction's id"
                }
            );
            assert_eq!(entry["outcome"], "allowed", "{context}: {entry}");
            assert_eq!(entry["actor"], actor, "{context}: {entry}");
            assert_eq!(entry["own_database"], true, "{context}: {entry}");
            let sql = statement["sql"].as_str().unwrap();
            if !has_constants(sql) {
                assert_eq!(
                    entry["statement"], sql,
                    "{context}: the statement Drizzle built"
                );
            }
        }

        // The Change history.
        let rows = history_where(&sdk.own, &format!("h.request = '{request}'"));
        assert_eq!(
            rows.iter()
                .map(|r| format!(
                    "{} {}",
                    r["table_name"].as_str().unwrap_or_default(),
                    r["kind"].as_str().unwrap_or_default()
                ))
                .collect::<Vec<_>>(),
            vec!["notes insert", "notes insert", "counters update"],
            "{context}: every change made in the transaction is in the Change history: {rows:#?}"
        );
        for row in &rows {
            assert_eq!(row["user_id"], user.id.as_str(), "{context}: {row}");
            assert_eq!(row["application"], LEDGER, "{context}: {row}");
            assert_eq!(row["actor"], actor, "{context}: {row}");
            assert_eq!(row["request"], request.as_str(), "{context}: {row}");
        }
    }
}

#[test]
fn a_thrown_error_rolls_everything_back_and_is_what_transaction_rejects_with() {
    let sdk = ledger();
    let user = ann();
    for actor in ACTORS {
        let context = format!("the own database as the {actor}");
        let marker = format!("throw-{actor}-{}", token());
        let (response, body) = sdk.get_ok(&user, &format!("/throw?as={actor}&marker={marker}"));
        assert_eq!(
            body["same"], true,
            "{context}: transaction rejects with the very error the callback threw, got {body}"
        );
        assert!(
            own_notes_with(&sdk, &marker).is_empty(),
            "{context}: rolled back"
        );
        let entries = sdk.entries_of(&response);
        assert_eq!(
            operations(&entries),
            vec!["begin", "execute", "rollback"],
            "{context}:\n{}",
            lines(&entries)
        );
        let id = id_of_begin(&entries, &context);
        assert_eq!(
            entries[1]["transaction"].as_str(),
            Some(id.as_str()),
            "{context}: {}",
            entries[1]
        );
        assert_eq!(
            entries[2]["transaction"].as_str(),
            Some(id.as_str()),
            "{context}: {}",
            entries[2]
        );
        assert!(
            entries[2]["error"].is_null(),
            "{context}: a rollback Application code made is not the host's own: {}",
            entries[2]
        );
        let rows = history_where(
            &sdk.own,
            &format!("h.request = '{}'", response.request_id()),
        );
        assert!(
            rows.is_empty(),
            "{context}: no history for a rolled back change: {rows:#?}"
        );
    }
    let marker = format!("shared-throw-{}", token());
    let (_, body) = sdk.get_ok(&user, &format!("/shared/throw?marker={marker}"));
    assert_eq!(body["same"], true, "on ops: {body}");
    assert!(
        shared_notes_with(&sdk, &marker).is_empty(),
        "on ops: rolled back"
    );
}

#[test]
fn tx_rollback_rolls_back_and_rejects_with_drizzles_rollback_error() {
    let sdk = ledger();
    for actor in ACTORS {
        let marker = format!("rollback-{actor}-{}", token());
        let (response, body) = sdk.get_ok(&ann(), &format!("/rollback?as={actor}&marker={marker}"));
        let caught = &body["caught"];
        assert_eq!(
            caught["isRollbackError"], true,
            "as the {actor}: tx.rollback() makes transaction reject with Drizzle's TransactionRollbackError, got {body}"
        );
        assert_eq!(caught["name"], "TransactionRollbackError", "{body}");
        assert!(
            own_notes_with(&sdk, &marker).is_empty(),
            "as the {actor}: rolled back"
        );
        let entries = sdk.entries_of(&response);
        assert_eq!(
            operations(&entries),
            vec!["begin", "execute", "rollback"],
            "{}",
            lines(&entries)
        );
    }
}

#[test]
fn a_transaction_on_a_shared_database_declared_at_write_commits_together() {
    let sdk = ledger();
    let user = ann();
    let marker = format!("shared-commit-{}", token());
    let (response, body) = sdk.get_ok(&user, &format!("/shared/commit?marker={marker}"));
    let request = response.request_id();
    let writer = sdk.scenario.db.writer.role.clone();
    let row = |suffix: &str| json!({ "body": format!("{marker}-{suffix}"), "writtenBy": writer, "labelUser": user.id, "labelRequest": request });
    assert_eq!(
        body["result"],
        json!({ "inside": [row("a"), row("b")], "outside": 0 }),
        "the transaction reads its own writes, as the write login, with the request's label; outside it nothing is seen before the commit"
    );
    let kept = shared_notes_with(&sdk, &marker);
    assert_eq!(kept.len(), 2, "both kept: {kept:?}");
    let entries = sdk.entries_of(&response);
    assert_eq!(
        operations(&entries),
        vec!["begin", "execute", "execute", "query", "query", "commit"],
        "{}",
        lines(&entries)
    );
    let id = id_of_begin(&entries, "ops");
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry["resource"], OPS, "{entry}");
        let expected = if i == 4 { None } else { Some(id.as_str()) };
        assert_eq!(
            entry["transaction"].as_str(),
            expected,
            "entry {i}: {entry}"
        );
    }
}

#[test]
fn a_transaction_through_a_handle_declared_at_read_is_read_only() {
    let sdk = ledger();
    let user = ann();
    let reader = sdk.scenario.db.reader.role.clone();
    for mode in ["", "&mode=read%20write", "&mode=read%20only"] {
        let (_, body) = sdk.get_ok(&user, &format!("/settings?target=devops{mode}"));
        let rows = &body["rows"];
        assert_eq!(
            rows[0]["ro"], "on",
            "a transaction on devops, declared at read ({mode:?}), is read-only: {body}"
        );
        assert_eq!(
            rows[0]["login"],
            reader.as_str(),
            "it runs as the read login: {body}"
        );
    }
    let marker = format!("forced-{}", token());
    let (response, body) = sdk.get_ok(&user, &format!("/shared/forced-write?marker={marker}"));
    let caught = &body["caught"];
    assert_eq!(
        caught["isRefused"], true,
        "an insert forced through a transaction on a handle declared at read is refused, got {body}"
    );
    assert_eq!(
        caught["refusal"],
        json!({ "reason": "declared-lower", "resource": RESOURCE, "declared": "read", "needed": "write" }),
        "{body}"
    );
    assert!(
        shared_notes_with(&sdk, &marker).is_empty(),
        "nothing written"
    );
    let entries = sdk.entries_of(&response);
    assert_eq!(operations(&entries)[0], "begin", "{}", lines(&entries));
}

#[test]
fn the_isolation_level_and_the_access_mode_reach_postgres() {
    let sdk = ledger();
    let user = ann();
    let writer = sdk.scenario.db.writer.role.clone();
    for target in ["own&as=user", "own&as=application", "ops"] {
        for iso in ["read committed", "repeatable read", "serializable"] {
            for mode in [None, Some("read write"), Some("read only")] {
                let mut path = format!("/settings?target={target}&iso={}", iso.replace(' ', "%20"));
                if let Some(mode) = mode {
                    path.push_str(&format!("&mode={}", mode.replace(' ', "%20")));
                }
                let (_, body) = sdk.get_ok(&user, &path);
                let row = &body["rows"][0];
                let context =
                    format!("{target}, isolationLevel {iso:?}, accessMode {mode:?}: {body}");
                assert_eq!(row["iso"], iso, "{context}");
                let read_only = if mode == Some("read only") {
                    "on"
                } else {
                    "off"
                };
                assert_eq!(row["ro"], read_only, "{context}");
                if target == "ops" && read_only == "on" {
                    assert_eq!(
                        row["login"],
                        sdk.scenario.db.reader.role.as_str(),
                        "accessMode read only on a handle declared at write begins at read, as the read login: {context}"
                    );
                }
                if target == "ops" && read_only == "off" {
                    assert_eq!(
                        row["login"],
                        writer.as_str(),
                        "a read-write transaction on ops runs as the write login: {context}"
                    );
                }
            }
        }
    }
}

#[test]
fn read_only_on_a_handle_declared_at_write_needs_only_read() {
    let sdk = ledger();
    sdk.scenario.grant(OPS, "ops-readers", "read");
    let olga = blackbox::User::new("olga").in_groups(&["ops-readers"]);
    let (response, body) = sdk.get_ok(&olga, "/settings?target=ops&mode=read%20only");
    assert_eq!(
        body["rows"][0]["login"],
        sdk.scenario.db.reader.role.as_str(),
        "a read-only transaction on ops runs as the read login: {body}"
    );
    assert_eq!(body["rows"][0]["ro"], "on", "{body}");
    let entries = sdk.entries_of(&response);
    assert_eq!(
        operations(&entries).first(),
        Some(&"begin"),
        "{}",
        lines(&entries)
    );
    assert_eq!(
        entries[0]["outcome"], "allowed",
        "the begin needs read only, which olga holds: {}",
        entries[0]
    );
    // Read-write needs write, which she does not hold.
    let refused = sdk.get(&olga, "/settings?target=ops&mode=read%20write");
    assert_eq!(
        refused.status,
        403,
        "a read-write transaction on ops by a User holding read is refused: {}",
        refused.describe()
    );
    assert_eq!(
        refused.json()["refusal"]["reason"],
        "no-grant",
        "{}",
        refused.describe()
    );
}

#[test]
fn a_nested_transaction_is_refused_and_nothing_of_it_reaches_the_host() {
    let sdk = ledger();
    for actor in ACTORS {
        let context = format!("as the {actor}");
        let marker = format!("nested-{actor}-{}", token());
        let (response, body) = sdk.get_ok(&ann(), &format!("/nested?as={actor}&marker={marker}"));
        let nested = &body["result"];
        assert_eq!(
            nested["isError"], true,
            "{context}: tx.transaction rejects with an Error, got {body}"
        );
        assert!(
            nested["message"]
                .as_str()
                .is_some_and(|m| m.contains("Transactions cannot be nested")),
            "{context}: the error says so, got {body}"
        );
        assert_eq!(
            own_notes_with(&sdk, &marker),
            vec![format!("{marker}-outer-1"), format!("{marker}-outer-2")],
            "{context}: the transaction around it goes on and commits; nothing of the nested one ran"
        );
        let entries = sdk.entries_of(&response);
        assert_eq!(
            operations(&entries),
            vec!["begin", "execute", "execute", "commit"],
            "{context}: nothing of the nested transaction reaches the host:\n{}",
            lines(&entries)
        );
        let begins = sdk
            .vm()
            .records_of(&response)
            .all
            .iter()
            .filter(|r| event(r) == "host-call" && r["operation"] == "tx.begin")
            .count();
        assert_eq!(begins, 1, "{context}: one tx.begin reached the host");
    }
}

#[test]
fn a_commit_after_a_statement_failed_rejects_with_transaction_aborted() {
    let sdk = ledger();
    for actor in ACTORS {
        let context = format!("as the {actor}");
        let marker = format!("aborted-{actor}-{}", token());
        let (response, body) = sdk.get_ok(&ann(), &format!("/aborted?as={actor}&marker={marker}"));
        let inner = &body["inner"];
        assert_eq!(inner["isOperationFailed"], true, "{context}: {body}");
        assert_eq!(
            inner["code"], "23505",
            "{context}: the duplicate key: {body}"
        );
        let caught = &body["caught"];
        assert_eq!(
            caught["isOperationFailed"], true,
            "{context}: the commit of an aborted transaction rejects with OperationFailed, got {body}"
        );
        assert_eq!(caught["kind"], "transaction-aborted", "{context}: {body}");
        assert!(
            own_notes_with(&sdk, &marker).is_empty(),
            "{context}: nothing kept"
        );
        let entries = sdk.entries_of(&response);
        assert_eq!(
            operations(&entries).get(..4),
            Some(&["begin", "execute", "execute", "commit"][..]),
            "{context}:\n{}",
            lines(&entries)
        );
        assert_eq!(entries[2]["outcome"], "failed", "{context}: {}", entries[2]);
        assert_eq!(entries[2]["error"], "23505", "{context}: {}", entries[2]);
        assert_eq!(entries[3]["outcome"], "failed", "{context}: {}", entries[3]);
        assert_eq!(
            entries[3]["error"], "transaction-aborted",
            "{context}: {}",
            entries[3]
        );
    }
}

#[test]
fn another_request_does_not_see_a_transactions_writes_until_the_commit() {
    let sdk = ledger();
    let user = ann();
    let marker = format!("held-{}", token());
    let database = sdk.own.name.clone();
    let runtime = vec![sdk.own.runtime.clone()];
    let during = thread::scope(|scope| {
        let held = scope.spawn(|| sdk.get_ok(&user, &format!("/hold?marker={marker}&ms=3000")));
        // Wait until the transaction is open and has written.
        let sql = sdk.own.superuser();
        let open = wait_for_none(Duration::from_secs(15), || {
            if in_transaction(&sql, &database, &runtime).is_empty() {
                vec!["not yet".to_owned()]
            } else {
                Vec::new()
            }
        });
        assert!(
            open.is_empty(),
            "the harness's own check: the transaction never showed in pg_stat_activity"
        );
        thread::sleep(Duration::from_millis(300));
        let (_, during) = sdk.get_ok(&user, &format!("/count?marker={marker}"));
        let (_, body) = held.join().unwrap();
        assert_eq!(body["result"], "held", "{body}");
        during
    });
    assert_eq!(
        during["count"], 0,
        "another request must not see the transaction's write before the commit: {during}"
    );
    let (_, after) = sdk.get_ok(&user, &format!("/count?marker={marker}"));
    assert_eq!(after["count"], 1, "after the commit it does: {after}");
}
