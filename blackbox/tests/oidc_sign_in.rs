//! The way through sign-in (CONTRACT.md, "Sign-in and sessions: The way
//! through sign-in"), step by step: what each redirect says, which cookies
//! it sets, `next` kept and never outside, an unknown Application, the
//! one-time code's rules, one sign-in for every Application, the scopes
//! and the client's authentication, and the addresses with the default
//! `https`.
//!
//! Acceptance criteria: "A browser with no session that opens an
//! Application is sent through the identity provider and lands back on the
//! Application signed in", "Having signed in once, opening a second
//! Application needs no further sign-in".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use blackbox::Request;
use blackbox::browser::{describe_steps, query_param, set_cookies, url};
use blackbox::oidc::{CLIENT_ID, Key, Outage, Provider};
use blackbox::sessions::{
    ADA, GRACE, Installation, SESSION_COOKIE, SIGN_IN_COOKIE, ada, assert_256_bits,
    assert_failure_page, assert_platform_cookie, assert_ten_minutes, grace, oidc_options,
    the_set_cookie,
};
use blackbox::{DOMAIN, VmOptions};
use serde_json::json;
use url::Url;

fn two_applications() -> Installation {
    Installation::start(&[("honest", "honest.js"), ("ledger", "honest.js")])
}

fn scopes_of(url: &Url) -> BTreeSet<String> {
    query_param(url, "scope")
        .unwrap_or_default()
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn set_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_browser_without_a_session_goes_through_the_identity_provider_and_lands_signed_in() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let target = "/reports/q1?year=2026&name=a%20b";
    let steps = at.sign_in(&mut browser, "honest", target);
    assert_eq!(
        steps.len(),
        6,
        "expected six steps: the Application, the sign-in address, the provider, the callback, /_vm/session, the Application\n{}",
        describe_steps(&steps)
    );

    // 1. The Application sends the browser to the sign-in address.
    let first = &steps[0];
    assert_eq!(
        first.response.status,
        302,
        "step 1\n{}",
        describe_steps(&steps)
    );
    at.vm
        .records_of(&first.response)
        .assert_no_application_code_ran();
    let to_sign_in = first.location();
    assert_eq!(
        format!(
            "{}{}",
            blackbox::browser::origin_of(&to_sign_in),
            to_sign_in.path()
        ),
        format!("{}/sign-in", at.auth_origin()),
        "step 1's Location"
    );
    assert_eq!(
        query_param(&to_sign_in, "application").as_deref(),
        Some("honest")
    );
    assert_eq!(query_param(&to_sign_in, "next").as_deref(), Some(target));
    let state = query_param(&to_sign_in, "state").unwrap_or_default();
    assert!(!state.is_empty(), "expected a state in {to_sign_in}");
    let sign_in_cookie = the_set_cookie(&first.response, SIGN_IN_COOKIE, "step 1");
    assert_platform_cookie(&sign_in_cookie, SIGN_IN_COOKIE, "step 1's cookie");
    assert_ten_minutes(&sign_in_cookie, "step 1's cookie");
    assert_eq!(
        sign_in_cookie.value, state,
        "step 1's cookie holds the state"
    );

    // 2. The sign-in address sends it to the provider.
    let second = &steps[1];
    assert_eq!(second.url, to_sign_in);
    assert_eq!(second.response.status, 302, "step 2");
    let authorize = second.location();
    assert_eq!(
        authorize.as_str().split('?').next(),
        Some(format!("{}/authorize", at.provider.issuer()).as_str()),
        "step 2 goes to the provider's authorization endpoint"
    );
    assert_eq!(
        query_param(&authorize, "response_type").as_deref(),
        Some("code")
    );
    assert_eq!(
        query_param(&authorize, "client_id").as_deref(),
        Some(CLIENT_ID)
    );
    assert_eq!(
        query_param(&authorize, "redirect_uri"),
        Some(at.callback()),
        "the redirect address registered at the provider"
    );
    assert_eq!(
        scopes_of(&authorize),
        set_of(&["openid", "profile", "email", "offline_access"])
    );
    assert!(query_param(&authorize, "state").is_some_and(|s| !s.is_empty()));
    assert!(query_param(&authorize, "nonce").is_some_and(|s| !s.is_empty()));
    assert_eq!(
        query_param(&authorize, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert!(
        query_param(&authorize, "code_challenge").is_some_and(|c| c.len() == 43),
        "expected an S256 code challenge (43 characters) in {authorize}"
    );

    // 3. The provider sends it back to the callback, which exchanges the
    // code and sends it on to the Application with a one-time code.
    assert!(steps[2].location().as_str().starts_with(&at.callback()));
    let callback = &steps[3];
    assert_eq!(callback.response.status, 302, "the callback");
    let platform_cookie = the_set_cookie(&callback.response, SESSION_COOKIE, "the callback");
    assert_platform_cookie(
        &platform_cookie,
        SESSION_COOKIE,
        "the platform session's cookie",
    );
    assert_256_bits(&platform_cookie.value, "the platform session's cookie");
    assert!(
        browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_some(),
        "the platform session's cookie is kept for the sign-in address"
    );
    let session = callback.location();
    assert_eq!(
        format!(
            "{}{}",
            blackbox::browser::origin_of(&session),
            session.path()
        ),
        format!("{}/_vm/session", at.app_origin("honest"))
    );
    assert!(query_param(&session, "code").is_some_and(|c| !c.is_empty()));
    assert_eq!(
        query_param(&session, "state").as_deref(),
        Some(state.as_str())
    );
    assert_eq!(query_param(&session, "next").as_deref(), Some(target));
    let exchanges = at.provider.grants("authorization_code");
    assert_eq!(exchanges.len(), 1, "one code exchanged");
    assert_eq!(
        exchanges[0].client_auth.as_deref(),
        Some("client_secret_basic")
    );
    assert_eq!(
        exchanges[0].pkce_ok,
        Some(true),
        "PKCE S256 checked by the provider"
    );
    assert_eq!(
        exchanges[0].form_param("redirect_uri"),
        Some(at.callback().as_str())
    );

    // 4. /_vm/session sets the Application's own cookie and sends the
    // browser to `next`.
    let fourth = &steps[4];
    assert_eq!(fourth.response.status, 302, "/_vm/session");
    at.vm
        .records_of(&fourth.response)
        .assert_no_application_code_ran();
    let app_cookie = the_set_cookie(&fourth.response, SESSION_COOKIE, "/_vm/session");
    assert_platform_cookie(
        &app_cookie,
        SESSION_COOKIE,
        "the Application's session cookie",
    );
    assert_256_bits(&app_cookie.value, "the Application's session cookie");
    assert_ne!(
        app_cookie.value, platform_cookie.value,
        "the Application's cookie is not the platform's"
    );
    assert_eq!(
        fourth.location(),
        at.app_url("honest", target),
        "sent to next"
    );

    // Landed, signed in as Ada.
    let user = steps[5].response.json()["user"].clone();
    assert_eq!(user, json!({ "id": ADA, "groups": ["finance"] }));
}

#[test]
fn opening_a_second_application_needs_no_visit_to_the_identity_provider() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let asked_before = at.provider.requests().len();
    let steps = at.sign_in(&mut browser, "ledger", "/books");
    let provider_host = at.provider.address().ip().to_string();
    assert!(
        steps
            .iter()
            .all(|s| s.url.host_str() != Some(provider_host.as_str())),
        "expected the second Application to need no visit to the provider\n{}",
        describe_steps(&steps)
    );
    assert_eq!(
        at.provider.requests().len(),
        asked_before,
        "expected the provider to be asked nothing for the second Application"
    );
    assert_eq!(steps.len(), 4, "{}", describe_steps(&steps));
    assert_eq!(steps[1].url.path(), "/sign-in");
    assert_eq!(steps[1].location().path(), "/_vm/session");
    assert_eq!(
        steps[1].location().host_str(),
        Some(format!("ledger.{DOMAIN}").as_str())
    );
    assert_eq!(
        steps[3].response.json()["user"],
        json!({ "id": ADA, "groups": ["finance"] })
    );
    // And the first is still signed in.
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}

#[test]
fn two_people_in_two_browsers_are_each_themselves() {
    let at = two_applications();
    let mut ada_browser = at.browser_as(ADA);
    let mut grace_browser = at.browser_as(GRACE);
    at.sign_in(&mut ada_browser, "honest", "/");
    at.sign_in(&mut grace_browser, "honest", "/");
    assert_eq!(
        at.user_seen(&mut ada_browser, "honest"),
        json!({ "id": ADA, "groups": ["finance"] })
    );
    assert_eq!(
        at.user_seen(&mut grace_browser, "honest"),
        json!({ "id": GRACE, "groups": ["devops"] })
    );
}

#[test]
fn only_a_page_is_sent_to_sign_in_and_anything_else_is_401_without_running_code() {
    let at = two_applications();
    let browser = at.browser();
    let page = at.app_url("honest", "/");
    let cases = [
        ("GET", Some("application/json")),
        ("GET", None),
        ("GET", Some("*/*")),
        ("POST", Some("text/html")),
        ("PUT", Some("text/html")),
        ("DELETE", None),
    ];
    for (method, accept) in cases {
        let mut request = browser.request(method, &page);
        if let Some(accept) = accept {
            request = request.header("accept", accept);
        }
        let answer = request.send();
        assert_eq!(
            answer.status,
            401,
            "{method} with Accept {accept:?} and no session\n{}",
            answer.describe()
        );
        assert!(
            answer
                .header("content-type")
                .is_some_and(|c| c.starts_with("text/plain")),
            "{method} with Accept {accept:?}: a 401 is text/plain\n{}",
            answer.describe()
        );
        at.vm.records_of(&answer).assert_no_application_code_ran();
    }
    for method in ["GET", "HEAD"] {
        let answer = browser
            .request(method, &page)
            .header("accept", "text/html")
            .send();
        assert_eq!(
            answer.status,
            302,
            "{method} of a page\n{}",
            answer.describe()
        );
        at.vm.records_of(&answer).assert_no_application_code_ran();
    }
}

#[test]
fn an_unknown_application_is_404_and_the_sign_in_address_takes_only_a_known_one() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let page = browser.open(&at.app_url("nowhere", "/"));
    assert_eq!(
        page.response.status,
        404,
        "a page of an unknown Application, no session\n{}",
        page.response.describe()
    );
    for query in [
        "application=nowhere&next=%2F&state=s",
        "application=evil.example&next=%2F&state=s",
        "application=honest.evil.example&next=%2F&state=s",
        "next=%2F&state=s",
    ] {
        let step = browser.open(&at.auth_url(&format!("/sign-in?{query}")));
        assert_eq!(
            step.response.status,
            404,
            "/sign-in?{query} without a platform session\n{}",
            step.response.describe()
        );
    }
    at.sign_in(&mut browser, "honest", "/");
    let step = browser.open(&at.auth_url("/sign-in?application=nowhere&next=%2F&state=s"));
    assert_eq!(
        step.response.status,
        404,
        "an unknown Application with a platform session\n{}",
        step.response.describe()
    );
}

