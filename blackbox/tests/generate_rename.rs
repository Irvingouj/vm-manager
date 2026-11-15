//! A change `drizzle-kit` asks about (CONTRACT.md, "Generating migrations:
//! A change drizzle-kit asks about", "An empty migration"; ticket "Generate
//! migrations, stopping before data is lost": a change `drizzle-kit` would
//! treat as a rename is reported with the add, copy, drop path, and is
//! never reported as done when nothing was written).
//!
//! Without a terminal `drizzle-kit` writes nothing for it and still ends
//! with `0` (the harness's own test of that is in `harness_generate.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::time::Duration;

use blackbox::User;
use blackbox::generate::{
    DataLoss, Generated, NOTES_AND_LABELS_SCHEMA_TS, NOTES_BODY_AND_TEXT_SCHEMA_TS,
    assert_destroys, assert_unchanged, column, generate, generate_in_words, generate_with_env,
    journal_migrations, own_tmpdir, project_with,
};
use blackbox::own_database::{Own, own_execute};
use blackbox::project::{
    NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, NOTES_SCHEMA_TS, Tree, sql_files, squashed,
    stack_line_in,
};
use blackbox::tool::{Machine, Run};

/// Fails unless the message gives what the contract says: that
/// `drizzle-kit` cannot tell a rename from a removal and an addition, and
/// the three steps, naming `vm generate --empty` and the `UPDATE` that
/// copies the values; with no stack and no path of the command's own
/// folder in `tmp`.
fn assert_question_message(message: &str, tmp: &std::path::Path) {
    let flat = squashed(message);
    let lower = flat.to_lowercase();
    assert!(
        lower.contains("rename"),
        "expected the message to say drizzle-kit cannot tell a rename from a removal and an addition, got {message:?}"
    );
    assert!(
        flat.contains("vm generate --empty"),
        "expected the message to name `vm generate --empty`, got {message:?}"
    );
    assert!(
        flat.contains("UPDATE"),
        "expected the message to give the statement that copies the values (UPDATE ... SET <new> = <old>), got {message:?}"
    );
    assert!(
        flat.matches("vm generate").count() >= 3,
        "expected each of the three steps followed by `vm generate`, got {message:?}"
    );
    assert!(
        flat.contains("src/db/schema.ts"),
        "expected the steps to say what to change in src/db/schema.ts, got {message:?}"
    );
    assert_eq!(
        stack_line_in(message),
        None,
        "expected no line of drizzle-kit's stack, got {message:?}"
    );
    let tmp = tmp.to_string_lossy();
    assert!(
        !message.contains(tmp.as_ref()),
        "expected no path of the command's own folder, got {message:?}"
    );
}

/// Fails unless nothing the run printed says a migration was generated:
/// no `generated` beside the error, nothing on standard output in words,
/// and not `drizzle-kit`'s own words for a file it wrote.
fn assert_no_word_of_a_migration(run: &Run) {
    for (label, text) in run.texts() {
        for words in ["Your SQL migration file", "[✓]", "migration generated"] {
            assert!(
                !text.contains(words),
                "{label} says {words:?}\n{}",
                run.describe()
            );
        }
    }
}

fn assert_asked(first: &str, second: &str, label: &str) {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    let project = project_with(label, first);
    project.write("src/db/schema.ts", second);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);

    let run = generate_with_env(
        &machine,
        &project.dir,
        &[],
        &[("TMPDIR", tmp.to_str().unwrap())],
    );
    let message = run.json_error(1, "rename-not-generated");
    assert!(
        run.json().get("generated").is_none(),
        "expected no `generated` beside the error\n{}",
        run.describe()
    );
    assert_no_word_of_a_migration(&run);
    assert_question_message(&message, &tmp);

    let words = generate_in_words(&machine, &project.dir, &[]);
    let said = words.text_error(1);
    assert_no_word_of_a_migration(&words);
    assert_question_message(&said, &tmp);

    // With the flag too: there is nothing to allow.
    generate(&machine, &project.dir, &["--allow-data-loss"]).json_error(1, "rename-not-generated");
    assert_unchanged(&before, &project.dir, label);
    assert_eq!(sql_files(&project.dir).len(), 1);
}

#[test]
fn a_column_dropped_and_another_added_in_its_table_is_rename_not_generated() {
    assert_asked(NOTES_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, "rename-column");
}

#[test]
fn a_table_dropped_and_another_added_is_rename_not_generated() {
    assert_asked(
        NOTES_AND_TAGS_SCHEMA_TS,
        NOTES_AND_LABELS_SCHEMA_TS,
        "rename-table",
    );
}

/// The path the message gives, followed: `body` becomes `text` over rows
/// that are there, and the platform applies the three migrations.
#[test]
fn the_add_copy_drop_path_renames_a_column_over_rows_that_are_there() {
    let own = Own::start();
    let ann = User::new("ann").in_groups(&["staff"]);
    let machine = Machine::new();
    let project = project_with("rename-path", NOTES_SCHEMA_TS);
    let db = own.install("rename-path", &journal_migrations(&project.dir));
    let (_, answers) = own.run(
        &ann,
        "rename-path",
        &[own_execute(
            "application",
            "insert into notes (body) values ('one'), ('two')",
        )],
    );
    assert!(answers[0]["ok"].is_object(), "{answers:?}");

    // 1. The new column added, the old one kept.
    project.write("src/db/schema.ts", NOTES_BODY_AND_TEXT_SCHEMA_TS);
    let added = Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    assert_eq!(added.destroys, Vec::<serde_json::Value>::new());

    // 2. An empty migration, filled with the statement that copies.
    let copy = Generated::made(
        &generate(&machine, &project.dir, &["--empty", "--name", "copy_text"]),
        &project.dir,
    );
    assert_eq!(copy.name, "0002_copy_text");
    assert_eq!(copy.statements, 0);
    let path = project.path(&copy.file);
    let written = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        format!("{written}\nUPDATE \"notes\" SET \"text\" = \"body\";\n"),
    )
    .unwrap();

    // 3. The old column removed: it stops for the data `body` holds, and
    // goes through with the flag.
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    let loss = DataLoss::of(&generate(&machine, &project.dir, &[]));
    assert_destroys(&loss.destroys, &[column("public.notes", "body")], "step 3");
    let dropped = Generated::made(
        &generate(&machine, &project.dir, &["--allow-data-loss"]),
        &project.dir,
    );
    assert_destroys(
        &dropped.destroys,
        &[column("public.notes", "body")],
        "step 3 with the flag",
    );

    let migrations = journal_migrations(&project.dir);
    assert_eq!(migrations.len(), 4, "{migrations:?}");
    own.send_ok("rename-path", &migrations);
    assert_eq!(
        db.column("SELECT text FROM notes ORDER BY id"),
        ["one", "two"],
        "expected the values in the new column"
    );
    assert_eq!(
        db.column(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'notes' ORDER BY ordinal_position"
        ),
        ["id", "text"]
    );
}
