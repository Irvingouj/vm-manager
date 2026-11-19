//! `vibe versions` and `vibe go-back` (CONTRACT.md, "Versions, going back, and
//! logs: The commands", "The instruction file"; ticket "Versions: list
//! them, go back to the previous one, read logs": listing Versions shows
//! each with its commit, when it was deployed, by whom, and which is
//! serving; going back switches serving at once and the output says that
//! migrations are not undone; each command has a JSON form).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::api::{assert_served_by, marked_bundle};
use blackbox::logs::{commit_of, from_to, go_back, version_body_with_commit};
use blackbox::own_database::{Own, first_tables, helpers, more_tables};
use blackbox::project::{
    code_in, help_commands, new_project, paragraphs, squashed, vm_commands_in,
};
use blackbox::tool::{Machine, Run};
use blackbox::{DOMAIN, INSTALLER, User, Vm};
use serde_json::{Value, json};

/// The Contributor of every Application these tests make.
fn alice() -> User {
    User::new(INSTALLER)
}

/// A User who is not a Contributor.
fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Creates `name` as [`alice`] with `n` Versions: `marked_bundle("v<i>")`
/// with the commit [`commit_of`]`(i)`.
fn with_versions(vm: &Vm, name: &str, n: u64) {
    let api = vm.api_as(&alice());
    api.create_application(name).expect_status(201);
    // Shared with everyone: these tests are not about who can open it.
    api.share_with_everyone(name);
    for i in 1..=n {
        api.send_version(
            name,
            &version_body_with_commit(&marked_bundle(&format!("v{i}")), &commit_of(i)),
        )
        .expect_status(201);
    }
}

/// A machine signed in as this User.
fn machine_as(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

/// The Application's record and its Versions.
fn state(vm: &Vm, name: &str) -> (Value, Vec<Value>) {
    let api = vm.api_as(&alice());
    (api.application(name), api.versions(name))
}

#[test]
fn vm_versions_lists_each_version_with_its_commit_when_and_by_whom_and_which_serves() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    go_back(&own.vm.api_as(&alice()), "board", &from_to(3, None)).expect_status(200);
    // Read by a Contributor: Versions are not for whom it is only shared
    // with.
    let machine = machine_as(&own.vm, &alice());
    let listed = machine.run(&["versions", "board", "--json"]).ok_json();
    assert_eq!(
        keys(&listed),
        [
            "address",
            "application",
            "latest_version",
            "serving_version",
            "versions"
        ],
        "{listed}"
    );
    assert_eq!(listed["application"], "board");
    assert_eq!(listed["address"], format!("board.{DOMAIN}"));
    assert_eq!(listed["serving_version"], 2);
    assert_eq!(listed["latest_version"], 3);
    let api = own.vm.api_as(&alice());
    let want: Vec<Value> = api
        .versions("board")
        .iter()
        .map(|v| {
            json!({
                "number": v["number"],
                "git_commit": v["git_commit"],
                "deployed_at": v["created_at"],
                "deployed_by": v["created_by"],
                "serving": v["number"] == 2,
            })
        })
        .collect();
    assert_eq!(
        listed["versions"],
        json!(want),
        "expected each Version, oldest first, with its commit, its created_at and created_by, and only Version 2 serving"
    );
    assert_eq!(listed["versions"][0]["git_commit"], commit_of(1));
    assert_eq!(listed["versions"][2]["deployed_by"], INSTALLER);
}

#[test]
fn vm_versions_in_words_has_a_line_for_each_version_and_marks_the_serving_one() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    go_back(&own.vm.api_as(&alice()), "board", &from_to(3, None)).expect_status(200);
    let machine = machine_as(&own.vm, &alice());
    let text = machine.run(&["versions", "board"]).ok_text();
    let line_of = |n: u64| -> String {
        let short: String = commit_of(n).chars().take(7).collect();
        let short = short.as_str();
        let found: Vec<&str> = text.lines().filter(|l| l.contains(short)).collect();
        assert_eq!(
            found.len(),
            1,
            "expected one line for Version {n} holding its commit ({short}...), got\n{text}"
        );
        found[0].to_owned()
    };
    for n in 1..=3 {
        let line = line_of(n);
        assert!(
            line.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == n.to_string()),
            "expected Version {n}'s line to hold its number, got {line:?}"
        );
        assert!(
            line.contains(INSTALLER),
            "expected Version {n}'s line to say who deployed it, got {line:?}"
        );
    }
    // The word `serving`, on the serving line only.
    let says_serving = |line: &str| {
        line.split(|c: char| !c.is_alphanumeric())
            .any(|w| w.eq_ignore_ascii_case("serving"))
    };
    assert!(
        says_serving(&line_of(2)),
        "expected the line of Version 2 marked as serving, got\n{text}"
    );
    let marked: Vec<&str> = text.lines().filter(|l| says_serving(l)).collect();
    assert_eq!(
        marked,
        [line_of(2).as_str()],
        "expected no other line to have the word `serving`, got\n{text}"
    );
}

