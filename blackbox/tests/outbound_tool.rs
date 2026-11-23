//! The tool's commands for the list (CONTRACT.md, "Outbound calls: The
//! tool"): `vibe outbound`, `vibe admin outbound add <host>` and `vibe admin
//! outbound remove <host>`, each with `--json` (the API's answer) and in
//! words; the sign-in they need (exit code 4); the refusal of someone who
//! is not a Platform administrator (exit code 3, reason
//! `not-an-administrator`); what the tool refuses before it asks (exit
//! code 2, nothing asked); `vibe --help` and `vibe admin --help`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::admin::administrator;
use blackbox::outbound_surface::{add, alice, asked, entries, hosts, signed_in, start};
use blackbox::project::{help_commands, squashed};
use blackbox::tool::{Machine, Run};
use serde_json::json;

fn json_run(machine: &Machine, args: &[&str]) -> Run {
    let mut all = args.to_vec();
    all.push("--json");
    machine.run(&all)
}

#[test]
fn the_help_names_outbound_beside_the_other_commands() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    assert!(
        help_commands(&help.stdout).iter().any(|c| c == "outbound"),
        "expected `vibe --help` to name `outbound`, it names {:?}\n{}",
        help_commands(&help.stdout),
        help.describe()
    );
    let help = machine.run(&["admin", "--help"]);
    help.expect_code(0);
    assert!(
        help_commands(&help.stdout).iter().any(|c| c == "outbound"),
        "expected `vibe admin --help` to name `outbound`, it names {:?}\n{}",
        help_commands(&help.stdout),
        help.describe()
    );
    let help = machine.run(&["outbound", "--help"]);
    help.expect_code(0);
    assert!(help.stdout.contains("--json"), "{}", help.describe());
    let help = machine.run(&["admin", "outbound", "--help"]);
    help.expect_code(0);
    let named = help_commands(&help.stdout);
    for command in ["add", "remove"] {
        assert!(
            named.iter().any(|c| c == command),
            "expected `vibe admin outbound --help` to name `{command}`, it names {named:?}\n{}",
            help.describe()
        );
    }
    for command in ["add", "remove"] {
        let help = machine.run(&["admin", "outbound", command, "--help"]);
        help.expect_code(0);
        assert!(help.stdout.contains("--json"), "{}", help.describe());
    }
}

#[test]
fn each_needs_a_sign_in() {
    let machine = Machine::new();
    json_run(&machine, &["outbound"]).json_error(4, "not-signed-in");
    json_run(&machine, &["admin", "outbound", "add", "api.example.com"])
        .json_error(4, "not-signed-in");
    json_run(
        &machine,
        &["admin", "outbound", "remove", "api.example.com"],
    )
    .json_error(4, "not-signed-in");
    machine.run(&["outbound"]).text_error(4);
}

#[test]
fn outbound_prints_the_apis_answer_with_json_and_a_line_an_entry_in_words() {
    let vm = start();
    add(&vm, "api.example.com");
    add(&vm, "*.example.org:8443");
    // A plain User may run it.
    let machine = signed_in(&vm, &alice());
    let printed = json_run(&machine, &["outbound"]).ok_json();
    assert_eq!(
        printed,
        json!({ "hosts": entries(&vm) }),
        "--json: the API's answer as it is"
    );
    let words = machine.run(&["outbound"]).ok_text();
    for entry in entries(&vm) {
        let host = entry["host"].as_str().unwrap();
        let lines: Vec<&str> = words.lines().filter(|l| l.contains(host)).collect();
        let [line] = lines.as_slice() else {
            panic!("expected one line for {host:?}, got {lines:?}\n{words}");
        };
        let by = entry["added_by"].as_str().unwrap();
        let date: String = entry["added_at"]
            .as_str()
            .unwrap()
            .chars()
            .take(10)
            .collect();
        assert!(
            squashed(line).contains(&format!("{host} added by {by}")),
            "expected the line to say `{host} added by {by} on <date>`, got {line:?}"
        );
        assert!(
            line.contains(&date),
            "expected the line to give the date {date}, got {line:?}"
        );
    }
    let first = words.find("*.example.org:8443").unwrap();
    let second = words.find("api.example.com").unwrap();
    assert!(first < second, "in the order of `host`\n{words}");
}

#[test]
fn with_no_host_it_says_none_may_be_called_and_how_an_administrator_allows_one() {
    let vm = start();
    let machine = signed_in(&vm, &alice());
    assert_eq!(
        json_run(&machine, &["outbound"]).ok_json(),
        json!({ "hosts": [] })
    );
    let words = machine.run(&["outbound"]).ok_text();
    let flat = squashed(&words).to_lowercase();
    assert!(
        flat.contains("no host") && flat.contains("platform administrator"),
        "expected the words to say that no host may be called and that a Platform administrator allows one\n{words}"
    );
    assert!(
        squashed(&words).contains("vibe admin outbound add <host>"),
        "expected the words to give the command `vibe admin outbound add <host>`\n{words}"
    );
}

