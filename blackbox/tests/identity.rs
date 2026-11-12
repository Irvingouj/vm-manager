//! Identity in the own database (CONTRACT.md, "Identity and Change history
//! in the own database: Identity" and "A change with no identity fails";
//! ticket "Identity in the own database, and Change history", fifth
//! acceptance criterion: "A write with no verifiable identity fails; it is
//! never recorded as made by nobody").
//!
//! `vm.identity()` gives the User in flight, the Application, the actor and
//! the request in `query` and `execute`, for both actors; `vm.ctx` is
//! readable opaque text; the label is still set. With nothing attached,
//! `vm.identity()` raises `VM001`, and a change of a captured table fails
//! with `VM001` for the runtime role, the owner role and the superuser,
//! leaving neither the change nor a history row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{
    APP, NO_IDENTITY, assert_nothing_attached, attempt, history_standard, identity_now, session_as,
    snapshot,
};
use blackbox::own_database::{ACTORS, Own, assert_ok, own_execute, own_query};
use blackbox::scenario::{assert_rows, op};
use serde_json::json;

#[test]
fn vm_identity_answers_who_the_operation_is_for_in_query_and_execute_for_both_actors() {
    let own = Own::start();
    own.install(APP, &history_standard());
    for user in [User::new("alice"), User::new("bob")] {
        let mut calls = Vec::new();
        let mut what = Vec::new();
        for actor in ACTORS {
            for operation in ["query", "execute"] {
                calls.push(op(
                    operation,
                    &json!({ "own": true, "as": actor, "sql": "select * from vm.identity()" }),
                ));
                what.push((operation, actor));
            }
        }
        let (response, answers) = own.run(&user, APP, &calls);
        let request = response.request_id();
        for ((operation, actor), answer) in what.iter().zip(&answers) {
            assert_rows(
                answer,
                &["user_id", "application", "actor", "request"],
                &json!([[user.id, APP, actor, request]]),
                &format!(
                    "vm.identity() in {operation} as the {actor} for {}",
                    user.id
                ),
            );
        }
    }
}

#[test]
fn vm_ctx_is_readable_text_and_the_label_is_still_set() {
    let own = Own::start();
    own.install(APP, &history_standard());
    let alice = User::new("alice");
    for actor in ACTORS {
        let (response, answer) = own.one(
            &alice,
            APP,
            &own_query(
                actor,
                "select current_setting('vm.ctx') as ctx, current_setting('vm.user') as u,
                        current_setting('vm.application') as a, current_setting('vm.request') as r,
                        current_setting('vm.actor') as k",
            ),
        );
        assert_ok(&answer, &format!("reading vm.ctx as the {actor}"));
        let row = &answer["ok"]["rows"][0];
        assert!(
            row[0].as_str().is_some_and(|ctx| !ctx.is_empty()),
            "expected vm.ctx to be readable non-empty text as the {actor}, got {answer}"
        );
        assert_eq!(
            row.as_array().unwrap().get(1..),
            Some(
                &[
                    json!("alice"),
                    json!(APP),
                    json!(response.request_id()),
                    json!(actor)
                ][..]
            ),
            "expected the label still set as the {actor}, got {answer}"
        );
    }
}

#[test]
fn with_nothing_attached_vm_identity_raises_vm001_for_every_role() {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    for (who, role) in [
        ("the runtime role", Some(db.runtime.as_str())),
        ("the owner role", Some(db.owner.as_str())),
        ("the superuser", None),
    ] {
        let sql = session_as(&db, role);
        let outside = sql.try_coded("SELECT * FROM vm.identity()");
        assert!(
            matches!(&outside, Err(e) if e.code == NO_IDENTITY),
            "expected vm.identity() to raise {NO_IDENTITY} for {who} outside a transaction block, got {outside:?}"
        );
        sql.batch("BEGIN");
        let inside = identity_now(&sql);
        assert!(
            matches!(&inside, Err(e) if e.code == NO_IDENTITY),
            "expected vm.identity() to raise {NO_IDENTITY} for {who} in a transaction, got {inside:?}"
        );
        let ctx = attempt(&sql, "SELECT current_setting('vm.ctx', true)");
        assert!(
            ctx.is_ok(),
            "expected vm.ctx to be readable (empty) for {who}, got {ctx:?}"
        );
        sql.batch("ROLLBACK");
    }
}

#[test]
fn a_change_with_no_identity_fails_with_vm001_for_the_runtime_role_the_owner_and_the_superuser() {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    let alice = User::new("alice");
    let (_, answers) = own.run(
        &alice,
        APP,
        &[
            own_execute(
                "user",
                "insert into things (title, size) values ('there', 1)",
            ),
            own_execute(
                "application",
                "insert into keyless (a, b) values ('there', 1)",
            ),
            own_execute(
                "user",
                "insert into inventory.stock (sku, warehouse, qty) values ('s', 'w', 1)",
            ),
        ],
    );
    for answer in &answers {
        assert_ok(answer, "seeding rows through the Application");
    }
    let before = snapshot(&db);
    let changes = [
        "INSERT INTO things (title) VALUES ('by nobody')",
        "UPDATE things SET size = 2",
        "DELETE FROM things",
        "UPDATE parents SET name = 'by nobody' WHERE id = 1",
        "DELETE FROM parents WHERE id = 1",
        "DELETE FROM children",
        "INSERT INTO counters VALUES ('n', 1)",
        "INSERT INTO keyless VALUES ('by nobody', 2)",
        "UPDATE keyless SET b = 3",
        "DELETE FROM keyless",
        "INSERT INTO inventory.stock VALUES ('x', 'y', 1)",
        "UPDATE inventory.stock SET qty = 9",
        "DELETE FROM inventory.stock",
    ];
    for (who, role) in [
        ("the runtime role", Some(db.runtime.as_str())),
        ("the owner role", Some(db.owner.as_str())),
        ("the superuser", None),
    ] {
        let sql = session_as(&db, role);
        let mut wrong = Vec::new();
        for change in changes {
            // On its own, outside a transaction block.
            let alone = sql.try_coded(change);
            if !matches!(&alone, Err(e) if e.code == NO_IDENTITY) {
                wrong.push(format!("  {change} on its own: {alone:?}"));
            }
            // Inside a transaction.
            sql.batch("BEGIN");
            let inside = attempt(&sql, change);
            if !matches!(&inside, Err(e) if e.code == NO_IDENTITY) {
                wrong.push(format!("  {change} in a transaction: {inside:?}"));
            }
            sql.batch("ROLLBACK");
        }
        assert!(
            wrong.is_empty(),
            "expected every change of a captured table by {who} with no identity to fail with {NO_IDENTITY}, but:\n{}",
            wrong.join("\n")
        );
        assert_eq!(
            snapshot(&db),
            before,
            "expected neither a change nor a history row after {who}'s changes with no identity"
        );
    }
    // The same checks the hostile tests use, as the runtime role.
    let sql = session_as(&db, Some(&db.runtime));
    sql.batch("BEGIN");
    assert_nothing_attached(&sql, "the runtime role in a transaction of its own");
    sql.batch("ROLLBACK");
    assert_eq!(snapshot(&db), before);
}
