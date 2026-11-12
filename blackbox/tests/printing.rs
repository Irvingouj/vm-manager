//! What an Application prints is captured per request (CONTRACT.md: "What
//! the Application prints ... appears as `printed` records").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::records::{application_of, user_of};
use blackbox::{User, Vm};

#[test]
fn each_console_level_appears_as_a_printed_record_with_its_request_user_and_level() {
    let vm = Vm::with_stand_in(&[("honest", "honest.js")]);
    let answer = vm.get_as(&User::new("alice"), "honest", "/print");
    answer.expect_status(200);
    let records = vm.records_of(&answer);
    let printed = records.printed();
    for level in ["log", "info", "warn", "error", "debug"] {
        let message = format!("{level} line from alice");
        let found: Vec<_> = printed
            .iter()
            .filter(|r| r["message"] == message.as_str())
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected one `printed` record with message {message:?}, got {printed:?}"
        );
        let record = found[0];
        assert_eq!(record["level"], level, "level in {record}");
        assert_eq!(user_of(record), Some("alice"), "user in {record}");
        assert_eq!(
            application_of(record),
            Some("honest"),
            "application in {record}"
        );
    }
}

#[test]
fn printed_records_carry_the_request_and_user_that_printed_them() {
    let vm = Vm::with_stand_in(&[("honest", "honest.js")]);
    let for_alice = vm.get_as(&User::new("alice"), "honest", "/print");
    let for_bob = vm.get_as(&User::new("bob"), "honest", "/print");
    for (answer, name) in [(&for_alice, "alice"), (&for_bob, "bob")] {
        answer.expect_status(200);
        let records = vm.records_of(answer);
        let printed = records.printed();
        // The handler's own line plus one per level.
        assert_eq!(
            printed.len(),
            6,
            "expected 6 printed records for {name}, got {printed:?}"
        );
        for record in printed {
            assert_eq!(user_of(record), Some(name), "user in {record}");
            let message = record["message"].as_str().unwrap_or_default();
            assert!(
                message.starts_with("honest: handling")
                    || message.ends_with(&format!("from {name}")),
                "expected only {name}'s lines in the records of {name}'s request, got {record}"
            );
        }
    }
    vm.assert_log_keeps_each_request_to_itself();
}
