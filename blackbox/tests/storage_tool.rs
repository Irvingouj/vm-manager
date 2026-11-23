//! `vibe resources` and `vibe admin applications` with a Storage Resource
//! (CONTRACT.md, "Storage: The tool and the Dashboard", first two bullets;
//! ticket "Storage: a shared, named file Resource, on a mounted directory
//! now and S3 later"): the kind is said for each Resource
//! (`marketing (storage)`), a Storage Resource's access is said in its own
//! words (`read` that files can be listed and read, `write` that they can
//! also be put and deleted; no access, whom to ask, as for any), `--json`
//! is unchanged (`kind` is `storage`); `vibe admin applications` says each
//! declared Resource's kind with its level.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use blackbox::User;
use blackbox::storage::{FILE_READERS, FILE_WRITERS, FILES_CONTACT, MARKETING, Storage};
use blackbox::tool::{Machine, Run};
use serde_json::json;

/// A program with [`MARKETING`] (a Storage Resource) and `payroll` (a
/// database Resource) registered, [`FILE_READERS`] granted `read` and
/// [`FILE_WRITERS`] `write` on [`MARKETING`], nothing on `payroll`.
fn ready() -> Storage {
    let s = Storage::start();
    s.register(MARKETING);
    s.register_postgres("payroll");
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s
}

/// The words from the first `name` to the first `next` after it (the
/// Resources' names are in byte order, so `next` is the block after
/// `name`'s), or to the end.
fn block_of(words: &str, name: &str, next: Option<&str>) -> String {
    let start = words
        .find(name)
        .unwrap_or_else(|| panic!("expected {name:?} in\n{words}"));
    let after = start + name.len();
    let end = next
        .and_then(|n| words.get(after..).and_then(|rest| rest.find(n)))
        .map_or(words.len(), |at| at + after);
    words.get(start..end).unwrap_or(words).to_owned()
}

/// Whether `word` is one of the words of `text` (split at every character
/// that is not a letter or a digit).
fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

/// `vibe admin <args> --json`.
fn json_run(machine: &Machine, args: &[&str]) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    all.push("--json");
    machine.run(&all)
}

/// `vibe admin <args>` in words.
fn words_run(machine: &Machine, args: &[&str]) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    machine.run(&all)
}

/// The lines of `words` from the one that starts the block of `name` to
/// the next blank line.
fn lines_of(words: &str, name: &str) -> String {
    let lines: Vec<&str> = words.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.split_whitespace().next() == Some(name) || l.trim() == name)
        .unwrap_or_else(|| panic!("expected a block starting with {name:?}\n{words}"));
    lines[start..]
        .iter()
        .take_while(|l| !l.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_kind_is_said_for_each_resource_and_json_is_unchanged() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let listed = machine.run(&["resources", "--json"]).ok_json();
    assert_eq!(
        listed,
        json!({ "resources": [
            {
                "name": MARKETING,
                "kind": "storage",
                "description": "The marketing team's files",
                "owner": "The Marketing team",
                "contact": FILES_CONTACT,
                "your_access": "read",
            },
            {
                "name": "payroll",
                "kind": "postgres",
                "description": "The payroll database",
                "owner": "The DevOps team",
                "contact": "Dana Ops",
                "your_access": null,
            },
        ] }),
        "expected every registered Resource, by name, `kind` `storage` for the Storage one, and rita's own access"
    );
    for resource in listed["resources"].as_array().unwrap() {
        let mut keys: Vec<&str> = resource
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "contact",
                "description",
                "kind",
                "name",
                "owner",
                "your_access"
            ],
            "expected `--json` unchanged, the fields as for any Resource, got {resource}"
        );
    }
}

