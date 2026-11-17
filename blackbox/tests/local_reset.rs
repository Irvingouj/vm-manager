//! `vm local reset`: the local databases started over (CONTRACT.md, "The
//! local loop, kept running: the browser, test rows and reset": "Starting
//! over"; ticket "Local loop in the browser, with test rows, reset and
//! working offline": reset empties the local own database and applies the
//! migrations again).
//!
//! A project declaring `devops` (pulled), whose own database has `notes`,
//! with a file of test rows for each.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;
use std::thread;
use std::time::Duration;

use blackbox::dev::Dev;
use blackbox::generate::{Generated, generate};
use blackbox::local::{
    APP, BUILDER, Board, DEVOPS, Local, Mirror, OUTSIDER, Serving, handler, local_project,
};
use blackbox::project::{NOTES_SCHEMA_TS, Tree};
use blackbox::tool::Run;
use serde_json::{Value, json};

const DEVOPS_ROWS: &str = "test-rows/devops.sql";
const OWN_ROWS: &str = "test-rows/own-database.sql";

/// The fields of what `vm local reset --json` prints, sorted.
const RESET_KEYS: [&str; 8] = [
    "application",
    "migrations_applied",
    "notes",
    "reset",
    "standins",
    "test_rows",
    "version",
    "warnings",
];

/// Makes the project of `board` one whose own database has `notes`, with a
/// file of test rows for `devops` and one for the own database.
fn with_notes_and_rows(board: &Board) {
    board.project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    board.project.write("src/index.ts", &handler(true, true));
    Generated::made(
        &generate(&board.local.machine, board.dir(), &["--name", "add_notes"]),
        board.dir(),
    );
    board.project.write(
        DEVOPS_ROWS,
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n",
    );
    board.project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('from the file');\n",
    );
}

/// `vm local reset --project <dir> --json <extra>`.
fn reset(local: &Local, dir: &Path, extra: &[&str]) -> Run {
    local.watch(dir);
    let mut args = vec![
        "local",
        "reset",
        "--project",
        dir.to_str().unwrap(),
        "--json",
    ];
    args.extend_from_slice(extra);
    local.run(&args, &[])
}

/// What `vm local reset --json` printed, which must end with `0` and have
/// exactly the contract's fields.
fn reset_ok(local: &Local, dir: &Path, extra: &[&str]) -> Value {
    let run = reset(local, dir, extra);
    let body = run.ok_json();
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, RESET_KEYS, "{}", run.describe());
    assert_eq!(body["reset"], json!(true), "{}", run.describe());
    body
}

fn note_bodies(value: &Value) -> Vec<String> {
    value["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected notes, got {value}"))
        .iter()
        .map(|n| n["body"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn reset_starts_the_local_databases_over_and_leaves_the_project_and_postgres_alone() {
    let mirror = Mirror::start(Serving::Archive);
    let board = Board::on(
        Local::new().without_bin().with_mirror(&mirror.base_url()),
        APP,
        "write",
        "write",
        "",
    );
    with_notes_and_rows(&board);
    let first = board.request_ok("/notes", BUILDER, &["--data", "written by a run"]);
    assert_eq!(first.status(), 200, "{}", first.describe());
    board.request_ok(
        "/add-incident",
        BUILDER,
        &["--data", "written into the stand-in"],
    );
    // A second local Version.
    let index = board.project.path("src/index.ts");
    let code = std::fs::read_to_string(&index).unwrap();
    std::fs::write(&index, format!("{code}\n// a second Version\n")).unwrap();
    assert_eq!(board.request_ok("/notes", BUILDER, &[]).version(), 2);
    let downloads = mirror.requests().len();
    assert!(
        downloads >= 1,
        "the harness's own check: Postgres was downloaded"
    );
    let postgres_before = Tree::of(&board.local.postgres_folder());
    let project_before = Tree::of_skipping(board.dir(), &[".vm/local", "node_modules"]);

    let body = reset_ok(&board.local, board.dir(), &[]);
    assert_eq!(body["application"], json!(APP));
    assert_eq!(
        body["version"],
        json!(1),
        "expected the Version numbered from 1"
    );
    assert_eq!(body["migrations_applied"], json!(["0000_add_notes"]));
    assert_eq!(
        body["standins"],
        json!([{ "resource": DEVOPS, "made": true }])
    );
    assert_eq!(
        body["test_rows"],
        json!([
            { "file": DEVOPS_ROWS, "rows": 1 },
            { "file": OWN_ROWS, "rows": 1 }
        ])
    );

    assert_eq!(
        mirror.requests().len(),
        downloads,
        "expected nothing downloaded again"
    );
    assert_eq!(
        postgres_before.differences(&Tree::of(&board.local.postgres_folder())),
        Vec::<String>::new(),
        "expected the programs of Postgres in the user's own folder untouched"
    );
    assert_eq!(
        project_before.differences(&Tree::of_skipping(
            board.dir(),
            &[".vm/local", "node_modules"]
        )),
        Vec::<String>::new(),
        "expected vm.dev.json, the test rows and every other file of the project untouched"
    );

    // The local records of the earlier runs are gone.
    let entries = board.local.operation_log(board.dir(), &[]).ok_json()["entries"].clone();
    assert_eq!(entries, json!([]), "expected the local Operation log empty");
    let changes = board.local.change_history(board.dir(), &[]).ok_json()["changes"].clone();
    assert!(
        changes
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["user_id"] == "test-rows"),
        "expected only the test rows' changes in the local Change history, got {changes:#}"
    );

    let notes = board.request_ok("/notes", BUILDER, &[]);
    assert_eq!(
        note_bodies(&notes.json()),
        ["from the file"],
        "expected the own database empty but for its test rows\n{}",
        notes.describe()
    );
    assert_eq!(notes.version(), 1, "expected Version 1 again");
    assert_eq!(notes.migrations_applied(), Vec::<String>::new());
    let incidents = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        incidents.json()["incidents"],
        json!([{ "id": 1, "title": "Printer on fire" }]),
        "expected the stand-in made again with its test rows, and what the Application wrote gone"
    );
    let left = board.local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running, found {left:#?}"
    );
}

