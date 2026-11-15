//! Each declared Resource pulled again at deploy (CONTRACT.md, "Deploying:
//! The command", steps 5 and 6, "What it prints"; ticket "Deploy: a Version
//! built on the Contributor's machine goes live": when a shared database's
//! structure has changed, deploy fails only if the code no longer compiles
//! against it, otherwise it updates the pulled definition and tells the
//! Contributor to commit it; the Version records the fingerprints; when the
//! Contributor's own Grant is below a declared level, deploy warns as the
//! check does and goes on).
//!
//! The shared database is the harness's usual one (`incidents`, ...),
//! registered as `devops`; the project's code reads `incidents.title`
//! through the pulled definition, so that it type-checks only while that
//! column is there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::User;
use blackbox::deploy::{
    BUILDERS, Deployed, READS_INCIDENTS_TS, assert_project_unchanged_but, commit_all, dana, deploy,
    deploy_in_words, fingerprint_in, init_repository, project_tree, signed_in, status,
};
use blackbox::project::{
    INDEX_TS, Report, check, checked_project, manifest_ts, pulled_by_hand, squashed,
};
use blackbox::scenario::{CONTACT, Scenario, resource_body};
use blackbox::sdk::TempProject;
use blackbox::structure::pull;
use blackbox::tool::{Machine, closed_address};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&["readers"])
}

/// A program with `devops` registered over the shared database and
/// [`BUILDERS`] granted `read` on it, and a machine signed in as dana.
fn ready() -> (Scenario, Machine) {
    let scenario = Scenario::start();
    scenario.register_writable("devops");
    scenario.grant("devops", BUILDERS, "read");
    let machine = signed_in(&scenario.vm);
    (scenario, machine)
}

/// `vm pull <resource> --project <dir> --json` on `machine`, which must end
/// with `0`.
fn pull_into(machine: &Machine, dir: &Path, resource: &str) -> Value {
    machine
        .run(&[
            "pull",
            resource,
            "--project",
            dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json()
}

/// The Manifest of `name` declaring these Resources at these levels, in
/// this order.
fn declaring(name: &str, declared: &[(&str, &str)]) -> String {
    let entries: Vec<String> = declared
        .iter()
        .map(|(r, a)| format!("{r}: {{ kind: \"postgres\", access: \"{a}\" }}"))
        .collect();
    manifest_ts(name, &format!("{{ {} }}", entries.join(", ")))
}

/// A project named `name` declaring these Resources, whose code reads
/// `incidents.title` of `devops`, pulled by `puller` (each declared
/// Resource), in a repository of its own, committed.
fn reading_devops(
    puller: &Machine,
    label: &str,
    name: &str,
    declared: &[(&str, &str)],
) -> TempProject {
    let project = checked_project(label);
    project.write("manifest.ts", &declaring(name, declared));
    project.write("src/index.ts", READS_INCIDENTS_TS);
    for (resource, _) in declared {
        pull_into(puller, &project.dir, resource);
    }
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled");
    project
}

/// The fingerprint the API answers for `resource` now, asked as `user`.
fn fingerprint_now(scenario: &Scenario, user: &User, resource: &str) -> String {
    let answer = pull(&scenario.vm.api_as(user), resource);
    answer.expect_status(200);
    answer.json()["fingerprint"].as_str().unwrap().to_owned()
}

fn schema_of(dir: &Path, resource: &str) -> String {
    fs::read_to_string(dir.join(format!("resources/{resource}/schema.ts"))).unwrap()
}

#[test]
fn an_unchanged_structure_is_pulled_again_and_nothing_is_written() {
    let (scenario, machine) = ready();
    scenario.register_writable("inventory");
    scenario.grant("inventory", BUILDERS, "read");
    let project = reading_devops(
        &machine,
        "deploy-unchanged",
        "unchanged-board",
        &[("inventory", "read"), ("devops", "read")],
    );
    let before = project_tree(&project.dir);
    let fingerprint = fingerprint_now(&scenario, &dana(), "devops");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![
            json!({ "resource": "devops", "changed": false, "files": [] }),
            json!({ "resource": "inventory", "changed": false, "files": [] }),
        ],
        "expected every declared Resource in `pulled`, by name, unchanged\n{}",
        deployed.run.describe()
    );
    let structures = json!({ "devops": fingerprint, "inventory": fingerprint_now(&scenario, &dana(), "inventory") });
    assert_eq!(deployed.structures, structures);
    assert!(deployed.notes_for("structure-updated").is_empty());
    assert!(deployed.warnings.is_empty(), "{}", deployed.run.describe());
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new(),
        "expected nothing written"
    );
    let version = scenario
        .vm
        .api_as(&dana())
        .get_version("unchanged-board", 1)
        .expect_status(200)
        .json();
    assert_eq!(
        version["structures"], structures,
        "expected the Version to record the fingerprints"
    );
    assert_eq!(
        version["declared_access"],
        json!({ "resources": {
            "devops": { "kind": "postgres", "access": "read" },
            "inventory": { "kind": "postgres", "access": "read" },
        } })
    );
    // The Application reads the shared database as dana.
    let page = scenario.vm.get_as(&dana(), "unchanged-board", "/");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert!(
        page.json()["titles"]
            .as_array()
            .is_some_and(|t| t.contains(&json!("Disk full on db-7"))),
        "{}",
        page.describe()
    );
}

