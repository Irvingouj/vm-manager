//! An own database from before ticket #62 is brought to the new form when
//! the program starts (CONTRACT.md, "The Change history in the platform
//! database: Where it is kept": "An own database from before this ticket is
//! brought to this form when the program starts, before it serves: once the
//! program listens, `vm.history` is gone. What it held is shipped like
//! anything in the outbox (by the shipment at start, and at the latest
//! before the next read of that Application's history) and is then in
//! `change_history`, each change once and with the `id` it had; later
//! changes have greater ids"; and "In the own database there is no
//! `vm.history`: for every role, a statement that names it fails with
//! `42P01`").
//!
//! The own database is made and written by the program as it was when
//! ticket #57 closed ([`EARLIER_COMMIT`], built by the harness from its own
//! worktree: `blackbox::history_earlier`), on a platform database whose role
//! may only create databases and roles: inserts, updates and deletes as
//! both actors, and a Version whose migration changes rows. What its
//! `vm.history` then holds, read as the harness's superuser, is the
//! earlier form's record. That program is stopped and the one under test
//! started on the same platform database. At once when it says it listens,
//! no role finds `vm.history`; the shipment at start empties `vm.outbox`
//! within 10 seconds, with no request, and `change_history` then holds
//! every earlier change once, with its `id` and its columns; a read through
//! `history` changes nothing of that. Changes made afterwards
//! have greater ids, `history` answers the old changes and the new, and a
//! second start adds nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{Value, json};

use blackbox::User;
use blackbox::history::{APP, SHIPPED_WITHIN, history, session_as};
use blackbox::history_earlier::{
    CHANGE_COLUMNS, EARLIER_COMMIT, change_history_now, differences, duplicate_ids,
    earlier_history, id_of, with_both_programs,
};
use blackbox::history_op::{changes_of, history_call};
use blackbox::out_of_reach::{WRITER, base_and, install_seeded, platform_role, start};
use blackbox::own_database::{Migration, Own, OwnDatabase, own_execute};
use blackbox::vm::VmOptions;

/// The Version 2 migration the earlier program runs: it changes rows.
fn earlier_migration() -> Migration {
    Migration::new(
        "0004_earlier_changes",
        &[
            r#"UPDATE "things" SET "size" = "size" + 10;"#,
            r#"DELETE FROM "keyless";"#,
        ],
    )
}

/// The Version 3 migration the program under test runs: it changes one
/// row.
fn later_migration() -> Migration {
    Migration::new(
        "0005_after_the_form",
        &[r#"UPDATE "parents" SET "name" = 'changed by 0005' WHERE "id" = 1;"#],
    )
}

/// Runs these `(actor, sql)` writes through the Application as
/// [`WRITER`], failing unless each is answered `ok`.
fn write(own: &Own, writes: &[(&str, &str)], context: &str) {
    let calls: Vec<Value> = writes
        .iter()
        .map(|(actor, sql)| own_execute(actor, sql))
        .collect();
    let (_, answers) = own.run(&User::new(WRITER), APP, &calls);
    for (answer, call) in answers.iter().zip(&calls) {
        assert!(
            answer["ok"].is_object(),
            "{context}: expected {call} to run, got {answer}"
        );
    }
}

/// What each role of `db` gets naming `vm.history`: `None` when it fails
/// with `42P01`, otherwise what happened.
fn vm_history_found_by(db: &OwnDatabase) -> Vec<String> {
    let roles = [
        None,
        Some(platform_role(db)),
        Some(db.owner.clone()),
        Some(db.runtime.clone()),
    ];
    let mut found = Vec::new();
    for role in &roles {
        let who = role.as_deref().unwrap_or("the harness's superuser");
        let sql = session_as(db, role.as_deref());
        match sql.try_coded("SELECT count(*) FROM vm.history") {
            Err(e) if e.code == "42P01" => {}
            Err(e) => found.push(format!("{who}: failed with {} ({e}), not 42P01", e.code)),
            Ok(rows) => found.push(format!(
                "{who}: read it ({:?} rows)",
                rows.first().and_then(|r| r.first().cloned().flatten())
            )),
        }
    }
    found
}

/// The ids `history` answers for `schema`.`table`, as numbers, in the
/// order answered.
fn ids_answered(own: &Own, schema: &str, table: &str) -> Vec<i64> {
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &history_call("user", schema, table, &json!({ "limit": 1000 })),
    );
    changes_of(&answer).iter().map(id_of).collect()
}

