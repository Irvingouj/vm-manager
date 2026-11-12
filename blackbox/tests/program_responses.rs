//! The responses the program gives itself, the order it checks them in, and
//! how a `Host` maps to an Application (CONTRACT.md, "Addresses" and
//! "Responses the program itself gives").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::HashSet;

use blackbox::records::user_of;
use blackbox::{Request, Response, User, Vm};

fn vm() -> Vm {
    Vm::with_stand_in(&[("honest", "honest.js"), ("errors", "errors.js")])
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// Sends `request`, expects `status`, and fails if any Application code ran
/// for it. Returns the answer.
fn refused(vm: &Vm, request: &Request, status: u16) -> Response {
    let response = request.send();
    response.expect_status(status);
    vm.records_of(&response).assert_no_application_code_ran();
    response
}

fn malformed_sign_in(request: Request) -> Request {
    request
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-user", "mallory")
}

#[test]
fn a_request_without_host_is_400_whether_signed_in_or_not() {
    let vm = vm();
    refused(&vm, &vm.request("honest", "/").without_host(), 400);
    refused(
        &vm,
        &vm.request("honest", "/").without_host().as_user(&alice()),
        400,
    );
    refused(
        &vm,
        &malformed_sign_in(vm.request("honest", "/").without_host()),
        400,
    );
}

#[test]
fn a_malformed_sign_in_is_400_even_for_an_unknown_subdomain() {
    let vm = vm();
    refused(&vm, &malformed_sign_in(vm.request("nowhere", "/")), 400);
    refused(&vm, &malformed_sign_in(vm.request("honest", "/")), 400);
}

#[test]
fn not_signed_in_is_401_for_known_and_unknown_subdomains_alike() {
    let vm = vm();
    let known = refused(&vm, &vm.request("honest", "/"), 401);
    let unknown = refused(&vm, &vm.request("nowhere", "/"), 401);
    let other_domain = refused(
        &vm,
        &vm.request("honest", "/").host("honest.elsewhere.test"),
        401,
    );
    assert_eq!(
        known.body(),
        unknown.body(),
        "expected the same 401 body whether the Application exists or not, so that it cannot be learned"
    );
    assert_eq!(known.body(), other_domain.body());
}

#[test]
fn a_signed_in_request_to_an_unknown_subdomain_is_404_and_runs_no_application_code() {
    let vm = vm();
    refused(&vm, &vm.request("nowhere", "/").as_user(&alice()), 404);
}

#[test]
fn a_host_that_is_not_an_application_subdomain_of_the_domain_is_404() {
    let vm = vm();
    let port = vm.address().port();
    for host in [
        "vm.test".to_owned(),
        "honest.elsewhere.test".to_owned(),
        "deeper.honest.vm.test".to_owned(),
        "honestvm.test".to_owned(),
        "honest.vm.test.elsewhere.test".to_owned(),
        format!("127.0.0.1:{port}"),
        "localhost".to_owned(),
        ".vm.test".to_owned(),
    ] {
        let request = vm.request("honest", "/").host(&host).as_user(&alice());
        let response = request.send();
        assert_eq!(
            response.status,
            404,
            "expected 404 for Host {host:?}, got\n{}",
            response.describe()
        );
        vm.records_of(&response).assert_no_application_code_ran();
    }
}

#[test]
fn letter_case_and_a_port_in_host_are_ignored() {
    let vm = vm();
    for host in [
        "HONEST.VM.TEST",
        "Honest.Vm.Test",
        "honest.vm.test:8080",
        "HONEST.vm.test:1",
    ] {
        let response = vm
            .request("honest", "/hello")
            .host(host)
            .as_user(&alice())
            .send();
        assert_eq!(
            response.status,
            200,
            "expected Host {host:?} to reach the Application `honest`, got\n{}",
            response.describe()
        );
        assert!(
            response.text().contains("alice"),
            "expected the answer of `honest` for {host:?}, got {}",
            response.text()
        );
    }
}

#[test]
fn every_response_carries_a_request_id_unique_to_its_request() {
    let vm = vm();
    let requests = [
        vm.request("honest", "/hello").as_user(&alice()),
        vm.request("honest", "/hello").as_user(&alice()),
        vm.request("honest", "/"),
        vm.request("nowhere", "/").as_user(&alice()),
        vm.request("honest", "/").without_host(),
        malformed_sign_in(vm.request("honest", "/")),
        vm.request("errors", "/throw").as_user(&alice()),
    ];
    let mut ids = HashSet::new();
    for request in &requests {
        let response = request.send();
        let id = response.request_id();
        assert!(
            ids.insert(id.clone()),
            "expected request ids to be unique, but {id:?} came twice"
        );
    }
}

#[test]
fn the_programs_own_responses_are_short_plain_text() {
    let vm = vm();
    let responses = [
        (400, vm.request("honest", "/").without_host().send()),
        (400, malformed_sign_in(vm.request("honest", "/")).send()),
        (401, vm.request("honest", "/").send()),
        (404, vm.request("nowhere", "/").as_user(&alice()).send()),
        (500, vm.request("errors", "/throw").as_user(&alice()).send()),
    ];
    for (status, response) in &responses {
        response.expect_status(*status);
        assert!(
            response
                .header("content-type")
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
            "expected a text/plain body for {status}, got\n{}",
            response.describe()
        );
        assert!(
            !response.body().is_empty() && response.body().len() <= 200,
            "expected a short body (1 to 200 bytes) for {status}, got {} bytes\n{}",
            response.body().len(),
            response.describe()
        );
    }
}

#[test]
fn the_request_record_of_a_refused_request_names_what_is_known() {
    let vm = vm();
    let check = |response: &Response, status: u64, user: Option<&str>| {
        let records = vm.records_of(response);
        let record = &records.request;
        assert_eq!(
            record["status"].as_u64(),
            Some(status),
            "status in {record}"
        );
        assert_eq!(user_of(record), user, "user in {record}");
        assert!(
            record["application"].is_null(),
            "expected `application: null` in {record}"
        );
        assert_eq!(record["method"], "GET", "method in {record}");
        assert_eq!(record["path"], "/somewhere", "path in {record}");
    };
    check(
        &vm.request("honest", "/somewhere").without_host().send(),
        400,
        None,
    );
    check(
        &malformed_sign_in(vm.request("honest", "/somewhere")).send(),
        400,
        None,
    );
    check(&vm.request("honest", "/somewhere").send(), 401, None);
    check(
        &vm.request("nowhere", "/somewhere").as_user(&alice()).send(),
        404,
        Some("alice"),
    );
}
