//! The tool's commands for Grants (CONTRACT.md, "Resources and Grants in
//! full: The tool", stage three): `vibe admin grant`, `vibe admin revoke`
//! and `vibe admin grants (--resource | --group)`: what they check before
//! they ask (exit code `2`, nothing asked), `--json` as the API's answer,
//! the words for each outcome, a Group with a `/` and a space revoked
//! through the tool, the listing in words, the refusals (exit `3`), a `503`
//! (exit `1`), the sign-in (`4`), and `vibe admin --help` naming them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::{platform_sql, request_records, utc_day};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::project::help_commands;
use blackbox::resources::{fill_grants, forget_who_and_when, grants_of_group, revoke};
use blackbox::scenario::{RESOURCE, Scenario, administrator};
use blackbox::sql::Sql;
use blackbox::storage::{MARKETING, Storage};
use blackbox::tool::{Machine, Run};
use blackbox::{User, Vm, VmOptions};
use serde_json::{Value, json};

fn signed_in(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

fn admin_run(machine: &Machine, args: &[&str], json: bool) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    if json {
        all.push("--json");
    }
    machine.run(&all)
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn world() -> (Scenario, Machine) {
    let s = Scenario::start();
    s.register(RESOURCE);
    let machine = signed_in(&s.vm, &administrator());
    (s, machine)
}

fn flat(words: &str) -> String {
    words
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[test]
fn vm_admin_help_names_the_three_beside_the_others() {
    let help = Machine::new().run(&["admin", "--help"]);
    help.expect_code(0);
    let listed = help_commands(&help.stdout);
    for command in [
        "connect-oidc",
        "applications",
        "operation-log",
        "resources",
        "resource",
        "grant",
        "revoke",
        "grants",
    ] {
        assert!(
            listed.iter().any(|c| c == command),
            "expected `vibe admin --help` to name `{command}`, it names {listed:?}\n{}",
            help.describe()
        );
    }
    for (command, options) in [
        ("grant", vec!["--group", "--access", "--json"]),
        ("revoke", vec!["--group", "--json"]),
        ("grants", vec!["--resource", "--group", "--json"]),
    ] {
        let run = Machine::new().run(&["admin", command, "--help"]);
        run.expect_code(0);
        for option in options {
            assert!(
                run.stdout.contains(option),
                "expected `vibe admin {command} --help` to name {option}\n{}",
                run.describe()
            );
        }
    }
}

#[test]
fn each_needs_a_sign_in() {
    let machine = Machine::new();
    for args in [
        vec!["grant", "devops", "--group", "ops", "--access", "read"],
        vec!["revoke", "devops", "--group", "ops"],
        vec!["grants", "--resource", "devops"],
        vec!["grants", "--group", "ops"],
    ] {
        admin_run(&machine, &args, true).json_error(4, "not-signed-in");
        admin_run(&machine, &args, false).text_error(4);
    }
}

#[test]
fn what_the_tool_checks_before_it_asks_is_exit_code_2_and_asks_nothing() {
    let (s, machine) = world();
    let long = "g".repeat(257);
    let wide = "é".repeat(129);
    let control = "a\u{1}b";
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "grant without --group",
            vec!["grant", RESOURCE, "--access", "read"],
        ),
        (
            "grant without --access",
            vec!["grant", RESOURCE, "--group", "ops"],
        ),
        (
            "grant with an --access that is not a level",
            vec!["grant", RESOURCE, "--group", "ops", "--access", "admin"],
        ),
        (
            "grant with a level in capitals",
            vec!["grant", RESOURCE, "--group", "ops", "--access", "Read"],
        ),
        (
            "grant with an empty --group",
            vec!["grant", RESOURCE, "--group", "", "--access", "read"],
        ),
        (
            "grant with a --group over 256 bytes",
            vec!["grant", RESOURCE, "--group", &long, "--access", "read"],
        ),
        (
            "grant with a --group over 256 bytes of two-byte letters",
            vec!["grant", RESOURCE, "--group", &wide, "--access", "read"],
        ),
        (
            "grant with a --group with a control character",
            vec!["grant", RESOURCE, "--group", control, "--access", "read"],
        ),
        (
            "grant with a name no Resource could have",
            vec!["grant", "Not_A_Name", "--group", "ops", "--access", "read"],
        ),
        (
            "grant without a name",
            vec!["grant", "--group", "ops", "--access", "read"],
        ),
        (
            "grant with two names",
            vec![
                "grant", RESOURCE, "other", "--group", "ops", "--access", "read",
            ],
        ),
        (
            "grant with --group twice",
            vec![
                "grant", RESOURCE, "--group", "ops", "--group", "ops", "--access", "read",
            ],
        ),
        (
            "grant with --access twice",
            vec![
                "grant", RESOURCE, "--group", "ops", "--access", "read", "--access", "read",
            ],
        ),
        (
            "grant with an option it does not have",
            vec![
                "grant", RESOURCE, "--group", "ops", "--access", "read", "--yes",
            ],
        ),
        ("revoke without --group", vec!["revoke", RESOURCE]),
        (
            "revoke with an empty --group",
            vec!["revoke", RESOURCE, "--group", ""],
        ),
        (
            "revoke with a --group over 256 bytes",
            vec!["revoke", RESOURCE, "--group", &long],
        ),
        (
            "revoke with a --group with a control character",
            vec!["revoke", RESOURCE, "--group", control],
        ),
        (
            "revoke with a name no Resource could have",
            vec!["revoke", "Not_A_Name", "--group", "ops"],
        ),
        ("revoke without a name", vec!["revoke", "--group", "ops"]),
        (
            "revoke with --group twice",
            vec!["revoke", RESOURCE, "--group", "ops", "--group", "ops"],
        ),
        (
            "revoke with an option it does not have",
            vec!["revoke", RESOURCE, "--group", "ops", "--access", "read"],
        ),
        ("grants with neither --resource nor --group", vec!["grants"]),
        (
            "grants with both",
            vec!["grants", "--resource", RESOURCE, "--group", "ops"],
        ),
        (
            "grants with a --resource no Resource could have",
            vec!["grants", "--resource", "Not_A_Name"],
        ),
        (
            "grants with an empty --group",
            vec!["grants", "--group", ""],
        ),
        (
            "grants with a --group over 256 bytes",
            vec!["grants", "--group", &long],
        ),
        (
            "grants with a --group with a control character",
            vec!["grants", "--group", control],
        ),
        (
            "grants with --resource twice",
            vec!["grants", "--resource", RESOURCE, "--resource", RESOURCE],
        ),
        (
            "grants with --group twice",
            vec!["grants", "--group", "ops", "--group", "ops"],
        ),
        (
            "grants with a name where an option goes",
            vec!["grants", RESOURCE],
        ),
        (
            "grants with an option it does not have",
            vec!["grants", "--resource", RESOURCE, "--limit", "5"],
        ),
    ];
    for json in [false, true] {
        for (what, args) in &cases {
            let before = request_records(&s.vm);
            let run = admin_run(&machine, args, json);
            assert_eq!(
                run.code,
                Some(2),
                "expected exit code 2 for {what}: `vibe admin {}`{}\n{}",
                args.join(" ").replace('\u{1}', "<U+0001>"),
                if json { " --json" } else { "" },
                run.describe()
            );
            assert!(
                !run.stderr.trim().is_empty() && run.stdout.trim().is_empty(),
                "{what}: expected the usage text on standard error and nothing on standard output\n{}",
                run.describe()
            );
            assert!(
                !run.stderr.contains("unrecognized subcommand"),
                "{what}: expected the command to exist and refuse its arguments, not to be unknown\n{}",
                run.describe()
            );
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                request_records(&s.vm),
                before,
                "{what}: expected nothing asked of the installation"
            );
        }
    }
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"],
        json!([]),
        "nothing was written"
    );
}

