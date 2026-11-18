//! Contributors and sharing with OIDC (CONTRACT.md, "Contributors and
//! sharing"), through the harness's identity provider: a person named by
//! the email the platform last read of them (letter case ignored,
//! `unknown-email`, `ambiguous-email`), `name` and `email` in the answers,
//! Groups as last read (taken out of a Group at the provider, a person
//! stops opening, and stops being a Contributor, at the next renewal), the
//! `403` page after a sign-in, sign-in first whatever the sharing, and
//! sharing and unsharing at the next request without signing in again.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::api::version_body;
use blackbox::browser::{Browser, Step, describe_steps};
use blackbox::oidc::Person;
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace, hrefs};
use blackbox::sharing::{
    TRACED, add, add_remove, assert_entry, assert_error_email_entry, assert_not_shared_page,
    assert_not_shared_plain, email, entry_named, everyone, expected, group, keys, remove, user,
};
use blackbox::tool::bearer;
use blackbox::{Response, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

const APP: &str = "board";

/// Ada, Grace, Hedy (who never signs in) and two people of one address.
fn people() -> Vec<Person> {
    vec![
        ada(),
        grace(),
        Person::new("00u-hedy-1914")
            .named("Hedy Lamarr")
            .email("hedy@example.test"),
        Person::new("00u-twin-a")
            .named("Twin A")
            .email("twin@example.test"),
        Person::new("00u-twin-b")
            .named("Twin B")
            .email("Twin@Example.test"),
    ]
}

fn installation(more: &[&str]) -> Installation {
    Installation::start_with(&[], &people(), more)
}

/// A `POST` of JSON to the API with this tool credential.
fn post(at: &Installation, credential: &str, target: &str, body: &Value) -> Response {
    bearer(&at.vm, target, credential)
        .method("POST")
        .json(body)
        .send()
}

fn get(at: &Installation, credential: &str, target: &str) -> Response {
    bearer(&at.vm, target, credential).send()
}

/// Creates `name` as the holder of `credential` and sends
/// `fixtures/honest.js` as its first Version, shared with nobody.
fn create(at: &Installation, credential: &str, name: &str) {
    post(at, credential, "/v1/applications", &json!({ "name": name })).expect_status(201);
    post(
        at,
        credential,
        &format!("/v1/applications/{name}/versions"),
        &version_body(&fixture_bundle(TRACED)),
    )
    .expect_status(201);
}

/// Ada's tool credential, and `board` made by her and shared with nobody.
fn with_board(at: &Installation) -> String {
    let credential = at.tool_credential_for(ADA);
    create(at, &credential, APP);
    credential
}

fn share(at: &Installation, credential: &str, body: &Value) -> Response {
    post(
        at,
        credential,
        &format!("/v1/applications/{APP}/sharing"),
        body,
    )
}

fn share_ok(at: &Installation, credential: &str, body: &Value) -> Value {
    let answer = share(at, credential, body);
    if answer.status != 200 {
        at.vm.fail(&format!(
            "expected `200` changing whom {APP} is shared with ({body}), got\n{}",
            answer.describe()
        ));
    }
    let after = answer.json();
    blackbox::sharing::assert_application_members(&after, "the answer to a change of sharing");
    after
}

fn contributors(at: &Installation, credential: &str, body: &Value) -> Response {
    post(
        at,
        credential,
        &format!("/v1/applications/{APP}/contributors"),
        body,
    )
}

/// Signs this person in through the browser on the way to `board`, which
/// is not shared with them: the way must end with the `403` page on its
/// address. Returns the browser and the steps.
fn sign_in_refused(at: &Installation, sub: &str) -> (Browser, Vec<Step>) {
    let mut browser = at.browser_as(sub);
    let steps = browser.navigate(&at.app_url(APP, "/"));
    let last = steps.last().unwrap();
    if last.response.status != 403 || last.url != at.app_url(APP, "/") {
        at.vm.fail(&format!(
            "expected the way to end with the `403` page at {APP}'s address, but it went:\n{}",
            describe_steps(&steps)
        ));
    }
    (browser, steps)
}

/// Sleeps until `period` and a little more have passed since `since`.
fn wait_past(since: Instant, period: Duration) {
    let until = since + period + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
}

#[test]
fn the_page_after_a_sign_in_names_whom_they_are_signed_in_as_and_no_contributor() {
    let at = installation(&[]);
    let _ada = with_board(&at);
    let (_, steps) = sign_in_refused(&at, GRACE);
    assert!(
        steps.iter().any(|s| s.url.path() == "/_vm/session"
            && s.url.host_str() == Some(format!("{APP}.vm.test").as_str())
            && s.response.status == 302),
        "expected `/_vm/session` to be answered (302) for a person it is not shared with:\n{}",
        describe_steps(&steps)
    );
    let page = &steps.last().unwrap().response;
    assert_not_shared_page(
        page,
        APP,
        &[GRACE, "Grace Hopper"],
        &[ADA, "Ada Lovelace", "ada@example.test"],
        "Grace, signed in, at an Application not shared with her",
    );
    // One link, to the platform's sign-out, the whole address: the
    // Application's own `/sign-out` is not the platform's.
    assert_eq!(
        hrefs(&page.text()),
        vec![format!("{}/sign-out", at.auth_origin())],
        "expected exactly one link, to `<sign-in address>/sign-out`\n{}",
        page.text()
    );
    let records = at.vm.records_of(page);
    let record = &records.request;
    assert_eq!(record["status"], 403, "{record}");
    assert_eq!(record["user"], GRACE, "{record}");
    assert_eq!(record["application"], APP, "{record}");
    assert!(record["version"].is_null(), "{record}");
    records.assert_no_application_code_ran();
}

#[test]
fn a_request_that_is_not_a_page_is_plain_text_after_a_sign_in() {
    let at = installation(&[]);
    let _ada = with_board(&at);
    let (mut browser, _) = sign_in_refused(&at, GRACE);
    let answer = at.get_json(&mut browser, APP, "/");
    assert_not_shared_plain(&at.vm, &answer, APP, GRACE, "Grace, asking for JSON");
}

#[test]
fn a_person_without_a_session_is_sent_to_sign_in_first_whatever_the_sharing() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    create(&at, &ada_credential, "open-to-all");
    post(
        &at,
        &ada_credential,
        "/v1/applications/open-to-all/sharing",
        &add(&[everyone()]),
    )
    .expect_status(200);
    for application in [APP, "open-to-all", "nothing-here"] {
        let mut browser = at.browser();
        at.assert_no_session(
            &mut browser,
            application,
            &format!("{application}, no session"),
        );
    }
}

