//! A migration that fails (CONTRACT.md, "The Application's own database:
//! What happens when a Version is sent"; ticket "An Application's own
//! database", third acceptance criterion).
//!
//! The pending migrations run in one transaction: when one fails, the own
//! database is exactly as before (a fingerprint of everything in it is
//! compared), no Version is recorded and no number is used up, the
//! previous Version goes on serving, and whoever sent the Version gets
//! `422 migration-failed` naming the migration, with Postgres's SQLSTATE
//! and message. Failing in a migration's first statement, in a later one
//! after earlier ones succeeded, and in the second of two pending
//! migrations; as the first Version and as a later one.
//!
//! The failing migrations touch no sequence that existed before them:
//! Postgres never rolls back `nextval`, so a sequence advanced by a rolled
//! back statement would differ, which the platform cannot prevent.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::own_database::{
    ACTORS, Migration, Own, OwnDatabase, more_tables, own_execute, own_query, standard,
};
use blackbox::scenario::assert_rows;
use blackbox::{Response, User, assert_api_error};
use serde_json::json;

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

/// Fails unless `sent` is `422 migration-failed` naming `migration`, with
/// this SQLSTATE and a `database_message` containing `message`.
fn assert_migration_failed(sent: &Response, migration: &str, code: &str, message: &str) {
    assert_api_error(sent, 422, "migration-failed");
    let error = &sent.json()["error"];
    assert_eq!(
        error["migration"].as_str(),
        Some(migration),
        "expected the error to name the migration that failed, {migration:?}, got {error}"
    );
    assert_eq!(
        error["code"].as_str(),
        Some(code),
        "expected the error to carry Postgres's SQLSTATE {code}, got {error}"
    );
    assert!(
        error["database_message"]
            .as_str()
            .is_some_and(|m| m.contains(message)),
        "expected the error's database_message to be Postgres's message, containing {message:?}, got {error}"
    );
}

/// Writes a row as each actor, so that the database holds rows the
/// failed Version must leave alone.
fn write_rows(own: &Own) {
    let calls: Vec<_> = ACTORS
        .iter()
        .map(|actor| {
            own_execute(
                actor,
                &format!("insert into items (title) values ('kept, by the {actor}') returning id"),
            )
        })
        .collect();
    let (_, answers) = own.run(&ann(), "board", &calls);
    for answer in &answers {
        assert!(answer["ok"].is_object(), "{answer}");
    }
}

/// Fails unless the database, the Application and its Versions are as
/// before the failed Version, and Version 1 still serves and reads its
/// rows.
fn assert_nothing_changed(own: &Own, db: &OwnDatabase, before: &str, what: &str) {
    assert_eq!(
        db.fingerprint(),
        before,
        "expected the own database exactly as before {what}"
    );
    assert_eq!(
        own.version_numbers("board"),
        vec![1],
        "expected no Version recorded for {what}"
    );
    assert_eq!(own.api().application("board")["serving_version"], 1);
    assert_eq!(
        own.serving(&ann(), "board"),
        1,
        "expected Version 1 still serving after {what}"
    );
    let (_, answer) = own.one(
        &ann(),
        "board",
        &own_query("application", "select count(*)::int4 as n from items"),
    );
    assert_rows(
        &answer,
        &["n"],
        &json!([[2]]),
        &format!("the rows written before {what}, read under Version 1"),
    );
}

/// Sends a good Version after the failed ones and checks it is number 2.
fn assert_next_version_is_2(own: &Own, db: &OwnDatabase) {
    let mut list = standard();
    list.push(more_tables());
    let record = own.send_ok("board", &list);
    assert_eq!(
        record["number"], 2,
        "expected the next Version after failed ones to be 2, with no number used up, in {record}"
    );
    assert!(db.has_table("extras"), "got {:?}", db.tables());
    assert_eq!(own.serving(&ann(), "board"), 2);
}

#[test]
fn a_later_migration_failing_in_its_first_statement_changes_nothing_and_the_previous_version_serves()
 {
    let own = Own::start();
    let db = own.install("board", &standard());
    write_rows(&own);
    let before = db.fingerprint();
    let mut list = standard();
    list.push(Migration::new(
        "0002_broken",
        &[
            r#"ALTER TABLE "nowhere" ADD COLUMN "x" integer;"#,
            r#"CREATE TABLE "never" ("x" integer);"#,
        ],
    ));
    let sent = own.send("board", &list);
    assert_migration_failed(&sent, "0002_broken", "42P01", "nowhere");
    assert_nothing_changed(
        &own,
        &db,
        &before,
        "a migration failing in its first statement",
    );
    assert!(!db.has_table("never"));
    assert_next_version_is_2(&own, &db);
}

