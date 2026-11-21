//! The tool's commands for Platform administrators (CONTRACT.md, "What
//! Platform administrators see and stop: The tool"): `vibe admin
//! applications`, `disable`, `enable` and `operation-log`, in words and
//! with `--json`; what the tool refuses before it asks (exit code `2`,
//! nothing asked); the paging lines; `vibe admin --help`; the API's
//! refusals and their exit codes; and the instruction file of a new
//! project (CONTRACT.md, "What Platform administrators see and stop: The
//! instruction file").
//!
//! Acceptance criterion: "Every command has a JSON form."
#![expect(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{self, administrator, disable_ok, list_page, log_page, request_records};
use blackbox::api::version_body_declaring;
use blackbox::project::{help_commands, new_project, paragraphs, squashed, vm_commands_in};
use blackbox::scenario::{READERS, RESOURCE, Scenario};
use blackbox::sharing::{TRACED, add, group, share_ok, user};
use blackbox::tool::{Machine, Run};
use blackbox::{DOMAIN, User, Vm, fixture_bundle};
use serde_json::json;

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn signed_in_as(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

fn json_run(machine: &Machine, args: &[&str]) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    all.push("--json");
    machine.run(&all)
}

fn words_run(machine: &Machine, args: &[&str]) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    machine.run(&all)
}

/// A program, `board` made by alice declaring `devops` at read with
/// contributors and sharing, and a machine signed in as the administrator.
fn with_board() -> (Vm, Machine) {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_application_with(&json!({ "name": "board", "description": "Incidents" }))
        .expect_status(201);
    api.send_version(
        "board",
        &version_body_declaring(
            &fixture_bundle(TRACED),
            &[(RESOURCE, "read"), ("hr", "write")],
        ),
    )
    .expect_status(201);
    blackbox::sharing::contributors_ok(&api, &vm, "board", &add(&[group("board-team")]));
    share_ok(
        &api,
        &vm,
        "board",
        &add(&[group("finance"), user("bob"), user("carol")]),
    );
    let machine = signed_in_as(&vm, &administrator());
    (vm, machine)
}

#[test]
fn vm_admin_help_names_the_four_beside_connect_oidc() {
    let help = Machine::new().run(&["admin", "--help"]);
    help.expect_code(0);
    let listed = help_commands(&help.stdout);
    for command in [
        "connect-oidc",
        "applications",
        "disable",
        "enable",
        "operation-log",
    ] {
        assert!(
            listed.iter().any(|c| c == command),
            "expected `vibe admin --help` to name `{command}`, it names {listed:?}\n{}",
            help.describe()
        );
    }
    for command in ["applications", "disable", "enable", "operation-log"] {
        let run = Machine::new().run(&["admin", command, "--help"]);
        run.expect_code(0);
        assert!(run.stdout.contains("--json"), "{}", run.describe());
    }
}

#[test]
fn each_needs_a_sign_in() {
    let machine = Machine::new();
    for args in [
        vec!["applications"],
        vec!["disable", "board"],
        vec!["enable", "board"],
        vec!["operation-log"],
    ] {
        json_run(&machine, &args).json_error(4, "not-signed-in");
    }
}

