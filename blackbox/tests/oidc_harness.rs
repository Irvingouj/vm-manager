//! The harness's own identity provider and browser (CONTRACT.md, "Sign-in
//! and sessions: The harness"), tried without the program: they do what the
//! tests of sign-in rely on them to do. These pass today.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use blackbox::Request;
use blackbox::browser::{Browser, query_param, url};
use blackbox::http::Response;
use blackbox::oidc::{
    CLIENT_ID, ConfigFault, KID_EC, KID_MAIN, KID_OTHER, Key, Outage, Person, Provider,
    TokenAnswer, TokenFault, TokenStatus, claims_of, es256_verifies, header_of, rs256_verifies,
    s256,
};
use serde_json::{Value, json};
use url::Url;

const REDIRECT: &str = "http://auth.vm.test:4000/callback";
const VERIFIER: &str = "a-code-verifier-of-the-harness-that-is-long-enough-0123456789";

fn ada() -> Person {
    Person::new("ada-sub")
        .named("Ada")
        .email("ada@example.test")
        .in_groups(&["finance"])
}

fn get(provider: &Provider, target: &str) -> Response {
    Request::new(
        provider.address(),
        target,
        &format!("127.0.0.1:{}", provider.address().port()),
    )
    .send()
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn basic(secret: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{CLIENT_ID}:{secret}")))
}

fn token(provider: &Provider, pairs: &[(&str, &str)], basic_auth: bool) -> Response {
    let mut request = Request::new(provider.address(), "/token", "127.0.0.1")
        .method("POST")
        .header("content-type", "application/x-www-form-urlencoded");
    if basic_auth {
        request = request.header("authorization", basic(provider.client_secret().expose()));
    }
    request.body(form(pairs)).send()
}

/// Goes through the authorization endpoint with a browser signed in as
/// `sub` and returns the code it was sent back with.
fn code_for(provider: &Provider, sub: &str, nonce: &str) -> String {
    let mut browser = Browser::new(provider.address());
    browser.sign_in_at_provider(provider, sub);
    let mut authorize = url(&format!("{}/authorize", provider.issuer()));
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("state", "the-state")
        .append_pair("nonce", nonce)
        .append_pair("code_challenge", &s256(VERIFIER))
        .append_pair("code_challenge_method", "S256");
    let step = browser.open(&authorize);
    assert_eq!(step.response.status, 302, "{}", step.response.describe());
    let back = step.location();
    assert_eq!(back.host_str(), Some("auth.vm.test"));
    assert_eq!(back.path(), "/callback");
    assert_eq!(query_param(&back, "state").as_deref(), Some("the-state"));
    assert_eq!(
        query_param(&back, "iss").as_deref(),
        Some(provider.issuer())
    );
    query_param(&back, "code").unwrap()
}

fn exchange(provider: &Provider, code: &str) -> Response {
    token(
        provider,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
        ],
        true,
    )
}

fn jwk(provider: &Provider, kid: &str) -> Option<Value> {
    let keys = get(provider, "/jwks").json();
    keys["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kid"] == kid)
        .cloned()
}

#[test]
fn the_configuration_names_the_endpoints_and_what_they_take() {
    let provider = Provider::start(&[ada()]);
    let configuration = get(&provider, "/.well-known/openid-configuration");
    configuration.expect_status(200);
    let c = configuration.json();
    assert_eq!(c["issuer"], provider.issuer());
    assert_eq!(
        c["authorization_endpoint"],
        format!("{}/authorize", provider.issuer())
    );
    assert_eq!(c["token_endpoint"], format!("{}/token", provider.issuer()));
    assert_eq!(c["jwks_uri"], format!("{}/jwks", provider.issuer()));
    assert_eq!(
        c["token_endpoint_auth_methods_supported"],
        json!(["client_secret_basic", "client_secret_post"])
    );
    assert!(jwk(&provider, KID_MAIN).is_some());
    assert!(jwk(&provider, KID_EC).is_some());
    assert!(jwk(&provider, KID_OTHER).is_none());
}

