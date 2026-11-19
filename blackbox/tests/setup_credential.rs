//! The setup credential (CONTRACT.md, "The tool signs in through the
//! browser, and OIDC is connected from the tool: The setup credential"):
//! `vibe setup` prints it once, the platform database keeps only its digest,
//! it is good for one connection and nothing else, a second `vibe setup` ends
//! the first one's, and its failures are those of a start-up.
//!
//! Acceptance criterion: "With the setup credential, a Platform
//! administrator connects OIDC and names the administrators' Group from the
//! tool; afterwards the setup credential no longer works" (the API's side;
//! the tool's is in `oidc_connection.rs`).
//!
//! Not tested: that the credential stops working after 24 hours. Only its
//! `expires_at` is looked at; waiting a day is not a test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use blackbox::Vm;
use blackbox::api::parse_rfc3339;
use blackbox::connection::{
    Connection, SETUP_PREFIX, masked, put_oidc, run_setup, setup_credential, shown, with_bearer,
};
use blackbox::oidc::Provider;
use blackbox::postgres::Platform;
use blackbox::program::{EXIT_TIMEOUT, Finished};
use blackbox::sessions::{Installation, ada, oidc_options};
use blackbox::tool::{closed_address, platform_tables_holding};
use serde_json::Value;

const DAY: i64 = 24 * 60 * 60;

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn described(finished: &Finished) -> String {
    format!(
        "exit: {}\nstandard output:\n{}\nstandard error:\n{}",
        finished.status_text(),
        masked(&finished.stdout, &[]),
        masked(&finished.stderr, &[])
    )
}

/// A program with an installation's key and no connection, beside a
/// provider with Ada.
fn unconnected() -> Installation {
    Installation::unconnected(Provider::start(&[ada()]), &oidc_options())
}

#[test]
fn vm_setup_with_json_prints_the_setup_credential_and_when_it_expires() {
    let platform = Platform::fresh();
    let finished = run_setup(Some(platform.url()), &["--json"]);
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "expected `vibe setup --json` to end with 0\n{}",
        described(&finished)
    );
    assert!(
        finished.stdout.ends_with('\n') && !finished.stdout.ends_with("\n\n"),
        "expected one JSON object and a line end\n{}",
        described(&finished)
    );
    let printed: Value = serde_json::from_str(&finished.stdout)
        .unwrap_or_else(|e| panic!("expected one JSON object ({e})\n{}", described(&finished)));
    let mut keys: Vec<&String> = printed.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["expires_at", "setup_credential"],
        "{}",
        described(&finished)
    );
    let credential = printed["setup_credential"].as_str().unwrap();
    let rest = credential.strip_prefix(SETUP_PREFIX).unwrap_or_else(|| {
        panic!(
            "expected the setup credential to begin with {SETUP_PREFIX}\n{}",
            described(&finished)
        )
    });
    assert!(
        rest.len() >= 43
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+/=".contains(c)),
        "expected 32 random bytes or more as text after {SETUP_PREFIX} (43 characters of base64 or more), got {} characters",
        rest.len()
    );
    let expires = printed["expires_at"]
        .as_str()
        .and_then(parse_rfc3339)
        .unwrap_or_else(|| panic!("expected `expires_at` as an RFC 3339 time, got {printed:?}"));
    assert!(
        (expires - (now() + DAY)).abs() <= 300,
        "expected the setup credential to expire 24 hours from now, it says {}",
        printed["expires_at"]
    );
    let again = setup_credential(&platform);
    assert_ne!(
        again.expose(),
        credential,
        "two runs of `vibe setup` make two credentials"
    );
}

