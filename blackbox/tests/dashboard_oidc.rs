//! The Dashboard with OIDC, through the harness's identity provider and
//! browser (CONTRACT.md, "The Dashboard: Signing in to it", and "Sign-in
//! and sessions"): the way in (`302` for a page, `401` otherwise; the
//! sign-in address takes `dashboard`), a cookie valid on the Dashboard
//! only, sign-out ending it, opening an Application from the Dashboard's
//! link without the identity provider being asked again, people shown by
//! name, sharing by email, and a form on an Application's page refused.
//!
//! Acceptance criteria: "Opening an Application from the Dashboard does
//! not ask for sign-in again"; "A Contributor shares an Application with a
//! person, a Group or everyone from the Dashboard, and the change shows for
//! those people".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::version_body;
use blackbox::browser::{Browser, Step, describe_steps, query_param};
use blackbox::dashboard::{
    Page, assert_changed, assert_refusal_words, assert_refused, oidc_page, oidc_post, oidc_sign_in,
    oidc_url,
};
use blackbox::oidc::Person;
use blackbox::sessions::{
    ADA, GRACE, Installation, SESSION_COOKIE, SIGN_IN_COOKIE, ada, assert_platform_cookie, grace,
    the_set_cookie,
};
use blackbox::sharing::{TRACED, add, keys, user};
use blackbox::tool::bearer;
use blackbox::{DOMAIN, Request, Response, fixture_bundle};
use serde_json::{Value, json};

const APP: &str = "board";
const HEDY: &str = "00u-hedy-1914";

