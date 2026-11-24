//! Setting a Grant says what it changed (CONTRACT.md, "Resources and
//! Grants in full: Grants in full"): `changed` and `was` in the answer of
//! `POST /v1/resources/<name>/grants`; the same level again writes nothing
//! (who set it and when stay, no record is logged); `granted_by` and
//! `granted_at` in every listing of Grants, null for a Grant from before
//! this stage; two writes at once; the record `grant-set`; at most 1,000
//! Grants on a Resource; the order of refusals; `too-large` past 64 KiB.
//!
//! A Grant "from before this stage" is made by hand: the rows of `grants`
//! have every column that may be NULL set to NULL
//! (`blackbox::resources::forget_who_and_when`) and the program is started
//! again to hold them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::api::assert_recent_rfc3339;
use blackbox::resources::{
    admin_resource, admin_resources, assert_utc_time, fill_grants, forget_who_and_when,
    grants_of_group, member_names, records_waited, revoke,
};
use blackbox::scenario::{ADMINISTRATORS, RESOURCE, Scenario, administrator};
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

fn other_administrator() -> User {
    User::new("other-admin").in_groups(&[ADMINISTRATORS])
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// The Resource's Grants as `GET /v1/resources/<name>/grants` answers.
fn listed(s: &Scenario) -> Value {
    let answer = s.admin().list_grants(RESOURCE);
    answer.expect_status(200);
    answer.json()
}

/// The entry of `group` in `GET /v1/resources/<name>/grants`.
fn entry(s: &Scenario, group: &str) -> Value {
    listed(s)["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["group"] == group)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "expected {group:?} to hold a Grant, the list is {}",
                listed(s)
            )
        })
}

#[test]
fn a_new_grant_says_changed_true_and_was_null() {
    let s = Scenario::start();
    s.register(RESOURCE);
    let answer = s.admin().grant(RESOURCE, "sre", "write");
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({ "resource": RESOURCE, "group": "sre", "access": "write", "changed": true, "was": null }),
        "a Group that held none"
    );
}

#[test]
fn another_level_says_what_the_group_held_before() {
    let s = Scenario::start();
    s.register(RESOURCE);
    let admin = s.admin();
    admin.grant(RESOURCE, "sre", "read").expect_status(200);
    for (access, was) in [
        ("write", "read"),
        ("structure", "write"),
        ("read", "structure"),
    ] {
        let answer = admin.grant(RESOURCE, "sre", access);
        answer.expect_status(200);
        assert_eq!(
            answer.json(),
            json!({ "resource": RESOURCE, "group": "sre", "access": access, "changed": true, "was": was }),
            "{was} to {access}"
        );
    }
}

#[test]
fn the_same_level_again_is_changed_false_and_writes_nothing() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.admin().grant(RESOURCE, "sre", "write").expect_status(200);
    let first = entry(&s, "sre");
    assert_eq!(first["granted_by"], json!(administrator().id), "{first}");
    assert_eq!(records_waited(&s.vm, "grant-set", 1).len(), 1);
    // A second goes by, so that a time written again could not be the same.
    thread::sleep(Duration::from_millis(1100));
    for (who, user) in [
        ("the same", administrator()),
        ("another", other_administrator()),
    ] {
        let answer = s.vm.api_as(&user).grant(RESOURCE, "sre", "write");
        answer.expect_status(200);
        assert_eq!(
            answer.json(),
            json!({ "resource": RESOURCE, "group": "sre", "access": "write", "changed": false, "was": "write" }),
            "the same level again, asked by {who} Platform administrator"
        );
        assert_eq!(
            entry(&s, "sre"),
            first,
            "expected `granted_by` and `granted_at` as they were after the same level again, asked by {who} Platform administrator"
        );
    }
    assert_eq!(
        records_waited(&s.vm, "grant-set", 0).len(),
        1,
        "expected no `grant-set` record for a Grant that did not change"
    );
}

