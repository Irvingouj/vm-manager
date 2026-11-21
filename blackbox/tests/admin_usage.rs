//! Usage: requests and Users (CONTRACT.md, "What Platform administrators
//! see and stop: Usage: requests and Users"): a request is counted when
//! the Application's code was given it (a `500` of its code counts; `401`,
//! `403` not shared, `404` and a disabled Application's answer do not),
//! for the signed-in User whose request it was; `usage` is the last 30 UTC
//! days, the day of the reading and the 29 before it, read from
//! `application_usage`; a counted request shows within 15 seconds.
//!
//! The tests write rows on past days in `application_usage` as the
//! Postgres administrator, which the contract allows.

use std::time::{Duration, Instant};

use blackbox::admin::{
    self, USAGE_PROMISED, disable_ok, place_usage, platform_sql, usage_of, usage_rows, utc_day,
    wait_for_usage,
};
use blackbox::sharing::{TRACED, create_unshared};
use blackbox::{User, Vm, fixture_bundle};

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol")
}

/// Creates `name` as alice with `file` as its first Version, shared with
/// everyone.
fn shared(vm: &Vm, name: &str, file: &str) {
    vm.api_as(&alice())
        .create_with_bundle(name, &fixture_bundle(file));
}

#[test]
fn what_its_code_was_given_counts_for_its_user_and_nothing_refused_before_the_code_does() {
    let vm = admin::start();
    shared(&vm, "counted", TRACED);
    shared(&vm, "fails", "errors.js");
    shared(&vm, "stopped", TRACED);
    create_unshared(&vm, &alice(), "secret", TRACED);
    vm.api_as(&alice())
        .create_application("empty")
        .expect_status(201);
    disable_ok(&vm, "stopped");

    // Counted: bob twice and carol once on `counted`; bob's `500` on
    // `fails`.
    for user in [bob(), bob(), carol()] {
        let r = vm.get_as(&user, "counted", "/hello");
        r.expect_status(200);
    }
    let failed = vm.get_as(&bob(), "fails", "/throw");
    failed.expect_status(500);
    // Not counted: not signed in, not shared, no Version, no such
    // Application, disabled.
    let anonymous = vm.request("counted", "/hello").send();
    anonymous.expect_status(401);
    let not_shared = vm.get_as(&bob(), "secret", "/hello");
    not_shared.expect_status(403);
    // By alice, its Contributor: to bob, with whom it is not shared, it
    // would be the `403` before.
    let no_version = vm.get_as(&alice(), "empty", "/hello");
    no_version.expect_status(404);
    let disabled = vm.get_as(&bob(), "stopped", "/hello");
    disabled.expect_status(503);
    let last_counted = Instant::now();

    let (usage, took) = wait_for_usage(&vm, "counted", 3, 2);
    assert!(
        last_counted.elapsed() <= USAGE_PROMISED + Duration::from_secs(3),
        "expected the counted requests in `usage` within {USAGE_PROMISED:?} of their responses (allowing 3 s for a loaded machine), it took {took:?} of polling"
    );
    let sql = platform_sql(&vm);
    assert_eq!(usage["days"], 30, "{usage}");
    assert_eq!(usage["from"], utc_day(&sql, 29), "{usage}");
    wait_for_usage(&vm, "fails", 1, 1);
    // One more period of writing (at least every 10 s) passes: nothing
    // refused before the code is counted then either.
    std::thread::sleep(Duration::from_secs(11));
    for (name, requests) in [
        ("counted", 3),
        ("fails", 1),
        ("secret", 0),
        ("empty", 0),
        ("stopped", 0),
    ] {
        let usage = usage_of(&vm, name);
        assert_eq!(
            usage["requests"], requests,
            "{name}: expected {requests} counted requests, got {usage}"
        );
    }
    let today = utc_day(&sql, 0);
    assert_eq!(
        usage_rows(&vm, "counted"),
        vec![
            (today.clone(), "bob".to_owned(), 2),
            (today.clone(), "carol".to_owned(), 1),
        ],
        "expected one row per Application, day and User in application_usage"
    );
    assert_eq!(usage_rows(&vm, "fails"), vec![(today, "bob".to_owned(), 1)]);
    assert!(usage_rows(&vm, "stopped").is_empty());
    assert!(usage_rows(&vm, "secret").is_empty());
}

#[test]
fn usage_is_the_last_30_utc_days_by_calendar_day() {
    let vm = admin::start();
    shared(&vm, "quiet", TRACED);
    shared(&vm, "idle", TRACED);
    // In the period: today - 29 and today - 10. Out of it: today - 30 and
    // today - 45.
    place_usage(&vm, "quiet", 29, "u1", 5);
    place_usage(&vm, "quiet", 10, "u2", 7);
    place_usage(&vm, "quiet", 10, "u1", 1);
    place_usage(&vm, "quiet", 30, "u3", 100);
    place_usage(&vm, "quiet", 45, "u1", 50);
    let sql = platform_sql(&vm);
    let usage = usage_of(&vm, "quiet");
    assert_eq!(usage["days"], 30, "{usage}");
    assert_eq!(
        usage["from"],
        utc_day(&sql, 29),
        "expected `from` to be the first of the 30 days: today (UTC) less 29"
    );
    assert_eq!(
        (usage["requests"].as_u64(), usage["users"].as_u64()),
        (Some(13), Some(2)),
        "expected 13 requests by 2 different Users in the 30 days (u3's day 30 and u1's day 45 are outside), got {usage}"
    );
    let idle = usage_of(&vm, "idle");
    assert_eq!(
        (idle["requests"].as_u64(), idle["users"].as_u64()),
        (Some(0), Some(0)),
        "{idle}"
    );
    // A request today adds to what the rows hold.
    vm.get_as(&User::new("u2"), "quiet", "/hello")
        .expect_status(200);
    wait_for_usage(&vm, "quiet", 14, 2);
    vm.get_as(&User::new("u4"), "quiet", "/hello")
        .expect_status(200);
    wait_for_usage(&vm, "quiet", 15, 3);
}

#[test]
fn a_counted_request_shows_within_15_seconds() {
    let vm = admin::start();
    shared(&vm, "board", TRACED);
    let response = vm.get_as(&bob(), "board", "/hello");
    response.expect_status(200);
    let answered = Instant::now();
    let (_, _) = wait_for_usage(&vm, "board", 1, 1);
    let took = answered.elapsed();
    assert!(
        took <= USAGE_PROMISED + Duration::from_secs(3),
        "expected the request in `usage` within {USAGE_PROMISED:?} of its response (3 s allowed for a loaded machine), it took {took:?}"
    );
    let rows = usage_rows(&vm, "board");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "bob");
}
