//! Starting, from the ticket on the tool's sign-in through the browser on
//! (CONTRACT.md, "The tool signs in through the browser, and OIDC is
//! connected from the tool: Starting, from this ticket on" and "The
//! installation's key"; "Sign-in and sessions: Starting with OIDC" for what
//! stays): the options of `vm serve` for OIDC are gone, the program uses
//! the connection its platform database holds, the stand-in and
//! `--administrators-group` are kept to tests and the local run, the
//! installation's key and what happens without it or with another, the
//! listening line's `sign_in`, the bounds of the session settings, no
//! secret in any output, and a start that asks the identity provider
//! nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use blackbox::connection::{Connection, SecretKey, installation_of};
use blackbox::oidc::{Outage, Provider};
use blackbox::postgres::Platform;
use blackbox::program::{EXIT_TIMEOUT, Finished, run_to_exit, vm_binary};
use blackbox::sessions::{ADA, Installation, ada, oidc_options};
use blackbox::{DOMAIN, Vm, VmOptions};
use serde_json::json;

/// A client secret no other test uses, to look for where it must not be.
const SECRET: &str = "start-up-secret-Qm7Vt2Lx9";

const LISTEN: [&str; 2] = ["--listen", "127.0.0.1:0"];

/// Runs `vm serve --domain <domain> --listen 127.0.0.1:0 <args>` on this
/// platform database (a fresh one when `None`), with `VM_SECRET_KEY` set to
/// `key` or unset and `VM_OIDC_CLIENT_SECRET` set to [`SECRET`], to its
/// exit.
fn serve(platform: Option<&Platform>, args: &[&str], key: Option<&str>) -> Finished {
    let fresh;
    let platform = match platform {
        Some(p) => p,
        None => {
            fresh = Platform::fresh();
            &fresh
        }
    };
    let mut command = Command::new(vm_binary());
    command
        .args(["serve", "--domain", DOMAIN])
        .args(LISTEN)
        .args(args)
        .env_remove("VM_SECRET_KEY")
        .env("VM_OIDC_CLIENT_SECRET", SECRET)
        .env("VM_DATABASE_URL", platform.url());
    if let Some(key) = key {
        command.env("VM_SECRET_KEY", key);
    }
    run_to_exit(command, EXIT_TIMEOUT).unwrap()
}

/// Fails unless the run exited non-zero by itself within [`EXIT_TIMEOUT`],
/// without a `listening` record, naming each of `named`, and showing none
/// of `hidden`.
fn assert_start_fails(case: &str, finished: &Finished, named: &[&str], hidden: &[&str]) {
    let mut stderr = finished.stderr.clone();
    let mut stdout = finished.stdout.clone();
    for value in hidden.iter().filter(|v| !v.is_empty()) {
        stderr = stderr.replace(value, "<HIDDEN>");
        stdout = stdout.replace(value, "<HIDDEN>");
    }
    let shown = format!(
        "case: {case}\nexit: {}\nstandard error:\n{stderr}",
        finished.status_text()
    );
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected `vm serve` to exit non-zero by itself within {EXIT_TIMEOUT:?}\n{shown}"
    );
    assert!(
        !finished.stderr.contains("\"listening\""),
        "expected no `listening` record from a start-up that cannot work\n{shown}"
    );
    for name in named {
        assert!(
            finished.stderr.contains(name),
            "expected the message on standard error to name {name:?}\n{shown}"
        );
    }
    for value in hidden.iter().filter(|v| !v.is_empty()) {
        assert!(
            !finished.stderr.contains(value) && !finished.stdout.contains(value),
            "expected a value that must not be shown to be in no output of the program, but it is (shown as <HIDDEN>)\n{shown}\nstandard output:\n{stdout}"
        );
    }
}

#[test]
fn the_oidc_options_of_serve_are_gone_and_each_is_exit_code_2() {
    for (option, value) in [
        ("--oidc-issuer", "http://127.0.0.1:9"),
        ("--oidc-client-id", "vm-harness-client"),
        ("--oidc-groups-claim", "roles"),
        ("--oidc-scopes", "openid email"),
    ] {
        let finished = serve(None, &[option, value], Some(SecretKey::fresh().expose()));
        assert_eq!(
            finished.status.and_then(|s| s.code()),
            Some(2),
            "expected `vm serve {option} {value}` to be exit code 2, like any option it does not have\nexit: {}\nstandard error:\n{}",
            finished.status_text(),
            finished.stderr.replace(SECRET, "<SECRET>")
        );
        assert!(!finished.stderr.contains(SECRET) && !finished.stdout.contains(SECRET));
    }
}

