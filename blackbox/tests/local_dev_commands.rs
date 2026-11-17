//! The tool's own local commands while `vm dev` runs in the project
//! (CONTRACT.md, "The local loop, kept running: the browser, test rows and
//! reset": "The tool's own commands while it runs"; ticket "Local loop in
//! the browser, with test rows, reset and working offline").
//!
//! A command answered by `vm dev` starts no Postgres of its own: the one
//! `vm dev` runs is the same before and after, and the request it sent is
//! among `vm dev`'s `request` lines. Each command is given `--timeout 60`,
//! so that one that waited for `vm dev` to end instead would end
//! `request-timed-out`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use blackbox::dev::{Dev, local_files_holding, random_looking_runs};
use blackbox::generate::{Generated, generate};
use blackbox::local::{APP, BUILDER, Board, OUTSIDER, Requested, handler, port_open};
use blackbox::project::{NOTES_SCHEMA_TS, is_running};
use blackbox::tool::mode_of;
use serde_json::json;

const WAIT: [&str; 2] = ["--timeout", "60"];

/// A [`Board`] whose own database has `notes`, migrated by `0000_add_notes`.
fn board_with_notes() -> Board {
    let board = Board::start("read", "read");
    board.project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    board.project.write("src/index.ts", &handler(true, true));
    Generated::made(
        &generate(&board.local.machine, board.dir(), &["--name", "add_notes"]),
        board.dir(),
    );
    board
}

/// Fails unless `vm dev` served the request `id` (a `request` line).
fn assert_served_by(dev: &Dev, id: &str, context: &str) {
    let served = dev.events("request").iter().any(|l| l["request_id"] == id);
    assert!(
        served,
        "{context}: expected the request {id} served by `vm dev` (one of its `request` lines)\n{}",
        dev.stdout()
    );
}

#[test]
fn vm_request_is_answered_by_it_as_the_user_as_names_whatever_the_browser_chose() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/incidents").status, 403);
    let requested = Requested::of(&board.request("/incidents", BUILDER, &WAIT));
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the request as the User --as names\n{}",
        requested.describe()
    );
    assert_eq!(requested.body["as"], json!(BUILDER));
    assert_eq!(requested.body["application"], json!(APP));
    assert_eq!(requested.version(), 1);
    assert_eq!(requested.operations().len(), 1, "{}", requested.describe());
    assert_eq!(
        requested.body["standins"],
        json!([]),
        "expected `standins` empty: the request caused no rebuild"
    );
    assert_eq!(requested.body["migrations_applied"], json!([]));
    assert_eq!(requested.test_rows(), json!([]));
    assert_served_by(&dev, &requested.request_id(), "vm request");
    let now = dev.postmaster();
    assert_eq!(
        now.pid, postgres.pid,
        "expected no second Postgres: the command was answered by `vm dev`"
    );
    assert!(is_running(postgres.pid));
    // The browser's choice is its own.
    assert_eq!(browser.get("/incidents").status, 403);
    dev.stop("INT").expect_code(0);
}

#[test]
fn vm_request_while_it_runs_has_the_same_reasons_and_exit_codes_as_alone() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let run = board.request("/incidents", "nobody-here", &WAIT);
    let message = run.json_error(1, "no-such-test-user");
    assert!(
        message.contains(BUILDER) && message.contains(OUTSIDER),
        "{message}"
    );
    let run = board.request("incidents", BUILDER, &WAIT);
    assert_eq!(run.code, Some(2), "{}", run.describe());
    dev.stop("INT").expect_code(0);
}

#[test]
fn what_a_request_while_it_runs_rebuilt_is_in_the_output_of_vm_request() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    board.project.write(
        "test-rows/devops.sql",
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n",
    );
    let requested = Requested::of(&board.request("/incidents", BUILDER, &WAIT));
    assert_eq!(
        requested.test_rows(),
        json!([{ "file": "test-rows/devops.sql", "rows": 1 }]),
        "expected the test rows the rebuild it caused loaded\n{}",
        requested.describe()
    );
    assert_eq!(
        requested.json()["incidents"],
        json!([{ "id": 1, "title": "Printer on fire" }])
    );
    assert_served_by(&dev, &requested.request_id(), "vm request");
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_local_operation_log_and_change_history_are_answered_by_it() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let request = browser
        .build("POST", "/notes")
        .body("written in the browser");
    let written = browser.send(&request);
    assert_eq!(written.status, 200, "{}", written.describe());
    let id = written.request_id();
    let entries = board.local.operation_log(board.dir(), &WAIT).ok_json()["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        entries.iter().any(|e| e["request"] == id.as_str()),
        "expected the browser's request in the local Operation log, got {entries:#?}"
    );
    let changes = board.local.change_history(board.dir(), &WAIT).ok_json()["changes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        changes
            .iter()
            .any(|c| c["request"] == id.as_str() && c["new"]["body"] == "written in the browser"),
        "expected the browser's write in the local Change history, got {changes:#?}"
    );
    assert_eq!(
        dev.postmaster().pid,
        postgres.pid,
        "expected no second Postgres"
    );
    dev.stop("INT").expect_code(0);
}

