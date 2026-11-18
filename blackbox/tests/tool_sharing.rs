//! The tool's commands of Contributors and sharing (CONTRACT.md,
//! "Contributors and sharing: The tool"): `vm show`, `vm share`,
//! `vm unshare`, `vm contributors add` and `vm contributors remove`, what
//! they print and how they end; `vm applications` with what is shared;
//! `vm deploy`'s `visible_to`; the instruction file of a new project.
//!
//! Acceptance criteria: "The tool's output after a deploy or a share names
//! who can now see the Application"; "The tool's list of Applications for
//! a person is exactly those they contribute to or that are shared with
//! them".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::deploy::{dana, deploy_in_words, deploy_ok, deploy_project, signed_in};
use blackbox::project::{help_commands, new_project, paragraphs, squashed, vm_commands_in};
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace};
use blackbox::sharing::{
    MAX_ENTRIES_A_REQUEST, MAX_SHARED_WITH, TRACED, add, assert_change_printed, assert_opens,
    contributors_ok, create_unshared, expected, group, keys, share_ok, tool_json, user,
};
use blackbox::tool::{Machine, bearer};
use blackbox::{User, Vm, VmOptions, fixture_bundle};
use serde_json::{Value, json};

const APP: &str = "board";

fn vm() -> Vm {
    Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    )
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// The program with `board` made by alice and shared with nobody, and a
/// machine signed in as alice.
fn with_board() -> (Vm, Machine) {
    let vm = vm();
    create_unshared(&vm, &alice(), APP, TRACED);
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    (vm, machine)
}

fn signed_in_as(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

#[test]
fn vm_help_lists_show_share_unshare_and_contributors() {
    let help = Machine::new().run(&["--help"]);
    help.expect_code(0);
    let listed = help_commands(&help.stdout);
    for command in ["show", "share", "unshare", "contributors"] {
        assert!(
            listed.iter().any(|c| c == command),
            "expected `vm --help` to list `{command}`, it lists {listed:?}"
        );
    }
}

#[test]
fn each_command_needs_a_sign_in() {
    let vm = vm();
    create_unshared(&vm, &alice(), APP, TRACED);
    let machine = Machine::new();
    for args in [
        vec!["show", APP],
        vec!["share", APP, "--user", "bob"],
        vec!["unshare", APP, "--user", "bob"],
        vec!["contributors", "add", APP, "--user", "bob"],
        vec!["contributors", "remove", APP, "--user", "bob"],
    ] {
        tool_json(&machine, &args).json_error(4, "not-signed-in");
    }
}

#[test]
fn the_four_that_change_something_need_an_entry_and_an_application() {
    let (_vm, machine) = with_board();
    for args in [
        vec!["share", APP],
        vec!["unshare", APP],
        vec!["contributors", "add", APP],
        vec!["contributors", "remove", APP],
        vec!["share", "--user", "bob"],
        vec!["contributors", "add", "--group", "finance"],
        vec!["show"],
        vec!["contributors", "add", APP, "--everyone"],
        vec!["contributors", "remove", APP, "--everyone"],
        vec!["share", APP, "--user"],
        vec!["contributors", APP, "--user", "bob"],
    ] {
        let run = tool_json(&machine, &args);
        assert_eq!(
            run.code,
            Some(2),
            "expected exit code 2 for `vm {}`\n{}",
            args.join(" "),
            run.describe()
        );
    }
}

#[test]
fn vm_share_prints_what_it_changed_and_who_can_see_it_now() {
    let (vm, machine) = with_board();
    let run = tool_json(
        &machine,
        &[
            "share", APP, "--user", "bob", "--group", "finance", "--user", "carol",
        ],
    );
    let printed = assert_change_printed(&run, APP);
    assert_eq!(
        keys(&printed["added"]),
        expected(&["user:bob", "group:finance", "user:carol"]),
        "{}",
        run.describe()
    );
    assert_eq!(printed["removed"], json!([]), "{}", run.describe());
    assert_eq!(
        keys(&printed["visible_to"]["contributors"]),
        expected(&["user:alice"])
    );
    assert_eq!(
        keys(&printed["visible_to"]["shared_with"]),
        expected(&["user:bob", "group:finance", "user:carol"])
    );
    assert_opens(&vm, &bob(), APP, "bob, after `vm share`");
    // The same again: nothing changed, the same who can see it.
    let again = assert_change_printed(&tool_json(&machine, &["share", APP, "--user", "bob"]), APP);
    assert_eq!(again["added"], json!([]));
    assert_eq!(again["visible_to"], printed["visible_to"]);
}

#[test]
fn vm_unshare_prints_what_it_removed_and_bob_can_no_longer_open_it() {
    let (vm, machine) = with_board();
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), blackbox::sharing::everyone()]),
    );
    let run = tool_json(
        &machine,
        &[
            "unshare",
            APP,
            "--everyone",
            "--user",
            "bob",
            "--user",
            "zed",
        ],
    );
    let printed = assert_change_printed(&run, APP);
    assert_eq!(printed["added"], json!([]));
    assert_eq!(
        {
            let mut removed = keys(&printed["removed"]);
            removed.sort();
            removed
        },
        expected(&["everyone", "user:bob"]),
        "only what this command changed: zed was not there\n{}",
        run.describe()
    );
    assert_eq!(printed["visible_to"]["shared_with"], json!([]));
    blackbox::sharing::assert_cannot_open(&vm, &bob(), APP, "bob, after `vm unshare`");
}

