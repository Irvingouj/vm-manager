//! `scripts/gate-hosted.sh`, the gate as a hosted runner runs it
//! (CONTRACT.md, "The first release: The scripts that make it"): it runs
//! the whole gate once (`scripts/gate.sh --no-fail-fast`), then each test
//! file that failed once more, alone and one test at a time (`cargo test
//! --locked <what cargo names for that file> -- --test-threads=1`, taken
//! from cargo's lines "to rerun pass ..." and "N targets failed"); it ends
//! with 0 only when the steps before the tests passed, npm's tests passed,
//! and every test file passed the first time or alone; and it prints which
//! files it ran again and how each ended.
//!
//! Driven through the seam of [`GateStandIns`]: the repository's real
//! scripts, from its root, with `cargo` and `npm` stand-ins first on the
//! `PATH` (and no `CARGO` set) that record every call and answer as each
//! case says, never a real gate. Cargo's lines are its own wording, taken
//! from a real failing run of cargo 1.99 on a scratch workspace.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::release::{GateStandIns, Ran, Rule};

/// A gate of stand-ins ends in seconds; one that does not is taken to
/// hang.
const GATE_TIMEOUT: Duration = Duration::from_secs(60);

/// The whole run of the tests, as `scripts/gate.sh --no-fail-fast` asks
/// cargo for it.
const WHOLE: &str = "test --workspace";

/// Cargo's line for one target that failed.
fn failed(target: &str) -> String {
    format!("error: test failed, to rerun pass `{target}`\n")
}

/// Cargo's line for a doctest target that failed.
fn doctest_failed(target: &str) -> String {
    format!("error: doctest failed, to rerun pass `{target}`\n")
}

/// Cargo's summary of the targets that failed, with `--no-fail-fast`.
fn summary(targets: &[&str]) -> String {
    let n = targets.len();
    let mut text = format!(
        "error: {n} target{} failed:\n",
        if n == 1 { "" } else { "s" }
    );
    for t in targets {
        text.push_str(&format!("    `{t}`\n"));
    }
    text
}

/// The call that runs `target` again, as the contract writes it.
fn again(target: &str) -> String {
    format!("test --locked {target} -- --test-threads=1")
}

/// A rule for running `target` again: it ends with `code`.
fn alone(target: &str, code: i32) -> Rule {
    let said = if code == 0 {
        String::new()
    } else {
        failed(target)
    };
    Rule::new(&again(target), code, &said)
}

/// Runs `scripts/gate-hosted.sh` with these stand-ins.
fn gate_hosted(cargo: &[Rule], npm: &[Rule]) -> (GateStandIns, Ran) {
    let stand_ins = GateStandIns::new(cargo, npm);
    let ran = stand_ins.run("gate-hosted.sh", &[], GATE_TIMEOUT);
    (stand_ins, ran)
}

/// The calls of cargo that ran tests again: `cargo test` but the whole
/// run.
fn reruns(stand_ins: &GateStandIns) -> Vec<String> {
    stand_ins
        .calls("cargo")
        .into_iter()
        .filter(|c| c.starts_with("test ") && !c.starts_with(WHOLE))
        .collect()
}

/// Fails unless the calls that ran tests again are exactly one of each of
/// `targets`, in the contract's form.
fn assert_ran_again(stand_ins: &GateStandIns, ran: &Ran, targets: &[&str]) {
    let mut sorted = reruns(stand_ins);
    sorted.sort();
    let mut expected: Vec<String> = targets.iter().map(|t| again(t)).collect();
    expected.sort();
    assert_eq!(
        sorted,
        expected,
        "expected exactly these test files run again, each once, alone and one test at a time (`cargo test --locked <target> -- --test-threads=1`); cargo was called as {:?}\n{}",
        stand_ins.calls("cargo"),
        ran.describe()
    );
}

/// The lines of the script's output that are its own: not cargo's lines
/// the stand-ins printed.
fn own_lines(ran: &Ran) -> Vec<String> {
    ran.said()
        .lines()
        .filter(|l| {
            !l.contains("to rerun pass")
                && !l.contains("failed:")
                && !l.trim_start().starts_with('`')
        })
        .map(str::to_owned)
        .collect()
}

