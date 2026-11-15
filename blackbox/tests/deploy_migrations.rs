//! Migrations at deploy (CONTRACT.md, "Deploying: The command", steps 7 and
//! 8, "What it prints", "What the installation may answer"; ticket "Deploy:
//! a Version built on the Contributor's machine goes live": a
//! data-destroying migration, when a Version is already serving, needs the
//! Contributor's explicit confirmation again at deploy; a failed migration
//! leaves the previous Version serving and the command reports the
//! database's error).
//!
//! Each project has an own table `notes` (`id`, `body`, `title`) made by
//! its first migration and two rows put in by a second, hand-written one
//! (`vm generate --empty`): so that once the first Version serves, the rows
//! are there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::deploy::{
    DataLossAtDeploy, Deployed, NOTES_WITH_TITLE_AND_COPY_SCHEMA_TS,
    NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS, NOTES_WITH_TITLE_REQUIRED_SCHEMA_TS, commit_all, dana,
    deploy, deploy_in_words, init_repository, project_tree, signed_in,
};
use blackbox::generate::{
    Generated, NOTES_WITH_TITLE_SCHEMA_TS, amount_schema, assert_data_loss_message,
    assert_destroys, column, generate, journal, journal_tags, project_with, type_change,
};
use blackbox::own_database::OwnDatabase;
use blackbox::project::{NOTES_SCHEMA_TS, manifest_ts, squashed};
use blackbox::sdk::TempProject;
use blackbox::tool::Machine;
use blackbox::{Vm, VmOptions};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// The rows the second migration puts in.
const SEED: &str = "INSERT INTO \"notes\" (\"body\", \"title\") VALUES ('first', 'kept title'), ('second', NULL);\n";

/// Writes `sql` into a new empty migration of the project named `name`
/// (`vm generate --empty --name <name>`); returns its tag.
fn hand_written(machine: &Machine, project: &TempProject, name: &str, sql: &str) -> String {
    let made = Generated::made(
        &generate(machine, &project.dir, &["--empty", "--name", name]),
        &project.dir,
    );
    fs::write(project.dir.join(&made.file), sql).unwrap();
    made.name
}

/// Changes the own schema to `schema` and runs `vm generate` (with these
/// arguments), which must generate a migration; its tag.
fn generated(machine: &Machine, project: &TempProject, schema: &str, extra: &[&str]) -> Generated {
    project.write("src/db/schema.ts", schema);
    Generated::made(&generate(machine, &project.dir, extra), &project.dir)
}

/// The project named `name`: `notes` and its rows, in a repository of its
/// own, committed.
fn notes_project(machine: &Machine, label: &str, name: &str) -> TempProject {
    let project = project_with(label, NOTES_WITH_TITLE_SCHEMA_TS);
    project.write("manifest.ts", &manifest_ts(name, "{}"));
    hand_written(machine, &project, "seed_notes", SEED);
    init_repository(&project.dir);
    commit_all(&project.dir, "notes and their rows");
    project
}

fn own(vm: &Vm, name: &str) -> OwnDatabase {
    OwnDatabase::find(vm.platform(), name)
        .unwrap_or_else(|| panic!("expected an own database for {name:?}"))
}

/// The columns of `public.notes` in the own database, in order.
fn notes_columns(db: &OwnDatabase) -> Vec<String> {
    db.column(
        "SELECT attname FROM pg_attribute WHERE attrelid = 'public.notes'::regclass \
         AND attnum > 0 AND NOT attisdropped ORDER BY attnum",
    )
}

/// The Application's Versions and which one serves.
fn versions_of(vm: &Vm, name: &str) -> (usize, Value) {
    let api = vm.api_as(&dana());
    (
        api.versions(name).len(),
        api.application(name)["serving_version"].clone(),
    )
}

