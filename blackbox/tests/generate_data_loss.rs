//! A migration that destroys data (CONTRACT.md, "Generating migrations: A
//! migration that destroys data"; ticket "Generate migrations, stopping
//! before data is lost": dropping a table or column, or narrowing a type,
//! stops with the refused-or-waiting exit code and a message naming what
//! would be lost and saying to ask the Builder; with the explicit
//! confirmation it is generated).
//!
//! Each test starts from a project whose first migration `drizzle-kit`
//! made by hand, changes `src/db/schema.ts` in the one way it is about, and
//! runs `vm generate` on a machine that is not signed in.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::generate::{
    DataLoss, Generated, NOT_WIDER, NOTES_AND_ARCHIVE_SCHEMA_TS, NOTES_WITH_TITLE_SCHEMA_TS,
    SEVERAL_AFTER_SCHEMA_TS, SEVERAL_BEFORE_SCHEMA_TS, WIDER, amount_schema,
    assert_data_loss_message, assert_destroys, assert_unchanged, column, generate,
    generate_in_words, journal_migrations, project_with, schema, table, type_change,
};
use blackbox::own_database::Own;
use blackbox::project::{NOTES_AND_TAGS_SCHEMA_TS, NOTES_SCHEMA_TS, Tree};
use blackbox::sdk::TempProject;
use blackbox::tool::Machine;
use serde_json::Value;

/// Runs `vm generate` on `project` (its schema already changed) with
/// `--json` and in words; fails unless both stop for data that would be
/// lost and nothing is written. Returns what `--json` printed and the words.
fn stopped(machine: &Machine, project: &TempProject) -> (DataLoss, String) {
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let loss = DataLoss::of(&generate(machine, &project.dir, &[]));
    let words = generate_in_words(machine, &project.dir, &[]).text_error(3);
    assert_unchanged(&before, &project.dir, "stopped for data loss");
    (loss, words)
}

