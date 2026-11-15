//! `vm request`: one request as a test User, on the Builder's machine
//! (CONTRACT.md, "The local loop: one request as a test User": "The
//! command", "What a run does", "What it prints"; ticket "Local loop: run
//! the Application on the Builder's machine and send a request as a test
//! User": in a template project with one pulled Resource, a request as the
//! Builder's test User returns the Application's response; a request as the
//! test User with no access gets the same refusal a deployed Application
//! would give; a Builder with structure-only access sees locally the
//! refusal their Users without a Grant will see; the command has a JSON
//! form).
//!
//! A project made by `vm new`, its Manifest declaring `devops` (the
//! harness's usual shared database, registered on a real installation), its
//! code reading `incidents` through `ctx.user().db("devops")` and answering
//! what it got or the refusal it got (`blackbox/src/local.rs`, `handler`),
//! pulled by dana, whose Group holds a Grant on it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::deploy::{commit_all, deploy, init_repository};
use blackbox::local::{
    APP, BUILDER, Board, DEVOPS, FAILURE, LOADING_LINE, Local, OUTSIDER, Requested, TEXT_BODY,
    dev_json, local_project, write_dev_json,
};
use blackbox::scenario::{CONTACT, Expected, Scenario, assert_entry};
use serde_json::{Value, json};

/// The refusal a test User with no Grant gets on `devops`.
fn no_grant(user: &str, held: Option<&str>) -> Value {
    json!({
        "reason": "no-grant",
        "resource": DEVOPS,
        "user": user,
        "needed": "read",
        "held": held,
        "contact": CONTACT,
    })
}

/// Fails unless the response is the handler's `403` holding this refusal.
fn assert_refused(requested: &Requested, refusal: &Value, context: &str) {
    assert_eq!(
        requested.status(),
        403,
        "{context}: expected the handler's 403 for a refusal\n{}",
        requested.describe()
    );
    let body = requested.json();
    assert_eq!(
        &body["refusal"],
        refusal,
        "{context}: expected the refusal an installation gives\n{}",
        requested.describe()
    );
    assert!(
        body["fix"].as_str().is_some_and(|f| f.contains(CONTACT)),
        "{context}: expected the refusal's fix to name whom to ask\n{}",
        requested.describe()
    );
}

#[test]
fn as_the_builder_the_response_is_the_applications_and_the_standin_holds_no_row() {
    let board = Board::start("read", "read");
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the Application's response: the stand-in holds no row\n{}",
        requested.describe()
    );
    let body = &requested.body;
    assert_eq!(body["as"], json!(BUILDER));
    assert_eq!(body["method"], json!("GET"));
    assert_eq!(body["path"], json!("/incidents"));
    assert_eq!(body["application"], json!(APP));
    assert_eq!(
        body["version"],
        json!(1),
        "expected the first local Version"
    );
    assert_eq!(body["body_base64"], Value::Null);
    assert_eq!(body["body_truncated"], json!(false));
    assert_eq!(body["uncaught"], json!([]));
    assert_eq!(body["migrations_applied"], json!([]));
    assert_eq!(
        body["standins"],
        json!([{ "resource": DEVOPS, "made": true }]),
        "expected the stand-in made by this first run"
    );
    assert_eq!(body["warnings"], json!([]), "{}", requested.describe());
    assert!(
        body["headers"]["content-type"]
            .as_array()
            .is_some_and(|v| v.iter().any(|t| t
                .as_str()
                .is_some_and(|t| t.starts_with("application/json")))),
        "expected the response's content-type among `headers`\n{}",
        requested.describe()
    );
    let operations = requested.operations();
    assert_eq!(
        operations.len(),
        1,
        "expected the request's one operation\n{}",
        requested.describe()
    );
    let id = requested.request_id();
    assert_entry(
        &operations[0],
        &Expected {
            rows: Some(0),
            ..Expected::allowed(&id, BUILDER, APP)
        },
        "the builder's read, in the command's own output",
    );
    // The shared database itself still holds its rows: nothing of it was
    // read for the stand-in.
    assert_eq!(
        board
            .scenario
            .db
            .superuser()
            .value("SELECT count(*) FROM incidents")
            .as_deref(),
        Some("3")
    );
    // A second run makes nothing again.
    let again = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        again.body["standins"],
        json!([{ "resource": DEVOPS, "made": false }]),
        "{}",
        again.describe()
    );
    assert_eq!(again.version(), 1, "expected the same local Version");
}