#[test]
fn what_the_tool_checks_before_it_asks_is_exit_code_2_and_asks_nothing() {
    let (vm, machine) = with_board();
    // The same commands well formed are asked and answered: what is
    // refused below is refused for what is wrong in it.
    for args in [
        vec!["applications", "--limit", "100", "--after", "a"],
        vec![
            "operation-log",
            "--user",
            "u",
            "--outcome",
            "allowed",
            "--limit",
            "1000",
            "--since",
            "2026-11-21T10:00:00Z",
            "--until",
            "2026-11-22T10:00:00+01:00",
            "--before",
            "5",
            "--application",
            "board",
            "--resource",
            "devops",
        ],
        vec!["disable", "board"],
        vec!["enable", "board"],
    ] {
        let before = request_records(&vm);
        let run = json_run(&machine, &args);
        run.expect_code(0);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            request_records(&vm) > before,
            "the harness's own check: a well-formed `vibe admin {}` asks the installation",
            args.join(" ")
        );
    }
    let long_user = "u".repeat(257);
    let long_name = "a".repeat(64);
    let cases: Vec<Vec<String>> = [
        vec!["disable"],
        vec!["enable"],
        vec!["disable", "Board"],
        vec!["enable", "-board"],
        vec!["disable", long_name.as_str()],
        vec!["disable", "board", "other"],
        vec!["applications", "--after", "Not_A_Name"],
        vec!["applications", "--limit", "0"],
        vec!["applications", "--limit", "101"],
        vec!["applications", "--limit", "x"],
        vec!["applications", "--limit", "5", "--limit", "5"],
        vec!["applications", "--after", "a", "--after", "b"],
        vec!["operation-log", "--application", "Board"],
        vec!["operation-log", "--resource", "Not A Name"],
        vec!["operation-log", "--user", ""],
        vec!["operation-log", "--user", long_user.as_str()],
        vec!["operation-log", "--user", "a\nb"],
        vec!["operation-log", "--outcome", "denied"],
        vec!["operation-log", "--since", "yesterday"],
        vec!["operation-log", "--since", "2026-11-21T10:00:00"],
        vec!["operation-log", "--until", "2026-11-21"],
        vec![
            "operation-log",
            "--since",
            "2026-11-22T10:00:00Z",
            "--until",
            "2026-11-21T10:00:00Z",
        ],
        vec![
            "operation-log",
            "--since",
            "2026-11-21T10:00:00Z",
            "--until",
            "2026-11-21T10:00:00Z",
        ],
        vec!["operation-log", "--before", "0"],
        vec!["operation-log", "--before", "-3"],
        vec!["operation-log", "--before", "x"],
        vec!["operation-log", "--limit", "0"],
        vec!["operation-log", "--limit", "1001"],
        vec!["operation-log", "--user", "a", "--user", "b"],
        vec![
            "operation-log",
            "--outcome",
            "allowed",
            "--outcome",
            "refused",
        ],
    ]
    .iter()
    .map(|args| args.iter().map(|a| (*a).to_owned()).collect())
    .collect();
    for args in &cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let before = request_records(&vm);
        let run = json_run(&machine, &refs);
        assert_eq!(
            run.code,
            Some(2),
            "expected exit code 2 for `vibe admin {}`\n{}",
            args.join(" "),
            run.describe()
        );
        assert!(
            !run.stderr.trim().is_empty(),
            "expected the usage text on standard error\n{}",
            run.describe()
        );
        // Give a request that was sent time to be recorded.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            request_records(&vm),
            before,
            "expected nothing asked of the installation for `vibe admin {}`",
            args.join(" ")
        );
    }
    assert!(
        list_page(&vm, "")["applications"][0]["disabled"].is_null(),
        "nothing was disabled"
    );
}

#[test]
fn vm_admin_applications_prints_the_apis_page_and_in_words_one_block_each() {
    let (vm, machine) = with_board();
    vm.api_as(&alice())
        .create_application("empty")
        .expect_status(201);
    vm.api_as(&alice())
        .create_with_bundle("open-to-all", &fixture_bundle(TRACED));
    disable_ok(&vm, "board");
    let printed = json_run(&machine, &["applications"]).ok_json();
    assert_eq!(
        printed,
        list_page(&vm, ""),
        "--json: the API's answer as it is"
    );

    let words = words_run(&machine, &["applications"]).ok_text();
    let block = block_of(&words, "board");
    let since = printed["applications"][0]["disabled"]["since"]
        .as_str()
        .unwrap()
        .to_owned();
    for named in [
        "board".to_owned(),
        format!("board.{DOMAIN}"),
        format!("disabled since {since} by {}", administrator().id),
        RESOURCE.to_owned(),
        "read".to_owned(),
        "hr".to_owned(),
        "write".to_owned(),
        "alice".to_owned(),
        "board-team".to_owned(),
        "0 requests by 0 Users in the last 30 days".to_owned(),
    ] {
        assert!(
            block.contains(&named),
            "expected the block of `board` to say {named:?}\n{words}"
        );
    }
    let flat = squashed(&block).to_lowercase();
    assert!(
        flat.contains('1')
            && flat.contains("group")
            && flat.contains('2')
            && flat.contains("people"),
        "expected whom it is shared with as the counts say (1 Group, 2 people)\n{block}"
    );
    let empty = block_of(&words, "empty");
    assert!(
        empty.to_lowercase().contains("none") || empty.to_lowercase().contains("no version"),
        "expected `empty` to say that no Version serves\n{empty}"
    );
    assert!(
        empty.to_lowercase().contains("nobody") || empty.to_lowercase().contains("not shared"),
        "expected `empty` to say it is shared with nobody\n{empty}"
    );
    let all = block_of(&words, "open-to-all");
    assert!(
        all.to_lowercase().contains("everyone"),
        "expected `open-to-all` to say it is shared with everyone\n{all}"
    );
    assert!(
        !words.to_lowercase().contains("--after"),
        "no paging line when `next` is null\n{words}"
    );
}