#[test]
fn grant_prints_the_apis_answer_with_json() {
    let (s, machine) = world();
    let new = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "read"],
        true,
    )
    .ok_json();
    assert_eq!(
        new,
        json!({ "resource": RESOURCE, "group": "ops", "access": "read", "changed": true, "was": null })
    );
    let changed = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "write"],
        true,
    )
    .ok_json();
    assert_eq!(
        changed,
        json!({ "resource": RESOURCE, "group": "ops", "access": "write", "changed": true, "was": "read" })
    );
    let same = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "write"],
        true,
    )
    .ok_json();
    assert_eq!(
        same,
        json!({ "resource": RESOURCE, "group": "ops", "access": "write", "changed": false, "was": "write" })
    );
    // The Group is the Group the API holds.
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"][0]["group"],
        json!("ops")
    );
    // Neither asks for a confirmation: no `--yes`.
    admin_run(
        &machine,
        &[
            "grant",
            RESOURCE,
            "--group",
            "a/b c",
            "--access",
            "structure",
        ],
        true,
    )
    .ok_json();
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"][0]["group"],
        json!("a/b c")
    );
}

#[test]
fn grant_says_in_words_what_it_changed() {
    let (_s, machine) = world();
    let new = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "read"],
        false,
    )
    .ok_text();
    let lower = flat(&new);
    assert!(
        lower.contains("ops")
            && lower.contains("read")
            && lower.contains(RESOURCE)
            && lower.contains("every application"),
        "expected the words to say that the Group holds `read` on the Resource from now on, in every Application\n{new}"
    );
    assert!(
        !lower.contains("already") && !lower.contains("held "),
        "a new Grant is not said to have been held\n{new}"
    );
    let changed = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "write"],
        false,
    )
    .ok_text();
    let lower = flat(&changed);
    assert!(
        lower.contains("held read") && lower.contains("write") && lower.contains("ops"),
        "expected the words to say that the Group held `read` before and holds `write` now\n{changed}"
    );
    let same = admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "ops", "--access", "write"],
        false,
    )
    .ok_text();
    let lower = flat(&same);
    assert!(
        lower.contains("already") && lower.contains("write") && lower.contains("nothing"),
        "expected the words to say that the Group already held `write` and nothing changed\n{same}"
    );
    assert!(
        !lower.contains("every application"),
        "nothing changed, so nothing holds from now on\n{same}"
    );
}