#[test]
fn the_client_secret_in_the_environment_of_serve_is_not_looked_at() {
    for options in [
        VmOptions::new().stand_in_sign_in(),
        VmOptions::new().env("VM_SECRET_KEY", SecretKey::fresh().expose()),
    ] {
        let vm = Vm::start(&options.env("VM_OIDC_CLIENT_SECRET", SECRET));
        let listening = vm.listening();
        assert_ne!(
            listening["sign_in"], "oidc",
            "the secret alone connects nothing: {listening}"
        );
        assert!(
            vm.lines().iter().all(|l| !l.raw.contains(SECRET)),
            "expected the secret in no line of the log"
        );
    }
}

#[test]
fn at_its_start_the_program_uses_the_connection_the_platform_database_holds() {
    let at = Installation::start(&[("honest", "honest.js")]).restarted();
    assert_eq!(
        at.vm.listening()["sign_in"],
        "oidc",
        "{}",
        at.vm.listening()
    );
    assert_eq!(at.vm.listening()["stand_in_sign_in"], false);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}

#[test]
fn the_identity_provider_is_asked_nothing_at_start_up() {
    let at = Installation::start(&[]);
    let asked_before = at.provider.requests().len();
    let at = at.restarted();
    assert_eq!(at.vm.listening()["sign_in"], "oidc");
    std::thread::sleep(Duration::from_millis(500));
    let asked: Vec<String> = at
        .provider
        .requests()
        .iter()
        .skip(asked_before)
        .map(|r| r.path.clone())
        .collect();
    assert!(
        asked.is_empty(),
        "expected the program to ask the identity provider nothing at start-up, it asked {asked:?}"
    );
}

#[test]
fn the_program_starts_with_the_identity_provider_unreachable_or_not_answering() {
    let at = Installation::start(&[]);
    at.provider.set_outage(Outage::Hang);
    let began = Instant::now();
    let at = at.restarted();
    assert_eq!(at.vm.listening()["sign_in"], "oidc");
    at.provider.set_outage(Outage::None);
    let at = at.restarted_beside(Provider::start(&[]));
    assert_eq!(at.vm.listening()["sign_in"], "oidc");
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "expected both starts to be quick, they took {:?}",
        began.elapsed()
    );
}

#[test]
fn the_stand_in_on_a_platform_database_that_holds_a_connection_is_a_start_up_failure() {
    let at = Installation::start(&[]);
    let platform = at.vm.platform().clone();
    let key = at.key().unwrap();
    drop(at);
    // The same start without the stand-in works: what fails below is the
    // stand-in.
    let started = Vm::start(
        &VmOptions::new()
            .env("VM_SECRET_KEY", key.expose())
            .platform(&platform),
    );
    assert_eq!(started.listening()["sign_in"], "oidc");
    drop(started);
    let another = SecretKey::fresh();
    for (case, key) in [
        ("with the key", Some(key.expose())),
        ("with another key", Some(another.expose())),
        ("without a key", None),
    ] {
        let finished = serve(Some(&platform), &["--stand-in-sign-in"], key);
        assert_start_fails(
            &format!("--stand-in-sign-in on a platform database that holds a connection, {case}"),
            &finished,
            &["--stand-in-sign-in"],
            &[key.unwrap_or_default(), SECRET],
        );
    }
}

#[test]
fn the_administrators_group_without_the_stand_in_is_a_start_up_failure_naming_connect_oidc() {
    let key = SecretKey::fresh();
    // With the stand-in it starts: what fails below is the stand-in's
    // absence.
    let stand_in = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group("platform-admins"),
    );
    assert_eq!(stand_in.listening()["sign_in"], "stand-in");
    drop(stand_in);
    for key in [Some(key.expose()), None] {
        let finished = serve(None, &["--administrators-group", "platform-admins"], key);
        assert_start_fails(
            "--administrators-group without --stand-in-sign-in",
            &finished,
            &["--administrators-group", "vm admin connect-oidc"],
            &[key.unwrap_or_default()],
        );
    }
}

#[test]
fn a_malformed_installation_key_is_a_start_up_failure_that_never_shows_it() {
    let short = STANDARD.encode([7_u8; 31]);
    let long = STANDARD.encode([7_u8; 33]);
    let hex = "a1".repeat(32);
    for (case, value) in [
        ("empty", ""),
        ("not base64", "this is not base64 at all, not one bit!!"),
        ("31 bytes", short.as_str()),
        ("33 bytes", long.as_str()),
        ("64 hex digits", hex.as_str()),
    ] {
        let began = Instant::now();
        let finished = serve(None, &[], Some(value));
        assert_start_fails(
            &format!("VM_SECRET_KEY {case}"),
            &finished,
            &["VM_SECRET_KEY"],
            &[value],
        );
        assert!(began.elapsed() <= EXIT_TIMEOUT, "{case}: within 15 seconds");
    }
}

#[test]
fn a_key_of_32_random_bytes_in_base64_is_taken() {
    let vm = Vm::start(&VmOptions::new().env("VM_SECRET_KEY", SecretKey::fresh().expose()));
    assert_eq!(vm.listening()["sign_in"], "none", "{}", vm.listening());
}