fn people() -> Vec<Person> {
    vec![
        ada(),
        grace(),
        Person::new(HEDY)
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

/// The installation, with `honest` (shared with everyone by the harness).
fn installation() -> Installation {
    Installation::start_with(&[("honest", "honest.js")], &people(), &[])
}

fn post(at: &Installation, credential: &str, target: &str, body: &Value) -> Response {
    bearer(&at.vm, target, credential)
        .method("POST")
        .json(body)
        .send()
}

/// Ada's tool credential, and `board` made by her with
/// `fixtures/honest.js`, shared with nobody.
fn with_board(at: &Installation) -> String {
    let credential = at.tool_credential_for(ADA);
    post(at, &credential, "/v1/applications", &json!({ "name": APP })).expect_status(201);
    post(
        at,
        &credential,
        &format!("/v1/applications/{APP}/versions"),
        &version_body(&fixture_bundle(TRACED)),
    )
    .expect_status(201);
    credential
}

fn shared_with(at: &Installation, credential: &str) -> Vec<String> {
    let answer = bearer(&at.vm, &format!("/v1/applications/{APP}"), credential).send();
    answer.expect_status(200);
    keys(&answer.json()["shared_with"])
}

/// Signs this person in to the Dashboard; their browser and the page.
fn signed_in(at: &Installation, sub: &str) -> (Browser, Page) {
    let mut browser = at.browser_as(sub);
    let (_, page) = oidc_sign_in(at, &mut browser);
    (browser, page)
}

fn at_provider(at: &Installation, steps: &[Step]) -> bool {
    let provider = at.provider.address();
    steps.iter().any(|s| {
        s.url.host_str() == Some(provider.ip().to_string().as_str())
            && s.url.port() == Some(provider.port())
    })
}

/// A request for JSON (not a page) to the Dashboard with the browser's
/// cookies.
fn dashboard_json(at: &Installation, browser: &mut Browser, target: &str) -> Response {
    let address = oidc_url(at, target);
    let request = browser
        .request("GET", &address)
        .header("accept", "application/json");
    browser.send(&address, &request)
}

/// Fails unless this browser has no session on the Dashboard.
fn assert_no_dashboard_session(at: &Installation, browser: &mut Browser, context: &str) {
    let json = dashboard_json(at, browser, "/");
    assert!(
        json.status == 401,
        "{context}: expected no session on the Dashboard (401 for a request that is not a page)\n{}",
        json.describe()
    );
    let page = browser.open(&oidc_url(at, "/"));
    assert!(
        page.is_redirect()
            && page.location().host_str() == Some(at.auth_host().as_str())
            && page.location().path() == "/sign-in",
        "{context}: expected a page of the Dashboard to be sent to the sign-in address\n{}",
        page.response.describe()
    );
}

#[test]
fn without_a_session_a_page_is_sent_to_sign_in_for_the_dashboard_and_anything_else_is_401() {
    let at = installation();
    let mut browser = at.browser();
    let target = "/applications/board?changed=shared";
    let step = browser.open(&oidc_url(&at, target));
    assert!(
        step.is_redirect(),
        "expected `302` to the sign-in address for a page of the Dashboard without a session\n{}",
        step.response.describe()
    );
    assert_eq!(step.response.status, 302, "{}", step.response.describe());
    let to = step.location();
    assert!(
        to.host_str() == Some(at.auth_host().as_str()) && to.path() == "/sign-in",
        "expected the way to `<sign-in address>/sign-in`, got {to}"
    );
    assert_eq!(
        query_param(&to, "application").as_deref(),
        Some("dashboard"),
        "expected `application=dashboard` in {to}"
    );
    assert_eq!(
        query_param(&to, "next").as_deref(),
        Some(target),
        "expected `next` the path and query"
    );
    assert!(
        query_param(&to, "state").is_some_and(|s| !s.is_empty()),
        "expected a `state` in the address"
    );
    let cookie = the_set_cookie(&step.response, SIGN_IN_COOKIE, "step 1 on the Dashboard");
    assert_platform_cookie(
        &cookie,
        SIGN_IN_COOKIE,
        "the cookie of step 1 on the Dashboard",
    );

    let head = browser
        .request("HEAD", &oidc_url(&at, "/"))
        .header("accept", "text/html")
        .send();
    assert_eq!(head.status, 302, "a HEAD of a page\n{}", head.describe());

    let json = dashboard_json(&at, &mut browser, "/");
    let post = {
        let address = oidc_url(&at, "/applications/board/sharing");
        let request = browser.form_from(
            &oidc_url(&at, "/"),
            &address,
            "action=share&kind=everyone&value=",
        );
        browser.send(&address, &request)
    };
    for (what, answer) in [("a request for JSON", json), ("a change of sharing", post)] {
        assert!(
            answer.status == 401
                && answer
                    .header("content-type")
                    .is_some_and(|t| t.starts_with("text/plain")),
            "{what} without a session: expected `401`, `text/plain`\n{}",
            answer.describe()
        );
    }
}

#[test]
fn signing_in_to_the_dashboard_lands_on_it_signed_in_with_its_own_cookie() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    let (steps, page) = oidc_sign_in(&at, &mut browser);
    assert!(
        at_provider(&at, &steps),
        "the harness's own check: a first sign-in goes through the provider\n{}",
        describe_steps(&steps)
    );
    let session = steps
        .iter()
        .find(|s| {
            s.url.host_str() == Some(format!("dashboard.{DOMAIN}").as_str())
                && s.url.path() == "/_vm/session"
        })
        .unwrap_or_else(|| {
            panic!(
                "expected step 4 at the Dashboard's /_vm/session\n{}",
                describe_steps(&steps)
            )
        });
    let cookie = the_set_cookie(&session.response, SESSION_COOKIE, "step 4 on the Dashboard");
    assert_platform_cookie(&cookie, SESSION_COOKIE, "the Dashboard's session cookie");
    let (id, text) = page.user();
    assert_eq!(
        id,
        ADA,
        "expected `data-vm-user` the person's id\n{}",
        page.describe()
    );
    assert!(
        text.contains("Ada Lovelace"),
        "expected the element `data-vm-user` to have her name, got {text:?}"
    );
    let sign_out = format!("{}/sign-out", at.auth_origin());
    assert!(
        page.hrefs().contains(&sign_out),
        "expected a link to {sign_out}, got {:?}",
        page.hrefs()
    );
    assert_eq!(
        page.names(),
        vec!["honest".to_owned()],
        "expected what is shared with Ada (the harness's fixture, shared with everyone)"
    );
}

#[test]
fn with_a_platform_session_the_sign_in_address_takes_dashboard_and_the_provider_is_not_asked() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let asked = at.provider.requests().len();
    let (steps, _) = oidc_sign_in(&at, &mut browser);
    assert!(
        !at_provider(&at, &steps) && at.provider.requests().len() == asked,
        "expected one sign-in for all: the Dashboard signed in to without the provider\n{}",
        describe_steps(&steps)
    );
    let direct = browser
        .open(&at.auth_url("/sign-in?application=dashboard&next=%2Fapplications%2Fx&state=s-123"));
    let to = direct.is_redirect().then(|| direct.location());
    assert!(
        to.as_ref().is_some_and(
            |to| to.host_str() == Some(format!("dashboard.{DOMAIN}").as_str())
                && to.path() == "/_vm/session"
                && query_param(to, "state").as_deref() == Some("s-123")
                && query_param(to, "next").as_deref() == Some("/applications/x")
                && query_param(to, "code").is_some()
        ),
        "expected the sign-in address to take `dashboard` as a known Application (never its `404`): `302` to the Dashboard's /_vm/session\n{}",
        direct.response.describe()
    );
}

