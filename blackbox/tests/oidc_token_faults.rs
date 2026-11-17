//! An ID token that does not hold, and a provider that answers what it
//! should not (CONTRACT.md, "Sign-in and sessions: The way through
//! sign-in", step 3, and "Starting with OIDC" on the provider's keys and
//! answers): no session begins, the browser gets a page of the platform's
//! own with a way to try again, and nothing of a token is shown or logged.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::oidc::{Key, Outage, TokenAnswer, TokenFault};
use blackbox::sessions::{ADA, Installation, SESSION_COOKIE, assert_failure_page};

fn installation() -> Installation {
    Installation::start(&[("honest", "honest.js")])
}

/// Goes through steps 1 to 3 and the callback with a fresh browser, and
/// fails unless the callback is a failure page with one of `statuses`
/// and no session began.
fn assert_sign_in_fails(at: &Installation, statuses: &[u16], context: &str) {
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let callback = browser.follow(steps.last().unwrap());
    let mut hidden = at.provider.issued();
    hidden.push(at.provider.client_secret().expose().to_owned());
    assert_failure_page(&callback.response, statuses, &hidden, context);
    assert!(
        browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none(),
        "{context}: expected no platform session"
    );
    let me = at.me(&mut browser);
    assert_eq!(
        me.status,
        401,
        "{context}: /me without a session\n{}",
        me.describe()
    );
    at.assert_no_session(&mut browser, "honest", context);
}

#[test]
fn an_id_token_that_does_not_hold_begins_no_session_and_shows_nothing_of_it() {
    let at = installation();
    let faults = [
        (TokenFault::BadSignature, "a signature that does not verify"),
        (TokenFault::WrongNonce, "another nonce"),
        (TokenFault::WrongAudience, "another audience"),
        (TokenFault::WrongIssuer, "another issuer"),
        (TokenFault::ExpiresIn(-120), "expired two minutes ago"),
        (TokenFault::AlgNone, "alg none"),
        (
            TokenFault::Hs256WithSecret,
            "HS256 keyed with the client secret",
        ),
    ];
    for (fault, context) in faults {
        at.provider.set_fault(Some(fault));
        // A token that does not hold: `400`.
        assert_sign_in_fails(&at, &[400], context);
    }
    at.provider.set_fault(None);
    let mut hidden = at.provider.issued();
    hidden.push(at.provider.client_secret().expose().to_owned());
    at.assert_none_in_log(&hidden, "after tokens that do not hold");
    // The provider is fine again: a sign-in works.
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
}

#[test]
fn an_id_token_signed_with_a_key_not_in_the_jwks_begins_no_session() {
    let at = installation();
    at.provider.sign_with(Key::Other);
    assert_sign_in_fails(&at, &[400], "a key the provider does not publish");
}

#[test]
fn an_unknown_key_fetches_the_keys_again_at_most_once_a_minute() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    at.provider.sign_with(Key::Other);
    for round in 0..3 {
        assert_sign_in_fails(&at, &[400], &format!("an unknown key, try {round}"));
    }
    let fetched = at.provider.requests_to("/jwks").len();
    assert!(
        (1..=2).contains(&fetched),
        "expected the keys fetched once, and again at most once within a minute for an unknown key; they were fetched {fetched} times"
    );
}

#[test]
fn a_new_key_is_taken_once_a_minute_has_passed_since_the_keys_were_fetched() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    at.provider.publish_other_key(true);
    at.provider.sign_with(Key::Other);
    std::thread::sleep(Duration::from_secs(62));
    let mut later = at.browser_as(ADA);
    at.sign_in(&mut later, "honest", "/");
}

#[test]
fn an_id_token_expired_within_the_allowed_clock_difference_is_taken() {
    let at = installation();
    at.provider.set_fault(Some(TokenFault::ExpiresIn(-10)));
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
}

#[test]
fn keys_larger_than_a_mebibyte_are_a_502_page() {
    let at = installation();
    at.provider.oversize_jwks(true);
    assert_sign_in_fails(&at, &[502], "a JWKS of more than 1 MiB");
}

#[test]
fn a_provider_that_closes_the_connection_at_the_callback_is_a_502_page() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    at.provider.set_outage(Outage::Close);
    let callback = browser.follow(steps.last().unwrap());
    assert_failure_page(
        &callback.response,
        &[502],
        &at.provider.issued(),
        "a provider that closes the connection",
    );
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
}

#[test]
fn a_token_answer_without_an_id_token_or_not_json_is_a_502_page() {
    let at = installation();
    for (answer, context) in [
        (
            TokenAnswer::WithoutIdToken,
            "a token answer without an ID token",
        ),
        (TokenAnswer::NotJson, "a token answer that is not JSON"),
    ] {
        at.provider.token_answer(answer);
        assert_sign_in_fails(&at, &[502], context);
    }
}

#[test]
fn a_code_the_provider_refuses_is_a_400_page() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    // Disabled between the authorization and the exchange: the provider
    // answers `invalid_grant` to the code.
    at.provider.disable(ADA);
    let callback = browser.follow(steps.last().unwrap());
    assert_failure_page(
        &callback.response,
        &[400],
        &at.provider.issued(),
        "a code the provider refuses",
    );
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
}
