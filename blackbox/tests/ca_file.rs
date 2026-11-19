//! A company's own certificate authority (CONTRACT.md, "The tool reaches an
//! installation over HTTPS: A company's own certificate authority"; ticket
//! "The tool reaches an installation over HTTPS, and a company's own
//! certificate authority is trusted").
//!
//! `VIBE_CA_FILE` names a PEM file of certificates trusted beside the
//! built-in ones. For the tool: a file that is not there, a folder, a file
//! with no certificate (only text; only a private key), a file larger than
//! 1 MiB, are reason `ca-file`, exit code `1`, naming the variable and the
//! path, with nothing sent; empty is as unset. For `vibe serve` and `vibe
//! setup` the same files are start-up failures. The identity provider
//! (the harness's, behind a TLS front whose certificate for `localhost` is
//! issued under a test authority) is usable when the authority is in
//! `vibe serve`'s file, and `PUT /v1/oidc` is `422`
//! `identity-provider-unusable` naming `VIBE_CA_FILE` when it is not. What
//! the program serves does not change.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::process::Command;
use std::time::{Duration, Instant};

use blackbox::connection::{Connection, masked};
use blackbox::postgres::Platform;
use blackbox::program::{Finished, run_to_exit, vm_binary};
use blackbox::sessions::{ADA, Installation, ada, oidc_options};
use blackbox::tls::{
    Authority, Behind, Files, Front, Handshake, PROVIDER_HOST, api_name, place_at, private_key_pem,
    provider_behind_tls, text_of, tls_get,
};
use blackbox::tool::{Machine, Run};
use blackbox::{Vm, VmOptions, assert_api_error};
use serde_json::Value;

/// The contract's 15 seconds for a start-up that cannot work.
const WITHIN: Duration = Duration::from_secs(15);

/// One MiB, the largest `VIBE_CA_FILE` taken.
const MIB: usize = 1024 * 1024;

/// A `VIBE_CA_FILE` that cannot be used.
#[derive(Clone, Copy, Debug)]
enum Bad {
    /// No such file.
    Missing,
    /// A folder.
    Folder,
    /// A file of text with no certificate in it.
    TextOnly,
    /// A file holding a private key and nothing else.
    KeyOnly,
    /// Certificates, more than 1 MiB of them.
    TooLarge,
    /// The good authority, then a `CERTIFICATE` section whose base64 is
    /// good and whose content is no certificate.
    NotACertificate,
    /// The good authority, then a `CERTIFICATE` section whose base64 is
    /// broken.
    BrokenBase64,
}