/// A `GET` of `/` at `host` asking for JSON with exactly this `Cookie`.
fn with_cookie(at: &Installation, host: &str, target: &str, cookie: &str) -> Response {
    Request::new(at.vm.address(), target, &format!("{host}:{}", at.port()))
        .header("accept", "application/json")
        .header("cookie", cookie)
        .send()
}

#[test]
fn the_dashboards_cookie_is_valid_on_the_dashboard_only() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    oidc_sign_in(&at, &mut browser);
    let dashboard_host = format!("dashboard.{DOMAIN}");
    let honest_host = format!("honest.{DOMAIN}");
    let dashboard = at.cookie_value(&browser, &dashboard_host, SESSION_COOKIE);
    let honest = at.cookie_value(&browser, &honest_host, SESSION_COOKIE);
    let platform = at.cookie_value(&browser, &at.auth_host(), SESSION_COOKIE);
    let named = |value: &str| format!("{SESSION_COOKIE}={value}");

    let own = with_cookie(&at, &dashboard_host, "/", &named(&dashboard));
    assert_eq!(
        own.status,
        200,
        "the harness's own check: the Dashboard's cookie there\n{}",
        own.describe()
    );
    for (what, host, target, cookie) in [
        (
            "the Dashboard's cookie at an Application",
            honest_host.as_str(),
            "/",
            &dashboard,
        ),
        (
            "the Dashboard's cookie at the API",
            "api.vm.test",
            "/v1/me",
            &dashboard,
        ),
        (
            "the Dashboard's cookie at the sign-in address",
            "auth.vm.test",
            "/me",
            &dashboard,
        ),
        (
            "an Application's cookie at the Dashboard",
            dashboard_host.as_str(),
            "/",
            &honest,
        ),
        (
            "the platform session's cookie at the Dashboard",
            dashboard_host.as_str(),
            "/",
            &platform,
        ),
    ] {
        let answer = with_cookie(&at, host, target, &named(cookie));
        assert_eq!(
            answer.status,
            401,
            "{what}: expected no session (401)\n{}",
            answer.describe()
        );
    }
}

#[test]
fn signing_out_ends_the_dashboards_session() {
    let at = installation();
    let (mut browser, _) = signed_in(&at, ADA);
    let address = at.auth_url("/sign-out");
    let request = browser
        .request("POST", &address)
        .header("accept", "text/html")
        .header("origin", at.auth_origin())
        .header("content-type", "application/x-www-form-urlencoded");
    let answer = browser.send(&address, &request);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_no_dashboard_session(&at, &mut browser, "after signing out");
}

