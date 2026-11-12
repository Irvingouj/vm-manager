//! The login is the boundary: no statement of an operation at `read`
//! changes a row, a table or a sequence in the shared database, also when
//! the read login itself would be allowed to write (CONTRACT.md, "The login
//! is the boundary"; ticket "Write access, and the database role as the
//! boundary", second acceptance criterion).
//!
//! Every assertion is on the shared database itself, through the harness's
//! own connection (its fingerprint), never on the statement check: wherever
//! the check would stop the direct form of a write, a form that slips past
//! a statement parser is sent too (SQL text run by `ts_stat`, a write hidden
//! in IT's SECURITY DEFINER function, in a CTE, in a subquery, in a
//! `WHERE`), as `docs/research/what-postgres-sees.md` found. The Users
//! asking hold `write`, through an Application that declares `write`, so
//! that only the level of the operation, not the User's, stands between the
//! statement and a write.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{READERS, RESOURCE, Scenario, WRITERS, op, resource_body_with_write};
use serde_json::{Value, json};

fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS, READERS])
}

/// SQL text that `ts_stat` runs, unseen by a statement parser: `inner` must
/// give one `text` value.
fn hidden(inner: &str) -> String {
    format!("select * from ts_stat($vm${inner}$vm$)")
}

/// `hidden`, with the text turned into the `tsvector` ts_stat wants, of at
/// least one word whatever the expression gives.
fn hidden_call(expression: &str) -> String {
    hidden(&format!(
        "select to_tsvector('simple', 'called ' || coalesce(({expression})::text, ''))"
    ))
}

/// Statements an Application at `read` might send to write, each followed
/// in its list by what it hopes the earlier ones opened. Direct forms and
/// forms hidden from a parser.
fn attempts() -> Vec<Vec<String>> {
    let insert = "insert into incidents values (9, 'injected', true, 1, now())".to_owned();
    let close = "select close_every_incident()".to_owned();
    let mut lists: Vec<Vec<String>> = [
        "insert into incidents values (9, 'injected', true, 1, now())",
        "update incidents set title = 'changed'",
        "delete from incidents",
        "insert into notes (body) values ('through a read')",
        "with d as (delete from incidents returning *) select count(*) from d",
        "with i as (insert into notes (body) values ('cte') returning id) select id from i",
        "with u as (update incidents set open = false returning id) select id from u",
        // IT's function that writes with its owner's rights.
        "select close_every_incident()",
        "select title from incidents where close_every_incident() > 0",
        "select * from (select close_every_incident() as n) s",
        "select (select close_every_incident()) as n",
        "with c as (select close_every_incident() as n) select n from c",
        "select id from incidents where id in (select close_every_incident())",
        "update incidents set title = title returning close_every_incident()",
        // Sequences.
        "select nextval('ticket_numbers')",
        "select setval('ticket_numbers', 100)",
        "select (select nextval('ticket_numbers')) as n",
        "select id from incidents where nextval('ticket_numbers') > 0",
        // A table made from a result, a large object.
        "select * into stolen from incidents",
        "create table stolen as select * from incidents",
        "select lo_from_bytea(0, 'x')",
        "select lo_create(0)",
        // Running other SQL text.
        "select query_to_xml('select close_every_incident()', true, true, '')",
        "select query_to_xml('insert into notes (body) values (''xml'') returning id', true, true, '')",
    ]
    .iter()
    .map(|sql| vec![(*sql).to_owned()])
    .collect();
    for hidden_sql in [
        hidden_call("close_every_incident()"),
        hidden_call("nextval('ticket_numbers')"),
        hidden_call("setval('ticket_numbers', 100)"),
        hidden_call("lo_from_bytea(0, 'x')"),
        hidden_call("query_to_xml('select close_every_incident()', true, true, '')"),
        hidden(
            "with d as (delete from incidents returning title) select to_tsvector('simple', title) from d",
        ),
        format!(
            "select title from incidents where exists ({})",
            hidden_call("close_every_incident()")
        ),
        format!(
            "with h as ({}) select * from h",
            hidden_call("close_every_incident()")
        ),
    ] {
        lists.push(vec![hidden_sql]);
    }
    // Leaving the read-only transaction, then writing: in the same
    // operation, and in later ones.
    for opener in [
        hidden_call("set_config('transaction_read_only', 'off', false)"),
        hidden_call("set_config('transaction_read_only', 'off', true)"),
        hidden_call("set_config('default_transaction_read_only', 'off', false)"),
        hidden_call("set_config('session_replication_role', 'replica', false)"),
        "select set_config('transaction_read_only', 'off', false)".to_owned(),
        "select set_config('default_transaction_read_only', 'off', false)".to_owned(),
        "set transaction read write".to_owned(),
        "commit".to_owned(),
    ] {
        lists.push(vec![opener, insert.clone(), close.clone()]);
    }
    for both in [
        format!(
            "select close_every_incident() from ({}) h",
            hidden_call("set_config('transaction_read_only', 'off', true)")
        ),
        format!(
            "with h as ({}) select close_every_incident() from h",
            hidden_call("set_config('default_transaction_read_only', 'off', false)")
        ),
    ] {
        lists.push(vec![both]);
    }
    lists
}