#[test]
fn vm_versions_limit_prints_the_newest_and_says_how_many_are_not_shown() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let machine = machine_as(&own.vm, &alice());
    let listed = machine
        .run(&["versions", "board", "--limit", "2", "--json"])
        .ok_json();
    let numbers: Vec<u64> = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["number"].as_u64().unwrap())
        .collect();
    assert_eq!(
        numbers,
        [2, 3],
        "expected the newest 2, oldest first, {listed}"
    );
    assert_eq!(listed["latest_version"], 3, "{listed}");
    let text = machine
        .run(&["versions", "board", "--limit", "2"])
        .ok_text();
    let short: String = commit_of(1).chars().take(7).collect();
    assert!(
        !text.contains(&short),
        "expected Version 1 not shown with --limit 2, got\n{text}"
    );
    assert!(
        text.lines().any(|l| l.contains("--limit")
            && l.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == "1")),
        "expected the words to say how many older Versions are not shown (1) and that --limit shows more, got\n{text}"
    );
    for limit in ["0", "1001", "-1", "two"] {
        machine
            .run(&["versions", "board", "--limit", limit, "--json"])
            .expect_code(2);
    }
}

#[test]
fn vm_versions_names_the_serving_version_also_when_its_line_is_not_printed() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    go_back(&own.vm.api_as(&alice()), "board", &from_to(3, Some(1))).expect_status(200);
    let machine = machine_as(&own.vm, &alice());
    let listed = machine
        .run(&["versions", "board", "--limit", "1", "--json"])
        .ok_json();
    assert_eq!(listed["serving_version"], 1, "{listed}");
    assert_eq!(listed["latest_version"], 3, "{listed}");
    assert_eq!(
        listed["versions"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    assert_eq!(listed["versions"][0]["number"], 3, "{listed}");
    assert_eq!(listed["versions"][0]["serving"], false, "{listed}");
}

#[test]
fn vm_versions_of_an_application_with_no_version_says_vm_deploy_sends_the_first() {
    let own = Own::start();
    own.api().create_application("empty").expect_status(201);
    // Shared with everyone: this test is not about who can open it.
    own.api().share_with_everyone("empty");
    let machine = machine_as(&own.vm, &alice());
    assert_eq!(
        machine.run(&["versions", "empty", "--json"]).ok_json(),
        json!({ "application": "empty", "address": format!("empty.{DOMAIN}"),
                "serving_version": null, "latest_version": null, "versions": [] })
    );
    let text = machine.run(&["versions", "empty"]).ok_text();
    assert!(
        text.contains("vibe deploy") && text.to_lowercase().contains("no version"),
        "expected the words to say there is no Version yet and that `vibe deploy` sends the first, got\n{text}"
    );
}

#[test]
fn vm_versions_needs_a_sign_in_and_an_application_the_installation_has() {
    let own = Own::start();
    with_versions(&own.vm, "board", 1);
    let nobody = Machine::new();
    nobody
        .run(&["versions", "board", "--json"])
        .json_error(4, "not-signed-in");
    nobody.run(&["versions", "board"]).text_error(4);
    let machine = machine_as(&own.vm, &alice());
    machine
        .run(&["versions", "nothing-here", "--json"])
        .json_error(3, "not-found");
    machine.run(&["versions", "nothing-here"]).text_error(3);
    let missing = machine.run(&["versions", "--json"]);
    missing.expect_code(2);
}

#[test]
fn vm_go_back_goes_back_from_the_serving_version_to_the_one_before() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let machine = machine_as(&own.vm, &alice());
    let gone = machine.run(&["go-back", "board", "--json"]).ok_json();
    assert_eq!(
        gone,
        json!({ "application": "board", "address": format!("board.{DOMAIN}"),
                "serving_version": 2, "was": 3, "migrations_not_undone": [] })
    );
    assert_served_by(
        &own.vm,
        &own.vm.get_as(&bob(), "board", "/"),
        "board",
        "v2",
        2,
    );
    let again = machine.run(&["go-back", "board", "--json"]).ok_json();
    assert_eq!(again["serving_version"], 1, "{again}");
    assert_eq!(again["was"], 2, "{again}");
    assert_served_by(
        &own.vm,
        &own.vm.get_as(&bob(), "board", "/"),
        "board",
        "v1",
        1,
    );
}

