//! `vibe check` and `vibe deploy` with a Storage Resource (CONTRACT.md,
//! "Storage: Pulling a Storage Resource, checking and deploying", last two
//! bullets; ticket "Storage: a shared, named file Resource, on a mounted
//! directory now and S3 later"): a declared Resource must be registered
//! with the kind the Manifest declares (else `resource-kind-mismatch`,
//! with the two kinds, saying to correct `kind` in `manifest.ts` or to
//! choose another Resource) and be pulled (`storage.json` with its
//! `generated`, else `resource-not-pulled` naming `vibe pull <name>`);
//! `own-grant-below-declared` and `pulled-not-declared` hold for Storage as
//! for databases; a deploy pulls each declared Storage Resource again,
//! writes `storage.json` when it changed, and sends no `structures` entry
//! for it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::User;
use blackbox::deploy::{
    BUILDERS, Deployed, commit_all, dana, deploy, deploy_in_words, init_repository, status,
};
use blackbox::project::{Report, Tree, check, checked_project};
use blackbox::sdk::TempProject;
use blackbox::storage::{FILES_CONTACT, MARKETING, Storage};
use blackbox::storage_tool::{assert_storage_json, stale_storage_json, storage_declaring};
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// A Builder whose Groups hold no Grant.
fn dana_without_a_grant() -> User {
    User::new("dana").in_groups(&["elsewhere"])
}

/// A Builder whose Groups hold no Grant.
fn nora() -> User {
    User::new("nora").in_groups(&["elsewhere"])
}

/// A program with [`MARKETING`] registered and [`BUILDERS`] granted
/// `granted` on it, and a machine signed in to it as dana.
fn ready(granted: &str) -> (Storage, Machine) {
    let s = Storage::start();
    s.register(MARKETING);
    s.grant(MARKETING, BUILDERS, granted);
    let machine = Machine::new();
    machine.sign_in(&s.vm, &dana());
    (s, machine)
}

/// A checked project named `name` declaring these Resources as (name,
/// kind, access level).
fn project(label: &str, name: &str, declared: &[(&str, &str, &str)]) -> TempProject {
    let project = checked_project(label);
    project.write("manifest.ts", &storage_declaring(name, declared));
    project
}