#[test]
fn revoke_prints_the_apis_answer_and_in_words_what_it_held() {
    let (s, machine) = world();
    s.grant(RESOURCE, "ops", "write");
    s.grant(RESOURCE, "dev", "read");
    let json_run = admin_run(&machine, &["revoke", RESOURCE, "--group", "dev"], true).ok_json();
    assert_eq!(
        json_run,
        json!({ "resource": RESOURCE, "group": "dev", "changed": true, "was": "read" })
    );
    let words = admin_run(&machine, &["revoke", RESOURCE, "--group", "ops"], false).ok_text();
    let lower = flat(&words);
    assert!(
        lower.contains("ops") && lower.contains("no longer") && lower.contains("write"),
        "expected the words to say that the Group no longer holds a Grant and that it held `write`\n{words}"
    );
    assert!(
        lower.contains("every application"),
        "expected the words to say that this holds from now on in every Application\n{words}"
    );
    assert!(
        words.contains("vibe admin grant"),
        "expected the words to name the command that gives it again\n{words}"
    );
    let none = admin_run(&machine, &["revoke", RESOURCE, "--group", "ops"], false).ok_text();
    let lower = flat(&none);
    assert!(
        lower.contains("none") && lower.contains("nothing changed"),
        "expected the words to say that the Group held none and nothing changed\n{none}"
    );
    let none_json = admin_run(&machine, &["revoke", RESOURCE, "--group", "ops"], true).ok_json();
    assert_eq!(
        none_json,
        json!({ "resource": RESOURCE, "group": "ops", "changed": false, "was": null })
    );
}

#[test]
fn a_group_with_a_slash_and_a_space_is_revoked_through_the_tool_and_no_other() {
    let (s, machine) = world();
    for group in [
        "ops/team x",
        "ops",
        "ops/team",
        "ops team x",
        "a+b",
        "a b",
        "100%",
        "équipe",
    ] {
        s.grant(RESOURCE, group, "read");
    }
    for group in ["ops/team x", "a+b", "100%", "équipe"] {
        let answer = admin_run(&machine, &["revoke", RESOURCE, "--group", group], true).ok_json();
        assert_eq!(
            answer,
            json!({ "resource": RESOURCE, "group": group, "changed": true, "was": "read" }),
            "revoking {group:?}"
        );
    }
    let left: Vec<String> = s.admin().list_grants(RESOURCE).json()["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["group"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(left, ["a b", "ops", "ops team x", "ops/team"]);
}

#[test]
fn grants_of_a_resource_prints_the_apis_answer_and_a_line_for_each_grant() {
    let (s, machine) = world();
    s.grant(RESOURCE, "ops", "write");
    s.vm.api_as(&User::new("other-admin").in_groups(&[blackbox::scenario::ADMINISTRATORS]))
        .grant(RESOURCE, "auditors", "read")
        .expect_status(200);
    let json_run = admin_run(&machine, &["grants", "--resource", RESOURCE], true).ok_json();
    assert_eq!(json_run, s.admin().list_grants(RESOURCE).json());
    let today = utc_day(&platform_sql(&s.vm), 0);
    let words = admin_run(&machine, &["grants", "--resource", RESOURCE], false).ok_text();
    let lines: Vec<String> = words.lines().map(flat_line).collect();
    let line_of = |group: &str| -> &String {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("{group} ")))
            .unwrap_or_else(|| panic!("expected a line for {group:?}\n{words}"))
    };
    assert_eq!(
        *line_of("auditors"),
        format!("auditors read set by other-admin on {today}"),
        "{words}"
    );
    assert_eq!(
        *line_of("ops"),
        format!("ops write set by {} on {today}", administrator().id),
        "{words}"
    );
    // One line for each Grant, none for anything else that holds a Group's
    // name.
    assert_eq!(
        lines.iter().filter(|l| l.contains(" set by ")).count(),
        2,
        "{words}"
    );
}

