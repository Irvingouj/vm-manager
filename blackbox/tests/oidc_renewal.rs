//! Renewal and the end of a session (CONTRACT.md, "Sign-in and sessions:
//! Renewal and the end of a session"): Groups read again at each renewal,
//! in every Application; a refusal or another `sub` ends the session; a
//! provider that does not answer is `503`; no refresh token, the lifetime,
//! one renewal for requests at once, at most 20 sessions a User.
//!
//! Acceptance criterion: "A change to a person's Groups at the identity
//! provider takes effect at the next session renewal, and the renewal
//! period can be set".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::Response;
use blackbox::oidc::{Outage, TokenAnswer, TokenFault, TokenStatus};
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace};
use serde_json::json;

fn renewing_every(seconds: u64) -> Installation {
    Installation::start_with(
        &[("honest", "honest.js"), ("ledger", "honest.js")],
        &[ada(), grace()],
        &["--session-renewal", &seconds.to_string()],
    )
}

/// Sleeps until `period` and a little more have passed since `since`.
fn wait_past(since: Instant, period: Duration) {
    let until = since + period + Duration::from_millis(700);
    let left = until.saturating_duration_since(Instant::now());
    thread::sleep(left);
}

#[test]
fn groups_changed_at_the_provider_take_effect_at_the_next_renewal_in_every_application() {
    let at = renewing_every(4);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    at.provider.set_groups(ADA, &["devops", "audit"]);
    let renewed_before = at.me(&mut browser).json()["session_renewed"].clone();
    for application in ["honest", "ledger"] {
        assert_eq!(
            at.user_seen(&mut browser, application)["groups"],
            json!(["finance"]),
            "{application}: before the renewal period the Groups read at sign-in hold"
        );
    }
    assert!(
        signed_in.elapsed() < Duration::from_secs(4),
        "the test was too slow to look before the renewal period ended ({:?})",
        signed_in.elapsed()
    );
    assert!(
        at.provider.grants("refresh_token").is_empty(),
        "no renewal before the period ends"
    );
    wait_past(signed_in, Duration::from_secs(4));
    for application in ["honest", "ledger"] {
        assert_eq!(
            at.user_seen(&mut browser, application),
            json!({ "id": ADA, "groups": ["devops", "audit"] }),
            "{application}: after the renewal the Groups of the new token"
        );
    }
    assert_eq!(
        at.provider.grants("refresh_token").len(),
        1,
        "one renewal of the one session serves every Application"
    );
    let me = at.me(&mut browser).json();
    assert_eq!(me["groups"], json!(["devops", "audit"]));
    assert_ne!(
        me["session_renewed"], renewed_before,
        "/me says when it was renewed"
    );
}

#[test]
fn a_person_disabled_at_the_provider_loses_the_session_at_renewal_everywhere() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    at.provider.disable(ADA);
    wait_past(signed_in, Duration::from_secs(2));
    at.assert_no_session(&mut browser, "honest", "a disabled person, honest");
    at.assert_no_session(&mut browser, "ledger", "a disabled person, ledger");
    at.assert_logged("session-ended", &[("reason", json!("refused"))]);
    at.assert_logged(
        "identity-provider-refused",
        &[("status", json!(400)), ("error", json!("invalid_grant"))],
    );
    assert_eq!(at.me(&mut browser).status, 401);
    // Grace, not disabled, is untouched.
    let mut other = at.browser_as(GRACE);
    at.sign_in(&mut other, "honest", "/");
    assert_eq!(at.user_seen(&mut other, "honest")["id"], GRACE);
}

#[test]
fn a_renewal_that_brings_another_sub_ends_the_session() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.provider.set_fault(Some(TokenFault::OtherSubject));
    wait_past(signed_in, Duration::from_secs(2));
    at.assert_no_session(&mut browser, "honest", "a renewal with another sub");
    at.assert_logged("session-ended", &[("reason", json!("another-sub"))]);
}