/// `vibe pull <resource> --project <dir> --json`, which must end with `0`;
/// what it printed.
fn pull(machine: &Machine, dir: &Path, resource: &str) -> Value {
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

/// As [`project`], each declared Resource pulled, in a repository of its
/// own, committed.
fn deployable(
    machine: &Machine,
    label: &str,
    name: &str,
    declared: &[(&str, &str, &str)],
) -> TempProject {
    let project = project(label, name, declared);
    for (resource, _, _) in declared {
        pull(machine, &project.dir, resource);
    }
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled");
    project
}

#[test]
fn a_declared_resource_registered_with_the_other_kind_fails_with_resource_kind_mismatch() {
    let (s, machine) = ready("read");
    s.register_postgres("devops");
    for (label, name, declared, resource, declared_kind, registered_kind, pulled) in [
        // The Manifest declares it `storage`; it is registered `postgres`.
        (
            "check-mismatch-storage",
            "mismatch-storage-board",
            [("devops", "storage", "read")].as_slice(),
            "devops",
            "storage",
            "postgres",
            false,
        ),
        // The other way: declared `postgres`, registered `storage`.
        (
            "check-mismatch-postgres",
            "mismatch-postgres-board",
            [(MARKETING, "postgres", "read")].as_slice(),
            MARKETING,
            "postgres",
            "storage",
            false,
        ),
        // The same, with the Storage Resource pulled: still the one
        // problem said of that Resource, "whatever is or is not pulled".
        (
            "check-mismatch-pulled",
            "mismatch-pulled-board",
            [(MARKETING, "postgres", "read")].as_slice(),
            MARKETING,
            "postgres",
            "storage",
            true,
        ),
    ] {
        let project = project(label, name, declared);
        if pulled {
            pull(&machine, &project.dir, resource);
        }
        let report = Report::failed(&check(&machine, &project.dir));
        let problem = report.problem("resources", "resource-kind-mismatch");
        assert_eq!(
            report.of_check("resources").len(),
            1,
            "{name}: expected the mismatch to be the one problem said of that Resource, whatever is or is not pulled\n{}",
            report.run.describe()
        );
        assert_eq!(problem["resource"], json!(resource), "{problem}");
        assert_eq!(problem["declared_kind"], json!(declared_kind), "{problem}");
        assert_eq!(
            problem["registered_kind"],
            json!(registered_kind),
            "{problem}"
        );
        let message = problem["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("manifest.ts") && message.contains("kind"),
            "{name}: expected the message to say to correct `kind` in manifest.ts, got {message:?}"
        );
        let lower = message.to_lowercase();
        assert!(
            lower.contains("correct") && lower.contains("another"),
            "{name}: expected the message to say both ways out: to correct `kind`, and to choose another Resource, got {message:?}"
        );
    }

    // A name nothing of is registered is the usual problem, as for any
    // Resource.
    let ghost = project(
        "check-ghost",
        "ghost-board",
        &[("ghost", "storage", "read")],
    );
    let report = Report::failed(&check(&machine, &ghost.dir));
    report.problem("resources", "resource-not-registered");
}

#[test]
fn a_declared_storage_resource_that_is_not_pulled_fails_and_gives_the_command() {
    let (_s, machine) = ready("read");
    for (case, hand_written) in [
        ("never pulled", false),
        ("a hand-written storage.json", true),
    ] {
        let project = project(
            &format!("check-not-pulled-{}", case.replace([' ', '-'], "-")),
            "not-pulled-board",
            &[(MARKETING, "storage", "read")],
        );
        if hand_written {
            fs::create_dir_all(project.dir.join(format!("resources/{MARKETING}"))).unwrap();
            project.write(
                &format!("resources/{MARKETING}/storage.json"),
                "{\n  \"generated\": \"mine\",\n}\n",
            );
        }
        let report = Report::failed(&check(&machine, &project.dir));
        let problem = report.problem("resources", "resource-not-pulled");
        let message = problem["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&format!("vibe pull {MARKETING}")),
            "{case}: expected the message to name the command, got {message:?}"
        );
        if hand_written {
            assert!(
                message.contains(&format!("resources/{MARKETING}/storage.json"))
                    && message.to_lowercase().contains("move"),
                "{case}: expected the message to name the file the tool did not write and say it is to be moved away first, got {message:?}"
            );
        }
    }
}

#[test]
fn a_pulled_storage_resource_passes_the_check_with_nothing_to_say() {
    let (_s, machine) = ready("read");
    let project = project(
        "check-pulled",
        "pulled-board",
        &[(MARKETING, "storage", "read")],
    );
    pull(&machine, &project.dir, MARKETING);
    Report::clean(&check(&machine, &project.dir));
}

#[test]
fn a_builder_whose_grant_is_below_the_declared_level_is_warned_about() {
    // `read` against `write`.
    let (_s, machine) = ready("read");
    let below = project(
        "check-below",
        "below-board",
        &[(MARKETING, "storage", "write")],
    );
    pull(&machine, &below.dir, MARKETING);
    let report = Report::passed(&check(&machine, &below.dir));
    let warnings = report.warnings_for("own-grant-below-declared");
    assert_eq!(warnings.len(), 1, "{}", report.run.describe());
    let warning = &warnings[0];
    assert_eq!(warning["resource"], json!(MARKETING), "{warning}");
    assert_eq!(warning["declared"], json!("write"), "{warning}");
    assert_eq!(warning["held"], json!("read"), "{warning}");
    assert_eq!(warning["contact"], json!(FILES_CONTACT), "{warning}");
    let message = warning["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(FILES_CONTACT),
        "expected the warning to name whom to ask, got {message:?}"
    );
    let lower = message.to_lowercase();
    assert!(
        lower.contains("refus"),
        "expected the warning to say that Users without the Grant will be refused, got {message:?}"
    );

    // No Grant at all against `read`.
    let (s, machine) = ready("read");
    let project = project(
        "check-no-grant",
        "no-grant-board",
        &[(MARKETING, "storage", "read")],
    );
    pull(&machine, &project.dir, MARKETING);
    let other = Machine::new();
    other.sign_in(&s.vm, &nora());
    let report = Report::passed(&check(&other, &project.dir));
    let warning = &report.warnings_for("own-grant-below-declared")[0];
    assert_eq!(warning["declared"], json!("read"), "{warning}");
    assert_eq!(warning["held"], Value::Null, "{warning}");
    assert_eq!(warning["contact"], json!(FILES_CONTACT), "{warning}");
}

#[test]
fn a_pulled_storage_resource_the_manifest_does_not_declare_is_a_note() {
    let (_s, machine) = ready("read");
    let project = checked_project("check-leftover");
    pull(&machine, &project.dir, MARKETING);
    let report = Report::passed(&check(&machine, &project.dir));
    let notes = report.notes_for("pulled-not-declared");
    assert_eq!(notes.len(), 1, "{}", report.run.describe());
    let note = &notes[0];
    assert_eq!(note["resource"], json!(MARKETING), "{note}");
    let message = note["message"].as_str().unwrap_or_default();
    let lower = message.to_lowercase();
    assert!(
        lower.contains("declare") && lower.contains("remove"),
        "expected the message to say to declare it or remove the folder, got {message:?}"
    );
}

#[test]
fn a_deploy_pulls_each_declared_storage_resource_again_and_sends_no_structures_entry() {
    let (s, machine) = ready("write");
    let project = deployable(
        &machine,
        "deploy-unchanged",
        "unchanged-files-board",
        &[(MARKETING, "storage", "write")],
    );
    let before = Tree::of(&project.dir);
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": MARKETING, "changed": false, "files": [] })],
        "expected the declared Storage Resource pulled again, unchanged\n{}",
        deployed.run.describe()
    );
    assert_eq!(
        deployed.structures,
        json!({}),
        "expected no `structures` entry for a Storage Resource\n{}",
        deployed.run.describe()
    );
    assert!(deployed.warnings.is_empty(), "{}", deployed.run.describe());
    assert!(deployed.notes.is_empty(), "{}", deployed.run.describe());
    let api = s.vm.api_as(&dana());
    let version = api.get_version("unchanged-files-board", 1);
    version.expect_status(200);
    assert_eq!(
        version.json()["declared_access"],
        json!({ "resources": { MARKETING: { "kind": "storage", "access": "write" } } }),
        "expected the Version's Declared access as sent"
    );
    let page = s.vm.get_as(&dana(), "unchanged-files-board", "/");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new(),
        "expected nothing written in the project by the deploy"
    );
    assert_eq!(
        status(&project.dir),
        "",
        "expected everything still committed: the deploy wrote nothing"
    );
}