/// The lines of `words` from the one that starts the block of `name` to
/// the next blank line.
fn block_of(words: &str, name: &str) -> String {
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
fn vm_admin_applications_pages_and_says_how_to_read_the_next_page() {
    let vm = admin::start();
    let machine = signed_in_as(&vm, &administrator());
    let none = json_run(&machine, &["applications"]).ok_json();
    assert_eq!(none, json!({ "applications": [], "next": null }));
    let words = words_run(&machine, &["applications"]).ok_text();
    assert!(
        words.to_lowercase().contains("no application"),
        "expected the words to say the installation has no Application yet\n{words}"
    );
    for name in ["a", "b", "c"] {
        vm.api_as(&alice())
            .create_application(name)
            .expect_status(201);
    }
    let page = json_run(&machine, &["applications", "--limit", "2"]).ok_json();
    assert_eq!(page, list_page(&vm, "?limit=2"));
    assert_eq!(page["next"], "b");
    let words = words_run(&machine, &["applications", "--limit", "2"]).ok_text();
    let last = words.trim_end().lines().last().unwrap_or_default();
    assert!(
        last.contains("vibe admin applications") && last.contains("--after b"),
        "expected a last line giving the whole command with `--after b`, got {last:?}\n{words}"
    );
    let rest = json_run(&machine, &["applications", "--limit", "2", "--after", "b"]).ok_json();
    assert_eq!(rest, list_page(&vm, "?limit=2&after=b"));
    assert!(rest["next"].is_null());
}

#[test]
fn vm_admin_applications_after_a_name_with_none_after_it_says_so() {
    let vm = admin::start();
    let machine = signed_in_as(&vm, &administrator());
    vm.api_as(&alice())
        .create_application("board")
        .expect_status(201);
    // A name no Application has, after every one; and the last
    // Application's own name.
    for after in ["zzz", "board"] {
        let none = json_run(&machine, &["applications", "--after", after]).ok_json();
        assert_eq!(
            none,
            json!({ "applications": [], "next": null }),
            "--after {after} --json: the API's answer as it is"
        );
        let words = words_run(&machine, &["applications", "--after", after]).ok_text();
        let lower = squashed(&words).to_lowercase();
        assert!(
            words.contains(after) && lower.contains("after"),
            "--after {after}: expected the words to say that no Application comes after {after:?}\n{words}"
        );
        assert!(
            !lower.contains("yet"),
            "--after {after}: the installation has an Application; expected the words not to say it has none yet\n{words}"
        );
        assert!(
            !words.contains("--after"),
            "--after {after}: no paging line when `next` is null\n{words}"
        );
    }
}

#[test]
fn vm_admin_operation_log_takes_a_time_whose_t_and_z_are_lower_case() {
    let s = Scenario::ready();
    let rita = User::new("rita").in_groups(&[READERS]);
    for n in 0..4 {
        s.query(
            &rita,
            "board",
            &json!({ "resource": RESOURCE, "sql": format!("select {n} as n") }),
        );
    }
    let all = log_page(&s.vm, "?limit=1000");
    let entries = all["entries"].as_array().unwrap();
    assert!(
        entries.len() >= 4,
        "the harness's own check: expected the four queries' entries, got {all}"
    );
    let pivot = entries[entries.len() / 2]["time"]
        .as_str()
        .unwrap()
        .to_owned();
    let lower = pivot.replace('T', "t").replace('Z', "z");
    assert_ne!(lower, pivot, "the harness's own check");
    let machine = signed_in_as(&s.vm, &administrator());
    for option in ["--since", "--until"] {
        let upper = json_run(&machine, &["operation-log", option, &pivot]).ok_json();
        let lowered = json_run(&machine, &["operation-log", option, &lower]).ok_json();
        assert_eq!(
            lowered, upper,
            "`vibe admin operation-log {option} {lower}`: expected exactly what {pivot} answers"
        );
        assert!(
            !upper["entries"].as_array().unwrap().is_empty(),
            "the harness's own check: {option} {pivot} matches an entry: {upper}"
        );
    }
    let both = json_run(
        &machine,
        &[
            "operation-log",
            "--since",
            &lower,
            "--until",
            "2999-01-01t00:00:00z",
        ],
    )
    .ok_json();
    assert_eq!(
        both,
        json_run(
            &machine,
            &[
                "operation-log",
                "--since",
                &pivot,
                "--until",
                "2999-01-01T00:00:00Z"
            ]
        )
        .ok_json(),
        "both in lower case: exactly what the upper case answers"
    );
}

#[test]
fn vm_admin_disable_and_enable_print_what_they_did() {
    let (vm, machine) = with_board();
    let printed = json_run(&machine, &["disable", "board"]).ok_json();
    let since = admin::assert_disabled_member(&printed["disabled"], &administrator().id, "printed");
    assert_eq!(
        printed,
        json!({ "application": "board", "address": format!("board.{DOMAIN}"),
                "disabled": { "since": since, "by": administrator().id }, "changed": true })
    );
    let again = json_run(&machine, &["disable", "board"]).ok_json();
    assert_eq!(again["changed"], false);
    assert_eq!(again["disabled"], printed["disabled"]);
    let words = words_run(&machine, &["disable", "board"]).ok_text();
    let lower = words.to_lowercase();
    assert!(
        lower.contains("already") && words.contains(&since) && words.contains(&administrator().id),
        "with `changed` false: that it was disabled already, since when and by whom\n{words}"
    );
    assert!(
        words.contains("vibe admin enable board"),
        "expected the words to say `vibe admin enable board` serves it again\n{words}"
    );

    let enabled = json_run(&machine, &["enable", "board"]).ok_json();
    assert_eq!(
        enabled,
        json!({ "application": "board", "address": format!("board.{DOMAIN}"),
                "disabled": null, "changed": true, "serving_version": 1 })
    );
    let not = words_run(&machine, &["enable", "board"]).ok_text();
    assert!(
        not.to_lowercase().contains("not disabled"),
        "enabling one that is not disabled: says it was not disabled\n{not}"
    );
    // In words, a change: disabled, its address answers so, including
    // requests in flight, and how to undo it.
    let words = words_run(&machine, &["disable", "board"]).ok_text();
    let lower = words.to_lowercase();
    for said in ["disabled", "in flight", &format!("board.{DOMAIN}")] {
        assert!(
            lower.contains(said),
            "expected the words to say {said:?}\n{words}"
        );
    }
    assert!(words.contains("vibe admin enable board"), "{words}");
    let words = words_run(&machine, &["enable", "board"]).ok_text();
    assert!(
        words.contains(&format!("board.{DOMAIN}")) && words.contains("Version 1"),
        "expected the words to say it serves again at its address as Version 1\n{words}"
    );
    // One with no Version.
    vm.api_as(&alice())
        .create_application("empty")
        .expect_status(201);
    json_run(&machine, &["disable", "empty"]).ok_json();
    let printed = json_run(&machine, &["enable", "empty"]).ok_json();
    assert!(printed["serving_version"].is_null(), "{printed}");
    json_run(&machine, &["disable", "empty"]).ok_json();
    let words = words_run(&machine, &["enable", "empty"]).ok_text();
    assert!(
        words.to_lowercase().contains("no version"),
        "an enabled Application with no Version serving says so\n{words}"
    );
}

#[test]
fn vm_admin_operation_log_prints_the_apis_page_with_the_filters_given() {
    let s = Scenario::ready();
    let rita = User::new("rita").in_groups(&[READERS]);
    for n in 0..3 {
        s.query(
            &rita,
            "board",
            &json!({ "resource": RESOURCE, "sql": format!("select {n} as n") }),
        );
    }
    s.query(
        &User::new("nobody"),
        "board",
        &json!({ "resource": RESOURCE, "sql": "select 9 as n" }),
    );
    let machine = signed_in_as(&s.vm, &administrator());
    let all = json_run(&machine, &["operation-log"]).ok_json();
    assert_eq!(
        all,
        log_page(&s.vm, ""),
        "--json: the API's answer as it is"
    );
    let filtered = json_run(
        &machine,
        &[
            "operation-log",
            "--user",
            "rita",
            "--application",
            "board",
            "--resource",
            RESOURCE,
            "--outcome",
            "allowed",
            "--limit",
            "2",
        ],
    )
    .ok_json();
    assert_eq!(
        filtered,
        log_page(
            &s.vm,
            &format!("?user=rita&application=board&resource={RESOURCE}&outcome=allowed&limit=2")
        )
    );
    let earlier = filtered["earlier"]
        .as_u64()
        .expect("an older entry matches");
    // In words: one line per entry, oldest first, and the paging line.
    let words = words_run(
        &machine,
        &[
            "operation-log",
            "--user",
            "rita",
            "--outcome",
            "allowed",
            "--limit",
            "2",
        ],
    )
    .ok_text();
    let entries = filtered["entries"].as_array().unwrap();
    let firsts: Vec<&str> = words
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .collect();
    for entry in entries {
        let id = entry["id"].as_u64().unwrap().to_string();
        let line = firsts
            .iter()
            .find(|l| l.split_whitespace().next() == Some(id.as_str()))
            .unwrap_or_else(|| {
                panic!("expected a line beginning with the entry's id {id}\n{words}")
            });
        for field in [
            "allowed",
            "rita",
            "board",
            RESOURCE,
            "query",
            entry["time"].as_str().unwrap(),
        ] {
            assert!(
                line.contains(field),
                "expected the line of {id} to say {field:?}: {line}"
            );
        }
    }
    assert!(
        words.contains("    select") || words.contains("\tselect") || words.contains("  select"),
        "expected each statement on the next line, indented\n{words}"
    );
    let last = words.trim_end().lines().last().unwrap_or_default();
    assert!(
        last.contains("vibe admin operation-log") && last.contains(&format!("--before {earlier}")),
        "expected a last line giving the whole command with `--before {earlier}`, got {last:?}\n{words}"
    );
    let refused = words_run(&machine, &["operation-log", "--outcome", "refused"]).ok_text();
    assert!(
        refused.contains("no-grant") && refused.contains("nobody"),
        "a refused entry's line says its reason\n{refused}"
    );
    let none = words_run(&machine, &["operation-log", "--user", "zed"]).ok_text();
    assert!(
        none.contains("no entries match") && none.contains("zed"),
        "with no entry: `no entries match` and which filters\n{none}"
    );
    let none = json_run(&machine, &["operation-log", "--user", "zed"]).ok_json();
    assert_eq!(none, json!({ "entries": [], "earlier": null }));
    // A time with an offset, given as the tool takes it.
    let since = json_run(
        &machine,
        &["operation-log", "--since", "2000-01-01T01:00:00+01:00"],
    )
    .ok_json();
    assert_eq!(since, all);
}

#[test]
fn the_apis_refusals_keep_their_reasons_and_exit_codes() {
    let (vm, machine) = with_board();
    json_run(&machine, &["disable", "nothing-here"]).json_error(3, "not-found");
    words_run(&machine, &["enable", "nothing-here"]).text_error(3);
    // Someone who is not an administrator: exit 3, the API's reason, and
    // nothing on standard output without --json.
    let alices = signed_in_as(&vm, &alice());
    for args in [
        vec!["applications"],
        vec!["disable", "board"],
        vec!["enable", "board"],
        vec!["operation-log"],
        vec!["disable", "nothing-here"],
    ] {
        let run = json_run(&alices, &args);
        run.json_error(3, "not-an-administrator");
        let words = words_run(&alices, &args);
        words.text_error(3);
    }
    assert!(list_page(&vm, "")["applications"][0]["disabled"].is_null());
}

#[test]
fn what_the_installation_wrote_is_escaped_in_words() {
    let vm = admin::start();
    let escape = "Ops\u{1b}[2Jboard\u{202e}x";
    vm.api_as(&alice())
        .create_application_with(&json!({ "name": "board", "description": escape }))
        .expect_status(201);
    let machine = signed_in_as(&vm, &administrator());
    let words = words_run(&machine, &["applications"]).ok_text();
    assert!(
        !words.contains('\u{1b}') && !words.contains('\u{202e}'),
        "no control or bidirectional character reaches the terminal raw\n{words:?}"
    );
    let printed = json_run(&machine, &["applications"]);
    assert!(
        printed.stdout.contains("\\u001b") && printed.stdout.contains("\\u202e"),
        "with --json each is written as a JSON escape\n{}",
        printed.describe()
    );
    assert_eq!(printed.json()["applications"][0]["description"], escape);
}

#[test]
fn agents_md_names_application_disabled_and_says_to_tell_the_builder() {
    let machine = Machine::new();
    let dir = machine.root().join("incident-notes");
    new_project(&machine, "incident-notes", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        text.contains("application-disabled"),
        "expected AGENTS.md to name the reason `application-disabled`\n{text}"
    );
    let rule = paragraphs(&text)
        .into_iter()
        .find(|p| p.contains("application-disabled") || p.to_lowercase().contains("disabled"))
        .unwrap_or_else(|| panic!("expected a paragraph on a disabled Application\n{text}"));
    let lower = squashed(&rule).to_lowercase();
    for word in ["platform administrator", "nothing", "builder"] {
        assert!(
            lower.contains(word),
            "expected the paragraph on a disabled Application to say {word:?}, got\n{rule}"
        );
    }
    assert!(
        squashed(&rule).contains("IT"),
        "expected the paragraph to say the Builder asks their IT\n{rule}"
    );
    assert!(
        !vm_commands_in(&text).iter().any(|c| c == "admin"),
        "AGENTS.md names no `vibe admin` command\n{text}"
    );
}