#[test]
fn vm_go_back_to_goes_back_to_that_version() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let machine = machine_as(&own.vm, &alice());
    let gone = machine
        .run(&["go-back", "board", "--to", "1", "--json"])
        .ok_json();
    assert_eq!(gone["serving_version"], 1, "{gone}");
    assert_eq!(gone["was"], 3, "{gone}");
    assert_served_by(
        &own.vm,
        &own.vm.get_as(&bob(), "board", "/"),
        "board",
        "v1",
        1,
    );
}

#[test]
fn vm_go_back_in_words_says_migrations_are_not_undone_and_names_them() {
    let own = Own::start();
    own.create("notes");
    own.send_ok("notes", &[first_tables()]);
    own.send_ok("notes", &[first_tables(), helpers(), more_tables()]);
    let machine = machine_as(&own.vm, &alice());
    let run = machine.run(&["go-back", "notes"]);
    let text = squashed(&run.ok_text());
    let lower = text.to_lowercase();
    assert!(
        lower.contains("not undone"),
        "expected the words to say that migrations are not undone, got\n{text}"
    );
    for name in ["0001_helpers", "0002_more_tables"] {
        assert!(
            text.contains(name),
            "expected the words to name {name}, got\n{text}"
        );
    }
    assert!(
        text.contains("migrations/"),
        "expected the words to say the next deploy must still carry them in migrations/, got\n{text}"
    );
    assert!(
        text.contains(&format!("notes.{DOMAIN}")) && text.contains('1') && text.contains('2'),
        "expected the words to say Version 1 serves now at the address in place of Version 2, got\n{text}"
    );
    assert_eq!(own.serving(&bob(), "notes"), 1);

    // With no migration not undone, the words still say they are not.
    with_versions(&own.vm, "board", 2);
    let text = squashed(&machine.run(&["go-back", "board"]).ok_text()).to_lowercase();
    assert!(
        text.contains("not undone"),
        "expected the words always to say migrations are not undone, got\n{text}"
    );
}

#[test]
fn vm_go_back_json_names_the_migrations_not_undone() {
    let own = Own::start();
    own.create("notes");
    own.send_ok("notes", &[first_tables()]);
    own.send_ok("notes", &[first_tables(), helpers()]);
    own.send_ok("notes", &[first_tables(), helpers(), more_tables()]);
    let machine = machine_as(&own.vm, &alice());
    let gone = machine
        .run(&["go-back", "notes", "--to", "1", "--json"])
        .ok_json();
    assert_eq!(
        gone["migrations_not_undone"],
        json!(["0001_helpers", "0002_more_tables"]),
        "{gone}"
    );
}

#[test]
fn vm_go_back_with_nothing_before_the_serving_version_is_no_previous_version() {
    let own = Own::start();
    own.api().create_application("empty").expect_status(201);
    // Shared with everyone: this test is not about who can open it.
    own.api().share_with_everyone("empty");
    with_versions(&own.vm, "single", 1);
    let machine = machine_as(&own.vm, &alice());
    for name in ["empty", "single"] {
        let before = state(&own.vm, name);
        let message = machine
            .run(&["go-back", name, "--json"])
            .json_error(3, "no-previous-version");
        assert!(
            message.to_lowercase().contains("nothing"),
            "expected the message to say there is nothing to go back to, got {message:?}"
        );
        machine.run(&["go-back", name]).text_error(3);
        assert_eq!(
            state(&own.vm, name),
            before,
            "expected nothing changed in {name:?}"
        );
    }
}

#[test]
fn vm_go_back_to_a_version_that_is_not_an_earlier_one_is_no_such_version() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let machine = machine_as(&own.vm, &alice());
    let before = state(&own.vm, "board");
    for to in ["3", "4", "9"] {
        let message = machine
            .run(&["go-back", "board", "--to", to, "--json"])
            .json_error(3, "no-such-version");
        for n in ["1", "2"] {
            assert!(
                message.contains(n),
                "expected the message to name the Versions there are (1, 2), got {message:?}"
            );
        }
        assert_eq!(
            state(&own.vm, "board"),
            before,
            "--to {to}: expected nothing changed"
        );
    }
    machine
        .run(&["go-back", "board", "--to", "4"])
        .text_error(3);
}

#[test]
fn vm_go_back_to_that_is_not_a_positive_integer_is_a_wrong_command_line() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let machine = machine_as(&own.vm, &alice());
    let before = state(&own.vm, "board");
    for to in ["0", "-1", "two", "1.5", ""] {
        let run = machine.run(&["go-back", "board", "--to", to, "--json"]);
        run.expect_code(2);
        assert!(!run.stderr.trim().is_empty(), "{}", run.describe());
    }
    machine.run(&["go-back", "--json"]).expect_code(2);
    assert_eq!(state(&own.vm, "board"), before, "expected nothing changed");
}