#[test]
fn a_versions_migrations_are_applied_and_each_deploy_names_those_it_added() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-migrations", "notes-board");
    let tags = journal_tags(&project.dir);
    assert_eq!(tags.len(), 2, "the harness's own check");
    let first = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        first.migrations_applied, tags,
        "expected `migrations_applied` to name the migrations this Version added, in order"
    );
    let db = own(&vm, "notes-board");
    assert!(db.has_table("notes"));
    assert_eq!(
        db.column("SELECT body FROM notes ORDER BY id"),
        ["first", "second"]
    );
    let version = vm
        .api_as(&dana())
        .get_version("notes-board", 1)
        .expect_status(200)
        .json();
    let names: Vec<&str> = version["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names, tags,
        "expected the Version to carry the migrations in the journal's order"
    );

    let tags_table = generated(&machine, &project, NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS, &[]);
    commit_all(&project.dir, "tags");
    let second = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(second.version, 2);
    assert_eq!(
        second.migrations_applied,
        std::slice::from_ref(&tags_table.name),
        "expected only the migration this Version added"
    );
    assert!(db.has_table("tags"));
    // A deploy with no new migration names none.
    project.write("README.md", "A change of no migration.\n");
    commit_all(&project.dir, "readme");
    let third = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(third.migrations_applied, Vec::<String>::new());
}

#[test]
fn a_destroying_migration_with_a_version_serving_is_asked_about_again_at_deploy() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-data-loss", "losing-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    // Allowed when it was generated; asked again at deploy.
    let dropping = generated(&machine, &project, NOTES_SCHEMA_TS, &["--allow-data-loss"]);
    commit_all(&project.dir, "drop title");
    let before = project_tree(&project.dir);
    let run = deploy(&machine, &project.dir, &[]);
    let loss = DataLossAtDeploy::of(&run);
    assert_eq!(loss.migrations.len(), 1, "{}", run.describe());
    let stopped = &loss.migrations[0];
    assert_eq!(stopped.migration, dropping.name);
    blackbox::generate::assert_destroys(
        &stopped.destroys,
        &[column("public.notes", "title")],
        "at deploy",
    );
    assert_eq!(stopped.statements.len(), 1, "{}", run.describe());
    assert!(
        stopped.statements[0].contains("DROP COLUMN") && stopped.statements[0].contains("title"),
        "expected the destroying statement as the migration has it, got {:?}",
        stopped.statements
    );
    assert!(
        !stopped.statements[0].contains("statement-breakpoint"),
        "expected the statement without the breakpoint line"
    );
    assert_data_loss_message(&loss.message, &["title"]);
    let lower = loss.message.to_lowercase();
    assert!(
        lower.contains("again") && lower.contains("rows"),
        "expected the message to say this is asked again at deploy because the rows are there now, got {:?}",
        loss.message
    );
    deploy_in_words(&machine, &project.dir, &[]).text_error(3);
    // Nothing changed on the installation.
    assert_eq!(versions_of(&vm, "losing-board"), (1, json!(1)));
    let db = own(&vm, "losing-board");
    assert!(notes_columns(&db).contains(&"title".to_owned()));
    assert_eq!(
        db.column("SELECT title FROM notes WHERE title IS NOT NULL"),
        ["kept title"]
    );
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );

    // Confirmed.
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.version, 2);
    assert_eq!(
        deployed.migrations_applied,
        std::slice::from_ref(&dropping.name)
    );
    assert!(
        !notes_columns(&db).contains(&"title".to_owned()),
        "expected the column gone"
    );
}

#[test]
fn a_destroying_migration_in_the_first_deploy_is_not_asked_about() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = project_with("deploy-first-loss", NOTES_WITH_TITLE_SCHEMA_TS);
    project.write("manifest.ts", &manifest_ts("first-loss-board", "{}"));
    let dropping = generated(&machine, &project, NOTES_SCHEMA_TS, &["--allow-data-loss"]);
    init_repository(&project.dir);
    commit_all(&project.dir, "made and dropped");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.version, 1);
    assert_eq!(
        deployed.migrations_applied.last(),
        Some(&dropping.name),
        "{}",
        deployed.run.describe()
    );
    assert!(!notes_columns(&own(&vm, "first-loss-board")).contains(&"title".to_owned()));
}