#[test]
fn next_is_only_ever_a_path_of_the_installation() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let cases = [
        ("//evil.example/x", "/"),
        ("/\\evil.example/x", "/"),
        ("https://evil.example/", "/"),
        ("evil.example", "/"),
        ("", "/"),
        ("/books?year=2026", "/books?year=2026"),
    ];
    for (next, kept) in cases {
        let mut sign_in = at.auth_url("/sign-in");
        sign_in
            .query_pairs_mut()
            .append_pair("application", "honest")
            .append_pair("next", next)
            .append_pair("state", "a-state");
        let step = browser.open(&sign_in);
        assert_eq!(
            step.response.status,
            302,
            "next {next:?}\n{}",
            step.response.describe()
        );
        let to = step.location();
        assert_eq!(
            to.host_str(),
            Some(format!("honest.{DOMAIN}").as_str()),
            "next {next:?}: the redirect stays in the installation"
        );
        assert_eq!(to.path(), "/_vm/session", "next {next:?}");
        assert_eq!(
            query_param(&to, "next").as_deref(),
            Some(kept),
            "next {next:?}"
        );
    }
    let mut no_next = at.auth_url("/sign-in");
    no_next
        .query_pairs_mut()
        .append_pair("application", "honest")
        .append_pair("state", "a-state");
    let step = browser.open(&no_next);
    assert_eq!(query_param(&step.location(), "next").as_deref(), Some("/"));
}

