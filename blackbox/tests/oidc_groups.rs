//! The Groups claim (CONTRACT.md, "Sign-in and sessions: Starting with
//! OIDC", `--oidc-groups-claim`): an array of strings or one string; absent
//! or of another shape, no Groups; entries trimmed, empty ones dropped,
//! repeats kept once, the order kept; more than 1,000 Groups, or one longer
//! than 256 bytes, and the sign-in fails.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::oidc::{Person, Provider};
use blackbox::sessions::{Installation, Link, SESSION_COOKIE, assert_failure_page, oidc_options};
use serde_json::{Value, json};

fn person(sub: &str, claim: Option<Value>) -> Person {
    Person::new(sub).named(sub).groups_claim(claim)
}

fn many(count: usize, bytes: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|i| {
                let prefix = format!("g{i:04}-");
                Value::String(format!("{prefix}{}", "x".repeat(bytes - prefix.len())))
            })
            .collect(),
    )
}

#[test]
fn the_groups_claim_in_each_of_its_shapes() {
    let cases: Vec<(&str, Option<Value>, Value)> = vec![
        (
            "array",
            Some(json!([" finance ", "", "devops", "finance", "  ", "audit"])),
            json!(["finance", "devops", "audit"]),
        ),
        ("one-string", Some(json!("finance")), json!(["finance"])),
        (
            "one-string-padded",
            Some(json!("  finance ")),
            json!(["finance"]),
        ),
        ("absent", None, json!([])),
        ("a-number", Some(json!(5)), json!([])),
        ("an-object", Some(json!({ "finance": true })), json!([])),
        ("null", Some(Value::Null), json!([])),
        ("empty-array", Some(json!([])), json!([])),
    ];
    let people: Vec<Person> = cases
        .iter()
        .map(|(sub, claim, _)| person(sub, claim.clone()))
        .collect();
    let at = Installation::start_with(&[("honest", "honest.js")], &people, &[]);
    for (sub, claim, want) in &cases {
        let mut browser = at.browser_as(sub);
        at.sign_in(&mut browser, "honest", "/");
        assert_eq!(
            at.user_seen(&mut browser, "honest"),
            json!({ "id": sub, "groups": want }),
            "the Groups claim {claim:?}"
        );
    }
}

#[test]
fn a_thousand_groups_of_256_bytes_are_taken() {
    let groups = many(1000, 256);
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[person("many", Some(groups.clone()))],
        &[],
    );
    let mut browser = at.browser_as("many");
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["groups"], groups);
}

#[test]
fn more_than_a_thousand_groups_or_one_longer_than_256_bytes_fails_the_sign_in() {
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[
            person("too-many", Some(many(1001, 16))),
            person("too-long", Some(json!(["finance", "y".repeat(257)]))),
        ],
        &[],
    );
    for sub in ["too-many", "too-long"] {
        let mut browser = at.browser_as(sub);
        let steps = at.walk_to_callback(&mut browser, "honest", "/");
        let callback = browser.follow(steps.last().unwrap());
        assert_failure_page(
            &callback.response,
            &[400],
            Link::To(&at.app_origin("honest")),
            &at.provider.issued(),
            sub,
        );
        assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
        at.assert_no_session(&mut browser, "honest", sub);
    }
}

#[test]
fn the_groups_are_read_from_the_claim_named_by_the_connection_s_groups_claim() {
    let at = Installation::start_connected(
        &[("honest", "honest.js")],
        &[person("ada", Some(json!(["finance", "devops"])))],
        &[],
        |c| c.groups_claim("roles"),
    );
    at.provider.groups_claim_name("roles");
    let mut browser = at.browser_as("ada");
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        json!(["finance", "devops"])
    );
}

#[test]
fn groups_under_another_claim_than_the_one_read_are_no_groups() {
    let provider = Provider::start(&[person("ada", Some(json!(["finance"])))]);
    provider.groups_claim_name("roles");
    let options = oidc_options().application("honest", "honest.js");
    let at = Installation::from(provider, &options);
    let mut browser = at.browser_as("ada");
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["groups"], json!([]));
}

#[test]
fn the_thousand_are_counted_once_repeats_are_dropped() {
    let mut groups = many(1000, 32);
    if let Value::Array(entries) = &mut groups {
        let repeat = entries[0].clone();
        entries.push(repeat);
    }
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[person("repeats", Some(groups))],
        &[],
    );
    let mut browser = at.browser_as("repeats");
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        many(1000, 32)
    );
}
