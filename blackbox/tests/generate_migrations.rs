//! A migration generated (CONTRACT.md, "Generating migrations: The command",
//! "An empty migration"; ticket "Generate migrations, stopping before data
//! is lost": adding a table or column generates a migration that the
//! platform's migrator applies; nothing is generated when the schema has
//! not changed; the command has a JSON form).
//!
//! Every `vm generate` here runs on a machine that is not signed in: the
//! command needs no sign-in. A second machine, signed in, runs `vm check`
//! where a test says the check finds `migrations/` in step.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::generate::{
    DEFAULT_SAYING_DROP_SCHEMA_TS, DEFAULT_SAYING_DROP_SQL, Generated, NOTES_WITH_TITLE_SCHEMA_TS,
    UNPARSEABLE_DEFAULT_SCHEMA_TS, assert_unchanged, generate, generate_args, generate_in_words,
    journal_migrations, project_with,
};
use blackbox::own_database::Own;
use blackbox::project::{
    NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, NOTES_SCHEMA_TS, Report, Tree, check,
    checked_project,
};
use blackbox::tool::{Machine, Run};
use blackbox::{User, Vm, VmOptions};
use serde_json::Value;

/// A program and a machine signed in to it, for `vm check`.
fn checker() -> (Vm, Machine) {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &User::new("bea").in_groups(&["builders"]));
    (vm, machine)
}

/// Fails unless `vm check` finds the project clean and `vm generate` then
/// generates nothing and writes nothing (bytes, modification times, the
/// set of files).
fn assert_in_step(machine: &Machine, checking: &Machine, dir: &std::path::Path) {
    Report::clean(&check(checking, dir));
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(dir);
    Generated::nothing(&generate(machine, dir, &[]));
    assert_unchanged(&before, dir, "generating again");
}

#[test]
fn a_table_added_is_one_migration_of_three_files_and_then_all_is_in_step() {
    let (_vm, checking) = checker();
    let machine = Machine::new();
    let project = checked_project("generate-table");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let generated = Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    assert!(
        generated.name.starts_with("0000_"),
        "expected the first migration to be numbered 0000, got {:?}",
        generated.name
    );
    assert_eq!(generated.statements, 1, "{}", generated.run.describe());
    assert_eq!(generated.destroys, Vec::<Value>::new());
    assert!(
        generated
            .sql(&project.dir)
            .contains("CREATE TABLE \"notes\""),
        "{}",
        generated.sql(&project.dir)
    );
    assert_in_step(&machine, &checking, &project.dir);
}

#[test]
fn a_column_added_is_the_next_migration_and_nothing_else_is_left_in_migrations() {
    let (_vm, checking) = checker();
    let machine = Machine::new();
    let project = project_with("generate-column", NOTES_SCHEMA_TS);
    let first = Tree::of(&project.path("migrations")).files();
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let generated = Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    assert!(
        generated.name.starts_with("0001_"),
        "expected the second migration to be numbered 0001, got {:?}",
        generated.name
    );
    assert_eq!(generated.statements, 1);
    assert_eq!(generated.destroys, Vec::<Value>::new());
    assert!(
        generated
            .sql(&project.dir)
            .contains("ALTER TABLE \"notes\" ADD COLUMN \"title\" text;"),
        "{}",
        generated.sql(&project.dir)
    );

    // Written whole: no temporary file beside them, in migrations/ or in
    // migrations/meta/.
    let mut want: Vec<String> = first;
    for file in &generated.files {
        let relative = file.strip_prefix("migrations/").unwrap().to_owned();
        if !want.contains(&relative) {
            want.push(relative);
        }
    }
    want.sort();
    assert_eq!(
        Tree::of(&project.path("migrations")).files(),
        want,
        "expected migrations/ to hold the earlier files and the three written, nothing else"
    );
    assert_in_step(&machine, &checking, &project.dir);
}