#[test]
fn a_hand_written_delete_or_truncate_is_rows_destroyed() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-rows", "rows-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let tag = hand_written(
        &machine,
        &project,
        "clear_notes",
        "DELETE FROM \"notes\" WHERE \"title\" IS NULL;\n",
    );
    commit_all(&project.dir, "delete");
    let rows = json!({ "what": "rows", "table": "public.notes" });
    let loss = DataLossAtDeploy::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(loss.migrations.len(), 1);
    assert_eq!(loss.migrations[0].migration, tag);
    assert_eq!(loss.migrations[0].destroys, vec![rows.clone()], "a DELETE");
    assert_eq!(loss.migrations[0].statements.len(), 1);
    assert!(loss.migrations[0].statements[0].contains("DELETE FROM"));

    let db = own(&vm, "rows-board");
    assert_eq!(db.column("SELECT count(*) FROM notes"), ["2"]);
    assert_eq!(versions_of(&vm, "rows-board"), (1, json!(1)));

    // Confirmed: the DELETE applies, Version 2 serves.
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.version, 2);
    assert_eq!(deployed.migrations_applied, std::slice::from_ref(&tag));
    assert_eq!(db.column("SELECT body FROM notes ORDER BY id"), ["first"]);

    // A TRUNCATE: asked about as rows destroyed; once allowed, the platform
    // refuses it (it never truncates a table that has history: VM002), and
    // Version 2 goes on serving with the row still there.
    let truncating = hand_written(&machine, &project, "empty_notes", "TRUNCATE \"notes\";\n");
    commit_all(&project.dir, "truncate");
    let loss = DataLossAtDeploy::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(loss.migrations.len(), 1);
    assert_eq!(loss.migrations[0].migration, truncating);
    assert_eq!(loss.migrations[0].destroys, vec![rows], "a TRUNCATE");
    assert!(loss.migrations[0].statements[0].contains("TRUNCATE"));
    let run = deploy(&machine, &project.dir, &["--allow-data-loss"]);
    run.json_error(1, "migration-failed");
    let error = run.json()["error"].clone();
    assert_eq!(error["code"], json!("VM002"), "{}", run.describe());
    assert_eq!(error["migration"], json!(truncating));
    assert_ne!(error.get("created"), Some(&json!(true)));
    assert_eq!(versions_of(&vm, "rows-board"), (2, json!(2)));
    assert_eq!(db.column("SELECT body FROM notes ORDER BY id"), ["first"]);
}

#[test]
fn an_update_that_copies_values_is_no_data_loss() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-copy", "copy-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let added = generated(&machine, &project, NOTES_WITH_TITLE_AND_COPY_SCHEMA_TS, &[]);
    let copy = hand_written(
        &machine,
        &project,
        "copy_body",
        "UPDATE \"notes\" SET \"copy\" = \"body\";\n",
    );
    commit_all(&project.dir, "copy body");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.migrations_applied, [added.name, copy]);
    assert_eq!(
        own(&vm, "copy-board").column("SELECT copy FROM notes ORDER BY id"),
        ["first", "second"]
    );
}