/// Requests to `/_vm/control/...` that do not hold the secret, each with
/// the status it must get: `404` at the Application's name, `307` at the
/// loopback addresses (sent on to the Application's name, where it is a
/// `404` again).
fn without_the_secret(dev: &Dev) -> Vec<(blackbox::Request, u16)> {
    let mut requests = Vec::new();
    for path in [
        "/_vm/control/",
        "/_vm/control/request",
        "/_vm/control/reset",
        "/_vm/control/operation-log",
        "/_vm/control/change-history",
        "/_vm/control/x/../reset",
    ] {
        for method in ["GET", "POST"] {
            requests.push((dev.request(path).method(method), 404));
            requests.push((
                dev.request(path)
                    .method(method)
                    .header("Authorization", "Bearer wrong-secret-0123456789abcdef")
                    .header("x-vm-control-secret", "wrong-secret-0123456789abcdef")
                    .header("Cookie", "vm_test_user=builder")
                    .json(&json!({ "path": "/notes", "as": "builder", "secret": "wrong" })),
                404,
            ));
        }
    }
    for host in ["127.0.0.1", "localhost"] {
        requests.push((
            blackbox::Request::new(
                dev.address(),
                "/_vm/control/reset",
                &format!("{host}:{}", dev.port),
            )
            .method("POST"),
            307,
        ));
    }
    requests
}

#[test]
fn the_control_paths_answer_404_without_the_secret_and_do_nothing() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let request = browser.build("POST", "/notes").body("kept");
    assert_eq!(browser.send(&request).status, 200);
    for (request, expected) in without_the_secret(&dev) {
        let response = request.send();
        assert_eq!(
            response.status,
            expected,
            "{request}: expected {expected} without the secret\n{}",
            response.describe()
        );
    }
    assert_eq!(
        browser.get("/notes").json()["notes"],
        json!([{ "id": 1, "body": "kept" }]),
        "expected nothing reset by a request without the secret"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_file_that_finds_it_is_0600_and_its_secret_is_in_no_output_log_or_page() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let found = local_files_holding(
        board.dir(),
        Some(&postgres.data),
        dev.port.to_string().as_bytes(),
    );
    assert!(
        !found.is_empty(),
        "expected a file of .vm/local/ holding the port {} of `vm dev`",
        dev.port
    );
    for file in &found {
        assert_eq!(
            mode_of(file),
            0o600,
            "expected {} of mode 0600",
            file.display()
        );
    }
    let known: Vec<String> = [
        board.dir().to_string_lossy().into_owned(),
        board.local.machine.root().to_string_lossy().into_owned(),
    ]
    .to_vec();
    let secrets: Vec<String> = found
        .iter()
        .flat_map(|f| random_looking_runs(&fs::read(f).unwrap(), &known))
        .collect();
    assert!(
        !secrets.is_empty(),
        "expected a secret made from the system's randomness in {found:?}: the harness looks for a run of 16 or more letters and digits"
    );
    // What a run shows: pages, answers, `vm request`, the records.
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut browser = dev.browser();
    seen.push(("the page".to_owned(), browser.get("/_vm/test-user").text()));
    browser.choose(BUILDER);
    seen.push(("an answer".to_owned(), browser.get("/notes").describe()));
    seen.push((
        "a 404".to_owned(),
        dev.request("/_vm/control/").send().describe(),
    ));
    seen.push((
        "a 421".to_owned(),
        dev.request("/").host("evil.example").send().describe(),
    ));
    let requested = board.request("/echo", BUILDER, &WAIT);
    seen.extend(requested.texts());
    seen.extend(board.local.operation_log(board.dir(), &WAIT).texts());
    seen.extend(board.local.change_history(board.dir(), &WAIT).texts());
    let run = dev.stop("INT");
    run.expect_code(0);
    seen.extend(run.texts());
    for log in ["vm.log", "postgres.log"] {
        let path = board.dir().join(".vm/local").join(log);
        seen.push((
            log.to_owned(),
            String::from_utf8_lossy(&fs::read(&path).unwrap_or_default()).into_owned(),
        ));
    }
    for secret in &secrets {
        for (whence, text) in &seen {
            assert!(
                !text.contains(secret.as_str()),
                "expected the secret of the file that finds `vm dev` in no output, log or page; {whence} holds it"
            );
        }
    }
}

#[test]
fn a_file_left_by_a_vm_dev_killed_outright_is_not_believed() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let port = dev.port;
    let killed = dev.kill();
    assert_eq!(
        killed.code, None,
        "the harness's own check: killed outright"
    );
    // Something else takes the port the file names: it must not be asked.
    let impostor = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .unwrap_or_else(|e| panic!("the harness's own check: cannot take {port} back: {e}"));
    impostor.set_nonblocking(true).unwrap();
    let requested = Requested::of(&board.request("/incidents", BUILDER, &["--timeout", "120"]));
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the command to run alone\n{}",
        requested.describe()
    );
    assert!(
        impostor.accept().is_err(),
        "expected no connection to the port a killed `vm dev` left in its file"
    );
    drop(impostor);
    assert!(!port_open(port));
    let left = board.local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running after the command, found {left:#?}"
    );
    // And a new `vm dev` starts.
    let again = Dev::start(&board.local, board.dir(), APP);
    again.stop("INT").expect_code(0);
}
