//! What the tool refuses on its way to an installation over HTTPS
//! (CONTRACT.md, "The tool reaches an installation over HTTPS: Where the
//! tool connects"; ticket "The tool reaches an installation over HTTPS, and
//! a company's own certificate authority is trusted"): a certificate for
//! another name, under an authority it does not know, or expired, is reason
//! `certificate`, exit code `1`, before any request is sent (the TLS front
//! saw no request bytes, no credential file was written); the command line
//! that mixes the two ways or gives what is no port or no address is exit
//! code `2`; plain `--connect` is as before; an installation that cannot be
//! reached over HTTPS ends as one that cannot be reached with `--connect`;
//! and the tool never falls back to plain HTTP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::connection::hide;
use blackbox::tls::{Authority, Behind, Files, Front, Handshake, api_name, place_at, text_of};
use blackbox::tool::{Machine, Run, closed_address, sign_in_args};
use blackbox::{DOMAIN, User, Vm};
use serde_json::Value;

/// Within this a refused or unreachable sign-in has ended (the tool's own
/// connection limit is 5 seconds).
const WITHIN: Duration = Duration::from_secs(15);

fn dana() -> User {
    User::new("dana").in_groups(&["builders"])
}

/// `vibe sign-in --domain <domain> --resolve 127.0.0.1 --port <port>` as
/// dana, `--json`.
fn https_sign_in(port: u16) -> Vec<String> {
    let mut args = vec!["sign-in".to_owned()];
    args.extend(place_at(port));
    args.extend([
        "--stand-in-user".to_owned(),
        "dana".to_owned(),
        "--stand-in-groups".to_owned(),
        "builders".to_owned(),
        "--json".to_owned(),
    ]);
    args
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Runs `args` with `VIBE_CA_FILE` set to `ca_file` when given (else not
/// set at all).
fn run_with_ca(machine: &Machine, args: &[String], ca_file: Option<&str>) -> Run {
    let env: Vec<(&str, &str)> = ca_file.map(|f| ("VIBE_CA_FILE", f)).into_iter().collect();
    hide(&machine.run_with_env(&refs(args), &env), &[])
}

/// Fails unless `run` ended with `certificate`, exit code `1`, within
/// [`WITHIN`]; the front completed no handshake and received no request
/// bytes; and nothing was kept on the machine. Returns the message.
fn assert_certificate_refused(run: &Run, front: &Front, machine: &Machine) -> String {
    let message = run.json_error(1, "certificate");
    assert!(run.elapsed < WITHIN, "{}", run.describe());
    let seen = front.settled();
    assert!(
        !seen.is_empty(),
        "expected the tool to have tried the front\n{}",
        front.describe()
    );
    for s in &seen {
        assert!(
            s.handshake != Handshake::Completed && s.received.is_empty(),
            "expected no handshake to complete and no request bytes to reach the front\n{}",
            front.describe()
        );
    }
    assert_eq!(
        seen[0].server_name.as_deref(),
        Some(api_name().as_str()),
        "expected the tool to ask for the server name {}",
        api_name()
    );
    assert!(
        machine.credential_files().is_empty(),
        "expected no credential file after a refused certificate, found {:?}",
        machine.credential_files()
    );
    message
}

#[test]
fn a_certificate_for_another_name_is_refused_saying_the_name_asked_for() {
    let vm = Vm::with_stand_in(&[]);
    let a = Authority::new("Harness authority A");
    let identity = a.issue(&["api.other.test", "*.other.test"]);
    let behind = Behind::with_identity(a, &identity, vm.address());
    let machine = Machine::new();
    let ca = text_of(behind.ca_file());
    let run = run_with_ca(&machine, &https_sign_in(behind.port()), Some(&ca));
    let message = assert_certificate_refused(&run, &behind.front, &machine);
    assert!(
        message.contains(&api_name()),
        "expected the message to say the name asked for ({}), got {message:?}",
        api_name()
    );
    assert!(
        message.to_lowercase().contains("name"),
        "expected the message to say the certificate is for another name, got {message:?}"
    );
}

#[test]
fn a_certificate_under_an_unknown_authority_is_refused_naming_vibe_ca_file() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    // Without VIBE_CA_FILE: authority A is not among the built-in ones.
    let run = run_with_ca(&machine, &https_sign_in(behind.port()), None);
    let message = assert_certificate_refused(&run, &behind.front, &machine);
    assert!(
        message.contains("VIBE_CA_FILE"),
        "expected the message to name VIBE_CA_FILE, got {message:?}"
    );
}

#[test]
fn a_ca_file_holding_only_another_authority_does_not_make_the_certificate_trusted() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let b = Authority::new("Harness authority B");
    let files = Files::new();
    let only_b = text_of(&files.authorities("authority-b.pem", &[&b]));
    let machine = Machine::new();
    let run = run_with_ca(&machine, &https_sign_in(behind.port()), Some(&only_b));
    let message = assert_certificate_refused(&run, &behind.front, &machine);
    assert!(
        message.contains("VIBE_CA_FILE"),
        "expected the message to name VIBE_CA_FILE, got {message:?}"
    );
}

