//! Revoking a Grant (CONTRACT.md, "Resources and Grants in full: Revoking a
//! Grant"): `DELETE /v1/resources/<name>/grants/<group>` answers `changed`
//! and `was`; a Group that held none is `200` with `changed` false; no such
//! Resource is `404` before the Group is looked at; a Group that is none is
//! `400`; a Group with a `/`, a space, a `+`, a `%` or non-ASCII letters is
//! revoked by its percent-encoded name and no other Group is touched; the
//! Group's Grants on other Resources and other Groups' Grants on this one
//! stay; a Grant given again after a revoke is a new Grant; a Storage
//! Resource's Grant is revoked the same; pulls and `your_access` follow; the
//! record `grant-revoked`.
//!
//! What a revoke does at once, in flight, and to a transaction is in
//! `grant_revoke_at_once`; who may ask is in `grant_revoke_refused`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::api::assert_recent_rfc3339;
use blackbox::resources::{
    admin_resource, grants_of_group, member_names, records_waited, revoke, revoke_raw,
    without_who_and_when,
};
use blackbox::scenario::{ADMINISTRATORS, RESOURCE, Scenario, assert_refusal};
use blackbox::storage::{
    FILE_READERS, FILES_CONTACT, MARKETING, Storage, VIEWER, assert_ok_null, get,
};
use blackbox::structure::pull;
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

fn other_administrator() -> User {
    User::new("other-admin").in_groups(&[ADMINISTRATORS])
}

/// The groups of the Grants on `resource`, by Group.
fn held(s: &Scenario, resource: &str) -> Vec<(String, String)> {
    let answer = s.admin().list_grants(resource);
    answer.expect_status(200);
    answer.json()["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["group"].as_str().unwrap().to_owned(),
                g["access"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(group: &str, access: &str) -> (String, String) {
    (group.to_owned(), access.to_owned())
}

#[test]
fn a_revoke_answers_changed_and_what_the_group_held_and_touches_nothing_else() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.register("second");
    s.grant(RESOURCE, "crew", "write");
    s.grant(RESOURCE, "ops", "read");
    s.grant("second", "crew", "read");
    s.grant("second", "ops", "write");
    let before_second = s.admin().list_grants("second").json();
    let ops_before = s.admin().list_grants(RESOURCE).json()["grants"][1].clone();

    let answer = revoke(&s.admin(), RESOURCE, "crew");
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({ "resource": RESOURCE, "group": "crew", "changed": true, "was": "write" })
    );
    assert_eq!(
        member_names(&answer.json()),
        ["changed", "group", "resource", "was"]
    );
    assert_eq!(
        held(&s, RESOURCE),
        [pair("ops", "read")],
        "the Grant is in no listing"
    );
    assert_eq!(
        s.admin().list_grants(RESOURCE).json()["grants"][0],
        ops_before,
        "another Group's Grant on this Resource is as it was, who set it and when too"
    );
    assert_eq!(
        s.admin().list_grants("second").json(),
        before_second,
        "the Group's Grant on another Resource is as it was"
    );
    let admin_one = admin_resource(&s.admin(), RESOURCE).json();
    assert_eq!(
        without_who_and_when(&admin_one["grants"]),
        json!([{ "group": "ops", "access": "read" }])
    );
    let by_group = grants_of_group(&s.admin(), "crew").json();
    assert_eq!(by_group["grants"].as_array().unwrap().len(), 1);
    assert_eq!(by_group["grants"][0]["resource"], json!("second"));
}

#[test]
fn a_group_that_held_none_is_200_with_changed_false_and_was_null_and_so_is_asking_again() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "read");
    let none = json!({ "resource": RESOURCE, "group": "nobody", "changed": false, "was": null });
    let first = revoke(&s.admin(), RESOURCE, "nobody");
    first.expect_status(200);
    assert_eq!(first.json(), none);
    assert_eq!(held(&s, RESOURCE), [pair("ops", "read")]);
    // Revoked, and asked again: safe.
    revoke(&s.admin(), RESOURCE, "ops").expect_status(200);
    let again = revoke(&s.admin(), RESOURCE, "ops");
    again.expect_status(200);
    assert_eq!(
        again.json(),
        json!({ "resource": RESOURCE, "group": "ops", "changed": false, "was": null })
    );
    assert_eq!(held(&s, RESOURCE), []);
}

