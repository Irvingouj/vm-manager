//! Who is allowed (CONTRACT.md, "Who is allowed"; ticket "A User's read of a
//! shared database is authorized and logged": a User with no Grant is
//! refused and the refusal says whom to ask; a Resource the Version did not
//! declare is refused; a Grant at structure level does not let rows be
//! read; a refusal reaches Application code as a structured error carrying
//! the reason and a sentence on how to fix it).
//!
//! Each refusal is checked with every field of its row, in the table's
//! order where two rows apply at once, with its Operation log entry, and
//! with the shared database's own server log showing nothing was sent.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::User;
use blackbox::scenario::{
    CONTACT, Expected, READERS, RESOURCE, Scenario, assert_entry, assert_malformed, assert_refusal,
    assert_rows, lines,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A statement that, if it ever reached the shared database, would show in
/// its server log by a marker of its own.
fn probe(what: &str) -> (String, String) {
    let marker = format!("{what}-{}", token());
    (
        marker.clone(),
        format!("select '{marker}' as probe from incidents"),
    )
}

fn ask(resource: &str, sql: &str) -> Value {
    json!({ "resource": resource, "sql": sql })
}

/// Sends a query that must reach the database, so that the absence of
/// another in the server's log means something.
fn control(s: &Scenario) {
    let (marker, sql) = probe("control");
    let (_, answer) = s.query(&rita(), "board", &ask(RESOURCE, &sql));
    assert!(
        answer.get("ok").is_some(),
        "expected the control query to run, got {answer}"
    );
    s.db.assert_received(&marker, "the control query");
}

#[test]
fn with_no_request_in_flight_a_query_is_refused_and_logged_without_a_user() {
    let s = Scenario::ready();
    s.install("loader", "query-hostile.js", &[(RESOURCE, "read")]);
    let (response, body) = s.post(&rita(), "loader", "/at-load", &json!({}));
    drop(response);
    assert_refusal(
        &body["atLoad"],
        "no-request",
        &json!({}),
        "a query made while the script loaded",
    );
    let entries: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| e["application"] == "loader")
        .collect();
    assert!(
        !entries.is_empty(),
        "expected the refused query made at load to have an Operation log entry, got none"
    );
    for entry in &entries {
        assert_entry(
            entry,
            &Expected {
                request: None,
                user: None,
                ..Expected::refused("", "", "loader", "no-request")
            },
            "a query made with no request in flight",
        );
    }
    control(&s);
    s.db.assert_not_received("at-load-probe", "a query made at load");
}

#[test]
fn a_query_from_a_timer_after_its_request_ended_is_never_allowed() {
    let s = Scenario::ready();
    s.install("loader", "query-hostile.js", &[(RESOURCE, "read")]);
    let (marker, _) = probe("later");
    let sql = format!("select '{marker}' as later_probe from incidents");
    s.post(
        &rita(),
        "loader",
        "/later",
        &json!({ "args": ask(RESOURCE, &sql), "delayMs": 100 }),
    );
    std::thread::sleep(Duration::from_millis(1500));
    control(&s);
    s.db.assert_not_received(&marker, "a query made after its request ended");
    let later: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| {
            e["statement"]
                .as_str()
                .is_some_and(|t| t.contains("later_probe"))
        })
        .collect();
    for entry in &later {
        assert!(
            entry["outcome"] == "refused" && entry["reason"] == "no-request",
            "expected a query made after its request ended to be refused with no-request (or never to run), got the entry {entry}"
        );
    }
}

#[test]
fn a_resource_the_version_does_not_declare_is_refused_whether_or_not_it_is_registered() {
    let s = Scenario::ready();
    s.register("billing");
    s.grant("billing", READERS, "write");
    for resource in ["billing", "ghost"] {
        let (marker, sql) = probe(resource);
        let (response, answer) = s.query(&rita(), "board", &ask(resource, &sql));
        assert_refusal(
            &answer,
            "not-declared",
            &json!({ "resource": resource }),
            &format!("{resource}, not declared by board"),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                resource: Some(resource),
                ..Expected::refused(&id, "rita", "board", "not-declared")
            },
            &format!("the entry of the refused query on {resource}"),
        );
        s.db.assert_not_received(&marker, "a query on an undeclared Resource");
    }
}

