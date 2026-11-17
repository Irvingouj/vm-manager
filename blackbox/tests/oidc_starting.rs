//! Starting with OIDC (CONTRACT.md, "Sign-in and sessions: Starting with
//! OIDC"): the three settings needed together, the issuer's address, the
//! stand-in kept to tests and the local run, the listening line's
//! `sign_in`, the bounds of the session settings, the secret in no output,
//! and a start that asks the identity provider nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::process::Command;
use std::time::{Duration, Instant};

use blackbox::oidc::{CLIENT_ID, Outage, Provider};
use blackbox::postgres::Platform;
use blackbox::program::{EXIT_TIMEOUT, Finished, run_to_exit, vm_binary};
use blackbox::sessions::{Installation, ada, oidc_options};
use blackbox::{DOMAIN, Vm, VmOptions};

/// A secret no other test uses, to look for where it must not be.
const SECRET: &str = "start-up-secret-Qm7Vt2Lx9";

/// Runs `vm serve --domain <domain> <args>` on a fresh platform database,
/// with `VM_OIDC_CLIENT_SECRET` set to `secret` or unset, to its exit.
fn serve(args: &[&str], secret: Option<&str>) -> Finished {
    let platform = Platform::fresh();
    let mut command = Command::new(vm_binary());
    command
        .args(["serve", "--domain", DOMAIN])
        .args(args)
        .env_remove("VM_OIDC_CLIENT_SECRET")
        .env("VM_DATABASE_URL", platform.url());
    if let Some(secret) = secret {
        command.env("VM_OIDC_CLIENT_SECRET", secret);
    }
    run_to_exit(command, EXIT_TIMEOUT).unwrap()
}

/// Fails unless `vm serve <args>` exits non-zero by itself, without a
/// `listening` record, naming each of `named`, showing the secret nowhere.
fn assert_start_fails(case: &str, args: &[&str], secret: Option<&str>, named: &[&str]) {
    let began = Instant::now();
    let finished = serve(args, secret);
    let shown = format!(
        "case: {case}\nargs: {args:?}\nexit: {} after {:?}\nstandard error:\n{}",
        finished.status_text(),
        began.elapsed(),
        finished.stderr.replace(SECRET, "<SECRET>")
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
    assert!(
        !finished.stderr.contains(SECRET) && !finished.stdout.contains(SECRET),
        "expected the client secret never to be shown, but it is in the program's output\n{shown}"
    );
}

/// (what the case is, the arguments, the secret, what must be named).
type Case<'a> = (&'a str, Vec<&'a str>, Option<&'a str>, Vec<&'a str>);

const LISTEN: [&str; 2] = ["--listen", "127.0.0.1:0"];

fn with_listen<'a>(args: &[&'a str]) -> Vec<&'a str> {
    LISTEN.iter().chain(args).copied().collect()
}

#[test]
fn the_three_oidc_settings_are_needed_together_and_the_missing_ones_are_named() {
    let issuer = "http://127.0.0.1:9";
    let cases: [Case<'_>; 6] = [
        (
            "no secret",
            vec!["--oidc-issuer", issuer, "--oidc-client-id", CLIENT_ID],
            None,
            vec!["VM_OIDC_CLIENT_SECRET"],
        ),
        (
            "no client id",
            vec!["--oidc-issuer", issuer],
            Some(SECRET),
            vec!["--oidc-client-id"],
        ),
        (
            "no issuer",
            vec!["--oidc-client-id", CLIENT_ID],
            Some(SECRET),
            vec!["--oidc-issuer"],
        ),
        (
            "the issuer alone",
            vec!["--oidc-issuer", issuer],
            None,
            vec!["--oidc-client-id", "VM_OIDC_CLIENT_SECRET"],
        ),
        (
            "the client id alone",
            vec!["--oidc-client-id", CLIENT_ID],
            None,
            vec!["--oidc-issuer", "VM_OIDC_CLIENT_SECRET"],
        ),
        (
            "the secret alone",
            vec![],
            Some(SECRET),
            vec!["--oidc-issuer", "--oidc-client-id"],
        ),
    ];
    for (case, args, secret, named) in &cases {
        assert_start_fails(case, &with_listen(args), *secret, named);
    }
}

#[test]
fn an_issuer_that_is_not_https_nor_loopback_http_is_a_start_up_failure() {
    // The same start with a loopback issuer works: what fails below is the
    // issuer.
    assert_eq!(
        start_with_issuer("http://127.0.0.1:9", &[]).listening()["sign_in"],
        "oidc"
    );
    for issuer in [
        "http://idp.example.com",
        "http://10.0.0.7:8080",
        "http://192.168.1.20",
        "http://127.0.0.1.nip.io",
        "http://localhost.example.com",
        "ftp://127.0.0.1/",
        "not a url",
        "https://",
        "",
    ] {
        assert_start_fails(
            &format!("issuer {issuer:?}"),
            &with_listen(&["--oidc-issuer", issuer, "--oidc-client-id", CLIENT_ID]),
            Some(SECRET),
            &["--oidc-issuer"],
        );
    }
}