#[test]
fn an_expired_certificate_is_refused_saying_it_is_expired() {
    let vm = Vm::with_stand_in(&[]);
    let a = Authority::new("Harness authority A");
    let identity = a.issue_expired(&[&api_name(), &format!("*.{DOMAIN}")]);
    let behind = Behind::with_identity(a, &identity, vm.address());
    let machine = Machine::new();
    let ca = text_of(behind.ca_file());
    let run = run_with_ca(&machine, &https_sign_in(behind.port()), Some(&ca));
    let message = assert_certificate_refused(&run, &behind.front, &machine);
    assert!(
        message.to_lowercase().contains("expired"),
        "expected the message to say the certificate is expired, got {message:?}"
    );
}

#[test]
fn a_certificate_not_valid_yet_is_refused_saying_it_is_not_valid_yet() {
    let vm = Vm::with_stand_in(&[]);
    let a = Authority::new("Harness authority A");
    let identity = a.issue_not_valid_yet(&[&api_name(), &format!("*.{DOMAIN}")]);
    let behind = Behind::with_identity(a, &identity, vm.address());
    let machine = Machine::new();
    let ca = text_of(behind.ca_file());
    let run = run_with_ca(&machine, &https_sign_in(behind.port()), Some(&ca));
    let message = assert_certificate_refused(&run, &behind.front, &machine);
    assert!(
        message.to_lowercase().contains("not valid yet"),
        "expected the message to say the certificate is not valid yet, got {message:?}"
    );
}

#[test]
fn a_signed_in_tool_whose_installation_now_has_an_unknown_certificate_sends_nothing() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    let ca = text_of(behind.ca_file());
    run_with_ca(&machine, &https_sign_in(behind.port()), Some(&ca)).ok_json();
    let credential = machine.kept_credential(&vm);
    let before = behind.front.settled().len();
    let b = Authority::new("Harness authority B");
    behind
        .front
        .replace_identity(&b.issue(&[&api_name(), &format!("*.{DOMAIN}")]));
    let run = run_with_ca(
        &machine,
        &["whoami".to_owned(), "--json".to_owned()],
        Some(&ca),
    );
    let message = run.json_error(1, "certificate");
    assert!(
        message.contains("VIBE_CA_FILE"),
        "expected the message to name VIBE_CA_FILE, got {message:?}"
    );
    let seen = behind.front.settled();
    let after = seen.get(before..).unwrap_or_default();
    assert!(
        !after.is_empty(),
        "expected whoami to have tried the front\n{}",
        behind.front.describe()
    );
    for s in after {
        assert!(
            s.handshake != Handshake::Completed && s.received.is_empty(),
            "expected nothing to be sent to an installation whose certificate is not trusted\n{}",
            behind.front.describe()
        );
        let as_text = vec![(
            "what the front received".to_owned(),
            String::from_utf8_lossy(&s.received).into_owned(),
        )];
        assert!(
            blackbox::tool::windows_in(&as_text, &credential).is_empty(),
            "the credential left the machine"
        );
    }
    // The credential is kept: a refused certificate is no sign-out.
    assert_eq!(machine.kept_credential(&vm), credential);
}

/// Fails unless `args` ends with exit code `2` having reached nothing and
/// kept nothing.
fn assert_usage_error(args: &[String], front: &Front) {
    let machine = Machine::new();
    let run = run_with_ca(&machine, args, None);
    run.expect_code(2);
    assert_eq!(
        front.accepted(),
        0,
        "expected a wrong command line to reach nothing\n{}",
        front.describe()
    );
    assert!(machine.credential_files().is_empty());
}

