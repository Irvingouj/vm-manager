//! The local own database and the local Versions (CONTRACT.md, "The local
//! loop: one request as a test User: What a run does", steps 2, 6 and 7;
//! ticket "Local loop: run the Application on the Builder's machine and
//! send a request as a test User": a schema change followed by a generated
//! migration is applied on the next run).
//!
//! A project made by `vm new` declaring no Resource, whose own database has
//! `notes` (its first migration made by `vm generate`), and whose handler
//! writes and reads notes as the User (`/notes`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::deploy::NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS;
use blackbox::generate::{Generated, NOTES_WITH_TITLE_SCHEMA_TS, generate};
use blackbox::local::{
    Local, OUTSIDER, Requested, dev_json, handler, local_project, write_dev_json,
};
use blackbox::project::{NOTES_AND_TAGS_SCHEMA_TS, NOTES_SCHEMA_TS};
use blackbox::scenario::{Expected, assert_entry};
use blackbox::sdk::TempProject;
use serde_json::json;

const NAME: &str = "notes-board";

/// `vm generate --name <name>` on the project, which must generate one
/// migration; its name.
fn generated(local: &Local, dir: &Path, extra: &[&str]) -> String {
    Generated::made(&generate(&local.machine, dir, extra), dir).name
}

/// The project: `notes` in the own schema, its first migration
/// `0000_add_notes`, the handler with `/notes`.
fn notes_project(local: &Local) -> TempProject {
    let project = local_project(&local.machine, NAME, &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    let name = generated(local, &project.dir, &["--name", "add_notes"]);
    assert_eq!(name, "0000_add_notes", "the harness's own check");
    project
}

fn notes_of(requested: &Requested) -> serde_json::Value {
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.json()["notes"].clone()
}

#[test]
fn a_write_to_the_own_database_in_one_run_is_there_in_the_next() {
    let local = Local::new();
    let project = notes_project(&local);
    let first = local.request_ok(&project.dir, "/notes", OUTSIDER, &["--data", "first note"]);
    assert_eq!(
        notes_of(&first),
        json!([{ "id": 1, "body": "first note" }]),
        "{}",
        first.describe()
    );
    assert_eq!(
        first.migrations_applied(),
        ["0000_add_notes"],
        "expected the first run to apply the project's migration"
    );
    assert_eq!(first.version(), 1);
    // Its operations: an execute and a query on the own database, as the
    // User.
    let id = first.request_id();
    let operations = first.operations();
    assert_eq!(operations.len(), 2, "{}", first.describe());
    assert_entry(
        &operations[0],
        &Expected {
            rows: Some(1),
            ..Expected::own(&id, OUTSIDER, NAME, "user", "execute")
        },
        "the insert",
    );
    assert_entry(
        &operations[1],
        &Expected::own(&id, OUTSIDER, NAME, "user", "query"),
        "the read",
    );

    let second = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(
        notes_of(&second),
        json!([{ "id": 1, "body": "first note" }]),
        "expected the note written by the earlier run: the local state persists\n{}",
        second.describe()
    );
    assert!(
        second.migrations_applied().is_empty(),
        "expected nothing applied again"
    );
    assert_eq!(second.version(), 1, "expected no new local Version");
}

#[test]
fn a_migration_generated_between_two_runs_is_applied_by_the_second() {
    let local = Local::new();
    let project = notes_project(&local);
    local.request_ok(&project.dir, "/notes", OUTSIDER, &["--data", "before"]);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let name = generated(&local, &project.dir, &["--name", "add_title"]);
    let second = local.request_ok(&project.dir, "/notes", OUTSIDER, &["--data", "after"]);
    assert_eq!(
        second.migrations_applied(),
        [name],
        "expected the migration generated since the last run applied by this one\n{}",
        second.describe()
    );
    assert_eq!(
        notes_of(&second),
        json!([
            { "id": 1, "body": "before", "title": null },
            { "id": 2, "body": "after", "title": null }
        ]),
        "expected the column the migration added, and the earlier row kept"
    );
    assert_eq!(
        second.version(),
        2,
        "expected a new local Version for new migrations"
    );
}

#[test]
fn a_migration_that_destroys_data_is_applied_with_no_question() {
    let local = Local::new();
    let project = notes_project(&local);
    project.write("src/db/schema.ts", NOTES_AND_TAGS_SCHEMA_TS);
    generated(&local, &project.dir, &["--name", "add_tags"]);
    local.request_ok(&project.dir, "/notes", OUTSIDER, &["--data", "kept"]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let dropped = generated(
        &local,
        &project.dir,
        &["--name", "drop_tags", "--allow-data-loss"],
    );
    let run = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(
        run.migrations_applied(),
        [dropped],
        "expected a destroying migration applied without the data-loss question: a local database holds test rows only\n{}",
        run.describe()
    );
}

/// Writes `sql` into a new empty migration `<number>_<name>` made by `vm
/// generate --empty`; its name.
fn hand_written(local: &Local, dir: &Path, name: &str, sql: &str) -> String {
    let made = generated(local, dir, &["--empty", "--name", name]);
    fs::write(dir.join(format!("migrations/{made}.sql")), sql).unwrap();
    made
}

/// A copy of the project's `migrations/`, given back by [`Restore::put_back`].
struct Restore {
    copy: tempfile_dir::Dir,
}

mod tempfile_dir {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A folder of the harness's own under the system's temporary folder,
    /// removed when dropped.
    pub struct Dir(pub PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.0));
        }
    }

    pub fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap().flatten() {
            let path = entry.path();
            let target = to.join(entry.file_name());
            if path.is_dir() {
                copy_tree(&path, &target);
            } else {
                fs::copy(&path, &target).unwrap();
            }
        }
    }
}