#[test]
fn every_listing_says_who_set_a_grant_and_when() {
    let s = Scenario::start();
    s.register(RESOURCE);
    // Set by two Platform administrators, in an order that is not by Group.
    s.admin().grant(RESOURCE, "zeta", "read").expect_status(200);
    s.vm.api_as(&other_administrator())
        .grant(RESOURCE, "alpha", "structure")
        .expect_status(200);
    s.admin().grant(RESOURCE, "mid", "write").expect_status(200);
    let answer = s.admin().list_grants(RESOURCE);
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(member_names(&body), ["grants", "grants_more"], "{body}");
    assert_eq!(body["grants_more"], json!(0));
    let grants = body["grants"].as_array().unwrap();
    let groups: Vec<&str> = grants
        .iter()
        .map(|g| g["group"].as_str().unwrap())
        .collect();
    assert_eq!(groups, ["alpha", "mid", "zeta"], "by Group in byte order");
    for g in grants {
        assert_eq!(
            member_names(g),
            ["access", "granted_at", "granted_by", "group"],
            "{g}"
        );
        assert_utc_time(&g["granted_at"], &format!("granted_at of {}", g["group"]));
        assert_recent_rfc3339(&g["granted_at"], 60, "granted_at");
    }
    assert_eq!(grants[0]["granted_by"], json!(other_administrator().id));
    assert_eq!(grants[1]["granted_by"], json!(administrator().id));
    assert_eq!(grants[2]["granted_by"], json!(administrator().id));
    // The same four members in each entry of the administrators' listings.
    let one = admin_resource(&s.admin(), RESOURCE).json();
    assert_eq!(
        one["grants"], body["grants"],
        "GET /v1/admin/resources/<name>"
    );
    assert_eq!(one["grants_more"], json!(0));
    let all = admin_resources(&s.admin(), "").json();
    assert_eq!(
        all["resources"][0]["grants"], body["grants"],
        "GET /v1/admin/resources"
    );
    // A change by another administrator is theirs, with a later time.
    thread::sleep(Duration::from_millis(1100));
    s.vm.api_as(&other_administrator())
        .grant(RESOURCE, "zeta", "write")
        .expect_status(200);
    let changed = entry(&s, "zeta");
    assert_eq!(changed["granted_by"], json!(other_administrator().id));
    assert_ne!(changed["granted_at"], grants[2]["granted_at"], "a new time");
    assert_eq!(
        entry(&s, "mid"),
        grants[1],
        "another Group's Grant is as it was"
    );
}

#[test]
fn a_grant_from_before_this_stage_has_neither_who_nor_when_until_it_is_changed() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "old", "read");
    s.grant(RESOURCE, "older", "write");
    forget_who_and_when(&s.vm);
    let Scenario { vm, db } = s;
    let s = Scenario {
        vm: blackbox::storage::terminate(vm).start(),
        db,
    };
    let old = json!({ "group": "old", "access": "read", "granted_by": null, "granted_at": null });
    let older =
        json!({ "group": "older", "access": "write", "granted_by": null, "granted_at": null });
    assert_eq!(listed(&s)["grants"], json!([old, older]), "GET .../grants");
    assert_eq!(
        admin_resource(&s.admin(), RESOURCE).json()["grants"],
        json!([old, older]),
        "GET /v1/admin/resources/<name>"
    );
    assert_eq!(
        admin_resources(&s.admin(), "").json()["resources"][0]["grants"],
        json!([old, older]),
        "GET /v1/admin/resources"
    );
    assert_eq!(
        grants_of_group(&s.admin(), "old").json()["grants"][0]["granted_by"],
        Value::Null,
        "GET /v1/admin/grants"
    );
    // The same level again writes nothing: it stays as it was.
    let again =
        s.vm.api_as(&other_administrator())
            .grant(RESOURCE, "old", "read");
    assert_eq!(
        again.json(),
        json!({ "resource": RESOURCE, "group": "old", "access": "read", "changed": false, "was": "read" })
    );
    assert_eq!(
        entry(&s, "old"),
        old,
        "the same level again leaves it unrecorded"
    );
    // Changed, it is recorded.
    let changed =
        s.vm.api_as(&other_administrator())
            .grant(RESOURCE, "old", "write");
    assert_eq!(changed.json()["was"], json!("read"));
    let now = entry(&s, "old");
    assert_eq!(now["granted_by"], json!(other_administrator().id), "{now}");
    assert_recent_rfc3339(&now["granted_at"], 60, "granted_at");
    assert_eq!(
        entry(&s, "older"),
        older,
        "the other old Grant is as it was"
    );
}

