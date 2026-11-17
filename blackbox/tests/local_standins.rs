//! The stand-ins of the pulled Resources, and the Resources and Grants of a
//! local run (CONTRACT.md, "The local loop: one request as a test User:
//! What a run does", steps 4 and 5, and "A refusal is the same as
//! deployed"; ticket "Local loop: run the Application on the Builder's
//! machine and send a request as a test User": no real row and no
//! credential of a real Resource reaches the Builder's machine; a request
//! as the test User with no access gets the same refusal a deployed
//! Application would give).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::local::{
    BUILDER, Board, DEVOPS, INCOMPLETE_KEYS, OUTSIDER, Requested, declaring, dev_json,
    statements_of_standin_file,
};
use blackbox::scenario::CONTACT;
use serde_json::{Value, json};

fn refusal_of(requested: &Requested) -> Value {
    assert_eq!(
        requested.status(),
        403,
        "expected the handler's 403 for a refusal\n{}",
        requested.describe()
    );
    requested.json()["refusal"].clone()
}

#[test]
fn the_standin_is_made_again_when_the_pulled_structure_changes() {
    let board = Board::start("read", "read");
    let first = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(first.json(), json!({ "rows": [] }), "{}", first.describe());
    assert_eq!(
        first.body["standins"],
        json!([{ "resource": DEVOPS, "made": true }])
    );
    board
        .scenario
        .db
        .superuser()
        .batch("ALTER TABLE incidents ADD COLUMN region text");
    board.local.pull_ok(board.dir(), DEVOPS);
    let second = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(
        second.body["standins"],
        json!([{ "resource": DEVOPS, "made": true }]),
        "expected the stand-in made again for the new fingerprint\n{}",
        second.describe()
    );
    assert_eq!(
        second.json(),
        json!({ "rows": [] }),
        "expected the stand-in to have the new column the code now reads\n{}",
        second.describe()
    );
    let third = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(
        third.body["standins"],
        json!([{ "resource": DEVOPS, "made": false }]),
        "expected it kept while the fingerprint is the same"
    );
}

#[test]
fn a_statement_the_local_postgres_cannot_run_is_left_out_with_a_warning() {
    let board = Board::start("read", "read");
    // A default that calls a function the shared database has and the
    // stand-in has not.
    board.scenario.db.superuser().batch(
        "CREATE FUNCTION next_ticket_code() RETURNS text LANGUAGE sql AS $f$ SELECT 'T-1' $f$;
         ALTER TABLE incidents ADD COLUMN code text DEFAULT next_ticket_code()",
    );
    board.local.pull_ok(board.dir(), DEVOPS);
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the run to go on without the statement\n{}",
        requested.describe()
    );
    let warnings = requested.warnings_for("stand-in-incomplete");
    assert_eq!(
        warnings.len(),
        1,
        "expected one warning for the statement left out\n{}",
        requested.describe()
    );
    let warning = &warnings[0];
    assert_incomplete_keys(warning);
    assert_eq!(warning["resource"], json!(DEVOPS), "{warning}");
    let written = fs::read_to_string(board.dir().join("resources/devops/standin.sql")).unwrap();
    let default = statements_of_standin_file(&written)
        .into_iter()
        .find(|s| s.contains("next_ticket_code"))
        .unwrap_or_else(|| panic!("expected the default's statement in standin.sql\n{written}"));
    assert!(
        default.to_ascii_uppercase().starts_with("ALTER TABLE")
            && default.to_ascii_uppercase().contains("SET DEFAULT"),
        "the harness's own check: the default is an ALTER ... SET DEFAULT, got {default:?}"
    );
    assert_eq!(
        warning["statement"],
        json!(default.lines().next().unwrap()),
        "expected `statement` the first line of the statement left out"
    );
    assert!(
        warning["error"]
            .as_str()
            .is_some_and(|e| e.contains("next_ticket_code")),
        "expected `error` the database's message, naming the function, got {warning}"
    );
}

/// Fails unless a `stand-in-incomplete` warning has exactly the contract's
/// keys, each text.
fn assert_incomplete_keys(warning: &Value) {
    let mut keys: Vec<&str> = warning
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys, INCOMPLETE_KEYS,
        "expected exactly these keys: {warning}"
    );
    for key in INCOMPLETE_KEYS {
        assert!(
            warning[key].is_string(),
            "expected `{key}` a text: {warning}"
        );
    }
    assert_eq!(warning["reason"], json!("stand-in-incomplete"));
}

#[test]
fn the_standins_statements_run_as_a_role_that_is_not_a_superuser() {
    let board = Board::start("read", "read");
    // A statement only a superuser may run, added to the tool's own file
    // (its three first lines kept): writing a file on the server.
    let target = board.local.machine.root().join("written-by-the-standin");
    let file = board.dir().join("resources/devops/standin.sql");
    let mut text = fs::read_to_string(&file)
        .expect("expected the pull to have written resources/devops/standin.sql");
    text.push_str(&format!("COPY (SELECT 1) TO '{}';\n\n", target.display()));
    fs::write(&file, text).unwrap();
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the run to go on without the statement\n{}",
        requested.describe()
    );
    let warnings = requested.warnings_for("stand-in-incomplete");
    assert_eq!(warnings.len(), 1, "{}", requested.describe());
    assert_incomplete_keys(&warnings[0]);
    assert!(
        warnings[0]["statement"]
            .as_str()
            .unwrap()
            .starts_with("COPY (SELECT 1) TO"),
        "expected the warning to be the COPY's, got {}",
        warnings[0]
    );
    assert!(
        !target.exists(),
        "expected no file written by a statement of the stand-in: they run as a role that is not a superuser"
    );
}