#[test]
fn a_deploy_writes_storage_json_when_it_changed() {
    let (_s, machine) = ready("write");
    let project = deployable(
        &machine,
        "deploy-changed",
        "changed-files-board",
        &[(MARKETING, "storage", "write")],
    );
    // A `storage.json` of the past: generated, but another contact.
    let stale = project.path(&format!("resources/{MARKETING}/storage.json"));
    fs::write(
        &stale,
        stale_storage_json(MARKETING, "The contact of the past"),
    )
    .unwrap();
    commit_all(&project.dir, "a contact of the past");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![json!({
            "resource": MARKETING,
            "changed": true,
            "files": [format!("resources/{MARKETING}/storage.json")],
        })],
        "expected the deploy's pull to write the changed file\n{}",
        deployed.run.describe()
    );
    assert_storage_json(&stale, MARKETING, FILES_CONTACT, "after the deploy's pull");
    // "with the note `structure-updated`, its `resource` and its `files`,
    // as for a database".
    let notes = deployed.notes_for("structure-updated");
    assert_eq!(notes.len(), 1, "{}", deployed.run.describe());
    assert_eq!(notes[0]["resource"], json!(MARKETING), "{}", notes[0]);
    assert_eq!(
        notes[0]["files"],
        json!([format!("resources/{MARKETING}/storage.json")]),
        "expected the note to name the file to commit"
    );
    assert!(
        notes[0]["message"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("commit"),
        "expected the note to say to commit the file, got {}",
        notes[0]
    );
    // In words: the file named and to be committed.
    commit_all(&project.dir, "the contact of today");
    fs::write(
        &stale,
        stale_storage_json(MARKETING, "The contact of the past"),
    )
    .unwrap();
    commit_all(&project.dir, "a contact of the past, again");
    let text = deploy_in_words(&machine, &project.dir, &[]).ok_text();
    assert!(
        text.contains(&format!("resources/{MARKETING}/storage.json"))
            && text.to_lowercase().contains("commit"),
        "expected the words to name the file and say to commit it\n{text}"
    );
    let left = status(&project.dir);
    assert!(
        left.contains(&format!("resources/{MARKETING}/storage.json")),
        "expected the updated file left for the Contributor to commit\n{left}"
    );
}