#[test]
fn no_such_resource_is_404_before_the_group_is_looked_at() {
    let s = Scenario::start();
    s.register(RESOURCE);
    let admin = s.admin();
    let long = "g".repeat(300);
    for (resource, segment) in [
        ("ghost", "ops".to_owned()),
        ("ghost", long),
        ("ghost", "%FF".to_owned()),
        ("ghost", "%01".to_owned()),
        ("Not_A_Name", "ops".to_owned()),
    ] {
        assert_api_error(&revoke_raw(&admin, resource, &segment), 404, "not-found");
    }
}

#[test]
fn a_group_that_is_none_is_400_malformed_and_nothing_changes() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "read");
    let admin = s.admin();
    let over = "g".repeat(257);
    let over_multibyte = "%C3%A9".repeat(129);
    for (what, segment) in [
        ("over 256 bytes", over.as_str()),
        (
            "over 256 bytes, of two-byte letters",
            over_multibyte.as_str(),
        ),
        ("a control character (U+0001)", "a%01b"),
        ("a control character (U+001F)", "a%1Fb"),
        ("a NUL", "a%00b"),
        ("a line end", "a%0Ab"),
        ("a byte that is not UTF-8", "%FF"),
        ("a truncated sequence", "ab%C3"),
        ("an overlong sequence", "%C0%AF"),
        ("a continuation byte alone", "%80"),
    ] {
        let answer = revoke_raw(&admin, RESOURCE, segment);
        assert_eq!(
            answer.status,
            400,
            "expected `400` malformed for a Group with {what}, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    assert_eq!(held(&s, RESOURCE), [pair("ops", "read")]);
}

#[test]
fn a_group_of_256_bytes_is_taken_and_compared_exactly() {
    let s = Scenario::start();
    s.register(RESOURCE);
    let long = "g".repeat(256);
    let wide = "é".repeat(128);
    for group in [&long, &wide] {
        s.grant(RESOURCE, group, "read");
        let answer = revoke(&s.admin(), RESOURCE, group);
        answer.expect_status(200);
        assert_eq!(
            answer.json()["changed"],
            json!(true),
            "{}",
            answer.describe()
        );
        assert_eq!(answer.json()["was"], json!("read"));
    }
    s.grant(RESOURCE, "Ops", "read");
    let other_case = revoke(&s.admin(), RESOURCE, "ops");
    assert_eq!(
        other_case.json()["changed"],
        json!(false),
        "compared exactly"
    );
    assert_eq!(held(&s, RESOURCE), [pair("Ops", "read")]);
}

#[test]
fn a_group_with_slash_space_plus_percent_or_non_ascii_letters_is_revoked_by_its_encoded_name_alone()
{
    let s = Scenario::start();
    s.register(RESOURCE);
    let groups = [
        "ops/team",
        "ops",
        "ops team",
        "ops+team",
        "100%",
        "100",
        "équipe",
        "ops/team/x",
        "a%2Fb",
        "a/b",
        " lead ",
        "日本語",
    ];
    for group in groups {
        s.grant(RESOURCE, group, "read");
    }
    let mut remaining: Vec<String> = groups.iter().map(|g| (*g).to_owned()).collect();
    remaining.sort();
    for group in groups {
        let answer = revoke(&s.admin(), RESOURCE, group);
        answer.expect_status(200);
        assert_eq!(
            answer.json(),
            json!({ "resource": RESOURCE, "group": group, "changed": true, "was": "read" }),
            "revoking {group:?} by its percent-encoded name"
        );
        remaining.retain(|g| g != group);
        let now: Vec<String> = held(&s, RESOURCE).into_iter().map(|(g, _)| g).collect();
        assert_eq!(
            now, remaining,
            "after revoking {group:?}, no other Group is touched"
        );
    }
}

#[test]
fn a_plus_in_the_path_is_a_plus_and_not_a_space() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops team", "read");
    s.grant(RESOURCE, "ops+team", "write");
    let answer = revoke_raw(&s.admin(), RESOURCE, "ops+team");
    answer.expect_status(200);
    assert_eq!(answer.json()["group"], json!("ops+team"));
    assert_eq!(answer.json()["was"], json!("write"));
    assert_eq!(held(&s, RESOURCE), [pair("ops team", "read")]);
}

