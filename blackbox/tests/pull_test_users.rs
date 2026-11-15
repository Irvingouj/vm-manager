//! The Builder's own Grant copied into `vm.dev.json` by `vm pull`
//! (CONTRACT.md, "The local loop: one request as a test User: What a pull
//! writes, from this ticket on", "The test Users file"; ticket "Local loop:
//! run the Application on the Builder's machine and send a request as a
//! test User": a request as the Builder's test User returns the
//! Application's response; a Builder with structure-only access sees
//! locally the refusal their Users without a Grant will see).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blackbox::User;
use blackbox::local::{builder_user, dev_json, outsider_user, write_dev_json};
use blackbox::project::{Tree, new_project};
use blackbox::scenario::READERS;
use blackbox::structure::{STOCK, Stock};
use blackbox::tool::{Machine, Run};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

const MANIFEST: &str = r#"import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "stock-tool",
  description: "A project to pull into.",
  resources: { stock: { kind: "postgres", access: "read" } },
});
"#;

/// A project directory on the machine with a `manifest.ts` and no
/// `vm.dev.json`.
fn bare_project(machine: &Machine, name: &str) -> PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.ts"), MANIFEST).unwrap();
    dir
}

fn pull(machine: &Machine, dir: &Path) -> Run {
    machine.run(&["pull", STOCK, "--project", dir.to_str().unwrap(), "--json"])
}

/// The Grant a pull copies, at this level.
fn builder_grant(access: &str) -> Value {
    json!({ "group": "builder", "resource": STOCK, "access": access })
}

fn users_of(file: &Value) -> Vec<Value> {
    file["users"].as_array().cloned().unwrap_or_default()
}

fn grants_of(file: &Value) -> Vec<Value> {
    file["grants"].as_array().cloned().unwrap_or_default()
}

/// Fails unless the text is JSON written two spaces a level: every line
/// indented by spaces only, each line after one that opens an object or an
/// array two spaces further in.
fn assert_two_spaces_a_level(text: &str) {
    let mut previous: Option<(usize, bool)> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        assert!(
            !line.trim_start_matches(' ').starts_with('\t'),
            "expected no tab in vm.dev.json\n{text}"
        );
        if let Some((before, opened)) = previous
            && opened
        {
            let closes_at_once = matches!(line.trim(), "}" | "]" | "}," | "],");
            if !closes_at_once {
                assert_eq!(
                    indent,
                    before + 2,
                    "expected two spaces a level in vm.dev.json, at {line:?}\n{text}"
                );
            }
        }
        let trimmed = line.trim_end();
        previous = Some((indent, trimmed.ends_with('{') || trimmed.ends_with('[')));
    }
}

#[test]
fn a_pull_makes_vm_dev_json_when_it_is_not_there_with_outsider_and_the_builders_grant() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = bare_project(&machine, "board");
    let printed = pull(&machine, &dir).ok_json();
    assert_eq!(
        printed["test_users"],
        json!({ "file": "vm.dev.json", "changed": true, "grant": builder_grant("read") }),
        "expected `test_users` to say the file changed and which Grant was copied, got {printed}"
    );
    let file = dev_json(&dir);
    let users = users_of(&file);
    assert!(
        users.contains(&outsider_user()) && users.contains(&builder_user()),
        "expected the made file to hold outsider, as a new project has it, and the builder, got {file}"
    );
    assert_eq!(users.len(), 2, "expected those two Users only, got {file}");
    assert_eq!(
        grants_of(&file),
        vec![builder_grant("read")],
        "expected the Builder's own Grant, and no other, got {file}"
    );
    assert_two_spaces_a_level(&fs::read_to_string(dir.join("vm.dev.json")).unwrap());
}

#[test]
fn a_new_projects_file_keeps_what_it_had_and_gains_the_builder_and_the_grant() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = machine.root().join("fresh");
    new_project(&machine, "stock-board", &dir);
    fs::write(dir.join("manifest.ts"), MANIFEST).unwrap();
    let before = dev_json(&dir);
    let printed = pull(&machine, &dir).ok_json();
    assert_eq!(printed["test_users"]["changed"], json!(true), "{printed}");
    let after = dev_json(&dir);
    assert_eq!(
        after["about"], before["about"],
        "expected `about` kept as it was"
    );
    let users = users_of(&after);
    assert_eq!(
        users.first(),
        Some(&outsider_user()),
        "expected outsider kept, as it was and where it was, got {after}"
    );
    assert!(
        users.contains(&builder_user()),
        "expected the builder added, got {after}"
    );
    assert_eq!(users.len(), 2, "{after}");
    assert_eq!(grants_of(&after), vec![builder_grant("read")], "{after}");
    assert_two_spaces_a_level(&fs::read_to_string(dir.join("vm.dev.json")).unwrap());
}

