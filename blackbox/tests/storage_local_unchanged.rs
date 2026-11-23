//! Nothing else changes for a project with no Storage (CONTRACT.md,
//! "Storage: The local loop", last bullet: "its local runs are in
//! everything else as before this section: the same refusals in the same
//! order, the same `warnings` in the same order").
//!
//! The contract's "What a run does" gives the order of the steps (the
//! stand-ins, then the Application: a Manifest the tooling cannot read is
//! `check-failed`, a project that does not bundle `build-failed`, then
//! the test rows) but not the order of two problems met in the same
//! step, nor the order of the warnings of two Resources. Where it does
//! not, the outcomes below are what the program built before the Storage
//! ticket did (`VIBE_BIN=<that program's debug/vibe> cargo test ...`
//! gave the same results for this file), kept as they were.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;

use blackbox::deploy::BUILDERS;
use blackbox::local::{APP, BUILDER, Board, DEVOPS, declaring};

/// A project of two pulled databases, `devops` and `inventory`, with
/// `inventory`'s `standin.sql` removed (a warning, `stand-in-not-pulled`)
/// and a statement added to `devops`'s that the stand-in cannot run (a
/// warning, `stand-in-incomplete`).
fn two_databases() -> Board {
    let board = Board::start("read", "read");
    board.scenario.register_writable("inventory");
    board.scenario.grant("inventory", BUILDERS, "read");
    board.project.write(
        "manifest.ts",
        &declaring(APP, &[(DEVOPS, "read"), ("inventory", "read")]),
    );
    board.local.pull_ok(board.dir(), "inventory");
    let file = board.dir().join("resources/devops/standin.sql");
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str("COPY (SELECT 1) TO '/tmp/vibe-blackbox-never-written';\n\n");
    fs::write(&file, text).unwrap();
    fs::remove_file(board.dir().join("resources/inventory/standin.sql")).unwrap();
    board
}

#[test]
fn the_warnings_of_the_pulled_resources_come_in_the_order_they_came_before_storage() {
    let board = two_databases();
    let run = board.local.request(board.dir(), "/incidents", BUILDER, &[]);
    run.expect_code(0);
    let reasons: Vec<(String, String)> = run.json()["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["reason"].as_str().unwrap_or_default().to_owned(),
                w["resource"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [
            ("stand-in-not-pulled".to_owned(), "inventory".to_owned()),
            ("stand-in-incomplete".to_owned(), "devops".to_owned()),
        ],
        "expected the warnings in the order the program gave them before Storage\n{}",
        run.describe()
    );
}

#[test]
fn two_problems_of_a_project_end_the_run_with_the_one_the_program_met_first_before_storage() {
    let board = two_databases();
    let good_manifest = fs::read_to_string(board.dir().join("manifest.ts")).unwrap();
    let good_index = fs::read_to_string(board.dir().join("src/index.ts")).unwrap();
    // A Manifest the tooling cannot read: not a plain literal.
    let broken_manifest = good_manifest.replace("access: \"read\"", "access: (\"re\" + \"ad\")");
    assert_ne!(broken_manifest, good_manifest, "the harness's own check");
    // A project that does not bundle.
    let broken_index = format!("import gone from \"./gone.ts\";\nconsole.log(gone);\n{good_index}");
    fs::create_dir_all(board.dir().join("test-rows")).unwrap();
    let rows = board.dir().join("test-rows/devops.sql");
    // A `standin.sql` that is not the tool's (a hand-written one).
    let by_hand = "-- written by hand\nCREATE TABLE mine (id integer);\n";
    fs::write(board.dir().join("resources/devops/standin.sql"), by_hand).unwrap();
    for (case, manifest, index, failing_rows, reason) in [
        (
            "manifest and test rows",
            &broken_manifest,
            &good_index,
            true,
            "check-failed",
        ),
        (
            "build and test rows",
            &good_manifest,
            &broken_index,
            true,
            "build-failed",
        ),
        (
            "manifest and build",
            &broken_manifest,
            &broken_index,
            false,
            "check-failed",
        ),
        (
            "manifest and a standin.sql by hand",
            &broken_manifest,
            &good_index,
            false,
            "check-failed",
        ),
        (
            "build and a standin.sql by hand",
            &good_manifest,
            &broken_index,
            false,
            "build-failed",
        ),
    ] {
        board.project.write("manifest.ts", manifest);
        board.project.write("src/index.ts", index);
        if failing_rows {
            fs::write(&rows, "INSERT INTO nothere VALUES (1);\n").unwrap();
        } else {
            drop(fs::remove_file(&rows));
        }
        let run = board.local.request(board.dir(), "/incidents", BUILDER, &[]);
        assert_eq!(run.code, Some(1), "{case}: {}", run.describe());
        assert_eq!(
            run.json()["error"]["reason"],
            serde_json::json!(reason),
            "{case}: expected the refusal the program gave before Storage\n{}",
            run.describe()
        );
    }
}
