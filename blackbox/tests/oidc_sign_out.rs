//! Signing out, and the User's record (CONTRACT.md, "Sign-in and sessions:
//! Renewal and the end of a session" on sign-out, and "The User's
//! record"): the page and its `POST`, every Application's session ended,
//! the cookie cleared, the provider not told; `/me`; every other path of
//! the sign-in address `404`.
//!
//! Acceptance criterion: "Signing out ends the session for every
//! Application".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::browser::{Browser, set_cookies};
use blackbox::oidc::Person;
use blackbox::sessions::{ADA, Installation, SESSION_COOKIE, ada, grace};
use serde_json::{Value, json};

fn two_applications() -> Installation {
    Installation::start(&[("honest", "honest.js"), ("ledger", "honest.js")])
}

fn sign_out(at: &Installation, browser: &mut Browser, origin: Option<&str>) -> blackbox::Response {
    let page = at.auth_url("/sign-out");
    let mut request = browser
        .request("POST", &page)
        .header("accept", "text/html")
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    browser.send(&page, &request)
}

#[test]
fn the_sign_out_page_has_one_button_that_posts() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let page = browser.open(&at.auth_url("/sign-out"));
    assert_eq!(page.response.status, 200, "{}", page.response.describe());
    assert!(
        page.response
            .header("content-type")
            .is_some_and(|c| c.starts_with("text/html")),
        "{}",
        page.response.describe()
    );
    let html = page.response.text().to_ascii_lowercase();
    assert!(
        html.contains("<form") && html.contains("post") && html.contains("<button"),
        "expected a form that posts, with a button: {html}"
    );
    assert_eq!(html.matches("<button").count(), 1, "one button: {html}");
    assert_eq!(
        at.me(&mut browser).status,
        200,
        "the page alone signs no one out"
    );
}

#[test]
fn signing_out_ends_the_session_for_every_application_and_clears_the_cookie() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    let asked_before = at.provider.requests().len();
    let answer = sign_out(&at, &mut browser, Some(&at.auth_origin()));
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert!(
        answer
            .header("content-type")
            .is_some_and(|c| c.starts_with("text/html")),
        "a page saying so\n{}",
        answer.describe()
    );
    let cleared = set_cookies(&answer)
        .into_iter()
        .find(|c| c.name == SESSION_COOKIE)
        .unwrap_or_else(|| {
            panic!(
                "expected the platform cookie cleared\n{}",
                answer.describe()
            )
        });
    assert!(
        cleared.clears(),
        "expected {:?} to clear the cookie",
        cleared.raw
    );
    assert!(!cleared.has("domain"), "{:?}", cleared.raw);
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
    // The Applications' own cookies are still in the browser, and are no
    // session any more.
    for application in ["honest", "ledger"] {
        assert!(
            browser
                .jar
                .get(
                    &format!("{application}.{}", blackbox::DOMAIN),
                    SESSION_COOKIE
                )
                .is_some(),
            "the browser still holds {application}'s cookie"
        );
        at.assert_no_session(&mut browser, application, "after signing out");
    }
    assert_eq!(at.me(&mut browser).status, 401);
    assert_eq!(
        at.provider.requests().len(),
        asked_before,
        "the identity provider is not told"
    );
    // A page signs in again through the provider, which answers without
    // asking.
    let steps = at.sign_in(&mut browser, "honest", "/");
    let provider_host = at.provider.address().ip().to_string();
    assert!(
        steps
            .iter()
            .any(|s| s.url.host_str() == Some(provider_host.as_str()))
    );
}

#[test]
fn a_sign_out_from_another_origin_is_refused_and_the_session_holds() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let from_application = sign_out(&at, &mut browser, Some(&at.app_origin("honest")));
    assert_eq!(
        from_application.status,
        403,
        "{}",
        from_application.describe()
    );
    let page = at.auth_url("/sign-out");
    let cross_site = browser
        .request("POST", &page)
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document");
    let cross_site = browser.send(&page, &cross_site);
    assert_eq!(cross_site.status, 403, "{}", cross_site.describe());
    assert_eq!(at.me(&mut browser).status, 200, "still signed in");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}

