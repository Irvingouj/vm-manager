//! Starting and stopping `vm serve` (CONTRACT.md, "Serving: Starting and
//! stopping"), the shape of its log, and the start-ups that cannot work
//! (no `VM_DATABASE_URL`, one that cannot be parsed, a Postgres that cannot
//! be reached or that refuses the role).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::net::TcpStream;
use std::time::{Duration, Instant};

use blackbox::postgres::{self, Platform};
use blackbox::program::{EXIT_TIMEOUT, run_vm_to_exit};
use blackbox::{DOMAIN, User, Vm, VmOptions};

#[test]
fn the_listening_record_gives_the_address_and_says_the_stand_in_is_on() {
    let vm = Vm::start(
        &VmOptions::new()
            .application("honest", "honest.js")
            .stand_in_sign_in(),
    );
    let listening = vm.listening();
    assert_eq!(
        listening["stand_in_sign_in"], true,
        "expected `stand_in_sign_in: true` in {listening}"
    );
    assert!(
        listening["address"]
            .as_str()
            .is_some_and(|a| a.starts_with("127.0.0.1:") && !a.ends_with(":0")),
        "expected the `listening` address to be 127.0.0.1 and the port actually chosen, got {listening}"
    );
    TcpStream::connect_timeout(&vm.address(), Duration::from_secs(5)).unwrap_or_else(|e| {
        vm.fail(&format!(
            "expected to connect to the listening address: {e}"
        ))
    });
}

#[test]
fn the_stand_in_sign_in_is_off_unless_switched_on() {
    let vm = Vm::start(&VmOptions::new().application("honest", "honest.js"));
    assert_eq!(
        vm.listening()["stand_in_sign_in"],
        false,
        "expected `stand_in_sign_in: false` in {}",
        vm.listening()
    );
}

#[test]
fn every_line_of_the_log_is_a_json_object_with_event_and_time_ms() {
    let vm = Vm::start(
        &VmOptions::new()
            .application("honest", "honest.js")
            .stand_in_sign_in(),
    );
    vm.get_as(&blackbox::User::new("alice"), "honest", "/print")
        .expect_status(200);
    vm.request("honest", "/").send().expect_status(401);
    vm.request("nowhere", "/").send().expect_status(401);
    let last = vm.request("honest", "/").without_host().send();
    vm.records_of(&last);
    for line in vm.lines() {
        let Some(record) = &line.record else {
            vm.fail(&format!(
                "expected every line of standard error to be a JSON object, got {:?}",
                line.raw
            ));
        };
        assert!(
            record["event"].is_string(),
            "expected an `event` string in {record}"
        );
        assert!(
            record["time_ms"].is_u64() || record["time_ms"].is_i64() || record["time_ms"].is_f64(),
            "expected a numeric `time_ms` in {record}"
        );
    }
}

#[test]
fn several_programs_run_side_by_side_on_their_own_ports() {
    let options = VmOptions::new()
        .application("honest", "honest.js")
        .stand_in_sign_in();
    let first = Vm::start(&options);
    let second = Vm::start(&options);
    assert_ne!(first.address(), second.address());
    let alice = blackbox::User::new("alice");
    first.get_as(&alice, "honest", "/hello").expect_status(200);
    second.get_as(&alice, "honest", "/hello").expect_status(200);
}

#[test]
fn the_program_stops_when_its_process_is_ended() {
    let vm = Vm::start(&VmOptions::new().application("honest", "honest.js"));
    let address = vm.address();
    drop(vm);
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err(),
        "expected nothing to listen on {address} once the program was ended"
    );
}

#[test]
fn with_no_applications_the_program_starts_and_every_application_address_is_404() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let alice = User::new("alice");
    for application in ["honest", "anything"] {
        let response = vm.get_as(&alice, application, "/");
        response.expect_status(404);
        vm.records_of(&response).assert_no_application_code_ran();
    }
}

/// Runs `vm serve` with `VM_DATABASE_URL` set to `database_url` (or unset)
/// and fails unless it exits non-zero by itself within the contract's 15
/// seconds, without listening, with a message on standard error that
/// mentions each of `named` and shows none of `secrets` anywhere.
fn assert_start_up_fails(case: &str, database_url: Option<&str>, named: &[&str], secrets: &[&str]) {
    let args = [
        "serve",
        "--domain",
        DOMAIN,
        "--listen",
        "127.0.0.1:0",
        "--stand-in-sign-in",
    ];
    let began = Instant::now();
    let finished = run_vm_to_exit(args, database_url);
    let took = began.elapsed();
    let shown = format!(
        "case: {case}\nargs: {args:?}\nexit: {} after {took:?}\nstandard error:\n{}",
        finished.status_text(),
        // Never show a secret in the test's own message either.
        secrets
            .iter()
            .fold(finished.stderr.clone(), |text, secret| text
                .replace(secret, "<SECRET>"))
    );
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected `vm serve` to exit non-zero by itself within {EXIT_TIMEOUT:?}\n{shown}"
    );
    assert!(
        !finished.stderr.contains("\"listening\""),
        "expected no `listening` record from a start-up that cannot work\n{shown}"
    );
    assert!(
        !finished.stderr.trim().is_empty(),
        "expected a message on standard error naming the problem\n{shown}"
    );
    for name in named {
        assert!(
            finished
                .stderr
                .to_lowercase()
                .contains(&name.to_lowercase()),
            "expected the message on standard error to name the problem ({name:?})\n{shown}"
        );
    }
    for secret in secrets {
        assert!(
            !finished.stderr.contains(secret) && !finished.stdout.contains(secret),
            "expected the password never to be shown, but it is in the program's output\n{shown}"
        );
    }
}