#[test]
fn in_words_it_shows_the_statements_and_says_to_read_them_and_commit_the_three_files() {
    let machine = Machine::new();
    let project = project_with("generate-words", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let words = generate_in_words(&machine, &project.dir, &["--name", "add_title"]).ok_text();
    let lower = words.to_lowercase();
    for named in [
        "ALTER TABLE \"notes\" ADD COLUMN \"title\" text;",
        "migrations/0001_add_title.sql",
        "migrations/meta/0001_snapshot.json",
        "migrations/meta/_journal.json",
    ] {
        assert!(
            words.contains(named),
            "expected the words to have {named:?}, got\n{words}"
        );
    }
    assert!(
        lower.contains("read") && lower.contains("commit"),
        "expected the words to say to read the statements and to commit the three files, got\n{words}"
    );
    assert!(project.path("migrations/0001_add_title.sql").is_file());
}

#[test]
fn nothing_to_generate_prints_null_and_writes_nothing() {
    let machine = Machine::new();
    // A project whose one migration is in step.
    let project = project_with("generate-nothing", NOTES_SCHEMA_TS);
    let before = Tree::of(&project.dir);
    Generated::nothing(&generate(&machine, &project.dir, &[]));
    let words = generate_in_words(&machine, &project.dir, &[]).ok_text();
    assert!(
        words.to_lowercase().contains("in step"),
        "expected the words to say the migrations are in step with the schema, got\n{words}"
    );
    assert_unchanged(&before, &project.dir, "nothing to generate");

    // A project as a new one stands: no table, migrations/ holding only
    // .gitkeep. No journal or meta/ is made either.
    let new = checked_project("generate-nothing-new");
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&new.dir);
    Generated::nothing(&generate(&machine, &new.dir, &[]));
    generate_in_words(&machine, &new.dir, &[]).ok_text();
    assert_unchanged(&before, &new.dir, "nothing to generate in a new project");
}