#[test]
fn vm_contributors_add_and_remove_print_what_changed_and_who_can_see_it() {
    let (vm, machine) = with_board();
    let added = assert_change_printed(
        &tool_json(
            &machine,
            &[
                "contributors",
                "add",
                APP,
                "--user",
                "bob",
                "--group",
                "finance",
            ],
        ),
        APP,
    );
    assert_eq!(
        keys(&added["added"]),
        expected(&["user:bob", "group:finance"])
    );
    assert_eq!(
        keys(&added["visible_to"]["contributors"]),
        expected(&["user:alice", "user:bob", "group:finance"])
    );
    let bobs = signed_in_as(&vm, &bob());
    let removed = assert_change_printed(
        &tool_json(
            &bobs,
            &[
                "contributors",
                "remove",
                APP,
                "--user",
                "alice",
                "--group",
                "finance",
            ],
        ),
        APP,
    );
    assert_eq!(
        keys(&removed["removed"]),
        expected(&["user:alice", "group:finance"])
    );
    assert_eq!(
        keys(&removed["visible_to"]["contributors"]),
        expected(&["user:bob"])
    );
}

#[test]
fn in_words_a_change_names_who_can_see_the_application_now() {
    let (vm, machine) = with_board();
    contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[group("board-keepers")]),
    );
    let run = machine.run(&[
        "share",
        APP,
        "--user",
        "bob",
        "--group",
        "finance",
        "--everyone",
    ]);
    let words = run.ok_text();
    for named in ["alice", "board-keepers", "bob", "finance"] {
        assert!(
            words.contains(named),
            "expected the words to name {named:?} among who can see it now\n{}",
            run.describe()
        );
    }
    assert!(
        words.to_ascii_lowercase().contains("everyone"),
        "expected the words to say everyone who can sign in can see it\n{}",
        run.describe()
    );
    // Unshared from all: visible to its Contributors only.
    let run = machine.run(&[
        "unshare",
        APP,
        "--user",
        "bob",
        "--group",
        "finance",
        "--everyone",
    ]);
    let words = run.ok_text().to_ascii_lowercase();
    assert!(
        words.contains("contributors only"),
        "expected the words to say it is visible to its Contributors only\n{}",
        run.describe()
    );
}

#[test]
fn vm_show_prints_the_application_as_the_api_answers_it() {
    let (vm, machine) = with_board();
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), group("finance")]),
    );
    let shown = tool_json(&machine, &["show", APP]).ok_json();
    assert_eq!(
        shown,
        vm.api_as(&alice()).application(APP),
        "expected `vm show --json` to print the Application as the API answers it"
    );
    let words = machine.run(&["show", APP]).ok_text();
    for named in [APP, "board.vm.test", "alice", "bob", "finance"] {
        assert!(
            words.contains(named),
            "expected the words of `vm show` to name {named:?}\n{words}"
        );
    }
    // To someone it is shared with: who maintains it, and that only its
    // Contributors see whom it is shared with.
    let bobs = signed_in_as(&vm, &bob());
    let shown = tool_json(&bobs, &["show", APP]).ok_json();
    assert!(shown["shared_with"].is_null(), "{shown}");
    let words = bobs.run(&["show", APP]).ok_text();
    assert!(
        words.contains("alice") && !words.contains("finance"),
        "expected bob to see the Contributors and not whom it is shared with\n{words}"
    );
    assert!(
        words.to_ascii_lowercase().contains("contributors"),
        "expected the words to say only its Contributors see that list\n{words}"
    );
}