#[test]
fn vm_go_back_by_someone_who_is_not_a_contributor_is_refused() {
    let own = Own::start();
    with_versions(&own.vm, "board", 3);
    let before = state(&own.vm, "board");
    let machine = machine_as(&own.vm, &bob());
    machine
        .run(&["go-back", "board", "--json"])
        .json_error(3, "not-a-contributor");
    machine.run(&["go-back", "board"]).text_error(3);
    assert_eq!(state(&own.vm, "board"), before, "expected nothing changed");
}

#[test]
fn vm_go_back_needs_a_sign_in_and_an_application_the_installation_has() {
    let own = Own::start();
    with_versions(&own.vm, "board", 2);
    Machine::new()
        .run(&["go-back", "board", "--json"])
        .json_error(4, "not-signed-in");
    let machine = machine_as(&own.vm, &alice());
    machine
        .run(&["go-back", "nothing-here", "--json"])
        .json_error(3, "not-found");
    assert_eq!(state(&own.vm, "board").0["serving_version"], 2);
}

/// Fails unless the run ended with `0` and its help (on either output)
/// names each of `options`.
fn assert_help_names(run: &Run, options: &[&str]) {
    run.expect_code(0);
    let text = format!("{}\n{}", run.stdout, run.stderr);
    for option in options {
        assert!(
            text.contains(option),
            "expected the help to name {option}\n{}",
            run.describe()
        );
    }
}

#[test]
fn vm_help_lists_versions_go_back_and_logs_and_each_help_names_its_options() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    for command in ["versions", "go-back", "logs"] {
        assert!(
            commands.iter().any(|c| c == command),
            "expected `vibe --help` to list `{command}`, got {commands:?}"
        );
    }
    assert_help_names(
        &machine.run(&["versions", "--help"]),
        &["--limit", "--json"],
    );
    assert_help_names(&machine.run(&["go-back", "--help"]), &["--to", "--json"]);
    assert_help_names(
        &machine.run(&["logs", "--help"]),
        &["--request", "--after", "--limit", "--follow", "--json"],
    );
}

#[test]
fn agents_md_names_the_three_commands_and_how_to_debug_and_go_back() {
    let machine = Machine::new();
    let dir = machine.root().join("new");
    new_project(&machine, "incident-notes", &dir);
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        text.lines().count() <= 120,
        "expected AGENTS.md at most 120 lines"
    );
    let named = vm_commands_in(&text);
    for command in ["versions", "go-back", "logs"] {
        assert!(
            named.iter().any(|c| c == command),
            "expected AGENTS.md to name `vibe {command}` in code, it names {named:?}\n{text}"
        );
    }
    let listed = help_commands(&machine.run(&["--help"]).stdout);
    for command in &named {
        assert!(
            listed.contains(command),
            "expected every `vibe <command>` of AGENTS.md to be one `vibe --help` lists; {command:?} is not ({listed:?})"
        );
    }
    assert!(
        code_in(&text).iter().any(|c| c
            .split_whitespace()
            .collect::<Vec<_>>()
            .starts_with(&["vibe", "logs"])),
        "expected `vibe logs <name>` in code"
    );
    let paragraphs: Vec<String> = paragraphs(&text)
        .iter()
        .map(|p| squashed(p).to_lowercase())
        .collect();
    let any = |what: &str, words: &[&str]| {
        assert!(
            paragraphs
                .iter()
                .any(|p| words.iter().all(|w| p.contains(w))),
            "expected a paragraph of AGENTS.md that says {what} (words {words:?})\n{text}"
        );
    };
    any("to debug, read `vibe logs`", &["debug", "vibe logs"]);
    any(
        "a response's x-vm-request-id is what --request takes",
        &["x-vm-request-id", "--request"],
    );
    any(
        "--after with the next of the last run reads what came since",
        &["--after", "next"],
    );
    any("--follow does not end by itself", &["--follow"]);
    assert!(
        paragraphs.iter().any(|p| p.contains("--follow")
            && (p.contains("not end")
                || p.contains("never end")
                || p.contains("does not stop")
                || p.contains("until"))),
        "expected AGENTS.md to say --follow does not end by itself\n{text}"
    );
    any(
        "vibe go-back is the quick way out of a bad deploy and does not undo migrations, which migrations/ keeps",
        &["vibe go-back", "migrations/"],
    );
    assert!(
        paragraphs.iter().any(|p| p.contains("vibe go-back")
            && (p.contains("not undo") || p.contains("does not undo") || p.contains("not undone"))),
        "expected AGENTS.md to say vibe go-back does not undo migrations\n{text}"
    );
}