/// Whether a line of the script's own names `target` and holds `word`.
fn says(ran: &Ran, target: &str, word: &str) -> bool {
    own_lines(ran)
        .iter()
        .any(|l| l.contains(target) && l.to_lowercase().contains(word))
}

fn assert_code(ran: &Ran, zero: bool, why: &str) {
    if zero {
        assert!(ran.ok(), "expected 0: {why}\n{}", ran.describe());
    } else {
        assert!(
            ran.code.is_some_and(|c| c != 0),
            "expected an exit code other than 0: {why}\n{}",
            ran.describe()
        );
    }
}

#[test]
fn when_everything_passes_the_first_time_it_ends_with_0_and_runs_nothing_again() {
    let (stand_ins, ran) = gate_hosted(&[], &[]);
    assert_code(&ran, true, "every step passed");
    let cargo = stand_ins.calls("cargo");
    for step in [
        "fmt --all --check",
        "clippy --workspace",
        "test --workspace",
    ] {
        assert!(
            cargo.iter().any(|c| c.starts_with(step)),
            "expected the whole gate run once (cargo {step}...); cargo was called as {cargo:?}"
        );
    }
    assert!(
        cargo
            .iter()
            .any(|c| c.starts_with(WHOLE) && c.split_whitespace().any(|w| w == "--no-fail-fast")),
        "expected the gate run with --no-fail-fast; cargo was called as {cargo:?}"
    );
    assert!(
        stand_ins.calls("npm").iter().any(|c| c == "test"),
        "expected npm's tests run"
    );
    assert_ran_again(&stand_ins, &ran, &[]);
}

#[test]
fn a_file_that_fails_side_by_side_and_passes_alone_is_run_again_and_the_gate_passes() {
    let target = "-p blackbox --test a";
    let (stand_ins, ran) = gate_hosted(
        &[alone(target, 0), Rule::new(WHOLE, 101, &failed(target))],
        &[],
    );
    assert_code(&ran, true, "the one file that failed passed alone");
    assert_ran_again(&stand_ins, &ran, &[target]);
    assert!(
        says(&ran, "--test a", "passed"),
        "expected the script to say that it ran {target} again and that it passed\n{}",
        ran.describe()
    );
}

#[test]
fn several_files_are_run_again_and_one_that_fails_twice_fails_the_gate_naming_it() {
    let targets = [
        "-p blackbox --test a",
        "-p blackbox --test b",
        "-p authz --lib",
    ];
    let whole = format!(
        "{}{}{}{}",
        failed(targets[0]),
        failed(targets[1]),
        failed(targets[2]),
        summary(&targets)
    );
    let (stand_ins, ran) = gate_hosted(
        &[
            alone(targets[0], 0),
            alone(targets[1], 101),
            alone(targets[2], 0),
            Rule::new(WHOLE, 101, &whole),
        ],
        &[],
    );
    assert_code(
        &ran,
        false,
        "-p blackbox --test b failed side by side and alone",
    );
    assert_ran_again(&stand_ins, &ran, &targets);
    assert!(
        says(&ran, "--test b", "fail"),
        "expected the script to name the file that failed twice, -p blackbox --test b\n{}",
        ran.describe()
    );
    assert!(
        !says(&ran, "--test a", "fail"),
        "expected the script not to say that -p blackbox --test a, which passed alone, failed\n{}",
        ran.describe()
    );
}

#[test]
fn a_library_a_binary_and_doctests_are_run_again_with_cargos_own_arguments() {
    let targets = ["-p authz --lib", "-p vibe --bin vibe", "-p sdk --doc"];
    let whole = format!(
        "{}{}{}{}",
        failed(targets[0]),
        failed(targets[1]),
        doctest_failed(targets[2]),
        summary(&targets)
    );
    let mut cargo: Vec<Rule> = targets.iter().map(|t| alone(t, 0)).collect();
    cargo.push(Rule::new(WHOLE, 101, &whole));
    let (stand_ins, ran) = gate_hosted(&cargo, &[]);
    assert_code(&ran, true, "each passed alone");
    assert_ran_again(&stand_ins, &ran, &targets);
}