#[test]
fn start_up_fails_without_vm_database_url() {
    assert_start_up_fails("VM_DATABASE_URL unset", None, &["VM_DATABASE_URL"], &[]);
}

#[test]
fn start_up_fails_for_a_vm_database_url_that_cannot_be_parsed() {
    let secret = "pw-unparseable-7Hq2Zx";
    let cases = [
        ("empty".to_owned(), String::new()),
        (
            "not a URL".to_owned(),
            format!("this is not a url {secret}"),
        ),
        (
            "a port that is not a number".to_owned(),
            format!("postgres://vm_test:{secret}@127.0.0.1:not-a-port/vm_platform"),
        ),
        (
            "a port out of range".to_owned(),
            format!("postgres://vm_test:{secret}@127.0.0.1:99999999/vm_platform"),
        ),
        (
            "an unclosed IPv6 bracket".to_owned(),
            format!("postgres://vm_test:{secret}@[::1/vm_platform"),
        ),
    ];
    for (case, url) in &cases {
        assert_start_up_fails(case, Some(url), &["VM_DATABASE_URL"], &[secret]);
    }
}

#[test]
fn start_up_fails_when_postgres_cannot_be_reached_on_a_closed_port() {
    let secret = "pw-unreachable-c4Lm9";
    let port = postgres::free_port().unwrap();
    let url = format!("postgres://vm_test:{secret}@127.0.0.1:{port}/vm_platform");
    assert_start_up_fails(
        "nothing listens on the port",
        Some(&url),
        &[&port.to_string()],
        &[secret],
    );
}

#[test]
fn start_up_fails_within_the_time_when_postgres_cannot_be_reached_at_all() {
    // TEST-NET-1 (RFC 5737): no host answers there, so a connection
    // attempt hangs until the program gives up.
    let secret = "pw-blackhole-Vb81q";
    let url = format!("postgres://vm_test:{secret}@192.0.2.1:5432/vm_platform");
    assert_start_up_fails(
        "an address no host answers",
        Some(&url),
        &["192.0.2.1"],
        &[secret],
    );
}

#[test]
fn start_up_fails_when_postgres_refuses_a_role_that_does_not_exist() {
    let postgres = postgres::shared();
    let secret = "pw-no-such-role-Qe5r";
    let url = postgres.url_as("no_such_role_t24", secret, Platform::fresh().name());
    assert_start_up_fails(
        "a role Postgres does not know",
        Some(&url),
        &["no_such_role_t24"],
        &[secret, postgres.password().expose()],
    );
}

#[test]
fn start_up_fails_when_postgres_refuses_the_password() {
    let postgres = postgres::shared();
    let wrong = format!("{}-wrong", postgres.password().expose());
    let url = postgres.url_as(postgres::SUPERUSER, &wrong, Platform::fresh().name());
    assert_start_up_fails(
        "the superuser with a wrong password",
        Some(&url),
        &[postgres::SUPERUSER],
        &[&wrong, postgres.password().expose()],
    );
}

#[test]
fn the_password_in_vm_database_url_never_appears_in_the_log() {
    let vm = Vm::with_stand_in(&[("honest", "honest.js"), ("broken", "throws-at-load.js")]);
    let alice = User::new("alice");
    vm.get_as(&alice, "honest", "/print").expect_status(200);
    vm.get_as(&alice, "broken", "/").expect_status(500);
    vm.get_as(&alice, "nowhere", "/").expect_status(404);
    let api = vm.api_as(&alice);
    api.create_application("Not A Name").expect_status(400);
    api.create_application("honest").expect_status(409);
    let last = api.post_raw("/v1/applications", "{ not json");
    vm.records_of(&last);
    let password = postgres::shared().password().expose().to_owned();
    let mut lines = vm.lines();
    let restarted = vm.restart();
    restarted
        .get_as(&alice, "honest", "/hello")
        .expect_status(200);
    lines.extend(restarted.lines());
    let shown: Vec<String> = lines
        .into_iter()
        .filter(|l| l.raw.contains(&password))
        .map(|l| l.raw.replace(&password, "<PASSWORD>"))
        .collect();
    assert!(
        shown.is_empty(),
        "expected the password never to appear in the program's log, but these lines hold it:\n{}",
        shown.join("\n")
    );
}