#[test]
fn add_and_remove_print_the_apis_answer_with_json() {
    let vm = start();
    let machine = signed_in(&vm, &administrator());
    let added = json_run(&machine, &["admin", "outbound", "add", "api.example.com"]).ok_json();
    let listed = entries(&vm);
    assert_eq!(
        added,
        json!({
            "host": "api.example.com",
            "added_by": administrator().id,
            "added_at": listed[0]["added_at"],
            "changed": true,
        })
    );
    let again = json_run(&machine, &["admin", "outbound", "add", "api.example.com"]).ok_json();
    assert_eq!(again["changed"], false);
    assert_eq!(again["added_at"], added["added_at"]);
    let removed = json_run(
        &machine,
        &["admin", "outbound", "remove", "api.example.com"],
    )
    .ok_json();
    assert_eq!(
        removed,
        json!({ "host": "api.example.com", "changed": true })
    );
    let again = json_run(
        &machine,
        &["admin", "outbound", "remove", "api.example.com"],
    )
    .ok_json();
    assert_eq!(
        again,
        json!({ "host": "api.example.com", "changed": false })
    );
    assert!(hosts(&vm).is_empty());
}

#[test]
fn every_form_of_an_entry_reaches_the_installation_as_it_was_typed() {
    let vm = start();
    let machine = signed_in(&vm, &administrator());
    for (typed, kept) in [
        ("*.example.com:8443", "*.example.com:8443"),
        ("[2001:db8::7]:8443", "[2001:db8::7]:8443"),
        ("[2001:db8::7]", "[2001:db8::7]"),
        ("bücher.example", "xn--bcher-kva.example"),
        ("API.Example.COM", "api.example.com"),
        ("203.0.113.7:8443", "203.0.113.7:8443"),
    ] {
        let printed = json_run(&machine, &["admin", "outbound", "add", typed]).ok_json();
        assert_eq!(
            (printed["host"].as_str(), printed["changed"].as_bool()),
            (Some(kept), Some(true)),
            "`vibe admin outbound add {typed}`: got {printed}"
        );
    }
    assert_eq!(hosts(&vm).len(), 6);
    let removed = json_run(
        &machine,
        &["admin", "outbound", "remove", "[2001:db8::7]:8443"],
    )
    .ok_json();
    assert_eq!(
        removed,
        json!({ "host": "[2001:db8::7]:8443", "changed": true })
    );
}

#[test]
fn add_says_in_words_that_applications_may_call_it_from_the_next_call() {
    let vm = start();
    let machine = signed_in(&vm, &administrator());
    let words = machine
        .run(&["admin", "outbound", "add", "api.example.com"])
        .ok_text();
    let flat = squashed(&words).to_lowercase();
    assert!(
        words.contains("api.example.com")
            && flat.contains("may now call")
            && flat.contains("next call"),
        "expected the words to say that Applications may now call the host, from the next call on\n{words}"
    );
    assert!(
        !flat.contains("every name"),
        "expected nothing of a wildcard's words for a name\n{words}"
    );
    let again = machine
        .run(&["admin", "outbound", "add", "api.example.com"])
        .ok_text();
    assert!(
        squashed(&again).to_lowercase().contains("already"),
        "expected the words to say that Applications might already call it\n{again}"
    );
    let wildcard = machine
        .run(&["admin", "outbound", "add", "*.example.org"])
        .ok_text();
    let flat = squashed(&wildcard).to_lowercase();
    assert!(
        wildcard.contains("*.example.org")
            && (flat.contains("every name") || flat.contains("beneath")),
        "expected the words, for a wildcard, to say that every name beneath it is allowed\n{wildcard}"
    );
    let confirmed = machine
        .run(&["admin", "outbound", "add", "ask.example.net"])
        .ok_text();
    assert!(
        !confirmed.contains("[y/N]") && !confirmed.to_lowercase().contains("confirm"),
        "expected no confirmation asked\n{confirmed}"
    );
}

#[test]
fn remove_says_in_words_that_applications_may_no_longer_call_it() {
    let vm = start();
    add(&vm, "api.example.com");
    let machine = signed_in(&vm, &administrator());
    let words = machine
        .run(&["admin", "outbound", "remove", "api.example.com"])
        .ok_text();
    let flat = squashed(&words).to_lowercase();
    assert!(
        words.contains("api.example.com")
            && flat.contains("no longer")
            && flat.contains("next call"),
        "expected the words to say that Applications may no longer call the host, from the next call on\n{words}"
    );
    let again = machine
        .run(&["admin", "outbound", "remove", "api.example.com"])
        .ok_text();
    let flat = squashed(&again).to_lowercase();
    assert!(
        again.contains("api.example.com")
            && (flat.contains("before")
                || flat.contains("was not")
                || flat.contains("not on the list")),
        "expected the words to say that they could not call it before\n{again}"
    );
    assert!(
        !flat.contains("no longer"),
        "expected nothing of a change when there was none\n{again}"
    );
}

