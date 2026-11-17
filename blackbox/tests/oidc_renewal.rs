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

use blackbox::oidc::{Outage, TokenAnswer, TokenFault, TokenStatus};
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace};
use blackbox::{Request, Response};
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

/// Signs Ada in, then starts the program again on the same platform
/// database with the same secret, with `before_start` done to the provider
/// first (so that the program, which holds no configuration after the
/// start, meets it at its first call), and waits until the session's
/// renewal is due. Returns the installation and Ada's browser.
fn renewal_due_after_a_start(
    seconds: u64,
    before_start: impl FnOnce(&Installation),
) -> (Installation, blackbox::browser::Browser) {
    let at = renewing_every(seconds);
    let mut browser = at.browser_as(ADA);
    let signed_in = Instant::now();
    at.sign_in(&mut browser, "honest", "/");
    at.provider.set_groups(ADA, &["devops"]);
    before_start(&at);
    let at = at.restarted();
    browser.reach(at.vm.address());
    wait_past(signed_in, Duration::from_secs(seconds));
    (at, browser)
}

/// Fails unless `answer` is `503` with `Retry-After`, `took` under 12 s.
fn assert_503_in_time(answer: &Response, took: Duration, context: &str) {
    assert_eq!(answer.status, 503, "{context}\n{}", answer.describe());
    assert!(
        answer.header("retry-after").is_some(),
        "{context}: expected Retry-After\n{}",
        answer.describe()
    );
    assert!(
        took < Duration::from_secs(12),
        "{context}: a renewal ends within 10 s, all its calls together; the answer took {took:?}"
    );
}

#[test]
fn a_renewal_that_must_first_fetch_a_configuration_that_is_held_ends_within_10_seconds() {
    let (at, mut browser) = renewal_due_after_a_start(3, |at| {
        at.provider.set_outage(Outage::Hang);
    });
    let began = Instant::now();
    let answer = at.get_json(&mut browser, "honest", "/");
    assert_503_in_time(&answer, began.elapsed(), "the configuration held");
    at.vm.records_of(&answer).assert_no_application_code_ran();
    assert!(
        !at.provider
            .requests_to("/.well-known/openid-configuration")
            .is_empty(),
        "the program, started again, asked for the configuration"
    );
    at.provider.set_outage(Outage::None);
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        json!(["devops"]),
        "once the provider answers, the session (which stayed) is renewed"
    );
}

#[test]
fn a_renewal_whose_configuration_is_slow_and_token_endpoint_held_ends_within_10_seconds_in_all() {
    // 6 s for the configuration and a token endpoint that does not answer
    // within 10 s: 10 s for each call would be 16 s.
    let (at, mut browser) = renewal_due_after_a_start(3, |at| {
        at.provider.config_delay(Duration::from_secs(6));
        at.provider.token_delay(Duration::from_secs(20));
    });
    let began = Instant::now();
    let answer = at.get_json(&mut browser, "honest", "/");
    assert_503_in_time(
        &answer,
        began.elapsed(),
        "a slow configuration, then a held token endpoint",
    );
    at.provider.config_delay(Duration::ZERO);
    at.provider.token_delay(Duration::ZERO);
    assert_eq!(
        at.user_seen(&mut browser, "honest")["groups"],
        json!(["devops"]),
        "once the provider answers, the session (which stayed) is renewed"
    );
}

