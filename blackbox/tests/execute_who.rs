//! Who may write (CONTRACT.md, "Writing to a shared database, and the role
//! as the boundary", with "Who is allowed"; ticket "Write access, and the
//! database role as the boundary", third acceptance criterion).
//!
//! `execute` needs Access level `write` from both sides: an Application
//! that declares the Resource at `read` is refused `declared-lower`, and a
//! User whose Groups hold only `read` is refused `no-grant` (with `held` and
//! `needed`) even when the Application declares `write`. A User may hold
//! `write` through any one of several Groups. A Resource registered without
//! a write login answers `read-only-resource` and nothing is sent. `query`
//! is unchanged whatever the levels: the read login, in a read-only
//! transaction, also for a User with `write`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{
    CONTACT, Expected, READERS, RESOURCE, Scenario, WRITERS, assert_entry, assert_error_kind,
    assert_executed, assert_refusal, assert_rows, lines, op, sql_on,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS])
}

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// An insert into `notes` that carries an unmistakable marker, and the
/// marker.
fn marked_insert(what: &str) -> (String, Value) {
    let marker = format!("{what}-{}", token());
    let args = sql_on(
        RESOURCE,
        &format!("insert into notes (body) values ('{marker}') returning id"),
    );
    (marker, args)
}

#[test]
fn an_application_declaring_less_than_write_is_refused_a_write() {
    let s = Scenario::writable();
    s.install("peek", "operations.js", &[(RESOURCE, "structure")]);
    s.install("sneak", "operations.js", &[]);
    s.install("ghostly", "operations.js", &[("ghost", "write")]);
    let before = s.db.fingerprint();
    let (marker, args) = marked_insert("declared-lower");
    let cases = [
        (
            "viewer",
            RESOURCE,
            "declared-lower",
            json!({ "resource": RESOURCE, "declared": "read", "needed": "write" }),
        ),
        (
            "peek",
            RESOURCE,
            "declared-lower",
            json!({ "resource": RESOURCE, "declared": "structure", "needed": "write" }),
        ),
        (
            "sneak",
            RESOURCE,
            "not-declared",
            json!({ "resource": RESOURCE }),
        ),
        (
            "ghostly",
            "ghost",
            "not-registered",
            json!({ "resource": "ghost" }),
        ),
    ];
    for (application, resource, reason, fields) in cases {
        let mut args = args.clone();
        args["resource"] = json!(resource);
        let (response, answer) = s.execute(&wanda(), application, &args);
        let context = format!("wanda (write Grant) in {application} executing on {resource}");
        assert_refusal(&answer, reason, &fields, &context);
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                operation: "execute",
                resource: Some(resource),
                application,
                ..Expected::refused(&id, "wanda", application, reason)
            },
            &context,
        );
    }
    s.db.assert_not_received(&marker, "writes refused for the declaration");
    assert_eq!(s.db.fingerprint(), before, "expected nothing written");
}

#[test]
fn a_user_whose_grant_is_read_is_refused_a_write_even_when_the_application_declares_write() {
    let s = Scenario::writable();
    s.grant(RESOURCE, "auditors", "structure");
    let before = s.db.fingerprint();
    let (marker, args) = marked_insert("no-grant");
    let cases = [
        (rita(), json!("read")),
        (
            User::new("ruth").in_groups(&["finance", READERS, "auditors"]),
            json!("read"),
        ),
        (
            User::new("sam").in_groups(&["auditors"]),
            json!("structure"),
        ),
        (User::new("nobody").in_groups(&["finance"]), json!(null)),
        (User::new("nogroups"), json!(null)),
    ];
    for (user, held) in cases {
        let (response, answer) = s.execute(&user, "board", &args);
        let context = format!(
            "{} in {:?} executing in board (declares write)",
            user.id, user.groups
        );
        assert_refusal(
            &answer,
            "no-grant",
            &json!({
                "resource": RESOURCE, "user": user.id, "needed": "write", "held": held,
                "contact": CONTACT,
            }),
            &context,
        );
        assert!(
            answer["refused"]["fix"]
                .as_str()
                .is_some_and(|f| f.contains(CONTACT)),
            "{context}: expected the fix to name the Resource's contact, got {answer}"
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                operation: "execute",
                ..Expected::refused(&id, &user.id, "board", "no-grant")
            },
            &context,
        );
    }
    s.db.assert_not_received(&marker, "writes refused for want of a write Grant");
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected the shared database unchanged"
    );
}

