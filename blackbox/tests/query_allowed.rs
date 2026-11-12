//! The honest path (ticket "A User's read of a shared database is authorized
//! and logged": a User in a granted Group gets rows; one Grant holds in two
//! different Applications with nothing set up per Application). CONTRACT.md,
//! "Operations on a shared database" and "Who is allowed".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::records::event;
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, assert_database_error, assert_entry, assert_refusal,
    assert_rows,
};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

const OPEN_INCIDENTS: &str = "select id, title from incidents where open = $1 order by id";

#[test]
fn a_user_in_a_granted_group_gets_rows_and_the_query_is_logged() {
    let s = Scenario::ready();
    let (response, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": OPEN_INCIDENTS, "params": [true] }),
    );
    assert_rows(
        &answer,
        &["id", "title"],
        &json!([[1, "Disk full on db-7"], [3, s.db.row_marker]]),
        "rita, in a Group granted read, querying open incidents",
    );
    let id = response.request_id();
    let entry = s.entry_of(&response);
    assert_entry(
        &entry,
        &Expected {
            statement: Some(OPEN_INCIDENTS),
            rows: Some(2),
            ..Expected::allowed(&id, "rita", "board")
        },
        "the Operation log entry of rita's query",
    );
    // The program's own log has its host-call record, as before.
    let records = s.vm.records_of(&response);
    let calls = records.host_calls();
    assert!(
        calls
            .iter()
            .any(|r| r["operation"] == "query" && r["outcome"] == "allowed"),
        "expected a `host-call` record for the allowed query, got {:?}",
        records.all
    );
    records.assert_host_saw_only("rita", "board");
}

#[test]
fn a_statement_without_params_runs_too() {
    let s = Scenario::ready();
    for args in [
        json!({ "resource": RESOURCE, "sql": "select count(*)::int4 as n from incidents" }),
        json!({ "resource": RESOURCE, "sql": "select count(*)::int4 as n from incidents", "params": [] }),
    ] {
        let (_, answer) = s.query(&rita(), "board", &args);
        assert_rows(&answer, &["n"], &json!([[3]]), &format!("{args}"));
    }
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": "select id from incidents where id > 100" }),
    );
    assert_rows(&answer, &["id"], &json!([]), "a query with no rows");
}

#[test]
fn every_value_is_encoded_as_the_contract_says() {
    let s = Scenario::ready();
    let sql = "select n, b, i2, i4, i8, f4, f8, num, t, ts, u, jb, j from kinds where label = $1";
    let columns = [
        "n", "b", "i2", "i4", "i8", "f4", "f8", "num", "t", "ts", "u", "jb", "j",
    ];
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": sql, "params": ["every"] }),
    );
    // null; a boolean; numbers for int2, int4, float4, float8; parsed JSON
    // for json and jsonb; the text Postgres prints for the rest (int8,
    // numeric, timestamptz in the database's UTC and ISO style, uuid).
    assert_rows(
        &answer,
        &columns,
        &json!([[
            null,
            true,
            -12,
            2_147_483_647,
            "9007199254740993",
            1.5,
            -0.25,
            "12345678901234567890.0123",
            "héllo ☃ \"quoted\" \\ back",
            "2026-01-02 03:04:05.678+00",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
            { "a": "x", "b": [1, 2.5, null] },
            { "k": true },
        ]]),
        "a row of every type",
    );
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": sql, "params": ["nulls"] }),
    );
    assert_rows(
        &answer,
        &columns,
        &json!([[
            null, null, null, null, null, null, null, null, null, null, null, null, null
        ]]),
        "a row of nulls of every type",
    );
}

#[test]
fn params_are_sent_as_text_for_the_server_to_interpret() {
    let s = Scenario::ready();
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({
            "resource": RESOURCE,
            "sql": "select $1::int4 + 1 as a, $2::text as b, $3::bool as c, $4::text is null as d, $5::numeric * 2 as e, $6::int8 as f",
            "params": [41, "x", true, null, 1.5, "9007199254740993"],
        }),
    );
    assert_rows(
        &answer,
        &["a", "b", "c", "d", "e", "f"],
        &json!([[42, "x", true, true, "3.0", "9007199254740993"]]),
        "params of each kind",
    );
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": "select title from incidents where id = $1", "params": ["2"] }),
    );
    assert_rows(
        &answer,
        &["title"],
        &json!([["Login page slow"]]),
        "a param given as a string, interpreted by the server as int4",
    );
}

#[test]
fn the_query_runs_as_the_resources_read_login() {
    let s = Scenario::ready();
    let marker = format!("as-login-{}", blackbox::shared_database::token());
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": format!("select '{marker}' as probe") }),
    );
    assert_rows(&answer, &["probe"], &json!([[marker]]), "a probe");
    s.db.assert_received(&marker, "the probe");
    let from: Vec<String> =
        s.db.server_log()
            .into_iter()
            .filter(|e| e.contains(&marker))
            .collect();
    assert!(
        from.iter()
            .all(|e| e.contains(&format!(" user={}:", s.db.reader.role))),
        "expected the probe to reach the shared database as the Resource's read login {}, got:\n{}",
        s.db.reader.role,
        from.join("\n")
    );
    // What the read login may not read, the query may not either.
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": "select note from staff_only" }),
    );
    assert_database_error(
        &answer,
        "42501",
        "a table the read login has no privilege on",
    );
}

