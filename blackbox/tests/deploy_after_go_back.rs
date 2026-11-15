//! Deploying after going back (CONTRACT.md, "Versions, going back, and
//! logs: Deploying after going back"): `vm deploy` compares the project
//! with the Application's latest Version, not the serving one, since its
//! migrations are the ones applied.
//!
//! Each project has an own table `notes` made by its first migration and
//! two rows put in by a second, hand-written one, as in the tests of
//! migrations at deploy.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::deploy::{
    Deployed, NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS, commit_all, dana, deploy, git_ok,
    init_repository, signed_in,
};
use blackbox::generate::{Generated, NOTES_WITH_TITLE_SCHEMA_TS, generate, project_with};
use blackbox::own_database::OwnDatabase;
use blackbox::project::{NOTES_SCHEMA_TS, manifest_ts};
use blackbox::records::event;
use blackbox::sdk::TempProject;
use blackbox::tool::Machine;
use blackbox::{Vm, VmOptions};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// The rows the second migration puts in.
const SEED: &str = "INSERT INTO \"notes\" (\"body\", \"title\") VALUES ('first', 'kept title'), ('second', NULL);\n";

/// Writes `sql` into a new empty migration named `name`; returns its tag.
fn hand_written(machine: &Machine, project: &TempProject, name: &str, sql: &str) -> String {
    let made = Generated::made(
        &generate(machine, &project.dir, &["--empty", "--name", name]),
        &project.dir,
    );
    fs::write(project.dir.join(&made.file), sql).unwrap();
    made.name
}

/// Changes the own schema to `schema` and generates its migration.
fn generated(machine: &Machine, project: &TempProject, schema: &str, extra: &[&str]) -> Generated {
    project.write("src/db/schema.ts", schema);
    Generated::made(&generate(machine, &project.dir, extra), &project.dir)
}

/// The project named `name`: `notes` and its rows, committed.
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

/// `vm go-back <name> --json`, which must end with `0`.
fn go_back(machine: &Machine, name: &str) -> Value {
    machine.run(&["go-back", name, "--json"]).ok_json()
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
fn after_going_back_a_deploy_applies_only_what_it_adds_to_the_latest_version() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "back-then-deploy", "back-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let tags = generated(&machine, &project, NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS, &[]);
    commit_all(&project.dir, "tags");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let gone = go_back(&machine, "back-board");
    assert_eq!(gone["serving_version"], 1, "{gone}");
    assert_eq!(gone["migrations_not_undone"], json!([tags.name]), "{gone}");

    let more = hand_written(
        &machine,
        &project,
        "more_notes",
        "INSERT INTO \"notes\" (\"body\") VALUES ('third');\n",
    );
    commit_all(&project.dir, "a third note");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.version, 3,
        "expected the next Version numbered after the latest"
    );
    assert_eq!(
        deployed.migrations_applied,
        [more],
        "expected only what the new Version adds to the latest Version's migrations"
    );
    let db = own(&vm, "back-board");
    assert!(db.has_table("tags"));
    assert_eq!(
        db.column("SELECT body FROM notes ORDER BY id"),
        ["first", "second", "third"],
        "expected the rows of the seed once and the new one"
    );
    // No deploy read the list of Versions, only the one Version: every
    // request to the API has a `request` record with its method and path.
    let lists: Vec<Value> = vm
        .records()
        .into_iter()
        .filter(|r| {
            event(r) == "request"
                && r["method"] == "GET"
                && r["path"] == "/v1/applications/back-board/versions"
        })
        .collect();
    assert_eq!(
        lists,
        Vec::<Value>::new(),
        "expected no deploy to read the list of Versions"
    );
    assert!(
        vm.records().iter().any(|r| event(r) == "request"
            && r["method"] == "GET"
            && r["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("/v1/applications/back-board/versions/"))),
        "expected a deploy to read the latest Version by itself (GET .../versions/<number>)"
    );
    assert_eq!(versions_of(&vm, "back-board"), (3, json!(3)));
}

#[test]
fn after_going_back_a_destroying_migration_the_latest_version_has_is_not_asked_about_again() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "back-no-question", "dropped-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let dropping = generated(&machine, &project, NOTES_SCHEMA_TS, &["--allow-data-loss"]);
    commit_all(&project.dir, "drop title");
    Deployed::of(&deploy(&machine, &project.dir, &["--allow-data-loss"]));
    let gone = go_back(&machine, "dropped-board");
    assert_eq!(
        gone["migrations_not_undone"],
        json!([dropping.name]),
        "{gone}"
    );

    project.write("README.md", "A change of no migration.\n");
    commit_all(&project.dir, "readme");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.version, 3);
    assert_eq!(
        deployed.migrations_applied,
        Vec::<String>::new(),
        "expected nothing applied: the latest Version has every migration"
    );
}

#[test]
fn after_going_back_migrations_diverged_is_told_against_the_latest_version() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = notes_project(&machine, "back-diverged", "diverged-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    let first_commit = git_ok(&project.dir, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let tags = generated(&machine, &project, NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS, &[]);
    commit_all(&project.dir, "tags");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    go_back(&machine, "diverged-board");

    // The project as it was when Version 1 was deployed: it is in step with
    // the serving Version and lacks a migration of the latest.
    git_ok(&project.dir, &["reset", "-q", "--hard", &first_commit]);
    let run = deploy(&machine, &project.dir, &[]);
    let message = run.json_error(1, "migrations-diverged");
    assert!(
        message.contains(&tags.name),
        "expected the message to name the latest Version's migration the project lacks ({}), got {message:?}",
        tags.name
    );
    assert_eq!(
        versions_of(&vm, "diverged-board"),
        (2, json!(1)),
        "expected nothing sent"
    );
}