/// A bad file of this kind in `files`; `authority` is what a file too
/// large repeats.
fn bad_file(files: &Files, bad: Bad, authority: &Authority) -> PathBuf {
    match bad {
        Bad::Missing => files.dir().join("not-there.pem"),
        Bad::Folder => {
            let dir = files.dir().join("a-folder.pem");
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
        Bad::TextOnly => files.write(
            "text-only.pem",
            b"This file was meant to hold the company's certificate authority.\n",
        ),
        Bad::KeyOnly => files.write("key-only.pem", private_key_pem().as_bytes()),
        Bad::TooLarge => files.write("too-large.pem", repeated(authority, MIB + 1).as_bytes()),
        Bad::NotACertificate => {
            let content = STANDARD.encode(
                "This is no certificate, though it is in a CERTIFICATE section. ".repeat(8),
            );
            let lines: Vec<String> = content
                .as_bytes()
                .chunks(64)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            let text = format!(
                "{}-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
                authority.pem(),
                lines.join("\n")
            );
            files.write("not-a-certificate.pem", text.as_bytes())
        }
        Bad::BrokenBase64 => {
            let text = format!(
                "{}-----BEGIN CERTIFICATE-----\nMIIB!!this*is*not*base64~~\n-----END CERTIFICATE-----\n",
                authority.pem()
            );
            files.write("broken-base64.pem", text.as_bytes())
        }
    }
}

/// `authority`'s certificate repeated until the text is at least `bytes`
/// long.
fn repeated(authority: &Authority, bytes: usize) -> String {
    let pem = authority.pem();
    let mut text = String::new();
    while text.len() < bytes {
        text.push_str(&pem);
    }
    text
}

/// `vibe sign-in` over HTTPS to the front's port as dana, `--json`.
fn sign_in_args(port: u16) -> Vec<String> {
    let mut args = vec!["sign-in".to_owned()];
    args.extend(place_at(port));
    args.extend([
        "--stand-in-user".to_owned(),
        "dana".to_owned(),
        "--json".to_owned(),
    ]);
    args
}

fn sign_in(machine: &Machine, port: u16, ca_file: &str) -> Run {
    let args = sign_in_args(port);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    machine.run_with_env(&refs, &[("VIBE_CA_FILE", ca_file)])
}

/// Fails unless `vibe sign-in` with this bad `VIBE_CA_FILE` ends `ca-file`,
/// exit code `1`, naming the variable and the path, having sent nothing
/// and kept nothing.
fn assert_tool_refuses(bad: Bad) {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let files = Files::new();
    let path = text_of(&bad_file(&files, bad, &behind.authority));
    let machine = Machine::new();
    let run = sign_in(&machine, behind.port(), &path);
    let message = run.json_error(1, "ca-file");
    assert!(
        message.contains("VIBE_CA_FILE") && message.contains(&path),
        "{bad:?}: expected the message to name VIBE_CA_FILE and the path {path}, got {message:?}"
    );
    assert_eq!(
        behind.front.accepted(),
        0,
        "{bad:?}: expected nothing to be sent\n{}",
        behind.front.describe()
    );
    assert!(machine.credential_files().is_empty());
}

#[test]
fn a_ca_file_that_is_not_there_ends_the_tool_with_ca_file() {
    assert_tool_refuses(Bad::Missing);
}

#[test]
fn a_ca_file_that_is_a_folder_ends_the_tool_with_ca_file() {
    assert_tool_refuses(Bad::Folder);
}

#[test]
fn a_ca_file_of_text_with_no_certificate_ends_the_tool_with_ca_file() {
    assert_tool_refuses(Bad::TextOnly);
}

#[test]
fn a_ca_file_holding_only_a_private_key_ends_the_tool_with_ca_file() {
    assert_tool_refuses(Bad::KeyOnly);
}

#[test]
fn a_ca_file_larger_than_1_mib_ends_the_tool_with_ca_file() {
    assert_tool_refuses(Bad::TooLarge);
}

#[test]
fn a_certificate_section_that_is_no_certificate_fails_the_whole_file_for_the_tool() {
    assert_tool_refuses(Bad::NotACertificate);
}

#[test]
fn a_certificate_section_with_broken_base64_fails_the_whole_file_for_the_tool() {
    assert_tool_refuses(Bad::BrokenBase64);
}

#[test]
fn a_private_key_beside_the_authority_is_passed_over_and_the_tool_reaches_the_installation() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let files = Files::new();
    let text = format!("{}{}", private_key_pem(), behind.authority.pem());
    let path = text_of(&files.write("key-and-authority.pem", text.as_bytes()));
    let machine = Machine::new();
    sign_in(&machine, behind.port(), &path).ok_json();
    assert!(behind.front.completed() >= 1, "{}", behind.front.describe());
}

#[test]
fn the_machines_own_bundle_is_taken_as_vibe_ca_file() {
    let bundle = Path::new("/etc/ssl/certs/ca-certificates.crt");
    if !bundle.is_file() {
        return;
    }
    let nowhere = blackbox::tool::closed_address();
    let machine = Machine::new();
    let run = sign_in(&machine, nowhere.port(), &text_of(bundle));
    // Not `ca-file`: the file is taken, and the tool goes on to find that
    // nothing listens.
    run.json_error(1, "unreachable");
}

