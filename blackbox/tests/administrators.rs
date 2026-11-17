//! Who is a Platform administrator with OIDC (CONTRACT.md, "The tool signs
//! in through the browser, and OIDC is connected from the tool: Who is a
//! Platform administrator"): a User whose Groups, as last read from the
//! provider, hold the connection's administrators' Group; taken out or put
//! in at the provider, it changes at the next renewal; a new connection's
//! Group counts at once.
//!
//! Acceptance criteria: "A member of the administrators' Group can run
//! admin commands; anyone else is refused", "Removing someone from the
//! Group ends their admin access at the next session renewal", and the
//! Group named from the tool ("With the setup credential, a Platform
//! administrator connects OIDC and names the administrators' Group from the
//! tool").
//!
//! Only tool credentials are looked at here: a browser's session signs in
//! on the sign-in address and the Applications' addresses, not on the
//! platform's API, so nothing an outsider sees of a session says whether it
//! is an administrator's (its Groups, which decide it, are tested in
//! `oidc_renewal.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::connection::{Connection, shown};
use blackbox::oidc::Person;
use blackbox::scenario::{ADMINISTRATORS, resource_body};
use blackbox::sessions::{ADA, GRACE, Installation, grace};
use blackbox::shared_database::SharedDatabase;
use blackbox::tool::{Machine, bearer};
use blackbox::{Response, assert_api_error};
use serde_json::json;

/// Ada, a Platform administrator: in `finance` and [`ADMINISTRATORS`].
fn ada_administrator() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

fn installation(more: &[&str]) -> Installation {
    Installation::start_with(&[], &[ada_administrator(), grace()], more)
}

fn me(at: &Installation, credential: &str) -> serde_json::Value {
    let answer = bearer(&at.vm, "/v1/me", credential).send();
    assert_eq!(answer.status, 200, "{}", shown(&answer, &[]));
    answer.json()
}

/// `POST /v1/resources` with a Resource over a fresh shared database.
fn register(at: &Installation, credential: &str, db: &SharedDatabase, name: &str) -> Response {
    at.vm
        .api_request("/v1/resources")
        .method("POST")
        .bearer(credential)
        .json(&resource_body(name, db, &db.reader))
        .send()
}

/// Sleeps until `period` and a little more have passed since `since`.
fn wait_past(since: Instant, period: Duration) {
    let until = since + period + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
}

#[test]
fn a_member_of_the_administrators_group_can_run_administrators_requests() {
    let at = installation(&[]);
    let credential = at.tool_credential_for(ADA);
    assert_eq!(me(&at, &credential)["platform_administrator"], true);
    let db = SharedDatabase::create();
    let registered = register(&at, &credential, &db, "devops");
    assert_eq!(registered.status, 201, "{}", shown(&registered, &[]));
    assert_eq!(registered.json()["created_by"], ADA);
    for target in [
        "/v1/oidc",
        "/v1/operation-log",
        "/v1/resources/devops/grants",
    ] {
        let answer = bearer(&at.vm, target, &credential).send();
        assert_eq!(answer.status, 200, "{target}\n{}", shown(&answer, &[]));
    }
    let granted = at
        .vm
        .api_request("/v1/resources/devops/grants")
        .method("POST")
        .bearer(&credential)
        .json(&json!({ "group": "finance", "access": "read" }))
        .send();
    assert_eq!(granted.status, 200, "{}", shown(&granted, &[]));
}

#[test]
fn anyone_else_is_refused_as_not_an_administrator() {
    let at = installation(&[]);
    let credential = at.tool_credential_for(GRACE);
    assert_eq!(me(&at, &credential)["platform_administrator"], false);
    let db = SharedDatabase::create();
    assert_api_error(
        &register(&at, &credential, &db, "devops"),
        403,
        "not-an-administrator",
    );
    for target in [
        "/v1/oidc",
        "/v1/operation-log",
        "/v1/resources/devops/grants",
    ] {
        assert_api_error(
            &bearer(&at.vm, target, &credential).send(),
            403,
            "not-an-administrator",
        );
    }
}