#[test]
fn a_deployed_migration_whose_text_changed_is_migrations_diverged() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-diverged-text", "diverged-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let first = journal_tags(&project.dir)[0].clone();
    let path = project.dir.join(format!("migrations/{first}.sql"));
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\n-- changed after it was deployed\n");
    fs::write(&path, text).unwrap();
    commit_all(&project.dir, "changed a deployed migration");
    let before = project_tree(&project.dir);
    let run = deploy(&machine, &project.dir, &[]);
    let message = run.json_error(1, "migrations-diverged");
    assert!(
        message.contains(&first),
        "expected the message to say which migration differs ({first}), got {message:?}"
    );
    let lower = message.to_lowercase();
    assert!(
        message.contains("migrations/") && lower.contains("history"),
        "expected the message to say to bring migrations/ in line with the repository's history, got {message:?}"
    );
    deploy_in_words(&machine, &project.dir, &[]).text_error(1);
    assert_eq!(versions_of(&vm, "diverged-board"), (1, json!(1)));
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn a_deployed_migration_removed_with_its_journal_entry_is_migrations_diverged() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-diverged-gone", "gone-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let tags = journal_tags(&project.dir);
    let seed = tags[1].clone();
    // The hand-written migration, its snapshot and its journal entry gone.
    fs::remove_file(project.dir.join(format!("migrations/{seed}.sql"))).unwrap();
    let number = seed.split('_').next().unwrap();
    fs::remove_file(
        project
            .dir
            .join(format!("migrations/meta/{number}_snapshot.json")),
    )
    .unwrap();
    let mut journal = journal(&project.dir);
    journal["entries"].as_array_mut().unwrap().pop();
    fs::write(
        project.dir.join("migrations/meta/_journal.json"),
        serde_json::to_string_pretty(&journal).unwrap(),
    )
    .unwrap();
    commit_all(&project.dir, "removed a deployed migration");
    let message = deploy(&machine, &project.dir, &[]).json_error(1, "migrations-diverged");
    assert!(
        message.contains(&seed),
        "expected the message to say which migration differs ({seed}), got {message:?}"
    );
    assert_eq!(versions_of(&vm, "gone-board"), (1, json!(1)));
}

#[test]
fn a_migration_that_fails_in_the_database_leaves_the_previous_version_serving() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-failing", "failing-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    // `title` made NOT NULL over a row whose title is null.
    let tightened = generated(&machine, &project, NOTES_WITH_TITLE_REQUIRED_SCHEMA_TS, &[]);
    commit_all(&project.dir, "title required");
    let before = project_tree(&project.dir);
    let run = deploy(&machine, &project.dir, &[]);
    let message = run.json_error(1, "migration-failed");
    let error = run.json()["error"].clone();
    assert_eq!(error["migration"], json!(tightened.name));
    assert_eq!(error["code"], json!("23502"), "{}", run.describe());
    let database_message = error["database_message"].as_str().unwrap_or_default();
    assert!(!database_message.is_empty(), "{}", run.describe());
    assert!(
        message.contains(database_message),
        "expected the message to have the database's error ({database_message:?}), got {message:?}"
    );
    let lower = squashed(&message).to_lowercase();
    assert!(
        lower.contains("goes on serving") || lower.contains("go on serving"),
        "expected the message to say the Version that was serving goes on serving, got {message:?}"
    );
    assert!(
        lower.contains("nothing"),
        "expected the message to say nothing was changed, got {message:?}"
    );
    assert_ne!(
        error.get("created"),
        Some(&json!(true)),
        "a deploy that did not create the Application has no `created: true`"
    );
    deploy_in_words(&machine, &project.dir, &[]).text_error(1);
    assert_eq!(versions_of(&vm, "failing-board"), (1, json!(1)));
    let page = vm.get_as(&dana(), "failing-board", "/");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert!(page.text().contains("hello dana"));
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );

    // A hand-written statement that fails.
    git_reset_to_previous(&project);
    let failing = hand_written(&machine, &project, "divide", "SELECT 1 / 0;\n");
    commit_all(&project.dir, "divide by zero");
    let run = deploy(&machine, &project.dir, &[]);
    run.json_error(1, "migration-failed");
    let error = run.json()["error"].clone();
    assert_eq!(error["migration"], json!(failing));
    assert_eq!(error["code"], json!("22012"), "{}", run.describe());
    assert_eq!(versions_of(&vm, "failing-board"), (1, json!(1)));
}