#[test]
fn an_empty_vibe_ca_file_is_as_unset() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    let run = sign_in(&machine, behind.port(), "");
    // Not `ca-file`: the built-in authorities alone, which do not hold A.
    let message = run.json_error(1, "certificate");
    assert!(message.contains("VIBE_CA_FILE"), "{message:?}");
    assert!(machine.credential_files().is_empty());
}

#[test]
fn a_ca_file_of_several_certificates_trusts_each_of_them() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let b = Authority::new("Harness authority B");
    let files = Files::new();
    let both = text_of(&files.authorities("b-then-a.pem", &[&b, &behind.authority]));
    let machine = Machine::new();
    sign_in(&machine, behind.port(), &both).ok_json();
    assert!(behind.front.completed() >= 1, "{}", behind.front.describe());
}

#[test]
fn a_ca_file_of_just_under_1_mib_is_taken() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let one = behind.authority.pem();
    // As many whole certificates as fit in 1 MiB: one more would not.
    let mut text = String::new();
    while text.len() + one.len() <= MIB {
        text.push_str(&one);
    }
    assert!(text.len() <= MIB && text.len() + one.len() > MIB);
    let files = Files::new();
    let path = text_of(&files.write("just-under.pem", text.as_bytes()));
    let machine = Machine::new();
    sign_in(&machine, behind.port(), &path).ok_json();
}

/// `vibe <args>` with `VIBE_DATABASE_URL` naming a fresh platform database
/// and `VIBE_CA_FILE` set to `ca_file`, run to its exit (killed at
/// [`WITHIN`]).
fn run_program(args: &[&str], ca_file: &str) -> (Finished, Duration) {
    let platform = Platform::fresh();
    let mut command = Command::new(vm_binary());
    command
        .args(args)
        .env("VIBE_DATABASE_URL", platform.url())
        .env("VIBE_CA_FILE", ca_file)
        .env_remove("VIBE_SECRET_KEY")
        .env_remove("VIBE_SETUP_CREDENTIAL")
        .env_remove("VIBE_OIDC_CLIENT_SECRET");
    let began = Instant::now();
    let finished = run_to_exit(command, WITHIN).unwrap();
    (finished, began.elapsed())
}

fn described(what: &str, finished: &Finished, took: Duration) -> String {
    format!(
        "{what}\nexit: {} after {took:?}\nstandard output:\n{}\nstandard error:\n{}",
        finished.status_text(),
        masked(&finished.stdout, &[]),
        masked(&finished.stderr, &[])
    )
}

/// Fails unless `vibe serve` with this bad file is a start-up failure:
/// exit code not `0` within 15 seconds, no `listening` record, the message
/// naming `VIBE_CA_FILE` and the path.
fn assert_serve_refuses(bad: Bad) {
    let files = Files::new();
    let authority = Authority::new("Harness authority A");
    let path = text_of(&bad_file(&files, bad, &authority));
    let (finished, took) = run_program(
        &[
            "serve",
            "--domain",
            blackbox::DOMAIN,
            "--listen",
            "127.0.0.1:0",
            "--stand-in-sign-in",
        ],
        &path,
    );
    let shown = described(&format!("`vibe serve` with {bad:?}"), &finished, took);
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected a start-up failure within {WITHIN:?}\n{shown}"
    );
    let listened = finished
        .stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|r| r["event"] == "listening");
    assert!(!listened, "expected no `listening` record\n{shown}");
    assert!(
        finished.stderr.contains("VIBE_CA_FILE") && finished.stderr.contains(&path),
        "expected the message to name VIBE_CA_FILE and the path {path}\n{shown}"
    );
}

