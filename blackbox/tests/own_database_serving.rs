//! Serving waits for the migration (CONTRACT.md, "The Application's own
//! database: What happens when a Version is sent": the pending migrations
//! are applied, and only then is the Version recorded, made the serving
//! one, and the `201` answered; "Versions": once the `201` has been
//! received, every new request is served by the new Version).
//!
//! A migration that sleeps is seen running in Postgres (the harness's own
//! view of `pg_stat_activity` in the own database); meanwhile requests are
//! served by the previous Version, whose tables are as they were. Once the
//! `201` is received, every new request sees the new Version and the new
//! structure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::own_database::{Migration, Own, own_query, standard};
use blackbox::scenario::op;
use serde_json::{Value, json};

/// How long the slow migration sleeps.
const SLOW_SECONDS: u32 = 4;

/// The text the slow migration's sleeping statement carries, to find it in
/// `pg_stat_activity`.
const SLOW_MARKER: &str = "slow-migration-marker";

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

/// whoami and a read of the new table, in one request: which Version
/// served it and whether it sees the new structure.
fn probe() -> Vec<Value> {
    vec![
        op("whoami", &json!({})),
        own_query("user", "select count(*)::int4 as n from slowly_made"),
    ]
}

#[test]
fn while_a_slow_migration_runs_the_previous_version_serves_and_after_the_201_the_new_one() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let mut list = standard();
    list.push(Migration::new(
        "0002_slow",
        &[
            r#"CREATE TABLE "slowly_made" ("x" integer);"#,
            &format!("SELECT pg_sleep({SLOW_SECONDS}) /* {SLOW_MARKER} */;"),
            r#"INSERT INTO "slowly_made" VALUES (1);"#,
        ],
    ));
    thread::scope(|scope| {
        let sending = scope.spawn(|| {
            let began = Instant::now();
            let sent = own.send("board", &list);
            (sent, began.elapsed())
        });
        // Wait for the migration to be seen sleeping in the own database.
        let deadline = Instant::now() + Duration::from_secs(10);
        let running = loop {
            let found = db.column(&format!(
                "SELECT count(*) FROM pg_stat_activity
                  WHERE datname = current_database() AND state = 'active'
                    AND query LIKE '%{SLOW_MARKER}%' AND pid <> pg_backend_pid()"
            ));
            if found == vec!["1"] {
                break true;
            }
            if Instant::now() >= deadline || sending.is_finished() {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(
            running,
            "expected the slow migration to be running in the own database {} after the Version was sent (its statement carries {SLOW_MARKER:?})",
            db.name
        );
        // Meanwhile: Version 1 serves, and the new table is not there for it.
        for round in 0..3 {
            let (_, answers) = own.run(&ann(), "board", &probe());
            assert_eq!(
                answers[0]["ok"]["version"],
                json!(1),
                "round {round}: expected Version 1 to serve while the migration runs, got {}",
                answers[0]
            );
            assert_eq!(
                answers[1]["error"]["kind"],
                json!("database"),
                "round {round}: expected the table the running migration makes to be invisible to Version 1, got {}",
                answers[1]
            );
            assert_eq!(
                answers[1]["error"]["code"],
                json!("42P01"),
                "{}",
                answers[1]
            );
            assert!(
                !sending.is_finished(),
                "the migration ended too soon to check this; round {round}"
            );
            thread::sleep(Duration::from_millis(200));
        }
        let (sent, took) = sending.join().unwrap();
        assert_eq!(
            sent.status,
            201,
            "expected `201` for the Version with the slow migration, got\n{}",
            sent.describe()
        );
        assert!(
            took >= Duration::from_secs(u64::from(SLOW_SECONDS)),
            "expected the `201` only once the migration had run ({SLOW_SECONDS} s), got it after {took:?}"
        );
        // After the 201: every new request sees Version 2 and its table.
        for round in 0..5 {
            let (_, answers) = own.run(&ann(), "board", &probe());
            assert_eq!(
                answers[0]["ok"]["version"],
                json!(2),
                "round {round}: expected Version 2 to serve once its 201 was received, got {}",
                answers[0]
            );
            assert_eq!(
                answers[1]["ok"]["rows"],
                json!([[1]]),
                "round {round}: expected the new table, filled, once the 201 was received, got {}",
                answers[1]
            );
        }
    });
}