#[test]
fn a_structure_changed_so_the_code_still_type_checks_is_written_and_built_against() {
    let (scenario, machine) = ready();
    let project = reading_devops(
        &machine,
        "deploy-changed",
        "changed-board",
        &[("devops", "read")],
    );
    let first = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let old = fingerprint_now(&scenario, &dana(), "devops");
    assert_eq!(first.structures, json!({ "devops": old }));

    scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents ADD COLUMN region text");
    let new = fingerprint_now(&scenario, &dana(), "devops");
    assert_ne!(new, old, "the harness's own check");
    let before = project_tree(&project.dir);
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.version, 2);
    assert_eq!(
        deployed.pulled,
        vec![
            json!({ "resource": "devops", "changed": true, "files": ["resources/devops/schema.ts"] })
        ],
        "{}",
        deployed.run.describe()
    );
    assert_eq!(
        deployed.structures,
        json!({ "devops": new }),
        "expected the Version built against the new structure"
    );
    let schema = schema_of(&project.dir, "devops");
    assert!(
        schema.contains("\"region\""),
        "expected the new column written"
    );
    assert_eq!(fingerprint_in(&schema), Some(new.clone()));
    let note = deployed.notes_for("structure-updated");
    assert_eq!(note.len(), 1, "{}", deployed.run.describe());
    assert_eq!(note[0]["resource"], json!("devops"));
    assert_eq!(note[0]["files"], json!(["resources/devops/schema.ts"]));
    let message = note[0]["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("commit"),
        "expected the note to say to commit the files, got {message:?}"
    );
    assert_eq!(
        scenario
            .vm
            .api_as(&dana())
            .get_version("changed-board", 2)
            .expect_status(200)
            .json()["structures"],
        json!({ "devops": new })
    );
    assert_project_unchanged_but(
        &before,
        &project.dir,
        &["resources/devops"],
        "a changed structure",
    );
    assert!(
        status(&project.dir).contains("resources/devops/schema.ts"),
        "expected the updated definition left for the Contributor to commit"
    );

    // In words: the definition updated, the Version built against it, to
    // commit the files.
    commit_all(&project.dir, "the new structure");
    scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents ADD COLUMN zone text");
    let text = deploy_in_words(&machine, &project.dir, &[]).ok_text();
    let flat = squashed(&text).to_lowercase();
    assert!(
        text.contains("resources/devops/schema.ts") && flat.contains("commit"),
        "expected the words to name the file and say to commit it\n{text}"
    );
    assert!(
        flat.contains("structure") && flat.contains("devops"),
        "expected the words to say devops's structure changed and the Version was built against it\n{text}"
    );
}