/// As [`assert_serve_refuses`], for `vibe setup --json`; and it printed no
/// setup credential.
fn assert_setup_refuses(bad: Bad) {
    let files = Files::new();
    let authority = Authority::new("Harness authority A");
    let path = text_of(&bad_file(&files, bad, &authority));
    let (finished, took) = run_program(&["setup", "--json"], &path);
    let shown = described(&format!("`vibe setup` with {bad:?}"), &finished, took);
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected a start-up failure within {WITHIN:?}\n{shown}"
    );
    assert!(
        finished.stderr.contains("VIBE_CA_FILE") && finished.stderr.contains(&path),
        "expected the message to name VIBE_CA_FILE and the path {path}\n{shown}"
    );
    assert!(
        !finished.stdout.contains("vmsc_") && !finished.stderr.contains("vmsc_"),
        "expected no setup credential to be printed\n{shown}"
    );
}

#[test]
fn vibe_serve_with_a_ca_file_that_is_not_there_does_not_start() {
    assert_serve_refuses(Bad::Missing);
}

#[test]
fn vibe_serve_with_a_ca_file_that_is_a_folder_does_not_start() {
    assert_serve_refuses(Bad::Folder);
}

#[test]
fn vibe_serve_with_a_ca_file_holding_no_certificate_does_not_start() {
    assert_serve_refuses(Bad::TextOnly);
}

#[test]
fn vibe_serve_with_a_ca_file_larger_than_1_mib_does_not_start() {
    assert_serve_refuses(Bad::TooLarge);
}

#[test]
fn vibe_serve_with_a_certificate_section_that_is_no_certificate_does_not_start() {
    assert_serve_refuses(Bad::NotACertificate);
}

#[test]
fn vibe_serve_with_a_certificate_section_with_broken_base64_does_not_start() {
    assert_serve_refuses(Bad::BrokenBase64);
}

#[test]
fn vibe_setup_with_a_ca_file_that_is_not_there_fails() {
    assert_setup_refuses(Bad::Missing);
}

#[test]
fn vibe_setup_with_a_ca_file_that_is_a_folder_fails() {
    assert_setup_refuses(Bad::Folder);
}

#[test]
fn vibe_setup_with_a_ca_file_holding_no_certificate_fails() {
    assert_setup_refuses(Bad::KeyOnly);
}

#[test]
fn vibe_setup_with_a_ca_file_larger_than_1_mib_fails() {
    assert_setup_refuses(Bad::TooLarge);
}

/// The identity provider behind its front under authority A, and a program
/// (the fixture `board` installed) started beside it with `VIBE_CA_FILE`
/// naming `ca_file`, or without it; OIDC not connected yet.
fn beside_provider(a: &Authority, ca_file: Option<&Path>) -> (Installation, Front) {
    let (provider, front) = provider_behind_tls(&[ada()], a);
    let mut options = oidc_options().application("board", "honest.js");
    if let Some(ca_file) = ca_file {
        options = options.env("VIBE_CA_FILE", &text_of(ca_file));
    }
    (Installation::unconnected(provider, &options), front)
}

#[test]
fn with_the_authority_in_vibe_ca_file_oidc_connects_and_a_person_signs_in() {
    let a = Authority::new("Harness authority A");
    let files = Files::new();
    let ca = files.authorities("a.pem", &[&a]);
    let (at, front) = beside_provider(&a, Some(&ca));
    assert!(at.provider.issuer().starts_with("https://"));
    let connected = at.connect_through_the_api(&Connection::to(&at.provider));
    connected.expect_status(200);
    let mut browser = at.browser_as(ADA);
    // The browser reaches the provider's authorization endpoint over TLS.
    browser.trust(a.der());
    at.sign_in(&mut browser, "board", "/");
    assert_eq!(at.user_seen(&mut browser, "board")["id"], ADA);
    let seen = front.settled();
    assert!(
        seen.iter().any(|s| s.handshake == Handshake::Completed),
        "expected the platform to reach the provider through its TLS front\n{}",
        front.describe()
    );
    assert!(
        seen.iter()
            .all(|s| s.server_name.as_deref() == Some(PROVIDER_HOST)),
        "expected the platform to ask for the provider's name\n{}",
        front.describe()
    );
    let lines = front.request_lines();
    for path in [
        "/.well-known/openid-configuration",
        "/authorize",
        "/token",
        "/jwks",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(&format!(" {path}"))),
            "expected {path} to be asked over TLS, got {lines:?}"
        );
    }
}