#[test]
fn without_a_key_the_program_serves_all_the_same() {
    let vm = Vm::start(&VmOptions::new());
    assert_eq!(vm.listening()["sign_in"], "none", "{}", vm.listening());
    let answer = installation_of(&vm);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_eq!(answer.json()["sign_in"], "none");
}

#[test]
fn without_a_key_a_connection_in_the_platform_database_is_not_used() {
    let at = Installation::start(&[]).restarted_with_key(None);
    assert_eq!(
        at.vm.listening()["sign_in"],
        "none",
        "{}",
        at.vm.listening()
    );
    at.assert_logged(
        "oidc-connection-unusable",
        &[("reason", json!("no-secret-key"))],
    );
    let answer = installation_of(&at.vm);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_eq!(
        answer.json()["sign_in"],
        "none",
        "a connection that is not used is no sign-in: {}",
        answer.describe()
    );
}

#[test]
fn with_another_key_a_connection_is_not_used_until_oidc_is_connected_again() {
    let at = Installation::start(&[("honest", "honest.js")]);
    let first = at.key().unwrap();
    let at = at.restarted_with_key(Some(&SecretKey::fresh()));
    assert_eq!(
        at.vm.listening()["sign_in"],
        "none",
        "{}",
        at.vm.listening()
    );
    at.assert_logged(
        "oidc-connection-unusable",
        &[("reason", json!("another-secret-key"))],
    );
    assert!(
        at.vm
            .lines()
            .iter()
            .all(|l| !l.raw.contains(first.expose())),
        "expected neither key in any line of the log"
    );
    at.connect(&Connection::to(&at.provider));
    assert_eq!(installation_of(&at.vm).json()["sign_in"], "oidc");
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let at = at.restarted();
    assert_eq!(
        at.vm.listening()["sign_in"],
        "oidc",
        "connected again under the new key, the connection is used at the next start"
    );
}

#[test]
fn the_listening_line_says_oidc_stand_in_or_none() {
    let stand_in = Vm::start(&VmOptions::new().stand_in_sign_in());
    assert_eq!(
        stand_in.listening()["sign_in"],
        "stand-in",
        "{}",
        stand_in.listening()
    );
    let none = Vm::start(&oidc_options());
    assert_eq!(none.listening()["sign_in"], "none", "{}", none.listening());
    let at = Installation::start(&[]).restarted();
    assert_eq!(
        at.vm.listening()["sign_in"],
        "oidc",
        "{}",
        at.vm.listening()
    );
}

#[test]
fn the_stand_in_on_an_address_that_is_not_loopback_is_a_start_up_failure() {
    for listen in ["0.0.0.0:0", "[::]:0"] {
        let platform = Platform::fresh();
        let mut command = Command::new(vm_binary());
        command
            .args(["serve", "--domain", DOMAIN, "--listen", listen])
            .arg("--stand-in-sign-in")
            .env_remove("VM_SECRET_KEY")
            .env("VM_DATABASE_URL", platform.url());
        let finished = run_to_exit(command, EXIT_TIMEOUT).unwrap();
        assert_start_fails(
            &format!("--stand-in-sign-in with --listen {listen}"),
            &finished,
            &["--stand-in-sign-in"],
            &[],
        );
    }
}

#[test]
fn the_session_settings_outside_their_bounds_are_start_up_failures() {
    let key = SecretKey::fresh();
    for (flag, value) in [
        ("--session-renewal", "0"),
        ("--session-renewal", "86401"),
        ("--session-renewal", "ten"),
        ("--session-lifetime", "59"),
        ("--session-lifetime", "2592001"),
        ("--public-scheme", "ftp"),
    ] {
        let finished = serve(None, &[flag, value], Some(key.expose()));
        assert_start_fails(
            &format!("{flag} {value}"),
            &finished,
            &[flag],
            &[key.expose(), SECRET],
        );
    }
}

#[test]
fn the_session_settings_at_their_bounds_are_taken() {
    for more in [
        ["--session-renewal", "1", "--session-lifetime", "60"],
        [
            "--session-renewal",
            "86400",
            "--session-lifetime",
            "2592000",
        ],
    ] {
        let at = Installation::start_with(&[], &[ada()], &more).restarted();
        assert_eq!(at.vm.listening()["sign_in"], "oidc", "{more:?}");
    }
}

#[test]
fn the_client_secret_and_the_key_are_in_no_line_of_the_log_after_a_sign_in() {
    let installation = Installation::start(&[("honest", "honest.js")]);
    let mut browser = installation.browser_as(ADA);
    installation.sign_in(&mut browser, "honest", "/");
    installation.me(&mut browser).expect_status(200);
    let secret = installation.provider.client_secret();
    let mut hidden = installation.provider.issued();
    hidden.push(secret.expose().to_owned());
    hidden.push(installation.key().unwrap().expose().to_owned());
    installation.assert_none_in_log(&hidden, "after a sign-in");
}
