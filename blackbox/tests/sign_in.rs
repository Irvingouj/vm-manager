//! The stand-in sign-in (CONTRACT.md, "Stand-in sign-in"): off unless
//! switched on, what it accepts, what is malformed, and that `x-vm-*`
//! headers never reach Application code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::expect::assert_user;
use blackbox::records::is_from_application_code;
use blackbox::{Request, User, Vm, VmOptions};
use serde_json::Value;

fn stand_in_on() -> Vm {
    Vm::with_stand_in(&[("honest", "honest.js")])
}

/// Sends `request`, expects `status`, and fails if any Application code ran
/// for it.
fn assert_refused_before_application_code(vm: &Vm, request: &Request, status: u16) {
    let response = request.send();
    response.expect_status(status);
    vm.records_of(&response).assert_no_application_code_ran();
}

#[test]
fn with_the_stand_in_off_a_stated_user_is_refused_401_and_no_application_code_runs() {
    let vm = Vm::start(
        &VmOptions::new()
            .application("honest", "honest.js")
            .application("calls-at-load", "calls-at-load.js"),
    );
    let alice = User::new("alice").in_groups(&["staff"]);
    for application in ["honest", "calls-at-load"] {
        for path in ["/", "/hello"] {
            let request = vm.request(application, path).as_user(&alice);
            assert_refused_before_application_code(&vm, &request, 401);
        }
    }
    // Not even an Application's top level ran: it would have left a
    // `host-call` record without a request id (calls-at-load.js).
    let from_code: Vec<Value> = vm
        .records_after_listening()
        .into_iter()
        .filter(is_from_application_code)
        .collect();
    assert!(
        from_code.is_empty(),
        "expected no Application code to run at all with the stand-in off, got {from_code:?}"
    );
}

#[test]
fn with_the_stand_in_off_its_headers_do_nothing_even_when_malformed() {
    let vm = Vm::start(&VmOptions::new().application("honest", "honest.js"));
    let twice = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-user", "bob");
    assert_refused_before_application_code(&vm, &twice, 401);
    let empty = vm.request("honest", "/").header("x-vm-stand-in-user", "");
    assert_refused_before_application_code(&vm, &empty, 401);
}

#[test]
fn with_the_stand_in_on_a_request_without_sign_in_is_refused_401_and_no_application_code_runs() {
    let vm = stand_in_on();
    let request = vm.request("honest", "/");
    assert_refused_before_application_code(&vm, &request, 401);
    let only_groups = vm
        .request("honest", "/")
        .header("x-vm-stand-in-groups", "staff");
    assert_refused_before_application_code(&vm, &only_groups, 401);
}

#[test]
fn the_stated_user_and_groups_reach_ctx_user() {
    let vm = stand_in_on();
    let alice = User::new("alice").in_groups(&["staff", "finance"]);
    let answer = vm.get_as(&alice, "honest", "/");
    answer.expect_status(200);
    assert_user(&answer.json()["user"], &alice, "ctx.user()");
}

#[test]
fn a_user_without_groups_has_an_empty_list_of_groups() {
    let vm = stand_in_on();
    let answer = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .send();
    answer.expect_status(200);
    assert_eq!(answer.json()["user"]["groups"], serde_json::json!([]));
    let empty_groups = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-groups", "")
        .send();
    empty_groups.expect_status(200);
    assert_eq!(empty_groups.json()["user"]["groups"], serde_json::json!([]));
}

#[test]
fn the_stand_in_headers_are_read_in_any_letter_case() {
    let vm = stand_in_on();
    let answer = vm
        .request("honest", "/")
        .header("X-VM-STAND-IN-USER", "alice")
        .header("X-Vm-Stand-In-Groups", "staff")
        .send();
    answer.expect_status(200);
    assert_user(
        &answer.json()["user"],
        &User::new("alice").in_groups(&["staff"]),
        "ctx.user() with the headers in odd letter case",
    );
}

#[test]
fn groups_are_trimmed_empty_ones_dropped_and_repeats_kept_once() {
    let vm = stand_in_on();
    let answer = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-groups", " staff ,, finance,staff , ,finance")
        .send();
    answer.expect_status(200);
    assert_user(
        &answer.json()["user"],
        &User::new("alice").in_groups(&["staff", "finance"]),
        "ctx.user() for Groups \" staff ,, finance,staff , ,finance\"",
    );
}