#[test]
fn a_code_is_exchanged_once_for_an_rs256_id_token_with_the_persons_claims() {
    let provider = Provider::start(&[ada()]);
    let code = code_for(&provider, "ada-sub", "the-nonce");
    let answer = exchange(&provider, &code);
    answer.expect_status(200);
    let body = answer.json();
    let id_token = body["id_token"].as_str().unwrap();
    assert!(body["refresh_token"].is_string());
    assert_eq!(header_of(id_token).unwrap()["alg"], "RS256");
    assert!(rs256_verifies(id_token, &jwk(&provider, KID_MAIN).unwrap()));
    let claims = claims_of(id_token).unwrap();
    assert_eq!(claims["iss"], provider.issuer());
    assert_eq!(claims["aud"], CLIENT_ID);
    assert_eq!(claims["sub"], "ada-sub");
    assert_eq!(claims["nonce"], "the-nonce");
    assert_eq!(claims["groups"], json!(["finance"]));
    assert_eq!(claims["name"], "Ada");
    assert_eq!(claims["email"], "ada@example.test");
    assert_eq!(exchange(&provider, &code).status, 400, "a code works once");
    let tokens = provider.grants("authorization_code");
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens[0].client_auth.as_deref(),
        Some("client_secret_basic")
    );
    assert_eq!(tokens[0].pkce_ok, Some(true));
    assert!(provider.issued().iter().any(|t| t == id_token));
    let authorize = &provider.requests_to("/authorize")[0];
    assert_eq!(authorize.query_param("code_challenge_method"), Some("S256"));
}

#[test]
fn a_wrong_verifier_or_a_wrong_secret_is_refused() {
    let provider = Provider::start(&[ada()]);
    let code = code_for(&provider, "ada-sub", "n");
    let wrong = token(
        &provider,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            (
                "code_verifier",
                "not-the-verifier-not-the-verifier-not-the-verifier",
            ),
        ],
        true,
    );
    assert_eq!(wrong.status, 400);
    assert_eq!(
        provider.grants("authorization_code")[0].pkce_ok,
        Some(false)
    );
    let code = code_for(&provider, "ada-sub", "n");
    let no_secret = token(
        &provider,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
            ("client_id", CLIENT_ID),
            ("client_secret", "wrong"),
        ],
        false,
    );
    assert_eq!(no_secret.status, 401);
}

#[test]
fn client_secret_post_is_taken_when_the_configuration_names_only_it() {
    let provider = Provider::start(&[ada()]);
    provider.token_auth_methods(&["client_secret_post"]);
    let configuration = get(&provider, "/.well-known/openid-configuration").json();
    assert_eq!(
        configuration["token_endpoint_auth_methods_supported"],
        json!(["client_secret_post"])
    );
    let code = code_for(&provider, "ada-sub", "n");
    let secret = provider.client_secret();
    let posted = token(
        &provider,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
            ("client_id", CLIENT_ID),
            ("client_secret", secret.expose()),
        ],
        false,
    );
    posted.expect_status(200);
    let code = code_for(&provider, "ada-sub", "n");
    assert_eq!(
        exchange(&provider, &code).status,
        401,
        "basic is not taken now"
    );
}