/// Takes the project back to its previous commit.
fn git_reset_to_previous(project: &TempProject) {
    blackbox::deploy::git_ok(&project.dir, &["reset", "-q", "--hard", "HEAD~1"]);
}

#[test]
fn a_migration_the_platform_refuses_is_migration_refused_with_its_rule() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-refused", "refused-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let committing = hand_written(&machine, &project, "commit_early", "COMMIT;\n");
    commit_all(&project.dir, "commit");
    let run = deploy(&machine, &project.dir, &[]);
    run.json_error(1, "migration-refused");
    let error = run.json()["error"].clone();
    assert_eq!(
        error["rule"],
        json!("transaction-control"),
        "{}",
        run.describe()
    );
    assert_eq!(error["migration"], json!(committing));
    assert_eq!(versions_of(&vm, "refused-board"), (1, json!(1)));
    assert_eq!(vm.get_as(&dana(), "refused-board", "/").status, 200);
}

#[test]
fn two_added_migrations_that_destroy_are_each_named_in_their_order() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-two-losses", "two-losses-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let dropping = generated(&machine, &project, NOTES_SCHEMA_TS, &["--allow-data-loss"]);
    let deleting = hand_written(
        &machine,
        &project,
        "drop_second",
        "DELETE FROM \"notes\" WHERE \"body\" = 'second';\n",
    );
    commit_all(&project.dir, "two losses");
    let run = deploy(&machine, &project.dir, &[]);
    let loss = DataLossAtDeploy::of(&run);
    let names: Vec<&str> = loss
        .migrations
        .iter()
        .map(|m| m.migration.as_str())
        .collect();
    assert_eq!(
        names,
        [dropping.name.as_str(), deleting.as_str()],
        "expected one item for each destroying migration, in their order\n{}",
        run.describe()
    );
    blackbox::generate::assert_destroys(
        &loss.migrations[0].destroys,
        &[column("public.notes", "title")],
        "the first",
    );
    assert_eq!(loss.migrations[0].statements.len(), 1);
    assert!(loss.migrations[0].statements[0].contains("DROP COLUMN"));
    assert_eq!(
        loss.migrations[1].destroys,
        vec![json!({ "what": "rows", "table": "public.notes" })]
    );
    assert_eq!(loss.migrations[1].statements.len(), 1);
    assert!(loss.migrations[1].statements[0].contains("DELETE FROM"));
    assert_eq!(versions_of(&vm, "two-losses-board"), (1, json!(1)));

    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.migrations_applied, [dropping.name, deleting]);
    let db = own(&vm, "two-losses-board");
    assert!(!notes_columns(&db).contains(&"title".to_owned()));
    assert_eq!(db.column("SELECT body FROM notes ORDER BY id"), ["first"]);
}

/// A project named `name` whose own table `notes` has a column `amount`
/// of the Drizzle builder `builder`, its first migration made, a row in
/// it, deployed as Version 1.
fn deployed_amount(
    vm: &Vm,
    machine: &Machine,
    label: &str,
    name: &str,
    builder: &str,
) -> TempProject {
    let project = project_with(label, &amount_schema(builder));
    project.write("manifest.ts", &manifest_ts(name, "{}"));
    hand_written(
        machine,
        &project,
        "one_row",
        "INSERT INTO \"notes\" (\"amount\") VALUES ('7');\n",
    );
    init_repository(&project.dir);
    commit_all(&project.dir, "amount");
    let first = Deployed::of(&deploy(machine, &project.dir, &[]));
    assert_eq!(first.version, 1);
    assert_eq!(versions_of(vm, name), (1, json!(1)));
    project
}