#[test]
fn opening_an_application_from_the_dashboards_link_does_not_ask_the_identity_provider_again() {
    let at = installation();
    with_board(&at);
    let (mut browser, page) = signed_in(&at, ADA);
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("contributor"),
        "{}",
        page.describe()
    );
    let href = board
        .open_href()
        .unwrap_or_else(|| panic!("expected `a[data-vm-open]` for {APP}\n{}", page.describe()));
    assert_eq!(
        href,
        format!("{}/", at.app_origin(APP)),
        "expected the link to be the Application's address in full"
    );
    let asked = at.provider.requests().len();
    let steps = browser.navigate(&blackbox::browser::url(&href));
    let last = steps.last().unwrap();
    assert!(
        last.response.status == 200 && last.url == at.app_url(APP, "/"),
        "expected the link to land on {APP} signed in with 200\n{}",
        describe_steps(&steps)
    );
    assert!(
        !at_provider(&at, &steps) && at.provider.requests().len() == asked,
        "expected the identity provider not to be asked again\n{}",
        describe_steps(&steps)
    );
    assert!(
        last.response.text().contains(ADA),
        "expected {APP}'s code given the request as Ada\n{}",
        last.response.describe()
    );
}

#[test]
fn people_are_shown_with_their_names_and_emails() {
    let at = installation();
    let credential = with_board(&at);
    signed_in(&at, GRACE);
    signed_in(&at, "00u-twin-a");
    post(
        &at,
        &credential,
        &format!("/v1/applications/{APP}/contributors"),
        &add(&[user(GRACE)]),
    )
    .expect_status(200);
    post(
        &at,
        &credential,
        &format!("/v1/applications/{APP}/sharing"),
        &add(&[user("00u-twin-a"), user(HEDY)]),
    )
    .expect_status(200);
    let (mut browser, page) = signed_in(&at, ADA);
    let board = page.listed(APP);
    assert_eq!(
        board.contributor_keys(),
        vec![format!("user:{ADA}"), format!("user:{GRACE}")],
        "{}",
        page.describe()
    );
    for (shown, name) in board
        .contributors
        .iter()
        .zip(["Ada Lovelace", "Grace Hopper"])
    {
        assert!(
            shown.text.contains(name),
            "expected a Contributor shown by their name {name:?}, got {shown:?}"
        );
    }
    let details = oidc_page(
        &at,
        &mut browser,
        &format!("/applications/{APP}"),
        200,
        "Ada at board",
    );
    let shared = details.shared();
    assert_eq!(
        details.shared_keys(),
        vec!["user:00u-twin-a".to_owned(), format!("user:{HEDY}")],
        "{}",
        details.describe()
    );
    assert!(
        shared[0].text.contains("Twin A") && shared[0].text.contains("twin@example.test"),
        "expected a person it is shared with shown with their name and email, got {:?}",
        shared[0]
    );
    assert!(
        shared[1].text.contains(HEDY) && !shared[1].text.contains("Hedy Lamarr"),
        "expected someone who never signed in here shown by their id, got {:?}",
        shared[1]
    );
}