#[test]
fn a_grant_given_again_after_a_revoke_is_a_new_grant() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "read");
    let first = s.admin().list_grants(RESOURCE).json()["grants"][0].clone();
    thread::sleep(Duration::from_millis(1100));
    revoke(&s.admin(), RESOURCE, "ops").expect_status(200);
    let again =
        s.vm.api_as(&other_administrator())
            .grant(RESOURCE, "ops", "read");
    again.expect_status(200);
    assert_eq!(
        again.json(),
        json!({ "resource": RESOURCE, "group": "ops", "access": "read", "changed": true, "was": null }),
        "a Grant given again after a revoke has no earlier level"
    );
    let now = s.admin().list_grants(RESOURCE).json()["grants"][0].clone();
    assert_eq!(now["granted_by"], json!(other_administrator().id), "{now}");
    assert_ne!(
        now["granted_at"], first["granted_at"],
        "a new time: {now} {first}"
    );
    assert_recent_rfc3339(&now["granted_at"], 60, "granted_at");
}

#[test]
fn a_storage_resources_grant_is_revoked_the_same() {
    let s = Storage::ready();
    let reader = User::new("rena").in_groups(&[FILE_READERS]);
    let (_, before) = s.one(&reader, VIEWER, &get(MARKETING, "none.txt"));
    assert_ok_null(&before, "a reader before the revoke");
    let answer = revoke(&s.admin(), MARKETING, FILE_READERS);
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({ "resource": MARKETING, "group": FILE_READERS, "changed": true, "was": "read" })
    );
    let (_, after) = s.one(&reader, VIEWER, &get(MARKETING, "none.txt"));
    assert_refusal(
        &after,
        "no-grant",
        &json!({ "resource": MARKETING, "user": "rena", "needed": "read", "held": null, "contact": FILES_CONTACT }),
        "a get after its Group's Grant was revoked",
    );
    let again = revoke(&s.admin(), MARKETING, FILE_READERS);
    assert_eq!(again.json()["changed"], json!(false));
    assert_eq!(again.json()["was"], Value::Null);
}

#[test]
fn a_pull_and_your_access_follow_a_revoke() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "wr", "write");
    s.grant(RESOURCE, "rd", "read");
    s.grant(RESOURCE, "st", "structure");
    let both = User::new("both").in_groups(&["wr", "rd"]);
    let puller = User::new("puller").in_groups(&["st"]);
    let api = s.vm.api_as(&both);
    let access = |api: &blackbox::api::Api<'_>| -> (Value, Value) {
        let one = api.get_resource(RESOURCE).json()["your_access"].clone();
        let listed = api.list_resources().json()["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == RESOURCE)
            .unwrap()["your_access"]
            .clone();
        (one, listed)
    };
    assert_eq!(access(&api), (json!("write"), json!("write")));
    pull(&s.vm.api_as(&puller), RESOURCE).expect_status(200);

    revoke(&s.admin(), RESOURCE, "wr").expect_status(200);
    assert_eq!(
        access(&api),
        (json!("read"), json!("read")),
        "the highest that is left"
    );
    revoke(&s.admin(), RESOURCE, "rd").expect_status(200);
    assert_eq!(access(&api), (Value::Null, Value::Null), "none is left");

    revoke(&s.admin(), RESOURCE, "st").expect_status(200);
    let refused = pull(&s.vm.api_as(&puller), RESOURCE);
    assert_api_error(&refused, 403, "no-grant");
}

#[test]
fn the_log_has_grant_revoked_when_something_changed_and_nothing_when_nothing_did() {
    let s = Scenario::start();
    s.register(RESOURCE);
    s.grant(RESOURCE, "ops", "write");
    revoke(&s.vm.api_as(&other_administrator()), RESOURCE, "ops").expect_status(200);
    revoke(&s.admin(), RESOURCE, "ops").expect_status(200);
    revoke(&s.admin(), RESOURCE, "never-held").expect_status(200);
    let records = records_waited(&s.vm, "grant-revoked", 1);
    assert_eq!(
        records.len(),
        1,
        "one change, two that changed nothing: {records:?}"
    );
    let record = &records[0];
    assert_eq!(
        member_names(record),
        ["by", "event", "group", "resource", "time_ms", "was"],
        "{record}"
    );
    assert_eq!(record["resource"], json!(RESOURCE));
    assert_eq!(record["group"], json!("ops"));
    assert_eq!(record["was"], json!("write"));
    assert_eq!(record["by"], json!(other_administrator().id));
    assert_eq!(
        records_waited(&s.vm, "grant-revoked", 0).len(),
        1,
        "no record for a revoke that changed nothing"
    );
}