#[test]
fn in_words_reset_says_what_it_did() {
    let board = Board::start("read", "read");
    with_notes_and_rows(&board);
    board.request_ok("/notes", BUILDER, &[]);
    board.local.watch(board.dir());
    let words = board
        .local
        .run(
            &["local", "reset", "--project", board.dir().to_str().unwrap()],
            &[],
        )
        .ok_text();
    for said in ["0000_add_notes", DEVOPS_ROWS, OWN_ROWS, DEVOPS] {
        assert!(
            words.contains(said),
            "expected the words to name {said}, got\n{words}"
        );
    }
}

#[test]
fn reset_in_a_project_that_never_ran_does_a_first_run_from_nothing() {
    let local = Local::new();
    let project = local_project(&local.machine, "fresh-board", &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    assert!(!project.dir.join(".vm/local").exists());
    let body = reset_ok(&local, &project.dir, &[]);
    assert_eq!(body["application"], json!("fresh-board"));
    assert_eq!(body["version"], json!(1));
    assert_eq!(body["migrations_applied"], json!(["0000_add_notes"]));
    assert_eq!(body["standins"], json!([]));
    assert_eq!(body["test_rows"], json!([]));
    let next = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(next.version(), 1);
    assert_eq!(next.migrations_applied(), Vec::<String>::new());
    assert_eq!(next.json()["notes"], json!([]));
}

#[test]
fn a_reset_that_fails_fails_as_a_run_fails() {
    let local = Local::new();
    let project = local_project(&local.machine, "broken-board", &[]);
    std::fs::write(project.dir.join("vm.dev.json"), "{ \"users\": 7 }\n").unwrap();
    reset(&local, &project.dir, &[]).json_error(1, "test-users-file");
    let elsewhere = local.machine.root().join("not-a-project");
    std::fs::create_dir_all(&elsewhere).unwrap();
    reset(&local, &elsewhere, &[]).json_error(1, "not-a-project");
}

#[test]
fn while_vm_dev_runs_the_reset_is_done_by_it_and_it_goes_on_serving_the_same_user() {
    let board = Board::start("read", "read");
    with_notes_and_rows(&board);
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let request = browser.build("POST", "/notes").body("before the reset");
    assert_eq!(browser.send(&request).status, 200);
    assert_eq!(
        note_bodies(&browser.get("/notes").json()),
        ["from the file", "before the reset"]
    );

    // A request under way, the reset, and a request sent meanwhile.
    let slow = browser.build("GET", "/slow?ms=6000");
    let meanwhile = browser.build("GET", "/notes");
    let (slow, body, late) = thread::scope(|scope| {
        let slow = scope.spawn(|| slow.send());
        thread::sleep(Duration::from_millis(1000));
        let reset = scope.spawn(|| reset_ok(&board.local, board.dir(), &["--timeout", "120"]));
        thread::sleep(Duration::from_millis(2000));
        let late = scope.spawn(|| meanwhile.send());
        (
            slow.join().unwrap(),
            reset.join().unwrap(),
            late.join().unwrap(),
        )
    });
    assert_eq!(
        slow.status,
        200,
        "expected the request under way finished\n{}",
        slow.describe()
    );
    assert_eq!(slow.text(), "slow");
    assert_eq!(body["version"], json!(1));
    assert_eq!(body["migrations_applied"], json!(["0000_add_notes"]));
    assert_eq!(
        late.status,
        200,
        "expected the request sent meanwhile answered after the reset\n{}",
        late.describe()
    );
    assert_eq!(
        note_bodies(&late.json()),
        ["from the file"],
        "expected the request sent meanwhile to have waited for the reset"
    );
    // The same address, the same User.
    let after = browser.get("/me");
    assert_eq!(
        after.status,
        200,
        "expected the chosen test User still chosen\n{}",
        after.describe()
    );
    assert_eq!(after.json()["id"], json!(BUILDER));
    assert!(!dev.events("listening").is_empty());
    dev.stop("INT").expect_code(0);
}