#[test]
fn sharing_and_unsharing_take_effect_at_the_next_request_without_signing_in_again() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    let (mut browser, _) = sign_in_refused(&at, GRACE);
    share_ok(&at, &ada_credential, &add(&[user(GRACE)]));
    let opened = at.get_json(&mut browser, APP, "/");
    assert_eq!(
        opened.status,
        200,
        "shared with her: the same session opens it at the next request\n{}",
        opened.describe()
    );
    assert_eq!(opened.json()["user"]["id"], GRACE);
    share_ok(&at, &ada_credential, &remove(&[user(GRACE)]));
    let refused = at.get_json(&mut browser, APP, "/");
    assert_not_shared_plain(&at.vm, &refused, APP, GRACE, "unshared");
    let page = browser.open(&at.app_url(APP, "/"));
    assert_eq!(
        page.response.status,
        403,
        "unshared: the page is the `403`, not a new sign-in\n{}",
        page.response.describe()
    );
}

#[test]
fn an_email_names_the_person_who_signed_in_with_it_letter_case_ignored() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    let (mut browser, _) = sign_in_refused(&at, GRACE);
    let after = share_ok(&at, &ada_credential, &add(&[email("GRACE@Example.TEST")]));
    let shared = &after["shared_with"];
    assert_eq!(
        keys(shared),
        vec![format!("user:{GRACE}")],
        "the entry kept is the person's user id"
    );
    assert_entry(
        entry_named(shared, &format!("user:{GRACE}")),
        ADA,
        Some("Grace Hopper"),
        Some("grace@example.test"),
        "Grace, by her email",
    );
    let opened = at.get_json(&mut browser, APP, "/");
    assert_eq!(opened.status, 200, "{}", opened.describe());
    // The same for the Contributors.
    let answer = contributors(&at, &ada_credential, &add(&[email("grace@example.test")]));
    answer.expect_status(200);
    let list = &answer.json()["contributors"];
    assert_eq!(
        keys(list),
        vec![format!("user:{ADA}"), format!("user:{GRACE}")]
    );
    assert_entry(
        entry_named(list, &format!("user:{ADA}")),
        ADA,
        Some("Ada Lovelace"),
        Some("ada@example.test"),
        "Ada, the creator",
    );
    // Adding her again by email changes nothing.
    let again = share_ok(&at, &ada_credential, &add(&[email("grace@EXAMPLE.test")]));
    assert_eq!(again["shared_with"], after["shared_with"]);
}

