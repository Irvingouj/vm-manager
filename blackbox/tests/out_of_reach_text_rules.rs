//! How migrations are sent, and what is refused before anything is sent
//! (CONTRACT.md, "The platform's part of an own database is out of a
//! migration's reach: How migrations are sent, and what is refused before
//! anything is sent"; ticket "The Change history is kept out of a
//! Contributor's reach", third acceptance criterion: "A Version whose
//! migration contains `COMMIT; BEGIN`, `COMMIT AND CHAIN` or any other
//! transaction control is refused before anything is committed").
//!
//! Each hostile migration first inserts a marker row into a captured table
//! and then controls the transaction, in every listed form, in mixed case,
//! with comments, as the second statement of a part, after a breakpoint.
//! Each Version must be `422 migration-refused` with the rule
//! `transaction-control`, naming the migration, and nothing may have been
//! committed: the marker row is absent and the whole database is as before
//! (`out_of_reach::untouched`). The same words inside a string, a comment, a
//! dollar-quoted body or a quoted identifier are not refused. Text the
//! parser cannot read is refused as `unparseable`. A `DO` block or a
//! procedure that commits fails in the database with `2D000`.
//!
//! That the rules are checked before any statement of any migration is sent
//! is shown here by a sequence: a value set into a sequence outlives a
//! rollback, so a first migration's `setval` on the first Version's
//! sequence `stash` would show if it had been sent. The test of the server
//! log is in `out_of_reach_never_sent.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use blackbox::out_of_reach::{
    Answers, Case, base_and, failed, install_seeded, panic_text, refused, run_cases, start,
};
use blackbox::own_database::{BREAKPOINT, Migration};

const TRANSACTION_CONTROL: Answers = &[refused("transaction-control")];
const UNPARSEABLE: Answers = &[refused("unparseable")];
const INVALID_TERMINATION: Answers = &[failed("2D000")];

/// The statement every hostile migration of this file begins with: a row
/// of a captured table, which must not be committed.
const MARKER: &str = r#"INSERT INTO "things" ("id", "title") VALUES (9001, 'marker before the transaction control');"#;

/// A migration `0004_<name>`: [`MARKER`], a breakpoint, then `text`.
fn after_marker(name: &str, text: &str) -> Migration {
    Migration {
        name: format!("0004_{name}"),
        sql: format!("{MARKER}\n{BREAKPOINT}\n{text}\n"),
    }
}

/// A case of [`after_marker`] refused with `answers`.
fn marker_case(name: &'static str, answers: Answers, text: &'static str) -> Case {
    Case::migrations(name, answers, move |_| vec![after_marker(name, text)])
}

/// Every listed form of transaction control, and spellings of them.
const FORMS: &[(&str, &str)] = &[
    ("commit", "COMMIT;"),
    ("commit-begin", "COMMIT; BEGIN;"),
    ("commit-work", "COMMIT WORK;"),
    ("commit-and-chain", "COMMIT AND CHAIN;"),
    ("end", "END;"),
    ("end-transaction", "END TRANSACTION;"),
    ("end-start-transaction", "END; START TRANSACTION;"),
    ("begin", "BEGIN;"),
    ("begin-work", "BEGIN WORK;"),
    ("begin-isolation", "BEGIN ISOLATION LEVEL SERIALIZABLE;"),
    ("start-transaction", "START TRANSACTION;"),
    (
        "start-transaction-read-only",
        "START TRANSACTION READ ONLY;",
    ),
    ("rollback", "ROLLBACK;"),
    ("rollback-and-chain", "ROLLBACK AND CHAIN;"),
    ("abort", "ABORT;"),
    ("abort-begin", "ABORT; BEGIN;"),
    ("prepare-transaction", "PREPARE TRANSACTION 'mine';"),
    (
        "prepare-transaction-begin",
        "PREPARE TRANSACTION 'mine'; BEGIN;",
    ),
    ("commit-prepared", "COMMIT PREPARED 'mine';"),
    ("rollback-prepared", "ROLLBACK PREPARED 'mine';"),
    ("savepoint", "SAVEPOINT mine;"),
    (
        "release-savepoint",
        "SAVEPOINT mine;\n--> statement-breakpoint\nRELEASE SAVEPOINT mine;",
    ),
    ("release", "RELEASE mine;"),
    ("rollback-to-savepoint", "ROLLBACK TO SAVEPOINT mine;"),
    ("rollback-to", "ROLLBACK TO mine;"),
    ("mixed-case-commit", "cOmMiT;"),
    ("mixed-case-chain", "Commit And Chain;"),
    ("lower-case-end", "end;"),
    ("block-comments", "/* nothing to see */ COMMIT /* here */;"),
    ("line-comment-before", "-- the end of the part\nCOMMIT;"),
    ("no-semicolon", "COMMIT"),
    ("spread-over-lines", "COMMIT\n  AND\n  CHAIN\n;"),
    (
        "second-statement-of-a-part",
        r#"UPDATE "things" SET "title" = "title" WHERE "id" = 9001; COMMIT;"#,
    ),
    (
        "between-statements-of-a-part",
        r#"UPDATE "things" SET "title" = 'x' WHERE "id" = 9001; COMMIT; UPDATE "things" SET "title" = 'y' WHERE "id" = 9001;"#,
    ),
    (
        "after-a-dollar-quoted-body",
        "CREATE FUNCTION \"f\"() RETURNS text LANGUAGE sql AS $f$ SELECT 'x' $f$; COMMIT;",
    ),
];