#[test]
fn two_writes_at_once_of_different_levels_answer_one_the_others_level_as_was() {
    let s = Scenario::start();
    s.register(RESOURCE);
    for round in 0..8 {
        let group = format!("race{round}");
        let barrier = Barrier::new(2);
        let answers: Vec<(&str, Value)> = thread::scope(|scope| {
            let handles: Vec<_> = ["read", "write"]
                .into_iter()
                .map(|access| {
                    let (s, group, barrier) = (&s, &group, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        let answer = s.admin().grant(RESOURCE, group, access);
                        answer.expect_status(200);
                        (access, answer.json())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let firsts: Vec<_> = answers.iter().filter(|(_, a)| a["was"].is_null()).collect();
        let seconds: Vec<_> = answers
            .iter()
            .filter(|(_, a)| !a["was"].is_null())
            .collect();
        assert_eq!(
            (firsts.len(), seconds.len()),
            (1, 1),
            "round {round}: expected one write to find no Grant and the other the first's level, got {answers:?}"
        );
        let (first, _) = firsts[0];
        let (second, second_answer) = seconds[0];
        assert_eq!(
            second_answer["was"],
            json!(first),
            "round {round}: {answers:?}"
        );
        assert_eq!(second_answer["changed"], json!(true));
        assert_eq!(
            entry(&s, &group)["access"],
            json!(second),
            "round {round}: the Grant in force is the one written last"
        );
    }
}

#[test]
fn the_log_has_grant_set_with_was_and_by_when_something_changed_and_only_then() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.admin().grant(RESOURCE, "sre", "read").expect_status(200);
    s.vm.api_as(&other_administrator())
        .grant(RESOURCE, "sre", "write")
        .expect_status(200);
    s.admin().grant(RESOURCE, "sre", "write").expect_status(200);
    let records = records_waited(&s.vm, "grant-set", 2);
    assert_eq!(records.len(), 2, "two changes, one repeat: {records:?}");
    let keys = |r: &Value| member_names(r);
    let want = [
        ("read", Value::Null, administrator().id),
        ("write", json!("read"), other_administrator().id),
    ];
    for (record, (access, was, by)) in records.iter().zip(want) {
        assert_eq!(
            keys(record),
            [
                "access", "by", "event", "group", "resource", "time_ms", "was"
            ],
            "{record}"
        );
        assert_eq!(record["resource"], json!(RESOURCE));
        assert_eq!(record["group"], json!("sre"));
        assert_eq!(record["access"], json!(access));
        assert_eq!(record["was"], was);
        assert_eq!(record["by"], json!(by));
    }
}

/// The member of a full Resource's page that says how many it holds.
fn says_how_many(message: &str) -> bool {
    message.contains("1000") || message.contains("1,000")
}

#[test]
fn a_resource_holds_at_most_1000_grants_and_a_group_that_holds_one_is_changed_and_revoked_when_full()
 {
    let s = Scenario::start();
    s.register(RESOURCE);
    let began = Instant::now();
    fill_grants(&s.vm, RESOURCE, 0, 1000, "read");
    eprintln!("1,000 Grants given in {:?}", began.elapsed());
    let admin = s.admin();
    let full = listed(&s);
    assert_eq!(full["grants"].as_array().unwrap().len(), 1000);
    assert_eq!(full["grants_more"], json!(0));

    // A 1,001st, for a Group that holds none.
    let refused = admin.grant(RESOURCE, "overflow", "read");
    assert_api_error(&refused, 409, "too-many");
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(
        says_how_many(&message) && message.contains("revoke"),
        "expected the message to say how many a Resource holds and to revoke one no longer needed, got {message:?}"
    );
    assert_eq!(listed(&s), full, "nothing written");
    assert_eq!(
        grants_of_group(&admin, "overflow").json()["grants"],
        json!([])
    );
    // Order of refusals: not-found, then malformed, then too-many.
    assert_api_error(
        &admin.post(
            "/v1/resources/ghost/grants",
            &json!({ "group": "overflow", "access": "admin" }),
        ),
        404,
        "not-found",
    );
    assert_api_error(
        &admin.grant(RESOURCE, "overflow", "admin"),
        400,
        "malformed",
    );
    assert_api_error(&admin.grant(RESOURCE, "", "read"), 400, "malformed");

    // A Group that holds one is changed, and the same level is not refused.
    let changed = admin.grant(RESOURCE, "group0007", "write");
    changed.expect_status(200);
    assert_eq!(
        changed.json(),
        json!({ "resource": RESOURCE, "group": "group0007", "access": "write", "changed": true, "was": "read" })
    );
    admin
        .grant(RESOURCE, "group0007", "write")
        .expect_status(200);
    assert_eq!(listed(&s)["grants"].as_array().unwrap().len(), 1000);
    // The administrators' listing shows the first 200 and counts the rest.
    let one = admin_resource(&admin, RESOURCE).json();
    assert_eq!(one["grants"].as_array().unwrap().len(), 200);
    assert_eq!(one["grants_more"], json!(800));

    // And revoked; then the list has room for one more, and only one.
    let revoked = revoke(&admin, RESOURCE, "group0007");
    revoked.expect_status(200);
    assert_eq!(revoked.json()["was"], json!("write"));
    assert_eq!(listed(&s)["grants"].as_array().unwrap().len(), 999);
    admin.grant(RESOURCE, "overflow", "read").expect_status(200);
    assert_eq!(listed(&s)["grants"].as_array().unwrap().len(), 1000);
    assert_api_error(
        &admin.grant(RESOURCE, "overflow-2", "read"),
        409,
        "too-many",
    );
}

#[test]
fn past_64_kib_is_too_large_for_everyone_and_at_64_kib_it_is_looked_at() {
    let s = Scenario::start();
    s.register(RESOURCE);
    let path = format!("/v1/resources/{RESOURCE}/grants");
    let body = r#"{"group":"sre","access":"read"}"#;
    let over = format!("{body}{}", " ".repeat(64 * 1024 + 1 - body.len()));
    let at = format!("{body}{}", " ".repeat(64 * 1024 - body.len()));
    assert_eq!(over.len(), 64 * 1024 + 1);
    assert_eq!(at.len(), 64 * 1024);
    for user in [administrator(), alice()] {
        let answer = s.vm.api_as(&user).post_raw(&path, &over);
        assert_api_error(&answer, 413, "too-large");
    }
    assert_eq!(listed(&s)["grants"], json!([]), "nothing written");
    assert_api_error(
        &s.vm.api_as(&alice()).post_raw(&path, &at),
        403,
        "not-an-administrator",
    );
    let accepted = s.vm.api_as(&administrator()).post_raw(&path, &at);
    accepted.expect_status(200);
    assert_eq!(accepted.json()["changed"], json!(true));
}