impl Restore {
    fn take(dir: &Path) -> Self {
        let copy = std::env::temp_dir().join(format!(
            "vm-blackbox-migrations-{}",
            blackbox::shared_database::token()
        ));
        tempfile_dir::copy_tree(&dir.join("migrations"), &copy);
        Self {
            copy: tempfile_dir::Dir(copy),
        }
    }

    fn put_back(&self, dir: &Path) {
        fs::remove_dir_all(dir.join("migrations")).unwrap();
        tempfile_dir::copy_tree(&self.copy.0, &dir.join("migrations"));
    }
}

#[test]
fn a_failing_migration_ends_the_run_as_at_a_deploy_and_changes_nothing() {
    let local = Local::new();
    let project = notes_project(&local);
    local.request_ok(&project.dir, "/notes", OUTSIDER, &["--data", "kept"]);
    let saved = Restore::take(&project.dir);
    let broken = hand_written(
        &local,
        &project.dir,
        "broken",
        "ALTER TABLE \"notes\" ADD COLUMN \"title\" text;\n--> statement-breakpoint\nINSERT INTO \"missing_table\" VALUES (1);\n",
    );
    let run = local.request(&project.dir, "/notes", OUTSIDER, &[]);
    run.json_error(1, "migration-failed");
    let error = run.json()["error"].clone();
    assert_eq!(
        error["migration"],
        json!(broken),
        "expected the error to name the migration, as at a deploy\n{}",
        run.describe()
    );
    assert_eq!(error["code"], json!("42P01"), "{}", run.describe());
    // Nothing changed: with the migration taken back out, the column it
    // added first is not there, so a migration adding it applies.
    saved.put_back(&project.dir);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let title = generated(&local, &project.dir, &["--name", "add_title"]);
    let after = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(after.migrations_applied(), [title], "{}", after.describe());
    assert_eq!(
        notes_of(&after),
        json!([{ "id": 1, "body": "kept", "title": null }]),
        "expected the note kept"
    );
    assert_eq!(
        after.version(),
        2,
        "expected the failed run to have made no local Version"
    );
}

#[test]
fn a_migration_that_breaks_a_rule_is_refused_as_at_a_deploy() {
    let local = Local::new();
    let project = notes_project(&local);
    local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    let refused = hand_written(&local, &project.dir, "commits", "COMMIT;\n");
    let run = local.request(&project.dir, "/notes", OUTSIDER, &[]);
    run.json_error(1, "migration-refused");
    let error = run.json()["error"].clone();
    assert_eq!(
        error["rule"],
        json!("transaction-control"),
        "{}",
        run.describe()
    );
    assert_eq!(error["migration"], json!(refused), "{}", run.describe());
}

#[test]
fn migrations_that_no_longer_begin_with_what_the_local_database_has_are_diverged() {
    let local = Local::new();
    let project = notes_project(&local);
    local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    // The migration not yet deployed is generated again, otherwise.
    fs::remove_dir_all(project.dir.join("migrations")).unwrap();
    fs::create_dir_all(project.dir.join("migrations")).unwrap();
    fs::write(project.dir.join("migrations/.gitkeep"), "").unwrap();
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS);
    let again = generated(&local, &project.dir, &["--name", "first_again"]);
    let run = local.request(&project.dir, "/notes", OUTSIDER, &[]);
    let message = run.json_error(1, "local-migrations-diverged");
    assert!(
        message.contains("0000_add_notes") || message.contains(&again),
        "expected the message to name the migration, got {message:?}"
    );
    assert!(
        message.contains(".vm/local"),
        "expected the message to say that removing .vm/local/ starts the local databases anew, got {message:?}"
    );
    fs::remove_dir_all(project.dir.join(".vm/local")).unwrap();
    let anew = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(anew.migrations_applied(), [again], "{}", anew.describe());
    assert_eq!(
        notes_of(&anew),
        json!([]),
        "expected the local databases anew"
    );
}

#[test]
fn a_new_local_version_is_made_only_when_the_bundle_the_declared_access_or_the_migrations_differ() {
    let local = Local::new();
    let project = notes_project(&local);
    assert_eq!(
        local.request_ok(&project.dir, "/", OUTSIDER, &[]).version(),
        1
    );
    assert_eq!(
        local.request_ok(&project.dir, "/", OUTSIDER, &[]).version(),
        1,
        "expected the same local Version for the same project"
    );
    // The test Users file is not part of a Version.
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "carol", "groups": [] }));
    write_dev_json(&project.dir, &file);
    assert_eq!(
        local.request_ok(&project.dir, "/", "carol", &[]).version(),
        1
    );
    // The code changed: a new Version.
    let index = project.path("src/index.ts");
    let code = fs::read_to_string(&index).unwrap();
    let changed = code.replace(
        "Response.json({ user: ctx.user().id })",
        "Response.json({ user: ctx.user().id, changed: true })",
    );
    assert_ne!(code, changed, "the harness's own check");
    fs::write(&index, changed).unwrap();
    let after_code = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(after_code.version(), 2, "{}", after_code.describe());
    assert_eq!(
        after_code.json(),
        json!({ "user": OUTSIDER, "changed": true })
    );
    assert_eq!(
        local.request_ok(&project.dir, "/", OUTSIDER, &[]).version(),
        2
    );
    // The Declared access changed: a new Version.
    project.write(
        "manifest.ts",
        &blackbox::local::declaring(NAME, &[("payroll", "read")]),
    );
    assert_eq!(
        local.request_ok(&project.dir, "/", OUTSIDER, &[]).version(),
        3
    );
}