#[test]
fn a_next_outside_given_through_the_identity_provider_still_lands_on_the_application() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let first = browser.open(&at.app_url("honest", "/"));
    let state = query_param(&first.location(), "state").unwrap();
    let mut sign_in = at.auth_url("/sign-in");
    sign_in
        .query_pairs_mut()
        .append_pair("application", "honest")
        .append_pair("next", "//evil.example/steal")
        .append_pair("state", &state);
    let steps = browser.navigate(&sign_in);
    let allowed = [
        at.auth_host(),
        format!("honest.{DOMAIN}"),
        at.provider.address().ip().to_string(),
    ];
    for step in &steps {
        let host = step.url.host_str().unwrap_or_default().to_owned();
        assert!(
            allowed.contains(&host),
            "expected every step to stay in the installation or at the provider\n{}",
            describe_steps(&steps)
        );
    }
    at.assert_landed(&steps, "honest", "/");
}

#[test]
fn a_callback_with_a_state_this_browser_did_not_start_begins_no_session() {
    let at = two_applications();
    let mut ada_browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut ada_browser, "honest", "/");
    let callback = steps.last().unwrap().location();
    let mut other = at.browser_as(GRACE);
    let answer = other.open(&callback);
    let hidden = at.provider.issued();
    assert_failure_page(
        &answer.response,
        &[400],
        &hidden,
        "another browser's callback",
    );
    assert!(other.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
    assert_eq!(
        at.provider.grants("authorization_code").len(),
        0,
        "expected the code not to be exchanged for a state this browser did not start"
    );
    let mut forged = callback.clone();
    forged.set_query(Some(&format!(
        "code={}&state=not-a-state-of-this-browser",
        query_param(&callback, "code").unwrap()
    )));
    let answer = ada_browser.open(&forged);
    assert_failure_page(&answer.response, &[400], &hidden, "a forged state");
    let bare = ada_browser.open(&at.auth_url("/callback"));
    assert_failure_page(&bare.response, &[400], &hidden, "a callback with nothing");
    assert!(
        ada_browser
            .jar
            .get(&at.auth_host(), SESSION_COOKIE)
            .is_none()
    );
}

