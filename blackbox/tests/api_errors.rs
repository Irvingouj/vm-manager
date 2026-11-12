//! The API's refusals and records (CONTRACT.md, "The platform's API"): each
//! row of the table has its status, its `reason` and the error shape;
//! paths and Versions that do not exist are `404 not-found`; API requests
//! have `request` records with `application` and `version` null, and a
//! request id.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::HashSet;
use std::time::Duration;

use blackbox::api::marked_bundle;
use blackbox::records::user_of;
use blackbox::{Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

#[test]
fn each_row_of_the_table_has_its_status_reason_and_the_error_shape() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("one"));
    let rows: Vec<(u16, &str, Response)> = vec![
        (400, "malformed", api.post_raw("/v1/applications", "{")),
        (400, "invalid-name", api.create_application("Not_A_Label")),
        (400, "reserved-name", api.create_application("dashboard")),
        (
            403,
            "not-a-contributor",
            vm.api_as(&bob())
                .send_bundle("board", &marked_bundle("two")),
        ),
        (404, "not-found", api.get_application("no-such-application")),
        (404, "not-found", api.get_version("board", 2)),
        (404, "not-found", api.get("/v1/no-such-path")),
        (409, "name-taken", api.create_application("board")),
        (
            413,
            "too-large",
            api.request("/v1/applications")
                .method("POST")
                .header("content-type", "application/json")
                .body(vec![b' '; 16 * 1024 * 1024 + 1])
                .timeout(Duration::from_secs(60))
                .send(),
        ),
    ];
    for (status, reason, response) in &rows {
        assert_api_error(response, *status, reason);
    }
}

#[test]
fn paths_the_api_does_not_have_are_404_not_found() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("one"));
    for target in [
        "/",
        "/v1",
        "/v1/",
        "/v2/applications/board",
        "/v1/application/board",
        "/v1/applications/board/nothing",
        "/v1/applications/board/versions/1/more",
        "/V1/applications/board",
        "/v1/applications/board/versions/1/../../../../etc/passwd",
    ] {
        let response = api.get(target);
        assert!(
            response.status == 404,
            "expected `404 not-found` for GET {target}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 404, "not-found");
    }
    assert_api_error(
        &api.post("/v1/no-such-path", &json!({ "name": "x" })),
        404,
        "not-found",
    );
}

#[test]
fn a_version_number_that_names_no_version_is_404_not_found() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("one"));
    for number in [
        "0",
        "2",
        "-1",
        "abc",
        "1.5",
        "1e0",
        "+1",
        "%201",
        "18446744073709551616",
        "99999999999999999999999999999999",
        "%31%00",
    ] {
        let target = format!("/v1/applications/board/versions/{number}");
        let response = api.get(&target);
        assert!(
            response.status == 404,
            "expected `404 not-found` for GET {target}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 404, "not-found");
    }
}

#[test]
fn api_requests_have_request_records_with_application_and_version_null() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let cases: Vec<(&str, &str, u16, Response)> = vec![
        (
            "POST",
            "/v1/applications",
            201,
            api.create_application("board"),
        ),
        (
            "POST",
            "/v1/applications/board/versions",
            201,
            api.send_bundle("board", &marked_bundle("one")),
        ),
        (
            "GET",
            "/v1/applications/board",
            200,
            api.get("/v1/applications/board?with=query"),
        ),
        (
            "GET",
            "/v1/applications/board/versions/1",
            200,
            api.get_version("board", 1),
        ),
        (
            "POST",
            "/v1/applications",
            409,
            api.create_application("board"),
        ),
        ("GET", "/v1/nothing", 404, api.get("/v1/nothing")),
    ];
    for (method, path, status, response) in &cases {
        response.expect_status(*status);
        let record = vm.records_of(response).request;
        let what = format!("the `request` record of {method} {path}: {record}");
        assert_eq!(record["method"], *method, "method in {what}");
        assert_eq!(record["path"], *path, "path (without the query) in {what}");
        assert_eq!(
            record["status"].as_u64(),
            Some(u64::from(*status)),
            "status in {what}"
        );
        assert_eq!(user_of(&record), Some("alice"), "user in {what}");
        assert!(
            record.get("application").is_some_and(Value::is_null),
            "expected `application: null` in {what}"
        );
        assert!(
            record.get("version").is_some_and(Value::is_null),
            "expected `version: null` in {what}"
        );
        vm.records_of(response).assert_no_application_code_ran();
    }
}

#[test]
fn every_api_answer_carries_its_own_request_id() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let answers = [
        api.create_application("board"),
        api.create_application("board"),
        api.get_application("board"),
        api.get("/v1/nothing"),
        vm.api_request("/v1/applications/board").send(),
        api.post_raw("/v1/applications", "not json"),
    ];
    let mut ids = HashSet::new();
    for answer in &answers {
        let id = answer.request_id();
        assert!(
            ids.insert(id.clone()),
            "expected request ids to be unique, {id:?} came twice"
        );
    }
}
