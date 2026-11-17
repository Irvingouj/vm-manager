//! The cookies (CONTRACT.md, "Sign-in and sessions: The cookies"): one per
//! address, none spanning the wildcard domain, random, valid only where
//! they were set, never seen by Application code, never set by it.
//!
//! Acceptance criteria: "The cookie for one Application's subdomain is not
//! sent to, and is not valid on, another Application's subdomain",
//! "Application code never sees the session cookie".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::Request;
use blackbox::browser::{query_param, set_cookies};
use blackbox::records::event;
use blackbox::sessions::{
    ADA, Installation, SESSION_COOKIE, SIGN_IN_COOKIE, assert_256_bits, the_set_cookie,
};
use blackbox::sql::Sql;
use blackbox::{DOMAIN, Response};
use serde_json::{Value, json};

fn installation() -> Installation {
    Installation::start(&[
        ("honest", "honest.js"),
        ("ledger", "honest.js"),
        ("cookies", "cookies.js"),
    ])
}

/// A `GET` of `/` on the Application asking for JSON with exactly this
/// `Cookie` header, sent by hand.
fn with_cookie_header(at: &Installation, application: &str, cookie: &str) -> Response {
    Request::new(
        at.vm.address(),
        "/",
        &format!("{application}.{DOMAIN}:{}", at.port()),
    )
    .header("accept", "application/json")
    .header("cookie", cookie)
    .send()
}

#[test]
fn an_applications_cookie_is_not_sent_to_and_not_valid_on_another_application() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let honest = at.cookie_value(&browser, &format!("honest.{DOMAIN}"), SESSION_COOKIE);
    let platform = at.cookie_value(&browser, &at.auth_host(), SESSION_COOKIE);
    assert_eq!(
        browser.jar.header_for(&at.app_url("ledger", "/")),
        None,
        "a browser sends no cookie of honest's or the sign-in address's to ledger"
    );
    for (cookie, whose) in [(&honest, "honest's"), (&platform, "the sign-in address's")] {
        let answer = with_cookie_header(&at, "ledger", &format!("{SESSION_COOKIE}={cookie}"));
        assert_eq!(
            answer.status,
            401,
            "{whose} cookie sent by hand to ledger is no session\n{}",
            answer.describe()
        );
        at.vm.records_of(&answer).assert_no_application_code_ran();
    }
    let platform_on_app =
        with_cookie_header(&at, "honest", &format!("{SESSION_COOKIE}={platform}"));
    assert_eq!(
        platform_on_app.status,
        401,
        "the sign-in address's cookie sent by hand to honest is no session\n{}",
        platform_on_app.describe()
    );
    let me = Request::new(
        at.vm.address(),
        "/me",
        &format!("{}:{}", at.auth_host(), at.port()),
    )
    .header("cookie", format!("{SESSION_COOKIE}={honest}"))
    .send();
    assert_eq!(
        me.status,
        401,
        "honest's cookie sent by hand to the sign-in address is no session\n{}",
        me.describe()
    );
    // While honest's own cookie is valid on honest.
    let own = with_cookie_header(&at, "honest", &format!("{SESSION_COOKIE}={honest}"));
    assert_eq!(own.status, 200, "{}", own.describe());
    assert_eq!(own.json()["user"]["id"], ADA);
}

#[test]
fn every_cookie_value_is_256_random_bits_and_no_two_are_alike() {
    let at = installation();
    let mut values = Vec::new();
    for _ in 0..2 {
        let mut browser = at.browser_as(ADA);
        let steps = at.sign_in(&mut browser, "honest", "/");
        let sign_in = the_set_cookie(&steps[0].response, SIGN_IN_COOKIE, "step 1");
        let platform = the_set_cookie(&steps[3].response, SESSION_COOKIE, "the callback");
        let application = the_set_cookie(&steps[4].response, SESSION_COOKIE, "/_vm/session");
        for (cookie, which) in [
            (&platform, "the platform session's cookie"),
            (&application, "the Application's session cookie"),
        ] {
            assert_256_bits(&cookie.value, which);
        }
        assert_256_bits(&sign_in.value, "step 1's cookie");
        values.extend([sign_in.value, platform.value, application.value]);
    }
    let mut distinct = values.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        values.len(),
        "expected every cookie value to differ: {values:?}"
    );
}

#[test]
fn application_code_never_sees_the_platforms_cookies_and_sees_its_own_byte_for_byte() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "cookies", "/");
    let session = at.cookie_value(&browser, &format!("cookies.{DOMAIN}"), SESSION_COOKIE);
    let header = format!(
        "theme=dark; {SESSION_COOKIE}={session}; lang=\"en GB\"; __Host-vm-session=x1; {SIGN_IN_COOKIE}=x2; __Host-vm-sign-in=x3; VM-Session=mine; Vm-Sign-In=too; __host-vm-session=lower; odd=a%20b=c; last=1"
    );
    let answer = with_cookie_header(&at, "cookies", &header);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    let body = answer.json();
    assert_eq!(body["user"]["id"], ADA, "the session is still read");
    let seen = body["cookie"].as_str().unwrap_or_else(|| {
        panic!("expected the code to see its own cookies, it saw no cookie header: {body}")
    });
    let pairs: Vec<&str> = seen.split(';').map(str::trim).collect();
    assert_eq!(
        pairs,
        vec![
            "theme=dark",
            "lang=\"en GB\"",
            "VM-Session=mine",
            "Vm-Sign-In=too",
            "__host-vm-session=lower",
            "odd=a%20b=c",
            "last=1"
        ],
        "expected the code to see its own cookies byte for byte (names matched exactly, letter case included) and none of the platform's; it saw {seen:?}"
    );
    for value in [session.as_str(), "x1", "x2", "x3"] {
        assert!(
            !seen.contains(&format!("={value}")),
            "expected no platform cookie's value in what the code saw: {seen:?}"
        );
    }
    let only_platform = with_cookie_header(
        &at,
        "cookies",
        &format!("{SESSION_COOKIE}={session}; {SIGN_IN_COOKIE}=x2"),
    );
    assert_eq!(only_platform.status, 200, "{}", only_platform.describe());
    let body = only_platform.json();
    assert_eq!(
        body["cookie"],
        Value::Null,
        "a Cookie header left empty is left out: {body}"
    );
    assert!(body["headers"].get("cookie").is_none(), "{body}");
}