#[test]
fn a_resource_pulled_before_this_ticket_has_no_standin_and_its_operations_are_not_registered() {
    let board = Board::start("read", "read");
    fs::remove_file(board.dir().join("resources/devops/standin.sql"))
        .expect("expected the pull to have written resources/devops/standin.sql, to remove it");
    let requested = board.request_ok("/incidents", BUILDER, &[]);
    let warnings = requested.warnings_for("stand-in-not-pulled");
    assert_eq!(warnings.len(), 1, "{}", requested.describe());
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("vm pull devops"),
        "expected the warning to say to run `vm pull devops` again, got {}",
        warnings[0]
    );
    assert_eq!(
        refusal_of(&requested),
        json!({ "reason": "not-registered", "resource": DEVOPS }),
        "expected a declared Resource with no stand-in refused as an installation refuses one not registered"
    );
}

#[test]
fn a_declared_resource_that_was_never_pulled_is_not_registered() {
    let board = Board::start("read", "read");
    board.project.write(
        "manifest.ts",
        &declaring(
            blackbox::local::APP,
            &[(DEVOPS, "read"), ("payroll", "read")],
        ),
    );
    board.project.write(
        "src/index.ts",
        &blackbox::local::handler(true, false).replace(
            "case \"/columns\": {",
            "case \"/payroll\": {\n        const rows = await (ctx.user() as any).db(\"payroll\").select().from(incidents);\n        return Response.json({ rows });\n      }\n      case \"/columns\": {",
        ),
    );
    let requested = board.request_ok("/payroll", BUILDER, &[]);
    assert_eq!(
        refusal_of(&requested),
        json!({ "reason": "not-registered", "resource": "payroll" }),
        "{}",
        requested.describe()
    );
}

#[test]
fn writing_through_a_read_declaration_or_a_read_grant_is_refused_as_deployed() {
    // Declared at read: declared-lower, whatever the Grant.
    let read = Board::start("read", "write");
    let lower = read.request_ok("/add-incident", BUILDER, &["--data", "a title"]);
    assert_eq!(
        refusal_of(&lower),
        json!({ "reason": "declared-lower", "resource": DEVOPS, "declared": "read", "needed": "write" }),
        "{}",
        lower.describe()
    );
    // Declared at write, the Builder granted read: no-grant.
    let write = Board::start("write", "read");
    let refused = write.request_ok("/add-incident", BUILDER, &["--data", "a title"]);
    assert_eq!(
        refusal_of(&refused),
        json!({
            "reason": "no-grant", "resource": DEVOPS, "user": BUILDER,
            "needed": "write", "held": "read", "contact": CONTACT
        }),
        "{}",
        refused.describe()
    );
}

#[test]
fn with_a_write_grant_a_write_lands_in_the_standin_and_never_in_the_shared_database() {
    let board = Board::start("write", "write");
    assert_eq!(
        dev_json(board.dir())["grants"],
        json!([{ "group": "builder", "resource": DEVOPS, "access": "write" }])
    );
    let title = format!("written locally {}", blackbox::shared_database::token());
    let requested = board.request_ok("/add-incident", BUILDER, &["--data", &title]);
    assert_eq!(
        requested.json(),
        json!({ "incidents": [{ "id": 1000, "title": title }] }),
        "expected the row written into the stand-in and read back\n{}",
        requested.describe()
    );
    let real = board.scenario.db.superuser();
    assert_eq!(
        real.value("SELECT count(*) FROM incidents").as_deref(),
        Some("3"),
        "expected the shared database's rows as they were"
    );
    assert_eq!(
        real.value(&format!(
            "SELECT count(*) FROM incidents WHERE title = '{title}'"
        ))
        .as_deref(),
        Some("0"),
        "expected nothing written into the shared database"
    );
    // As outsider, with no Grant, the write is refused.
    let outsider = board.request_ok("/add-incident", OUTSIDER, &["--data", "x"]);
    assert_eq!(refusal_of(&outsider)["reason"], json!("no-grant"));
}

#[test]
fn the_standin_of_a_resource_no_longer_pulled_is_removed_by_the_next_run() {
    let board = Board::start("read", "read");
    let first = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(
        first.body["standins"],
        json!([{ "resource": DEVOPS, "made": true }]),
        "{}",
        first.describe()
    );
    // While it is pulled and unchanged, it is kept.
    let kept = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(
        kept.body["standins"],
        json!([{ "resource": DEVOPS, "made": false }]),
        "the harness's own check: an unchanged stand-in is kept"
    );
    // The code stops importing the pulled definition, and its folder goes.
    board
        .project
        .write("src/index.ts", &blackbox::local::handler(false, false));
    fs::remove_dir_all(board.dir().join("resources/devops")).unwrap();
    let without = board.request_ok("/", BUILDER, &[]);
    assert_eq!(
        without.body["standins"],
        json!([]),
        "expected no stand-in of a Resource no longer pulled\n{}",
        without.describe()
    );
    // Pulled again with the same structure: had the stand-in been kept, it
    // would be used as it was; removed, it is made anew.
    board.local.pull_ok(board.dir(), DEVOPS);
    board
        .project
        .write("src/index.ts", &blackbox::local::handler(true, false));
    let again = board.request_ok("/columns", BUILDER, &[]);
    assert_eq!(
        again.body["standins"],
        json!([{ "resource": DEVOPS, "made": true }]),
        "expected the stand-in removed when its folder was, and so made anew once pulled again\n{}",
        again.describe()
    );
}