#[test]
fn a_file_named_twice_by_cargo_is_run_again_once() {
    let targets = ["-p blackbox --test a", "-p blackbox --test b"];
    let whole = format!(
        "{}{}{}",
        failed(targets[0]),
        failed(targets[1]),
        summary(&targets)
    );
    let (stand_ins, ran) = gate_hosted(
        &[
            alone(targets[0], 0),
            alone(targets[1], 0),
            Rule::new(WHOLE, 101, &whole),
        ],
        &[],
    );
    assert_code(&ran, true, "both passed alone");
    assert_ran_again(&stand_ins, &ran, &targets);
}

#[test]
fn when_cargo_fmt_or_clippy_fails_the_gate_fails_and_no_test_runs() {
    for step in ["fmt", "clippy"] {
        let (stand_ins, ran) = gate_hosted(
            &[Rule::new(
                step,
                1,
                &format!("the stand-in's {step} failed\n"),
            )],
            &[],
        );
        assert_code(&ran, false, &format!("cargo {step} failed"));
        let tests: Vec<String> = stand_ins
            .calls("cargo")
            .into_iter()
            .filter(|c| c.starts_with("test"))
            .collect();
        assert!(
            tests.is_empty(),
            "expected no test run, the first time or again, once cargo {step} failed; cargo ran {tests:?}\n{}",
            ran.describe()
        );
    }
}

#[test]
fn when_npms_tests_fail_and_every_cargo_test_passes_the_gate_fails() {
    let (stand_ins, ran) = gate_hosted(&[], &[Rule::new("test", 1, "npm test failed\n")]);
    assert_code(&ran, false, "npm's tests failed");
    assert_ran_again(&stand_ins, &ran, &[]);
}

#[test]
fn when_npms_tests_fail_a_file_that_passes_alone_does_not_make_the_gate_pass() {
    let target = "-p blackbox --test a";
    let (stand_ins, ran) = gate_hosted(
        &[alone(target, 0), Rule::new(WHOLE, 101, &failed(target))],
        &[Rule::new("test", 1, "npm test failed\n")],
    );
    assert_code(
        &ran,
        false,
        "npm's tests failed, whatever the file run again did",
    );
    assert_ran_again(&stand_ins, &ran, &[target]);
}

#[test]
fn when_cargo_test_fails_naming_no_file_the_gate_fails_and_nothing_is_run_again() {
    let (stand_ins, ran) = gate_hosted(
        &[Rule::new(
            WHOLE,
            101,
            "error: this file contains an unclosed delimiter\nerror: could not compile `blackbox` (test \"a\") due to 1 previous error\n",
        )],
        &[],
    );
    assert_code(&ran, false, "cargo test failed with no test file named");
    assert_ran_again(&stand_ins, &ran, &[]);
}

/// The harness's own check of the stand-ins: they record each call and
/// answer by the first rule whose prefix matches.
#[test]
fn the_stand_ins_record_their_calls_and_answer_by_their_rules() {
    let stand_ins = GateStandIns::new(
        &[
            Rule::new("test --locked -p x", 7, "seven\n"),
            Rule::new("test", 3, "three\n"),
        ],
        &[],
    );
    let folder = blackbox::release::Folder::new("stand-in-check");
    let probe = folder.path().join("probe.sh");
    blackbox::program::write_program(
        &probe,
        b"#!/bin/sh\ncargo test --locked -p x --lib; echo \"a=$?\"\ncargo test --workspace; echo \"b=$?\"\ncargo fmt --all; echo \"c=$?\"\nnpm test; echo \"d=$?\"\n",
    )
    .unwrap();
    let mut command = std::process::Command::new(&probe);
    let search = format!(
        "{}:{}",
        stand_ins.bin().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    command.env("PATH", search);
    let ran = blackbox::release::run(command, GATE_TIMEOUT);
    assert!(
        ran.stdout.contains("a=7") && ran.stdout.contains("b=3"),
        "{}",
        ran.describe()
    );
    assert!(
        ran.stdout.contains("c=0") && ran.stdout.contains("d=0"),
        "{}",
        ran.describe()
    );
    assert!(ran.stderr.contains("seven") && ran.stderr.contains("three"));
    assert_eq!(
        stand_ins.calls("cargo"),
        vec![
            "test --locked -p x --lib".to_owned(),
            "test --workspace".to_owned(),
            "fmt --all".to_owned()
        ]
    );
    assert_eq!(stand_ins.calls("npm"), vec!["test".to_owned()]);
}
