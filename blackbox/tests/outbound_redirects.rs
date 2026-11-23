//! Redirects (CONTRACT.md, "Outbound calls: Who may call what", the last
//! bullet): each one followed is decided as a call of its own with an
//! Operation log entry of its own; at most 5; one that is refused rejects the
//! whole `fetch` with that refusal and `redirected_from`; `authorization`,
//! `cookie` and `proxy-authorization` do not go on to another host or port;
//! `303`, and `301` or `302` after a `POST`, go on as `GET` with no body;
//! `307` and `308` send the method and the body again, which a stream cannot
//! (`body-not-replayable`); `manual` and `error`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::outbound::{
    Origin, Outbound, Reply, assert_failure, assert_fetch_entry, assert_refused, assert_resolved,
    refusal_of, sha256,
};
use serde_json::json;

const SOON: Duration = Duration::from_secs(10);

/// A program, and two origins, both listed.
fn two() -> (Outbound, Origin, Origin) {
    let o = Outbound::start();
    let a = o.origin();
    let b = o.origin();
    o.allow(&a.host());
    o.allow(&b.host());
    (o, a, b)
}

#[test]
fn a_redirect_to_a_listed_host_is_followed_and_each_call_has_an_entry_of_its_own() {
    let (o, a, b) = two();
    a.always(Reply::redirect(302, &b.url("/final?from=a")));
    b.always(Reply::ok(b"the end"));
    let response = o.call_response(&json!({ "url": a.url("/start") }));
    let report = o.report(&response);
    assert_resolved(&report, 200, "a redirect to a listed host");
    assert_eq!(report["resolved"]["text"], "the end", "{report}");
    assert_eq!(report["resolved"]["redirected"], true, "{report}");
    assert_eq!(
        report["resolved"]["url"],
        json!(b.url("/final?from=a")),
        "expected `url` the last address called, got {report}"
    );
    assert_eq!(a.the_request().path(), "/start");
    assert_eq!(b.the_request().target, "/final?from=a");
    let entries = o.entries_of(&response);
    assert_eq!(
        entries.len(),
        2,
        "expected one entry for the call and one for the redirect followed, got {entries:?}"
    );
    assert_fetch_entry(
        &entries[0],
        "allowed",
        Some(&a.host()),
        None,
        None,
        "the call",
    );
    assert_fetch_entry(
        &entries[1],
        "allowed",
        Some(&b.host()),
        None,
        None,
        "the redirect",
    );
}

#[test]
fn every_kind_of_redirect_with_a_location_is_followed_a_relative_one_too() {
    let (o, a, _b) = two();
    for status in [301, 302, 303, 307, 308] {
        a.script(move |seen| {
            if seen.path() == "/hop" {
                Reply::redirect(status, "/landed?by=relative")
            } else {
                Reply::ok(b"landed")
            }
        });
        let report = o.call(&json!({ "url": a.url("/hop") }));
        assert_resolved(&report, 200, &format!("a {status}"));
        assert_eq!(report["resolved"]["text"], "landed", "{status}: {report}");
        assert_eq!(
            report["resolved"]["url"],
            json!(a.url("/landed?by=relative")),
            "{status}: {report}"
        );
    }
    // A 3xx with no `location` is the answer.
    a.always(Reply::new(302).body(b"nowhere"));
    let report = o.call(&json!({ "url": a.url("/no-location") }));
    assert_resolved(&report, 302, "a 302 with no location");
    assert_eq!(report["resolved"]["redirected"], false, "{report}");
}

#[test]
fn a_redirect_to_a_host_not_on_the_list_rejects_the_whole_fetch_and_nothing_reaches_it() {
    let o = Outbound::start();
    let a = o.origin();
    let unlisted = o.origin();
    o.allow(&a.host());
    a.always(Reply::redirect(302, &unlisted.url("/secret")));
    let response = o.call_response(&json!({ "url": a.url("/start") }));
    let report = o.report(&response);
    assert_refused(
        &report,
        "host-not-listed",
        Some(&unlisted.host()),
        "a redirect to an unlisted host",
    );
    assert_eq!(
        refusal_of(&report)["refusal"]["redirected_from"],
        json!(a.host()),
        "expected `redirected_from` the host that sent it, got {report}"
    );
    unlisted.assert_untouched("a redirect to an unlisted host");
    let entries = o.entries_of(&response);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_fetch_entry(
        &entries[0],
        "allowed",
        Some(&a.host()),
        None,
        None,
        "the call",
    );
    assert_fetch_entry(
        &entries[1],
        "refused",
        Some(&unlisted.host()),
        Some("host-not-listed"),
        None,
        "the redirect",
    );
}

