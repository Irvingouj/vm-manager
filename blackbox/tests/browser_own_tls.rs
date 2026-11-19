//! One walk of "A real browser in the gate" against the program's own TLS
//! (CONTRACT.md, "The program serves TLS itself: What is the same as
//! behind a proxy": "A browser walks it: one of the walks ... (sign-in,
//! the Dashboard, an Application opened from it) is run against the
//! program's own TLS, with no front of the harness between them").
//!
//! Firefox, a fresh profile trusting the test's authority, reaches the
//! program itself ([`Site::own_tls`]): it signs in through the identity
//! provider, lands on the Dashboard, opens an Application from it without
//! the provider being asked again, and holds only `__Host-` cookies,
//! `Secure`, for each name, as `browser_sign_in.rs` asserts them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::firefox::{self, Firefox};
use blackbox::sessions::{ADA, ada};
use blackbox::walks::{
    Site, at_provider, authorizations, choose_at_provider, on_site, page_json, wait_for_element,
};
use serde_json::Value;

const APP: &str = "board";

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
/// that host alone, `Secure`, `HttpOnly`, `Path=/`.
fn assert_host_cookies(browser: &mut Firefox, host: &str) {
    let held = browser
        .must("WebDriver:GetCookies", &serde_json::json!({}))
        .as_array()
        .cloned()
        .unwrap_or_default();
    let described: Vec<String> = held.iter().map(shown).collect();
    eprintln!("the browser holds for {host}: {described:?}");
    assert!(
        held.iter().any(|c| c["name"] == "__Host-vm-session"),
        "expected the browser to hold `__Host-vm-session` for {host}; it holds {described:?}"
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
            "expected every cookie for {host} to be a `__Host-` cookie of {host} alone, Secure, HttpOnly, Path=/; this one is {}",
            shown(cookie)
        );
    }
}

#[test]
fn a_person_signs_in_lands_on_the_dashboard_and_opens_an_application_over_the_programs_own_tls() {
    firefox::required();
    let site = Site::own_tls(&[ada()]);
    assert!(
        site.front.is_none(),
        "no front between the browser and the program"
    );
    site.application_of(ADA, APP, Some("The incident board"));
    let mut browser = site.browser(&[APP]);

    // Sign-in: to the provider and back, onto the Dashboard.
    let before = authorizations(site.provider());
    browser.navigate(&site.dashboard("/"));
    assert!(
        at_provider(&mut browser, site.provider()),
        "expected the browser, with no session, sent to the identity provider\n{}",
        browser.describe()
    );
    assert_eq!(authorizations(site.provider()), before + 1);
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
    assert_host_cookies(&mut browser, &Site::host("dashboard"));

    // An Application opened from it, the provider not asked again.
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

    // The platform session, on the sign-in address.
    browser.navigate(&site.auth("/me"));
    assert_eq!(page_json(&mut browser)["id"], ADA, "{}", browser.describe());
    assert_host_cookies(&mut browser, &Site::host("auth"));
}
