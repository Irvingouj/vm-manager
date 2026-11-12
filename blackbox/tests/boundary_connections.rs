//! Each Access level has its own connections, logged in directly as that
//! level's login, and connections are never shared between levels or
//! Resources (CONTRACT.md, "The login is the boundary").
//!
//! Three Resources: `devops` over one shared database with its read and
//! write logins, `loose` over the same database with the careless login at
//! both levels, and `other` over a second shared database. `query` and
//! `execute` traffic goes to all three at once, every statement carrying a
//! marker for its Resource and operation. Then each database's own server
//! log shows that every marked statement came from the login it should
//! have, and the answers say the same.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{READERS, RESOURCE, Scenario, WRITERS, op, resource_body_with_write};
use blackbox::shared_database::{Login, SharedDatabase, token};
use serde_json::{Value, json};

#[test]
fn a_connection_of_one_level_or_one_resource_never_serves_another() {
    let s = Scenario::start();
    let other = SharedDatabase::create();
    s.register_writable(RESOURCE);
    s.register_body(
        "loose",
        &resource_body_with_write("loose", &s.db, &s.db.careless, &s.db.careless),
    );
    s.register_body(
        "other",
        &resource_body_with_write("other", &other, &other.reader, &other.writer),
    );
    for resource in [RESOURCE, "loose", "other"] {
        s.grant(resource, WRITERS, "write");
    }
    s.install(
        "board",
        "operations.js",
        &[(RESOURCE, "write"), ("loose", "write"), ("other", "write")],
    );
    let user = User::new("wanda").in_groups(&[WRITERS, READERS]);
    let marker = token();
    // (Resource, database, operation, the login it must come from)
    let lanes: [(&str, &SharedDatabase, &str, &Login); 6] = [
        (RESOURCE, &s.db, "query", &s.db.reader),
        (RESOURCE, &s.db, "execute", &s.db.writer),
        ("loose", &s.db, "query", &s.db.careless),
        ("loose", &s.db, "execute", &s.db.careless),
        ("other", &other, "query", &other.reader),
        ("other", &other, "execute", &other.writer),
    ];
    let call = |resource: &str, operation: &str, round: usize| -> Value {
        let tag = format!("lane-{marker}-{resource}-{operation}-{round}");
        let sql = if operation == "execute" {
            format!(
                "insert into notes (body) select '{tag}' from pg_sleep(0.05) returning session_user::text as login"
            )
        } else {
            format!("select session_user::text as login, '{tag}' as tag from pg_sleep(0.05)")
        };
        op(operation, &json!({ "resource": resource, "sql": sql }))
    };
    let bodies: Vec<(User, &str, Value)> = (0..4)
        .map(|round| {
            let calls: Vec<Value> = lanes
                .iter()
                .cycle()
                .skip(round)
                .take(12)
                .map(|(resource, _, operation, _)| call(resource, operation, round))
                .collect();
            (
                user.clone(),
                "board",
                json!({ "calls": calls, "mode": "together" }),
            )
        })
        .collect();
    let results = s.at_once(&bodies);
    for (round, (_, answers)) in results.iter().enumerate() {
        let lanes_of_round = lanes.iter().cycle().skip(round).take(12);
        for ((resource, _, operation, login), answer) in lanes_of_round.zip(answers) {
            assert_eq!(
                answer["ok"]["rows"][0][0].as_str(),
                Some(login.role.as_str()),
                "expected {operation} on {resource} to run as {}, got {answer}",
                login.role
            );
        }
    }
    for (resource, database, operation, login) in lanes {
        for round in 0..4 {
            let tag = format!("lane-{marker}-{resource}-{operation}-{round}");
            if database.received(&tag) {
                database.assert_marker_came_from(
                    &tag,
                    login,
                    &format!("{operation} on {resource}"),
                );
            }
        }
        database.assert_received(
            &format!("lane-{marker}-{resource}-{operation}-"),
            &format!("{operation} on {resource}"),
        );
    }
    // Nothing of one database's traffic reached the other.
    other.assert_not_received(&format!("lane-{marker}-{RESOURCE}-"), "devops's traffic");
    other.assert_not_received(&format!("lane-{marker}-loose-"), "loose's traffic");
    s.db.assert_not_received(&format!("lane-{marker}-other-"), "other's traffic");
}