#[test]
fn at_most_256_sessions_are_renewed_at_once_and_meanwhile_others_are_answered_at_once() {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    const PEOPLE: usize = 14;
    const EACH: usize = 20;
    const RENEWING: usize = 256;
    const RENEWAL: u64 = 6;
    let test_began = Instant::now();
    let people: Vec<blackbox::oidc::Person> = (0..PEOPLE)
        .map(|i| blackbox::oidc::Person::new(&format!("00u-many-{i:02}")))
        .collect();
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &people,
        &["--session-renewal", &RENEWAL.to_string()],
    );
    // 280 sessions, 20 for each of 14 people, each in a browser of its own;
    // kept as the Cookie header honest is sent with.
    let first_sign_in = Instant::now();
    let cookies: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let at = &at;
                scope.spawn(move || {
                    let mut cookies = Vec::new();
                    for n in (t..PEOPLE * EACH).step_by(8) {
                        let mut browser = at.browser_as(&format!("00u-many-{:02}", n % PEOPLE));
                        at.sign_in(&mut browser, "honest", "/");
                        cookies.push(browser.jar.header_for(&at.app_url("honest", "/")).unwrap());
                    }
                    cookies
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let signed_in = first_sign_in.elapsed();
    assert!(
        signed_in < Duration::from_secs(RENEWAL - 1),
        "the sign-ins took {signed_in:?}, too long to make them all before the first renewal"
    );
    let host = format!("honest.{}:{}", blackbox::DOMAIN, at.port());
    let raw = |cookie: &str| {
        format!(
            "GET / HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        )
    };
    // Every session due; the provider holds every call.
    wait_past(first_sign_in, Duration::from_secs(RENEWAL) + signed_in);
    at.provider.set_outage(Outage::Park);
    let fired = Instant::now();
    let held: Vec<(TcpStream, Instant)> = cookies[..RENEWING]
        .iter()
        .map(|cookie| {
            let mut stream = TcpStream::connect(at.vm.address()).unwrap();
            stream.write_all(raw(cookie).as_bytes()).unwrap();
            (stream, Instant::now())
        })
        .collect();
    let sent_in = fired.elapsed();
    // Each held client stays connected and takes its answer when it comes.
    let (held_answers, meanwhile, parked, still_held) = thread::scope(|scope| {
        let reader = scope.spawn(move || {
            held.into_iter()
                .map(|(mut stream, sent)| {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(25)))
                        .unwrap();
                    let mut answer = Vec::new();
                    drop(stream.read_to_end(&mut answer));
                    let line = String::from_utf8_lossy(&answer)
                        .lines()
                        .next()
                        .unwrap_or("no answer")
                        .to_owned();
                    let retry_after = String::from_utf8_lossy(&answer)
                        .to_ascii_lowercase()
                        .contains("\r\nretry-after:");
                    (line, retry_after, sent.elapsed())
                })
                .collect::<Vec<_>>()
        });
        while at.provider.parked() < RENEWING && fired.elapsed() < Duration::from_secs(4) {
            thread::sleep(Duration::from_millis(20));
        }
        let parked = at.provider.parked();
        let timed = |what: &'static str, request: Request| {
            let began = Instant::now();
            let answer = request.timeout(Duration::from_secs(25)).try_send();
            (what, answer, began.elapsed())
        };
        let meanwhile = vec![
            timed(
                "a request of nobody signed in",
                Request::new(at.vm.address(), "/", &host).header("accept", "application/json"),
            ),
            timed(
                "a page of nobody signed in",
                at.browser().navigation(&at.app_url("honest", "/")),
            ),
            timed(
                "the API with no credential",
                at.vm.api_request("/v1/applications"),
            ),
            timed(
                "one more due session",
                Request::new(at.vm.address(), "/", &host)
                    .header("accept", "application/json")
                    .header("cookie", &cookies[RENEWING + 10]),
            ),
        ];
        let still_held = fired.elapsed();
        let joined = reader.join().unwrap();
        // The provider answers again.
        at.provider.set_outage(Outage::None);
        (joined, meanwhile, parked, still_held)
    });
    let context = format!(
        "{RENEWING} requests held (sent in {sent_in:?}, {parked} calls held at the provider; the last request meanwhile ended {still_held:?} after the first held one was sent)"
    );
    for ((what, answer, took), want) in meanwhile.iter().zip([401, 302, 401, 503]) {
        let answer = answer
            .as_ref()
            .unwrap_or_else(|e| panic!("{what}, while {context}: {e}"));
        assert!(
            answer.status == want && *took < Duration::from_secs(1),
            "{what}, while {context}: expected {want} at once, got {} in {took:?}\n{}",
            answer.status,
            answer.describe()
        );
        if want == 503 {
            assert!(
                answer.header("retry-after").is_some(),
                "{what}: expected Retry-After\n{}",
                answer.describe()
            );
            at.vm.records_of(answer).assert_no_application_code_ran();
        }
    }
    let late: Vec<_> = held_answers
        .iter()
        .filter(|(line, retry_after, took)| {
            line != "HTTP/1.1 503 Service Unavailable"
                || !retry_after
                || *took > Duration::from_secs(12)
        })
        .take(5)
        .collect();
    assert!(
        late.is_empty(),
        "each held request ends 503 with Retry-After within about 12 s of its start; some did not: {late:?}"
    );
    for (n, cookie) in cookies[..20].iter().chain(&cookies[RENEWING..]).enumerate() {
        let answer = Request::new(at.vm.address(), "/", &host)
            .header("accept", "application/json")
            .header("cookie", cookie)
            .timeout(Duration::from_secs(15))
            .send();
        assert_eq!(
            answer.status,
            200,
            "once the provider answers, the session {n} is served\n{}",
            answer.describe()
        );
    }
    eprintln!(
        "280 sign-ins in {signed_in:?}; {RENEWING} requests sent in {sent_in:?}; {parked} calls held; the test took {:?}",
        test_began.elapsed()
    );
}
