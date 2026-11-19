//! Signing in, opening an Application from the Dashboard, and signing out,
//! walked in a real browser over HTTPS (CONTRACT.md, "A real browser in
//! the gate: What it walks", 1, 2 and 5, and "Over HTTPS, as an
//! installation is reached"; "Sign-in and sessions"; "The Dashboard:
//! Signing in to it").
//!
//! Acceptance criteria: "In a real browser, as tests of the gate: sign-in
//! through an OIDC provider and landing on the Dashboard; opening an
//! Application from it without signing in again; ... sign-out by its
//! form".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::firefox::{self, Firefox};
use blackbox::sessions::{ADA, ada, grace};
use blackbox::walks::{
    Site, at_provider, authorizations, choose_at_provider, on_site, page_json,
    sign_in_to_dashboard, wait_for_element,
};
use serde_json::Value;

const APP: &str = "board";

fn site() -> Site {
    Site::start(&[ada(), grace()])
}

/// The cookies the browser holds for the page shown now (its host), as
/// Marionette gives them; values are never printed.
fn cookies_here(browser: &mut Firefox) -> Vec<Value> {
    browser
        .must("WebDriver:GetCookies", &serde_json::json!({}))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A cookie without its value, for a message.
fn shown(cookie: &Value) -> String {
    format!(
        "{} (domain {}, path {}, secure {}, httpOnly {}, sameSite {})",
        cookie["name"],
        cookie["domain"],
        cookie["path"],
        cookie["secure"],
        cookie["httpOnly"],
        cookie["sameSite"]
    )
}

/// Fails unless the browser, on a page of `host`, holds a session cookie
/// there and every cookie it holds for `host` is a `__Host-` cookie of
/// that host alone, `Secure`, `HttpOnly`, `Path=/` ("Sign-in and sessions:
/// The cookies", with `--public-scheme https`). Returns what it held.
fn assert_host_cookies(browser: &mut Firefox, host: &str) -> Vec<String> {
    let held = cookies_here(browser);
    let described: Vec<String> = held.iter().map(shown).collect();
    eprintln!("the browser holds for {host}: {described:?}");
    assert!(
        held.iter().any(|c| c["name"] == "__Host-vm-session"),
        "expected the browser to hold the session cookie `__Host-vm-session` for {host}; it holds {described:?}"
    );
    for cookie in &held {
        let ok = cookie["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("__Host-"))
            && cookie["secure"] == true
            && cookie["httpOnly"] == true
            && cookie["path"] == "/"
            && cookie["domain"] == host;
        assert!(
            ok,
            "expected every cookie the browser holds for {host} to be a `__Host-` cookie of {host} alone, Secure, HttpOnly, Path=/; this one is {}",
            shown(cookie)
        );
    }
    described
}

#[test]
fn the_browser_asks_for_the_dashboard_is_sent_to_the_provider_and_back_and_is_on_it_signed_in() {
    firefox::required();
    let site = site();
    let mut browser = site.browser(&[]);
    let before = authorizations(site.provider());
    browser.navigate(&site.dashboard("/"));
    assert!(
        at_provider(&mut browser, site.provider()),
        "expected the browser, with no session, sent to the identity provider\n{}",
        browser.describe()
    );
    assert_eq!(
        authorizations(site.provider()),
        before + 1,
        "expected one authorization request at the provider"
    );
    choose_at_provider(&mut browser, site.provider(), ADA);
    let landed = browser.wait_for_page("the Dashboard", |u| on_site(u, &site, "dashboard"));
    assert_eq!(
        url::Url::parse(&landed).unwrap().path(),
        "/",
        "expected to land on the Dashboard's `/`, got {landed}"
    );
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    let user = browser.the("[data-vm-user]");
    assert_eq!(
        browser.attribute(&user, "data-vm-user").as_deref(),
        Some(ADA),
        "expected the Dashboard to say Ada is signed in\n{}",
        browser.describe()
    );
    let text = browser.text_of(&user);
    assert!(
        text.contains("Ada Lovelace"),
        "expected her name in the element that says who is signed in, got {text:?}"
    );

    // What the browser itself holds: the Dashboard's session on its own
    // address, the platform session on the sign-in address.
    assert_host_cookies(&mut browser, &Site::host("dashboard"));
    browser.navigate(&site.auth("/me"));
    assert_eq!(
        page_json(&mut browser)["id"],
        ADA,
        "expected the platform session to be Ada's\n{}",
        browser.describe()
    );
    assert_host_cookies(&mut browser, &Site::host("auth"));
}

#[test]
fn a_click_on_an_applications_link_shows_it_for_that_person_without_asking_the_provider_again() {
    firefox::required();
    let site = site();
    site.application_of(ADA, APP, Some("The incident board"));
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    let asked = authorizations(site.provider());
    let link = wait_for_element(
        &mut browser,
        &format!("[data-vm-application=\"{APP}\"] a[data-vm-open]"),
    );
    browser.click(&link);
    browser.wait_for_page("the Application's page", |u| on_site(u, &site, APP));
    let shown = page_json(&mut browser);
    assert_eq!(
        shown["user"]["id"],
        ADA,
        "expected the Application's page answered for Ada\n{}",
        browser.describe()
    );
    assert_eq!(
        authorizations(site.provider()),
        asked,
        "expected the identity provider not asked again when opening the Application"
    );
    assert_host_cookies(&mut browser, &Site::host(APP));
}

#[test]
fn signing_out_by_the_form_sends_the_dashboard_and_an_application_opened_before_to_sign_in_again() {
    firefox::required();
    let site = site();
    site.application_of(ADA, APP, None);
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    browser.navigate(&site.application(APP, "/"));
    assert_eq!(page_json(&mut browser)["user"]["id"], ADA);

    browser.navigate(&site.auth("/sign-out"));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    let buttons = browser
        .find_all("form[method=\"post\" i] button, form[method=\"post\" i] input[type=\"submit\"]");
    assert_eq!(
        buttons.len(),
        1,
        "expected the sign-out page to have one button\n{}",
        browser.describe()
    );
    blackbox::walks::click_to_another_page(&mut browser, &buttons[0]);
    assert!(
        on_site(&browser.url(), &site, "auth"),
        "{}",
        browser.describe()
    );
    assert_eq!(
        browser.status(),
        Some(200),
        "expected the sign-out taken (200) when the browser posts its form\n{}",
        browser.describe()
    );
    let said = browser.text().to_lowercase();
    assert!(
        said.contains("signed out"),
        "expected the page to say the person is signed out\n{}",
        browser.describe()
    );

    // The provider now asks who you are every time, so that being sent to
    // sign in shows as the provider's page.
    site.provider().ask_who_every_time(true);
    for (what, address) in [
        ("the Dashboard", site.dashboard("/")),
        ("the Application opened before", site.application(APP, "/")),
    ] {
        let before = authorizations(site.provider());
        browser.navigate(&address);
        assert!(
            at_provider(&mut browser, site.provider()),
            "after signing out, expected {what} to send the browser to sign in again\n{}",
            browser.describe()
        );
        assert_eq!(
            authorizations(site.provider()),
            before + 1,
            "after signing out, expected {what} to ask the provider"
        );
    }
}