#[test]
fn a_deploy_of_a_declared_storage_resource_that_is_not_pulled_fails_the_checks() {
    let (s, machine) = ready("write");
    let project = project(
        "deploy-not-pulled",
        "not-pulled-files-board",
        &[(MARKETING, "storage", "write")],
    );
    init_repository(&project.dir);
    commit_all(&project.dir, "never pulled");
    let run = deploy(&machine, &project.dir, &[]);
    let report = Report::failed(&run);
    let message = report.problem("resources", "resource-not-pulled")["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains(&format!("vibe pull {MARKETING}")),
        "expected the failed deploy's report to name the command, got {message:?}"
    );
    // The checks end the deploy before anything is built or sent: no
    // Application of the name exists.
    let answer =
        s.vm.api_as(&dana())
            .get_application("not-pulled-files-board");
    answer.expect_status(404);
    assert!(
        !project
            .dir
            .join(format!("resources/{MARKETING}/storage.json"))
            .exists(),
        "expected nothing pulled into the project by the failed deploy"
    );
}

#[test]
fn a_deploy_does_not_write_a_storage_json_that_is_not_there() {
    // CONTRACT.md: "A `storage.json` that is not there is not written by a
    // deploy: it is the checks' `resource-not-pulled`."
    let (s, machine) = ready("write");
    let project = deployable(
        &machine,
        "deploy-missing",
        "missing-files-board",
        &[(MARKETING, "storage", "write")],
    );
    let file = project.path(&format!("resources/{MARKETING}/storage.json"));
    fs::remove_file(&file).unwrap();
    commit_all(&project.dir, "the file removed");
    let run = deploy(&machine, &project.dir, &[]);
    let report = Report::failed(&run);
    let message = report.problem("resources", "resource-not-pulled")["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains(&format!("vibe pull {MARKETING}")),
        "expected the report to name the command, got {message:?}"
    );
    assert!(
        !file.exists(),
        "expected the deploy not to write the file that was not there"
    );
    s.vm.api_as(&dana())
        .get_application("missing-files-board")
        .expect_status(404);
}