#[test]
fn a_dropped_table_stops_with_data_loss_and_writes_nothing() {
    let machine = Machine::new();
    let project = project_with("loss-table", NOTES_AND_TAGS_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let (loss, words) = stopped(&machine, &project);
    assert_destroys(&loss.destroys, &[table("public.tags")], "a dropped table");
    assert_eq!(loss.statements.len(), 1, "{}", loss.run.describe());
    assert!(
        loss.statements[0].contains("DROP TABLE \"tags\""),
        "{:?}",
        loss.statements
    );
    assert_data_loss_message(&loss.message, &["tags"]);
    assert_data_loss_message(&words, &["tags"]);
}

#[test]
fn a_dropped_column_stops_with_data_loss_and_writes_nothing() {
    let machine = Machine::new();
    let project = project_with("loss-column", NOTES_WITH_TITLE_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let (loss, words) = stopped(&machine, &project);
    assert_destroys(
        &loss.destroys,
        &[column("public.notes", "title")],
        "a dropped column",
    );
    assert_eq!(loss.statements.len(), 1, "{}", loss.run.describe());
    assert!(
        loss.statements[0].contains("DROP COLUMN \"title\""),
        "{:?}",
        loss.statements
    );
    assert_data_loss_message(&loss.message, &["title"]);
    assert_data_loss_message(&words, &["title"]);
}

#[test]
fn a_type_made_not_wider_stops_with_data_loss_from_and_to() {
    let machine = Machine::new();
    for (before, from, after, to) in NOT_WIDER {
        let project = project_with("loss-type", &amount_schema(before));
        project.write("src/db/schema.ts", &amount_schema(after));
        let (loss, words) = stopped(&machine, &project);
        let context = format!("{from} to {to}");
        assert_destroys(
            &loss.destroys,
            &[type_change("public.notes", "amount", from, to)],
            &context,
        );
        assert_eq!(
            loss.statements.len(),
            1,
            "{context}: {}",
            loss.run.describe()
        );
        assert!(
            loss.statements[0].contains("SET DATA TYPE"),
            "{context}: {:?}",
            loss.statements
        );
        assert_data_loss_message(&loss.message, &["amount"]);
        assert_data_loss_message(&words, &["amount"]);
    }
}

#[test]
fn a_type_made_wider_goes_through_without_the_flag() {
    let machine = Machine::new();
    for (before, from, after, to) in WIDER {
        let project = project_with("wider-type", &amount_schema(before));
        project.write("src/db/schema.ts", &amount_schema(after));
        let generated = Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
        assert_eq!(
            generated.destroys,
            Vec::<Value>::new(),
            "{from} to {to}: expected a wider type to destroy nothing\n{}",
            generated.run.describe()
        );
        assert!(
            generated.sql(&project.dir).contains("SET DATA TYPE"),
            "{from} to {to}: {}",
            generated.sql(&project.dir)
        );
    }
}

/// `drizzle-kit` drops the schema's table, then the schema: two statements
/// that destroy, each listed.
#[test]
fn a_dropped_schema_stops_with_data_loss_and_names_it() {
    let machine = Machine::new();
    let project = project_with("loss-schema", NOTES_AND_ARCHIVE_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let (loss, words) = stopped(&machine, &project);
    assert_destroys(
        &loss.destroys,
        &[table("archive.old"), schema("archive")],
        "a dropped schema and its table",
    );
    assert_eq!(loss.statements.len(), 2, "{}", loss.run.describe());
    assert!(
        loss.statements[0].contains("DROP TABLE \"archive\".\"old\"")
            && loss.statements[1].contains("DROP SCHEMA \"archive\""),
        "{:?}",
        loss.statements
    );
    assert_data_loss_message(&loss.message, &["archive"]);
    assert_data_loss_message(&words, &["archive"]);
}

#[test]
fn several_losses_at_once_are_all_listed_in_the_order_of_the_statements() {
    let machine = Machine::new();
    let project = project_with("loss-several", SEVERAL_BEFORE_SCHEMA_TS);
    project.write("src/db/schema.ts", SEVERAL_AFTER_SCHEMA_TS);
    let (loss, words) = stopped(&machine, &project);
    assert_destroys(
        &loss.destroys,
        &[
            table("public.tags"),
            type_change("public.notes", "amount", "bigint", "integer"),
            column("public.notes", "body"),
        ],
        "three at once",
    );
    assert_eq!(
        loss.statements.len(),
        3,
        "expected the three destroying statements and no other\n{}",
        loss.run.describe()
    );
    for (statement, holds) in loss.statements.iter().zip([
        "DROP TABLE \"tags\"",
        "SET DATA TYPE integer",
        "DROP COLUMN \"body\"",
    ]) {
        assert!(
            statement.contains(holds),
            "expected {holds:?} in this place, got {:?}",
            loss.statements
        );
    }
    for message in [&loss.message, &words] {
        assert_data_loss_message(message, &["tags", "amount", "body"]);
    }
}

#[test]
fn with_allow_data_loss_it_is_generated_and_says_what_will_be_lost() {
    let machine = Machine::new();
    let project = project_with("loss-allowed", NOTES_WITH_TITLE_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let generated = Generated::made(
        &generate(&machine, &project.dir, &["--allow-data-loss"]),
        &project.dir,
    );
    assert_destroys(
        &generated.destroys,
        &[column("public.notes", "title")],
        "--allow-data-loss",
    );
    assert_eq!(generated.statements, 1);
    assert!(
        generated
            .sql(&project.dir)
            .contains("ALTER TABLE \"notes\" DROP COLUMN \"title\";"),
        "{}",
        generated.sql(&project.dir)
    );

    // In words, on a project of its own.
    let other = project_with("loss-allowed-words", NOTES_WITH_TITLE_SCHEMA_TS);
    other.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let words = generate_in_words(&machine, &other.dir, &["--allow-data-loss"]).ok_text();
    let lower = words.to_lowercase();
    assert!(
        words.contains("title") && (lower.contains("lost") || lower.contains("lose")),
        "expected the words to say what will be lost (the column title) when the migration is applied, got\n{words}"
    );
    assert!(
        words.contains("DROP COLUMN \"title\""),
        "expected the statement shown, got\n{words}"
    );
}

#[test]
fn the_destroying_migration_generated_with_the_flag_is_applied_by_the_platform() {
    let own = Own::start();
    let machine = Machine::new();
    let project = project_with("loss-applied", NOTES_WITH_TITLE_SCHEMA_TS);
    let db = own.install("loss-applied", &journal_migrations(&project.dir));
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    Generated::made(
        &generate(&machine, &project.dir, &["--allow-data-loss"]),
        &project.dir,
    );
    own.send_ok("loss-applied", &journal_migrations(&project.dir));
    assert_eq!(
        db.column(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'notes' ORDER BY ordinal_position"
        ),
        ["id", "body"],
        "expected the column gone"
    );
}
