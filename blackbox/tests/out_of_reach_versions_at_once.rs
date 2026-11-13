//! At most 8 Versions applied at once (CONTRACT.md, "The migration role's
//! password": "At most 8 Versions are being applied at once in the
//! program, whoever sent them and whether or not their senders still wait.
//! One more is answered `503 unavailable` at once and nothing of it is
//! kept; it can be sent again").
//!
//! - Nine Applications, each with its own database made by a first
//!   Version, are sent a second Version at once whose migration sleeps 4 s:
//!   at least one is answered `503` `unavailable` within 2 s, nothing of it
//!   is kept, the others are `201`, and the refused one applies when sent
//!   again;
//! - eight slow Versions whose senders gave up after 1 s, still being
//!   applied, and a ninth sent then: `503` `unavailable` within 2 s.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::time::{Duration, Instant};

use blackbox::http::{Request, Response};
use blackbox::out_of_reach::{base, base_and, start};
use blackbox::own_database::{
    FIXTURE, Migration, Own, OwnDatabase, literal, version_body_with_migrations,
};
use blackbox::postgres::shared;
use blackbox::sql::Sql;
use blackbox::vm::fixture_bundle;

/// "At once": well before the slow migrations end.
const AT_ONCE: Duration = Duration::from_secs(2);

/// A migration that makes `slow_made` and sleeps `seconds`.
fn slow(seconds: u32) -> Migration {
    Migration::new(
        "0004_slow",
        &[
            r#"CREATE TABLE "slow_made" ("id" integer PRIMARY KEY NOT NULL);"#,
            &format!("SELECT pg_sleep({seconds});"),
        ],
    )
}

fn version_request(
    own: &Own,
    application: &str,
    migrations: &[Migration],
    timeout: Duration,
) -> Request {
    own.api()
        .request(&format!("/v1/applications/{application}/versions"))
        .method("POST")
        .json(&version_body_with_migrations(
            &fixture_bundle(FIXTURE),
            &[],
            migrations,
        ))
        .timeout(timeout)
}

fn has_slow_made(db: &OwnDatabase) -> bool {
    db.column("SELECT count(*)::text FROM pg_class WHERE relname = 'slow_made'")
        == vec!["1".to_owned()]
}

/// Fails unless `sent` is `503` `unavailable`, answered within
/// [`AT_ONCE`], and nothing of it is kept.
fn assert_unavailable(own: &Own, application: &str, sent: &Response, took: Duration) {
    assert_eq!(
        (sent.status, sent.json()["error"]["reason"].as_str()),
        (503, Some("unavailable")),
        "expected `503 unavailable`, got\n{}",
        sent.describe()
    );
    assert!(
        took <= AT_ONCE,
        "expected `503 unavailable` at once (within {AT_ONCE:?}), it took {took:?}"
    );
    assert_eq!(
        own.version_numbers(application),
        vec![1],
        "expected nothing of the refused Version kept"
    );
    assert!(
        !has_slow_made(&own.own_database(application)),
        "expected nothing of the refused Version kept: its table is there"
    );
}

/// How many of these own databases run a `pg_sleep`.
fn sleeping(databases: &[String]) -> usize {
    let sql = Sql::connect(&shared().url("postgres")).unwrap();
    let names: Vec<String> = databases.iter().map(|d| literal(d)).collect();
    sql.value(&format!(
        "SELECT count(DISTINCT datname)::text FROM pg_stat_activity
          WHERE datname IN ({}) AND query LIKE '%pg_sleep%' AND state = 'active'",
        names.join(", ")
    ))
    .and_then(|n| n.parse().ok())
    .unwrap_or(0)
}

#[test]
fn of_nine_versions_sent_at_once_one_is_answered_unavailable_at_once_and_applies_when_sent_again() {
    let own = start();
    let applications: Vec<String> = (0..9).map(|i| format!("at-once-{i}")).collect();
    for application in &applications {
        own.install(application, &base());
    }
    let list = base_and(&[slow(4)]);
    let requests: Vec<Request> = applications
        .iter()
        .map(|a| version_request(&own, a, &list, Duration::from_secs(60)))
        .collect();
    let barrier = Barrier::new(requests.len());
    let answers: Vec<(Response, Duration)> = std::thread::scope(|scope| {
        let handles: Vec<_> = requests
            .iter()
            .map(|r| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let sent_at = Instant::now();
                    let sent = r.send();
                    (sent, sent_at.elapsed())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let refused: Vec<usize> = answers
        .iter()
        .enumerate()
        .filter(|(_, (a, _))| a.status != 201)
        .map(|(i, _)| i)
        .collect();
    let statuses: Vec<(u16, Duration)> = answers.iter().map(|(a, t)| (a.status, *t)).collect();
    assert!(
        !refused.is_empty(),
        "expected at least one of nine Versions sent at once answered `503 unavailable` (CONTRACT.md: \"At most 8 Versions are being applied at once\"); all were `201`: {statuses:?}"
    );
    for &i in &refused {
        let (sent, took) = &answers[i];
        assert_unavailable(&own, &applications[i], sent, *took);
    }
    for (i, (sent, _)) in answers.iter().enumerate() {
        if !refused.contains(&i) {
            assert!(has_slow_made(&own.own_database(&applications[i])));
            assert_eq!(own.version_numbers(&applications[i]), vec![1, 2]);
            assert_eq!(sent.status, 201);
        }
    }
    for &i in &refused {
        let sent = own.send(&applications[i], &list);
        assert_eq!(
            sent.status,
            201,
            "expected the refused Version applied when sent again, got\n{}",
            sent.describe()
        );
    }
}

#[test]
fn versions_whose_senders_went_away_count_toward_the_eight() {
    let own = start();
    let gone: Vec<String> = (0..8).map(|i| format!("gone-{i}")).collect();
    let databases: Vec<String> = gone.iter().map(|a| own.install(a, &base()).name).collect();
    let ninth = "ninth";
    own.install(ninth, &base());
    let list = base_and(&[slow(8)]);
    let requests: Vec<Request> = gone
        .iter()
        .map(|a| version_request(&own, a, &list, Duration::from_secs(1)))
        .collect();
    let barrier = Barrier::new(requests.len());
    let given_up: Vec<bool> = std::thread::scope(|scope| {
        let handles: Vec<_> = requests
            .iter()
            .map(|r| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    r.try_send().is_err()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(
        given_up.iter().all(|g| *g),
        "the harness's own check: expected the eight senders to give up before their answers: {given_up:?}"
    );
    let still = sleeping(&databases);
    let sent_at = Instant::now();
    let sent = version_request(&own, ninth, &list, Duration::from_secs(60)).send();
    let took = sent_at.elapsed();
    assert_eq!(
        still, 8,
        "the harness's own check: expected the eight Versions whose senders went away still being applied when the ninth was sent"
    );
    assert_unavailable(&own, ninth, &sent, took);
    // Once they end, the ninth applies.
    let deadline = Instant::now() + Duration::from_secs(20);
    while sleeping(&databases) > 0 {
        assert!(
            Instant::now() < deadline,
            "the harness's own check: expected the eight slow Versions ended within 20 s"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let again = own.send(ninth, &list);
    assert_eq!(
        again.status,
        201,
        "expected the ninth Version applied when sent again, got\n{}",
        again.describe()
    );
}