#[test]
fn a_migration_failing_after_its_earlier_statements_succeeded_leaves_nothing_of_them() {
    let own = Own::start();
    let db = own.install("board", &standard());
    write_rows(&own);
    let before = db.fingerprint();
    let mut list = standard();
    list.push(Migration::new(
        "0002_half_done",
        &[
            r#"CREATE TABLE "half" ("id" serial PRIMARY KEY NOT NULL, "x" integer);"#,
            r#"INSERT INTO "half" ("x") VALUES (1), (2);"#,
            r#"ALTER TABLE "items" ADD COLUMN "half" text;"#,
            r#"UPDATE "items" SET "title" = 'rewritten by a migration that fails';"#,
            r#"DELETE FROM "seeded";"#,
            r#"CREATE INDEX "items_title_idx" ON "items" ("title");"#,
            r#"INSERT INTO "seeded" VALUES (7, 'first'), (7, 'again');"#,
        ],
    ));
    let sent = own.send("board", &list);
    assert_migration_failed(&sent, "0002_half_done", "23505", "duplicate key");
    assert_nothing_changed(
        &own,
        &db,
        &before,
        "a migration failing in its last statement after six succeeded",
    );
    assert!(!db.has_table("half"), "got {:?}", db.tables());
    assert_next_version_is_2(&own, &db);
}

#[test]
fn when_the_second_of_two_pending_migrations_fails_the_first_is_not_applied_either() {
    let own = Own::start();
    let db = own.install("board", &standard());
    write_rows(&own);
    let before = db.fingerprint();
    let mut list = standard();
    list.push(more_tables());
    list.push(Migration::new(
        "0003_clash",
        &[r#"CREATE TABLE "items" ("x" integer);"#],
    ));
    let sent = own.send("board", &list);
    assert_migration_failed(&sent, "0003_clash", "42P07", "items");
    assert_nothing_changed(
        &own,
        &db,
        &before,
        "the second of two pending migrations failing",
    );
    assert!(
        !db.has_table("extras"),
        "expected the first pending migration rolled back with the second, got {:?}",
        db.tables()
    );
    // Several failures in a row use up no number either.
    let sent = own.send("board", &list);
    assert_migration_failed(&sent, "0003_clash", "42P07", "items");
    assert_nothing_changed(&own, &db, &before, "the same failure sent again");
    assert_next_version_is_2(&own, &db);
}

#[test]
fn a_migration_failing_on_a_value_reports_postgres_s_message() {
    let own = Own::start();
    let db = own.install("board", &standard());
    write_rows(&own);
    let before = db.fingerprint();
    let mut list = standard();
    list.push(Migration::new(
        "0002_divides",
        &[
            r#"CREATE TABLE "ratios" ("r" integer);"#,
            r#"INSERT INTO "ratios" SELECT 1 / (count(*) - count(*))::integer FROM "items";"#,
        ],
    ));
    let sent = own.send("board", &list);
    assert_migration_failed(&sent, "0002_divides", "22012", "division by zero");
    assert_nothing_changed(&own, &db, &before, "a migration dividing by zero");
}

#[test]
fn a_first_version_whose_migration_fails_is_422_and_records_no_version() {
    let own = Own::start();
    own.create("board");
    let broken = vec![Migration::new(
        "0000_twice",
        &[
            r#"CREATE TABLE "items" ("id" integer);"#,
            r#"INSERT INTO "items" VALUES (1);"#,
            r#"CREATE TABLE "items" ("id" integer);"#,
        ],
    )];
    let sent = own.send("board", &broken);
    assert_migration_failed(&sent, "0000_twice", "42P07", "items");
    assert!(
        own.api().versions("board").is_empty(),
        "expected no Version recorded"
    );
    assert_eq!(
        own.api().application("board")["serving_version"],
        json!(null),
        "expected no serving Version"
    );
    let response = own.vm.get_as(&ann(), "board", "/");
    assert_eq!(
        response.status,
        404,
        "expected an Application with no Version to be 404, got\n{}",
        response.describe()
    );
    // CONTRACT.md does not say whether the own database made for a first
    // Version whose migration failed remains; if it does, it is empty.
    if let Some(db) = own.find_own_database("board") {
        assert!(
            db.tables().is_empty(),
            "expected the own database left by a failed first Version to hold nothing of the migration, got {:?}",
            db.tables()
        );
    }
    // The next good first Version is number 1 and gets its tables.
    let record = own.send_ok("board", &standard());
    assert_eq!(record["number"], 1, "in {record}");
    let db = own.own_database("board");
    assert!(
        db.has_table("items") && db.has_table("seeded"),
        "got {:?}",
        db.tables()
    );
    assert_eq!(own.serving(&ann(), "board"), 1);
}
