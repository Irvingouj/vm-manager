//! The identity provider's certificate for another name, expired, or not
//! valid yet (CONTRACT.md, "The tool reaches an installation over HTTPS: A
//! company's own certificate authority", the bullet "The identity
//! provider"; ticket "The tool reaches an installation over HTTPS, and a
//! company's own certificate authority is trusted").
//!
//! The harness's provider is behind a TLS front with the issuer
//! `https://localhost:<port>`; its certificate is issued under a test
//! authority A that `vibe serve` trusts through `VIBE_CA_FILE`, so that the
//! one fault is the certificate's name or time. `PUT /v1/oidc` is `422`
//! `identity-provider-unusable`, the message says which it was, and nothing
//! is connected. (The authority not known: `ca_file.rs`.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::assert_api_error;
use blackbox::connection::Connection;
use blackbox::sessions::{Installation, ada, oidc_options};
use blackbox::tls::{Authority, Files, Identity, provider_behind_tls_with, text_of};

/// Fails unless connecting to the provider whose certificate `issue` makes
/// under A, with A in `VIBE_CA_FILE`, is `422` `identity-provider-unusable`
/// saying `words`, with nothing connected and nothing asked of the
/// provider.
fn assert_refused(issue: impl FnOnce(&Authority, &[&str]) -> Identity, words: &str) {
    let a = Authority::new("Harness authority A");
    let files = Files::new();
    let ca = text_of(&files.authorities("a.pem", &[&a]));
    let (provider, front) = provider_behind_tls_with(&[ada()], |names| issue(&a, names));
    let at = Installation::unconnected(provider, &oidc_options().env("VIBE_CA_FILE", &ca));
    let answer = at.connect_through_the_api(&Connection::to(&at.provider));
    assert_api_error(&answer, 422, "identity-provider-unusable");
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains(words),
        "expected the message to say {words:?}, got {message:?}"
    );
    assert_eq!(
        at.vm.api_request("/v1/installation").send().json()["sign_in"],
        "none",
        "expected nothing to be connected"
    );
    assert!(
        front.request_lines().is_empty(),
        "expected nothing asked of the provider\n{}",
        front.describe()
    );
}

#[test]
fn a_provider_certificate_for_another_name_than_the_issuers_is_unusable_saying_so() {
    assert_refused(|a, _| a.issue(&["idp.other.test"]), "another name");
}

#[test]
fn an_expired_provider_certificate_is_unusable_saying_so() {
    assert_refused(|a, names| a.issue_expired(names), "expired");
}

#[test]
fn a_provider_certificate_not_valid_yet_is_unusable_saying_so() {
    assert_refused(|a, names| a.issue_not_valid_yet(names), "not valid yet");
}