#[test]
fn a_declared_resource_that_is_not_registered_is_refused() {
    let s = Scenario::ready();
    s.install(
        "dreamer",
        "query.js",
        &[("ghost", "read"), ("phantom", "structure")],
    );
    for resource in ["ghost", "phantom"] {
        let (response, answer) = s.query(&rita(), "dreamer", &ask(resource, "select 1"));
        // `phantom` is declared at structure: not-registered comes first.
        assert_refusal(
            &answer,
            "not-registered",
            &json!({ "resource": resource }),
            &format!("{resource}, declared but not registered"),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                resource: Some(resource),
                ..Expected::refused(&id, "rita", "dreamer", "not-registered")
            },
            "the entry of a query on a Resource not registered",
        );
    }
}

#[test]
fn a_resource_declared_at_structure_cannot_be_read_whatever_the_users_grants() {
    let s = Scenario::ready();
    s.grant(RESOURCE, "writers", "write");
    s.install("peek", "query.js", &[(RESOURCE, "structure")]);
    for user in [
        rita(),
        User::new("wes").in_groups(&["writers"]),
        // declared-lower comes before no-grant.
        User::new("nobody"),
    ] {
        let (marker, sql) = probe("declared-lower");
        let (response, answer) = s.query(&user, "peek", &ask(RESOURCE, &sql));
        assert_refusal(
            &answer,
            "declared-lower",
            &json!({ "resource": RESOURCE, "declared": "structure", "needed": "read" }),
            &format!("{} in an Application declaring structure", user.id),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected::refused(&id, &user.id, "peek", "declared-lower"),
            "the entry of a query refused as declared lower",
        );
        s.db.assert_not_received(&marker, "a query refused as declared lower");
    }
}

#[test]
fn a_user_with_no_grant_is_refused_and_told_whom_to_ask() {
    let s = Scenario::ready();
    s.grant(RESOURCE, "auditors", "structure");
    for user in [
        User::new("nobody"),
        User::new("fin").in_groups(&["finance", "everyone"]),
    ] {
        let (marker, sql) = probe("no-grant");
        let (response, answer) = s.query(&user, "board", &ask(RESOURCE, &sql));
        assert_refusal(
            &answer,
            "no-grant",
            &json!({
                "resource": RESOURCE, "user": user.id, "needed": "read", "held": null,
                "contact": CONTACT,
            }),
            &format!("{} in {:?}, holding no Grant", user.id, user.groups),
        );
        let fix = answer["refused"]["fix"].as_str().unwrap_or_default();
        assert!(
            fix.contains(CONTACT),
            "expected the fix to name the Resource's contact {CONTACT:?}, got {fix:?}"
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected::refused(&id, &user.id, "board", "no-grant"),
            "the entry of a query refused for no Grant",
        );
        s.db.assert_not_received(&marker, "a query refused for no Grant");
        let records = s.vm.records_of(&response);
        assert!(
            records
                .host_calls()
                .iter()
                .any(|r| r["operation"] == "query"
                    && r["outcome"] == "refused"
                    && r["reason"] == "no-grant"),
            "expected the program's log to record the refused host call with its reason, got {:?}",
            records.all
        );
    }
}

#[test]
fn a_grant_at_structure_does_not_let_rows_be_read() {
    let s = Scenario::ready();
    s.grant(RESOURCE, "auditors", "structure");
    let (marker, sql) = probe("structure-only");
    for user in [
        User::new("ada").in_groups(&["auditors"]),
        User::new("abe").in_groups(&["auditors", "finance"]),
    ] {
        let (response, answer) = s.query(&user, "board", &ask(RESOURCE, &sql));
        assert_refusal(
            &answer,
            "no-grant",
            &json!({
                "resource": RESOURCE, "user": user.id, "needed": "read", "held": "structure",
                "contact": CONTACT,
            }),
            &format!("{}, whose only Grant is structure", user.id),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected::refused(&id, &user.id, "board", "no-grant"),
            "the entry of a query refused for a structure Grant",
        );
    }
    control(&s);
    s.db.assert_not_received(&marker, "queries refused for a structure Grant");
}