#[test]
fn as_outsider_the_refusal_is_the_one_the_deployed_application_gives() {
    let board = Board::start("read", "read");
    let local = board.request_ok("/incidents", OUTSIDER, &[]);
    assert_refused(&local, &no_grant(OUTSIDER, None), "outsider, locally");
    let operations = local.operations();
    assert_eq!(operations.len(), 1, "{}", local.describe());
    assert_entry(
        &operations[0],
        &Expected::refused(&local.request_id(), OUTSIDER, APP, "no-grant"),
        "the refusal, in the command's own output",
    );

    // The same bundle deployed, asked by a User with no Grant.
    init_repository(board.dir());
    commit_all(board.dir(), "A board that reads devops");
    blackbox::deploy::Deployed::of(&deploy(&board.local.machine, board.dir(), &[]));
    let deployed = board
        .scenario
        .vm
        .get_as(&User::new(OUTSIDER), APP, "/incidents");
    assert_eq!(deployed.status, 403, "{}", deployed.describe());
    let deployed_body = deployed.json();
    assert_eq!(
        local.json()["refusal"],
        deployed_body["refusal"],
        "expected the local refusal to be the deployed Application's, field for field"
    );
    assert_eq!(
        local.json()["fix"],
        deployed_body["fix"],
        "expected the same fix"
    );
}

#[test]
fn a_builder_whose_grant_is_structure_only_gets_the_refusal_of_a_user_who_may_not_read_rows() {
    let board = Board::start("read", "structure");
    assert_eq!(
        dev_json(board.dir())["grants"],
        json!([{ "group": "builder", "resource": DEVOPS, "access": "structure" }]),
        "expected the pull to have copied the structure-only Grant"
    );
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    assert_refused(
        &requested,
        &no_grant(BUILDER, Some("structure")),
        "the builder with a structure-only Grant",
    );
}

#[test]
fn the_grants_of_vm_dev_json_decide_each_run() {
    let board = Board::start("read", "read");
    let mut file = dev_json(board.dir());
    // outsider joins a Group that is granted read: the next run serves it.
    for user in file["users"].as_array_mut().unwrap() {
        if user["id"] == OUTSIDER {
            user["groups"] = json!(["visitors"]);
        }
    }
    file["grants"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "group": "visitors", "resource": DEVOPS, "access": "read" }));
    write_dev_json(board.dir(), &file);
    let allowed = board.request_ok("/incidents", OUTSIDER, &[]);
    assert_eq!(
        allowed.json(),
        json!({ "user": OUTSIDER, "incidents": [] }),
        "expected a Grant added to vm.dev.json to decide the next run\n{}",
        allowed.describe()
    );
    // The Grant removed: gone at the next run.
    file["grants"]
        .as_array_mut()
        .unwrap()
        .retain(|g| g["group"] != "visitors");
    write_dev_json(board.dir(), &file);
    let refused = board.request_ok("/incidents", OUTSIDER, &[]);
    assert_refused(
        &refused,
        &no_grant(OUTSIDER, None),
        "outsider after its Group's Grant was removed",
    );
    // The builder's own Grant edited to structure: refused as such.
    for grant in file["grants"].as_array_mut().unwrap() {
        if grant["group"] == "builder" {
            grant["access"] = json!("structure");
        }
    }
    write_dev_json(board.dir(), &file);
    let lowered = board.request_ok("/incidents", BUILDER, &[]);
    assert_refused(
        &lowered,
        &no_grant(BUILDER, Some("structure")),
        "the builder after their Grant in vm.dev.json was edited to structure",
    );
}

#[test]
fn it_needs_no_sign_in_and_asks_the_installation_nothing() {
    let Board {
        local,
        project,
        scenario,
    } = Board::start("read", "read");
    local.machine.run(&["sign-out", "--json"]).ok_json();
    let Scenario { vm, db } = scenario;
    drop(vm);
    let requested = local.request_ok(&project.dir, "/incidents", BUILDER, &[]);
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the run to work signed out and with no installation reachable\n{}",
        requested.describe()
    );
    drop(db);
}

#[test]
fn a_500_is_a_result_and_uncaught_holds_the_error() {
    let board = Board::start("read", "read");
    let requested = board.request_ok("/fail", BUILDER, &[]);
    assert_eq!(
        requested.status(),
        500,
        "expected the program's 500 for an uncaught error, with exit code 0\n{}",
        requested.describe()
    );
    let uncaught = requested.body["uncaught"].as_array().unwrap();
    assert_eq!(uncaught.len(), 1, "{}", requested.describe());
    assert!(
        uncaught[0].as_str().unwrap().contains(FAILURE),
        "expected the error's message in `uncaught`\n{}",
        requested.describe()
    );
}

