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
    Generated, NOTES_WITH_TITLE_SCHEMA_TS, assert_data_loss_message, column, generate, journal,
    journal_tags, project_with,
};
use blackbox::own_database::OwnDatabase;
use blackbox::project::{NOTES_SCHEMA_TS, manifest_ts};
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

    // The same migration, not yet deployed, now a TRUNCATE.
    fs::write(
        project.dir.join(format!("migrations/{tag}.sql")),
        "TRUNCATE \"notes\";\n",
    )
    .unwrap();
    commit_all(&project.dir, "truncate");
    let loss = DataLossAtDeploy::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(loss.migrations[0].destroys, vec![rows], "a TRUNCATE");
    assert!(loss.migrations[0].statements[0].contains("TRUNCATE"));
    let db = own(&vm, "rows-board");
    assert_eq!(db.column("SELECT count(*) FROM notes"), ["2"]);
    assert_eq!(versions_of(&vm, "rows-board"), (1, json!(1)));

    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    assert_eq!(deployed.migrations_applied, [tag]);
    assert_eq!(db.column("SELECT count(*) FROM notes"), ["0"]);
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
    let lower = message.to_lowercase();
    assert!(
        lower.contains("previous") && lower.contains("serving"),
        "expected the message to say the previous Version goes on serving, got {message:?}"
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