/// Asks `fixtures/cookies.js` to answer with this one `Set-Cookie`.
fn set_by_code(
    at: &Installation,
    browser: &mut blackbox::browser::Browser,
    line: &str,
) -> Response {
    let mut target = at.app_url("cookies", "/set-cookie");
    target.query_pairs_mut().append_pair("line", line);
    let request = browser
        .request("GET", &target)
        .header("accept", "application/json");
    let answer = browser.send(&target, &request);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_eq!(answer.json()["set"], line, "the code ran and set {line:?}");
    answer
}

#[test]
fn a_set_cookie_naming_a_platform_cookie_or_with_a_domain_is_dropped_and_logged() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "cookies", "/");
    let dropped = [
        ("vm-session=forged; Path=/", "platform-name"),
        ("__Host-vm-session=forged; Path=/; Secure", "platform-name"),
        ("vm-sign-in=forged; Path=/", "platform-name"),
        ("__Host-vm-sign-in=forged; Path=/; Secure", "platform-name"),
        ("wide=1; Domain=vm.test; Path=/", "domain"),
        ("own=1; domain=cookies.vm.test", "domain"),
    ];
    for (line, _) in &dropped {
        let answer = set_by_code(&at, &mut browser, line);
        assert!(
            answer.headers_named("set-cookie").is_empty(),
            "expected {line:?} to be dropped from the answer\n{}",
            answer.describe()
        );
    }
    let kept = set_by_code(&at, &mut browser, "ordinary=1; Path=/; HttpOnly");
    assert_eq!(
        kept.headers_named("set-cookie"),
        vec!["ordinary=1; Path=/; HttpOnly"],
        "an ordinary cookie is kept as the code set it"
    );
    for reason in ["platform-name", "domain"] {
        let want = dropped.iter().filter(|(_, r)| *r == reason).count();
        let logged = at
            .vm
            .records()
            .iter()
            .filter(|r| {
                event(r) == "set-cookie-dropped"
                    && r["application"] == "cookies"
                    && r["reason"] == reason
            })
            .count();
        assert_eq!(
            logged, want,
            "expected {want} `set-cookie-dropped` records with reason {reason:?}"
        );
    }
    // The session was not overwritten, and the ordinary cookie comes back
    // to the code.
    let answer = at.get_json(&mut browser, "cookies", "/");
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_eq!(answer.json()["user"]["id"], ADA);
    assert_eq!(answer.json()["cookie"], json!("ordinary=1"));
}

#[test]
fn the_sign_in_cookie_and_state_differ_for_each_sign_in_begun() {
    let at = installation();
    let mut browser = at.browser();
    let first = browser.open(&at.app_url("honest", "/"));
    let second = browser.open(&at.app_url("ledger", "/"));
    let state_one = query_param(&first.location(), "state").unwrap();
    let state_two = query_param(&second.location(), "state").unwrap();
    assert_ne!(state_one, state_two);
    for step in [&first, &second] {
        assert!(
            set_cookies(&step.response).iter().all(|c| !c.has("domain")),
            "{}",
            step.response.describe()
        );
    }
    assert_eq!(
        browser
            .jar
            .get(&format!("honest.{DOMAIN}"), SIGN_IN_COOKIE)
            .map(|c| c.value.clone()),
        Some(state_one)
    );
    assert_eq!(
        browser
            .jar
            .get(&format!("ledger.{DOMAIN}"), SIGN_IN_COOKIE)
            .map(|c| c.value.clone()),
        Some(state_two)
    );
}

#[test]
fn the_platform_database_holds_no_cookie_value() {
    let at = installation();
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    at.sign_in(&mut browser, "ledger", "/");
    let values = [
        at.cookie_value(&browser, &at.auth_host(), SESSION_COOKIE),
        at.cookie_value(&browser, &format!("honest.{DOMAIN}"), SESSION_COOKIE),
        at.cookie_value(&browser, &format!("ledger.{DOMAIN}"), SESSION_COOKIE),
    ];
    let db = Sql::connect(at.vm.platform().url())
        .unwrap_or_else(|e| panic!("cannot reach the platform database: {e}"));
    let tables = db.batch(
        "SELECT format('%I.%I', table_schema, table_name) FROM information_schema.tables \
         WHERE table_type = 'BASE TABLE' AND table_schema NOT IN ('pg_catalog', 'information_schema')",
    );
    assert!(
        !tables.is_empty(),
        "expected the platform database to have tables"
    );
    for value in &values {
        assert_256_bits(value, "a session cookie");
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+/=".contains(c)),
            "the cookie's value is safe to put in a query: {value:?}"
        );
        for table in tables
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
        {
            let found = db
                .value(&format!(
                    "SELECT count(*) FROM {table} AS t WHERE t::text LIKE '%{value}%'"
                ))
                .unwrap_or_default();
            assert_eq!(
                found, "0",
                "expected the platform database to hold only a cookie's SHA-256, but {table} holds a cookie's value"
            );
        }
    }
}