#[test]
fn a_user_holding_write_through_one_of_several_groups_may_write() {
    let s = Scenario::writable();
    s.grant(RESOURCE, "auditors", "structure");
    for (i, user) in [
        User::new("wes").in_groups(&["finance", READERS, WRITERS, "everyone"]),
        User::new("wyn").in_groups(&["auditors", WRITERS]),
        User::new("wil").in_groups(&[WRITERS, READERS]),
    ]
    .into_iter()
    .enumerate()
    {
        let body = format!("by-{}-{i}", user.id);
        let (response, answer) = s.execute(
            &user,
            "board",
            &json!({ "resource": RESOURCE, "sql": "insert into notes (body) values ($1) returning body", "params": [body] }),
        );
        assert_executed(
            &answer,
            &["body"],
            &json!([[body]]),
            1,
            &format!("{} in {:?}", user.id, user.groups),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected::executed(&id, &user.id, "board", 1),
            &format!("the entry of {}'s write", user.id),
        );
    }
}

#[test]
fn a_grant_raised_to_write_applies_from_the_next_request() {
    let s = Scenario::writable();
    let user = User::new("raised").in_groups(&["ops"]);
    s.grant(RESOURCE, "ops", "read");
    let (_, answer) = s.execute(
        &user,
        "board",
        &sql_on(
            RESOURCE,
            "insert into notes (body) values ('raised') returning body",
        ),
    );
    assert_eq!(
        answer["refused"]["refusal"]["held"], "read",
        "with a read Grant: {answer}"
    );
    s.grant(RESOURCE, "ops", "write");
    let (_, answer) = s.execute(
        &user,
        "board",
        &sql_on(
            RESOURCE,
            "insert into notes (body) values ('raised') returning body",
        ),
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([["raised"]]),
        1,
        "after the Grant was raised to write",
    );
}

#[test]
fn a_resource_without_a_write_login_answers_read_only_resource_and_sends_nothing() {
    let s = Scenario::writable();
    s.register("archive");
    s.grant("archive", WRITERS, "write");
    s.install("archivist", "operations.js", &[("archive", "write")]);
    let before = s.db.fingerprint();
    let marker = format!("read-only-resource-{}", token());
    let (response, answers) = s.run(
        &wanda(),
        "archivist",
        &json!({ "calls": [
            op("execute", &sql_on("archive", &format!("insert into notes (body) values ('{marker}') returning id"))),
            op("query", &sql_on("archive", "select count(*)::int4 as n from incidents")),
        ] }),
    );
    assert_error_kind(
        &answers[0],
        "read-only-resource",
        "execute on a Resource registered without a write login",
    );
    assert_rows(
        &answers[1],
        &["n"],
        &json!([[3]]),
        "query on that Resource still reads",
    );
    s.db.assert_not_received(&marker, "an execute on a Resource without a write login");
    assert_eq!(s.db.fingerprint(), before);
    let entries = s.entries_of(&response);
    assert_eq!(entries.len(), 2, "{}", lines(&entries));
    let id = response.request_id();
    assert_entry(
        &entries[0],
        &Expected {
            operation: "execute",
            resource: Some("archive"),
            ran: Some(false),
            ..Expected::failed(&id, "wanda", "archivist", "read-only-resource")
        },
        "the entry of an execute that never reached the database, recording why",
    );
    let text = entries[0].to_string();
    assert!(
        !text.contains(&marker),
        "expected no value in the entry, got {text}"
    );
}

#[test]
fn query_runs_as_the_read_login_in_a_read_only_transaction_whatever_the_levels() {
    let s = Scenario::writable();
    let both = User::new("bea").in_groups(&[READERS, WRITERS]);
    for (user, application) in [
        (rita(), "viewer"),
        (rita(), "board"),
        (wanda(), "viewer"),
        (wanda(), "board"),
        (both.clone(), "board"),
    ] {
        let marker = format!("query-as-{}-{application}-{}", user.id, token());
        let (_, answer) = s.one(
            &user,
            application,
            &op(
                "query",
                &sql_on(
                    RESOURCE,
                    &format!(
                        "select current_user::text as u, session_user::text as s, \
                         current_setting('transaction_read_only') as ro, '{marker}' as m"
                    ),
                ),
            ),
        );
        assert_rows(
            &answer,
            &["u", "s", "ro", "m"],
            &json!([[s.db.reader.role, s.db.reader.role, "on", marker]]),
            &format!(
                "{} in {application}: query as the read login, read-only",
                user.id
            ),
        );
        s.db.assert_marker_came_from(
            &marker,
            &s.db.reader,
            &format!("{} in {application}", user.id),
        );
    }
    // A write through query fails for a User with write in an Application
    // declaring write, too.
    let before = s.db.fingerprint();
    for sql in [
        "insert into notes (body) values ('through query')",
        "update incidents set open = false",
        "delete from kinds",
        "with d as (delete from incidents returning id) select count(*) from d",
    ] {
        let (_, answer) = s.one(&both, "board", &op("query", &sql_on(RESOURCE, sql)));
        assert!(
            answer.get("ok").is_none(),
            "expected {sql:?} through query to fail for a User with write, got {answer}"
        );
    }
    assert_eq!(
        s.db.fingerprint(),
        before,
        "expected nothing written through query"
    );
}