#[test]
fn a_port_outside_1_to_65535_is_exit_code_2() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    for port in ["0", "70000", "-1", "https"] {
        let mut args = https_sign_in(behind.port());
        let at = args.iter().position(|a| a == "--port").unwrap();
        args[at + 1] = port.to_owned();
        assert_usage_error(&args, &behind.front);
    }
}

#[test]
fn a_resolve_that_is_not_an_ip_address_is_exit_code_2() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    for resolve in ["not-an-ip", "localhost", "127.0.0.1:443"] {
        let mut args = https_sign_in(behind.port());
        let at = args.iter().position(|a| a == "--resolve").unwrap();
        args[at + 1] = resolve.to_owned();
        assert_usage_error(&args, &behind.front);
    }
}

#[test]
fn connect_together_with_port_or_resolve_is_exit_code_2() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let plain = sign_in_args(vm.address(), &dana());
    let with_port: Vec<String> = plain
        .iter()
        .cloned()
        .chain(["--port".to_owned(), behind.port().to_string()])
        .collect();
    let with_resolve: Vec<String> = plain
        .iter()
        .cloned()
        .chain(["--resolve".to_owned(), "127.0.0.1".to_owned()])
        .collect();
    assert_usage_error(&with_port, &behind.front);
    assert_usage_error(&with_resolve, &behind.front);
}

#[test]
fn connect_to_an_address_that_is_not_loopback_is_still_exit_code_2() {
    let machine = Machine::new();
    let run = machine.run(&[
        "sign-in",
        "--domain",
        DOMAIN,
        "--connect",
        "192.0.2.1:80",
        "--stand-in-user",
        "dana",
        "--json",
    ]);
    run.expect_code(2);
    assert!(machine.credential_files().is_empty());
}

#[test]
fn a_plain_connect_sign_in_works_as_before_and_its_credential_file_serves_later_commands() {
    let vm = Vm::with_stand_in(&[]);
    let machine = Machine::new();
    let printed = machine.sign_in(&vm, &dana());
    assert_eq!(printed["user"], "dana", "{printed}");
    let me = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(me["user"], "dana", "{me}");
    let listed = machine.run(&["applications", "--json"]).ok_json();
    assert_eq!(listed["applications"], Value::Array(Vec::new()), "{listed}");
    let out = machine.run(&["sign-out", "--json"]).ok_json();
    assert_eq!(out["ended_at_installation"], true, "{out}");
}

#[test]
fn an_installation_that_cannot_be_reached_over_https_ends_as_with_connect() {
    let nowhere = closed_address();
    let machine = Machine::new();
    let plain = machine.run(
        &refs(&sign_in_args(nowhere, &dana()))
            .into_iter()
            .chain(["--json"])
            .collect::<Vec<_>>(),
    );
    let plain_body = plain.json();
    let plain_reason = plain_body["error"]["reason"].as_str().unwrap_or_default();
    assert_eq!(plain.code, Some(1), "{}", plain.describe());
    let over_https = run_with_ca(&machine, &https_sign_in(nowhere.port()), None);
    let message = over_https.json_error(1, plain_reason);
    assert!(
        over_https.elapsed < WITHIN,
        "expected it to end within {WITHIN:?}\n{}",
        over_https.describe()
    );
    assert!(
        message.contains(&api_name()) && message.contains(&nowhere.port().to_string()),
        "expected the message to name {} and the port {}, got {message:?}",
        api_name(),
        nowhere.port()
    );
    assert!(machine.credential_files().is_empty());
}

#[test]
fn the_tool_never_falls_back_to_plain_http() {
    let vm = Vm::with_stand_in(&[]);
    // The program itself, which speaks plain HTTP, on the port given with
    // `--port`: through a plain front that records what came.
    let plain = Front::plain(vm.address());
    let machine = Machine::new();
    let run = run_with_ca(&machine, &https_sign_in(plain.port()), None);
    run.json_error(1, "certificate");
    let seen = plain.settled();
    assert!(
        !seen.is_empty(),
        "expected the tool to have connected\n{}",
        plain.describe()
    );
    for s in &seen {
        assert_eq!(
            s.first_bytes.first(),
            Some(&0x16),
            "expected every connection to begin with a TLS handshake record\n{}",
            plain.describe()
        );
    }
    assert!(
        plain.request_lines().is_empty(),
        "expected no HTTP request line to reach a plain listener, got {:?}",
        plain.request_lines()
    );
    assert!(machine.credential_files().is_empty());
}