#[test]
fn the_tool_signed_in_through_the_browser_says_who_is_an_administrator() {
    let at = installation(&[]);
    let ada_machine = Machine::new();
    let printed = at.sign_in_tool(&ada_machine, ADA);
    assert_eq!(printed["platform_administrator"], true, "{printed}");
    let who = ada_machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(who["platform_administrator"], true, "{who}");
    let grace_machine = Machine::new();
    let printed = at.sign_in_tool(&grace_machine, GRACE);
    assert_eq!(printed["platform_administrator"], false, "{printed}");
}

#[test]
fn the_administrators_group_is_the_one_named_when_connecting() {
    let at = Installation::start_connected(&[], &[ada_administrator(), grace()], &[], |c| {
        c.administrators_group("devops")
    });
    let ada = at.tool_credential_for(ADA);
    let grace = at.tool_credential_for(GRACE);
    assert_eq!(
        me(&at, &grace)["platform_administrator"],
        true,
        "Grace is in devops, the Group named"
    );
    assert_eq!(
        me(&at, &ada)["platform_administrator"],
        false,
        "Ada is in platform-admins, which was not named"
    );
    assert_eq!(bearer(&at.vm, "/v1/oidc", &grace).send().status, 200);
    assert_api_error(
        &bearer(&at.vm, "/v1/oidc", &ada).send(),
        403,
        "not-an-administrator",
    );
}

#[test]
fn removing_someone_from_the_group_ends_their_administrators_access_at_the_next_renewal() {
    let at = installation(&["--session-renewal", "5"]);
    let began = Instant::now();
    let credential = at.tool_credential_for(ADA);
    let signed_in = Instant::now();
    at.provider.set_groups(ADA, &["finance"]);
    let before = bearer(&at.vm, "/v1/oidc", &credential).send();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the test was too slow to look before the renewal period ended ({:?})",
        began.elapsed()
    );
    assert_eq!(
        before.status,
        200,
        "until the renewal the Groups read at sign-in hold\n{}",
        shown(&before, &[])
    );
    wait_past(signed_in, Duration::from_secs(5));
    assert_api_error(
        &bearer(&at.vm, "/v1/oidc", &credential).send(),
        403,
        "not-an-administrator",
    );
    let after = me(&at, &credential);
    assert_eq!(after["groups"], json!(["finance"]), "{after}");
    assert_eq!(after["platform_administrator"], false, "{after}");
}

#[test]
fn putting_someone_into_the_group_makes_them_an_administrator_at_the_next_renewal() {
    let at = installation(&["--session-renewal", "5"]);
    let began = Instant::now();
    let credential = at.tool_credential_for(GRACE);
    let signed_in = Instant::now();
    at.provider.set_groups(GRACE, &["devops", ADMINISTRATORS]);
    assert_api_error(
        &bearer(&at.vm, "/v1/oidc", &credential).send(),
        403,
        "not-an-administrator",
    );
    assert!(began.elapsed() < Duration::from_secs(5), "too slow");
    wait_past(signed_in, Duration::from_secs(5));
    let answer = bearer(&at.vm, "/v1/oidc", &credential).send();
    assert_eq!(answer.status, 200, "{}", shown(&answer, &[]));
    assert_eq!(me(&at, &credential)["platform_administrator"], true);
}

#[test]
fn who_is_an_administrator_follows_a_new_connection_s_group_at_once() {
    let at = installation(&[]);
    let ada = at.tool_credential_for(ADA);
    let grace = at.tool_credential_for(GRACE);
    at.connect(&Connection::to(&at.provider).administrators_group("devops"));
    assert_eq!(
        me(&at, &grace)["platform_administrator"],
        true,
        "at once, with no renewal"
    );
    assert_eq!(me(&at, &ada)["platform_administrator"], false);
    assert_api_error(
        &bearer(&at.vm, "/v1/oidc", &ada).send(),
        403,
        "not-an-administrator",
    );
}