fn flat_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn an_old_grant_is_not_recorded_in_words() {
    let (s, _machine) = world();
    s.grant(RESOURCE, "old", "read");
    s.grant(RESOURCE, "alsoold", "write");
    forget_who_and_when(&s.vm);
    let Scenario { vm, db } = s;
    let s = Scenario {
        vm: blackbox::storage::terminate(vm).start(),
        db,
    };
    let machine = signed_in(&s.vm, &administrator());
    let words = admin_run(&machine, &["grants", "--resource", RESOURCE], false).ok_text();
    let old = words
        .lines()
        .map(flat_line)
        .find(|l| l.starts_with("old "))
        .unwrap_or_else(|| panic!("expected a line for `old`\n{words}"));
    assert_eq!(old, "old read not recorded", "{words}");
    let by_group = admin_run(&machine, &["grants", "--group", "alsoold"], false).ok_text();
    let line = by_group
        .lines()
        .map(flat_line)
        .find(|l| l.starts_with("devops "))
        .unwrap_or_else(|| panic!("expected a line for the Resource\n{by_group}"));
    assert_eq!(line, "devops (postgres) write not recorded", "{by_group}");
    assert!(!words.contains("set by") && !by_group.contains("set by"));
    let json_run = admin_run(&machine, &["grants", "--resource", RESOURCE], true).ok_json();
    assert_eq!(json_run["grants"][0]["granted_by"], Value::Null);
}

#[test]
fn grants_of_a_group_prints_the_apis_answer_and_a_line_for_each_resource() {
    let s = Storage::start();
    s.register_postgres("devops");
    s.register(MARKETING);
    s.grant("devops", "ops", "write");
    s.grant(MARKETING, "ops", "read");
    s.grant(MARKETING, "other", "write");
    let machine = signed_in(&s.vm, &administrator());
    let json_run = admin_run(&machine, &["grants", "--group", "ops"], true).ok_json();
    assert_eq!(json_run, grants_of_group(&s.admin(), "ops").json());
    let today = utc_day(&platform_sql(&s.vm), 0);
    let words = admin_run(&machine, &["grants", "--group", "ops"], false).ok_text();
    let lines: Vec<String> = words.lines().map(flat_line).collect();
    for want in [
        format!(
            "devops (postgres) write set by {} on {today}",
            administrator().id
        ),
        format!(
            "marketing (storage) read set by {} on {today}",
            administrator().id
        ),
    ] {
        assert!(lines.contains(&want), "expected the line {want:?}\n{words}");
    }
    assert_eq!(lines.iter().filter(|l| l.contains(" set by ")).count(), 2);
    // A Group with a space and a slash is asked as the API takes it.
    s.grant("devops", "a b/c+d", "read");
    let odd = admin_run(&machine, &["grants", "--group", "a b/c+d"], true).ok_json();
    assert_eq!(odd["group"], json!("a b/c+d"));
    assert_eq!(odd["grants"][0]["resource"], json!("devops"));
}

#[test]
fn the_empty_lists_say_so_and_name_the_command_that_gives_one() {
    let (s, machine) = world();
    let words = admin_run(&machine, &["grants", "--resource", RESOURCE], false).ok_text();
    let lower = flat(&words);
    assert!(
        lower.contains("no group") && words.contains("vibe admin grant"),
        "expected the words to say that no Group holds a Grant on the Resource, and the command that gives one\n{words}"
    );
    let json_run = admin_run(&machine, &["grants", "--resource", RESOURCE], true).ok_json();
    assert_eq!(json_run, json!({ "grants": [], "grants_more": 0 }));
    let words = admin_run(&machine, &["grants", "--group", "nobody"], false).ok_text();
    let lower = flat(&words);
    assert!(
        lower.contains("nobody") && lower.contains("none") && words.contains("vibe admin grant"),
        "expected the words to say that the Group holds none, and the command that gives one\n{words}"
    );
    let json_run = admin_run(&machine, &["grants", "--group", "nobody"], true).ok_json();
    assert_eq!(json_run, json!({ "group": "nobody", "grants": [] }));
    assert_eq!(
        grants_of_group(&s.admin(), "nobody").json(),
        json!({ "group": "nobody", "grants": [] })
    );
}

