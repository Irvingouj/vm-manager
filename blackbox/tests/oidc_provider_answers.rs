//! What the identity provider answers, and what the program makes of it
//! (CONTRACT.md, "Sign-in and sessions: The way through sign-in", step 3,
//! and "What the program's log says"): `408`, `429` and `5xx` are no
//! refusal; a configuration or keys that cannot be used; the log's
//! `identity-provider-unanswered` and `identity-provider-refused`; an ID
//! token with times in fractions of a second.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::VmOptions;
use blackbox::oidc::{CLIENT_ID, ConfigFault, Provider, TokenAnswer, TokenFault, TokenStatus};
use blackbox::sessions::{ADA, Installation, Link, SESSION_COOKIE, ada, assert_failure_page};
use serde_json::{Value, json};

fn installation() -> Installation {
    Installation::start(&[("honest", "honest.js")])
}

/// Steps 1 to 3 and the callback with a fresh browser; fails unless the
/// callback is a failure page with this status linking to honest.
fn assert_callback_fails(at: &Installation, status: u16, context: &str) {
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let callback = browser.follow(steps.last().unwrap());
    let mut hidden = at.provider.issued();
    hidden.push(at.provider.client_secret().expose().to_owned());
    assert_failure_page(
        &callback.response,
        &[status],
        Link::To(&at.app_origin("honest")),
        &hidden,
        context,
    );
    assert!(
        browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none(),
        "{context}: no session begins"
    );
}

/// Step 1 and step 2 with a fresh browser; fails unless step 2 is a `502`
/// page linking to honest.
fn assert_step_two_fails(at: &Installation, context: &str) {
    let mut browser = at.browser_as(ADA);
    let first = browser.open(&at.app_url("honest", "/"));
    let second = browser.follow(&first);
    assert_failure_page(
        &second.response,
        &[502],
        Link::To(&at.app_origin("honest")),
        &[],
        context,
    );
}

fn unanswered(call: &str, problem: &str) -> [(&'static str, Value); 2] {
    [("call", json!(call)), ("problem", json!(problem))]
}

fn status(code: u16, error: &str, description: &str) -> TokenStatus {
    TokenStatus {
        status: code,
        error: error.to_owned(),
        description: description.to_owned(),
    }
}

#[test]
fn a_408_429_or_5xx_at_the_callback_is_a_502_page_and_logged_as_unanswered() {
    let at = installation();
    for code in [408, 429, 500, 503] {
        at.provider
            .token_status(Some(status(code, "temporarily_unavailable", "later")));
        assert_callback_fails(&at, 502, &format!("the token endpoint answers {code}"));
        at.assert_logged(
            "identity-provider-unanswered",
            &[
                ("call", json!("token")),
                ("problem", json!("status")),
                ("status", json!(code)),
            ],
        );
    }
    assert!(
        at.logged("identity-provider-refused").is_empty(),
        "408, 429 and 5xx are no refusal: {:?}",
        at.logged("identity-provider-refused")
    );
}

#[test]
fn a_refusal_at_the_callback_is_logged_with_its_error_and_never_its_description() {
    let at = installation();
    let description = "harness-description-Pw4kT2-never-logged";
    at.provider
        .token_status(Some(status(400, "invalid_grant", description)));
    assert_callback_fails(&at, 400, "the token endpoint refuses the code");
    at.assert_logged(
        "identity-provider-refused",
        &[("status", json!(400)), ("error", json!("invalid_grant"))],
    );
    at.assert_none_in_log(
        &[description.to_owned()],
        "the provider's error_description",
    );
}

#[test]
fn a_configuration_naming_another_issuer_is_a_502_page_at_step_two() {
    let at = installation();
    at.provider.config_fault(Some(ConfigFault::AnotherIssuer));
    assert_step_two_fails(&at, "a configuration naming another issuer");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("configuration", "another-issuer"),
    );
}

#[test]
fn a_configuration_without_a_token_endpoint_is_a_502_page_at_step_two() {
    let at = installation();
    at.provider
        .config_fault(Some(ConfigFault::LacksTokenEndpoint));
    assert_step_two_fails(&at, "a configuration without a token endpoint");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("configuration", "lacks-endpoint"),
    );
}

#[test]
fn a_configuration_larger_than_a_mebibyte_is_a_502_page_at_step_two() {
    let at = installation();
    at.provider.config_fault(Some(ConfigFault::Oversize));
    assert_step_two_fails(&at, "a configuration of more than 1 MiB");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("configuration", "too-large"),
    );
}

#[test]
fn a_provider_that_cannot_be_reached_is_logged_as_unreachable() {
    let provider = Provider::start(&[ada()]);
    let options = VmOptions::new()
        .args_extra(&[
            "--oidc-issuer",
            &Provider::unreachable_issuer(),
            "--oidc-client-id",
            CLIENT_ID,
            "--public-scheme",
            "http",
        ])
        .env("VM_OIDC_CLIENT_SECRET", provider.client_secret().expose())
        .application("honest", "honest.js");
    let at = Installation::from(provider, &options);
    assert_step_two_fails(&at, "a provider that cannot be reached");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("configuration", "unreachable"),
    );
}

#[test]
fn keys_that_are_no_jwks_are_a_502_page_at_the_callback() {
    let at = installation();
    at.provider.unreadable_jwks(true);
    assert_callback_fails(&at, 502, "keys that cannot be read");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("keys", "unreadable-keys"),
    );
}

#[test]
fn keys_larger_than_a_mebibyte_are_logged_as_too_large() {
    let at = installation();
    at.provider.oversize_jwks(true);
    assert_callback_fails(&at, 502, "keys of more than 1 MiB");
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("keys", "too-large"),
    );
}

#[test]
fn token_answers_that_cannot_be_used_are_502_pages_and_logged_by_problem() {
    let at = installation();
    for (answer, problem) in [
        (TokenAnswer::Oversize, "too-large"),
        (TokenAnswer::NotJson, "not-json"),
        (TokenAnswer::WithoutIdToken, "no-id-token"),
    ] {
        at.provider.token_answer(answer);
        assert_callback_fails(&at, 502, &format!("a token answer: {answer:?}"));
        at.assert_logged(
            "identity-provider-unanswered",
            &unanswered("token", problem),
        );
    }
}

#[test]
fn a_token_endpoint_that_takes_too_long_is_logged_as_timed_out() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    at.provider.token_delay(Duration::from_secs(20));
    let callback = browser.follow(steps.last().unwrap());
    assert_eq!(
        callback.response.status,
        502,
        "{}",
        callback.response.describe()
    );
    at.assert_logged(
        "identity-provider-unanswered",
        &unanswered("token", "timed-out"),
    );
}

#[test]
fn the_log_of_the_provider_holds_nothing_of_a_token_a_code_a_cookie_or_the_secret() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    at.provider.token_answer(TokenAnswer::WithoutIdToken);
    assert_callback_fails(&at, 502, "a token answer without an ID token");
    let mut hidden = at.provider.issued();
    hidden.push(at.provider.client_secret().expose().to_owned());
    for host in [at.auth_host(), format!("honest.{}", blackbox::DOMAIN)] {
        hidden.push(at.cookie_value(&browser, &host, SESSION_COOKIE));
    }
    at.assert_none_in_log(&hidden, "after a sign-in and a failed one");
}

#[test]
fn an_id_token_with_times_in_fractions_of_a_second_is_taken() {
    let at = installation();
    at.provider.set_fault(Some(TokenFault::FractionalTimes));
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}