/// Starts the program with OIDC against this issuer (the client id and a
/// secret given) and these more arguments.
fn start_with_issuer(issuer: &str, more: &[&str]) -> Vm {
    Vm::start(
        &VmOptions::new()
            .args_extra(&["--oidc-issuer", issuer, "--oidc-client-id", CLIENT_ID])
            .args_extra(more)
            .env("VM_OIDC_CLIENT_SECRET", SECRET),
    )
}

#[test]
fn https_and_loopback_http_issuers_are_taken_and_the_listening_line_says_oidc() {
    for issuer in [
        "https://idp.example.com",
        "https://idp.example.com/realms/staff",
        "http://127.0.0.1:9",
        "http://localhost:9",
        "http://[::1]:9",
    ] {
        let vm = start_with_issuer(issuer, &[]);
        let listening = vm.listening();
        assert_eq!(
            listening["sign_in"], "oidc",
            "issuer {issuer}: expected `sign_in: oidc` in {listening}"
        );
        assert_eq!(
            listening["stand_in_sign_in"], false,
            "issuer {issuer}: expected `stand_in_sign_in: false` in {listening}"
        );
        assert!(
            vm.lines().iter().all(|l| !l.raw.contains(SECRET)),
            "issuer {issuer}: expected the secret in no line of the log"
        );
    }
}

#[test]
fn the_listening_line_says_stand_in_or_none() {
    let stand_in = Vm::start(&VmOptions::new().stand_in_sign_in());
    assert_eq!(
        stand_in.listening()["sign_in"],
        "stand-in",
        "{}",
        stand_in.listening()
    );
    let none = Vm::start(&VmOptions::new());
    assert_eq!(none.listening()["sign_in"], "none", "{}", none.listening());
}

#[test]
fn the_stand_in_together_with_oidc_is_a_start_up_failure() {
    // Each alone starts: what fails below is having both.
    assert_eq!(
        start_with_issuer("http://127.0.0.1:9", &[]).listening()["sign_in"],
        "oidc"
    );
    assert_eq!(
        Vm::start(&VmOptions::new().stand_in_sign_in()).listening()["sign_in"],
        "stand-in"
    );
    assert_start_fails(
        "--stand-in-sign-in with --oidc-issuer",
        &with_listen(&[
            "--stand-in-sign-in",
            "--oidc-issuer",
            "http://127.0.0.1:9",
            "--oidc-client-id",
            CLIENT_ID,
        ]),
        Some(SECRET),
        &["--stand-in-sign-in"],
    );
}

#[test]
fn the_stand_in_on_an_address_that_is_not_loopback_is_a_start_up_failure() {
    for listen in ["0.0.0.0:0", "[::]:0"] {
        assert_start_fails(
            &format!("--stand-in-sign-in with --listen {listen}"),
            &["--listen", listen, "--stand-in-sign-in"],
            None,
            &["--stand-in-sign-in"],
        );
    }
}

#[test]
fn the_session_settings_outside_their_bounds_are_start_up_failures() {
    let oidc = [
        "--oidc-issuer",
        "http://127.0.0.1:9",
        "--oidc-client-id",
        CLIENT_ID,
    ];
    for (flag, value) in [
        ("--session-renewal", "0"),
        ("--session-renewal", "86401"),
        ("--session-renewal", "ten"),
        ("--session-lifetime", "59"),
        ("--session-lifetime", "2592001"),
        ("--public-scheme", "ftp"),
    ] {
        let mut args = with_listen(&oidc);
        args.extend([flag, value]);
        assert_start_fails(&format!("{flag} {value}"), &args, Some(SECRET), &[flag]);
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
        let vm = start_with_issuer("http://127.0.0.1:9", &more);
        assert_eq!(vm.listening()["sign_in"], "oidc", "{more:?}");
    }
}

#[test]
fn the_identity_provider_is_asked_nothing_at_start_up() {
    let provider = Provider::start(&[ada()]);
    let vm = Vm::start(&oidc_options(&provider));
    assert_eq!(vm.listening()["sign_in"], "oidc");
    std::thread::sleep(Duration::from_millis(500));
    let asked = provider.requests();
    assert!(
        asked.is_empty(),
        "expected the program to ask the identity provider nothing at start-up, it asked {:?}",
        asked.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

#[test]
fn the_program_starts_with_the_identity_provider_unreachable_or_not_answering() {
    let refused = Provider::unreachable_issuer();
    let began = Instant::now();
    let vm = start_with_issuer(&refused, &[]);
    assert_eq!(vm.listening()["sign_in"], "oidc");
    drop(vm);
    let hanging = Provider::start(&[ada()]);
    hanging.set_outage(Outage::Hang);
    let vm = Vm::start(&oidc_options(&hanging));
    assert_eq!(vm.listening()["sign_in"], "oidc");
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "expected both starts to be quick, they took {:?}",
        began.elapsed()
    );
}

#[test]
fn the_client_secret_is_in_no_line_of_the_log_after_a_sign_in() {
    let installation = Installation::start(&[("honest", "honest.js")]);
    let mut browser = installation.browser_as(blackbox::sessions::ADA);
    installation.sign_in(&mut browser, "honest", "/");
    installation.me(&mut browser).expect_status(200);
    let secret = installation.provider.client_secret();
    let mut hidden = installation.provider.issued();
    hidden.push(secret.expose().to_owned());
    installation.assert_none_in_log(&hidden, "after a sign-in");
}