#[test]
fn name_and_email_are_those_last_read_and_null_for_someone_who_never_signed_in() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    sign_in_refused(&at, GRACE);
    let after = share_ok(
        &at,
        &ada_credential,
        &add(&[user(GRACE), user("00u-hedy-1914")]),
    );
    let shared = &after["shared_with"];
    assert_entry(
        entry_named(shared, &format!("user:{GRACE}")),
        ADA,
        Some("Grace Hopper"),
        Some("grace@example.test"),
        "Grace, who signed in",
    );
    assert_entry(
        entry_named(shared, "user:00u-hedy-1914"),
        ADA,
        None,
        None,
        "Hedy, who never signed in here",
    );
    // A new sign-in brings what the provider says now.
    at.provider.set_name(GRACE, Some("Grace B. Hopper"));
    at.provider
        .set_email(GRACE, Some("grace.hopper@example.test"));
    let mut browser = at.browser_as(GRACE);
    at.sign_in(&mut browser, APP, "/");
    let read = get(&at, &ada_credential, &format!("/v1/applications/{APP}"));
    read.expect_status(200);
    assert_entry(
        entry_named(&read.json()["shared_with"], &format!("user:{GRACE}")),
        ADA,
        Some("Grace B. Hopper"),
        Some("grace.hopper@example.test"),
        "Grace, after a sign-in with a new name and email",
    );
}

#[test]
fn an_email_of_someone_who_never_signed_in_is_422_unknown_email() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    for address in ["hedy@example.test", "nobody@example.test"] {
        let answer = share(&at, &ada_credential, &add(&[user(GRACE), email(address)]));
        assert_api_error(&answer, 422, "unknown-email");
        assert_error_email_entry(&answer, address, "unknown-email");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains(address) && message.contains("vm whoami"),
            "expected the message to name the entry and say that `vm whoami` shows the user id, got {message:?}"
        );
        let answer = contributors(&at, &ada_credential, &add(&[email(address)]));
        assert_api_error(&answer, 422, "unknown-email");
        assert_error_email_entry(&answer, address, "unknown-email, Contributors");
    }
    let read = get(&at, &ada_credential, &format!("/v1/applications/{APP}"));
    assert_eq!(
        read.json()["shared_with"],
        json!([]),
        "nothing was changed, not even the good entry beside the refused one"
    );
}

#[test]
fn an_email_two_people_signed_in_with_is_409_ambiguous_email() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    sign_in_refused(&at, "00u-twin-a");
    sign_in_refused(&at, "00u-twin-b");
    for address in ["twin@example.test", "TWIN@EXAMPLE.TEST"] {
        let answer = share(&at, &ada_credential, &add(&[email(address)]));
        assert_api_error(&answer, 409, "ambiguous-email");
        assert_error_email_entry(&answer, address, "ambiguous-email");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            message.contains("twin@example.test") && message.contains("user id"),
            "expected the message to name the entry and say to use the user id, got {message:?}"
        );
        let answer = contributors(&at, &ada_credential, &add(&[email(address)]));
        assert_api_error(&answer, 409, "ambiguous-email");
        assert_error_email_entry(&answer, address, "ambiguous-email, Contributors");
    }
    let read = get(&at, &ada_credential, &format!("/v1/applications/{APP}"));
    assert_eq!(read.json()["shared_with"], json!([]), "nothing was changed");
    let after = share_ok(&at, &ada_credential, &add(&[user("00u-twin-a")]));
    assert_eq!(keys(&after["shared_with"]), expected(&["user:00u-twin-a"]));
}