#[test]
fn a_redirect_to_http_to_the_platform_or_to_an_address_never_called_is_refused() {
    let o = Outbound::start();
    let a = o.origin();
    let not_opened = Origin::start_at("127.0.0.2".parse().unwrap(), o.cert().wire());
    o.allow(&a.host());
    o.allow(&format!("127.0.0.2:{}", not_opened.port()));
    o.allow("169.254.169.254");
    let cases = [
        (format!("http://localhost:{}/plain", a.port()), "not-https"),
        ("https://api.vm.test/v1/me".to_owned(), "platform-itself"),
        (
            format!("https://127.0.0.2:{}/inside", not_opened.port()),
            "address-not-allowed",
        ),
        (
            "https://169.254.169.254/latest/meta-data/".to_owned(),
            "address-not-allowed",
        ),
    ];
    for (location, reason) in cases {
        a.always(Reply::redirect(302, &location));
        let report = o.call(&json!({ "url": a.url("/start") }));
        assert_refused(&report, reason, None, &location);
        assert_eq!(
            refusal_of(&report)["refusal"]["redirected_from"],
            json!(a.host()),
            "{location}: expected `redirected_from`, got {report}"
        );
    }
    not_opened.assert_untouched("a redirect to an address not opened");
}

#[test]
fn the_sixth_redirect_is_a_limit_redirects_failure_and_five_are_followed() {
    let (o, a, _b) = two();
    // Five hops and then an answer: followed.
    a.script(|seen| {
        let n: u32 = seen
            .target
            .split("n=")
            .nth(1)
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        if n < 5 {
            Reply::redirect(302, &format!("/hop?n={}", n + 1))
        } else {
            Reply::ok(b"after five")
        }
    });
    let response = o.call_response(&json!({ "url": a.url("/hop?n=0") }));
    let report = o.report(&response);
    assert_resolved(&report, 200, "five redirects");
    assert_eq!(report["resolved"]["text"], "after five", "{report}");
    assert_eq!(
        o.entries_of(&response).len(),
        6,
        "expected an entry for the call and one for each of the five redirects"
    );

    // Six: the sixth is not followed.
    let before = a.requests().len();
    a.script(|seen| {
        let n: u32 = seen
            .target
            .split("n=")
            .nth(1)
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        Reply::redirect(302, &format!("/hop?n={}", n + 1))
    });
    let response = o.call_response(&json!({ "url": a.url("/hop?n=0") }));
    let report = o.report(&response);
    assert_failure(&report, "limit", "a sixth redirect");
    assert_eq!(
        report["rejected"]["failure"]["limit"], "redirects",
        "expected `limit` `redirects`, got {report}"
    );
    let seen = a.wait_requests(before + 6, SOON);
    assert_eq!(
        seen.len() - before,
        6,
        "expected the call and five redirects to arrive, and no more"
    );
    let entries = o.entries_of(&response);
    assert_eq!(entries.len(), 6, "{entries:?}");
    let last = entries.last().unwrap();
    assert_eq!(
        (last["outcome"].as_str(), last["error"].as_str()),
        (Some("failed"), Some("redirects")),
        "expected the last entry failed with `redirects`, got {last}"
    );
}