#[test]
fn without_json_the_credential_alone_is_on_standard_output_and_what_to_do_with_it_on_standard_error()
 {
    let platform = Platform::fresh();
    let finished = run_setup(Some(platform.url()), &[]);
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "{}",
        described(&finished)
    );
    let lines: Vec<&str> = finished.stdout.lines().collect();
    assert!(
        lines.len() == 1 && lines[0].starts_with(SETUP_PREFIX) && !lines[0].contains(' '),
        "expected the setup credential alone on standard output\n{}",
        described(&finished)
    );
    let credential = lines[0];
    assert!(
        !finished.stderr.trim().is_empty() && !finished.stderr.contains(credential),
        "expected what to do with it on standard error, without the credential\n{}",
        described(&finished)
    );
    assert!(
        finished.stderr.contains("connect-oidc"),
        "expected standard error to say to connect OIDC with `vibe admin connect-oidc`\n{}",
        described(&finished)
    );
}

#[test]
fn the_platform_database_keeps_only_the_credential_s_digest() {
    let platform = Platform::fresh();
    let credential = setup_credential(&platform);
    let holding = platform_tables_holding(&platform, credential.expose());
    assert!(
        holding.is_empty(),
        "expected the platform database to hold no form of the setup credential that can be used, but these tables hold it: {holding:?}"
    );
}

#[test]
fn vm_setup_works_whether_the_program_is_running_or_not() {
    // Never started: `vibe setup` makes the platform database and its
    // structure itself.
    let platform = Platform::fresh();
    let before = setup_credential(&platform);
    let provider = Provider::start(&[ada()]);
    let vm = Vm::start(&oidc_options().platform(&platform));
    let answer = put_oidc(
        &vm,
        Some(before.expose()),
        &Connection::to(&provider).body(),
    );
    assert_eq!(
        answer.status,
        200,
        "the credential of a `vibe setup` run before the program ever started\n{}",
        shown(&answer, &[provider.client_secret().expose()])
    );
    // Running: as every Installation of the harness connects.
    let at = unconnected();
    let while_running = setup_credential(at.vm.platform());
    let answer = put_oidc(
        &at.vm,
        Some(while_running.expose()),
        &Connection::to(&at.provider).body(),
    );
    assert_eq!(
        answer.status,
        200,
        "the credential of a `vibe setup` run while the program runs\n{}",
        shown(&answer, &[at.provider.client_secret().expose()])
    );
}

#[test]
fn the_setup_credential_is_good_for_connecting_only() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    for (method, target) in [
        ("GET", "/v1/me"),
        ("GET", "/v1/resources"),
        ("GET", "/v1/applications"),
        ("POST", "/v1/tool-credentials"),
        ("GET", "/v1/oidc"),
        ("GET", "/v1/operation-log"),
        ("POST", "/v1/applications"),
    ] {
        let answer = with_bearer(&at.vm, method, target, setup.expose())
            .json(&serde_json::json!({ "name": "made-with-setup" }))
            .send();
        assert_eq!(
            answer.status,
            401,
            "{method} {target} with the setup credential\n{}",
            shown(&answer, &[])
        );
    }
    let answer = put_oidc(
        &at.vm,
        Some(setup.expose()),
        &Connection::to(&at.provider).body(),
    );
    assert_eq!(
        answer.status,
        200,
        "the requests it was refused for did not use it up\n{}",
        shown(&answer, &[at.provider.client_secret().expose()])
    );
}

#[test]
fn the_setup_credential_connects_once_and_is_401_after() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let body = Connection::to(&at.provider).body();
    let secret = at.provider.client_secret();
    let first = put_oidc(&at.vm, Some(setup.expose()), &body);
    assert_eq!(first.status, 200, "{}", shown(&first, &[secret.expose()]));
    let again = put_oidc(&at.vm, Some(setup.expose()), &body);
    assert_eq!(
        again.status,
        401,
        "the setup credential is ended by the connection it made\n{}",
        shown(&again, &[secret.expose()])
    );
    let restarted = at.restarted();
    let after_a_start = put_oidc(&restarted.vm, Some(setup.expose()), &body);
    assert_eq!(
        after_a_start.status,
        401,
        "and stays ended when the program starts again\n{}",
        shown(&after_a_start, &[secret.expose()])
    );
}