#[test]
fn a_person_taken_out_of_a_group_at_the_provider_stops_opening_at_the_next_renewal() {
    let at = installation(&["--session-renewal", "2"]);
    let ada_credential = with_board(&at);
    share_ok(&at, &ada_credential, &add(&[group("devops")]));
    let mut browser = at.browser_as(GRACE);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, APP, "/");
    at.provider.set_groups(GRACE, &["audit"]);
    wait_past(signed_in, Duration::from_secs(2));
    let answer = at.get_json(&mut browser, APP, "/");
    assert_not_shared_plain(
        &at.vm,
        &answer,
        APP,
        GRACE,
        "Grace, taken out of devops, after the renewal",
    );
    assert!(
        !at.provider.grants("refresh_token").is_empty(),
        "the session was renewed"
    );
    // Put back in, she opens it again at the next renewal.
    at.provider.set_groups(GRACE, &["devops"]);
    wait_past(Instant::now(), Duration::from_secs(2));
    let again = at.get_json(&mut browser, APP, "/");
    assert_eq!(again.status, 200, "{}", again.describe());
}

#[test]
fn a_person_taken_out_of_a_group_at_the_provider_stops_being_a_contributor_at_the_next_renewal() {
    let at = installation(&["--session-renewal", "2"]);
    let ada_credential = with_board(&at);
    contributors(&at, &ada_credential, &add(&[group("devops")])).expect_status(200);
    let grace_credential = at.tool_credential_for(GRACE);
    let versions = format!("/v1/applications/{APP}/versions");
    post(
        &at,
        &grace_credential,
        &versions,
        &version_body(&fixture_bundle(TRACED)),
    )
    .expect_status(201);
    let mut browser = at.browser_as(GRACE);
    at.sign_in(&mut browser, APP, "/");
    at.provider.set_groups(GRACE, &[]);
    wait_past(Instant::now(), Duration::from_secs(2));
    assert_api_error(
        &post(
            &at,
            &grace_credential,
            &versions,
            &version_body(&fixture_bundle(TRACED)),
        ),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &contributors(&at, &grace_credential, &add(&[user(GRACE)])),
        403,
        "not-a-contributor",
    );
    let answer = at.get_json(&mut browser, APP, "/");
    assert_not_shared_plain(
        &at.vm,
        &answer,
        APP,
        GRACE,
        "Grace, no longer a Contributor by devops and not shared with",
    );
}

#[test]
fn the_same_person_by_id_and_by_email_in_add_and_remove_is_400_malformed_after_unknown_email() {
    let at = installation(&[]);
    let ada_credential = with_board(&at);
    sign_in_refused(&at, GRACE);
    share_ok(&at, &ada_credential, &add(&[user(GRACE)]));
    let before = get(&at, &ada_credential, &format!("/v1/applications/{APP}")).json();
    for (path, body) in [
        (
            "sharing",
            add_remove(&[user(GRACE)], &[email("grace@example.test")]),
        ),
        (
            "sharing",
            add_remove(&[email("GRACE@example.test")], &[user(GRACE)]),
        ),
        (
            "contributors",
            add_remove(&[user(GRACE)], &[email("grace@example.test")]),
        ),
        (
            "contributors",
            add_remove(&[email("Grace@Example.Test")], &[user(GRACE)]),
        ),
    ] {
        let target = format!("/v1/applications/{APP}/{path}");
        assert_api_error(
            &post(&at, &ada_credential, &target, &body),
            400,
            "malformed",
        );
        // With an address nobody signed in with beside it, `unknown-email`
        // comes first: the emails are resolved before the same person is
        // found on both sides.
        let mut with_unknown = body.clone();
        with_unknown["add"]
            .as_array_mut()
            .unwrap()
            .push(email("nobody@example.test"));
        let answer = post(&at, &ada_credential, &target, &with_unknown);
        assert_api_error(&answer, 422, "unknown-email");
        assert_error_email_entry(&answer, "nobody@example.test", &format!("{with_unknown}"));
    }
    assert_eq!(
        get(&at, &ada_credential, &format!("/v1/applications/{APP}")).json(),
        before,
        "nothing was changed"
    );
}
