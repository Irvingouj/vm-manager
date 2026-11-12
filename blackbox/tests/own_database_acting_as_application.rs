//! Acting as the Application is only for the own database (CONTRACT.md,
//! "The Application's own database: Operations on the own database";
//! ticket "An Application's own database", fifth acceptance criterion:
//! "the refusal's reason is 'not the own database'").
//!
//! `as: "application"` with a `resource` is refused with `not-own-database`
//! naming the Resource, whatever is declared and granted, right after
//! `no-request` and before anything about the Resource (also for one not
//! declared or not registered); nothing is sent, and the Operation log
//! entry says so. `as` defaults to `"user"`. Both `own: true` and
//! `resource`, neither, and an unknown `as` are malformed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::expect::assert_refused;
use blackbox::records::{event, request_of};
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, WRITERS, assert_entry, assert_malformed, assert_refusal,
    assert_rows, lines, op,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

/// A User who holds `write` (and `read`) on the Resource.
fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS, READERS])
}

/// A select that carries `marker`, to show it never reached the shared
/// database.
fn marked(marker: &str) -> String {
    format!("select '{marker}' as m")
}

#[test]
fn acting_as_the_application_on_a_resource_is_refused_with_not_own_database_whatever_is_granted() {
    // `board` declares the Resource at write; wanda holds write.
    let s = Scenario::writable();
    for operation in ["query", "execute"] {
        let marker = format!("as-application-{}", token());
        let args = json!({ "resource": RESOURCE, "as": "application", "sql": marked(&marker) });
        let (response, answer) = s.one(&wanda(), "board", &op(operation, &args));
        assert_refusal(
            &answer,
            "not-own-database",
            &json!({ "resource": RESOURCE }),
            &format!("{operation} as the Application on a Resource declared and granted at write"),
        );
        s.db.assert_not_received(&marker, "an operation refused with not-own-database");
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                operation,
                actor: Some("application"),
                own_database: Some(false),
                ..Expected::refused(&id, "wanda", "board", "not-own-database")
            },
            &format!("the entry of a {operation} refused with not-own-database"),
        );
        let records = s.vm.records_of(&response);
        let calls = records.host_calls();
        assert!(
            calls
                .iter()
                .any(|r| r["outcome"] == "refused" && r["reason"] == "not-own-database"),
            "expected a refused `host-call` record with reason not-own-database, got:\n{}",
            blackbox::records::describe(&calls)
        );
    }
    // A control: the same as the User runs.
    let (_, answer) = s.one(
        &wanda(),
        "board",
        &op(
            "query",
            &json!({ "resource": RESOURCE, "as": "user", "sql": "select 1 as one" }),
        ),
    );
    assert_rows(
        &answer,
        &["one"],
        &json!([[1]]),
        "the same query as the User",
    );
}

#[test]
fn not_own_database_comes_before_anything_about_the_resource() {
    let s = Scenario::writable();
    s.install("dreamer", "operations.js", &[("ghost", "read")]);
    s.install("plain", "operations.js", &[]);
    let nobody = User::new("nobody");
    // (who, application, operation, resource, what would refuse otherwise)
    let cases: [(&User, &str, &str, &str, &str); 6] = [
        (&wanda(), "plain", "query", RESOURCE, "not-declared"),
        (&wanda(), "plain", "query", "nothing_here", "not-declared"),
        (&wanda(), "dreamer", "query", "ghost", "not-registered"),
        (&wanda(), "viewer", "execute", RESOURCE, "declared-lower"),
        (&nobody, "board", "query", RESOURCE, "no-grant"),
        (&nobody, "board", "execute", RESOURCE, "no-grant"),
    ];
    for (user, application, operation, resource, otherwise) in cases {
        let marker = format!("before-{otherwise}-{}", token());
        let sql = marked(&marker);
        // The control: as the User it is refused for what it lacks.
        let (_, as_user) = s.one(
            user,
            application,
            &op(operation, &json!({ "resource": resource, "sql": sql })),
        );
        assert_refused(&as_user, otherwise, "the control, as the User");
        let (response, answer) = s.one(
            user,
            application,
            &op(
                operation,
                &json!({ "resource": resource, "as": "application", "sql": sql }),
            ),
        );
        assert_refusal(
            &answer,
            "not-own-database",
            &json!({ "resource": resource }),
            &format!(
                "{operation} as the Application on {resource:?} from {application:?}, which as the User is {otherwise}"
            ),
        );
        s.db.assert_not_received(
            &marker,
            "an operation refused as the User or as the Application",
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                operation,
                resource: Some(resource),
                actor: Some("application"),
                own_database: Some(false),
                ..Expected::refused(&id, &user.id, application, "not-own-database")
            },
            &format!("the entry of {operation} on {resource:?} as the Application"),
        );
    }
}

