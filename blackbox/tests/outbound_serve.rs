//! `vibe serve --outbound-private <range>` (CONTRACT.md, "Outbound calls:
//! Who may call what", the bullet "What the operator of the machine may
//! open"): an address or a range in CIDR form, at most 16 times; the
//! program's `listening` record has `outbound_private` with the ranges as
//! given and `[]` without the option; a value that is not a range, or a
//! 17th, is a start-up failure that says why and names the option; `vibe
//! serve --help` names it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::dashboard::stand_in_options;
use blackbox::own_tls::{listened, serve_once};
use blackbox::program::run_vm_to_exit;
use blackbox::{Vm, VmOptions};
use serde_json::{Value, json};

/// The longest a start-up failure may take.
const WITHIN: Duration = Duration::from_secs(15);

fn started_with(ranges: &[&str]) -> Vm {
    let mut options: VmOptions = stand_in_options();
    for range in ranges {
        options = options.args_extra(&["--outbound-private", range]);
    }
    Vm::start(&options)
}

#[test]
fn without_the_option_the_listening_record_says_none() {
    let vm = started_with(&[]);
    assert_eq!(
        vm.listening()["outbound_private"],
        json!([]),
        "expected `outbound_private` to be `[]` without the option, got {}",
        vm.listening()
    );
}

#[test]
fn an_address_a_range_and_ipv6_are_taken_and_listed_as_given() {
    let ranges = [
        "10.9.0.0/24",
        "10.9.1.5",
        "192.168.0.0/16",
        "169.254.169.254",
        "127.0.0.1",
        "fd00::/8",
        "fd12:3456::1",
        "::1",
    ];
    let vm = started_with(&ranges);
    assert_eq!(
        vm.listening()["outbound_private"],
        json!(ranges),
        "expected the ranges as given, in the order given, got {}",
        vm.listening()
    );
}

#[test]
fn sixteen_times_starts_and_the_record_has_all_sixteen() {
    let ranges: Vec<String> = (0..16).map(|n| format!("10.{n}.0.0/16")).collect();
    let refs: Vec<&str> = ranges.iter().map(String::as_str).collect();
    let vm = started_with(&refs);
    let got: Vec<&str> = vm.listening()["outbound_private"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        got,
        refs,
        "expected all 16 in `outbound_private`, got {}",
        vm.listening()
    );
}

#[test]
fn the_option_may_be_written_with_an_equals_sign() {
    let options = stand_in_options().args_extra(&["--outbound-private=10.9.0.0/24"]);
    let vm = Vm::start(&options);
    assert_eq!(vm.listening()["outbound_private"], json!(["10.9.0.0/24"]));
}

/// Fails unless the program, given these extra arguments, ends with an
/// exit code that is not `0` within [`WITHIN`], without a `listening`
/// record, saying on standard error why in words that name the option.
fn assert_refused(what: &str, more: &[&str]) {
    let (finished, took) = serve_once(more, &[]);
    let shown = format!(
        "{what}\nexit: {} after {took:?}\nstandard output:\n{}\nstandard error:\n{}",
        finished.status_text(),
        finished.stdout,
        finished.stderr
    );
    assert!(
        finished.status.is_some_and(|s| !s.success()) && took < WITHIN,
        "expected a start-up failure (exit code not 0) within {WITHIN:?}\n{shown}"
    );
    assert!(
        !listened(&finished),
        "expected no `listening` record\n{shown}"
    );
    assert!(
        finished.stderr.contains("outbound-private"),
        "expected the message on standard error to name the option `--outbound-private`\n{shown}"
    );
    assert!(
        finished.stderr.trim().len() > "--outbound-private".len() + 10,
        "expected a message that says why, not the option's name alone\n{shown}"
    );
}

#[test]
fn a_value_that_is_not_a_range_is_a_start_up_failure_naming_the_option() {
    // The option exists and takes a range: it is the value that is refused.
    drop(started_with(&["10.9.0.0/24"]));
    for bad in [
        "10.0.0.0/33",
        "10.0.0.0/",
        "10.0.0.0/-1",
        "10.0.0.0/8/8",
        "10.0.0/8",
        "300.0.0.0/8",
        "::/129",
        "banana",
        "example.com",
        "api.example.com/24",
        "http://10.0.0.1",
        "10.0.0.0 /8",
        "",
    ] {
        assert_refused(
            &format!("--outbound-private {bad:?}"),
            &["--outbound-private", bad],
        );
    }
}

#[test]
fn one_bad_value_among_good_ones_refuses_the_start() {
    drop(started_with(&["10.9.0.0/24", "fd00::/8"]));
    assert_refused(
        "a good, a bad, a good",
        &[
            "--outbound-private",
            "10.9.0.0/24",
            "--outbound-private",
            "banana",
            "--outbound-private",
            "fd00::/8",
        ],
    );
}

#[test]
fn a_17th_is_a_start_up_failure_naming_the_option() {
    // Sixteen are taken: it is the 17th that is refused.
    let sixteen: Vec<String> = (0..16).map(|n| format!("10.{n}.0.0/16")).collect();
    let refs: Vec<&str> = sixteen.iter().map(String::as_str).collect();
    drop(started_with(&refs));
    let ranges: Vec<String> = (0..17).map(|n| format!("10.{n}.0.0/16")).collect();
    let mut more: Vec<&str> = Vec::new();
    for range in &ranges {
        more.push("--outbound-private");
        more.push(range);
    }
    assert_refused("17 times", &more);
}

#[test]
fn serve_help_names_the_option() {
    let finished = run_vm_to_exit(["serve", "--help"], None);
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "expected `vibe serve --help` to end with 0, got {}\n{}",
        finished.status_text(),
        finished.stderr
    );
    assert!(
        finished.stdout.contains("--outbound-private"),
        "expected `vibe serve --help` to name `--outbound-private`, got\n{}",
        finished.stdout
    );
}