#[test]
fn the_apis_refusals_keep_their_reasons_and_end_with_3() {
    let s = Storage::start();
    s.register_postgres("devops");
    s.register(MARKETING);
    s.grant("devops", "ops", "read");
    let machine = signed_in(&s.vm, &administrator());
    // `structure` on a Storage Resource: the API's `malformed`.
    let message = admin_run(
        &machine,
        &[
            "grant",
            MARKETING,
            "--group",
            "ops",
            "--access",
            "structure",
        ],
        true,
    )
    .json_error(3, "malformed");
    assert!(
        message.contains("read") && message.contains("write"),
        "expected the message to say that a Storage Resource has two levels, got {message:?}"
    );
    admin_run(
        &machine,
        &[
            "grant",
            MARKETING,
            "--group",
            "ops",
            "--access",
            "structure",
        ],
        false,
    )
    .text_error(3);
    // No such Resource.
    for args in [
        vec!["grant", "ghost", "--group", "ops", "--access", "read"],
        vec!["revoke", "ghost", "--group", "ops"],
        vec!["grants", "--resource", "ghost"],
    ] {
        admin_run(&machine, &args, true).json_error(3, "not-found");
        admin_run(&machine, &args, false).text_error(3);
    }
    // Not a Platform administrator.
    let alice = signed_in(&s.vm, &alice());
    for args in [
        vec!["grant", "devops", "--group", "ops", "--access", "write"],
        vec!["revoke", "devops", "--group", "ops"],
        vec!["grants", "--resource", "devops"],
        vec!["grants", "--group", "ops"],
    ] {
        admin_run(&alice, &args, true).json_error(3, "not-an-administrator");
        admin_run(&alice, &args, false).text_error(3);
    }
    assert_eq!(
        s.admin().list_grants("devops").json()["grants"][0]["access"],
        json!("read"),
        "nothing was changed by a refused command"
    );
}

#[test]
fn a_1001st_grant_is_too_many_with_exit_3_and_a_revoke_makes_room() {
    let (s, machine) = world();
    fill_grants(&s.vm, RESOURCE, 0, 1000, "read");
    admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "overflow", "--access", "read"],
        true,
    )
    .json_error(3, "too-many");
    admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "overflow", "--access", "read"],
        false,
    )
    .text_error(3);
    admin_run(
        &machine,
        &[
            "grant",
            RESOURCE,
            "--group",
            "group0001",
            "--access",
            "write",
        ],
        true,
    )
    .ok_json();
    admin_run(
        &machine,
        &["revoke", RESOURCE, "--group", "group0002"],
        true,
    )
    .ok_json();
    admin_run(
        &machine,
        &["grant", RESOURCE, "--group", "overflow", "--access", "read"],
        true,
    )
    .ok_json();
    assert_eq!(revoke(&s.admin(), RESOURCE, "overflow").status, 200);
}

/// A program on a platform database of its own, `devops` registered, with
/// the row of the Resource held by a lock of the harness's own for 10 s,
/// and what `run` answers meanwhile.
fn while_the_row_is_locked(name: &str, run: impl Fn(&Machine) -> Run) -> (Run, Duration) {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let platform = Platform::on(&postgres, name);
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "read");
    let machine = signed_in(&s.vm, &administrator());
    let holder = Sql::connect(&postgres.url(name)).expect("the harness's own connection");
    holder.batch("BEGIN");
    holder.batch("SELECT id FROM resources WHERE name = 'devops' FOR UPDATE");
    let began = Instant::now();
    let answer = run(&machine);
    let took = began.elapsed();
    holder.batch("ROLLBACK");
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"][0]["access"],
        json!("read"),
        "nothing was changed"
    );
    (answer, took)
}

/// Takes about 10 s: the write waits for the row's lock.
#[test]
fn a_503_of_a_grant_is_exit_code_1() {
    let (run, took) = while_the_row_is_locked("vm_platform_tool_grant_503", |machine| {
        admin_run(
            machine,
            &["grant", RESOURCE, "--group", "ops", "--access", "write"],
            true,
        )
    });
    run.json_error(1, "unavailable");
    assert!(
        took >= Duration::from_secs(9),
        "expected the write to wait 10 s for the row's lock, took {took:?}"
    );
}