#[test]
fn credentials_are_not_sent_on_to_another_host_or_port_and_are_sent_on_to_the_same_one() {
    let (o, a, b) = two();
    let headers = json!({
        "authorization": "Bearer secret-token", "cookie": "sid=secret-cookie",
        "proxy-authorization": "Basic nope", "x-keep": "kept",
    });
    // The same host and port: sent on.
    a.script(|seen| {
        if seen.path() == "/start" {
            Reply::redirect(302, "/same")
        } else {
            Reply::ok(b"same")
        }
    });
    let report = o.call(&json!({ "url": a.url("/start"), "headers": headers }));
    assert_resolved(&report, 200, "a redirect to the same host");
    let seen = a.wait_requests(2, SOON);
    for hop in &seen {
        assert_eq!(
            hop.header("authorization"),
            Some("Bearer secret-token"),
            "{}",
            hop.describe()
        );
        assert_eq!(
            hop.header("cookie"),
            Some("sid=secret-cookie"),
            "{}",
            hop.describe()
        );
        assert_eq!(hop.header("x-keep"), Some("kept"), "{}", hop.describe());
    }
    // Another port of the same name: not sent on; the other headers are.
    a.always(Reply::redirect(302, &b.url("/other-port")));
    b.always(Reply::ok(b"other"));
    let report = o.call(&json!({ "url": a.url("/start"), "headers": headers }));
    assert_resolved(&report, 200, "a redirect to another port");
    let landed = b.the_request();
    for gone in ["authorization", "cookie", "proxy-authorization"] {
        assert!(
            landed.header(gone).is_none(),
            "expected `{gone}` not sent on to another port, got {}",
            landed.describe()
        );
    }
    assert_eq!(
        landed.header("x-keep"),
        Some("kept"),
        "{}",
        landed.describe()
    );
}

#[test]
fn a_303_and_a_301_or_302_after_a_post_go_on_as_a_get_with_no_body() {
    let (o, a, b) = two();
    for (status, method) in [(301, "POST"), (302, "POST"), (303, "POST"), (303, "PUT")] {
        a.always(Reply::redirect(status, &b.url("/second")));
        let before = b.requests().len();
        let report = o.call(&json!({
            "url": a.url("/first"), "method": method,
            "headers": { "content-type": "text/plain" },
            "body": { "kind": "string", "text": "a body of the first call" },
        }));
        assert_resolved(&report, 200, &format!("{status} after {method}"));
        let first = a.requests().last().unwrap().clone();
        assert_eq!(first.method, method);
        assert_eq!(first.body_len, 24, "{}", first.describe());
        let second = &b.wait_requests(before + 1, SOON)[before];
        assert_eq!(
            second.method,
            "GET",
            "{status} after {method}: {}",
            second.describe()
        );
        assert_eq!(
            second.body_len,
            0,
            "{status} after {method}: {}",
            second.describe()
        );
        assert!(
            second.content_length.is_none_or(|n| n == 0) && !second.chunked,
            "{status} after {method}: expected no body, got {}",
            second.describe()
        );
    }
}

#[test]
fn a_307_and_a_308_send_the_method_and_the_body_again_when_it_can_be_sent_again() {
    let (o, a, b) = two();
    for status in [307, 308] {
        a.always(Reply::redirect(status, &b.url("/second")));
        let before = b.requests().len();
        let report = o.call(&json!({
            "url": a.url("/first"), "method": "POST",
            "body": { "kind": "string", "text": "send me twice" },
        }));
        assert_resolved(
            &report,
            200,
            &format!("a {status} after a POST with a string"),
        );
        let second = &b.wait_requests(before + 1, SOON)[before];
        assert_eq!(second.method, "POST", "{}", second.describe());
        assert_eq!(
            second.body_sha256,
            sha256(b"send me twice"),
            "{}",
            second.describe()
        );
        // A string whose JSON text is well within 64 KiB travelled with the
        // call and is sent again (the boundary itself, `65534` letters and
        // the two quotes, is not pinned: how "JSON text" counts is the
        // implementation's).
        let text = "b".repeat(60 * 1024);
        let before = b.requests().len();
        let report = o.call(&json!({
            "url": a.url("/first"), "method": "PUT",
            "body": { "kind": "string", "text": text },
        }));
        assert_resolved(&report, 200, &format!("a {status} after a PUT with 60 KiB"));
        let second = &b.wait_requests(before + 1, SOON)[before];
        assert_eq!(second.method, "PUT", "{}", second.describe());
        assert_eq!(
            second.body_sha256,
            sha256(text.as_bytes()),
            "{status}: expected the 60 KiB string sent again whole: {}",
            second.describe()
        );
    }
}

