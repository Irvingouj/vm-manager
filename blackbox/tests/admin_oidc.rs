//! What Platform administrators see and stop, with OIDC (CONTRACT.md,
//! "What Platform administrators see and stop": "Who may", "Who is
//! refused", "What a disabled Application answers"), through the harness's
//! identity provider: a tool credential of a member of the administrators'
//! Group may, anyone else's is refused `not-an-administrator`; taken out of
//! the Group at the provider, a person is refused from the next renewal;
//! a disabled Application's page comes after the steps of a sign-in and
//! has one link, to the platform's sign-out.

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{LIST, OPERATION_LOG, assert_disabled_page};
use blackbox::api::version_body;
use blackbox::browser::describe_steps;
use blackbox::oidc::Person;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, Installation, grace, hrefs};
use blackbox::sharing::TRACED;
use blackbox::tool::bearer;
use blackbox::{Response, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

fn ada_administrator() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

fn installation(more: &[&str]) -> Installation {
    Installation::start_with(&[], &[ada_administrator(), grace()], more)
}

fn post(at: &Installation, credential: &str, target: &str, body: &Value) -> Response {
    bearer(&at.vm, target, credential)
        .method("POST")
        .json(body)
        .send()
}

/// `board`, made by Grace with `fixtures/honest.js`, shared with nobody
/// else; Grace's credential.
fn graces_board(at: &Installation) -> String {
    let grace = at.tool_credential_for(GRACE);
    post(at, &grace, "/v1/applications", &json!({ "name": "board" })).expect_status(201);
    post(
        at,
        &grace,
        "/v1/applications/board/versions",
        &version_body(&fixture_bundle(TRACED)),
    )
    .expect_status(201);
    grace
}

#[test]
fn a_tool_credential_of_an_administrator_may_and_anyone_elses_is_refused() {
    let at = installation(&[]);
    let grace = graces_board(&at);
    let ada = at.tool_credential_for(ADA);
    for target in [LIST.to_owned(), format!("{OPERATION_LOG}?user={GRACE}")] {
        let answer = bearer(&at.vm, &target, &ada).send();
        assert_eq!(answer.status, 200, "{target}\n{}", answer.describe());
        assert_api_error(
            &bearer(&at.vm, &target, &grace).send(),
            403,
            "not-an-administrator",
        );
    }
    // Grace is board's Contributor: that gives nothing here.
    for action in ["disable", "enable"] {
        assert_api_error(
            &post(
                &at,
                &grace,
                &format!("/v1/applications/board/{action}"),
                &json!({}),
            ),
            403,
            "not-an-administrator",
        );
    }
    let disabled = post(&at, &ada, "/v1/applications/board/disable", &json!({}));
    assert_eq!(disabled.status, 200, "{}", disabled.describe());
    assert_eq!(disabled.json()["disabled"]["by"], ADA);
    let listed = bearer(&at.vm, LIST, &ada).send().json();
    assert_eq!(listed["applications"][0]["name"], "board");
    // The Contributor entry is a person with the name and email the
    // platform read at their sign-in.
    let contributor = &listed["applications"][0]["contributors"][0];
    assert_eq!(contributor["user"], GRACE, "{contributor}");
    assert_eq!(contributor["name"], "Grace Hopper", "{contributor}");
}

#[test]
fn taken_out_of_the_group_a_person_is_refused_from_the_next_renewal() {
    let at = installation(&["--session-renewal", "5"]);
    let began = Instant::now();
    let ada = at.tool_credential_for(ADA);
    let signed_in = Instant::now();
    at.provider.set_groups(ADA, &["finance"]);
    let before = bearer(&at.vm, LIST, &ada).send();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the test was too slow to look before the renewal ({:?})",
        began.elapsed()
    );
    assert_eq!(
        before.status,
        200,
        "until the renewal\n{}",
        before.describe()
    );
    let until = signed_in + Duration::from_secs(5) + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
    for target in [LIST, OPERATION_LOG] {
        assert_api_error(
            &bearer(&at.vm, target, &ada).send(),
            403,
            "not-an-administrator",
        );
    }
}

#[test]
fn a_disabled_applications_page_comes_after_the_sign_in_and_links_to_sign_out_only() {
    let at = installation(&[]);
    let _grace = graces_board(&at);
    let ada = at.tool_credential_for(ADA);
    post(&at, &ada, "/v1/applications/board/disable", &json!({})).expect_status(200);
    // Grace, its Contributor, signs in on the way and lands on the page.
    let mut browser = at.browser_as(GRACE);
    let steps = browser.navigate(&at.app_url("board", "/"));
    let last = steps.last().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.url.host_str() == Some("auth.vm.test")),
        "a person without a session is sent to sign in first:\n{}",
        describe_steps(&steps)
    );
    assert!(
        last.url == at.app_url("board", "/") && last.response.status == 503,
        "expected the way to end with the disabled page at board's address:\n{}",
        describe_steps(&steps)
    );
    let page = &last.response;
    assert_disabled_page(
        page,
        "board",
        &[GRACE, "Grace Hopper"],
        &[ADA, "Ada Lovelace"],
        "Grace at her disabled Application",
    );
    assert_eq!(
        hrefs(&page.text()),
        vec![format!("{}/sign-out", at.auth_origin())],
        "expected exactly one link, to `<sign-in address>/sign-out`\n{}",
        page.text()
    );
}