#[test]
fn a_storage_resources_access_is_said_in_its_own_words() {
    let s = ready();
    let machine = Machine::new();

    // `read`: files can be listed and read.
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let words = machine.run(&["resources"]).ok_text();
    assert!(
        words.contains(&format!("{MARKETING} (storage)")),
        "expected the kind said beside the name (`{MARKETING} (storage)`), got\n{words}"
    );
    assert!(
        words.contains("payroll (postgres)"),
        "expected the kind said for a database too, got\n{words}"
    );
    let block = block_of(&words, MARKETING, Some("payroll"));
    let lower = block.to_lowercase();
    assert!(
        lower.contains("list"),
        "expected `read` said as: files can be listed, got\n{block}"
    );
    assert!(
        lower.contains("read"),
        "expected `read` said as: files can be read, got\n{block}"
    );
    assert!(
        !lower.contains("no access"),
        "expected rita, who holds a Grant, not told there is no access\n{block}"
    );
    for word in ["put", "deleted", "delete", "rows", "row"] {
        assert!(
            !has_word(&lower, word),
            "expected `read` said as files that can be listed and read only, not {word:?}, got\n{block}"
        );
    }

    // `write`: they can also be put and deleted.
    machine.sign_in(&s.vm, &blackbox::storage::wanda());
    let words = machine.run(&["resources"]).ok_text();
    let block = block_of(&words, MARKETING, Some("payroll"));
    let lower = block.to_lowercase();
    for word in ["put", "delete"] {
        assert!(
            lower.contains(word),
            "expected `write` said as: files can also be {word}, got\n{block}"
        );
    }
    assert!(
        !lower.contains("no access"),
        "expected wanda, who holds a Grant, not told there is no access\n{block}"
    );
    assert!(
        !has_word(&lower, "rows") && !has_word(&lower, "row"),
        "expected `write` said in the words of files, not of rows, got\n{block}"
    );

    // No access: whom to ask, as for any.
    machine.sign_in(&s.vm, &blackbox::storage::nobody());
    let words = machine.run(&["resources"]).ok_text();
    let block = block_of(&words, MARKETING, Some("payroll"));
    assert!(
        block.contains(&format!("no access: ask {FILES_CONTACT}")),
        "expected the words to say whom to ask, got\n{block}"
    );
}

#[test]
fn vm_admin_applications_says_each_declared_resources_kind_with_its_level() {
    let place = blackbox::storage::Place::new("admin-tool");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &blackbox::dashboard::stand_in_options());
    s.register(MARKETING);
    s.register_postgres("devops");
    let api = s.vm.api_as(&User::new("alice").in_groups(&["staff"]));
    api.create_application("board").expect_status(201);
    let sent = api.send_version(
        "board",
        &blackbox::storage::version_body_with_kinds(
            &blackbox::vm::fixture_bundle("honest.js"),
            &[
                (MARKETING, "storage", "write"),
                ("devops", "postgres", "read"),
            ],
        ),
    );
    sent.expect_status(201);

    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::admin::administrator());
    let printed = json_run(&machine, &["applications"]).ok_json();
    assert_eq!(
        printed,
        blackbox::admin::list_page(&s.vm, ""),
        "`--json` is the API's answer as it is"
    );
    let board = &printed["applications"][0];
    assert_eq!(
        board["declared_access"]["resources"][MARKETING],
        json!({ "kind": "storage", "access": "write" }),
        "expected the Declared access to say the Storage Resource's kind with its level, got {board}"
    );
    assert_eq!(
        board["declared_access"]["resources"]["devops"],
        json!({ "kind": "postgres", "access": "read" }),
        "expected a database's kind with its level too, got {board}"
    );

    let words = words_run(&machine, &["applications"]).ok_text();
    let block = lines_of(&words, "board");
    // Each Resource's kind and level are said beside its name: from the
    // name to the next Resource's name on its line, or to the line's end.
    let names = [MARKETING, "devops"];
    for (name, kind, level, other_kind, other_level) in [
        (MARKETING, "storage", "write", "postgres", "read"),
        ("devops", "postgres", "read", "storage", "write"),
    ] {
        let line = block
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("expected a line naming {name:?}, got\n{block}"));
        let start = line.find(name).unwrap_or(0);
        let after = start + name.len();
        let end = names
            .iter()
            .filter(|n| **n != name)
            .filter_map(|n| line.get(after..).and_then(|rest| rest.find(n)))
            .map(|at| at + after)
            .min()
            .unwrap_or(line.len());
        let said = line.get(start..end).unwrap_or(line);
        for word in [kind, level] {
            assert!(
                said.contains(word),
                "expected {name}'s own words to say its kind {kind:?} and level {level:?} ({word:?} is missing), got {said:?} in\n{block}"
            );
        }
        for word in [other_kind, other_level] {
            assert!(
                !said.contains(word),
                "expected {name}'s own words not to say {word:?} (the other Resource's), got {said:?} in\n{block}"
            );
        }
    }
}
