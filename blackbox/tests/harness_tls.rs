//! The harness's own TLS parts (src/tls.rs), without the program: a plain
//! client of the harness reaches a plain server of the harness through a
//! TLS front when it trusts the front's authority, and is refused when it
//! trusts another, or the certificate is for another name or expired; the
//! front records what it saw; the identity provider behind its front
//! answers its configuration over `https://localhost:<port>`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::DOMAIN;
use blackbox::tls::{
    Authority, Front, Handshake, PROVIDER_HOST, api_name, provider_behind_tls, tls_get,
};
use blackbox::tool::NotAnInstallation;

/// The plain server of the harness behind the fronts: it answers every
/// request with this text.
const ITS_ANSWER: &str = "this is not an installation";

#[test]
fn a_client_trusting_the_fronts_authority_reaches_the_plain_server_behind_it() {
    let server = NotAnInstallation::start();
    let a = Authority::new("Harness authority A");
    let front = Front::tls(
        &a.issue(&[&api_name(), &format!("*.{DOMAIN}")]),
        server.address(),
    );
    let host = format!("{}:{}", api_name(), front.port());
    let answer = tls_get(front.address(), &api_name(), &[&a], "/v1/me", &host)
        .unwrap_or_else(|e| panic!("expected the client to reach the server, got {e}"));
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with(ITS_ANSWER), "{answer}");
    let seen = front.settled();
    assert_eq!(seen.len(), 1, "{}", front.describe());
    assert_eq!(seen[0].handshake, Handshake::Completed);
    assert_eq!(seen[0].server_name.as_deref(), Some(api_name().as_str()));
    assert_eq!(front.request_lines(), vec!["GET /v1/me HTTP/1.1"]);
    assert_eq!(front.hosts(), vec![host]);
    // A wildcard name of the same certificate.
    let other = format!("board.{DOMAIN}");
    tls_get(front.address(), &other, &[&a], "/", &other)
        .unwrap_or_else(|e| panic!("expected *.{DOMAIN} to be taken, got {e}"));
}

/// Fails unless the client is refused and the front completed no
/// handshake and passed nothing on.
fn assert_refused(front: &Front, result: Result<String, String>, what: &str) -> String {
    let error = match result {
        Ok(answer) => panic!("expected {what} to be refused, got {answer}"),
        Err(e) => e,
    };
    let seen = front.settled();
    assert_eq!(seen.len(), 1, "{}", front.describe());
    assert!(
        matches!(seen[0].handshake, Handshake::Failed(_)),
        "expected the front's handshake to fail, {}",
        front.describe()
    );
    assert!(seen[0].received.is_empty(), "{}", front.describe());
    error
}

#[test]
fn a_client_trusting_only_another_authority_is_refused_and_nothing_is_passed_on() {
    let server = NotAnInstallation::start();
    let a = Authority::new("Harness authority A");
    let b = Authority::new("Harness authority B");
    let front = Front::tls(&a.issue(&[&api_name()]), server.address());
    let error = assert_refused(
        &front,
        tls_get(front.address(), &api_name(), &[&b], "/", &api_name()),
        "a client trusting only B",
    );
    assert!(error.to_lowercase().contains("issuer"), "{error}");
}

#[test]
fn a_certificate_for_another_name_is_refused_by_the_client() {
    let server = NotAnInstallation::start();
    let a = Authority::new("Harness authority A");
    let front = Front::tls(&a.issue(&["api.other.test"]), server.address());
    let error = assert_refused(
        &front,
        tls_get(front.address(), &api_name(), &[&a], "/", &api_name()),
        "a certificate for another name",
    );
    assert!(error.to_lowercase().contains("name"), "{error}");
}

#[test]
fn an_expired_certificate_is_refused_by_the_client() {
    let server = NotAnInstallation::start();
    let a = Authority::new("Harness authority A");
    let front = Front::tls(&a.issue_expired(&[&api_name()]), server.address());
    let error = assert_refused(
        &front,
        tls_get(front.address(), &api_name(), &[&a], "/", &api_name()),
        "an expired certificate",
    );
    assert!(error.to_lowercase().contains("expired"), "{error}");
}

#[test]
fn a_replaced_identity_counts_from_the_next_connection() {
    let server = NotAnInstallation::start();
    let a = Authority::new("Harness authority A");
    let b = Authority::new("Harness authority B");
    let front = Front::tls(&a.issue(&[&api_name()]), server.address());
    tls_get(front.address(), &api_name(), &[&a], "/", &api_name()).unwrap();
    front.replace_identity(&b.issue(&[&api_name()]));
    assert!(tls_get(front.address(), &api_name(), &[&a], "/", &api_name()).is_err());
    let seen = front.settled();
    assert_eq!(seen.len(), 2, "{}", front.describe());
    assert_eq!(seen[0].handshake, Handshake::Completed);
    assert!(matches!(seen[1].handshake, Handshake::Failed(_)));
}

#[test]
fn a_plain_front_records_the_bytes_as_they_came() {
    let server = NotAnInstallation::start();
    let front = Front::plain(server.address());
    let a = Authority::new("Harness authority A");
    // A TLS client against a plain front: the front passes its ClientHello
    // on, and the server's plain answer ends the handshake.
    assert!(tls_get(front.address(), &api_name(), &[&a], "/", &api_name()).is_err());
    let seen = front.settled();
    assert_eq!(seen.len(), 1, "{}", front.describe());
    assert_eq!(seen[0].handshake, Handshake::Plain);
    assert_eq!(seen[0].first_bytes.first(), Some(&0x16), "a TLS record");
    assert!(front.request_lines().is_empty(), "{}", front.describe());
}

#[test]
fn the_identity_provider_behind_its_front_answers_its_configuration_over_https() {
    let a = Authority::new("Harness authority A");
    let (provider, front) = provider_behind_tls(&[], &a);
    let issuer = provider.issuer().to_owned();
    assert_eq!(issuer, format!("https://{PROVIDER_HOST}:{}", front.port()));
    let host = format!("{PROVIDER_HOST}:{}", front.port());
    let answer = tls_get(
        front.address(),
        PROVIDER_HOST,
        &[&a],
        "/.well-known/openid-configuration",
        &host,
    )
    .unwrap_or_else(|e| panic!("expected the configuration over TLS, got {e}"));
    let body = answer.split("\r\n\r\n").nth(1).unwrap_or_default();
    let configuration: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("expected JSON ({e}), got {answer}"));
    assert_eq!(configuration["issuer"], issuer.as_str());
    assert_eq!(configuration["token_endpoint"], format!("{issuer}/token"));
    assert_eq!(configuration["jwks_uri"], format!("{issuer}/jwks"));
    assert_eq!(
        configuration["authorization_endpoint"],
        format!("http://127.0.0.1:{}/authorize", provider.address().port())
    );
    let b = Authority::new("Harness authority B");
    assert!(
        tls_get(
            front.address(),
            PROVIDER_HOST,
            &[&b],
            "/.well-known/openid-configuration",
            &host
        )
        .is_err()
    );
}