/// Sends every attempt through `query` on `resource` as `user` in `board`,
/// each list in one request, then the inserts again from fresh requests
/// made together (to reach other pooled connections); fails unless the
/// shared database is exactly as before.
fn assert_attempts_change_nothing(s: &Scenario, resource: &str, user: &User) {
    let before = s.db.fingerprint();
    let mut seen = Vec::new();
    for list in attempts() {
        let calls: Vec<Value> = list
            .iter()
            .map(|sql| op("query", &json!({ "resource": resource, "sql": sql })))
            .collect();
        let (_, answers) = s.run(user, "board", &json!({ "calls": calls }));
        for (sql, answer) in list.iter().zip(answers) {
            seen.push(format!("  {sql}\n    -> {answer}"));
        }
        let after = s.db.fingerprint();
        assert_eq!(
            after,
            before,
            "expected the shared database unchanged after these operations at read through {resource:?} as {} (who holds write), but it changed. What the operations answered:\n{}",
            user.id,
            seen.join("\n")
        );
    }
    let later = s.spread(
        &[(user.clone(), "board"), (user.clone(), "board")],
        &op(
            "query",
            &json!({ "resource": resource, "sql": "insert into incidents values (9, 'injected', true, 1, now())" }),
        ),
        8,
    );
    assert!(
        later.iter().all(|a| a.get("ok").is_none()),
        "expected every later insert through query to fail, got {later:?}"
    );
    assert_eq!(s.db.fingerprint(), before, "after the later inserts");
}

/// The write scenario, with `loose`: a Resource over the same database
/// whose `read` login is the careless one (which could write), with the
/// write login too, granted `write` to [`WRITERS`] and declared at `write`
/// by `board`.
fn scenario() -> Scenario {
    let s = Scenario::start();
    s.register_writable(RESOURCE);
    s.register_body(
        "loose",
        &resource_body_with_write("loose", &s.db, &s.db.careless, &s.db.writer),
    );
    for resource in [RESOURCE, "loose"] {
        s.grant(resource, WRITERS, "write");
    }
    s.install(
        "board",
        "operations.js",
        &[(RESOURCE, "write"), ("loose", "write")],
    );
    s
}

#[test]
fn no_statement_through_query_writes_with_the_read_login() {
    let s = scenario();
    assert_attempts_change_nothing(&s, RESOURCE, &wanda());
}

#[test]
fn no_statement_through_query_writes_even_when_the_read_login_could() {
    let s = scenario();
    assert_attempts_change_nothing(&s, "loose", &wanda());
}
