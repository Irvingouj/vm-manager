//! A sign-in under way (CONTRACT.md, "Sign-in and sessions: The way
//! through sign-in"): the platform keeps nothing of it, so sign-ins begun
//! and never finished stop no other, and one under way goes on across a
//! start of the program; its `state` is good only in the browser that
//! began it; the bounds of `next` and `state`; two pages of one Application
//! signing in at once; where a failure page's link goes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;

use blackbox::browser::{query_param, set_cookies};
use blackbox::sessions::{
    ADA, GRACE, Installation, Link, SESSION_COOKIE, SIGN_IN_COOKIE, ada, assert_failure_page,
    assert_link,
};
use blackbox::{DOMAIN, Request};

fn two_applications() -> Installation {
    Installation::start(&[("honest", "honest.js"), ("ledger", "honest.js")])
}

#[test]
fn ten_thousand_sign_ins_begun_and_never_finished_stop_no_other() {
    let at = two_applications();
    const BEGUN: usize = 10_050;
    const THREADS: usize = 8;
    let host = format!("{}:{}", at.auth_host(), at.port());
    let address = at.vm.address();
    let refused: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let host = host.as_str();
                scope.spawn(move || {
                    let mut refused = Vec::new();
                    for i in (t..BEGUN).step_by(THREADS) {
                        let answer = Request::new(
                            address,
                            &format!("/sign-in?application=honest&next=%2F&state=begun-{i}"),
                            host,
                        )
                        .send();
                        if answer.status != 302 && refused.len() < 5 {
                            refused.push(format!("sign-in {i}: {}", answer.describe()));
                        }
                    }
                    refused
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    assert!(
        refused.is_empty(),
        "expected each of {BEGUN} sign-ins begun to be sent to the provider (302); some were not:\n{}",
        refused.join("\n")
    );
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
}

#[test]
fn a_sign_in_under_way_goes_on_after_the_program_is_started_again() {
    // A public port of its own, so that the addresses (the redirect address
    // registered at the provider among them) stay the same across the
    // start.
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[ada()],
        &["--public-port", "8080"],
    );
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/books");
    let back = steps.last().unwrap().clone();
    let at = at.restarted();
    browser.reach(at.vm.address());
    let callback = browser.follow(&back);
    assert_eq!(
        callback.response.status,
        302,
        "the callback after the program was started again\n{}",
        callback.response.describe()
    );
    let mut rest = vec![callback];
    while rest.last().unwrap().is_redirect() && rest.len() < 5 {
        let next = browser.follow(rest.last().unwrap());
        rest.push(next);
    }
    at.assert_landed(&rest, "honest", "/books");
}

#[test]
fn the_state_the_provider_got_is_refused_in_another_browser_that_began_its_own() {
    let at = two_applications();
    let mut ada_browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut ada_browser, "honest", "/");
    let callback = steps.last().unwrap().location();
    // Grace's browser began a sign-in of its own: it holds its own cookie
    // on the sign-in address.
    let mut grace_browser = at.browser_as(GRACE);
    at.walk_to_callback(&mut grace_browser, "honest", "/");
    let answer = grace_browser.open(&callback);
    assert_failure_page(
        &answer.response,
        &[400],
        Link::NowhereOrTo(&at.app_origin("honest")),
        &at.provider.issued(),
        "Ada's state in Grace's browser",
    );
    assert!(
        grace_browser
            .jar
            .get(&at.auth_host(), SESSION_COOKIE)
            .is_none()
    );
}

#[test]
fn a_state_changed_by_one_character_is_refused_and_its_page_links_nowhere() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let callback = steps.last().unwrap().location();
    let state = query_param(&callback, "state").unwrap();
    let mut changed: Vec<char> = state.chars().collect();
    let at_middle = changed.len() / 2;
    changed[at_middle] = if changed[at_middle] == 'A' { 'B' } else { 'A' };
    let changed: String = changed.into_iter().collect();
    let mut forged = callback.clone();
    forged.set_query(Some(&format!(
        "code={}&state={}",
        query_param(&callback, "code").unwrap(),
        url::form_urlencoded::byte_serialize(changed.as_bytes()).collect::<String>()
    )));
    let answer = browser.open(&forged);
    assert_failure_page(
        &answer.response,
        &[400],
        Link::Nowhere,
        &at.provider.issued(),
        "a state changed by one character",
    );
    assert!(browser.jar.get(&at.auth_host(), SESSION_COOKIE).is_none());
    assert!(
        at.provider.grants("authorization_code").is_empty(),
        "the code is not exchanged for a state that does not open"
    );
}