/// Fails unless `PUT /v1/oidc` to the provider behind TLS is `422`
/// `identity-provider-unusable`, the message saying the certificate was
/// not trusted and naming `VIBE_CA_FILE`.
fn assert_provider_not_trusted(at: &Installation) {
    let answer = at.connect_through_the_api(&Connection::to(&at.provider));
    assert_api_error(&answer, 422, "identity-provider-unusable");
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("VIBE_CA_FILE") && message.to_lowercase().contains("certificate"),
        "expected the message to say the provider's certificate was not trusted and name VIBE_CA_FILE, got {message:?}"
    );
    at.vm
        .api_request("/v1/installation")
        .send()
        .expect_status(200);
    assert_eq!(
        at.vm.api_request("/v1/installation").send().json()["sign_in"],
        "none",
        "expected nothing to be changed"
    );
}

#[test]
fn without_vibe_ca_file_a_provider_under_a_company_authority_is_unusable() {
    let a = Authority::new("Harness authority A");
    let (at, _front) = beside_provider(&a, None);
    assert_provider_not_trusted(&at);
}

#[test]
fn with_a_ca_file_holding_only_another_authority_the_provider_is_unusable() {
    let a = Authority::new("Harness authority A");
    let b = Authority::new("Harness authority B");
    let files = Files::new();
    let only_b = files.authorities("b.pem", &[&b]);
    let (at, _front) = beside_provider(&a, Some(&only_b));
    assert_provider_not_trusted(&at);
}

#[test]
fn a_connection_kept_after_a_start_without_the_file_is_kept_and_its_sign_ins_fail_as_unreachable() {
    let a = Authority::new("Harness authority A");
    let files = Files::new();
    let ca = files.authorities("a.pem", &[&a]);
    let (at, _front) = beside_provider(&a, Some(&ca));
    at.connect_through_the_api(&Connection::to(&at.provider))
        .expect_status(200);
    let Installation { vm, provider, .. } = at;
    let options = vm
        .options()
        .clone()
        .without_env("VIBE_CA_FILE")
        .platform(vm.platform());
    drop(vm);
    let again = Installation::unconnected(provider, &options);
    assert_eq!(
        again.vm.listening()["sign_in"],
        "oidc",
        "expected the connection to be kept"
    );
    let mut browser = again.browser_as(ADA);
    let first = browser.open(&again.app_url("board", "/"));
    assert!(first.is_redirect(), "{}", first.response.describe());
    let at_sign_in = browser.follow(&first);
    assert_eq!(
        at_sign_in.response.status,
        502,
        "expected the sign-in to end as with a provider that cannot be reached\n{}",
        at_sign_in.response.describe()
    );
}

#[test]
fn with_vibe_ca_file_the_program_still_serves_plain_http_and_gives_no_certificate() {
    let a = Authority::new("Harness authority A");
    let files = Files::new();
    let ca = files.authorities("a.pem", &[&a]);
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .env("VIBE_CA_FILE", &text_of(&ca)),
    );
    let answer = vm.api_request("/v1/installation").send();
    answer.expect_status(200);
    assert_eq!(
        answer.json()["sign_in"],
        "stand-in",
        "{}",
        answer.describe()
    );
    assert!(
        tls_get(
            vm.address(),
            &api_name(),
            &[&a],
            "/v1/installation",
            &api_name()
        )
        .is_err(),
        "expected the program to give no certificate to anyone"
    );
}
