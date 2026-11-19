//! What every command of the tool does (CONTRACT.md, "The tool: Every
//! command"; ticket "The tool signs in, and lists Resources and
//! Applications": every command has a JSON form, exit codes separate
//! failure from refused-or-waiting, and the help says which is which).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::scenario::Scenario;
use blackbox::tool::{Broken, Machine, Run, closed_address, sign_in_args};
use blackbox::{DOMAIN, User, VmOptions};

fn alice() -> User {
    User::new("alice").in_groups(&["readers"])
}

/// The commands that need a credential.
const SIGNED_IN_COMMANDS: [&str; 3] = ["whoami", "resources", "applications"];

/// Whether `line` holds `n` as a number standing alone.
fn holds_number(line: &str, n: u32) -> bool {
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == n.to_string())
}

#[test]
fn help_lists_the_five_exit_codes_and_what_each_means() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let text = format!("{}\n{}", help.stdout, help.stderr).to_lowercase();
    for (code, meaning) in [
        (0, "done"),
        (1, "fail"),
        (2, "command line"),
        (3, "refused"),
        (4, "not signed in"),
    ] {
        assert!(
            text.lines()
                .any(|line| holds_number(line, code) && line.contains(meaning)),
            "expected `vibe --help` to list the exit code {code} with its meaning ({meaning:?}) on one line, got\n{}",
            help.describe()
        );
    }
    assert!(
        text.lines()
            .any(|l| holds_number(l, 3) && l.contains("wait")),
        "expected the exit code 3 to say it is also for waiting for the Builder, got\n{}",
        help.describe()
    );
}

#[test]
fn every_command_with_json_prints_one_object_and_a_line_end() {
    let s = Scenario::start();
    let machine = Machine::new();
    let mut sign_in: Vec<String> = sign_in_args(s.vm.address(), &alice());
    sign_in.push("--json".to_owned());
    let sign_in: Vec<&str> = sign_in.iter().map(String::as_str).collect();
    machine.run(&sign_in).ok_json();
    for command in SIGNED_IN_COMMANDS {
        machine.run(&[command, "--json"]).ok_json();
    }
    machine.run(&["sign-out", "--json"]).ok_json();
    // Nothing to sign out of: still `0`, still one object.
    machine.run(&["sign-out", "--json"]).ok_json();
    for command in SIGNED_IN_COMMANDS {
        machine
            .run(&[command, "--json"])
            .json_error(4, "not-signed-in");
    }
}

#[test]
fn without_json_results_are_text_on_standard_output_and_errors_on_standard_error() {
    let s = Scenario::start();
    let machine = Machine::new();
    let sign_in: Vec<String> = sign_in_args(s.vm.address(), &alice());
    let sign_in: Vec<&str> = sign_in.iter().map(String::as_str).collect();
    let printed = machine.run(&sign_in).ok_text();
    assert!(
        serde_json::from_str::<serde_json::Value>(&printed).is_err(),
        "expected words without --json, got JSON:\n{printed}"
    );
    for command in SIGNED_IN_COMMANDS {
        let text = machine.run(&[command]).ok_text();
        assert!(
            serde_json::from_str::<serde_json::Value>(&text).is_err(),
            "expected words from `vibe {command}` without --json, got JSON:\n{text}"
        );
    }
    machine.run(&["sign-out"]).ok_text();
    for command in SIGNED_IN_COMMANDS {
        let message = machine.run(&[command]).text_error(4);
        assert!(
            message.contains("vibe sign-in"),
            "expected the message to say to run `vibe sign-in`, got {message:?}"
        );
    }
}

/// Fails unless the run ended with `2`, said why on standard error, and
/// with `--json` printed at most the error object on standard output.
fn assert_wrong_command_line(run: &Run) {
    run.expect_code(2);
    assert!(
        !run.stderr.trim().is_empty(),
        "expected the usage on standard error, got\n{}",
        run.describe()
    );
    if !run.stdout.trim().is_empty() {
        let body = run.json();
        assert!(
            body["error"]["reason"].is_string(),
            "expected nothing on standard output, or the error object, got\n{}",
            run.describe()
        );
    }
}