#[test]
fn shared_by_email_from_the_dashboard_the_person_finds_it_and_opens_it_and_after_an_unshare_neither()
 {
    let at = installation();
    let credential = with_board(&at);
    let (mut grace_browser, grace_page) = signed_in(&at, GRACE);
    assert!(
        !grace_page.names().iter().any(|n| n == APP),
        "the harness's own check"
    );

    let (mut ada_browser, _) = signed_in(&at, ADA);
    let page = oidc_page(
        &at,
        &mut ada_browser,
        &format!("/applications/{APP}"),
        200,
        "Ada at board",
    );
    let (action, body) = page.share_submission("email", "GRACE@Example.TEST");
    let answer = oidc_post(&mut ada_browser, &page, &action, &body);
    assert_changed(
        &answer,
        &oidc_url(&at, "/"),
        APP,
        "shared",
        "sharing with Grace by her email",
    );
    assert_eq!(
        shared_with(&at, &credential),
        vec![format!("user:{GRACE}")],
        "expected the entry kept to be the person's user id"
    );

    let grace_page = oidc_page(
        &at,
        &mut grace_browser,
        "/",
        200,
        "Grace's Dashboard after the share",
    );
    let board = grace_page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("shared"),
        "{}",
        grace_page.describe()
    );
    let asked = at.provider.requests().len();
    let href = board
        .open_href()
        .unwrap_or_else(|| panic!("expected a link to open {APP}"));
    let steps = grace_browser.navigate(&blackbox::browser::url(&href));
    let last = steps.last().unwrap();
    assert!(
        last.response.status == 200
            && last.url == at.app_url(APP, "/")
            && at.provider.requests().len() == asked,
        "expected Grace to open {APP} from her Dashboard, the provider not asked\n{}",
        describe_steps(&steps)
    );

    let page = oidc_page(
        &at,
        &mut ada_browser,
        &format!("/applications/{APP}"),
        200,
        "Ada at board",
    );
    let (action, body) = page.unshare_submission("user", GRACE);
    let answer = oidc_post(&mut ada_browser, &page, &action, &body);
    assert_changed(
        &answer,
        &oidc_url(&at, "/"),
        APP,
        "unshared",
        "unsharing Grace with her entry's form",
    );
    let grace_page = oidc_page(
        &at,
        &mut grace_browser,
        "/",
        200,
        "Grace's Dashboard after the unshare",
    );
    assert!(
        !grace_page.names().iter().any(|n| n == APP),
        "expected Grace not to find {APP} after the unshare\n{}",
        grace_page.describe()
    );
    let refused = at.get_json(&mut grace_browser, APP, "/");
    assert_eq!(
        refused.status,
        403,
        "expected Grace refused at {APP} after the unshare\n{}",
        refused.describe()
    );
}

#[test]
fn an_email_nobody_signed_in_with_is_422_and_one_two_people_signed_in_with_is_409() {
    let at = installation();
    let credential = with_board(&at);
    signed_in(&at, "00u-twin-a");
    signed_in(&at, "00u-twin-b");
    let (mut browser, _) = signed_in(&at, ADA);
    for (address, status, reason) in [
        ("hedy@example.test", 422, "unknown-email"),
        ("nobody@example.test", 422, "unknown-email"),
        ("TWIN@example.test", 409, "ambiguous-email"),
    ] {
        let page = oidc_page(
            &at,
            &mut browser,
            &format!("/applications/{APP}"),
            200,
            "Ada at board",
        );
        let (action, body) = page.share_submission("email", address);
        let answer = oidc_post(&mut browser, &page, &action, &body);
        let refused = assert_refused(answer, &oidc_url(&at, "/"), status, reason, APP, address);
        assert_refusal_words(
            &refused,
            &[&[address], &["user id", "user ID"]],
            &format!("{address}: {reason} names the address as given and says to use the user id"),
        );
    }
    assert!(
        shared_with(&at, &credential).is_empty(),
        "nothing is changed"
    );
}

#[test]
fn a_form_on_an_applications_page_cannot_share_and_its_script_cannot_read_the_dashboard() {
    let at = installation();
    let credential = with_board(&at);
    let (mut browser, _) = signed_in(&at, ADA);
    at.sign_in(&mut browser, "honest", "/");
    let honest_page = at.app_url("honest", "/");
    let action = oidc_url(&at, &format!("/applications/{APP}/sharing"));
    let request = browser.form_from(&honest_page, &action, "action=share&kind=everyone&value=");
    let answer = browser.send(&action, &request);
    assert!(
        answer.status == 403
            && answer
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/plain")),
        "expected a form posted from an Application's page refused, `403`, `text/plain`\n{}",
        answer.describe()
    );
    assert!(
        shared_with(&at, &credential).is_empty(),
        "nothing was changed"
    );
    let dashboard = oidc_url(&at, "/");
    let request = browser.fetch_from(&honest_page, "GET", &dashboard);
    let read = browser.send(&dashboard, &request);
    assert_eq!(
        read.status,
        403,
        "expected an Application's script refused reading the Dashboard\n{}",
        read.describe()
    );
}