#[test]
fn groups_are_compared_exactly() {
    let s = Scenario::ready();
    let (marker, sql) = probe("look-alike");
    // Letter case, a Cyrillic look-alike letter, a longer and a shorter name.
    for group in ["Readers", "READERS", "rеaders", "readers2", "reader"] {
        let user = User::new("lena").in_groups(&[group]);
        let (_, answer) = s.query(&user, "board", &ask(RESOURCE, &sql));
        assert_refusal(
            &answer,
            "no-grant",
            &json!({
                "resource": RESOURCE, "user": "lena", "needed": "read", "held": null,
                "contact": CONTACT,
            }),
            &format!("a User in {group:?}, which is not {READERS:?}"),
        );
    }
    // And a Grant to a look-alike does not reach the real Group.
    s.grant(RESOURCE, "wrіters", "write");
    let (_, answer) = s.query(
        &User::new("will").in_groups(&["writers"]),
        "board",
        &ask(RESOURCE, &sql),
    );
    assert_refusal(
        &answer,
        "no-grant",
        &json!({
            "resource": RESOURCE, "user": "will", "needed": "read", "held": null,
            "contact": CONTACT,
        }),
        "a User in \"writers\" when only a look-alike was granted",
    );
    control(&s);
    s.db.assert_not_received(&marker, "queries from look-alike Groups");
}

#[test]
fn the_rows_apply_in_the_contracts_order() {
    let s = Scenario::ready();
    // Not declared and not registered: not-declared.
    let (_, answer) = s.query(&User::new("nobody"), "board", &ask("ghost", "select 1"));
    assert_refusal(
        &answer,
        "not-declared",
        &json!({ "resource": "ghost" }),
        "undeclared, unregistered, no Grant",
    );
    // Declared at structure and not registered: not-registered.
    s.install("dreamer", "query.js", &[("ghost", "structure")]);
    let (_, answer) = s.query(&User::new("nobody"), "dreamer", &ask("ghost", "select 1"));
    assert_refusal(
        &answer,
        "not-registered",
        &json!({ "resource": "ghost" }),
        "declared at structure, unregistered, no Grant",
    );
    // With no request in flight, whatever else: no-request.
    s.install("loader", "query-hostile.js", &[]);
    let (_, body) = s.post(&User::new("nobody"), "loader", "/at-load", &json!({}));
    assert_refusal(
        &body["atLoad"],
        "no-request",
        &json!({}),
        "at load, in an Application that declares nothing",
    );
}

#[test]
fn a_malformed_query_reaches_no_database_and_has_no_entry() {
    let s = Scenario::ready();
    let (marker, sql) = probe("malformed");
    let calls = [
        json!({ "sql": sql }),
        json!({ "resource": RESOURCE }),
        json!({ "resource": RESOURCE, "sql": sql, "params": [{ "a": 1 }] }),
        json!({ "resource": RESOURCE, "sql": sql, "params": [[1]] }),
        json!({ "resource": RESOURCE, "sql": sql, "params": "not a list" }),
        json!({ "resource": RESOURCE, "sql": 7 }),
        json!({ "resource": 7, "sql": sql }),
        json!("select 1"),
        json!(null),
    ];
    let (response, answers) = s.run(&rita(), "board", &json!({ "calls": calls }));
    for (call, answer) in calls.iter().zip(&answers) {
        assert_malformed(answer, &format!("the arguments {call}"));
    }
    let entries = s.entries_of(&response);
    assert!(
        entries.is_empty(),
        "expected no Operation log entry for a malformed operation, got:\n{}",
        lines(&entries)
    );
    control(&s);
    s.db.assert_not_received(&marker, "malformed queries");
}

#[test]
fn whoami_is_not_an_operation_on_a_resource_and_has_no_entry() {
    let s = Scenario::ready();
    let response = s.vm.get_as(&rita(), "board", "/whoami");
    response.expect_status(200);
    assert!(
        response.json()["whoami"]["ok"].is_object(),
        "{}",
        response.describe()
    );
    let entries = s.entries_of(&response);
    assert!(
        entries.is_empty(),
        "expected whoami to have no Operation log entry, got:\n{}",
        lines(&entries)
    );
}

#[test]
fn each_refused_answer_still_lets_the_application_answer_its_request() {
    // A refusal is a value, not a rejection: the Application goes on and
    // answers, and its next query in the same request runs.
    let s = Scenario::ready();
    let (_, answers) = s.run(
        &rita(),
        "board",
        &json!({ "calls": [
            ask("ghost", "select 1"),
            ask(RESOURCE, "select count(*)::int4 as n from incidents"),
        ] }),
    );
    assert_refusal(
        &answers[0],
        "not-declared",
        &json!({ "resource": "ghost" }),
        "first",
    );
    assert_rows(
        &answers[1],
        &["n"],
        &json!([[3]]),
        "the query after a refusal",
    );
}