#[test]
fn a_builder_without_a_grant_deploys_with_the_storage_json_of_the_project_and_a_warning() {
    // CONTRACT.md: "For a declared Storage Resource the Builder holds no
    // Grant on, the deploy goes on with the `storage.json` of the project
    // and warns as it does for a database (it could not be pulled
    // again)."
    let (s, machine) = ready("write");
    let project = deployable(
        &machine,
        "deploy-no-grant",
        "no-grant-files-board",
        &[(MARKETING, "storage", "read")],
    );
    let before = Tree::of(&project.dir);
    let other = Machine::new();
    other.sign_in(&s.vm, &dana_without_a_grant());
    let deployed = Deployed::of(&deploy(&other, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": MARKETING, "changed": false, "files": [] })],
        "expected the Resource said pulled, unchanged\n{}",
        deployed.run.describe()
    );
    assert_eq!(
        deployed.structures,
        json!({}),
        "{}",
        deployed.run.describe()
    );
    let warnings = deployed.warnings_for("own-grant-below-declared");
    assert_eq!(warnings.len(), 1, "{}", deployed.run.describe());
    assert_eq!(warnings[0]["resource"], json!(MARKETING), "{}", warnings[0]);
    assert_eq!(warnings[0]["declared"], json!("read"), "{}", warnings[0]);
    assert_eq!(warnings[0]["held"], Value::Null, "{}", warnings[0]);
    assert_eq!(
        warnings[0]["contact"],
        json!(FILES_CONTACT),
        "{}",
        warnings[0]
    );
    // Beside `own-grant-below-declared` (or in it): the warning that the
    // Resource could not be pulled again.
    let again: Vec<String> = deployed
        .warnings_for("storage-not-pulled")
        .iter()
        .filter_map(|w| w["message"].as_str())
        .map(str::to_lowercase)
        .collect();
    assert_eq!(
        again.len(),
        1,
        "expected one warning of its own, reason `storage-not-pulled`, that the Resource could not be pulled again, got {:?}",
        deployed.warnings
    );
    let lower = &again[0];
    assert!(
        lower.contains("storage.json") && lower.contains(MARKETING),
        "expected the warning to name the Resource and storage.json, got {lower:?}"
    );
    assert!(
        !lower.contains("schema.ts") && !lower.contains("structure"),
        "expected nothing of a structure or of schema.ts in the warning, got {lower:?}"
    );
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new(),
        "expected nothing written by a deploy that pulled nothing"
    );
    s.vm.api_as(&dana())
        .get_version("no-grant-files-board", 1)
        .expect_status(200);
}

#[test]
fn only_a_storage_json_with_its_generated_counts_as_pulled_for_the_note_too() {
    // CONTRACT.md: "only a `storage.json` with its `generated` counts as
    // pulled, for both".
    let (_s, machine) = ready("read");
    let project = checked_project("check-leftover-by-hand");
    fs::create_dir_all(project.dir.join(format!("resources/{MARKETING}"))).unwrap();
    project.write(
        &format!("resources/{MARKETING}/storage.json"),
        "{\n  \"generated\": \"mine\"\n}\n",
    );
    let report = Report::passed(&check(&machine, &project.dir));
    assert!(
        report.notes_for("pulled-not-declared").is_empty(),
        "expected no `pulled-not-declared` for a file the tool did not write\n{}",
        report.run.describe()
    );
}

#[test]
fn a_storage_json_too_large_to_be_the_tools_ends_a_deploy_and_is_left_as_it_was() {
    // CONTRACT.md: "a file too large to be one the tool wrote is
    // `not-generated` too, and is left as it is", for the pull and the
    // deploy; the file here begins like the tool's and goes on.
    let (_s, machine) = ready("write");
    let project = deployable(
        &machine,
        "deploy-large",
        "large-files-board",
        &[(MARKETING, "storage", "write")],
    );
    let file = project.path(&format!("resources/{MARKETING}/storage.json"));
    let mut content = fs::read(&file).unwrap();
    content.extend(std::iter::repeat_n(b' ', 17 * 1024 * 1024));
    fs::write(&file, &content).unwrap();
    commit_all(&project.dir, "a file grown too large");
    let run = deploy(&machine, &project.dir, &[]);
    run.expect_code(1);
    let body = run.json();
    let reason = body["error"]["reason"].as_str().unwrap_or_default();
    // The checks come first and say a `storage.json` that is not the
    // tool's is not pulled; a deploy that reaches the pull says
    // `not-generated`. Either way the file is named and left alone.
    assert!(
        reason == "not-generated" || reason == "check-failed",
        "expected the deploy to end `not-generated` (or the checks' `check-failed`), got\n{}",
        run.describe()
    );
    if reason == "check-failed" {
        Report::failed(&run).problem("resources", "resource-not-pulled");
    }
    assert!(
        body.to_string()
            .contains(&format!("resources/{MARKETING}/storage.json")),
        "expected the file named\n{}",
        run.describe()
    );
    assert!(
        fs::read(&file).unwrap() == content,
        "expected the file byte for byte as it was"
    );
}
