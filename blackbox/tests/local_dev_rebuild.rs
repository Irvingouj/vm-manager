//! `vibe dev` serves the project as it is now (CONTRACT.md, "The local loop,
//! kept running: the browser, test rows and reset": "The project as it is
//! now", and "Test rows": a file that fails is answered with the local
//! run's page until it is corrected; ticket "Local loop in the browser, with
//! test rows, reset and working offline").
//!
//! A `vibe dev --json` runs while the test changes the project; what it
//! rebuilt is read from its `rebuilt` and `rebuild-failed` lines.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::time::Duration;

use blackbox::dev::{Dev, html_unescaped};
use blackbox::generate::{Generated, NOTES_WITH_TITLE_SCHEMA_TS, generate};
use blackbox::local::{
    APP, BUILDER, Board, DEVOPS, Local, OUTSIDER, dev_json, handler, local_project, write_dev_json,
};
use blackbox::project::NOTES_SCHEMA_TS;
use blackbox::sdk::TempProject;
use serde_json::{Value, json};

const NAME: &str = "rebuilt-board";

/// The handler's default answer, as the test changes it.
const DEFAULT_ANSWER: &str = "Response.json({ user: ctx.user().id })";
const CHANGED_ANSWER: &str = "Response.json({ user: ctx.user().id, changed: true })";

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Rewrites `src/index.ts` with `from` replaced by `to`.
fn change_code(project: &TempProject, from: &str, to: &str) {
    let path = project.path("src/index.ts");
    let code = fs::read_to_string(&path).unwrap();
    let changed = code.replace(from, to);
    assert_ne!(
        code, changed,
        "the harness's own check: {from:?} is in the code"
    );
    fs::write(&path, changed).unwrap();
}

fn notes_project(local: &Local) -> TempProject {
    let project = local_project(&local.machine, NAME, &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    project
}

#[test]
fn a_change_of_the_code_is_served_at_the_next_request_and_said_in_a_rebuilt_line() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").json(), json!({ "user": OUTSIDER }));
    change_code(&project, DEFAULT_ANSWER, CHANGED_ANSWER);
    let after = browser.get("/");
    assert_eq!(
        after.json(),
        json!({ "user": OUTSIDER, "changed": true }),
        "expected the code as it is now served at the next request\n{}",
        after.describe()
    );
    let rebuilt = dev.events("rebuilt");
    assert_eq!(
        rebuilt.len(),
        1,
        "expected one `rebuilt` line\n{}",
        dev.stdout()
    );
    assert_eq!(
        sorted_keys(&rebuilt[0]),
        [
            "application",
            "event",
            "migrations_applied",
            "standins",
            "storages",
            "test_rows",
            "url",
            "version"
        ],
        "expected the `rebuilt` line with exactly its fields, got {}",
        rebuilt[0]
    );
    assert_eq!(
        rebuilt[0]["version"],
        json!(2),
        "expected a new local Version for a new bundle"
    );
    assert_eq!(rebuilt[0]["migrations_applied"], json!([]));
    dev.stop("INT").expect_code(0);
}

#[test]
fn nothing_is_rebuilt_when_nothing_changed() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    for _ in 0..3 {
        assert_eq!(browser.get("/").status, 200);
    }
    dev.wait_for_line("three `request` lines", Duration::from_secs(10), |_| {
        dev.events("request").len() >= 3
    });
    assert_eq!(
        dev.events("rebuilt"),
        Vec::<Value>::new(),
        "expected nothing rebuilt when nothing of the project changed\n{}",
        dev.stdout()
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_migration_generated_while_it_runs_is_applied_before_the_next_request() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let request = browser.build("POST", "/notes").body("before");
    assert_eq!(browser.send(&request).status, 200);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let added = Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_title"]),
        &project.dir,
    )
    .name;
    let after = browser.get("/notes");
    assert_eq!(after.status, 200, "{}", after.describe());
    assert_eq!(
        after.json()["notes"],
        json!([{ "id": 1, "body": "before", "title": null }]),
        "expected the migration applied and the row kept"
    );
    let rebuilt = dev.events("rebuilt");
    assert_eq!(rebuilt.len(), 1, "{}", dev.stdout());
    assert_eq!(rebuilt[0]["migrations_applied"], json!([added]));
    assert_eq!(rebuilt[0]["version"], json!(2));
    dev.stop("INT").expect_code(0);
}

