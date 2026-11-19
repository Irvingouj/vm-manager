//! No real row and no credential of a real Resource reaches the Builder's
//! machine (CONTRACT.md, "The local loop: one request as a test User":
//! "What a pull writes, from this ticket on", "What a run does"; ticket
//! "Local loop: run the Application on the Builder's machine and send a
//! request as a test User").
//!
//! After pulls and runs that read, are refused and write, every file the
//! machine holds (the project, its `.vibe/local/` with the private Postgres's
//! data, the user's data folder, `TMPDIR`, the home) and everything the
//! commands printed is searched, byte for byte, for the shared database's
//! logins, its name, and values of its rows.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::local::{BUILDER, Board, DEVOPS, OUTSIDER, files_holding};
use blackbox::scenario::assert_none_holds;
use blackbox::shared_database::token;

#[test]
fn no_real_row_and_no_credential_of_the_resource_is_anywhere_on_the_builders_machine() {
    let board = Board::start("write", "write");
    let marker = format!("a-real-row-{}", token());
    board.scenario.db.superuser().batch(&format!(
        "INSERT INTO incidents VALUES (77, '{marker}', true, 1, '2026-11-01 08:00:00+00')"
    ));
    let mut texts = Vec::new();
    texts.extend(board.local.pull(board.dir(), DEVOPS).texts());
    for (path, user, extra) in [
        ("/incidents", BUILDER, vec![]),
        ("/incidents", OUTSIDER, vec![]),
        ("/add-incident", BUILDER, vec!["--data", "a test row"]),
        ("/columns", BUILDER, vec![]),
    ] {
        let run = board.request(path, user, &extra);
        run.expect_code(0);
        texts.extend(run.texts());
    }
    texts.extend(board.local.operation_log(board.dir(), &[]).texts());
    texts.extend(board.local.change_history(board.dir(), &[]).texts());

    let db = &board.scenario.db;
    let mut markers = db.credential_markers();
    markers.push(("a value of a real row", marker));
    markers.push(("a value of a real row", db.row_marker.clone()));
    markers.push(("a value of a real row", "Disk full on db-7".to_owned()));
    assert_none_holds(
        &texts,
        &markers,
        "what the pull and the local commands printed",
    );

    let places = [
        board.dir().to_path_buf(),
        board.local.machine.root().to_path_buf(),
    ];
    let mut found = Vec::new();
    for place in &places {
        for (what, value) in &markers {
            for file in files_holding(place, value.as_bytes()) {
                found.push(format!("{what} in {}", file.display()));
            }
        }
    }
    assert!(
        board.dir().join(".vibe/local").is_dir(),
        "the harness's own check: the local state is there to be searched"
    );
    assert_eq!(
        found,
        Vec::<String>::new(),
        "expected nothing of the real Resource but its structure on the Builder's machine"
    );
}
