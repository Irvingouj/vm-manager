//! Test rows: SQL files of the project loaded into the stand-ins and the
//! local own database (CONTRACT.md, "The local loop, kept running: the
//! browser, test rows and reset": "Test rows"; ticket "Local loop in the
//! browser, with test rows, reset and working offline": test rows in project
//! files are loaded into the stand-ins and the own database, and loading is
//! repeatable).
//!
//! Seen through `vibe request` alone: the Application reads the stand-in of
//! `devops` as builder (`/incidents`, `/devops-notes`, `/columns`) and its
//! own `notes` (`/notes`); `test_rows` of the output says what each run
//! loaded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::generate::{Generated, generate};
use blackbox::local::{APP, BUILDER, Board, Local, OUTSIDER, Requested, handler, local_project};
use blackbox::project::{NOTES_SCHEMA_TS, new_project};
use blackbox::sdk::TempProject;
use blackbox::tool::Run;
use serde_json::{Value, json};

const DEVOPS_ROWS: &str = "test-rows/devops.sql";
const OWN_ROWS: &str = "test-rows/own-database.sql";

const TWO_INCIDENTS: &str = "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00'),\n  (2, 'Coffee machine down', false, 1, '2026-11-02 10:00:00+00');\n";

fn incidents(requested: &Requested) -> Value {
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.json()["incidents"].clone()
}

fn notes_of(requested: &Requested) -> Value {
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.json()["notes"].clone()
}

/// Fails unless the run ended `test-rows-failed` naming `file`, with the
/// first line of the statement that failed beginning with `statement` and
/// the database's message; returns the error.
fn failed_on(run: &Run, file: &str, statement: &str) -> Value {
    run.json_error(1, "test-rows-failed");
    let error = run.json()["error"].clone();
    let mut keys: Vec<&str> = error
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["error", "file", "message", "reason", "statement"],
        "expected `file`, `statement` and `error` beside the message\n{}",
        run.describe()
    );
    assert_eq!(error["file"], json!(file), "{}", run.describe());
    let first_line = error["statement"].as_str().unwrap();
    assert!(
        first_line.starts_with(statement) && !first_line.contains('\n'),
        "expected the first line of the statement that failed, {statement:?}..., got {first_line:?}"
    );
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|e| !e.trim().is_empty()),
        "expected the database's message\n{}",
        run.describe()
    );
    error
}

/// A project declaring nothing whose own database has `notes`, migrated by
/// `0000_add_notes`.
fn notes_project(local: &Local) -> TempProject {
    let project = local_project(&local.machine, "rows-board", &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    project
}

#[test]
fn a_new_project_has_a_test_rows_folder_whose_readme_says_what_goes_there() {
    let local = Local::new();
    let dir = local.machine.root().join("fresh");
    new_project(&local.machine, "fresh", &dir);
    let readme = fs::read_to_string(dir.join("test-rows/README.md"))
        .unwrap_or_else(|e| panic!("expected test-rows/README.md in a new project: {e}"));
    assert!(
        readme.contains("own-database.sql") && readme.contains(".sql"),
        "expected the README to say that each pulled Resource has its <resource>.sql and the own database own-database.sql\n{readme}"
    );
}

#[test]
fn a_standins_file_is_loaded_when_the_standin_is_made_and_the_application_reads_its_rows() {
    let board = Board::start("read", "read");
    board.project.write(DEVOPS_ROWS, TWO_INCIDENTS);
    let first = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        incidents(&first),
        json!([
            { "id": 1, "title": "Printer on fire" },
            { "id": 2, "title": "Coffee machine down" }
        ]),
        "expected the file's rows in the stand-in\n{}",
        first.describe()
    );
    assert_eq!(
        first.test_rows(),
        json!([{ "file": DEVOPS_ROWS, "rows": 2 }]),
        "{}",
        first.describe()
    );
    let second = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        second.test_rows(),
        json!([]),
        "expected a file loaded already not loaded again"
    );
    assert_eq!(incidents(&second).as_array().map(Vec::len), Some(2));
    // The shared database itself is untouched.
    assert_eq!(
        board
            .scenario
            .db
            .superuser()
            .value("SELECT string_agg(title, ',' ORDER BY id) FROM incidents WHERE id IN (1, 2)")
            .as_deref(),
        Some("Disk full on db-7,Login page slow")
    );
}

