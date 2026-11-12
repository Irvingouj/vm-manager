//! Change history and migrations (CONTRACT.md, "Identity and Change history
//! in the own database: Change history"; ticket "Identity in the own
//! database, and Change history", third acceptance criterion: "A table
//! added by a later migration is captured with nothing done by the
//! Contributor").
//!
//! Every table a migration makes is captured from the end of that
//! migration. Rows a migration changes in a table an earlier migration made
//! (in the same Version or an earlier one) are recorded with `actor`
//! `migration`, the Contributor who sent the Version, the migration's name
//! and no request; rows a migration puts into a table it makes itself are
//! not. A migration that fails leaves no history row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{
    APP, Change, assert_at_within, assert_change, assert_history_matches_operation_log,
    assert_ids_increase, db_now, history, history_after, history_standard, last_history_id,
    row_json,
};
use blackbox::own_database::{Migration, Own, assert_ok, own_execute};
use blackbox::scenario::lines;
use blackbox::{INSTALLER, api::assert_api_error};
use serde_json::json;

/// A migration of the first Version that changes a row of a table an
/// earlier migration of the same Version made.
fn touch_in_first_version() -> Migration {
    Migration::new(
        "0003_touch_in_first_version",
        &[r#"UPDATE "parents" SET "name" = 'touched by 0003' WHERE "id" = 1;"#],
    )
}

/// A migration of the second Version: changes rows of tables earlier ones
/// made, and makes a table `later` and puts and changes rows in it.
fn change_and_add() -> Migration {
    Migration::new(
        "0004_change_and_add",
        &[
            r#"UPDATE "parents" SET "name" = 'renamed by 0004' WHERE "id" = 1;"#,
            r#"INSERT INTO "parents" VALUES (2, 'added by 0004');"#,
            r#"DELETE FROM "children" WHERE "id" = 10;"#,
            r#"CREATE TABLE "later" (
	"id" integer PRIMARY KEY NOT NULL,
	"note" text NOT NULL
);"#,
            r#"INSERT INTO "later" VALUES (1, 'put in by the migration that made it');"#,
            r#"UPDATE "later" SET "note" = 'changed by the migration that made it';"#,
        ],
    )
}

/// A migration that changes rows and then fails.
fn changes_then_fails() -> Migration {
    Migration::new(
        "0005_changes_then_fails",
        &[
            r#"UPDATE "parents" SET "name" = 'never' WHERE "id" = 1;"#,
            r#"INSERT INTO "later" VALUES (50, 'never');"#,
            r#"INSERT INTO "parents" VALUES (2, 'a key that is taken');"#,
        ],
    )
}

#[test]
fn rows_changed_by_migrations_are_recorded_as_the_migration_and_a_later_table_is_captured() {
    let own = Own::start();
    let before = postgres_now();
    // Version 1: the tables, and a migration that changes a row of one an
    // earlier migration of the same Version made.
    let mut list = history_standard();
    list.push(touch_in_first_version());
    own.create(APP);
    own.send_ok(APP, &list);
    let db = own.own_database(APP);
    let after_first = db_now(&db);
    let rows = history(&db);
    assert_eq!(
        rows.len(),
        1,
        "expected only the change of 0003 recorded: rows a migration puts into a table it makes itself are not; got:\n{}",
        lines(&rows)
    );
    let made = json!({ "id": 1, "name": "made by 0000" });
    let touched = json!({ "id": 1, "name": "touched by 0003" });
    assert_change(
        &rows[0],
        &Change::by_migration(INSTALLER, "0003_touch_in_first_version")
            .on("public", "parents")
            .update(json!({ "id": 1 }), made, touched.clone(), json!(["name"])),
        "a migration's update of a table an earlier migration of the same Version made",
    );
    assert_at_within(
        &db,
        &rows,
        &before,
        &after_first,
        "the first Version's migration",
    );
    // Version 2.
    let last = last_history_id(&db);
    let before_second = db_now(&db);
    list.push(change_and_add());
    own.send_ok(APP, &list);
    let after_second = db_now(&db);
    let rows = history_after(&db, last);
    assert_eq!(
        rows.len(),
        3,
        "expected the update, the insert and the delete of 0004 in tables earlier migrations made, and nothing for `later`, which 0004 made; got:\n{}",
        lines(&rows)
    );
    let by = || Change::by_migration(INSTALLER, "0004_change_and_add");
    assert_change(
        &rows[0],
        &by().on("public", "parents").update(
            json!({ "id": 1 }),
            touched,
            json!({ "id": 1, "name": "renamed by 0004" }),
            json!(["name"]),
        ),
        "0004's update",
    );
    assert_change(
        &rows[1],
        &by().on("public", "parents").insert(
            json!({ "id": 2 }),
            json!({ "id": 2, "name": "added by 0004" }),
        ),
        "0004's insert",
    );
    assert_change(
        &rows[2],
        &by().on("public", "children").delete(
            json!({ "id": 10 }),
            json!({ "id": 10, "parent_id": 1, "name": "child of 1, made by 0000" }),
        ),
        "0004's delete",
    );
    assert_ids_increase(&rows, "0004's changes");
    assert_at_within(&db, &rows, &before_second, &after_second, "0004's changes");
    // `later`, made by Version 2's migration, is captured with nothing done
    // by the Contributor.
    let last = last_history_id(&db);
    let row1 = row_json(&db, "later", "id = 1");
    assert_eq!(
        row1,
        json!({ "id": 1, "note": "changed by the migration that made it" })
    );
    let alice = User::new("alice");
    let (inserted, answer) = own.one(
        &alice,
        APP,
        &own_execute(
            "user",
            "insert into later (id, note) values (2, 'by alice')",
        ),
    );
    assert_ok(
        &answer,
        "an insert into a table a later Version's migration made",
    );
    let (updated, answer) = own.one(
        &alice,
        APP,
        &own_execute(
            "application",
            "update later set note = 'by the application' where id = 1",
        ),
    );
    assert_ok(&answer, "an update of a row a migration put in");
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 2, "{}", lines(&rows));
    assert_change(
        &rows[0],
        &Change::by("alice", "user", &inserted.request_id())
            .on("public", "later")
            .insert(json!({ "id": 2 }), json!({ "id": 2, "note": "by alice" })),
        "an insert into a table made by a later migration",
    );
    assert_change(
        &rows[1],
        &Change::by("alice", "application", &updated.request_id())
            .on("public", "later")
            .update(
                json!({ "id": 1 }),
                row1,
                json!({ "id": 1, "note": "by the application" }),
                json!(["note"]),
            ),
        "an update of a row a migration put into a table it made",
    );
    // A migration that changes rows and then fails leaves no history row.
    let count = history(&db).len();
    let parents = row_json(&db, "parents", "id = 1");
    list.push(changes_then_fails());
    let sent = own.send(APP, &list);
    assert_api_error(&sent, 422, "migration-failed");
    assert_eq!(
        history(&db).len(),
        count,
        "expected no history row from a migration that failed"
    );
    assert_eq!(row_json(&db, "parents", "id = 1"), parents);
    assert_history_matches_operation_log(&own, &db, "after migrations and operations");
}

/// The test Postgres's time, read on `postgres` (the own database does
/// not exist yet).
fn postgres_now() -> String {
    blackbox::sql::Sql::connect(&blackbox::postgres::shared().url("postgres"))
        .unwrap()
        .value("SELECT clock_timestamp()::text")
        .unwrap()
}
