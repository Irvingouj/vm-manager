//! The harness of the tests of generating migrations checked against the
//! real `drizzle-kit` and the real platform, without `vibe generate`: the
//! shapes the tests change the own schema in make what those tests assume,
//! the journal is read in its order, and the add, copy, drop path made by
//! hand is applied by the platform over rows that are there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::User;
use blackbox::generate::{
    DEFAULT_SAYING_DROP_SCHEMA_TS, DEFAULT_SAYING_DROP_SQL, NOT_WIDER, NOTES_AND_ARCHIVE_SCHEMA_TS,
    NOTES_AND_LABELS_SCHEMA_TS, NOTES_BODY_AND_TEXT_SCHEMA_TS, SEVERAL_AFTER_SCHEMA_TS,
    SEVERAL_BEFORE_SCHEMA_TS, TypeChange, UNPARSEABLE_DEFAULT_SCHEMA_TS, WIDER, amount_schema,
    drizzle_kit, journal_migrations, journal_tags, project_with, statements_of,
};
use blackbox::own_database::{Own, own_execute};
use blackbox::project::{
    NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, NOTES_SCHEMA_TS, sql_files,
};
use blackbox::sdk::TempProject;

/// The statements of the one migration `drizzle-kit` made after the first.
fn second_migration(project: &TempProject) -> Vec<String> {
    let made = drizzle_kit(&project.dir, &[]);
    assert_eq!(made.code, Some(0), "{}", made.describe());
    let files = sql_files(&project.dir);
    assert_eq!(files.len(), 2, "{}", made.describe());
    statements_of(&fs::read_to_string(project.path(&format!("migrations/{}", files[1]))).unwrap())
}

fn assert_type_changes(changes: &[TypeChange], label: &str) {
    for (from, _, to, to_type) in changes {
        let project = project_with(label, &amount_schema(from));
        project.write("src/db/schema.ts", &amount_schema(to));
        let statements = second_migration(&project);
        assert_eq!(
            statements,
            [format!(
                "ALTER TABLE \"notes\" ALTER COLUMN \"amount\" SET DATA TYPE {};",
                to_type.replace(',', ", ")
            )],
            "{from} to {to}"
        );
    }
}

#[test]
fn each_wider_change_of_type_is_one_set_data_type() {
    assert_type_changes(&WIDER, "harness-wider");
}

#[test]
fn each_change_of_type_not_wider_is_one_set_data_type() {
    assert_type_changes(&NOT_WIDER, "harness-not-wider");
}