#[test]
fn a_307_and_a_308_of_a_call_whose_body_was_bytes_or_a_string_over_64_kib_fail_body_not_replayable()
{
    let (o, a, b) = two();
    let long = "c".repeat(100 * 1024);
    let bodies = [
        ("bytes of 1 KiB", json!({ "kind": "bytes", "length": 1024 })),
        (
            "an ArrayBuffer of 1 KiB",
            json!({ "kind": "arraybuffer", "length": 1024 }),
        ),
        (
            "a string of 100 KiB",
            json!({ "kind": "string", "text": long }),
        ),
    ];
    for status in [307, 308] {
        a.always(Reply::redirect(status, &b.url("/second")));
        for (what, body) in &bodies {
            let before = b.requests().len();
            let response = o.call_response(&json!({
                "url": a.url("/first"), "method": "POST", "body": body,
            }));
            assert_failure(
                &o.report(&response),
                "body-not-replayable",
                &format!("{status} after {what}"),
            );
            assert_eq!(
                b.requests().len(),
                before,
                "{status} after {what}: expected nothing sent on"
            );
            let entries = o.entries_of(&response);
            let last = entries.last().unwrap();
            assert_eq!(
                (last["outcome"].as_str(), last["error"].as_str()),
                (Some("failed"), Some("body-not-replayable")),
                "{status} after {what}: {entries:?}"
            );
        }
    }
}

#[test]
fn a_307_and_a_308_of_a_call_whose_body_was_a_stream_fail_body_not_replayable() {
    let (o, a, b) = two();
    for status in [307, 308] {
        a.always(Reply::redirect(status, &b.url("/second")));
        let before = b.requests().len();
        let response = o.call_response(&json!({
            "url": a.url("/first"), "method": "POST",
            "body": { "kind": "stream", "chunks": ["one", "two"] },
        }));
        assert_failure(
            &o.report(&response),
            "body-not-replayable",
            &format!("{status}"),
        );
        assert_eq!(b.requests().len(), before, "expected nothing sent on");
        let entries = o.entries_of(&response);
        let last = entries.last().unwrap();
        assert_eq!(
            (last["outcome"].as_str(), last["error"].as_str()),
            (Some("failed"), Some("body-not-replayable")),
            "{status}: {entries:?}"
        );
    }
    // A 302 after a POST drops the body, so a stream does not matter.
    a.always(Reply::redirect(302, &b.url("/second")));
    let report = o.call(&json!({
        "url": a.url("/first"), "method": "POST",
        "body": { "kind": "stream", "chunks": ["one", "two"] },
    }));
    assert_resolved(&report, 200, "a 302 after a POST with a stream");
}

#[test]
fn with_manual_the_redirect_is_the_answer_and_with_error_it_is_a_redirect_failure() {
    let (o, a, b) = two();
    a.always(Reply::redirect(302, &b.url("/never")).header("x-marker", "manual"));
    let response = o.call_response(&json!({ "url": a.url("/first"), "redirect": "manual" }));
    let report = o.report(&response);
    assert_resolved(&report, 302, "redirect: manual");
    let headers = report["resolved"]["headers"].as_array().unwrap();
    assert!(
        headers
            .iter()
            .any(|h| h[0] == "location" && h[1] == json!(b.url("/never"))),
        "expected the `location` among the headers, got {report}"
    );
    assert_eq!(report["resolved"]["redirected"], false, "{report}");
    assert_eq!(o.entries_of(&response).len(), 1);
    b.assert_untouched("redirect: manual");

    let response = o.call_response(&json!({ "url": a.url("/first"), "redirect": "error" }));
    assert_failure(&o.report(&response), "redirect", "redirect: error");
    let entries = o.entries_of(&response);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(
        (entries[0]["outcome"].as_str(), entries[0]["error"].as_str()),
        (Some("failed"), Some("redirect")),
        "{entries:?}"
    );
    b.assert_untouched("redirect: error");
}

#[test]
fn a_redirected_answer_given_back_as_the_response_is_the_last_one() {
    let (o, a, b) = two();
    a.always(Reply::redirect(302, &b.url("/final")));
    b.always(Reply::ok(b"given back").header("x-from", "b"));
    let response =
        o.vm.request(
            blackbox::outbound::APP,
            &format!("/pass?url={}", a.url("/start")),
        )
        .as_user(&blackbox::outbound::bob())
        .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_eq!(response.text(), "given back");
}