#[test]
fn pulling_again_with_the_same_access_writes_nothing_and_says_so() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = bare_project(&machine, "board");
    pull(&machine, &dir).ok_json();
    let before = Tree::of(&dir);
    std::thread::sleep(Duration::from_millis(1100));
    let printed = pull(&machine, &dir).ok_json();
    assert_eq!(
        printed["test_users"],
        json!({ "file": "vm.dev.json", "changed": false, "grant": builder_grant("read") }),
        "{printed}"
    );
    assert_eq!(
        before.differences(&Tree::of(&dir)),
        Vec::<String>::new(),
        "expected vm.dev.json written only when something changed: bytes and modification time as they were"
    );
}

#[test]
fn a_changed_access_replaces_the_grant_that_group_had_on_the_resource() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = bare_project(&machine, "board");
    pull(&machine, &dir).ok_json();
    stock.scenario.grant(STOCK, READERS, "structure");
    let printed = pull(&machine, &dir).ok_json();
    assert_eq!(printed["your_access"], json!("structure"));
    assert_eq!(
        printed["test_users"],
        json!({ "file": "vm.dev.json", "changed": true, "grant": builder_grant("structure") }),
        "{printed}"
    );
    assert_eq!(
        grants_of(&dev_json(&dir)),
        vec![builder_grant("structure")],
        "expected the Grant replaced, not a second one beside it"
    );
}

#[test]
fn everything_else_in_the_file_is_kept() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = bare_project(&machine, "board");
    let carol = json!({ "id": "carol", "groups": ["ops", "night shift"], "team": "on call" });
    let outsider = json!({
        "id": "outsider",
        "displayName": "Outsider (no access)",
        "email": "outsider@example.com",
        "groups": [],
        "kept": [1, 2, 3]
    });
    let ops_grant =
        json!({ "group": "ops", "resource": STOCK, "access": "write", "why": "night shift" });
    let elsewhere = json!({ "group": "builder", "resource": "other_db", "access": "read" });
    let file = json!({
        "about": { "written": "by the Builder", "lines": ["one", "two"] },
        "users": [outsider, carol],
        "grants": [ops_grant, builder_grant("write"), elsewhere],
        "remarks": "a key the tool does not know"
    });
    write_dev_json(&dir, &file);
    let printed = pull(&machine, &dir).ok_json();
    assert_eq!(printed["test_users"]["changed"], json!(true), "{printed}");
    let after = dev_json(&dir);
    assert_eq!(after["about"], file["about"], "expected `about` kept");
    assert_eq!(
        after["remarks"], file["remarks"],
        "expected a key the tool does not know kept"
    );
    let users = users_of(&after);
    for user in [&outsider, &carol] {
        assert!(
            users.contains(user),
            "expected {user} kept as it was, got {after}"
        );
    }
    assert!(users.contains(&builder_user()), "{after}");
    assert_eq!(users.len(), 3, "{after}");
    let grants = grants_of(&after);
    for grant in [&ops_grant, &elsewhere] {
        assert!(
            grants.contains(grant),
            "expected {grant} kept as it was, got {after}"
        );
    }
    assert!(
        grants.contains(&builder_grant("read")) && !grants.contains(&builder_grant("write")),
        "expected the builder's Grant on {STOCK} replaced by the one held now, got {after}"
    );
    assert_eq!(grants.len(), 3, "{after}");
    assert_two_spaces_a_level(&fs::read_to_string(dir.join("vm.dev.json")).unwrap());
}

#[test]
fn a_file_that_cannot_be_read_ends_the_pull_before_anything_is_written() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    for (case, text, named) in [
        ("not JSON", "{ \"users\": [ \n".to_owned(), None),
        (
            "users that are not a list",
            "{ \"users\": {}, \"grants\": [] }\n".to_owned(),
            Some("users"),
        ),
        (
            "a Grant at an unknown level",
            serde_json::to_string(&json!({
                "users": [],
                "grants": [ { "group": "ops", "resource": STOCK, "access": "admin" } ]
            }))
            .unwrap(),
            Some("access"),
        ),
    ] {
        let dir = bare_project(&machine, &format!("broken-{}", case.replace(' ', "-")));
        fs::write(dir.join("vm.dev.json"), &text).unwrap();
        let before = Tree::of(&dir);
        let message = pull(&machine, &dir).json_error(1, "test-users-file");
        assert!(
            message.contains("vm.dev.json"),
            "{case}: expected the message to name the file, got {message:?}"
        );
        if let Some(word) = named {
            assert!(
                message.contains(word),
                "{case}: expected the message to say what is wrong ({word:?}), got {message:?}"
            );
        }
        assert_eq!(
            before.differences(&Tree::of(&dir)),
            Vec::<String>::new(),
            "{case}: expected nothing written: no resources/, vm.dev.json as it was"
        );
    }
}

#[test]
fn indentation_is_read_as_two_spaces_a_level() {
    assert_two_spaces_a_level("{\n  \"a\": [\n    1\n  ],\n  \"b\": {}\n}\n");
    let four = std::panic::catch_unwind(|| assert_two_spaces_a_level("{\n    \"a\": 1\n}\n"));
    assert!(
        four.is_err(),
        "the harness's own check: four spaces are not two"
    );
}
