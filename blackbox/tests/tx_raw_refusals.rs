//! What `tx.begin`, and `query` / `execute` with `tx`, refuse or take as
//! malformed (CONTRACT.md, "Transactions: Through the raw interface"):
//!
//! - malformed arguments: no `access`, an unknown `access` or `isolation`,
//!   no target or two, a bad `own` or `as`; `tx` beside `resource`, `own`
//!   or `as`. Nothing reaches the database, and there is no Operation log
//!   entry;
//! - the refusals of `tx.begin`, in the order of "Who is allowed"
//!   (`not-own-database`, `not-declared`, `not-registered`,
//!   `declared-lower`, `no-grant`), each logged as a refused `begin` with
//!   `transaction` null;
//! - `unavailable` when the pool has no connection to give: a Resource
//!   whose login Postgres lets have one connection only, so a second
//!   transaction cannot get one while the first holds it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::scenario::{
    CONTACT, READERS, RESOURCE, assert_error_kind, assert_malformed, assert_refusal, lines,
};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, GHOST, Target, TxEntry, Txs, VIEWER, assert_began, assert_ended, assert_tx_entry, begin,
    begin_with, commit, insert_marker, install, nobody, query_in, rita, step, tx, wanda,
};
use serde_json::{Value, json};

#[test]
fn malformed_begins_are_answered_malformed_and_reach_nothing() {
    let t = Txs::start();
    let user = wanda();
    let cases: Vec<(&str, Value)> = vec![
        ("no access", json!({ "resource": RESOURCE })),
        ("no access, own", json!({ "own": true, "as": "user" })),
        (
            "an unknown access",
            json!({ "resource": RESOURCE, "access": "admin" }),
        ),
        (
            "an access that is not a string",
            json!({ "resource": RESOURCE, "access": 1 }),
        ),
        (
            "a null access",
            json!({ "resource": RESOURCE, "access": null }),
        ),
        (
            "an unknown isolation",
            json!({ "resource": RESOURCE, "access": "write", "isolation": "snapshot" }),
        ),
        (
            "an isolation in capitals",
            json!({ "resource": RESOURCE, "access": "write", "isolation": "SERIALIZABLE" }),
        ),
        (
            "read uncommitted, which is not one of the three",
            json!({ "own": true, "access": "write", "isolation": "read uncommitted" }),
        ),
        (
            "an isolation carrying SQL",
            json!({ "resource": RESOURCE, "access": "write", "isolation": "serializable; drop table notes" }),
        ),
        (
            "an isolation that is not a string",
            json!({ "own": true, "access": "read", "isolation": 3 }),
        ),
        ("no target", json!({ "access": "write" })),
        (
            "both targets",
            json!({ "resource": RESOURCE, "own": true, "access": "write" }),
        ),
        (
            "own not exactly true",
            json!({ "own": "yes", "access": "write" }),
        ),
        (
            "an unknown actor",
            json!({ "own": true, "as": "admin", "access": "write" }),
        ),
        ("arguments that are not an object", json!("devops")),
        ("null arguments", Value::Null),
        ("an array", json!([RESOURCE, "write"])),
    ];
    let steps: Vec<Value> = cases
        .iter()
        .map(|(_, args)| begin_with(args.clone(), "never"))
        .collect();
    let (response, answers) = t.run(&user, APP, &steps);
    for ((what, args), answer) in cases.iter().zip(&answers) {
        assert_malformed(answer, &format!("tx.begin with {what}: {args}"));
    }
    let entries = t.entries_of(&response);
    assert!(
        entries.is_empty(),
        "a malformed tx.begin has no Operation log entry, got:\n{}",
        lines(&entries)
    );
    t.db()
        .assert_not_received("drop table notes", "a malformed isolation");
    assert!(
        t.sessions_in_transaction(Target::Shared(RESOURCE), std::time::Duration::from_secs(2))
            .is_empty(),
        "a malformed tx.begin leaves no transaction open"
    );
}