#[test]
fn printed_has_the_lines_printed_while_loading_then_the_requests_own() {
    let board = Board::start("read", "read");
    let requested = board.request_ok("/print", BUILDER, &[]);
    assert_eq!(
        requested.body["printed"],
        json!([
            { "level": "log", "message": LOADING_LINE, "loading": true },
            { "level": "log", "message": "handling print", "loading": false },
            { "level": "warn", "message": "a warning line", "loading": false },
        ]),
        "expected what the request's log entry holds\n{}",
        requested.describe()
    );
    assert_eq!(requested.text(), "printed");
}

#[test]
fn a_body_that_is_not_utf8_is_given_in_base64() {
    let board = Board::start("read", "read");
    let requested = board.request_ok("/bytes", BUILDER, &[]);
    assert_eq!(
        requested.body["body"],
        Value::Null,
        "{}",
        requested.describe()
    );
    assert_eq!(
        requested.body["body_base64"],
        json!("//4AQQ=="),
        "expected the bytes ff fe 00 41 in base64\n{}",
        requested.describe()
    );
}

#[test]
fn at_most_one_mebibyte_of_the_body_is_printed() {
    let board = Board::start("read", "read");
    let whole = board.request_ok("/big?n=1048576", BUILDER, &[]);
    assert_eq!(whole.text().len(), 1_048_576);
    assert_eq!(whole.body["body_truncated"], json!(false));
    let cut = board.request_ok("/big?n=1048577", BUILDER, &[]);
    assert_eq!(
        cut.text().len(),
        1_048_576,
        "expected the body cut to 1 MiB"
    );
    assert_eq!(
        cut.body["body_truncated"],
        json!(true),
        "expected `body_truncated` true for a longer body"
    );
}

#[test]
fn headers_are_by_lower_case_name_with_their_values_in_order() {
    let board = Board::start("read", "read");
    let requested = board.request_ok("/headers", BUILDER, &[]);
    assert_eq!(
        requested.body["headers"]["x-many"],
        json!(["1", "2"]),
        "{}",
        requested.describe()
    );
    assert_eq!(
        requested.body["headers"]["x-one"],
        json!(["a"]),
        "{}",
        requested.describe()
    );
}

#[test]
fn in_words_the_status_headers_body_printed_lines_and_operations_are_said() {
    let board = Board::start("read", "read");
    let run = board
        .local
        .request_in_words(board.dir(), "/incidents", OUTSIDER, &[]);
    let words = run.ok_text();
    let first = words.lines().next().unwrap_or_default();
    assert!(
        first.contains("403"),
        "expected the status first, got\n{words}"
    );
    assert!(
        words.to_lowercase().contains("content-type"),
        "expected the headers, got\n{words}"
    );
    assert!(
        words.contains("\n\n"),
        "expected an empty line before the body, got\n{words}"
    );
    assert!(
        words.contains("no-grant") && words.contains(DEVOPS),
        "expected the body, then one line for the operation with its Resource and the refusal's reason, got\n{words}"
    );
    assert!(
        words.contains(LOADING_LINE),
        "expected what was printed, got\n{words}"
    );
    assert!(
        words
            .lines()
            .any(|l| l.contains(DEVOPS) && l.contains("refused") && l.contains("no-grant")),
        "expected one line for the operation: its Resource, its outcome and the refusal's reason, got\n{words}"
    );
}

#[test]
fn what_the_application_answers_reaches_the_terminal_escaped_with_its_line_ends() {
    let board = Board::start("read", "read");
    let words = board
        .local
        .request_in_words(board.dir(), "/text", BUILDER, &[])
        .ok_text();
    assert!(
        !words.contains('\u{1b}'),
        "expected no raw escape character in the words, got {words:?}"
    );
    assert!(
        words.contains("line one\nline two"),
        "expected the body's line ends kept, got {words:?}"
    );
    let json = board.request("/text", BUILDER, &[]);
    assert!(
        !json.stdout.contains('\u{1b}'),
        "expected the escape character written as a JSON escape, got {:?}",
        json.stdout
    );
    assert_eq!(
        Requested::of(&json).text(),
        TEXT_BODY,
        "expected the JSON to parse to the body as it was"
    );
}

#[test]
fn path_is_the_path_as_it_was_given_with_its_query() {
    let local = Local::new();
    let project = local_project(&local.machine, "query-board", &[]);
    let given = "/echo?a=1&b=two%20words&a=3";
    let requested = local.request_ok(&project.dir, given, OUTSIDER, &[]);
    assert_eq!(
        requested.body["path"],
        json!(given),
        "expected `path` as given, with its query\n{}",
        requested.describe()
    );
    assert_eq!(
        requested.json()["search"],
        json!("?a=1&b=two%20words&a=3"),
        "expected the Application given the query as it was\n{}",
        requested.describe()
    );
}
