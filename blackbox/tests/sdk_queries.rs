//! An Application written with the SDK and Drizzle, bundled by the SDK's
//! tooling, runs on the real program (CONTRACT.md, "The SDK"): it selects
//! from a shared database through Drizzle (a typed query, a join, a
//! relational query, a `with ... select`), writes to a shared database
//! declared at `write`, and inserts, updates, selects and deletes in its own
//! database as the User and as the Application.
//!
//! Every statement Drizzle sends is exactly one host operation, so one
//! Operation log entry: `query` for a select (also one beginning with
//! `with`), `execute` otherwise, with the right Resource or `own_database`,
//! the right actor, and the statement Drizzle built (which the fixture
//! reports from `toSQL()`) without values. The Change history of the own
//! database names the right actor and User.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::history;
use blackbox::scenario::RESOURCE;
use blackbox::sdk::{OPS, Sdk, ann, assert_one_entry_per_statement, d, own_op, shared_op};
use serde_json::{Value, json};

fn incident(id: &str, title: &str, severity: &str) -> Value {
    d::object(&[
        ("id", d::number(id)),
        ("title", d::string(title)),
        ("severity", d::number(severity)),
    ])
}

/// Fails if any Operation log entry of the request this answers holds
/// `marker`.
fn assert_no_entry_holds(sdk: &Sdk, response: &blackbox::Response, marker: &str, context: &str) {
    for entry in sdk.entries_of(response) {
        assert!(
            !entry.to_string().contains(marker),
            "{context}: an Operation log entry holds {marker:?}, a value from a row or a parameter: {entry}"
        );
    }
}

#[test]
fn a_typed_select_on_a_shared_database_is_one_query_with_the_statement_drizzle_built() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/shared/select");
    assert_eq!(
        body["rows"],
        json!([
            incident("1", "Disk full on db-7", "2"),
            incident("3", &sdk.scenario.db.row_marker, "3"),
        ]),
        "expected the open incidents, typed as Drizzle declares (id and severity numbers, title a string), got {body}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[shared_op("query", RESOURCE)],
        "a typed select on devops",
    );
    assert_no_entry_holds(
        &sdk,
        &response,
        &sdk.scenario.db.row_marker,
        "a typed select on devops",
    );
}

#[test]
fn a_join_on_a_shared_database_is_one_query() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/shared/join");
    let row = |service: &str, tier: &str| {
        d::object(&[
            ("service", d::string(service)),
            ("team", d::string("Payments")),
            ("tier", d::number(tier)),
        ])
    };
    assert_eq!(
        body["rows"],
        json!([row("checkout", "2"), row("ledger", "1")]),
        "expected the Payments team's services from the join, got {body}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[shared_op("query", RESOURCE)],
        "a join on devops",
    );
    assert_no_entry_holds(&sdk, &response, "Payments", "a join on devops");
}

#[test]
fn a_relational_query_on_a_shared_database_is_one_query() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/shared/relational");
    let row = |id: &str, name: &str, team: &str| {
        d::object(&[
            ("id", d::number(id)),
            ("name", d::string(name)),
            ("team", d::object(&[("name", d::string(team))])),
        ])
    };
    assert_eq!(
        body["rows"],
        json!([
            row("10", "db-7", "Platform"),
            row("11", "checkout", "Payments"),
            row("12", "ledger", "Payments"),
        ]),
        "expected every service with its team, from db.query.services.findMany({{ with: {{ team }} }}), got {body}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[shared_op("query", RESOURCE)],
        "a relational query on devops",
    );
}

#[test]
fn a_with_select_on_a_shared_database_is_a_query() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/shared/with");
    let row = |id: &str, severity: &str| {
        d::object(&[("id", d::number(id)), ("severity", d::number(severity))])
    };
    assert_eq!(
        body["rows"],
        json!([row("1", "2"), row("3", "3")]),
        "expected the open incidents through the `with`, got {body}"
    );
    let sql = body["statements"][0]["sql"].as_str().unwrap_or_default();
    assert!(
        sql.trim_start().to_ascii_lowercase().starts_with("with "),
        "the fixture's statement should begin with `with`, so that this test shows what it means to: {sql}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[shared_op("query", RESOURCE)],
        "a `with ... select` on devops",
    );
}