#[test]
fn a_changed_file_is_loaded_again_from_empty_tables_whose_serial_values_start_again() {
    let board = Board::start("read", "read");
    board.project.write(
        DEVOPS_ROWS,
        "INSERT INTO notes (body) VALUES ('first'), ('second');\n",
    );
    let first = board.request_ok("/devops-notes", BUILDER, &[]);
    assert_eq!(
        notes_of(&first),
        json!([{ "id": 1, "body": "first" }, { "id": 2, "body": "second" }]),
        "{}",
        first.describe()
    );
    board.project.write(
        DEVOPS_ROWS,
        "INSERT INTO notes (body) VALUES ('first'), ('second'), ('third');\n",
    );
    let again = board.request_ok("/devops-notes", BUILDER, &[]);
    assert_eq!(
        notes_of(&again),
        json!([
            { "id": 1, "body": "first" },
            { "id": 2, "body": "second" },
            { "id": 3, "body": "third" }
        ]),
        "expected the tables emptied and their serial values started again before the file was loaded again\n{}",
        again.describe()
    );
    assert_eq!(
        again.test_rows(),
        json!([{ "file": DEVOPS_ROWS, "rows": 3 }])
    );
}

#[test]
fn rows_the_application_wrote_stay_while_the_file_is_the_same_and_a_changed_file_replaces_them() {
    let board = Board::start("write", "write");
    board.project.write(DEVOPS_ROWS, TWO_INCIDENTS);
    let written = board.request_ok("/add-incident", BUILDER, &["--data", "written by the code"]);
    assert_eq!(written.status(), 200, "{}", written.describe());
    let kept = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        incidents(&kept),
        json!([
            { "id": 1, "title": "Printer on fire" },
            { "id": 2, "title": "Coffee machine down" },
            { "id": 1000, "title": "written by the code" }
        ]),
        "expected what the Application wrote kept from one run to the next while the file is the same\n{}",
        kept.describe()
    );
    board.project.write(
        DEVOPS_ROWS,
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (7, 'Only this one', true, 1, '2026-11-03 10:00:00+00');\n",
    );
    let replaced = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        incidents(&replaced),
        json!([{ "id": 7, "title": "Only this one" }]),
        "expected a changed file to replace every row of the stand-in\n{}",
        replaced.describe()
    );
}

#[test]
fn a_removed_file_empties_the_standin() {
    let board = Board::start("read", "read");
    board.project.write(DEVOPS_ROWS, TWO_INCIDENTS);
    assert_eq!(
        incidents(&board.request_ok("/incidents", BUILDER, &[]))
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    fs::remove_file(board.dir().join(DEVOPS_ROWS)).unwrap();
    let emptied = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        incidents(&emptied),
        json!([]),
        "expected the stand-in emptied when its file is removed\n{}",
        emptied.describe()
    );
}

#[test]
fn a_file_that_makes_a_table_fails_and_nothing_of_it_is_loaded() {
    let board = Board::start("write", "write");
    board.project.write(
        DEVOPS_ROWS,
        &format!("{TWO_INCIDENTS}CREATE TABLE sneaky (a int4);\n"),
    );
    let run = board.request("/incidents", BUILDER, &[]);
    failed_on(&run, DEVOPS_ROWS, "CREATE TABLE sneaky");
    // The next run tries again.
    let again = board.request("/incidents", BUILDER, &[]);
    failed_on(&again, DEVOPS_ROWS, "CREATE TABLE sneaky");
    // The table was never made.
    board
        .project
        .write(DEVOPS_ROWS, "INSERT INTO sneaky VALUES (1);\n");
    let error = failed_on(
        &board.request("/incidents", BUILDER, &[]),
        DEVOPS_ROWS,
        "INSERT INTO sneaky",
    );
    assert!(
        error["error"].as_str().unwrap().contains("sneaky"),
        "expected the database to say sneaky is not there, got {error}"
    );
    // Nor is any other change of the structure allowed to the write login.
    for statement in [
        "ALTER TABLE incidents ADD COLUMN extra text;",
        "DROP TABLE kinds;",
        "CREATE FUNCTION f() RETURNS int4 LANGUAGE sql AS 'SELECT 1';",
        "COPY incidents TO '/tmp/out.csv';",
    ] {
        board.project.write(DEVOPS_ROWS, &format!("{statement}\n"));
        let first_word = statement.split_whitespace().next().unwrap();
        failed_on(
            &board.request("/incidents", BUILDER, &[]),
            DEVOPS_ROWS,
            first_word,
        );
    }
    board.project.write(DEVOPS_ROWS, TWO_INCIDENTS);
    assert_eq!(
        incidents(&board.request_ok("/incidents", BUILDER, &[]))
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

#[test]
fn a_views_standin_can_be_given_rows() {
    let board = Board::on(
        Local::new(),
        APP,
        "read",
        "read",
        "CREATE VIEW open_incidents AS SELECT id, title FROM incidents WHERE open;\nGRANT SELECT ON open_incidents TO %READER%, %WRITER%;",
    );
    assert!(
        fs::read_to_string(board.dir().join("resources/devops/schema.ts"))
            .unwrap()
            .contains("openIncidents"),
        "the harness's own check: the view pulled"
    );
    board.project.write(
        "src/index.ts",
        r#"import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { openIncidents } from "../resources/devops/schema.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const rows = await ctx.user().db("devops").select().from(openIncidents);
    return Response.json({ rows });
  },
});
"#,
    );
    board.project.write(
        DEVOPS_ROWS,
        "INSERT INTO open_incidents (id, title) VALUES (5, 'Seen through the view');\n",
    );
    let requested = board.request_ok("/", BUILDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    assert_eq!(
        requested.json()["rows"],
        json!([{ "id": 5, "title": "Seen through the view" }]),
        "expected the view's stand-in given the file's rows\n{}",
        requested.describe()
    );
}