#[test]
fn an_error_from_the_identity_provider_is_a_page_with_a_way_to_try_again() {
    let at = two_applications();
    at.provider.deny(true);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let answer = browser.follow(steps.last().unwrap());
    assert_failure_page(
        &answer.response,
        &[400],
        &[],
        "access_denied from the provider",
    );
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
    at.assert_no_session(&mut browser, "honest", "after access_denied");
}

/// Fails unless the answer is a `400` page that sets no cookie.
fn assert_refused_code(answer: &blackbox::Response, context: &str) {
    assert_eq!(answer.status, 400, "{context}\n{}", answer.describe());
    assert!(
        answer
            .header("content-type")
            .is_some_and(|c| c.starts_with("text/html")),
        "{context}: a page of the platform's own\n{}",
        answer.describe()
    );
    assert!(
        set_cookies(answer).is_empty(),
        "{context}: nothing is set\n{}",
        answer.describe()
    );
}

#[test]
fn the_one_time_code_works_once() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let session = at.walk_to_session_code(&mut browser, "honest", "/");
    let state = query_param(&session, "state").unwrap();
    let used = browser.open(&session);
    assert_eq!(used.response.status, 302, "{}", used.response.describe());
    // The browser still holds (or is given back) step 1's cookie: only the
    // code's being used before is wrong.
    let mut again = at.browser();
    again
        .jar
        .insert(&format!("honest.{DOMAIN}"), SIGN_IN_COOKIE, &state);
    let answer = again.open(&session);
    assert_refused_code(&answer.response, "a code used twice");
    at.vm
        .records_of(&answer.response)
        .assert_no_application_code_ran();
}

#[test]
fn the_one_time_code_is_for_its_application_only() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let session = at.walk_to_session_code(&mut browser, "honest", "/");
    let state = query_param(&session, "state").unwrap();
    let mut elsewhere = at.app_url("ledger", "/_vm/session");
    elsewhere.set_query(session.query());
    let mut thief = at.browser();
    thief
        .jar
        .insert(&format!("ledger.{DOMAIN}"), SIGN_IN_COOKIE, &state);
    let answer = thief.open(&elsewhere);
    assert_refused_code(&answer.response, "honest's code at ledger");
    assert!(
        thief
            .jar
            .get(&format!("ledger.{DOMAIN}"), SESSION_COOKIE)
            .is_none()
    );
}

#[test]
fn the_one_time_code_needs_the_cookie_of_step_one_and_its_state() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let session = at.walk_to_session_code(&mut browser, "honest", "/");
    let without_cookie = at.browser().open(&session);
    assert_refused_code(&without_cookie.response, "without step 1's cookie");
    let mut other_state = session.clone();
    other_state.set_query(Some(&format!(
        "code={}&state=another-state&next=%2F",
        query_param(&session, "code").unwrap()
    )));
    let mismatch = browser.open(&other_state);
    assert_refused_code(&mismatch.response, "a state that is not the cookie's");
    let nothing = browser.open(&at.app_url("honest", "/_vm/session"));
    assert_refused_code(&nothing.response, "no code at all");
}