#[test]
fn someone_who_is_not_a_platform_administrator_is_refused_with_exit_code_3() {
    let vm = start();
    add(&vm, "there.example.com");
    let machine = signed_in(&vm, &alice());
    for args in [
        vec!["admin", "outbound", "add", "api.example.com"],
        vec!["admin", "outbound", "remove", "there.example.com"],
    ] {
        json_run(&machine, &args).json_error(3, "not-an-administrator");
        machine.run(&args).text_error(3);
    }
    assert_eq!(hosts(&vm), ["there.example.com"], "nothing changed");
    // The list is for everyone.
    machine.run(&["outbound"]).ok_text();
}

#[test]
fn what_the_tool_checks_before_it_asks_is_exit_code_2_and_asks_nothing() {
    let vm = start();
    let machine = signed_in(&vm, &administrator());
    // The same commands well formed are asked and answered: what is
    // refused below is refused for what is wrong in it.
    for args in [
        vec!["outbound"],
        vec!["admin", "outbound", "add", "well-formed.example.com"],
        vec!["admin", "outbound", "add", "Upper.Example.COM:443"],
        vec!["admin", "outbound", "remove", "well-formed.example.com"],
    ] {
        let before = asked(&vm);
        json_run(&machine, &args).expect_code(0);
        thread::sleep(Duration::from_millis(200));
        assert!(
            asked(&vm) > before,
            "the harness's own check: a well-formed `vibe {}` asks the installation",
            args.join(" ")
        );
    }
    let long = "a".repeat(300);
    let long_label = format!("{}.example.com", "a".repeat(64));
    let mut cases: Vec<Vec<&str>> = Vec::new();
    for verb in ["add", "remove"] {
        for entry in [
            "*.com",
            "*",
            "a.*.example.com",
            "https://api.example.com",
            "api.example.com/path",
            "user@api.example.com",
            "api example.com",
            "api.example.com:0",
            "api.example.com:65536",
            "api.example.com:x",
            "-a.example.com",
            "a-.example.com",
            "2001:db8::7:8443",
            "[2001:db8::7",
            "",
            long.as_str(),
            long_label.as_str(),
        ] {
            cases.push(vec!["admin", "outbound", verb, entry]);
        }
        cases.push(vec!["admin", "outbound", verb]);
        cases.push(vec![
            "admin",
            "outbound",
            verb,
            "a.example.com",
            "b.example.com",
        ]);
        cases.push(vec![
            "admin",
            "outbound",
            verb,
            "a.example.com",
            "--json",
            "--json",
        ]);
    }
    cases.push(vec!["outbound", "extra"]);
    cases.push(vec!["outbound", "--json", "--json"]);
    cases.push(vec!["outbound", "--nonsense"]);
    for args in &cases {
        let before = asked(&vm);
        // Without `--json`, so that an option given twice is the case's own.
        let run = machine.run(args);
        assert_eq!(
            run.code,
            Some(2),
            "expected exit code 2 for `vibe {}`\n{}",
            args.join(" "),
            run.describe()
        );
        assert!(
            !run.stderr.trim().is_empty(),
            "expected the usage text on standard error\n{}",
            run.describe()
        );
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            asked(&vm),
            before,
            "expected nothing asked of the installation for `vibe {}`",
            args.join(" ")
        );
    }
    assert_eq!(
        hosts(&vm),
        ["upper.example.com"],
        "what was refused added nothing and removed nothing"
    );
}

#[test]
fn a_name_whose_last_label_is_a_number_is_refused_by_the_tool_before_it_asks() {
    let vm = start();
    let machine = signed_in(&vm, &administrator());
    for entry in [
        "127.1",
        "0x7f.0.0.1",
        "2130706433",
        "1.2.3",
        "010.0.0.1",
        "203.0.113.010",
        "example.123",
        "example.0x1f",
        "*.example.123",
    ] {
        for verb in ["add", "remove"] {
            let before = asked(&vm);
            let run = machine.run(&["admin", "outbound", verb, entry]);
            assert_eq!(
                run.code,
                Some(2),
                "expected exit code 2 for `vibe admin outbound {verb} {entry}`\n{}",
                run.describe()
            );
            thread::sleep(Duration::from_millis(100));
            assert_eq!(asked(&vm), before, "expected nothing asked for {entry}");
        }
    }
    assert!(hosts(&vm).is_empty());
}