#[test]
fn a_user_holds_the_highest_level_any_of_their_groups_is_granted() {
    let s = Scenario::ready();
    s.grant(RESOURCE, "auditors", "structure");
    s.grant(RESOURCE, "writers", "write");
    let args = json!({ "resource": RESOURCE, "sql": "select count(*)::int4 as n from incidents" });
    for user in [
        User::new("ann").in_groups(&["auditors", READERS]),
        User::new("ben").in_groups(&[READERS, "auditors"]),
        User::new("cat").in_groups(&["auditors", "writers"]),
        User::new("dan").in_groups(&["writers"]),
        User::new("eve").in_groups(&["everyone", "finance", READERS]),
    ] {
        let (_, answer) = s.query(&user, "board", &args);
        assert_rows(
            &answer,
            &["n"],
            &json!([[3]]),
            &format!("{} in {:?}", user.id, user.groups),
        );
    }
}

#[test]
fn one_grant_holds_in_two_applications_with_nothing_set_up_per_application() {
    let s = Scenario::ready();
    s.install("ledger", "query.js", &[(RESOURCE, "read")]);
    let args = json!({ "resource": RESOURCE, "sql": OPEN_INCIDENTS, "params": [false] });
    for application in ["board", "ledger"] {
        let (response, answer) = s.query(&rita(), application, &args);
        assert_rows(
            &answer,
            &["id", "title"],
            &json!([[2, "Login page slow"]]),
            &format!("rita in {application}"),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                statement: Some(OPEN_INCIDENTS),
                rows: Some(1),
                ..Expected::allowed(&id, "rita", application)
            },
            &format!("the entry of rita's query in {application}"),
        );
    }
}

#[test]
fn an_application_that_does_not_declare_the_resource_cannot_reach_it_whatever_the_users_grants() {
    let s = Scenario::ready();
    s.grant(RESOURCE, "writers", "write");
    s.install("other", "query.js", &[]);
    s.install("elsewhere", "query.js", &[("billing", "read")]);
    let marker = format!("undeclared-{}", blackbox::shared_database::token());
    for application in ["other", "elsewhere"] {
        for user in [rita(), User::new("wes").in_groups(&["writers", READERS])] {
            let (response, answer) = s.query(
                &user,
                application,
                &json!({ "resource": RESOURCE, "sql": format!("select '{marker}' as probe") }),
            );
            assert_refusal(
                &answer,
                "not-declared",
                &json!({ "resource": RESOURCE }),
                &format!(
                    "{} in {application}, which does not declare {RESOURCE}",
                    user.id
                ),
            );
            let id = response.request_id();
            assert_entry(
                &s.entry_of(&response),
                &Expected::refused(&id, &user.id, application, "not-declared"),
                "the entry of a query on an undeclared Resource",
            );
        }
    }
    // The same User reaches it through an Application that declares it,
    // which also shows the statement log is working.
    let (_, answer) = s.query(
        &rita(),
        "board",
        &json!({ "resource": RESOURCE, "sql": "select 'declared-control' as probe" }),
    );
    assert_rows(
        &answer,
        &["probe"],
        &json!([["declared-control"]]),
        "control",
    );
    s.db.assert_received("declared-control", "the control query");
    s.db.assert_not_received(&marker, "queries refused as not declared");
}

#[test]
fn a_later_grant_for_the_same_group_takes_effect_for_the_next_request() {
    let s = Scenario::ready();
    let args = json!({ "resource": RESOURCE, "sql": "select count(*)::int4 as n from incidents" });
    let (_, answer) = s.query(&rita(), "board", &args);
    assert_rows(&answer, &["n"], &json!([[3]]), "with read");
    s.grant(RESOURCE, READERS, "structure");
    let (_, answer) = s.query(&rita(), "board", &args);
    assert_refusal(
        &answer,
        "no-grant",
        &json!({
            "resource": RESOURCE, "user": "rita", "needed": "read", "held": "structure",
            "contact": blackbox::scenario::CONTACT,
        }),
        "after the Grant was lowered to structure",
    );
    s.grant(RESOURCE, READERS, "read");
    let (_, answer) = s.query(&rita(), "board", &args);
    assert_rows(&answer, &["n"], &json!([[3]]), "after it was raised again");
}

#[test]
fn queries_made_together_in_one_request_each_get_their_own_answer_and_entry() {
    let s = Scenario::ready();
    let calls: Vec<Value> = (1..=3)
        .map(|id| {
            json!({ "resource": RESOURCE, "sql": "select id from incidents where id = $1", "params": [id] })
        })
        .collect();
    let (response, answers) = s.run(
        &rita(),
        "board",
        &json!({ "calls": calls, "mode": "together" }),
    );
    for (i, answer) in answers.iter().enumerate() {
        assert_rows(
            answer,
            &["id"],
            &json!([[i + 1]]),
            &format!("call {i} of three made together"),
        );
    }
    let entries = s.entries_of(&response);
    assert_eq!(
        entries.len(),
        3,
        "expected three entries for three queries, got {entries:?}"
    );
    let id = response.request_id();
    for entry in &entries {
        assert_entry(
            entry,
            &Expected {
                statement: Some("select id from incidents where id = $1"),
                rows: Some(1),
                ..Expected::allowed(&id, "rita", "board")
            },
            "each of three",
        );
    }
    assert_eq!(
        s.vm.records_of(&response)
            .all
            .iter()
            .filter(|r| event(r) == "host-call" && r["operation"] == "query")
            .count(),
        3,
        "expected a host-call record per query"
    );
}