/// Takes about 10 s: the write waits for the row's lock.
#[test]
fn a_503_of_a_revoke_is_exit_code_1() {
    let (run, took) = while_the_row_is_locked("vm_platform_tool_revoke_503", |machine| {
        admin_run(machine, &["revoke", RESOURCE, "--group", "ops"], true)
    });
    run.json_error(1, "unavailable");
    assert!(
        took >= Duration::from_secs(9),
        "expected the write to wait 10 s for the row's lock, took {took:?}"
    );
}

#[test]
fn the_tools_own_checks_come_before_the_sign_in_is_looked_for() {
    let machine = Machine::new();
    for args in [
        vec!["grant", RESOURCE, "--access", "read"],
        vec!["grant", RESOURCE, "--group", "ops", "--access", "admin"],
        vec!["revoke", RESOURCE],
        vec!["revoke", "Not_A_Name", "--group", "ops"],
        vec!["grants"],
        vec!["grants", "--resource", RESOURCE, "--group", "ops"],
    ] {
        for json in [false, true] {
            let run = admin_run(&machine, &args, json);
            assert_eq!(
                run.code,
                Some(2),
                "expected exit code 2, not 4, for `vibe admin {}` with no sign-in\n{}",
                args.join(" "),
                run.describe()
            );
            assert!(
                !run.stderr.contains("unrecognized subcommand"),
                "{}",
                run.describe()
            );
        }
    }
}

/// Puts `new` where a text column of `grants` holds `current`, by hand.
fn rewrite_by_hand(vm: &Vm, current: &str, new: &str) {
    let platform = platform_sql(vm);
    let columns: Vec<String> = platform
        .batch(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name = 'grants' AND data_type IN ('text', 'character varying')",
        )
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    let hex: String = new.bytes().map(|b| format!("{b:02x}")).collect();
    for column in columns {
        platform.batch(&format!(
            "UPDATE grants SET \"{column}\" = convert_from(decode('{hex}', 'hex'), 'UTF8') WHERE \"{column}\" = '{current}'"
        ));
    }
}

fn assert_no_raw_control(text: &str, context: &str) {
    let raw: Vec<String> = text
        .chars()
        .filter(|c| {
            (c.is_control() && *c != '\n')
                || matches!(u32::from(*c), 0x202A..=0x202E | 0x2066..=0x2069 | 0x200E | 0x200F)
        })
        .map(|c| format!("U+{:04X}", u32::from(c)))
        .collect();
    assert!(
        raw.is_empty(),
        "{context}: expected no raw control character or bidi override, found {raw:?} in\n{}",
        text.escape_debug()
    );
}

/// There is no stand-in installation that answers what a test chooses, so
/// the Group and `granted_by` are written by hand in the platform database
/// (a Group with a control character cannot be given through the API) and
/// the program is started again. `granted_at` is a time and `resource` a
/// validated name: neither can hold such bytes in a real installation.
#[test]
fn what_the_installation_answered_is_escaped_in_the_listings_in_words() {
    let (s, _machine) = world();
    s.grant(RESOURCE, "plain", "read");
    s.grant(RESOURCE, "victim", "write");
    let by = "\u{1b}[2J\u{202e}by-admin";
    let group = "\u{1b}[31m\u{202e}evil";
    rewrite_by_hand(&s.vm, "victim", group);
    rewrite_by_hand(&s.vm, &administrator().id, by);
    let Scenario { vm, db } = s;
    let s = Scenario {
        vm: blackbox::storage::terminate(vm).start(),
        db,
    };
    let machine = signed_in(&s.vm, &administrator());
    let words = admin_run(&machine, &["grants", "--resource", RESOURCE], false).ok_text();
    assert_no_raw_control(&words, "`vibe admin grants --resource`");
    assert!(words.contains("\\u{1b}"), "{}", words.escape_debug());
    let words = admin_run(&machine, &["grants", "--group", "plain"], false).ok_text();
    assert_no_raw_control(&words, "`vibe admin grants --group`");
    assert!(words.contains("\\u{1b}"), "{}", words.escape_debug());
    let run = admin_run(&machine, &["grants", "--resource", RESOURCE], true);
    assert_eq!(
        run.ok_json()["grants"][0]["group"],
        json!(group),
        "with --json the API's answer as it is"
    );
}