#[test]
fn a_second_vm_setup_ends_the_first_one_s_credential() {
    let at = unconnected();
    let first = setup_credential(at.vm.platform());
    let second = setup_credential(at.vm.platform());
    let body = Connection::to(&at.provider).body();
    let secret = at.provider.client_secret();
    let with_first = put_oidc(&at.vm, Some(first.expose()), &body);
    assert_eq!(
        with_first.status,
        401,
        "the first credential, after a second `vibe setup`\n{}",
        shown(&with_first, &[secret.expose()])
    );
    let with_second = put_oidc(&at.vm, Some(second.expose()), &body);
    assert_eq!(
        with_second.status,
        200,
        "{}",
        shown(&with_second, &[secret.expose()])
    );
}

#[test]
fn vm_setup_is_the_way_back_in_after_a_connection() {
    let at = Installation::start(&[]);
    let again =
        at.connect_through_the_api(&Connection::to(&at.provider).administrators_group("finance"));
    assert_eq!(
        again.status,
        200,
        "a new `vibe setup` connects again over a connection\n{}",
        shown(&again, &[at.provider.client_secret().expose()])
    );
    assert_eq!(again.json()["administrators_group"], "finance");
}

#[test]
fn a_setup_credential_nobody_made_is_401() {
    let at = unconnected();
    let real = setup_credential(at.vm.platform());
    let made_up = format!("{SETUP_PREFIX}{}", "A".repeat(43));
    let mut altered = real.expose().to_owned();
    let last = altered.pop().unwrap();
    altered.push(if last == 'A' { 'B' } else { 'A' });
    let body = Connection::to(&at.provider).body();
    for (case, value) in [("made up", made_up.as_str()), ("altered", &altered)] {
        let answer = put_oidc(&at.vm, Some(value), &body);
        assert_eq!(
            answer.status,
            401,
            "{case}\n{}",
            shown(&answer, &[at.provider.client_secret().expose()])
        );
    }
    let none = put_oidc(&at.vm, None, &body);
    assert_eq!(none.status, 401, "not signed in at all");
}

#[test]
fn vm_setup_failures_are_those_of_a_start_up_and_never_show_a_password() {
    let password = "setup-password-Vx81Lq";
    let closed = closed_address();
    let unreachable = format!(
        "postgres://someone:{password}@127.0.0.1:{}/platform",
        closed.port()
    );
    for (case, url, named) in [
        ("no VIBE_DATABASE_URL", None, Some("VIBE_DATABASE_URL")),
        (
            "a VIBE_DATABASE_URL that cannot be parsed",
            Some("not a url"),
            Some("VIBE_DATABASE_URL"),
        ),
        (
            "a Postgres that cannot be reached",
            Some(unreachable.as_str()),
            None,
        ),
    ] {
        let began = Instant::now();
        let finished = run_setup(url, &["--json"]);
        let shown = described(&finished).replace(password, "<PASSWORD>");
        assert_eq!(
            finished.status.and_then(|s| s.code()),
            Some(1),
            "{case}: expected exit code 1 within {EXIT_TIMEOUT:?}\n{shown}"
        );
        assert!(began.elapsed() <= EXIT_TIMEOUT, "{case}: within 15 seconds");
        assert!(
            !finished.stderr.trim().is_empty() || !finished.stdout.trim().is_empty(),
            "{case}: a message naming the problem\n{shown}"
        );
        if let Some(named) = named {
            assert!(
                finished.stderr.contains(named) || finished.stdout.contains(named),
                "{case}: expected the message to name {named}\n{shown}"
            );
        }
        assert!(
            !finished.stderr.contains(password) && !finished.stdout.contains(password),
            "{case}: the password is never shown\n{shown}"
        );
        assert!(
            !finished.stdout.contains(SETUP_PREFIX),
            "{case}: no setup credential is made\n{shown}"
        );
    }
}

#[test]
fn a_setup_credential_does_not_work_on_another_installation() {
    let one = unconnected();
    let other = unconnected();
    let made_for_one = setup_credential(one.vm.platform());
    let answer = put_oidc(
        &other.vm,
        Some(made_for_one.expose()),
        &Connection::to(&other.provider).body(),
    );
    assert_eq!(
        answer.status,
        401,
        "{}",
        shown(&answer, &[other.provider.client_secret().expose()])
    );
}