#[test]
fn the_one_time_code_is_refused_after_sixty_seconds() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let session = at.walk_to_session_code(&mut browser, "honest", "/");
    std::thread::sleep(Duration::from_secs(62));
    let late = browser.open(&session);
    assert_refused_code(&late.response, "a code 62 seconds old");
}

#[test]
fn only_vm_session_is_the_platforms_under_vm_and_never_reaches_application_code() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let session = at.get_json(&mut browser, "honest", "/_vm/session");
    assert_eq!(session.status, 400, "{}", session.describe());
    at.vm.records_of(&session).assert_no_application_code_ran();
    let other = at.get_json(&mut browser, "honest", "/_vm/other");
    other.expect_status(200);
    assert_eq!(
        other.json()["url"]
            .as_str()
            .map(|u| u.ends_with("/_vm/other")),
        Some(true)
    );
    assert!(
        !at.vm.records_of(&other).printed().is_empty(),
        "expected the Application's code to run for /_vm/other"
    );
}

#[test]
fn the_scopes_asked_for_are_the_default_or_the_ones_given_and_always_openid() {
    for (given, want) in [
        (Some("openid email"), set_of(&["openid", "email"])),
        (
            Some("email profile"),
            set_of(&["openid", "email", "profile"]),
        ),
    ] {
        let mut more = vec![];
        if let Some(scopes) = given {
            more = vec!["--oidc-scopes", scopes];
        }
        let at = Installation::start_with(&[("honest", "honest.js")], &[ada()], &more);
        let mut browser = at.browser_as(ADA);
        let steps = at.sign_in(&mut browser, "honest", "/");
        assert_eq!(
            scopes_of(&steps[1].location()),
            want,
            "--oidc-scopes {given:?}"
        );
    }
}

#[test]
fn the_client_authenticates_with_client_secret_post_when_the_provider_names_only_it() {
    let provider = Provider::start(&[ada()]);
    provider.token_auth_methods(&["client_secret_post"]);
    let options = oidc_options(&provider).application("honest", "honest.js");
    let at = Installation::from(provider, &options);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let exchange = &at.provider.grants("authorization_code")[0];
    assert_eq!(exchange.client_auth.as_deref(), Some("client_secret_post"));
    assert_eq!(exchange.header("authorization"), None);
}

#[test]
fn an_es256_id_token_is_taken() {
    let at = Installation::start(&[("honest", "honest.js")]);
    at.provider.sign_with(Key::Ec);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}

#[test]
fn a_provider_unreachable_at_first_is_used_once_it_answers() {
    let at = Installation::start(&[("honest", "honest.js")]);
    at.provider.set_outage(Outage::Close);
    let mut browser = at.browser_as(ADA);
    let first = browser.open(&at.app_url("honest", "/"));
    assert_eq!(first.response.status, 302, "step 1 needs no provider");
    let second = browser.follow(&first);
    assert!(
        [502, 503].contains(&second.response.status),
        "expected the sign-in address to say the sign-in cannot be started while the provider does not answer, got\n{}",
        second.response.describe()
    );
    at.provider.set_outage(Outage::None);
    let mut fresh = at.browser_as(ADA);
    at.sign_in(&mut fresh, "honest", "/");
}

#[test]
fn a_token_endpoint_that_does_not_answer_within_ten_seconds_is_a_502_page() {
    let at = Installation::start(&[("honest", "honest.js")]);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    at.provider.token_delay(Duration::from_secs(20));
    let began = Instant::now();
    let answer = browser.follow(steps.last().unwrap());
    let took = began.elapsed();
    assert_failure_page(
        &answer.response,
        &[502],
        &at.provider.issued(),
        "a token endpoint that does not answer",
    );
    assert!(
        took < Duration::from_secs(14),
        "expected the call to the provider to end within 10 seconds, the callback took {took:?}"
    );
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
}

/// Starts the program with OIDC with the defaults for the addresses (no
/// `--public-scheme`), these more arguments, and `honest`.
fn with_public(more: &[&str]) -> Installation {
    let provider = Provider::start(&[ada()]);
    let options = VmOptions::new()
        .args_extra(&[
            "--oidc-issuer",
            provider.issuer(),
            "--oidc-client-id",
            CLIENT_ID,
        ])
        .args_extra(more)
        .env("VM_OIDC_CLIENT_SECRET", provider.client_secret().expose())
        .application("honest", "honest.js");
    Installation::from(provider, &options)
}