#[test]
fn two_user_headers_are_malformed_400_and_no_application_code_runs() {
    let vm = stand_in_on();
    let same_case = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-user", "mallory");
    assert_refused_before_application_code(&vm, &same_case, 400);
    let other_case = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("X-VM-Stand-In-User", "mallory");
    assert_refused_before_application_code(&vm, &other_case, 400);
    let same_value = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-user", "alice");
    assert_refused_before_application_code(&vm, &same_value, 400);
}

#[test]
fn two_groups_headers_are_malformed_400() {
    let vm = stand_in_on();
    let request = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-groups", "staff")
        .header("X-VM-Stand-In-Groups", "admins");
    assert_refused_before_application_code(&vm, &request, 400);
}

#[test]
fn an_empty_user_is_malformed_400() {
    let vm = stand_in_on();
    let request = vm.request("honest", "/").header("x-vm-stand-in-user", "");
    assert_refused_before_application_code(&vm, &request, 400);
    let with_groups = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "")
        .header("x-vm-stand-in-groups", "staff");
    assert_refused_before_application_code(&vm, &with_groups, 400);
}

#[test]
fn a_user_with_a_control_character_is_malformed_400() {
    let vm = stand_in_on();
    let request = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "ali\tce");
    assert_refused_before_application_code(&vm, &request, 400);
}

#[test]
fn groups_with_a_control_character_are_malformed_400() {
    let vm = stand_in_on();
    let request = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-groups", "staff,adm\tins");
    assert_refused_before_application_code(&vm, &request, 400);
}

#[test]
fn a_user_of_256_bytes_is_accepted_and_one_of_257_is_malformed_400() {
    let vm = stand_in_on();
    let longest = "u".repeat(256);
    let answer = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", &longest)
        .send();
    answer.expect_status(200);
    assert_eq!(answer.json()["user"]["id"].as_str(), Some(longest.as_str()));
    let too_long = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "u".repeat(257));
    assert_refused_before_application_code(&vm, &too_long, 400);
}

#[test]
fn groups_longer_than_256_bytes_are_malformed_400() {
    let vm = stand_in_on();
    let groups = vec!["g".repeat(20); 13].join(",");
    assert!(groups.len() > 256);
    let request = vm
        .request("honest", "/")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-groups", groups);
    assert_refused_before_application_code(&vm, &request, 400);
}

#[test]
fn a_user_id_that_is_not_ascii_is_handed_over_unchanged() {
    let vm = stand_in_on();
    let user = User::new("zoë.o'brien@example.com").in_groups(&["équipe"]);
    let answer = vm.get_as(&user, "honest", "/");
    answer.expect_status(200);
    assert_user(
        &answer.json()["user"],
        &user,
        "ctx.user() for a UTF-8 User id",
    );
}

#[test]
fn x_vm_headers_never_reach_application_code_in_any_letter_case() {
    let vm = stand_in_on();
    let answer = vm
        .request("honest", "/")
        .as_user(&User::new("alice").in_groups(&["staff"]))
        .header("X-VM-Stand-In-Note", "hidden")
        .header("x-Vm-anything", "hidden")
        .header("X-VM-REQUEST-ID", "forged")
        .header("x-vm-", "hidden")
        .header("x-vmnot-a-vm-header", "kept")
        .header("x-other", "kept")
        .send();
    answer.expect_status(200);
    let headers = answer.json()["headers"].clone();
    let names: Vec<&String> = headers
        .as_object()
        .expect("headers as an object")
        .keys()
        .collect();
    let leaked: Vec<&&String> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().starts_with("x-vm-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "expected no x-vm-* header to reach Application code, but it saw {leaked:?} among {headers}"
    );
    assert_eq!(
        headers["x-other"], "kept",
        "expected other headers to arrive: {headers}"
    );
    assert_eq!(
        headers["x-vmnot-a-vm-header"], "kept",
        "expected a header that only starts with `x-vm` (no dash) to arrive: {headers}"
    );
}

#[test]
fn a_request_id_sent_by_the_caller_is_not_the_request_id() {
    let vm = stand_in_on();
    let answer = vm
        .request("honest", "/")
        .as_user(&User::new("alice"))
        .header("x-vm-request-id", "forged-request-id")
        .send();
    answer.expect_status(200);
    assert_ne!(answer.request_id(), "forged-request-id");
    assert_eq!(
        answer.json()["whoami"]["ok"]["request"].as_str(),
        Some(answer.request_id().as_str())
    );
}
