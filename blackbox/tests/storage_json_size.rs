//! One size for `resources/<name>/storage.json` (CONTRACT.md, "Storage:
//! Pulling a Storage Resource, checking and deploying", first bullets: "a
//! file larger than 64 KiB is not one the tool wrote, whatever it begins
//! with, for every command alike (`not-generated` to a pull, not pulled to
//! `vibe check`, `vibe deploy` and the local run), and is left as it is"):
//! a file of 100 KiB that is the tool's own padded is not the tool's for
//! the pull, the check and the deploy; one just under 64 KiB with a long
//! contact is, for all of them. (The local run's two cases are in
//! `storage_local.rs`.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::deploy::{BUILDERS, Deployed, commit_all, dana, deploy, init_repository};
use blackbox::project::{Report, check, checked_project};
use blackbox::storage::{MARKETING, Storage, storage_body};
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// The largest contact this test gives a Resource: with the other three
/// members the file stays just under 64 KiB (65,536 bytes).
const LONG_CONTACT: usize = 65_000;

fn start(contact: Option<&str>) -> (Storage, Machine) {
    let s = Storage::start();
    match contact {
        None => {
            s.register(MARKETING);
        }
        Some(contact) => {
            let dir = s.dir(MARKETING);
            fs::create_dir_all(&dir).unwrap();
            let mut body = storage_body(MARKETING, &dir.to_string_lossy());
            body["contact"] = json!(contact);
            let answer = s.admin().create_resource(&body);
            assert_eq!(
                answer.status,
                201,
                "the harness's own check: a contact of {} bytes is taken\n{}",
                contact.len(),
                answer.describe()
            );
        }
    }
    s.grant(MARKETING, BUILDERS, "write");
    let machine = Machine::new();
    machine.sign_in(&s.vm, &dana());
    (s, machine)
}

fn pull(machine: &Machine, dir: &Path) -> blackbox::tool::Run {
    machine.run(&[
        "pull",
        MARKETING,
        "--project",
        dir.to_str().unwrap(),
        "--json",
    ])
}

fn declare(project: &blackbox::sdk::TempProject, name: &str) {
    project.write(
        "manifest.ts",
        &blackbox::storage_tool::storage_declaring(name, &[(MARKETING, "storage", "write")]),
    );
}

#[test]
fn a_padded_storage_json_over_64_kib_is_not_the_tools_for_the_pull_the_check_and_the_deploy() {
    let (_s, machine) = start(None);
    let project = checked_project("size-padded");
    declare(&project, "padded-board");
    pull(&machine, &project.dir).ok_json();
    let file = project.path(&format!("resources/{MARKETING}/storage.json"));
    // The tool's own file, valid JSON with the right members, and a long
    // extra one.
    let mut body: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    body["padding"] = json!("x".repeat(100 * 1024));
    let padded = format!("{}\n", serde_json::to_string_pretty(&body).unwrap());
    fs::write(&file, &padded).unwrap();
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled, then padded");

    let message = pull(&machine, &project.dir).json_error(1, "not-generated");
    assert!(
        message.contains(&format!("resources/{MARKETING}/storage.json")),
        "the pull: expected the message to name the file, got {message:?}"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        padded,
        "the pull: expected the file byte for byte as it was"
    );

    let report = Report::failed(&check(&machine, &project.dir));
    report.problem("resources", "resource-not-pulled");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        padded,
        "the check: expected the file as it was"
    );

    let run = deploy(&machine, &project.dir, &[]);
    Report::failed(&run).problem("resources", "resource-not-pulled");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        padded,
        "the deploy: expected the file as it was"
    );
}

#[test]
fn a_storage_json_just_under_64_kib_with_a_long_contact_is_the_tools_for_all_three() {
    let contact = "c".repeat(LONG_CONTACT);
    let (_s, machine) = start(Some(&contact));
    let project = checked_project("size-long");
    declare(&project, "long-contact-board");
    let first = pull(&machine, &project.dir).ok_json();
    assert_eq!(
        first["files"],
        json!([{ "path": format!("resources/{MARKETING}/storage.json"), "changed": true }]),
        "{first}"
    );
    let file = project.path(&format!("resources/{MARKETING}/storage.json"));
    let size = fs::metadata(&file).unwrap().len();
    assert!(
        (64_000..65_536).contains(&size),
        "the harness's own check: a file just under 64 KiB, got {size} bytes"
    );
    let second = pull(&machine, &project.dir).ok_json();
    assert_eq!(
        second["files"],
        json!([{ "path": format!("resources/{MARKETING}/storage.json"), "changed": false }]),
        "the pull again: expected the file the tool's own, unchanged, got {second}"
    );
    Report::clean(&check(&machine, &project.dir));
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": MARKETING, "changed": false, "files": [] })],
        "the deploy: expected the Resource pulled again, unchanged\n{}",
        deployed.run.describe()
    );
}