#[test]
fn a_file_for_a_resource_that_is_not_pulled_is_a_note_and_does_nothing() {
    let board = Board::start("read", "read");
    board.project.write(
        "test-rows/payroll.sql",
        "INSERT INTO salaries VALUES (1);\n",
    );
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    let notes = requested.notes_for("test-rows-without-resource");
    assert_eq!(notes.len(), 1, "{}", requested.describe());
    assert!(
        notes[0].to_string().contains("payroll"),
        "expected the note to name the file or the Resource, got {}",
        notes[0]
    );
    assert_eq!(requested.test_rows(), json!([]));
}

/// `sql` padded with a comment to exactly `size` bytes.
fn padded(sql: &str, size: usize) -> String {
    let mut text = String::from(sql);
    text.push_str("-- ");
    while text.len() < size - 1 {
        text.push('x');
    }
    text.push('\n');
    assert_eq!(text.len(), size, "the harness's own check");
    text
}

#[test]
fn a_file_of_16_mib_is_loaded_and_a_larger_one_is_test_rows_failed() {
    let board = Board::start("read", "read");
    let limit = 16 * 1024 * 1024;
    board
        .project
        .write(DEVOPS_ROWS, &padded(TWO_INCIDENTS, limit));
    let loaded = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        loaded.test_rows(),
        json!([{ "file": DEVOPS_ROWS, "rows": 2 }]),
        "expected a file of 16 MiB loaded\n{}",
        loaded.describe()
    );
    board
        .project
        .write(DEVOPS_ROWS, &padded(TWO_INCIDENTS, limit + 1));
    let run = board.request("/incidents", BUILDER, &[]);
    run.json_error(1, "test-rows-failed");
    assert_eq!(
        run.json()["error"]["file"],
        json!(DEVOPS_ROWS),
        "{}",
        run.describe()
    );
}

#[test]
fn the_own_databases_file_is_loaded_after_the_migrations_and_recorded_as_test_rows() {
    let local = Local::new();
    let project = notes_project(&local);
    project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('from the file'), ('also from the file');\n",
    );
    let first = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(
        notes_of(&first),
        json!([
            { "id": 1, "body": "from the file" },
            { "id": 2, "body": "also from the file" }
        ]),
        "{}",
        first.describe()
    );
    assert_eq!(first.migrations_applied(), ["0000_add_notes"]);
    assert_eq!(first.test_rows(), json!([{ "file": OWN_ROWS, "rows": 2 }]));
    let changes = local.change_history(&project.dir, &[]).ok_json()["changes"]
        .as_array()
        .unwrap()
        .clone();
    let loaded: Vec<&Value> = changes
        .iter()
        .filter(|c| c["table"] == "notes" && c["kind"] == "insert")
        .collect();
    assert_eq!(
        loaded.len(),
        2,
        "expected each row in the local Change history, got {changes:#?}"
    );
    for change in &loaded {
        assert_eq!(change["user_id"], json!("test-rows"), "{change}");
        assert_eq!(change["actor"], json!("application"), "{change}");
        assert!(
            change["request"]
                .as_str()
                .is_some_and(|r| r.starts_with("test-rows-")),
            "expected a request that begins with test-rows-, got {change}"
        );
    }
    let entries = local.operation_log(&project.dir, &[]).ok_json()["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        entries
            .iter()
            .all(|e| e["request"] == first.request_id().as_str()),
        "expected the loading of test rows in no entry of the Operation log, got {entries:#?}"
    );
}