#[test]
fn a_narrowing_type_change_is_asked_about_at_deploy() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deployed_amount(
        &vm,
        &machine,
        "deploy-narrowing",
        "narrowing-board",
        "text(\"amount\")",
    );
    let narrowing = generated(
        &machine,
        &project,
        &amount_schema("varchar(\"amount\", { length: 10 })"),
        &["--allow-data-loss"],
    );
    commit_all(&project.dir, "narrow amount");
    let loss = DataLossAtDeploy::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(loss.migrations.len(), 1);
    assert_eq!(loss.migrations[0].migration, narrowing.name);
    assert_destroys(
        &loss.migrations[0].destroys,
        &[type_change("public.notes", "amount", "text", "varchar(10)")],
        "text to varchar(10)",
    );
    assert_eq!(versions_of(&vm, "narrowing-board"), (1, json!(1)));
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.version, 2);
}

#[test]
fn a_widening_type_change_is_not_asked_about() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deployed_amount(
        &vm,
        &machine,
        "deploy-widening",
        "widening-board",
        "integer(\"amount\")",
    );
    // `vm generate` writes a widening without --allow-data-loss.
    let widening = generated(
        &machine,
        &project,
        &amount_schema("bigint(\"amount\", { mode: \"number\" })"),
        &[],
    );
    assert_eq!(widening.destroys, Vec::<Value>::new());
    commit_all(&project.dir, "widen amount");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.version, 2);
    assert_eq!(deployed.migrations_applied, [widening.name]);
}

#[test]
fn each_type_change_is_judged_against_the_snapshot_of_the_migration_before_it() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deployed_amount(
        &vm,
        &machine,
        "deploy-two-types",
        "two-types-board",
        "integer(\"amount\")",
    );
    // integer to bigint (wider), then bigint to integer (not): only the
    // second is asked about, and from `bigint`, the type its own previous
    // snapshot has.
    let widening = generated(
        &machine,
        &project,
        &amount_schema("bigint(\"amount\", { mode: \"number\" })"),
        &[],
    );
    let narrowing = generated(
        &machine,
        &project,
        &amount_schema("integer(\"amount\")"),
        &["--allow-data-loss"],
    );
    commit_all(&project.dir, "widen then narrow");
    let run = deploy(&machine, &project.dir, &[]);
    let loss = DataLossAtDeploy::of(&run);
    assert_eq!(
        loss.migrations
            .iter()
            .map(|m| m.migration.as_str())
            .collect::<Vec<_>>(),
        [narrowing.name.as_str()],
        "expected only the narrowing asked about\n{}",
        run.describe()
    );
    assert_destroys(
        &loss.migrations[0].destroys,
        &[type_change("public.notes", "amount", "bigint", "integer")],
        "bigint to integer, after integer to bigint",
    );
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.migrations_applied, [widening.name, narrowing.name]);
}

#[test]
fn a_delete_in_a_with_is_rows_destroyed_and_one_in_a_string_or_a_comment_is_not() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-with-delete", "with-delete-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));

    // Words only: in a string and in comments.
    let words = hand_written(
        &machine,
        &project,
        "only_words",
        "-- DELETE FROM \"notes\";\n/* TRUNCATE \"notes\"; */\nSELECT 'DELETE FROM notes; TRUNCATE notes';\n",
    );
    commit_all(&project.dir, "words");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.version, 2,
        "expected no question for DELETE in a string or a comment"
    );
    assert_eq!(deployed.migrations_applied, [words]);

    // A DELETE in a WITH.
    let with = hand_written(
        &machine,
        &project,
        "with_delete",
        "WITH gone AS (DELETE FROM \"notes\" WHERE \"title\" IS NULL RETURNING id) SELECT count(*) FROM gone;\n",
    );
    commit_all(&project.dir, "with delete");
    let loss = DataLossAtDeploy::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(loss.migrations.len(), 1);
    assert_eq!(loss.migrations[0].migration, with);
    assert_eq!(
        loss.migrations[0].destroys,
        vec![json!({ "what": "rows", "table": "public.notes" })]
    );
    assert_eq!(loss.migrations[0].statements.len(), 1);
    assert!(loss.migrations[0].statements[0].contains("WITH"));
    assert_eq!(versions_of(&vm, "with-delete-board"), (2, json!(2)));
    assert_eq!(
        own(&vm, "with-delete-board").column("SELECT count(*) FROM notes"),
        ["2"]
    );
}