#[test]
fn the_shapes_that_drop_are_as_the_tests_assume() {
    // A table dropped.
    let project = project_with("harness-drop-table", NOTES_AND_TAGS_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    assert_eq!(second_migration(&project), ["DROP TABLE \"tags\" CASCADE;"]);

    // A column dropped.
    let project = project_with(
        "harness-drop-column",
        blackbox::generate::NOTES_WITH_TITLE_SCHEMA_TS,
    );
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    assert_eq!(
        second_migration(&project),
        ["ALTER TABLE \"notes\" DROP COLUMN \"title\";"]
    );

    // A schema and its table dropped: the table first.
    let project = project_with("harness-drop-schema", NOTES_AND_ARCHIVE_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    assert_eq!(
        second_migration(&project),
        [
            "DROP TABLE \"archive\".\"old\" CASCADE;",
            "DROP SCHEMA \"archive\";"
        ]
    );

    // Three at once, in this order: the table, the type, the column.
    let project = project_with("harness-several", SEVERAL_BEFORE_SCHEMA_TS);
    project.write("src/db/schema.ts", SEVERAL_AFTER_SCHEMA_TS);
    let statements = second_migration(&project);
    let at = |needle: &str| {
        statements
            .iter()
            .position(|s| s.contains(needle))
            .unwrap_or_else(|| panic!("no statement holds {needle:?}: {statements:?}"))
    };
    let (dropped, typed, column) = (
        at("DROP TABLE \"tags\""),
        at("SET DATA TYPE integer"),
        at("DROP COLUMN \"body\""),
    );
    assert!(dropped < typed && typed < column, "{statements:?}");
}

#[test]
fn the_shapes_that_drop_nothing_are_as_the_tests_assume() {
    let project = project_with("harness-string", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", DEFAULT_SAYING_DROP_SCHEMA_TS);
    assert_eq!(second_migration(&project), [DEFAULT_SAYING_DROP_SQL]);

    let project = project_with("harness-unparseable", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", UNPARSEABLE_DEFAULT_SCHEMA_TS);
    assert_eq!(
        second_migration(&project),
        ["ALTER TABLE \"notes\" ADD COLUMN \"odd\" text DEFAULT (((;"]
    );
}

/// `drizzle-kit` asks, and without a terminal writes nothing and ends with
/// `0` (its error, with a stack, on standard error).
#[test]
fn the_changes_drizzle_kit_asks_about_write_nothing_and_end_with_0() {
    for (label, first, second) in [
        (
            "harness-ask-column",
            NOTES_SCHEMA_TS,
            NOTES_RENAMED_SCHEMA_TS,
        ),
        (
            "harness-ask-table",
            NOTES_AND_TAGS_SCHEMA_TS,
            NOTES_AND_LABELS_SCHEMA_TS,
        ),
    ] {
        let project = project_with(label, first);
        project.write("src/db/schema.ts", second);
        let asked = drizzle_kit(&project.dir, &[]);
        assert_eq!(asked.code, Some(0), "{label}: {}", asked.describe());
        assert_eq!(
            sql_files(&project.dir).len(),
            1,
            "{label}: {}",
            asked.describe()
        );
        assert!(
            asked.stderr.contains("TTY"),
            "{label}: {}",
            asked.describe()
        );
    }
}

#[test]
fn the_journal_is_read_in_its_order_with_each_files_text() {
    let project = project_with("harness-journal", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_AND_TAGS_SCHEMA_TS);
    drizzle_kit(&project.dir, &["--name", "tags"]);
    let tags = journal_tags(&project.dir);
    assert_eq!(tags.len(), 2, "{tags:?}");
    assert!(tags[0].starts_with("0000_"), "{tags:?}");
    assert_eq!(tags[1], "0001_tags");
    let migrations = journal_migrations(&project.dir);
    for (migration, tag) in migrations.iter().zip(&tags) {
        assert_eq!(&migration.name, tag);
        assert_eq!(
            migration.sql,
            fs::read_to_string(project.path(&format!("migrations/{tag}.sql"))).unwrap()
        );
    }
    assert!(migrations[1].sql.contains("CREATE TABLE \"tags\""));
}

#[test]
fn the_add_copy_drop_path_made_by_hand_is_applied_by_the_platform_over_rows() {
    let own = Own::start();
    let ann = User::new("ann").in_groups(&["staff"]);
    let project = project_with("harness-path", NOTES_SCHEMA_TS);
    let db = own.install("path-by-hand", &journal_migrations(&project.dir));
    let (_, answers) = own.run(
        &ann,
        "path-by-hand",
        &[own_execute(
            "application",
            "insert into notes (body) values ('one'), ('two')",
        )],
    );
    assert!(answers[0]["ok"].is_object(), "{answers:?}");

    project.write("src/db/schema.ts", NOTES_BODY_AND_TEXT_SCHEMA_TS);
    assert_eq!(drizzle_kit(&project.dir, &[]).code, Some(0));
    let custom = drizzle_kit(&project.dir, &["--custom", "--name", "copy_text"]);
    assert_eq!(custom.code, Some(0), "{}", custom.describe());
    let copy = project.path("migrations/0002_copy_text.sql");
    let written = fs::read_to_string(&copy).unwrap();
    fs::write(
        &copy,
        format!("{written}\nUPDATE \"notes\" SET \"text\" = \"body\";\n"),
    )
    .unwrap();
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    assert_eq!(drizzle_kit(&project.dir, &[]).code, Some(0));
    assert_eq!(sql_files(&project.dir).len(), 4);

    own.send_ok("path-by-hand", &journal_migrations(&project.dir));
    assert_eq!(
        db.column("SELECT text FROM notes ORDER BY id"),
        ["one", "two"]
    );
    assert_eq!(
        db.column(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'notes' ORDER BY ordinal_position"
        ),
        ["id", "text"]
    );
}