#[test]
fn a_wrong_command_line_ends_with_2_and_keeps_nothing() {
    let s = Scenario::start();
    let machine = Machine::new();
    let address = s.vm.address().to_string();
    let port = s.vm.address().port();
    let mut cases: Vec<Vec<String>> = vec![
        vec!["no-such-command".into()],
        vec!["whoami".into(), "--no-such-option".into()],
        vec!["resources".into(), "extra".into()],
        // (No User is no longer a wrong command line: the tool asks the
        // installation how it signs in first, and the stand-in's answer
        // without `--stand-in-user` is `sign-in-refused`. See
        // `tool_browser_sign_in.rs`.)
        // No domain.
        vec![
            "sign-in".into(),
            "--connect".into(),
            address,
            "--stand-in-user".into(),
            "alice".into(),
        ],
        // No address.
        vec![
            "sign-in".into(),
            "--domain".into(),
            DOMAIN.into(),
            "--stand-in-user".into(),
            "alice".into(),
        ],
    ];
    for connect in [
        "10.1.2.3:8080".to_owned(),
        "192.168.0.1:80".to_owned(),
        format!("0.0.0.0:{port}"),
        "8.8.8.8:443".to_owned(),
        "example.com:80".to_owned(),
        "not an address".to_owned(),
        "127.0.0.1".to_owned(),
        format!("[2001:db8::1]:{port}"),
    ] {
        cases.push(vec![
            "sign-in".into(),
            "--domain".into(),
            DOMAIN.into(),
            "--connect".into(),
            connect,
            "--stand-in-user".into(),
            "alice".into(),
        ]);
    }
    for case in cases {
        for json in [false, true] {
            let mut args: Vec<&str> = case.iter().map(String::as_str).collect();
            if json {
                args.push("--json");
            }
            assert_wrong_command_line(&machine.run(&args));
        }
    }
    assert!(
        machine.all_files().is_empty(),
        "expected nothing kept on the machine, found {:?}",
        machine.all_files()
    );
    // The control: the same command line made right is not refused, so it
    // is the wrong part that made each of those `2`.
    machine.sign_in(&s.vm, &alice());
    machine.run(&["whoami", "--json"]).ok_json();
}

#[test]
fn a_loopback_address_is_a_right_command_line() {
    let machine = Machine::new();
    for address in [
        closed_address().to_string(),
        format!("[::1]:{}", closed_address().port()),
    ] {
        let run = machine.run(&[
            "sign-in",
            "--domain",
            DOMAIN,
            "--connect",
            &address,
            "--stand-in-user",
            "alice",
            "--json",
        ]);
        run.json_error(1, "unreachable");
    }
}

/// Fails unless the run with a broken stream ended as it would have
/// (`want`) or with `1`, and without a crash.
fn assert_no_crash(run: &Run, want: i32, broken: Broken) {
    assert!(
        run.code == Some(want) || run.code == Some(1),
        "expected exit code {want} (what the command ends with) or 1 with {broken:?}, got\n{}",
        run.describe()
    );
    for crash in ["panicked", "RUST_BACKTRACE", "stack backtrace"] {
        assert!(
            !run.stderr.contains(crash),
            "expected no crash with {broken:?}, but standard error holds {crash:?}:\n{}",
            run.describe()
        );
    }
}

#[test]
fn a_command_whose_standard_output_is_closed_ends_without_a_crash() {
    let s = Scenario::start();
    s.register("devops");
    let machine = Machine::new();
    machine.sign_in(&s.vm, &alice());
    let commands = [
        vec!["resources"],
        vec!["resources", "--json"],
        vec!["whoami"],
        vec!["whoami", "--json"],
    ];
    for broken in [Broken::StdoutClosed, Broken::StdoutReaderGone] {
        for args in &commands {
            assert_no_crash(&machine.run_broken(args, broken), 0, broken);
        }
    }
    // The control: the credential is still there and works.
    machine.run(&["whoami", "--json"]).ok_json();

    // A command that ends with 4 keeps its 4, or 1.
    let nobody = Machine::new();
    for broken in [Broken::StdoutClosed, Broken::StdoutReaderGone] {
        for args in &commands {
            assert_no_crash(&nobody.run_broken(args, broken), 4, broken);
        }
    }
}

#[test]
fn a_command_whose_standard_error_is_closed_ends_without_a_crash() {
    let nobody = Machine::new();
    let unreachable = Machine::new();
    let owned = sign_in_args(closed_address(), &alice());
    let sign_in: Vec<&str> = owned.iter().map(String::as_str).collect();
    for broken in [Broken::StderrClosed, Broken::BothClosed] {
        // Not signed in: the message goes to standard error.
        for args in [vec!["whoami"], vec!["resources"], vec!["whoami", "--json"]] {
            assert_no_crash(&nobody.run_broken(&args, broken), 4, broken);
        }
        // Unreachable.
        assert_no_crash(&unreachable.run_broken(&sign_in, broken), 1, broken);
        // A wrong command line writes its usage there.
        assert_no_crash(&nobody.run_broken(&["no-such-command"], broken), 2, broken);
    }
    // The control: with nothing broken these end as said.
    nobody.run(&["whoami"]).text_error(4);
    unreachable.run(&sign_in).text_error(1);
}

#[test]
fn an_answer_of_5xx_keeps_the_apis_reason_and_ends_with_1() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_tool_5xx");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    let machine = Machine::new();
    machine.sign_in(&s.vm, &alice());
    machine.run(&["applications", "--json"]).ok_json();
    postgres.stop();
    for command in SIGNED_IN_COMMANDS {
        let message = machine
            .run(&[command, "--json"])
            .json_error(1, "unavailable");
        assert!(
            !message.contains("vibe sign-in"),
            "expected a signed-in Builder not to be told to sign in again while the platform database is down, got {message:?}"
        );
        machine.run(&[command]).text_error(1);
    }
}