#[test]
fn every_form_of_transaction_control_is_refused_and_nothing_is_committed() {
    let own = start();
    let mut cases: Vec<Case> = FORMS
        .iter()
        .map(|(name, text)| marker_case(name, TRANSACTION_CONTROL, text))
        .collect();
    // Transaction control first, the marker after it.
    cases.push(Case::text(
        "commit-first",
        TRANSACTION_CONTROL,
        "COMMIT;\n--> statement-breakpoint\nINSERT INTO \"things\" (\"id\", \"title\") VALUES (9001, 'marker after the commit');\n",
    ));
    // The whole migration in one part, with no breakpoint.
    cases.push(Case::text(
        "one-part",
        TRANSACTION_CONTROL,
        "INSERT INTO \"things\" (\"id\", \"title\") VALUES (9001, 'marker'); COMMIT; BEGIN; INSERT INTO \"things\" (\"id\", \"title\") VALUES (9002, 'after');\n",
    ));
    run_cases(&own, "tx", cases);
}

#[test]
fn a_rule_broken_in_a_later_migration_refuses_the_version_before_the_first_is_sent() {
    let own = start();
    let first = |name: &str| Migration::new(name, &["SELECT setval('stash', 777);", MARKER]);
    let cases = vec![
        Case::migrations("later-commit", TRANSACTION_CONTROL, move |_| {
            vec![
                first("0004_harmless"),
                Migration::new("0005_commits", &["COMMIT AND CHAIN;"]),
            ]
        })
        .naming("0005_commits"),
        Case::migrations("later-unparseable", UNPARSEABLE, move |_| {
            vec![
                first("0004_harmless"),
                Migration::new("0005_garbled", &["CREATE TABLE ( broken"]),
            ]
        })
        .naming("0005_garbled"),
    ];
    run_cases(&own, "later", cases);
}

#[test]
fn text_the_parser_cannot_read_is_refused_with_postgres_s_words() {
    let own = start();
    let cases = vec![
        marker_case("unparseable", UNPARSEABLE, "CREATE TABLE ( broken;")
            .mentioning("syntax error"),
        marker_case("unterminated-string", UNPARSEABLE, "SELECT 'never closed;")
            .mentioning("unterminated"),
        marker_case(
            "unterminated-dollar-quote",
            UNPARSEABLE,
            "CREATE FUNCTION \"f\"() RETURNS int LANGUAGE sql AS $f$ SELECT 1;",
        ),
    ];
    run_cases(&own, "garbled", cases);
}

