//! Signing in, opening an Application from the Dashboard, and signing out,
//! walked in a real browser (CONTRACT.md, "A real browser in the gate:
//! What it walks", 1, 2 and 5; "Sign-in and sessions"; "The Dashboard:
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

use blackbox::DOMAIN;
use blackbox::firefox;
use blackbox::sessions::{ADA, Installation, ada, grace};
use blackbox::walks::{
    application, application_of, at_provider, auth, authorizations, browser_for, dashboard, on,
    page_json, sign_in_to_dashboard, wait_for_element,
};

const APP: &str = "board";

fn installation() -> Installation {
    Installation::start_with(&[], &[ada(), grace()], &[])
}

#[test]
fn the_browser_asks_for_the_dashboard_is_sent_to_the_provider_and_back_and_is_on_it_signed_in() {
    firefox::required();
    let at = installation();
    let mut browser = browser_for(&[]);
    let before = authorizations(&at.provider);
    browser.navigate(&dashboard(&at, "/"));
    assert!(
        at_provider(&mut browser, &at.provider),
        "expected the browser, with no session, sent to the identity provider\n{}",
        browser.describe()
    );
    assert_eq!(
        authorizations(&at.provider),
        before + 1,
        "expected one authorization request at the provider"
    );
    blackbox::walks::choose_at_provider(&mut browser, &at.provider, ADA);
    let landed = browser.wait_for_page("the Dashboard", |u| {
        on(u, &format!("dashboard.{DOMAIN}"), at.port())
    });
    assert!(
        landed.ends_with('/') && url::Url::parse(&landed).unwrap().path() == "/",
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
}

#[test]
fn a_click_on_an_applications_link_shows_it_for_that_person_without_asking_the_provider_again() {
    firefox::required();
    let at = installation();
    application_of(&at, ADA, APP, Some("The incident board"));
    let mut browser = browser_for(&[APP]);
    sign_in_to_dashboard(&mut browser, &at, ADA);
    let asked = authorizations(&at.provider);
    let link = wait_for_element(
        &mut browser,
        &format!("[data-vm-application=\"{APP}\"] a[data-vm-open]"),
    );
    browser.click(&link);
    browser.wait_for_page("the Application's page", |u| {
        on(u, &format!("{APP}.{DOMAIN}"), at.port())
    });
    let shown = page_json(&mut browser);
    assert_eq!(
        shown["user"]["id"],
        ADA,
        "expected the Application's page answered for Ada\n{}",
        browser.describe()
    );
    assert_eq!(
        authorizations(&at.provider),
        asked,
        "expected the identity provider not asked again when opening the Application"
    );
}

#[test]
fn signing_out_by_the_form_sends_the_dashboard_and_an_application_opened_before_to_sign_in_again() {
    firefox::required();
    let at = installation();
    application_of(&at, ADA, APP, None);
    let mut browser = browser_for(&[APP]);
    sign_in_to_dashboard(&mut browser, &at, ADA);
    browser.navigate(&application(&at, APP, "/"));
    assert_eq!(page_json(&mut browser)["user"]["id"], ADA);

    browser.navigate(&auth(&at, "/sign-out"));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    let buttons = browser
        .find_all("form[method=\"post\" i] button, form[method=\"post\" i] input[type=\"submit\"]");
    assert_eq!(
        buttons.len(),
        1,
        "expected the sign-out page to have one button\n{}",
        browser.describe()
    );
    browser.click(&buttons[0]);
    browser.wait_for_page("the page after signing out", |u| {
        on(u, &format!("auth.{DOMAIN}"), at.port())
    });
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
    at.provider.ask_who_every_time(true);
    for (what, address) in [
        ("the Dashboard", dashboard(&at, "/")),
        ("the Application opened before", application(&at, APP, "/")),
    ] {
        let before = authorizations(&at.provider);
        browser.navigate(&address);
        assert!(
            at_provider(&mut browser, &at.provider),
            "after signing out, expected {what} to send the browser to sign in again\n{}",
            browser.describe()
        );
        assert_eq!(
            authorizations(&at.provider),
            before + 1,
            "after signing out, expected {what} to ask the provider"
        );
    }
}