/// Step 1 and step 2 for an installation the browser cannot follow (its
/// addresses are `https`): sent by hand to the program.
fn first_two_steps(at: &Installation) -> (blackbox::Response, Url, Url) {
    let page = Request::new(at.vm.address(), "/", &format!("honest.{DOMAIN}"))
        .header("accept", "text/html")
        .send();
    assert_eq!(page.status, 302, "{}", page.describe());
    let to_sign_in = url(page.header("location").unwrap());
    let sign_in = Request::new(
        at.vm.address(),
        &blackbox::browser::target_of(&to_sign_in),
        &format!("auth.{DOMAIN}"),
    )
    .send();
    assert_eq!(sign_in.status, 302, "{}", sign_in.describe());
    let authorize = url(sign_in.header("location").unwrap());
    (page, to_sign_in, authorize)
}

#[test]
fn with_the_default_https_the_addresses_have_no_port_and_the_cookies_the_host_prefix() {
    let at = with_public(&[]);
    let (page, to_sign_in, authorize) = first_two_steps(&at);
    assert_eq!(
        to_sign_in.as_str().split('?').next(),
        Some(format!("https://auth.{DOMAIN}/sign-in").as_str())
    );
    assert_eq!(
        query_param(&authorize, "redirect_uri"),
        Some(format!("https://auth.{DOMAIN}/callback"))
    );
    let cookie = the_set_cookie(&page, "__Host-vm-sign-in", "step 1 with https");
    assert!(cookie.has("secure"), "{:?}", cookie.raw);
    assert!(cookie.has("httponly"), "{:?}", cookie.raw);
    assert_eq!(cookie.value_of("path"), Some("/"), "{:?}", cookie.raw);
    assert!(!cookie.has("domain"), "{:?}", cookie.raw);
    assert!(
        cookie
            .value_of("samesite")
            .is_some_and(|v| v.eq_ignore_ascii_case("lax")),
        "{:?}",
        cookie.raw
    );
}

#[test]
fn a_public_port_and_scheme_are_written_into_the_addresses() {
    let at = with_public(&["--public-port", "8443"]);
    let (_, to_sign_in, authorize) = first_two_steps(&at);
    assert!(
        to_sign_in
            .as_str()
            .starts_with(&format!("https://auth.{DOMAIN}:8443/sign-in?")),
        "{to_sign_in}"
    );
    assert_eq!(
        query_param(&authorize, "redirect_uri"),
        Some(format!("https://auth.{DOMAIN}:8443/callback"))
    );
    let at = with_public(&["--public-scheme", "http", "--public-port", "80"]);
    let (page, to_sign_in, authorize) = first_two_steps(&at);
    assert!(
        to_sign_in
            .as_str()
            .starts_with(&format!("http://auth.{DOMAIN}/sign-in?")),
        "{to_sign_in}"
    );
    assert_eq!(
        query_param(&authorize, "redirect_uri"),
        Some(format!("http://auth.{DOMAIN}/callback"))
    );
    the_set_cookie(&page, SIGN_IN_COOKIE, "step 1 with http");
}

#[test]
fn the_code_is_given_the_sub_as_the_user_and_stand_in_headers_mean_nothing() {
    let at = Installation::start_with(&[("honest", "honest.js")], &[ada(), grace()], &[]);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let at_url = at.app_url("honest", "/");
    let request = browser
        .request("GET", &at_url)
        .header("accept", "application/json")
        .header("x-vm-stand-in-user", GRACE)
        .header("x-vm-stand-in-groups", "devops,platform-admins");
    let answer = browser.send(&at_url, &request);
    answer.expect_status(200);
    assert_eq!(
        answer.json()["user"],
        json!({ "id": ADA, "groups": ["finance"] })
    );
    let whoami = &answer.json()["whoami"]["ok"]["user"];
    assert_eq!(whoami, &json!({ "id": ADA, "groups": ["finance"] }));
    let stranger = at
        .browser()
        .request("GET", &at_url)
        .header("accept", "application/json")
        .header("x-vm-stand-in-user", GRACE)
        .send();
    assert_eq!(
        stranger.status, 401,
        "stand-in headers sign no one in under OIDC"
    );
}