#[test]
fn a_structure_the_code_no_longer_type_checks_against_fails_the_deploy_and_stays_written() {
    let (scenario, machine) = ready();
    let project = reading_devops(
        &machine,
        "deploy-incompatible",
        "incompatible-board",
        &[("devops", "read")],
    );
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents DROP COLUMN title");
    let new = fingerprint_now(&scenario, &dana(), "devops");
    let before = project_tree(&project.dir);
    let versions_before = scenario.vm.api_as(&dana()).versions("incompatible-board");
    let run = deploy(&machine, &project.dir, &[]);
    let report = Report::failed(&run);
    let errors = report.problems_for("type-error");
    assert!(
        !errors.is_empty() && errors.iter().all(|p| p["check"] == "types"),
        "expected the type error the dropped column makes\n{}",
        run.describe()
    );
    assert!(
        errors.iter().any(|p| p["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("src/index.ts"))),
        "expected the type error in src/index.ts\n{}",
        run.describe()
    );
    let message = report.error_message();
    assert!(
        message.contains("devops"),
        "expected the message of the failed deploy to say which Resource's structure changed, got {message:?}"
    );
    // No Version; the files stay updated.
    let api = scenario.vm.api_as(&dana());
    assert_eq!(
        api.versions("incompatible-board"),
        versions_before,
        "expected the list of Versions unchanged"
    );
    assert_eq!(
        api.application("incompatible-board")["serving_version"],
        json!(1)
    );
    let schema = schema_of(&project.dir, "devops");
    assert!(
        !schema.contains("\"title\""),
        "expected the definition updated"
    );
    assert_eq!(fingerprint_in(&schema), Some(new));
    assert_project_unchanged_but(
        &before,
        &project.dir,
        &["resources/devops"],
        "a failed deploy",
    );
}

/// Fails unless the deploy's warning `own-grant-below-declared` for
/// `resource` has the fields the check's has for the same project.
fn assert_warns_as_the_check(deployed: &Deployed, machine: &Machine, dir: &Path, resource: &str) {
    let warnings = deployed.warnings_for("own-grant-below-declared");
    assert_eq!(warnings.len(), 1, "{}", deployed.run.describe());
    let ours = &warnings[0];
    let report = Report::passed(&check(machine, dir));
    let theirs = report.warnings_for("own-grant-below-declared");
    assert_eq!(theirs.len(), 1, "the harness's own check: the check warns");
    for field in ["resource", "declared", "held", "contact"] {
        assert_eq!(
            ours[field],
            theirs[0][field],
            "expected the deploy's warning to have the check's `{field}`\n{}",
            deployed.run.describe()
        );
    }
    assert_eq!(ours["resource"], json!(resource));
    assert_eq!(ours["contact"], json!(CONTACT));
}

#[test]
fn a_resource_the_user_holds_no_grant_on_is_used_as_pulled_with_a_warning() {
    let scenario = Scenario::start();
    scenario.register_writable("devops");
    scenario.grant("devops", "readers", "read");
    let machine = signed_in(&scenario.vm);
    let ritas = Machine::new();
    ritas.sign_in(&scenario.vm, &rita());
    // rita, who holds a Grant, pulled it; dana deploys.
    let project = reading_devops(
        &ritas,
        "deploy-no-grant",
        "no-grant-board",
        &[("devops", "read")],
    );
    let pulled = fingerprint_in(&schema_of(&project.dir, "devops")).unwrap();
    // The structure changes meanwhile: the definition in the project is
    // used as it is.
    scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents ADD COLUMN region text");
    let before = project_tree(&project.dir);
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.structures,
        json!({ "devops": pulled }),
        "expected the fingerprint the definition's second line gives"
    );
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": "devops", "changed": false, "files": [] })]
    );
    let warning = &deployed.warnings_for("own-grant-below-declared")[0];
    assert_eq!(warning["declared"], json!("read"));
    assert_eq!(warning["held"], Value::Null);
    let message = warning["message"].as_str().unwrap();
    assert!(
        message.contains(CONTACT),
        "expected whom to ask, got {message:?}"
    );
    let lower = message.to_lowercase();
    assert!(
        lower.contains("pull")
            && (lower.contains("could not")
                || lower.contains("cannot")
                || lower.contains("not be")),
        "expected the warning to say the structure could not be pulled again, got {message:?}"
    );
    assert!(
        lower.contains("definition") || lower.contains("schema.ts"),
        "expected the warning to say the definition in the project was used, got {message:?}"
    );
    assert_warns_as_the_check(&deployed, &machine, &project.dir, "devops");
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
    assert_eq!(
        scenario
            .vm
            .api_as(&dana())
            .get_version("no-grant-board", 1)
            .expect_status(200)
            .json()["structures"],
        json!({ "devops": pulled })
    );
}