#[test]
fn vm_dev_json_edited_while_it_runs_decides_the_next_request() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/incidents").status, 403);
    let mut file = dev_json(board.dir());
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
    let allowed = browser.get("/incidents");
    assert_eq!(
        allowed.status,
        200,
        "expected the Grant added to vibe.dev.json to decide the next request\n{}",
        allowed.describe()
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_pull_after_the_shared_structure_changed_remakes_the_standin_with_its_test_rows() {
    let board = Board::start("read", "read");
    board.project.write(
        "test-rows/devops.sql",
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n",
    );
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let before = browser.get("/columns").json();
    assert!(
        before["rows"][0].get("area").is_none(),
        "the harness's own check: no `area` yet, got {before}"
    );
    board
        .scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents ADD COLUMN area text");
    board.local.pull_ok(board.dir(), DEVOPS);
    let after = browser.get("/columns");
    assert_eq!(after.status, 200, "{}", after.describe());
    let rows = after.json()["rows"].clone();
    assert_eq!(rows.as_array().map(Vec::len), Some(1), "{rows}");
    assert_eq!(
        rows[0].get("area"),
        Some(&Value::Null),
        "expected the stand-in remade with the new column, and its test rows loaded again, got {rows}"
    );
    let rebuilt = dev.events("rebuilt");
    assert_eq!(rebuilt.len(), 1, "{}", dev.stdout());
    assert_eq!(
        rebuilt[0]["standins"],
        json!([{ "resource": DEVOPS, "made": true }])
    );
    assert_eq!(
        rebuilt[0]["test_rows"],
        json!([{ "file": "test-rows/devops.sql", "rows": 1 }])
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_project_that_does_not_build_is_answered_500_with_the_reason_and_message_until_corrected() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").status, 200);
    let good = fs::read_to_string(project.path("src/index.ts")).unwrap();
    project.write(
        "src/index.ts",
        &format!("import gone from \"./<i>gone</i>.ts\";\nconsole.log(gone);\n{good}"),
    );
    let failed = browser.get("/");
    assert_eq!(failed.status, 500, "{}", failed.describe());
    assert!(
        failed
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/html")),
        "expected a page of the local run's own\n{}",
        failed.describe()
    );
    let page = failed.text();
    assert!(
        !page.contains("<i>gone</i>"),
        "expected what the build said escaped as HTML\n{page}"
    );
    let plain = html_unescaped(&page);
    assert!(
        plain.contains("build-failed"),
        "expected the reason\n{page}"
    );
    // The message is the one `vibe request` gives (answered by `vibe dev`).
    let run = local.request(&project.dir, "/", OUTSIDER, &["--timeout", "60"]);
    let message = run.json_error(1, "build-failed");
    assert!(
        plain.contains(&message),
        "expected the page to give the message `vibe request` gives, {message:?}\n{page}"
    );
    let line = dev.wait_for_line("a `rebuild-failed` line", Duration::from_secs(10), |l| {
        l["event"] == "rebuild-failed"
    });
    assert_eq!(line["error"]["reason"], json!("build-failed"), "{line}");
    assert!(
        line["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{line}"
    );
    // Every request until it is corrected; not the Version built before.
    assert_eq!(browser.get("/").status, 500);
    project.write("src/index.ts", &good);
    let fixed = browser.get("/");
    assert_eq!(
        fixed.status,
        200,
        "expected the first request after the fix to build again\n{}",
        fixed.describe()
    );
    assert_eq!(fixed.json(), json!({ "user": OUTSIDER }));
    dev.stop("INT").expect_code(0);
}

#[test]
fn test_rows_that_fail_are_answered_with_the_page_until_the_file_is_corrected() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/notes").status, 200);
    project.write(
        "test-rows/own-database.sql",
        "INSERT INTO notes (body) VALUES ('kept');\nINSERT INTO missing_table VALUES (1);\n",
    );
    let failed = browser.get("/notes");
    assert_eq!(failed.status, 500, "{}", failed.describe());
    assert!(
        html_unescaped(&failed.text()).contains("test-rows-failed"),
        "expected the reason on the page\n{}",
        failed.describe()
    );
    let line = dev.wait_for_line("a `rebuild-failed` line", Duration::from_secs(10), |l| {
        l["event"] == "rebuild-failed"
    });
    assert_eq!(line["error"]["reason"], json!("test-rows-failed"), "{line}");
    assert_eq!(
        line["error"]["file"],
        json!("test-rows/own-database.sql"),
        "{line}"
    );
    assert_eq!(browser.get("/notes").status, 500);
    project.write(
        "test-rows/own-database.sql",
        "INSERT INTO notes (body) VALUES ('kept');\n",
    );
    let fixed = browser.get("/notes");
    assert_eq!(fixed.status, 200, "{}", fixed.describe());
    assert_eq!(fixed.json()["notes"], json!([{ "id": 1, "body": "kept" }]));
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_project_of_more_than_20000_entries_is_rebuilt_only_when_started_again() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let bulk = project.dir.join("bulk");
    fs::create_dir_all(&bulk).unwrap();
    for n in 0..20_001 {
        fs::write(bulk.join(format!("{n}.txt")), "").unwrap();
    }
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").json(), json!({ "user": OUTSIDER }));
    let said = dev.events("too-many-files");
    assert_eq!(
        said.len(),
        1,
        "expected `vibe dev` to say once, when it starts, that the project has too many files\n{}",
        dev.stdout()
    );
    assert_eq!(sorted_keys(&said[0]), ["event", "message"], "{}", said[0]);
    assert!(
        said[0]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{}",
        said[0]
    );
    change_code(&project, DEFAULT_ANSWER, CHANGED_ANSWER);
    assert_eq!(
        browser.get("/").json(),
        json!({ "user": OUTSIDER }),
        "expected a project of more than 20,000 entries not looked at again while it runs"
    );
    assert_eq!(dev.events("rebuilt"), Vec::<Value>::new());
    assert_eq!(
        dev.events("too-many-files").len(),
        1,
        "expected it said once only\n{}",
        dev.stdout()
    );
    dev.stop("INT").expect_code(0);
    let again = Dev::start(&local, &project.dir, NAME);
    let mut browser = again.browser();
    browser.choose(OUTSIDER);
    assert_eq!(
        browser.get("/").json(),
        json!({ "user": OUTSIDER, "changed": true }),
        "expected the change served once started again"
    );
    again.stop("INT").expect_code(0);
}

#[test]
fn a_renamed_application_is_served_at_its_new_name_and_its_old_name_is_421() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").status, 200);
    let renamed = "renamed-board";
    project.write("manifest.ts", &blackbox::local::declaring(renamed, &[]));
    let old = browser.get("/");
    assert_eq!(
        old.status,
        421,
        "expected the request that found the new name answered 421 at the old one\n{}",
        old.describe()
    );
    let rebuilt = dev.wait_for_line("a `rebuilt` line", Duration::from_secs(10), |l| {
        l["event"] == "rebuilt"
    });
    assert_eq!(rebuilt["application"], json!(renamed), "{rebuilt}");
    assert_eq!(
        rebuilt["url"],
        json!(format!("http://{renamed}.localhost:{}/", dev.port)),
        "{rebuilt}"
    );
    let mut at_new =
        blackbox::dev::Browser::new(dev.address(), &format!("{renamed}.localhost:{}", dev.port));
    at_new.set_cookie("vm_test_user", OUTSIDER);
    let served = at_new.get("/");
    assert_eq!(served.status, 200, "{}", served.describe());
    assert_eq!(served.json(), json!({ "user": OUTSIDER }));
    let requested = blackbox::local::Requested::of(&local.request(
        &project.dir,
        "/",
        OUTSIDER,
        &["--timeout", "60"],
    ));
    assert_eq!(requested.body["application"], json!(renamed));
    let id = requested.request_id();
    assert!(
        dev.events("request")
            .iter()
            .any(|l| l["request_id"] == id.as_str()),
        "expected the tool's command to go on finding `vibe dev` after the rename\n{}",
        dev.stdout()
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_project_grown_past_20000_entries_while_vm_dev_runs_is_said_once() {
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").status, 200);
    assert_eq!(dev.events("too-many-files"), Vec::<Value>::new());
    let bulk = project.dir.join("bulk");
    fs::create_dir_all(&bulk).unwrap();
    for n in 0..20_001 {
        fs::write(bulk.join(format!("{n}.txt")), "").unwrap();
    }
    for _ in 0..3 {
        assert_eq!(browser.get("/").status, 200);
    }
    let said = dev.events("too-many-files");
    assert_eq!(
        said.len(),
        1,
        "expected `vibe dev` to say once, at the request that found the project grown past 20,000 entries\n{}",
        dev.stdout()
    );
    assert_eq!(sorted_keys(&said[0]), ["event", "message"], "{}", said[0]);
    dev.stop("INT").expect_code(0);
}