#[test]
fn as_defaults_to_the_user_and_entries_on_a_resource_say_so() {
    let s = Scenario::writable();
    for args in [
        json!({ "resource": RESOURCE, "sql": "select 1 as one" }),
        json!({ "resource": RESOURCE, "as": "user", "sql": "select 1 as one" }),
    ] {
        let (response, answer) = s.one(&wanda(), "board", &op("query", &args));
        assert_rows(&answer, &["one"], &json!([[1]]), &format!("{args}"));
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                actor: Some("user"),
                own_database: Some(false),
                rows: Some(1),
                ..Expected::allowed(&id, "wanda", "board")
            },
            &format!("the entry of {args} on a Resource"),
        );
    }
    // A refusal on a Resource as the User says so too.
    let (response, _) = s.one(
        &User::new("nobody"),
        "board",
        &op(
            "execute",
            &json!({ "resource": RESOURCE, "sql": "select 1" }),
        ),
    );
    let id = response.request_id();
    assert_entry(
        &s.entry_of(&response),
        &Expected {
            operation: "execute",
            actor: Some("user"),
            own_database: Some(false),
            ..Expected::refused(&id, "nobody", "board", "no-grant")
        },
        "the entry of a refusal on a Resource as the User",
    );
}

/// Arguments whose target or actor is malformed, each carrying `marker` in
/// its SQL.
fn malformed_targets(marker: &str) -> Vec<(&'static str, Value)> {
    let sql = format!("insert into notes (body) values ('{marker}') returning id");
    vec![
        (
            "own: true and a resource",
            json!({ "own": true, "resource": RESOURCE, "sql": sql }),
        ),
        (
            "own: true and a resource, as the Application",
            json!({ "own": true, "resource": RESOURCE, "as": "application", "sql": sql }),
        ),
        ("neither own nor resource", json!({ "sql": sql })),
        (
            "neither, as the Application",
            json!({ "as": "application", "sql": sql }),
        ),
        (
            "own: false and no resource",
            json!({ "own": false, "sql": sql }),
        ),
        (
            "own: \"true\" and no resource",
            json!({ "own": "true", "sql": sql }),
        ),
        ("own: 1 and no resource", json!({ "own": 1, "sql": sql })),
        (
            "as: \"admin\" on the own database",
            json!({ "own": true, "as": "admin", "sql": sql }),
        ),
        (
            "as: \"User\" on the own database",
            json!({ "own": true, "as": "User", "sql": sql }),
        ),
        (
            "as: \"APPLICATION\" on the own database",
            json!({ "own": true, "as": "APPLICATION", "sql": sql }),
        ),
        (
            "as: \"platform\" on a resource",
            json!({ "resource": RESOURCE, "as": "platform", "sql": sql }),
        ),
        (
            "as: 1 on a resource",
            json!({ "resource": RESOURCE, "as": 1, "sql": sql }),
        ),
        (
            "as: true on the own database",
            json!({ "own": true, "as": true, "sql": sql }),
        ),
        (
            "as: [\"user\"] on the own database",
            json!({ "own": true, "as": ["user"], "sql": sql }),
        ),
    ]
}

#[test]
fn a_malformed_target_or_actor_is_malformed_and_nothing_is_sent() {
    let s = Scenario::writable();
    // Each call's SQL would write the marker into `notes` of the shared
    // database, were it sent.
    let marker = format!("malformed-target-{}", token());
    for operation in ["query", "execute"] {
        let cases = malformed_targets(&marker);
        let calls: Vec<Value> = cases.iter().map(|(_, args)| op(operation, args)).collect();
        let (response, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
        for ((what, _), answer) in cases.iter().zip(&answers) {
            assert_malformed(answer, &format!("{operation} with {what}"));
        }
        let entries = s.entries_of(&response);
        assert!(
            entries.is_empty(),
            "expected no Operation log entry for malformed operations, got:\n{}",
            lines(&entries)
        );
        let records = s.vm.records_of(&response);
        let wrong: Vec<&Value> = records
            .host_calls()
            .into_iter()
            .filter(|r| !(r["outcome"] == "refused" && r["reason"] == "malformed"))
            .collect();
        assert!(
            wrong.is_empty(),
            "expected each malformed call recorded as refused with reason malformed, got:\n{}",
            blackbox::records::describe(&wrong)
        );
    }
    s.db.assert_not_received(&marker, "malformed operations");
}

#[test]
fn with_no_request_in_flight_every_actor_and_target_is_refused_with_no_request() {
    let s = Scenario::writable();
    s.install("loader", "own-at-load.js", &[(RESOURCE, "write")]);
    let response = s.vm.get_as(&wanda(), "loader", "/");
    response.expect_status(200);
    let body = response.json();
    let at_load = &body["atLoad"];
    for name in [
        "own_query_as_user",
        "own_query_as_application",
        "own_execute_as_application",
        "resource_as_application",
    ] {
        let answered = &at_load[name]["answered"];
        assert!(
            !answered.is_null(),
            "expected the call {name} at load to resolve with a refusal (not reject or throw), got {body}"
        );
        assert_refused(answered, "no-request", &format!("{name} at the top level"));
    }
    let refused: Vec<Value> =
        s.vm.records()
            .into_iter()
            .filter(|r| {
                event(r) == "host-call"
                    && r["application"] == "loader"
                    && request_of(r).is_none()
                    && r["outcome"] == "refused"
            })
            .collect();
    assert!(
        refused.iter().all(|r| r["reason"] == "no-request") && refused.len() >= 4,
        "expected the calls at load recorded as refused with no-request, got:\n{}",
        lines(&refused)
    );
    s.vm.assert_log_keeps_each_request_to_itself();
}