#[test]
fn refusals_keep_the_apis_reasons_and_end_with_their_exit_codes() {
    let (vm, machine) = with_board();
    let bobs = signed_in_as(&vm, &bob());
    tool_json(&bobs, &["show", APP]).json_error(3, "not-shared");
    tool_json(&bobs, &["share", APP, "--user", "bob"]).json_error(3, "not-a-contributor");
    tool_json(&bobs, &["contributors", "add", APP, "--user", "bob"])
        .json_error(3, "not-a-contributor");
    tool_json(&machine, &["show", "nothing-here"]).json_error(3, "not-found");
    tool_json(&machine, &["share", "nothing-here", "--user", "bob"]).json_error(3, "not-found");
    tool_json(&machine, &["share", APP, "--email", "bob@example.test"])
        .json_error(3, "unknown-email");
    tool_json(
        &machine,
        &["contributors", "remove", APP, "--user", "alice"],
    )
    .json_error(3, "last-contributor");
    // too-many is one of "the rest": exit code 1.
    let api = vm.api_as(&alice());
    for round in 0..(MAX_SHARED_WITH / MAX_ENTRIES_A_REQUEST) {
        let batch: Vec<Value> = (0..MAX_ENTRIES_A_REQUEST)
            .map(|n| user(&format!("p{round}-{n}")))
            .collect();
        share_ok(&api, &vm, APP, &add(&batch));
    }
    tool_json(&machine, &["share", APP, "--user", "one-more"]).json_error(1, "too-many");
    // Without --json the message is on standard error.
    let run = bobs.run(&["share", APP, "--user", "bob"]);
    run.text_error(3);
}

#[test]
fn vm_applications_lists_those_contributed_to_and_those_shared_saying_which() {
    let vm = vm();
    let api = vm.api_as(&alice());
    for name in ["kept-by-bob", "shared-to-bob", "not-for-bob"] {
        create_unshared(&vm, &alice(), name, TRACED);
    }
    contributors_ok(&api, &vm, "kept-by-bob", &add(&[user("bob")]));
    share_ok(&api, &vm, "shared-to-bob", &add(&[group("finance")]));
    let bob_in_finance = User::new("bob").in_groups(&["finance"]);
    let machine = signed_in_as(&vm, &bob_in_finance);
    let listed = tool_json(&machine, &["applications"]).ok_json();
    let pairs: Vec<(String, Value)> = listed["applications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_owned(),
                a["contributor"].clone(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("kept-by-bob".to_owned(), json!(true)),
            ("shared-to-bob".to_owned(), json!(false)),
        ],
        "{listed}"
    );
    let words = machine.run(&["applications"]).ok_text();
    assert!(
        words.contains("kept-by-bob")
            && words.contains("shared-to-bob")
            && !words.contains("not-for-bob"),
        "{words}"
    );
    assert!(
        words.to_ascii_lowercase().contains("shared"),
        "expected the words to say which Applications are shared with the User\n{words}"
    );
}

#[test]
fn vm_deploy_names_whom_the_application_is_shared_with() {
    let vm = vm();
    let machine = signed_in(&vm);
    let name = "deployed-board";
    let project = deploy_project("sharing-deploy", name);
    let first = deploy_ok(&machine, &project.dir);
    assert_eq!(keys(&json!(first.contributors)), expected(&["user:dana"]));
    assert_eq!(first.shared_with, Vec::<Value>::new());
    let words = deploy_in_words(&machine, &project.dir, &[]).ok_text();
    assert!(
        words.contains("vm share") && words.to_ascii_lowercase().contains("contributors only"),
        "expected the words to say it is visible to its Contributors only and that `vm share` shares it\n{words}"
    );
    share_ok(
        &vm.api_as(&dana()),
        &vm,
        name,
        &add(&[group("finance"), user("bob")]),
    );
    let shared = deploy_ok(&machine, &project.dir);
    assert_eq!(
        keys(&json!(shared.shared_with)),
        expected(&["group:finance", "user:bob"])
    );
    let words = deploy_in_words(&machine, &project.dir, &[]).ok_text();
    for named in ["dana", "finance", "bob"] {
        assert!(
            words.contains(named),
            "expected the deploy's words to name {named:?} among who can see it\n{words}"
        );
    }
}