/// The ids of `rows` of `schema`.`table`, newest first.
fn ids_of_table(rows: &[Value], schema: &str, table: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = rows
        .iter()
        .filter(|r| r["schema_name"] == schema && r["table_name"] == table)
        .map(id_of)
        .collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids
}

#[test]
fn an_own_database_from_before_the_ticket_has_its_history_in_change_history_once_with_its_ids() {
    with_both_programs(
        "an_own_database_from_before_the_ticket_has_its_history_in_change_history_once_with_its_ids",
        |programs| {
            // The earlier program makes the own database and writes it.
            programs.use_earlier();
            let own = start();
            let db = install_seeded(&own, APP);
            write(
                &own,
                &[
                    ("application", "delete from things where id = 101"),
                    (
                        "user",
                        "update parents set name = 'renamed parent' where id = 2",
                    ),
                    ("user", "delete from counters where name = 'hits'"),
                    (
                        "application",
                        "update inventory.stock set qty = 6 where sku = 'sku-1'",
                    ),
                ],
                "the harness's own writes with the earlier program",
            );
            own.send_ok(APP, &base_and(&[earlier_migration()]));
            let before = earlier_history(&db);
            let kinds: BTreeSet<&str> = before.iter().filter_map(|r| r["kind"].as_str()).collect();
            let actors: BTreeSet<&str> =
                before.iter().filter_map(|r| r["actor"].as_str()).collect();
            assert!(
                kinds == BTreeSet::from(["insert", "update", "delete"])
                    && actors == BTreeSet::from(["user", "application", "migration"])
                    && before.iter().all(|r| CHANGE_COLUMNS
                        .iter()
                        .all(|c| r.as_object().is_some_and(|o| o.contains_key(*c)))),
                "the harness's own check: the program at {EARLIER_COMMIT} was expected to leave inserts, updates and deletes by both actors and a migration in vm.history, each with the columns {CHANGE_COLUMNS:?}; it left kinds {kinds:?}, actors {actors:?}:\n{}",
                lines(&before)
            );
            let earlier_ids: BTreeSet<i64> = before.iter().map(id_of).collect();
            let max_earlier = earlier_ids.last().copied().unwrap_or(0);
            let platform = own.vm.platform().clone();
            drop(own);

            // The program under test starts on what it left.
            programs.use_current();
            let own = Own::start_with(&VmOptions::new().platform(&platform));
            let mut failures = Vec::new();
            let found = vm_history_found_by(&db);
            if !found.is_empty() {
                failures.push(format!(
                    "expected no role to find vm.history in the own database once the program says it listens (42P01 for every role), but:\n  {}",
                    found.join("\n  ")
                ));
            }
            // "By the shipment at start": vm.outbox empty within 10 s of the
            // start, with no request.
            let deadline = Instant::now() + SHIPPED_WITHIN;
            let mut left = outbox_rows(&db);
            while !matches!(left, Ok(0)) && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
                left = outbox_rows(&db);
            }
            if !matches!(left, Ok(0)) {
                failures.push(format!(
                    "expected vm.outbox empty within {SHIPPED_WITHIN:?} of the start, what vm.history held shipped by the shipment at start; got {left:?}"
                ));
            }
            let at_start = change_history_now(&db);
            if let Ok(after) = &at_start {
                let wrong = differences(APP, &before, after);
                if !wrong.is_empty() {
                    failures.push(format!(
                        "expected each of the {} changes vm.history held in change_history once, as it was, after the shipment at start:\n  {}",
                        before.len(),
                        wrong.join("\n  ")
                    ));
                }
            }
            // "At the latest before the next read of that Application's
            // history": a read, which then finds nothing more to ship.
            let (_, answer) = own.one(
                &User::new(WRITER),
                APP,
                &history_call("user", "public", "things", &json!({ "limit": 1 })),
            );
            if !answer["ok"].is_object() {
                failures.push(format!(
                    "expected a read of the history of public.things to be answered after the start, got {answer}"
                ));
            }
            match change_history_now(&db) {
                Err(e) => failures.push(format!(
                    "expected the {} changes the earlier program left in vm.history to be in change_history of the platform database once the program had started and the history had been read, but change_history cannot be read: {e}",
                    before.len()
                )),
                Ok(after) => {
                    let wrong = differences(APP, &before, &after);
                    if !wrong.is_empty() {
                        failures.push(format!(
                            "expected each of the {} changes vm.history held to be in change_history once, as it was, once the program had started and the history had been read:\n  {}",
                            before.len(),
                            wrong.join("\n  ")
                        ));
                    }
                }
            }
            assert!(
                failures.is_empty(),
                "the own database from before the ticket (program at {EARLIER_COMMIT}) was not brought to the new form:\n{}\nwhat vm.history held:\n{}",
                failures.join("\n"),
                lines(&before)
            );

            // Changes made afterwards come after them.
            write(
                &own,
                &[
                    (
                        "user",
                        "insert into things (id, title, size) values (103, 'after the change of form', 7)",
                    ),
                    (
                        "application",
                        "update parents set name = 'renamed again' where id = 2",
                    ),
                    ("user", "delete from children where id = 20"),
                ],
                "writes after the change of form",
            );
            own.send_ok(APP, &base_and(&[earlier_migration(), later_migration()]));
            let after = history(&db);
            assert_eq!(
                duplicate_ids(&after),
                Vec::<i64>::new(),
                "expected no id twice in change_history of {APP:?}"
            );
            let wrong = differences(APP, &before, &after);
            assert!(
                wrong.is_empty(),
                "expected the earlier changes unchanged after more writes:\n  {}",
                wrong.join("\n  ")
            );
            let later: Vec<&Value> = after
                .iter()
                .filter(|r| !earlier_ids.contains(&id_of(r)))
                .collect();
            assert!(
                later.len() == 4 && later.iter().all(|r| id_of(r) > max_earlier),
                "expected the 4 changes made after the start (3 writes and one migration's update) in change_history, each with an id greater than {max_earlier}, the greatest of the earlier ones; the rows not from vm.history are:\n{}",
                lines(&later.into_iter().cloned().collect::<Vec<_>>())
            );

            // `history` answers the old changes and the new, newest first.
            for (schema, table) in [("public", "things"), ("public", "parents")] {
                let answered = ids_answered(&own, schema, table);
                let kept = ids_of_table(&after, schema, table);
                assert_eq!(
                    answered, kept,
                    "expected `history` of {schema}.{table} to answer every change change_history holds for it (those from vm.history and the later ones), newest first"
                );
            }

            // A second start adds nothing.
            drop(own);
            let own = Own::start_with(&VmOptions::new().platform(&platform));
            let again = history(&db);
            assert_eq!(
                again, after,
                "expected change_history of {APP:?} unchanged by a second start of the program"
            );
            drop(own);
        },
    );
}

/// The rows of `db`'s `vm.outbox`, counted by the harness's superuser, or
/// why they could not be.
fn outbox_rows(db: &OwnDatabase) -> Result<i64, String> {
    db.superuser()
        .try_batch("SELECT count(*)::text FROM vm.outbox")
        .map(|rows| {
            rows.into_iter()
                .next()
                .and_then(|r| r.into_iter().next().flatten())
                .and_then(|v| v.parse().ok())
                .unwrap_or(-1)
        })
}

fn lines(rows: &[Value]) -> String {
    rows.iter().map(|r| format!("  {r}\n")).collect()
}