#[test]
fn a_provider_that_does_not_answer_at_renewal_is_503_with_retry_after_and_the_code_does_not_run() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.provider.set_groups(ADA, &["devops"]);
    wait_past(signed_in, Duration::from_secs(2));
    for (outage, within) in [(Outage::Close, 5), (Outage::Hang, 14)] {
        at.provider.set_outage(outage);
        let began = Instant::now();
        let answer = at.get_json(&mut browser, "honest", "/");
        let took = began.elapsed();
        assert_eq!(answer.status, 503, "{outage:?}\n{}", answer.describe());
        assert!(
            answer.header("retry-after").is_some(),
            "{outage:?}: expected Retry-After\n{}",
            answer.describe()
        );
        at.vm.records_of(&answer).assert_no_application_code_ran();
        assert!(
            took < Duration::from_secs(within),
            "{outage:?}: expected the answer within {within} s (the provider is asked within 10 s), it took {took:?}"
        );
    }
    at.assert_logged(
        "identity-provider-unanswered",
        &[("call", json!("token")), ("problem", json!("timed-out"))],
    );
    at.provider.set_outage(Outage::None);
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        json!(["devops"]),
        "once the provider answers, the session is renewed"
    );
}

#[test]
fn without_a_refresh_token_the_session_ends_at_its_first_renewal_and_a_page_signs_in_again() {
    let at = renewing_every(2);
    at.provider.issue_refresh_tokens(false);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
    wait_past(signed_in, Duration::from_secs(2));
    at.assert_no_session(&mut browser, "honest", "no refresh token");
    at.assert_logged("session-ended", &[("reason", json!("no-refresh-token"))]);
    assert!(at.provider.grants("refresh_token").is_empty());
    // A page goes through sign-in again; the provider answers without
    // asking (the browser is still signed in there).
    at.provider.issue_refresh_tokens(true);
    let steps = at.sign_in(&mut browser, "honest", "/");
    let provider_host = at.provider.address().ip().to_string();
    assert!(
        steps
            .iter()
            .any(|s| s.url.host_str() == Some(provider_host.as_str()))
    );
}

#[test]
fn a_session_ends_at_its_lifetime_for_every_application() {
    let at = Installation::start_with(
        &[("honest", "honest.js"), ("ledger", "honest.js")],
        &[ada()],
        &["--session-lifetime", "60"],
    );
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    wait_past(signed_in, Duration::from_secs(50));
    assert_eq!(
        at.user_seen(&mut browser, "honest")["id"],
        ADA,
        "before the lifetime"
    );
    wait_past(signed_in, Duration::from_secs(61));
    at.assert_no_session(&mut browser, "honest", "past the lifetime");
    at.assert_no_session(&mut browser, "ledger", "past the lifetime");
    assert_eq!(at.me(&mut browser).status, 401);
}