#[test]
fn refresh_reads_the_groups_again_and_a_disabled_person_is_refused() {
    let provider = Provider::start(&[ada()]);
    let code = code_for(&provider, "ada-sub", "n");
    let first = exchange(&provider, &code).json();
    let refresh = first["refresh_token"].as_str().unwrap().to_owned();
    provider.set_groups("ada-sub", &["devops"]);
    let again = token(
        &provider,
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
        true,
    );
    again.expect_status(200);
    let claims = claims_of(again.json()["id_token"].as_str().unwrap()).unwrap();
    assert_eq!(claims["groups"], json!(["devops"]));
    assert!(
        claims.get("nonce").is_none(),
        "a renewal's token has no nonce: {claims}"
    );
    let rotated = again.json()["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(rotated, refresh, "a renewal brings a new refresh token");
    let reused = token(
        &provider,
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
        true,
    );
    assert_eq!(reused.status, 400, "a used refresh token is refused");
    assert_eq!(reused.json()["error"], "invalid_grant");
    provider.disable("ada-sub");
    let refused = token(
        &provider,
        &[("grant_type", "refresh_token"), ("refresh_token", &rotated)],
        true,
    );
    assert_eq!(refused.status, 400);
    assert_eq!(refused.json()["error"], "invalid_grant");
}

#[test]
fn without_refresh_tokens_none_is_issued() {
    let provider = Provider::start(&[ada()]);
    provider.issue_refresh_tokens(false);
    let code = code_for(&provider, "ada-sub", "n");
    let body = exchange(&provider, &code).json();
    assert!(body.get("refresh_token").is_none(), "{body}");
}

#[test]
fn faults_make_the_token_they_say() {
    let provider = Provider::start(&[ada()]);
    let token_with = |fault: Option<TokenFault>| {
        provider.set_fault(fault);
        let code = code_for(&provider, "ada-sub", "the-nonce");
        exchange(&provider, &code).json()["id_token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let main = jwk(&provider, KID_MAIN).unwrap();
    let bad = token_with(Some(TokenFault::BadSignature));
    assert_eq!(header_of(&bad).unwrap()["kid"], KID_MAIN);
    assert!(!rs256_verifies(&bad, &main));
    assert_ne!(
        claims_of(&token_with(Some(TokenFault::WrongNonce))).unwrap()["nonce"],
        "the-nonce"
    );
    assert_ne!(
        claims_of(&token_with(Some(TokenFault::WrongAudience))).unwrap()["aud"],
        CLIENT_ID
    );
    assert_ne!(
        claims_of(&token_with(Some(TokenFault::WrongIssuer))).unwrap()["iss"],
        provider.issuer()
    );
    let expired = claims_of(&token_with(Some(TokenFault::ExpiresIn(-120)))).unwrap();
    assert!(expired["exp"].as_i64().unwrap() < expired["auth_time"].as_i64().unwrap() - 100);
    assert_eq!(
        header_of(&token_with(Some(TokenFault::AlgNone))).unwrap()["alg"],
        "none"
    );
    assert_eq!(
        header_of(&token_with(Some(TokenFault::Hs256WithSecret))).unwrap()["alg"],
        "HS256"
    );
    assert_eq!(
        claims_of(&token_with(Some(TokenFault::OtherSubject))).unwrap()["sub"],
        "ada-sub-someone-else"
    );
    provider.set_fault(None);
    provider.sign_with(Key::Other);
    let other = token_with(None);
    assert_eq!(header_of(&other).unwrap()["kid"], KID_OTHER);
    assert!(jwk(&provider, KID_OTHER).is_none());
    provider.publish_other_key(true);
    assert!(rs256_verifies(&other, &jwk(&provider, KID_OTHER).unwrap()));
    provider.sign_with(Key::Ec);
    let ec = token_with(None);
    assert_eq!(header_of(&ec).unwrap()["alg"], "ES256");
    assert!(es256_verifies(&ec, &jwk(&provider, KID_EC).unwrap()));
}

#[test]
fn the_groups_claim_takes_any_name_and_shape() {
    let provider = Provider::start(&[ada().groups_claim(Some(json!("finance")))]);
    provider.groups_claim_name("roles");
    let code = code_for(&provider, "ada-sub", "n");
    let claims = claims_of(
        exchange(&provider, &code).json()["id_token"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(claims["roles"], "finance");
    assert!(claims.get("groups").is_none());
}

#[test]
fn without_a_person_the_authorization_endpoint_asks_and_denying_sends_an_error_back() {
    let provider = Provider::start(&[ada()]);
    let mut browser = Browser::new(provider.address());
    let authorize: Url = url(&format!(
        "{}/authorize?response_type=code&client_id={CLIENT_ID}&redirect_uri={REDIRECT}&state=s",
        provider.issuer()
    ));
    let step = browser.open(&authorize);
    assert_eq!(step.response.status, 200);
    browser.sign_in_at_provider(&provider, "ada-sub");
    provider.deny(true);
    let denied = browser.open(&authorize);
    assert_eq!(denied.response.status, 302);
    assert_eq!(
        query_param(&denied.location(), "error").as_deref(),
        Some("access_denied")
    );
    assert_eq!(
        query_param(&denied.location(), "state").as_deref(),
        Some("s")
    );
}

#[test]
fn an_outage_answers_nothing_and_the_provider_comes_back() {
    let provider = Provider::start(&[ada()]);
    provider.set_outage(Outage::Hang);
    let hung = Request::new(provider.address(), "/jwks", "127.0.0.1")
        .timeout(Duration::from_secs(1))
        .try_send();
    assert!(
        hung.is_err(),
        "expected no answer while hanging, got {hung:?}"
    );
    provider.set_outage(Outage::Close);
    let closed = Request::new(provider.address(), "/jwks", "127.0.0.1")
        .timeout(Duration::from_secs(2))
        .try_send();
    assert!(
        closed.is_err(),
        "expected no answer while closing, got {closed:?}"
    );
    provider.set_outage(Outage::None);
    get(&provider, "/jwks").expect_status(200);
}

#[test]
fn an_oversize_jwks_is_larger_than_a_mebibyte() {
    let provider = Provider::start(&[ada()]);
    provider.oversize_jwks(true);
    assert!(get(&provider, "/jwks").body().len() > 1024 * 1024);
}

#[test]
fn the_token_answer_can_lack_an_id_token_or_not_be_json() {
    let provider = Provider::start(&[ada()]);
    provider.token_answer(TokenAnswer::WithoutIdToken);
    let code = code_for(&provider, "ada-sub", "n");
    let body = exchange(&provider, &code).json();
    assert!(
        body.get("id_token").is_none() && body["access_token"].is_string(),
        "{body}"
    );
    provider.token_answer(TokenAnswer::NotJson);
    let code = code_for(&provider, "ada-sub", "n");
    let answer = exchange(&provider, &code);
    assert_eq!(answer.status, 200);
    assert!(serde_json::from_slice::<Value>(answer.body()).is_err());
}

#[test]
fn the_token_endpoint_answers_a_status_of_the_tests_choosing() {
    let provider = Provider::start(&[ada()]);
    provider.token_status(Some(TokenStatus {
        status: 429,
        error: "slow_down".to_owned(),
        description: "too many".to_owned(),
    }));
    let code = code_for(&provider, "ada-sub", "n");
    let answer = exchange(&provider, &code);
    assert_eq!(answer.status, 429);
    assert_eq!(answer.json()["error"], "slow_down");
    assert_eq!(answer.json()["error_description"], "too many");
    provider.token_status(None);
    let code = code_for(&provider, "ada-sub", "n");
    exchange(&provider, &code).expect_status(200);
}

#[test]
fn the_configuration_and_keys_can_be_made_unusable() {
    let provider = Provider::start(&[ada()]);
    let configuration = || get(&provider, "/.well-known/openid-configuration");
    provider.config_fault(Some(ConfigFault::AnotherIssuer));
    assert_ne!(configuration().json()["issuer"], provider.issuer());
    provider.config_fault(Some(ConfigFault::LacksTokenEndpoint));
    assert!(configuration().json().get("token_endpoint").is_none());
    provider.config_fault(Some(ConfigFault::Oversize));
    assert!(configuration().body().len() > 1024 * 1024);
    provider.config_fault(None);
    provider.unreadable_jwks(true);
    assert!(get(&provider, "/jwks").json()["keys"].is_string());
    provider.unreadable_jwks(false);
    provider.token_answer(TokenAnswer::Oversize);
    let code = code_for(&provider, "ada-sub", "n");
    assert!(exchange(&provider, &code).body().len() > 1024 * 1024);
}

#[test]
fn fractional_times_are_halves_of_a_second() {
    let provider = Provider::start(&[ada()]);
    provider.set_fault(Some(TokenFault::FractionalTimes));
    let code = code_for(&provider, "ada-sub", "n");
    let id_token = exchange(&provider, &code).json()["id_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(rs256_verifies(
        &id_token,
        &jwk(&provider, KID_MAIN).unwrap()
    ));
    let claims = claims_of(&id_token).unwrap();
    for field in ["iat", "exp"] {
        let value = claims[field].as_f64().unwrap();
        assert!(
            (value.fract() - 0.5).abs() < f64::EPSILON,
            "{field}: {value}"
        );
    }
}

#[test]
fn another_secret_is_taken_when_told() {
    let provider = Provider::start(&[ada()]);
    let other = "the-other-secret-0123456789";
    let try_with = |secret: &str| {
        let code = code_for(&provider, "ada-sub", "n");
        Request::new(provider.address(), "/token", "127.0.0.1")
            .method("POST")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", basic(secret))
            .body(form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("code_verifier", VERIFIER),
            ]))
            .send()
    };
    assert_eq!(try_with(other).status, 401);
    provider.also_take_secret(other);
    try_with(other).expect_status(200);
}

#[test]
fn a_request_left_unanswered_is_still_recorded() {
    let provider = Provider::start(&[ada()]);
    provider.set_outage(Outage::Hang);
    let hung = Request::new(provider.address(), "/token", "127.0.0.1")
        .method("POST")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "x"),
        ]))
        .timeout(Duration::from_secs(1))
        .try_send();
    assert!(hung.is_err());
    assert_eq!(provider.grants("refresh_token").len(), 1);
    provider.set_outage(Outage::None);
}
