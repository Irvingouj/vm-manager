//! Starting and stopping `vm serve` (CONTRACT.md, "Serving: Starting and
//! stopping"), and the shape of its log.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::net::TcpStream;
use std::time::Duration;

use blackbox::program::run_vm_to_exit;
use blackbox::{DOMAIN, Vm, VmOptions, fixture};

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

/// Runs `vm serve` with these extra arguments and fails unless it exits
/// non-zero, within the timeout, without listening, with a message on
/// standard error that mentions each of `named`.
fn assert_start_up_fails(case: &str, application_args: &[String], named: &[&str]) {
    let mut args = vec![
        "serve".to_owned(),
        "--domain".to_owned(),
        DOMAIN.to_owned(),
        "--listen".to_owned(),
        "127.0.0.1:0".to_owned(),
        "--stand-in-sign-in".to_owned(),
    ];
    args.extend_from_slice(application_args);
    let finished = run_vm_to_exit(&args);
    let shown = format!(
        "case: {case}\nargs: {args:?}\nexit: {}\nstandard error:\n{}",
        finished.status_text(),
        finished.stderr
    );
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected `vm serve` to exit non-zero by itself\n{shown}"
    );
    assert!(
        !finished.stderr.contains("\"listening\""),
        "expected no `listening` record from a start-up that cannot work\n{shown}"
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
}

fn application(name: &str, file: &str) -> Vec<String> {
    vec![
        "--application".to_owned(),
        format!("{name}={}", fixture(file).display()),
    ]
}

#[test]
fn start_up_fails_for_an_application_name_that_is_not_a_dns_label() {
    let too_long = "a".repeat(64);
    for name in [
        "Bad_Name",
        "-starts-with-a-dash",
        "ends-with-a-dash-",
        "has.a.dot",
        "has space",
        too_long.as_str(),
    ] {
        let mut args = application("honest", "honest.js");
        args.extend(application(name, "honest.js"));
        assert_start_up_fails(&format!("the name {name:?}"), &args, &[name]);
    }
}

#[test]
fn start_up_fails_for_an_empty_application_name() {
    let args = vec![
        "--application".to_owned(),
        format!("={}", fixture("honest.js").display()),
    ];
    assert_start_up_fails("an empty name", &args, &["application"]);
}

#[test]
fn start_up_fails_for_an_application_name_given_twice() {
    let mut args = application("twice", "honest.js");
    args.extend(application("twice", "errors.js"));
    assert_start_up_fails("the name `twice` given twice", &args, &["twice"]);
}

#[test]
fn start_up_fails_for_an_application_file_that_cannot_be_read() {
    let missing = std::env::temp_dir().join("blackbox-no-such-dir/no-such-application.js");
    let args = vec![
        "--application".to_owned(),
        format!("missing={}", missing.display()),
    ];
    assert_start_up_fails(
        "a file that does not exist",
        &args,
        &["no-such-application.js"],
    );
}

#[test]
fn start_up_fails_with_no_application() {
    assert_start_up_fails("no --application at all", &[], &["application"]);
}

#[test]
fn a_maximal_dns_label_is_a_valid_application_name() {
    let name = format!("a{}c9", "-b".repeat(30));
    assert_eq!(name.len(), 63);
    let vm = Vm::start(
        &VmOptions::new()
            .application(&name, "honest.js")
            .stand_in_sign_in(),
    );
    vm.get_as(&blackbox::User::new("alice"), &name, "/hello")
        .expect_status(200);
}
