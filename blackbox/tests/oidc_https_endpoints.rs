//! An endpoint's scheme follows the issuer's (CONTRACT.md, "Connecting
//! OIDC", the bullet "The provider is asked before the connection is
//! taken"; ticket "The tool reaches an installation over HTTPS, and a
//! company's own certificate authority is trusted").
//!
//! The harness's identity provider is behind a TLS front with the issuer
//! `https://localhost:<port>`, its certificate under a test authority that
//! `vibe serve` trusts through `VIBE_CA_FILE`. Its configuration then names
//! one endpoint (authorization, token, keys) as `http` on its own loopback
//! address, where it does answer: `PUT /v1/oidc` is `422`
//! `identity-provider-unusable` and changes nothing; and a connection made
//! while the configuration was good, after a restart, ends a sign-in with
//! the `502` page, logged as `lacks-endpoint`. (The plain case, an `http`
//! loopback issuer with `http` endpoints, is
//! `oidc_connection::connecting_from_the_tool_with_the_setup_credential_takes_effect_at_once`.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::assert_api_error;
use blackbox::connection::Connection;
use blackbox::oidc::{ConfigFault, Endpoint};
use blackbox::sessions::{ADA, Installation, Link, ada, assert_failure_page, oidc_options};
use blackbox::tls::{Authority, Files, Front, provider_behind_tls, text_of};
use serde_json::json;

/// The provider behind TLS under a fresh authority, and a program (the
/// fixture `honest` installed) trusting that authority through
/// `VIBE_CA_FILE`, OIDC not connected. The files and the front live as
/// long as the test holds them.
fn beside_https_provider() -> (Installation, Front, Files) {
    let a = Authority::new("Harness authority A");
    let files = Files::new();
    let ca = files.authorities("a.pem", &[&a]);
    let (provider, front) = provider_behind_tls(&[ada()], &a);
    let options = oidc_options()
        .application("honest", "honest.js")
        .env("VIBE_CA_FILE", &text_of(&ca));
    (Installation::unconnected(provider, &options), front, files)
}

/// Fails unless connecting while the configuration names `endpoint` as
/// plain `http` is `422` `identity-provider-unusable` and changes nothing.
fn assert_plain_endpoint_unusable(endpoint: Endpoint) {
    let (at, _front, _files) = beside_https_provider();
    at.provider
        .config_fault(Some(ConfigFault::PlainEndpoint(endpoint)));
    let answer = at.connect_through_the_api(&Connection::to(&at.provider));
    assert_api_error(&answer, 422, "identity-provider-unusable");
    let installation = at.vm.api_request("/v1/installation").send();
    assert_eq!(
        installation.json()["sign_in"],
        "none",
        "{endpoint:?}: expected nothing to be connected\n{}",
        installation.describe()
    );
}

#[test]
fn an_http_authorization_endpoint_under_an_https_issuer_is_unusable() {
    assert_plain_endpoint_unusable(Endpoint::Authorization);
}

#[test]
fn an_http_token_endpoint_under_an_https_issuer_is_unusable() {
    assert_plain_endpoint_unusable(Endpoint::Token);
}

#[test]
fn an_http_jwks_uri_under_an_https_issuer_is_unusable() {
    assert_plain_endpoint_unusable(Endpoint::Keys);
}

#[test]
fn an_http_endpoint_named_after_the_connection_ends_a_sign_in_with_502_logged_as_lacks_endpoint() {
    let (at, front, files) = beside_https_provider();
    at.connect_through_the_api(&Connection::to(&at.provider))
        .expect_status(200);
    at.provider
        .config_fault(Some(ConfigFault::PlainEndpoint(Endpoint::Authorization)));
    // Started again, so that no configuration kept from before is used.
    let at = at.restarted();
    let mut browser = at.browser_as(ADA);
    let first = browser.open(&at.app_url("honest", "/"));
    assert!(first.is_redirect(), "{}", first.response.describe());
    let second = browser.follow(&first);
    assert_failure_page(
        &second.response,
        &[502],
        Link::To(&at.app_origin("honest")),
        &[],
        "a configuration naming an http authorization endpoint under an https issuer",
    );
    at.assert_logged(
        "identity-provider-unanswered",
        &[
            ("call", json!("configuration")),
            ("problem", json!("lacks-endpoint")),
        ],
    );
    drop((front, files));
}
