//! `vibe local operation-log` and `vibe local change-history` (CONTRACT.md,
//! "The local loop: one request as a test User: Reading what was recorded
//! locally"; ticket "Local loop: run the Application on the Builder's
//! machine and send a request as a test User": the local Operation log and
//! Change history can be read with the tool).
//!
//! A project made by `vibe new` declaring `devops` (pulled), whose own
//! database has `notes`, and whose handler reads `devops` and writes notes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_recent_rfc3339;
use blackbox::generate::{Generated, generate};
use blackbox::local::{APP, BUILDER, Board, DEVOPS, Local, Mirror, OUTSIDER, Serving, handler};
use blackbox::project::NOTES_SCHEMA_TS;
use blackbox::scenario::{Expected, assert_entry};
use blackbox::tool::Run;
use serde_json::{Value, json};

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

fn entries(run: &Run) -> Vec<Value> {
    let body = run.ok_json();
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["entries"], "{}", run.describe());
    body["entries"].as_array().unwrap().clone()
}

fn changes(run: &Run) -> Vec<Value> {
    let body = run.ok_json();
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["changes"], "{}", run.describe());
    body["changes"].as_array().unwrap().clone()
}

/// The fields of a change, sorted.
const CHANGE_KEYS: [&str; 14] = [
    "actor",
    "at",
    "changed",
    "id",
    "key",
    "kind",
    "migration",
    "new",
    "old",
    "request",
    "schema",
    "structure",
    "table",
    "user_id",
];

#[test]
fn a_project_that_never_ran_has_empty_records_and_nothing_is_made_or_downloaded() {
    let mirror = Mirror::start(Serving::Archive);
    let local = Local::new().without_bin().with_mirror(&mirror.base_url());
    let project = blackbox::local::local_project(&local.machine, "never-ran", &[]);
    assert_eq!(
        entries(&local.operation_log(&project.dir, &[])),
        Vec::<Value>::new()
    );
    assert_eq!(
        changes(&local.change_history(&project.dir, &[])),
        Vec::<Value>::new()
    );
    assert!(
        !project.dir.join(".vibe/local").exists(),
        "expected nothing made in the project"
    );
    assert!(
        !local.postgres_folder().exists() && mirror.requests().is_empty(),
        "expected nothing downloaded, got requests {:?}",
        mirror.requests()
    );
    // In words, it says so.
    let words = local
        .run(
            &[
                "local",
                "operation-log",
                "--project",
                project.dir.to_str().unwrap(),
            ],
            &[],
        )
        .ok_text();
    assert!(!words.trim().is_empty());
}

#[test]
fn the_local_operation_log_has_each_runs_operations_oldest_first() {
    let board = board_with_notes();
    let read = board.request_ok("/incidents", BUILDER, &[]);
    let refused = board.request_ok("/incidents", OUTSIDER, &[]);
    let wrote = board.request_ok("/notes", BUILDER, &["--data", "a note"]);
    let all = entries(&board.local.operation_log(board.dir(), &[]));
    assert_eq!(
        all.len(),
        4,
        "expected the read, the refusal, and the note's insert and read\n{all:#?}"
    );
    assert_entry(
        &all[0],
        &Expected {
            rows: Some(0),
            ..Expected::allowed(&read.request_id(), BUILDER, APP)
        },
        "the builder's read",
    );
    assert_entry(
        &all[1],
        &Expected::refused(&refused.request_id(), OUTSIDER, APP, "no-grant"),
        "outsider's refusal",
    );
    assert_entry(
        &all[2],
        &Expected {
            rows: Some(1),
            ..Expected::own(&wrote.request_id(), BUILDER, APP, "user", "execute")
        },
        "the note's insert",
    );
    assert_entry(
        &all[3],
        &Expected::own(&wrote.request_id(), BUILDER, APP, "user", "query"),
        "the note's read",
    );
    // Each entry as the command's own output had it.
    assert_eq!(all[0], read.operations()[0]);
    assert_eq!(all[1], refused.operations()[0]);
    // --limit: the newest, still oldest first.
    let newest = entries(&board.local.operation_log(board.dir(), &["--limit", "2"]));
    assert_eq!(
        newest,
        all[2..].to_vec(),
        "expected the newest 2, oldest first"
    );
    // In words, one block an entry.
    let words = board
        .local
        .run(
            &[
                "local",
                "operation-log",
                "--project",
                board.dir().to_str().unwrap(),
            ],
            &[],
        )
        .ok_text();
    for id in [read.request_id(), refused.request_id(), wrote.request_id()] {
        assert!(
            words.contains(&id),
            "expected each entry in words, with its request, got\n{words}"
        );
    }
    assert!(
        words.contains("no-grant") && words.contains(DEVOPS),
        "{words}"
    );
}