#[test]
fn name_gives_the_migrations_name_after_its_number() {
    let machine = Machine::new();
    let project = project_with("generate-name", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let generated = Generated::made(
        &generate(&machine, &project.dir, &["--name", "add_title"]),
        &project.dir,
    );
    assert_eq!(generated.name, "0001_add_title");
    assert_eq!(generated.file, "migrations/0001_add_title.sql");

    // The longest name the rule takes: 100 characters.
    let long = format!("a_{}", "9".repeat(98));
    let with_tags = format!(
        "{NOTES_WITH_TITLE_SCHEMA_TS}{}",
        NOTES_AND_TAGS_SCHEMA_TS
            .split_once("export const tags")
            .map(|(_, tags)| format!("\nexport const tags{tags}"))
            .unwrap()
    );
    project.write("src/db/schema.ts", &with_tags);
    let generated = Generated::made(
        &generate(&machine, &project.dir, &["--name", &long]),
        &project.dir,
    );
    assert_eq!(generated.name, format!("0002_{long}"));
}

/// Fails unless the run ended with `2`, said why on standard error, and
/// printed on standard output at most the error object.
fn assert_wrong_command_line(run: &Run) {
    run.expect_code(2);
    assert!(!run.stderr.trim().is_empty(), "{}", run.describe());
    if !run.stdout.trim().is_empty() {
        assert!(
            run.json()["error"]["reason"].is_string(),
            "{}",
            run.describe()
        );
    }
}

#[test]
fn a_name_outside_the_rule_is_a_wrong_command_line_and_writes_nothing() {
    let machine = Machine::new();
    let project = project_with("generate-bad-name", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let dir = project.dir.to_str().unwrap();
    let too_long = "a".repeat(101);
    for name in [
        "",
        "Add_title",
        "add-title",
        "add title",
        "add.title",
        "../escape",
        "a/b",
        "é",
        too_long.as_str(),
    ] {
        let run = machine.run_into_files(
            &generate_args(dir, &["--name", name]),
            &[],
            Duration::from_secs(60),
        );
        assert_wrong_command_line(&run);
    }
    assert_unchanged(&before, &project.dir, "a wrong --name");
    // The control: a name in the rule generates.
    Generated::made(
        &generate(&machine, &project.dir, &["--name", "0_ok_9"]),
        &project.dir,
    );
}

#[test]
fn the_platform_applies_what_it_generates_round_after_round() {
    let own = Own::start();
    let machine = Machine::new();
    let project = checked_project("generate-applied");

    // Round 1: a table.
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    let migrations = journal_migrations(&project.dir);
    assert_eq!(migrations.len(), 1);
    let db = own.install("generated-notes", &migrations);
    assert!(db.has_table("notes"), "got {:?}", db.tables());

    // Round 2: a column, then a table.
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    let migrations = journal_migrations(&project.dir);
    assert_eq!(migrations.len(), 2);
    let record = own.send_ok("generated-notes", &migrations);
    assert_eq!(record["number"], 2, "{record}");
    assert_eq!(
        db.column(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'notes' ORDER BY ordinal_position"
        ),
        ["id", "body", "title"],
        "expected the added column applied"
    );
}

#[test]
fn allow_data_loss_on_a_migration_that_destroys_nothing_changes_nothing() {
    let machine = Machine::new();
    let mut printed = Vec::new();
    let mut texts = Vec::new();
    for flag in [false, true] {
        let project = project_with("generate-allow-nothing", NOTES_SCHEMA_TS);
        project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
        let mut extra = vec!["--name", "add_title"];
        if flag {
            extra.push("--allow-data-loss");
        }
        let generated = Generated::made(&generate(&machine, &project.dir, &extra), &project.dir);
        assert_eq!(generated.destroys, Vec::<Value>::new());
        texts.push(generated.sql(&project.dir));
        printed.push(generated.body);
    }
    assert_eq!(
        printed[0], printed[1],
        "expected the same output with and without --allow-data-loss"
    );
    assert_eq!(
        texts[0], texts[1],
        "expected the same migration with and without --allow-data-loss"
    );
}

#[test]
fn drop_table_inside_a_string_is_no_statement_and_destroys_nothing() {
    let machine = Machine::new();
    let project = project_with("generate-string", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", DEFAULT_SAYING_DROP_SCHEMA_TS);
    let generated = Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    assert_eq!(
        generated.destroys,
        Vec::<Value>::new(),
        "{}",
        generated.run.describe()
    );
    assert!(
        generated
            .sql(&project.dir)
            .contains(DEFAULT_SAYING_DROP_SQL),
        "the harness's own check: {}",
        generated.sql(&project.dir)
    );
}

#[test]
fn a_migration_the_parser_cannot_read_is_migration_unparseable_and_nothing_is_written() {
    let machine = Machine::new();
    let project = project_with("generate-unparseable", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", UNPARSEABLE_DEFAULT_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    generate(&machine, &project.dir, &[]).json_error(1, "migration-unparseable");
    generate_in_words(&machine, &project.dir, &[]).text_error(1);
    generate(&machine, &project.dir, &["--allow-data-loss"]).json_error(1, "migration-unparseable");
    assert_unchanged(&before, &project.dir, "migration-unparseable");
}

#[test]
fn an_empty_migration_has_no_statement_and_the_check_passes_after() {
    let (_vm, checking) = checker();
    let machine = Machine::new();
    let project = project_with("generate-empty", NOTES_SCHEMA_TS);
    let generated = Generated::made(
        &generate(
            &machine,
            &project.dir,
            &["--empty", "--name", "copy_values"],
        ),
        &project.dir,
    );
    assert_eq!(generated.name, "0001_copy_values");
    assert_eq!(generated.statements, 0, "{}", generated.run.describe());
    assert_eq!(generated.destroys, Vec::<Value>::new());
    assert_in_step(&machine, &checking, &project.dir);

    // Without --name: drizzle-kit's own name, after the number.
    let generated = Generated::made(
        &generate(&machine, &project.dir, &["--empty"]),
        &project.dir,
    );
    assert!(generated.name.starts_with("0002_"), "{:?}", generated.name);
    assert_eq!(generated.statements, 0);
}

#[test]
fn an_empty_migration_is_not_generated_while_the_schema_is_behind() {
    let machine = Machine::new();
    let project = project_with("generate-empty-behind", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let message = generate(&machine, &project.dir, &["--empty", "--name", "copy"])
        .json_error(1, "migrations-behind");
    assert!(
        message.contains("generate"),
        "expected the message to say to generate the pending change first, got {message:?}"
    );
    generate_in_words(&machine, &project.dir, &["--empty"]).text_error(1);
    assert_unchanged(&before, &project.dir, "--empty while behind");
    // The control: the pending change generates.
    Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
}

/// The pending change is one `drizzle-kit` asks about (a column dropped and
/// another added): the schema is not in step, so `migrations-behind`.
#[test]
fn an_empty_migration_is_not_generated_while_a_change_drizzle_kit_asks_about_is_pending() {
    let machine = Machine::new();
    let project = project_with("generate-empty-asked", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    generate(&machine, &project.dir, &["--empty", "--name", "copy"])
        .json_error(1, "migrations-behind");
    generate_in_words(&machine, &project.dir, &["--empty"]).text_error(1);
    assert_unchanged(&before, &project.dir, "--empty while a rename is pending");
}