#[test]
fn inserts_updates_and_deletes_on_a_shared_database_declared_at_write_are_each_one_execute() {
    let sdk = Sdk::board();
    let marker = "sdk-shared-write-5c1e";
    let (response, body) = sdk.get_ok(&ann(), &format!("/shared/write?marker={marker}"));
    let inserted = &body["inserted"]["entries"];
    assert_eq!(
        inserted["body"],
        d::string(&format!("{marker}-inserted")),
        "expected the inserted row back from returning(), got {body}"
    );
    assert_eq!(
        inserted["writtenBy"],
        d::string(&sdk.scenario.db.writer.role),
        "expected the row to have been written by the Resource's write login (its default is current_user), got {body}"
    );
    let id = inserted["id"].clone();
    assert_eq!(
        body["updated"],
        json!([d::object(&[
            ("id", id.clone()),
            ("body", d::string(&format!("{marker}-updated")))
        ])]),
        "expected the updated row back from returning(), got {body}"
    );
    assert_eq!(
        body["deleted"],
        json!([d::object(&[("id", id)])]),
        "expected the deleted row's id back from returning(), got {body}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[
            shared_op("execute", OPS),
            shared_op("execute", OPS),
            shared_op("execute", OPS),
        ],
        "insert, update and delete on ops",
    );
    assert_no_entry_holds(&sdk, &response, marker, "insert, update and delete on ops");
    let left = sdk.scenario.db.superuser().value(&format!(
        "SELECT count(*)::text FROM notes WHERE body LIKE '{marker}%'"
    ));
    assert_eq!(
        left.as_deref(),
        Some("0"),
        "expected the row inserted and deleted through ops to be gone from the shared database"
    );
}

/// Runs the own-database sequence of the fixture as `actor` and checks its
/// answer, its Operation log entries and its Change history.
fn own_database_sequence_as(actor: &str) {
    let sdk = Sdk::board();
    let marker = format!("sdk-own-{actor}-9d2b");
    let (response, body) = sdk.get_ok(&ann(), &format!("/own?as={actor}&marker={marker}"));
    let context = format!("the own database as {actor}");
    let inserted = &body["inserted"]["entries"];
    assert_eq!(
        inserted["body"],
        d::string(&format!("{marker}-inserted")),
        "{context}: expected the inserted note from returning(), got {body}"
    );
    assert_eq!(
        inserted["author"],
        d::string(actor),
        "{context}: got {body}"
    );
    assert_eq!(
        inserted["id"]["type"], "number",
        "{context}: expected a serial id as a number, got {body}"
    );
    let id = inserted["id"].clone();
    let updated_note = d::object(&[
        ("id", id.clone()),
        ("body", d::string(&format!("{marker}-updated"))),
        ("author", d::string(actor)),
    ]);
    assert_eq!(
        body["updated"],
        json!([updated_note]),
        "{context}: expected the updated note from returning(), got {body}"
    );
    assert_eq!(
        body["selected"],
        json!([updated_note]),
        "{context}: expected the select to see the update, got {body}"
    );
    assert_eq!(
        body["relational"],
        json!([updated_note]),
        "{context}: expected db.query.notes.findMany to see the update, got {body}"
    );
    assert_eq!(
        body["counted"],
        json!([d::object(&[
            ("name", d::string("hits")),
            ("n", d::number("1"))
        ])]),
        "{context}: expected the counter raised through a `sql` fragment in set(), got {body}"
    );
    assert_eq!(
        body["deleted"],
        json!([d::object(&[("id", id)])]),
        "{context}: got {body}"
    );
    assert_one_entry_per_statement(
        &sdk,
        &response,
        &body,
        &ann(),
        &[
            own_op("execute", actor),
            own_op("execute", actor),
            own_op("query", actor),
            own_op("query", actor),
            own_op("execute", actor),
            own_op("execute", actor),
        ],
        &context,
    );
    assert_no_entry_holds(&sdk, &response, &marker, &context);

    // The Change history: the insert, the two updates and the delete, each
    // under this actor, the User in flight and this request.
    let request = response.request_id();
    let changes: Vec<(String, String, String, String, String)> = history(&sdk.own)
        .iter()
        .map(|row| {
            let text = |k: &str| row[k].as_str().unwrap_or("<null>").to_owned();
            (
                text("table_name"),
                text("kind"),
                text("actor"),
                text("user_id"),
                text("request"),
            )
        })
        .collect();
    let change = |table: &str, kind: &str| {
        (
            table.to_owned(),
            kind.to_owned(),
            actor.to_owned(),
            "ann".to_owned(),
            request.clone(),
        )
    };
    assert_eq!(
        changes,
        vec![
            change("notes", "insert"),
            change("notes", "update"),
            change("counters", "update"),
            change("notes", "delete"),
        ],
        "{context}: expected the Change history to record each change made through Drizzle, as (table, kind, actor, user_id, request)"
    );
}

#[test]
fn the_own_database_as_the_user_inserts_updates_selects_and_deletes_one_operation_per_statement() {
    own_database_sequence_as("user");
}

#[test]
fn the_own_database_as_the_application_inserts_updates_selects_and_deletes_one_operation_per_statement()
 {
    own_database_sequence_as("application");
}