#[test]
fn a_changed_own_database_file_makes_the_own_database_anew_first() {
    let local = Local::new();
    let project = notes_project(&local);
    project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('from the file');\n",
    );
    local.request_ok(
        &project.dir,
        "/notes",
        OUTSIDER,
        &["--data", "written by the code"],
    );
    project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('changed file'), ('second row');\n",
    );
    let anew = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(
        notes_of(&anew),
        json!([
            { "id": 1, "body": "changed file" },
            { "id": 2, "body": "second row" }
        ]),
        "expected the own database made anew: the row the code wrote gone, the file's rows from id 1\n{}",
        anew.describe()
    );
    assert_eq!(
        anew.migrations_applied(),
        ["0000_add_notes"],
        "expected the migrations applied again to the new own database"
    );
    assert_eq!(anew.test_rows(), json!([{ "file": OWN_ROWS, "rows": 2 }]));
}

#[test]
fn a_failing_statement_of_the_own_databases_file_leaves_it_as_the_migrations_make_it() {
    let local = Local::new();
    let project = notes_project(&local);
    project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('half');\nINSERT INTO missing_table VALUES (1);\n",
    );
    let run = local.request(&project.dir, "/notes", OUTSIDER, &[]);
    failed_on(&run, OWN_ROWS, "INSERT INTO missing_table");
    let changes = local.change_history(&project.dir, &[]).ok_json()["changes"].clone();
    assert!(
        !changes.to_string().contains("half"),
        "expected nothing of the failing file loaded, got {changes:#}"
    );
    project.write(OWN_ROWS, "INSERT INTO notes (body) VALUES ('whole');\n");
    let fixed = local.request_ok(&project.dir, "/notes", OUTSIDER, &[]);
    assert_eq!(
        notes_of(&fixed),
        json!([{ "id": 1, "body": "whole" }]),
        "{}",
        fixed.describe()
    );
}

#[test]
fn test_rows_lists_the_files_loaded_in_name_order_and_in_words_one_line_each() {
    let board = Board::start("read", "read");
    board.project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    board.project.write("src/index.ts", &handler(true, true));
    Generated::made(
        &generate(&board.local.machine, board.dir(), &["--name", "add_notes"]),
        board.dir(),
    );
    board
        .project
        .write(OWN_ROWS, "INSERT INTO notes (body) VALUES ('own');\n");
    board.project.write(DEVOPS_ROWS, TWO_INCIDENTS);
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        requested.test_rows(),
        json!([
            { "file": DEVOPS_ROWS, "rows": 2 },
            { "file": OWN_ROWS, "rows": 1 }
        ]),
        "{}",
        requested.describe()
    );
    // In words, after both files changed again.
    board.project.write(
        OWN_ROWS,
        "INSERT INTO notes (body) VALUES ('own, again');\n",
    );
    board.project.write(
        DEVOPS_ROWS,
        &TWO_INCIDENTS.replace("Printer on fire", "Printer on fire again"),
    );
    let words = board
        .local
        .request_in_words(board.dir(), "/incidents", BUILDER, &[])
        .ok_text();
    for file in [DEVOPS_ROWS, OWN_ROWS] {
        assert_eq!(
            words.lines().filter(|l| l.contains(file)).count(),
            1,
            "expected one line for {file}, got\n{words}"
        );
    }
}

#[test]
fn a_test_rows_file_that_is_a_link_or_a_folder_is_test_rows_failed() {
    let board = Board::start("read", "read");
    let elsewhere = board.local.machine.root().join("rows-elsewhere.sql");
    fs::write(&elsewhere, TWO_INCIDENTS).unwrap();
    fs::create_dir_all(board.dir().join("test-rows")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, board.dir().join(DEVOPS_ROWS)).unwrap();
    let run = board.request("/incidents", BUILDER, &[]);
    run.json_error(1, "test-rows-failed");
    assert_eq!(
        run.json()["error"]["file"],
        json!(DEVOPS_ROWS),
        "expected a link not followed\n{}",
        run.describe()
    );
    fs::remove_file(board.dir().join(DEVOPS_ROWS)).unwrap();
    fs::create_dir_all(board.dir().join(DEVOPS_ROWS)).unwrap();
    let run = board.request("/incidents", BUILDER, &[]);
    run.json_error(1, "test-rows-failed");
    assert_eq!(
        run.json()["error"]["file"],
        json!(DEVOPS_ROWS),
        "expected a folder refused\n{}",
        run.describe()
    );
}

#[test]
fn an_own_database_file_with_no_own_database_is_a_note_and_does_nothing() {
    let local = Local::new();
    let project = local_project(&local.machine, "no-own-board", &[]);
    project.write(OWN_ROWS, "INSERT INTO notes (body) VALUES ('nowhere');\n");
    let requested = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    let notes = requested.notes_for("test-rows-without-own-database");
    assert_eq!(notes.len(), 1, "{}", requested.describe());
    assert_eq!(notes[0]["file"], json!(OWN_ROWS), "{}", notes[0]);
    assert_eq!(requested.test_rows(), json!([]));
}