#[test]
fn a_grant_below_the_declared_level_warns_as_the_check_does_and_goes_on() {
    // `structure` against `read`.
    let scenario = Scenario::start();
    scenario.register_writable("devops");
    scenario.grant("devops", BUILDERS, "structure");
    let machine = signed_in(&scenario.vm);
    let project = reading_devops(
        &machine,
        "deploy-structure-only",
        "viewing-board",
        &[("devops", "read")],
    );
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let warning = &deployed.warnings_for("own-grant-below-declared")[0];
    assert_eq!(warning["declared"], json!("read"));
    assert_eq!(warning["held"], json!("structure"));
    assert_warns_as_the_check(&deployed, &machine, &project.dir, "devops");
    assert_eq!(
        deployed.structures,
        json!({ "devops": fingerprint_now(&scenario, &dana(), "devops") })
    );
    assert_eq!(deployed.version, 1);

    // `read` against `write`.
    let (scenario, machine) = ready();
    let project = reading_devops(
        &machine,
        "deploy-read-only",
        "writing-board",
        &[("devops", "write")],
    );
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let warning = &deployed.warnings_for("own-grant-below-declared")[0];
    assert_eq!(warning["declared"], json!("write"));
    assert_eq!(warning["held"], json!("read"));
    assert_warns_as_the_check(&deployed, &machine, &project.dir, "devops");
    assert_eq!(deployed.version, 1);
    assert_eq!(
        scenario.vm.api_as(&dana()).application("writing-board")["serving_version"],
        json!(1)
    );
}

#[test]
fn a_shared_database_that_does_not_answer_is_used_as_pulled_with_a_warning() {
    let scenario = Scenario::start();
    let mut body = resource_body("offline", &scenario.db, &scenario.db.reader);
    body["connection"]["port"] = json!(closed_address().port());
    scenario.register_body("offline", &body);
    scenario.grant("offline", BUILDERS, "read");
    let machine = signed_in(&scenario.vm);
    let project = checked_project("deploy-offline");
    project.write(
        "manifest.ts",
        &declaring("offline-board", &[("offline", "read")]),
    );
    project.write("src/index.ts", INDEX_TS);
    pulled_by_hand(&project, "offline");
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled by hand");
    let in_line = fingerprint_in(&schema_of(&project.dir, "offline")).unwrap();
    let before = project_tree(&project.dir);
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let warnings = deployed.warnings_for("structure-not-pulled");
    assert_eq!(warnings.len(), 1, "{}", deployed.run.describe());
    assert_eq!(warnings[0]["resource"], json!("offline"));
    let lower = warnings[0]["message"].as_str().unwrap().to_lowercase();
    assert!(
        lower.contains("pull") && (lower.contains("definition") || lower.contains("schema.ts")),
        "expected the warning to say the structure could not be pulled again and the definition was used, got {lower:?}"
    );
    assert_eq!(deployed.structures, json!({ "offline": in_line }));
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": "offline", "changed": false, "files": [] })]
    );
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn a_pulled_resource_the_manifest_does_not_declare_is_a_note_and_the_deploy_goes_on() {
    let scenario = Scenario::start();
    let machine = signed_in(&scenario.vm);
    let project = checked_project("deploy-leftover");
    project.write("manifest.ts", &manifest_ts("leftover-board", "{}"));
    pulled_by_hand(&project, "leftover");
    init_repository(&project.dir);
    commit_all(&project.dir, "first");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let notes = deployed.notes_for("pulled-not-declared");
    assert_eq!(notes.len(), 1, "{}", deployed.run.describe());
    assert_eq!(notes[0]["resource"], json!("leftover"));
    assert_eq!(deployed.pulled, Vec::<Value>::new());
    assert_eq!(deployed.structures, json!({}));
}