#[test]
fn a_do_block_or_a_procedure_that_commits_fails_in_the_database() {
    let own = start();
    let cases = vec![
        marker_case(
            "do-commit",
            INVALID_TERMINATION,
            "DO $f$ BEGIN COMMIT; END $f$;",
        ),
        marker_case(
            "do-rollback",
            INVALID_TERMINATION,
            "DO $f$ BEGIN ROLLBACK; END $f$;",
        ),
        marker_case(
            "do-commit-and-chain",
            INVALID_TERMINATION,
            "DO $f$ BEGIN COMMIT AND CHAIN; END $f$;",
        ),
        marker_case(
            "call-a-procedure-that-commits",
            INVALID_TERMINATION,
            "CREATE PROCEDURE \"commits\"() LANGUAGE plpgsql AS $f$ BEGIN COMMIT; END $f$;\n--> statement-breakpoint\nCALL \"commits\"();",
        ),
    ];
    run_cases(&own, "procedural", cases);
}

/// The words of transaction control where they are not transaction
/// control: each Version applies (`201`) and its rows are there.
#[test]
fn the_words_inside_a_string_a_comment_a_body_or_an_identifier_are_not_refused() {
    let own = start();
    let texts: &[(&str, &str, &str)] = &[
        (
            "string",
            "INSERT INTO \"things\" (\"id\", \"title\") VALUES (9101, 'COMMIT; BEGIN; ROLLBACK AND CHAIN');",
            "SELECT title FROM things WHERE id = 9101",
        ),
        (
            "escaped-string",
            "INSERT INTO \"things\" (\"id\", \"title\") VALUES (9102, E'it''s \\'; COMMIT; --');",
            "SELECT title FROM things WHERE id = 9102",
        ),
        (
            "line-comment",
            "-- COMMIT; BEGIN;\nINSERT INTO \"things\" (\"id\", \"title\") VALUES (9103, 'after a comment');",
            "SELECT title FROM things WHERE id = 9103",
        ),
        (
            "block-comment",
            "/* COMMIT; SAVEPOINT x; */ INSERT INTO \"things\" (\"id\", \"title\") VALUES (9104, 'after a block comment');",
            "SELECT title FROM things WHERE id = 9104",
        ),
        (
            "dollar-quoted-string",
            "INSERT INTO \"things\" (\"id\", \"title\") VALUES (9105, $q$COMMIT; BEGIN;$q$);",
            "SELECT title FROM things WHERE id = 9105",
        ),
        (
            "function-body",
            "CREATE FUNCTION \"says_commit\"() RETURNS text LANGUAGE plpgsql AS $f$ BEGIN RETURN 'COMMIT'; END $f$;",
            "SELECT says_commit()",
        ),
        (
            "procedure-body-not-called",
            "CREATE PROCEDURE \"would_commit\"() LANGUAGE plpgsql AS $f$ BEGIN COMMIT; END $f$;",
            "SELECT proname FROM pg_proc WHERE proname = 'would_commit'",
        ),
        (
            "quoted-identifiers",
            "CREATE TABLE \"commit\" (\"begin\" text, \"rollback\" integer, \"savepoint\" text, \"end\" text);",
            "SELECT count(*) FROM \"commit\"",
        ),
        (
            "unquoted-column-names",
            "CREATE TABLE \"ledger\" (\"id\" integer PRIMARY KEY, \"release\" text, \"abort\" boolean, \"prepared\" boolean);",
            "SELECT count(*) FROM ledger",
        ),
    ];
    let mut failures = Vec::new();
    for (i, (name, text, check)) in texts.iter().enumerate() {
        let application = format!("words-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install_seeded(&own, &application);
            let migration = Migration {
                name: format!("0004_{name}"),
                sql: format!("{MARKER}\n{BREAKPOINT}\n{text}\n"),
            };
            let sent = own.send(&application, &base_and(&[migration]));
            assert_eq!(
                sent.status,
                201,
                "expected the Version to apply, got\n{}",
                sent.describe()
            );
            assert_eq!(
                db.column("SELECT title FROM things WHERE id = 9001"),
                vec!["marker before the transaction control"],
                "expected the marker row committed with the Version"
            );
            let checked = db.superuser().try_coded(check);
            assert!(
                checked.as_ref().is_ok_and(|rows| !rows.is_empty()),
                "expected {check:?} to find what the migration made, got {checked:?}"
            );
        }));
        if let Err(payload) = outcome {
            let why: String = panic_text(payload.as_ref()).chars().take(3000).collect();
            failures.push(format!("--- {name} ---\n{why}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} migrations holding the words of transaction control where they are not were not applied:\n{}",
        failures.len(),
        texts.len(),
        failures.join("\n")
    );
}