#[test]
fn a_next_longer_than_1024_bytes_through_the_provider_lands_on_the_root() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let long = format!("/{}", "a".repeat(1100));
    let steps = browser.navigate(&at.app_url("honest", &long));
    at.assert_landed(&steps, "honest", "/");
}

#[test]
fn with_a_platform_session_a_next_of_2000_bytes_is_kept() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let long = format!("/{}", "b".repeat(1999));
    assert_eq!(long.len(), 2000);
    at.sign_in(&mut browser, "ledger", &long);
}

#[test]
fn the_state_of_step_one_is_at_most_128_bytes() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    let sign_in = |state: &str| {
        let mut url = at.auth_url("/sign-in");
        url.query_pairs_mut()
            .append_pair("application", "honest")
            .append_pair("next", "/");
        // An empty `state` here leaves it out.
        if !state.is_empty() {
            url.query_pairs_mut().append_pair("state", state);
        }
        url
    };
    let check = |browser: &mut blackbox::browser::Browser, signed_in: &str| {
        for (state, want) in [
            ("s".repeat(128), 302),
            ("s".repeat(129), 400),
            (String::new(), 400),
        ] {
            let step = browser.open(&sign_in(&state));
            assert_eq!(
                step.response.status,
                want,
                "{signed_in}: a state of {} bytes\n{}",
                state.len(),
                step.response.describe()
            );
        }
        let mut empty = at.auth_url("/sign-in");
        empty
            .query_pairs_mut()
            .append_pair("application", "honest")
            .append_pair("next", "/")
            .append_pair("state", "");
        let step = browser.open(&empty);
        assert_eq!(step.response.status, 400, "{signed_in}: an empty state");
    };
    check(&mut browser, "without a platform session");
    at.sign_in(&mut browser, "honest", "/");
    check(&mut browser, "with a platform session");
}

#[test]
fn two_pages_of_one_application_signing_in_at_once_in_one_browser_both_land() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    // Two step 1s; then the first through the provider to its one-time
    // code, the second through step 2 with the platform session that made.
    let first = browser.open(&at.app_url("honest", "/first"));
    let second = browser.open(&at.app_url("honest", "/second"));
    assert_eq!(first.response.status, 302);
    assert_eq!(second.response.status, 302);
    let to_provider = browser.follow(&first);
    let back = browser.follow(&to_provider);
    let callback = browser.follow(&back);
    assert_eq!(
        callback.location().path(),
        "/_vm/session",
        "{}",
        callback.response.describe()
    );
    let second_sign_in = browser.follow(&second);
    assert_eq!(
        second_sign_in.location().path(),
        "/_vm/session",
        "{}",
        second_sign_in.response.describe()
    );
    for (step, next) in [(&callback, "/first"), (&second_sign_in, "/second")] {
        let session = browser.follow(step);
        assert_eq!(
            session.response.status,
            302,
            "step 4 of the page {next}\n{}",
            session.response.describe()
        );
        assert_eq!(session.location(), at.app_url("honest", next));
        assert!(
            set_cookies(&session.response)
                .iter()
                .all(|c| c.name != SIGN_IN_COOKIE),
            "step 4 leaves the step-1 cookie to run out\n{}",
            session.response.describe()
        );
    }
    assert!(
        browser
            .jar
            .get(&format!("honest.{DOMAIN}"), SIGN_IN_COOKIE)
            .is_some()
    );
}

#[test]
fn a_failure_page_links_to_the_application_only_when_it_is_known() {
    let at = two_applications();
    let mut browser = at.browser_as(ADA);
    // A name no Application could have: no link.
    let impossible =
        browser.open(&at.auth_url("/sign-in?application=evil.example&next=%2F&state=s"));
    assert_eq!(impossible.response.status, 400);
    assert_link(
        &impossible.response,
        Link::Nowhere,
        "a name no Application could have",
    );
    // A callback without a state: no link.
    let bare = browser.open(&at.auth_url("/callback?code=x"));
    assert_failure_page(
        &bare.response,
        &[400],
        Link::Nowhere,
        &[],
        "a callback without a state",
    );
    // The provider's error for a sign-in to honest: a link to honest.
    at.provider.deny(true);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let denied = browser.follow(steps.last().unwrap());
    assert_failure_page(
        &denied.response,
        &[400],
        Link::To(&at.app_origin("honest")),
        &[],
        "the provider's error",
    );
    at.provider.deny(false);
    // Signed in, an unknown Application: a 404 page without a link.
    at.sign_in(&mut browser, "honest", "/");
    let unknown = browser.open(&at.auth_url("/sign-in?application=nowhere&next=%2F&state=s"));
    assert_eq!(unknown.response.status, 404);
    assert_link(
        &unknown.response,
        Link::Nowhere,
        "the 404 of an unknown Application",
    );
}