#[test]
fn requests_of_one_session_at_once_wait_for_one_renewal() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    at.provider.set_groups(ADA, &["devops"]);
    at.provider.token_delay(Duration::from_secs(2));
    wait_past(signed_in, Duration::from_secs(2));
    let requests: Vec<blackbox::Request> = (0..8)
        .map(|i| {
            let application = if i % 2 == 0 { "honest" } else { "ledger" };
            browser
                .request("GET", &at.app_url(application, "/"))
                .header("accept", "application/json")
        })
        .collect();
    let start = Barrier::new(requests.len());
    let answers: Vec<Response> = thread::scope(|scope| {
        let handles: Vec<_> = requests
            .iter()
            .map(|request| {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    request.send()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for answer in &answers {
        assert_eq!(answer.status, 200, "{}", answer.describe());
        assert_eq!(answer.json()["user"]["groups"], json!(["devops"]));
    }
    assert_eq!(
        at.provider.grants("refresh_token").len(),
        1,
        "expected one renewal for the eight requests of one session"
    );
}

#[test]
fn a_user_holds_at_most_twenty_sessions_and_the_oldest_ends_first() {
    let at = Installation::start(&[("honest", "honest.js")]);
    let mut browsers: Vec<_> = (0..21).map(|_| at.browser_as(ADA)).collect();
    for browser in &mut browsers {
        at.sign_in(browser, "honest", "/");
    }
    at.assert_no_session(&mut browsers[0], "honest", "the oldest of 21 sessions");
    assert_eq!(at.me(&mut browsers[0]).status, 401);
    for (i, browser) in browsers.iter_mut().enumerate().skip(1) {
        assert_eq!(
            at.user_seen(browser, "honest")["id"],
            ADA,
            "session {i} of 21 still holds"
        );
    }
}

#[test]
fn a_renewal_answered_without_an_id_token_ends_the_session() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    at.provider.token_answer(TokenAnswer::WithoutIdToken);
    wait_past(signed_in, Duration::from_secs(2));
    at.assert_no_session(&mut browser, "honest", "a renewal without an ID token");
    at.assert_no_session(&mut browser, "ledger", "a renewal without an ID token");
    at.assert_logged("session-ended", &[("reason", json!("no-id-token"))]);
    assert_eq!(at.provider.grants("refresh_token").len(), 1);
}

#[test]
fn a_renewal_whose_id_token_does_not_hold_ends_the_session() {
    for fault in [
        TokenFault::BadSignature,
        TokenFault::WrongAudience,
        TokenFault::ExpiresIn(-120),
    ] {
        let at = renewing_every(2);
        let mut browser = at.browser_as(ADA);
        let signed_in = Instant::now();
        at.sign_in(&mut browser, "honest", "/");
        at.provider.set_fault(Some(fault));
        wait_past(signed_in, Duration::from_secs(2));
        at.assert_no_session(
            &mut browser,
            "honest",
            &format!("a renewal whose ID token does not hold ({fault:?})"),
        );
        assert_eq!(at.provider.grants("refresh_token").len(), 1, "{fault:?}");
        at.assert_logged("session-ended", &[("reason", json!("invalid-id-token"))]);
    }
}

#[test]
fn requests_that_come_while_the_provider_does_not_answer_share_one_renewal_and_are_each_503() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    wait_past(signed_in, Duration::from_secs(2));
    at.provider.set_outage(Outage::Hang);
    let requests: Vec<blackbox::Request> = (0..5)
        .map(|i| {
            let application = if i % 2 == 0 { "honest" } else { "ledger" };
            browser
                .request("GET", &at.app_url(application, "/"))
                .header("accept", "application/json")
        })
        .collect();
    let answers: Vec<(Response, Duration)> = thread::scope(|scope| {
        let handles: Vec<_> = requests
            .iter()
            .enumerate()
            .map(|(i, request)| {
                scope.spawn(move || {
                    // A little apart: each comes while the first's renewal
                    // is under way.
                    thread::sleep(Duration::from_millis(300) * u32::try_from(i).unwrap());
                    let began = Instant::now();
                    let answer = request.send();
                    (answer, began.elapsed())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (i, (answer, took)) in answers.iter().enumerate() {
        assert_eq!(answer.status, 503, "request {i}\n{}", answer.describe());
        assert!(
            answer.header("retry-after").is_some(),
            "request {i}: expected Retry-After\n{}",
            answer.describe()
        );
        assert!(
            *took < Duration::from_secs(12),
            "request {i}: expected an answer within about 12 s of its own start (one renewal of at most 10 s), it took {took:?}"
        );
        at.vm.records_of(answer).assert_no_application_code_ran();
    }
    assert_eq!(
        at.provider.grants("refresh_token").len(),
        1,
        "expected the provider to be asked once for the one renewal of five requests"
    );
    at.provider.set_outage(Outage::None);
    at.provider.set_groups(ADA, &["devops"]);
    assert_eq!(
        at.user_seen(&mut browser, "ledger")["groups"],
        json!(["devops"]),
        "the session stayed and is renewed once the provider answers"
    );
}

#[test]
fn a_renewal_goes_on_when_the_request_that_began_it_goes_away() {
    let at = renewing_every(3);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.provider.set_groups(ADA, &["devops"]);
    at.provider.token_delay(Duration::from_secs(2));
    wait_past(signed_in, Duration::from_secs(3));
    let gone = browser
        .request("GET", &at.app_url("honest", "/"))
        .header("accept", "application/json")
        .timeout(Duration::from_millis(300))
        .try_send();
    assert!(
        gone.is_err(),
        "expected no answer within 300 ms while the provider takes 2 s, got {gone:?}"
    );
    thread::sleep(Duration::from_millis(2500));
    at.provider.token_delay(Duration::ZERO);
    assert_eq!(
        at.user_seen(&mut browser, "honest"),
        json!({ "id": ADA, "groups": ["devops"] }),
        "the renewal went on to its end: the session holds, with the new Groups"
    );
    assert_eq!(
        at.provider.grants("refresh_token").len(),
        1,
        "one refresh grant for that renewal"
    );
    // The refresh token the provider replaced was kept: the next renewal
    // works too (the provider refuses a used one).
    let renewed = Instant::now();
    at.provider.set_groups(ADA, &["audit"]);
    wait_past(renewed, Duration::from_secs(3));
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        json!(["audit"]),
        "the renewal after it works"
    );
    assert_eq!(at.provider.grants("refresh_token").len(), 2);
    assert!(
        at.logged("session-ended").is_empty(),
        "no session ended: {:?}",
        at.logged("session-ended")
    );
}

fn status(code: u16, error: &str, description: &str) -> TokenStatus {
    TokenStatus {
        status: code,
        error: error.to_owned(),
        description: description.to_owned(),
    }
}

#[test]
fn a_408_429_or_5xx_at_a_renewal_is_503_and_the_session_stays() {
    for code in [408, 429, 500, 503] {
        let at = renewing_every(2);
        let mut browser = at.browser_as(ADA);
        let signed_in = Instant::now();
        at.sign_in(&mut browser, "honest", "/");
        at.provider.set_groups(ADA, &["devops"]);
        wait_past(signed_in, Duration::from_secs(2));
        at.provider
            .token_status(Some(status(code, "temporarily_unavailable", "try later")));
        let answer = at.get_json(&mut browser, "honest", "/");
        assert_eq!(answer.status, 503, "{code}\n{}", answer.describe());
        assert!(
            answer.header("retry-after").is_some(),
            "{code}\n{}",
            answer.describe()
        );
        at.vm.records_of(&answer).assert_no_application_code_ran();
        at.assert_logged(
            "identity-provider-unanswered",
            &[
                ("call", json!("token")),
                ("problem", json!("status")),
                ("status", json!(code)),
            ],
        );
        at.provider.token_status(None);
        assert_eq!(
            at.user_seen(&mut browser, "honest")["groups"],
            json!(["devops"]),
            "{code}: the session stayed, and is renewed once the provider answers"
        );
        assert!(at.logged("session-ended").is_empty(), "{code}");
    }
}

#[test]
fn another_4xx_at_a_renewal_ends_the_session_and_its_description_is_never_logged() {
    let at = renewing_every(2);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    wait_past(signed_in, Duration::from_secs(2));
    let description = "harness-description-Zq81vB-never-logged";
    at.provider
        .token_status(Some(status(400, "invalid_request", description)));
    at.assert_no_session(
        &mut browser,
        "honest",
        "a 400 invalid_request at the renewal",
    );
    at.assert_logged("session-ended", &[("reason", json!("refused"))]);
    at.assert_logged(
        "identity-provider-refused",
        &[("status", json!(400)), ("error", json!("invalid_request"))],
    );
    at.assert_none_in_log(
        &[description.to_owned()],
        "the provider's error_description",
    );
}

#[test]
fn after_the_client_secret_changes_a_session_ends_at_its_next_renewal() {
    let at = renewing_every(8);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    let at = at.restarted_with_secret("harness-second-secret-Hk3d9Qw7");
    browser.reach(at.vm.address());
    assert_eq!(
        at.user_seen(&mut browser, "honest")["id"],
        ADA,
        "the session holds until its renewal"
    );
    wait_past(signed_in, Duration::from_secs(8));
    at.assert_no_session(&mut browser, "honest", "after the client secret changed");
    at.assert_logged(
        "session-ended",
        &[("reason", json!("unreadable-refresh-token"))],
    );
}