#[test]
fn an_added_migration_the_parser_cannot_read_is_migration_unparseable() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-unparseable", "unparseable-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    hand_written(&machine, &project, "broken", "CREATE TABLE ((( broken;\n");
    commit_all(&project.dir, "broken SQL");
    let before = project_tree(&project.dir);
    deploy(&machine, &project.dir, &[]).json_error(1, "migration-unparseable");
    assert_eq!(versions_of(&vm, "unparseable-board"), (1, json!(1)));
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn a_migration_whose_text_is_not_utf_8_is_migrations_unreadable() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "deploy-not-utf8", "not-utf8-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let made = Generated::made(
        &generate(&machine, &project.dir, &["--empty", "--name", "latin"]),
        &project.dir,
    );
    fs::write(
        project.dir.join(&made.file),
        b"SELECT 'caf\xe9';\n".as_slice(),
    )
    .unwrap();
    commit_all(&project.dir, "latin-1 bytes");
    let before = project_tree(&project.dir);
    deploy(&machine, &project.dir, &[]).json_error(1, "migrations-unreadable");
    assert_eq!(versions_of(&vm, "not-utf8-board"), (1, json!(1)));
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn an_application_this_deploy_created_stays_when_its_version_is_not_taken() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = blackbox::project::checked_project("deploy-created-stays");
    project.write("manifest.ts", &manifest_ts("stays-board", "{}"));
    let failing = hand_written(
        &machine,
        &project,
        "into_nowhere",
        "INSERT INTO \"nowhere\" VALUES (1);\n",
    );
    init_repository(&project.dir);
    commit_all(&project.dir, "a migration that fails");
    let run = deploy(&machine, &project.dir, &[]);
    let message = run.json_error(1, "migration-failed");
    let error = run.json()["error"].clone();
    assert_eq!(error["created"], json!(true), "{}", run.describe());
    assert_eq!(error["migration"], json!(failing));
    let database_message = error["database_message"].as_str().unwrap_or_default();
    assert!(
        !database_message.is_empty() && message.contains(database_message),
        "expected the message to have the database's error, got {message:?}"
    );
    let lower = squashed(&message).to_lowercase();
    assert!(
        lower.contains("no version") && (lower.contains("exists") || lower.contains("created")),
        "expected the message to say the Application now exists with no Version, got {message:?}"
    );
    // There was no Version serving: the message may give the contract's
    // sentence ("when there was one"), and never says a previous Version
    // goes on serving without it.
    if lower.contains("goes on serving") || lower.contains("go on serving") {
        assert!(
            lower.contains("when") || lower.contains("if there"),
            "expected no word that a Version goes on serving at a first deploy, but for the contract's \"when there was one\", got {message:?}"
        );
    }
    assert!(
        !lower.contains("previous version goes on serving"),
        "expected no previous Version said to serve at a first deploy, got {message:?}"
    );
    let api = vm.api_as(&dana());
    let application = api.application("stays-board");
    assert_eq!(application["serving_version"], Value::Null);
    assert_eq!(application["contributors"], json!([{ "user": "dana" }]));
    assert_eq!(api.versions("stays-board"), Vec::<Value>::new());
    let page = vm.get_as(&dana(), "stays-board", "/");
    assert_ne!(
        page.status,
        200,
        "expected nothing at the address of an Application with no Version\n{}",
        page.describe()
    );

    // The migration corrected (it was never taken) and committed.
    fs::write(
        project.dir.join(format!("migrations/{failing}.sql")),
        "SELECT 1;\n",
    )
    .unwrap();
    commit_all(&project.dir, "corrected");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert!(!deployed.created, "expected `created` false the next time");
    assert_eq!(deployed.version, 1);
    assert_eq!(api.application("stays-board")["serving_version"], json!(1));
}