#[test]
fn the_instruction_file_says_who_can_see_an_application_and_names_the_commands() {
    let machine = Machine::new();
    let dir = machine.root().join("new-app");
    new_project(&machine, "new-app", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let named = vm_commands_in(&text);
    for command in ["share", "unshare", "contributors", "show"] {
        assert!(
            named.iter().any(|c| c == command),
            "expected AGENTS.md to name `vm {command}`, it names {named:?}"
        );
    }
    let flat = squashed(&text);
    assert!(
        flat.contains("vm contributors add"),
        "expected AGENTS.md to name `vm contributors add`\n{text}"
    );
    let paragraphs = paragraphs(&text);
    assert!(
        paragraphs.iter().any(|p| {
            let lower = squashed(p).to_lowercase();
            lower.contains("contributors only") && lower.contains("shared")
        }),
        "expected a paragraph saying a deployed Application is visible to its Contributors only until it is shared\n{text}"
    );
    assert!(
        paragraphs.iter().any(|p| {
            let lower = squashed(p).to_lowercase();
            lower.contains("everyone") && lower.contains("ask") && lower.contains("builder")
        }),
        "expected a paragraph saying to ask the Builder before sharing with everyone\n{text}"
    );
    assert!(
        paragraphs.iter().any(|p| {
            let lower = squashed(p).to_lowercase();
            lower.contains("who can see") && lower.contains("share") && lower.contains("builder")
        }),
        "expected a paragraph saying to tell the Builder who can see it after a deploy or a share\n{text}"
    );
}

/// With OIDC: `--email` shows as the person's `user`, with `name` and
/// `email`; the words name each person by name and email, and say beside
/// an id that the platform never saw that person sign in.
#[test]
fn with_oidc_an_email_shows_as_the_person_and_an_id_never_seen_is_said_so() {
    let at = Installation::start_with(&[], &[ada(), grace()], &[]);
    let ada_credential = at.tool_credential_for(ADA);
    for (target, body) in [
        ("/v1/applications".to_owned(), json!({ "name": APP })),
        (
            format!("/v1/applications/{APP}/versions"),
            blackbox::api::version_body(&fixture_bundle(TRACED)),
        ),
    ] {
        bearer(&at.vm, &target, &ada_credential)
            .method("POST")
            .json(&body)
            .send()
            .expect_status(201);
    }
    // Grace signs in once (the Application is not shared with her yet).
    let mut browser = at.browser_as(GRACE);
    browser.navigate(&at.app_url(APP, "/"));
    let machine = Machine::new();
    at.sign_in_tool(&machine, ADA);
    let run = tool_json(
        &machine,
        &[
            "share",
            APP,
            "--email",
            "Grace@Example.test",
            "--user",
            "00u-typo",
        ],
    );
    let printed = assert_change_printed(&run, APP);
    let added = &printed["added"];
    assert_eq!(
        keys(added),
        vec![format!("user:{GRACE}"), "user:00u-typo".to_owned()],
        "{}",
        run.describe()
    );
    let grace_entry = blackbox::sharing::entry_named(added, &format!("user:{GRACE}"));
    assert_eq!(grace_entry["name"], "Grace Hopper", "{grace_entry}");
    assert_eq!(grace_entry["email"], "grace@example.test", "{grace_entry}");
    let words_run = machine.run(&["share", APP, "--user", "00u-typo"]);
    let words = words_run.ok_text();
    assert!(
        words.contains("Grace Hopper") && words.contains("grace@example.test"),
        "expected the words to name Grace by her name and email\n{words}"
    );
    assert!(
        words.contains("Ada Lovelace"),
        "expected the words to name the Contributor Ada by name\n{words}"
    );
    let typo_line = words
        .lines()
        .find(|l| l.contains("00u-typo"))
        .unwrap_or_else(|| panic!("expected the words to name 00u-typo\n{words}"))
        .to_ascii_lowercase();
    assert!(
        typo_line.contains("never")
            || typo_line.contains("not signed in")
            || typo_line.contains("not seen"),
        "expected the words to say beside 00u-typo that the platform never saw them sign in, got {typo_line:?}\n{words}"
    );
}