#[test]
fn the_local_change_history_has_the_change_of_a_run_right_after_it() {
    let board = board_with_notes();
    let first = board.request_ok("/notes", BUILDER, &["--data", "first"]);
    let second = board.request_ok("/notes", OUTSIDER, &["--data", "second"]);
    let all = changes(&board.local.change_history(board.dir(), &[]));
    assert_eq!(all.len(), 2, "expected the two inserts\n{all:#?}");
    for change in &all {
        let mut keys: Vec<&str> = change
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys, CHANGE_KEYS,
            "expected every field of a change: {change}"
        );
        assert!(change["id"].is_string(), "expected `id` as text: {change}");
        assert_recent_rfc3339(&change["at"], 300, "a change's `at`");
    }
    // Newest first.
    let newest = &all[0];
    assert_eq!(newest["schema"], json!("public"));
    assert_eq!(newest["table"], json!("notes"));
    assert_eq!(newest["kind"], json!("insert"));
    assert_eq!(newest["key"], json!({ "id": 2 }));
    assert_eq!(newest["old"], Value::Null);
    assert_eq!(
        newest["new"],
        json!({ "id": 2, "body": "second" }),
        "expected the row as it was recorded, by column name"
    );
    assert_eq!(newest["changed"], Value::Null);
    assert_eq!(newest["user_id"], json!(OUTSIDER));
    assert_eq!(newest["actor"], json!("user"));
    assert_eq!(newest["request"], json!(second.request_id()));
    assert_eq!(newest["migration"], Value::Null);
    assert_eq!(newest["structure"], Value::Null);
    assert_eq!(all[1]["user_id"], json!(BUILDER));
    assert_eq!(all[1]["request"], json!(first.request_id()));
    assert_eq!(all[1]["new"], json!({ "id": 1, "body": "first" }));

    // --limit and --table.
    assert_eq!(
        changes(&board.local.change_history(board.dir(), &["--limit", "1"])),
        all[..1].to_vec()
    );
    assert_eq!(
        changes(
            &board
                .local
                .change_history(board.dir(), &["--table", "notes"])
        ),
        all
    );
    assert_eq!(
        changes(
            &board
                .local
                .change_history(board.dir(), &["--table", "public.notes"])
        ),
        all
    );
    assert_eq!(
        changes(
            &board
                .local
                .change_history(board.dir(), &["--table", "other"])
        ),
        Vec::<Value>::new(),
        "expected no change of a table that has none"
    );
    // In words, one block a change.
    let words = board
        .local
        .run(
            &[
                "local",
                "change-history",
                "--project",
                board.dir().to_str().unwrap(),
            ],
            &[],
        )
        .ok_text();
    for id in [first.request_id(), second.request_id()] {
        assert!(
            words.contains(&id),
            "expected each change in words\n{words}"
        );
    }
}

#[test]
fn reading_leaves_no_process_running() {
    let board = board_with_notes();
    board.request_ok("/notes", BUILDER, &["--data", "x"]);
    entries(&board.local.operation_log(board.dir(), &[]));
    changes(&board.local.change_history(board.dir(), &[]));
    let left = board
        .local
        .processes_after(std::time::Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected the private Postgres stopped after reading, found {left:?}"
    );
}