#[test]
fn tx_beside_resource_own_or_as_is_malformed_and_runs_nothing() {
    let t = Txs::start();
    let user = wanda();
    let marker = format!("beside-{}", token());
    let target = Target::Shared(RESOURCE);
    let insert = insert_marker(target, &marker);
    let mut steps = vec![begin(target, "write", "t")];
    let extras = [
        json!({ "resource": RESOURCE }),
        json!({ "own": true }),
        json!({ "as": "user" }),
        json!({ "as": "application" }),
        json!({ "resource": RESOURCE, "as": "user" }),
    ];
    for operation in ["query", "execute"] {
        for extra in &extras {
            let mut args = extra.clone();
            args["tx"] = tx("t");
            args["sql"] = json!(if operation == "query" {
                "select 1 as one".to_owned()
            } else {
                insert.clone()
            });
            steps.push(step(operation, args));
        }
    }
    steps.push(commit("t"));
    let (response, answers) = t.run(&user, APP, &steps);
    let id = assert_began(&answers[0], "begin");
    for (i, answer) in answers[1..answers.len() - 1].iter().enumerate() {
        assert_malformed(
            answer,
            &format!("call {i}: `tx` beside {:?}", steps[i + 1]["args"]),
        );
    }
    assert_ended(
        answers.last().unwrap(),
        "the transaction is still there to commit",
    );
    assert_eq!(t.shared_notes_with(&marker), 0, "nothing malformed ran");
    let entries = t.entries_of(&response);
    let operations: Vec<&str> = entries
        .iter()
        .map(|e| e["operation"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        operations,
        vec!["begin", "commit"],
        "a malformed call has no Operation log entry:\n{}",
        lines(&entries)
    );
    assert_eq!(entries[0]["transaction"].as_str(), Some(id.as_str()));
}

/// The refused `begin` entry of the request `response` answers.
fn refused_begin(
    t: &Txs,
    response: &blackbox::Response,
    reason: &str,
    user: &str,
    application: &str,
    resource: Option<&str>,
    actor: &str,
) {
    let entries = t.entries_of(response);
    assert_eq!(
        entries.len(),
        1,
        "a refused tx.begin has one Operation log entry:\n{}",
        lines(&entries)
    );
    assert_tx_entry(
        &entries[0],
        &TxEntry {
            operation: "begin",
            transaction: None,
            outcome: "refused",
            reason: Some(reason),
            error: None,
            resource,
            actor,
            request: &response.request_id(),
            user,
            application,
        },
        &format!("a tx.begin refused with {reason}"),
    );
}

#[test]
fn begin_is_refused_as_an_operation_needing_its_access_in_the_contract_order() {
    let t = Txs::start();

    // not-own-database comes right after no-request: before not-declared.
    for resource in [RESOURCE, "payroll"] {
        let (response, answers) = t.run(
            &wanda(),
            APP,
            &[begin_with(
                json!({ "resource": resource, "as": "application", "access": "read" }),
                "t",
            )],
        );
        assert_refusal(
            &answers[0],
            "not-own-database",
            &json!({ "resource": resource }),
            &format!("tx.begin as the Application on {resource:?}"),
        );
        refused_begin(
            &t,
            &response,
            "not-own-database",
            "wanda",
            APP,
            Some(resource),
            "application",
        );
    }

    let (response, answers) = t.run(
        &wanda(),
        APP,
        &[begin(Target::Shared("payroll"), "read", "t")],
    );
    assert_refusal(
        &answers[0],
        "not-declared",
        &json!({ "resource": "payroll" }),
        "tx.begin on a Resource the Version does not declare",
    );
    refused_begin(
        &t,
        &response,
        "not-declared",
        "wanda",
        APP,
        Some("payroll"),
        "user",
    );

    let (response, answers) = t.run(&wanda(), APP, &[begin(Target::Shared(GHOST), "read", "t")]);
    assert_refusal(
        &answers[0],
        "not-registered",
        &json!({ "resource": GHOST }),
        "tx.begin on a declared Resource that is not registered",
    );
    refused_begin(
        &t,
        &response,
        "not-registered",
        "wanda",
        APP,
        Some(GHOST),
        "user",
    );

    // declared-lower before no-grant: a User with no Grant at all.
    let (response, answers) = t.run(
        &nobody(),
        VIEWER,
        &[begin(Target::Shared(RESOURCE), "write", "t")],
    );
    assert_refusal(
        &answers[0],
        "declared-lower",
        &json!({ "resource": RESOURCE, "declared": "read", "needed": "write" }),
        "tx.begin at write on a Resource declared at read",
    );
    refused_begin(
        &t,
        &response,
        "declared-lower",
        "nobody",
        VIEWER,
        Some(RESOURCE),
        "user",
    );

    let (response, answers) = t.run(
        &nobody(),
        APP,
        &[begin(Target::Shared(RESOURCE), "read", "t")],
    );
    assert_refusal(
        &answers[0],
        "no-grant",
        &json!({ "resource": RESOURCE, "user": "nobody", "needed": "read", "held": null, "contact": CONTACT }),
        "tx.begin at read by a User with no Grant",
    );
    refused_begin(
        &t,
        &response,
        "no-grant",
        "nobody",
        APP,
        Some(RESOURCE),
        "user",
    );

    let (response, answers) = t.run(
        &rita(),
        APP,
        &[begin(Target::Shared(RESOURCE), "write", "t")],
    );
    assert_refusal(
        &answers[0],
        "no-grant",
        &json!({ "resource": RESOURCE, "user": "rita", "needed": "write", "held": "read", "contact": CONTACT }),
        "tx.begin at write by a User holding read",
    );
    refused_begin(
        &t,
        &response,
        "no-grant",
        "rita",
        APP,
        Some(RESOURCE),
        "user",
    );

    // At read, the same User begins.
    let (_, answers) = t.run(
        &rita(),
        APP,
        &[begin(Target::Shared(RESOURCE), "read", "t"), commit("t")],
    );
    assert_began(&answers[0], "tx.begin at read by a User holding read");
    assert_ended(&answers[1], "commit");
}

#[test]
fn a_pool_with_no_connection_to_give_answers_unavailable() {
    let t = Txs::start();
    // A login Postgres lets have one connection at a time.
    let role = format!("tight_role_{}", token());
    let password = format!("pw-tight-{}", token());
    let db = t.db();
    db.superuser().batch(&format!(
        "CREATE ROLE {role} LOGIN PASSWORD '{password}' CONNECTION LIMIT 1;
         GRANT CONNECT ON DATABASE {name} TO {role};
         GRANT USAGE ON SCHEMA public TO {role};
         GRANT SELECT ON incidents TO {role};",
        name = db.name()
    ));
    let s = &t.scenario;
    s.register_body(
        "tight",
        &json!({
            "name": "tight", "kind": "postgres", "description": "One connection only",
            "owner": "The DevOps team", "contact": CONTACT,
            "connection": {
                "host": db.host(), "port": db.port(), "database": db.name(),
                "read": { "role": role, "password": password },
            },
        }),
    );
    s.grant("tight", READERS, "read");
    install(t.vm(), "tightly", &[("tight", "read")]);
    let target = Target::Shared("tight");
    let (_, answers) = t.run(
        &rita(),
        "tightly",
        &[
            begin(target, "read", "a"),
            query_in("a", "select count(*)::int4 as n from incidents"),
            begin(target, "read", "b"),
            commit("a"),
            begin(target, "read", "c"),
            query_in("c", "select count(*)::int4 as n from incidents"),
            commit("c"),
        ],
    );
    assert_began(
        &answers[0],
        "the first transaction takes the one connection",
    );
    assert_eq!(answers[1]["ok"]["rows"], json!([[3]]), "{}", answers[1]);
    assert_error_kind(
        &answers[2],
        "unavailable",
        "a second tx.begin while the only connection is held by the first",
    );
    let text = answers[2].to_string();
    for (what, value) in [
        ("the login", role.as_str()),
        ("its password", password.as_str()),
        ("the database", db.name()),
    ] {
        assert!(
            !text.contains(value),
            "`unavailable` says nothing about the connection, but it names {what}: {text}"
        );
    }
    assert_ended(&answers[3], "commit of the first");
    assert_began(
        &answers[4],
        "once the first ended its connection can be given again",
    );
    assert_eq!(answers[5]["ok"]["rows"], json!([[3]]), "{}", answers[5]);
    assert_ended(&answers[6], "commit");
}