#[test]
fn signing_out_in_one_browser_leaves_another_browsers_session() {
    let at = two_applications();
    let mut first = at.browser_as(ADA);
    let mut second = at.browser_as(ADA);
    at.sign_in(&mut first, "honest", "/");
    at.sign_in(&mut second, "honest", "/");
    sign_out(&at, &mut first, Some(&at.auth_origin())).expect_status(200);
    at.assert_no_session(&mut first, "honest", "the browser that signed out");
    assert_eq!(at.user_seen(&mut second, "honest")["id"], ADA);
}

#[test]
fn me_gives_the_users_record_for_this_browsers_session_and_401_without_one() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    assert_eq!(at.me(&mut browser).status, 401, "no session yet");
    at.sign_in(&mut browser, "honest", "/");
    let me = at.me(&mut browser);
    assert_eq!(me.status, 200, "{}", me.describe());
    assert!(
        me.header("content-type")
            .is_some_and(|c| c.starts_with("application/json")),
        "{}",
        me.describe()
    );
    let record = me.json();
    assert_eq!(record["id"], ADA);
    assert_eq!(record["name"], "Ada Lovelace");
    assert_eq!(record["email"], "ada@example.test");
    assert_eq!(record["groups"], json!(["finance"]));
    for field in ["first_sign_in", "session_started", "session_renewed"] {
        assert!(
            !record[field].is_null(),
            "expected `{field}` in the record: {record}"
        );
    }
    // A second sign-in in another browser: the same first sign-in, another
    // session.
    std::thread::sleep(Duration::from_millis(1100));
    let mut later = at.browser_as(ADA);
    at.sign_in(&mut later, "honest", "/");
    let again = at.me(&mut later).json();
    assert_eq!(again["first_sign_in"], record["first_sign_in"]);
    assert_ne!(again["session_started"], record["session_started"]);
}

#[test]
fn name_and_email_are_brought_up_to_date_at_renewal_and_at_sign_in() {
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[ada(), grace()],
        &["--session-renewal", "2"],
    );
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.provider.set_name(ADA, Some("Ada King"));
    std::thread::sleep(
        (signed_in + Duration::from_millis(2700)).saturating_duration_since(Instant::now()),
    );
    at.user_seen(&mut browser, "honest");
    assert_eq!(at.me(&mut browser).json()["name"], "Ada King", "at renewal");
    at.provider.set_email(ADA, Some("ada.king@example.test"));
    let mut other = at.browser_as(ADA);
    at.sign_in(&mut other, "honest", "/");
    assert_eq!(
        at.me(&mut other).json()["email"],
        "ada.king@example.test",
        "at sign-in"
    );
}

fn absent_or_null(record: &Value, field: &str) -> bool {
    record.get(field).is_none_or(Value::is_null)
}

#[test]
fn a_name_or_email_longer_than_256_bytes_is_left_out() {
    let long = Person::new("long-sub")
        .named(&"n".repeat(257))
        .email(&format!("{}@example.test", "e".repeat(250)));
    let exact = Person::new("exact-sub").named(&"n".repeat(256));
    let at = Installation::start_with(&[("honest", "honest.js")], &[long, exact], &[]);
    let mut browser = at.browser_as("long-sub");
    at.sign_in(&mut browser, "honest", "/");
    let record = at.me(&mut browser).json();
    assert_eq!(record["id"], "long-sub");
    assert!(absent_or_null(&record, "name"), "{record}");
    assert!(absent_or_null(&record, "email"), "{record}");
    let mut browser = at.browser_as("exact-sub");
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.me(&mut browser).json()["name"], "n".repeat(256));
}

#[test]
fn every_other_path_of_the_sign_in_address_is_404() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    for path in [
        "/",
        "/anything",
        "/me/x",
        "/sign-in/x",
        "/callback/x",
        "/sign-out/x",
        "/_vm/session",
        "/.well-known/openid-configuration",
    ] {
        let at_url = at.auth_url(path);
        let request = browser.request("GET", &at_url);
        let answer = browser.send(&at_url, &request);
        assert_eq!(answer.status, 404, "{path}\n{}", answer.describe());
    }
}
