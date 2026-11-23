//! What Application code does and what goes over the wire (CONTRACT.md,
//! "Outbound calls: What Application code does" and "What goes over the
//! wire"): the request the other side sees (its headers exactly, its
//! method, path and query, its body with its framing), the seven methods
//! and the eighth, an answer of any status, what the answer's headers
//! carry, TLS and HTTP/1.1, and no connection kept.
//!
//! The server is the harness's own (`blackbox::outbound::Origin`), with a
//! certificate under a test authority that `VIBE_CA_FILE` names.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::outbound::{
    APP, Origin, Outbound, Reply, assert_failure, assert_resolved, bob, host_call_records, sha256,
};
use serde_json::{Value, json};

/// A program with the fixture, an origin, and the origin listed.
fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

/// What the fixture's `bytes`, `arraybuffer`, `view` and `dataview` bodies
/// hold: byte `i` of the buffer is `(i + 1) % 251`, a view starts at 4.
fn pattern(length: usize, offset: usize) -> Vec<u8> {
    (0..length)
        .map(|i| u8::try_from((i + offset + 1) % 251).unwrap())
        .collect()
}

#[test]
fn the_request_has_the_method_path_query_host_and_user_agent_vibe_and_nothing_else() {
    let (o, origin) = listed();
    // The request the Application is itself asked with carries things of its
    // own that must not leave: the User, a cookie, a credential.
    let response =
        o.vm.request(APP, "/call")
            .as_user(&bob())
            .method("POST")
            .header("cookie", "session=cookie-marker-77")
            .header("authorization", "Bearer incoming-marker-88")
            .header("x-forwarded-for", "10.9.8.7")
            .json(&json!({ "url": origin.url("/a/b%20c/d?x=1&y=%7Ez&x=2") }))
            .send();
    assert_resolved(&o.report(&response), 200, "a plain GET");
    let seen = origin.the_request();
    assert_eq!(seen.method, "GET");
    assert_eq!(
        seen.target, "/a/b%20c/d?x=1&y=%7Ez&x=2",
        "expected the path and the query as given"
    );
    assert_eq!(seen.version, "HTTP/1.1", "{}", seen.describe());
    assert_eq!(
        seen.names(),
        ["host", "user-agent"],
        "expected the platform to add `user-agent` and send `host`, and nothing else, got {}",
        seen.describe()
    );
    assert_eq!(seen.header("user-agent"), Some("vibe"));
    assert_eq!(seen.header("host"), Some(origin.host().as_str()));
    let everything = format!("{:?}", seen.headers);
    for marker in ["marker", "bob", "10.9.8.7", "vm.test", "fixture", APP] {
        assert!(
            !everything.contains(marker),
            "expected nothing of the User, the Application, the request or the installation ({marker:?}) sent, got {everything}"
        );
    }
    assert!(
        seen.header("accept-encoding").is_none(),
        "{}",
        seen.describe()
    );
    assert_eq!(seen.body_len, 0);
}

#[test]
fn headers_the_code_sets_are_sent_as_set_and_a_user_agent_of_its_own_replaces_the_platforms() {
    let (o, origin) = listed();
    let report = o.call(&json!({
        "url": origin.url("/h"),
        "headers": { "x-one": "1", "Accept": "text/plain", "authorization": "Bearer mine",
                     "user-agent": "mine/1" },
    }));
    assert_resolved(&report, 200, "headers set");
    let seen = origin.the_request();
    assert_eq!(
        seen.names(),
        ["accept", "authorization", "host", "user-agent", "x-one"],
        "{}",
        seen.describe()
    );
    assert_eq!(seen.header("user-agent"), Some("mine/1"));
    assert_eq!(seen.header("x-one"), Some("1"));
    assert_eq!(seen.header("authorization"), Some("Bearer mine"));
    assert!(
        !seen.headers.iter().any(|(_, v)| v.contains("vibe")),
        "expected `user-agent: vibe` only when the code set none, got {}",
        seen.describe()
    );
}

#[test]
fn the_headers_that_are_the_platforms_to_set_are_dropped_when_the_code_gives_them() {
    let (o, origin) = listed();
    let report = o.call(&json!({
        "url": origin.url("/dropped"),
        "method": "POST",
        "body": { "kind": "string", "text": "hello" },
        "headers": {
            "host": "evil.example", "connection": "keep-alive", "content-length": "3",
            "transfer-encoding": "chunked", "upgrade": "websocket", "te": "trailers",
            "trailer": "x-t", "keep-alive": "timeout=5", "expect": "100-continue",
            "proxy-authorization": "Basic x", "proxy-connection": "keep-alive",
            "proxy-foo": "1", "x-kept": "yes",
        },
    }));
    assert_resolved(&report, 200, "headers that are the platform's");
    let seen = origin.the_request();
    assert_eq!(
        seen.names(),
        ["content-length", "host", "user-agent", "x-kept"],
        "expected only the platform's own `host`, `content-length` and `user-agent` and the code's `x-kept`, got {}",
        seen.describe()
    );
    assert_eq!(seen.header("host"), Some(origin.host().as_str()));
    assert_eq!(seen.header("content-length"), Some("5"));
    assert_eq!(seen.text(), "hello");
}

#[test]
fn each_of_the_seven_methods_is_sent_and_an_eighth_is_a_malformed_failure_with_no_entry() {
    let (o, origin) = listed();
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        origin.always(Reply::ok(b"answer"));
        let before = origin.requests().len();
        let report = o.call(&json!({ "url": origin.url("/m"), "method": method }));
        assert_resolved(&report, 200, method);
        let seen = origin.wait_requests(before + 1, Duration::from_secs(10));
        assert_eq!(seen[before].method, method);
        if method == "HEAD" {
            assert_eq!(
                report["resolved"]["length"], 0,
                "expected no body for a HEAD, got {report}"
            );
        }
    }
    let sent = origin.requests().len();
    for method in ["TRACE", "CONNECT", "PROPFIND", "FOO", "get ", "G\u{e9}T"] {
        let response = o.call_response(&json!({ "url": origin.url("/m"), "method": method }));
        let report = o.report(&response);
        assert_failure(&report, "malformed", method);
        assert!(
            o.entries_of(&response).is_empty(),
            "{method:?}: expected no Operation log entry for a malformed call"
        );
        let records = host_call_records(&o.vm, &response);
        assert!(
            records
                .iter()
                .any(|r| r["outcome"] == "refused" && r["reason"] == "malformed"),
            "{method:?}: expected a `host-call` record refused `malformed`, got {records:?}"
        );
    }
    assert_eq!(
        origin.requests().len(),
        sent,
        "expected nothing sent for a malformed call"
    );
}

#[test]
fn a_header_that_cannot_be_sent_an_address_over_8_kib_and_too_many_headers_are_malformed() {
    let (o, origin) = listed();
    let long = "a".repeat(8 * 1024 - 40);
    let report = o.call(&json!({ "url": origin.url(&format!("/{long}")) }));
    assert_resolved(&report, 200, "an address of at most 8 KiB");
    let sent = origin.requests().len();
    let too_long = "a".repeat(8 * 1024 + 1);
    let response = o.call_response(&json!({ "url": origin.url(&format!("/{too_long}")) }));
    assert_failure(
        &o.report(&response),
        "malformed",
        "an address of more than 8 KiB",
    );
    assert!(
        o.entries_of(&response).is_empty(),
        "no entry for a malformed call"
    );

    let mut many = serde_json::Map::new();
    for i in 0..101 {
        many.insert(format!("x-h-{i}"), json!("v"));
    }
    let response = o.call_response(&json!({ "url": origin.url("/h"), "headers": many }));
    assert_failure(&o.report(&response), "malformed", "101 headers");
    let big = "v".repeat(64 * 1024);
    let response =
        o.call_response(&json!({ "url": origin.url("/h"), "headers": { "x-big": big } }));
    assert_failure(
        &o.report(&response),
        "malformed",
        "headers of more than 64 KiB",
    );
    assert_eq!(
        origin.requests().len(),
        sent,
        "nothing is sent for a malformed call"
    );
    let ok_headers: serde_json::Map<String, Value> =
        (0..90).map(|i| (format!("x-ok-{i}"), json!("v"))).collect();
    let report = o.call(&json!({ "url": origin.url("/h"), "headers": ok_headers }));
    assert_resolved(&report, 200, "90 headers are taken");
}

#[test]
fn a_string_bytes_a_buffer_a_view_params_a_blob_and_nothing_are_sent_with_their_content_length() {
    let (o, origin) = listed();
    let text = "h\u{e9}llo \u{4e16}\u{754c}";
    let cases: Vec<(&str, Value, Vec<u8>)> = vec![
        (
            "string",
            json!({ "kind": "string", "text": text }),
            text.as_bytes().to_vec(),
        ),
        (
            "bytes",
            json!({ "kind": "bytes", "length": 100_000 }),
            pattern(100_000, 0),
        ),
        (
            "arraybuffer",
            json!({ "kind": "arraybuffer", "length": 777 }),
            pattern(777, 0),
        ),
        (
            "view",
            json!({ "kind": "view", "length": 1000 }),
            pattern(1000, 4),
        ),
        (
            "dataview",
            json!({ "kind": "dataview", "length": 1000 }),
            pattern(1000, 4),
        ),
        (
            "blob",
            json!({ "kind": "blob", "text": "blob body", "type": "text/x-blob" }),
            b"blob body".to_vec(),
        ),
    ];
    for (name, body, expected) in cases {
        let before = origin.requests().len();
        let report = o.call(&json!({ "url": origin.url("/body"), "method": "POST", "body": body }));
        assert_resolved(&report, 200, name);
        let seen = &origin.wait_requests(before + 1, SOON)[before];
        assert_eq!(
            seen.content_length,
            Some(u64::try_from(expected.len()).unwrap()),
            "{name}: expected the body sent with its content-length, got {}",
            seen.describe()
        );
        assert!(
            !seen.chunked,
            "{name}: expected no chunked framing, got {}",
            seen.describe()
        );
        assert_eq!(
            seen.body_sha256,
            sha256(&expected),
            "{name}: expected the body's exact bytes, got {}",
            seen.describe()
        );
    }
    let blob = &origin.requests()[5];
    assert_eq!(
        blob.header("content-type"),
        Some("text/x-blob"),
        "{}",
        blob.describe()
    );

    let before = origin.requests().len();
    let report = o.call(&json!({
        "url": origin.url("/params"), "method": "POST",
        "body": { "kind": "params", "pairs": [["a", "1"], ["b", "x y"]] },
    }));
    assert_resolved(&report, 200, "params");
    let seen = &origin.wait_requests(before + 1, SOON)[before];
    assert_eq!(seen.text(), "a=1&b=x+y", "{}", seen.describe());
    assert!(
        seen.content_length.is_some() && !seen.chunked,
        "{}",
        seen.describe()
    );

    let before = origin.requests().len();
    let report = o.call(&json!({
        "url": origin.url("/form"), "method": "POST",
        "body": { "kind": "form", "fields": { "name": "value-one" } },
    }));
    assert_resolved(&report, 200, "form data");
    let seen = &origin.wait_requests(before + 1, SOON)[before];
    assert!(
        seen.header("content-type")
            .is_some_and(|t| t.starts_with("multipart/form-data")),
        "expected a multipart content-type, got {}",
        seen.describe()
    );
    assert!(seen.text().contains("value-one"), "{}", seen.describe());
    assert!(
        seen.content_length.is_some() && !seen.chunked,
        "{}",
        seen.describe()
    );

    let before = origin.requests().len();
    let report = o.call(&json!({ "url": origin.url("/none"), "method": "GET" }));
    assert_resolved(&report, 200, "no body");
    let seen = &origin.wait_requests(before + 1, SOON)[before];
    assert!(
        seen.body_len == 0 && !seen.chunked && seen.header("transfer-encoding").is_none(),
        "{}",
        seen.describe()
    );
}

const SOON: Duration = Duration::from_secs(10);

#[test]
fn a_stream_of_the_codes_own_is_sent_chunked_as_it_is_produced() {
    let (o, origin) = listed();
    let report = o.call(&json!({
        "url": origin.url("/stream"), "method": "POST",
        "body": { "kind": "stream", "chunks": ["ab", "cd"], "count": 3, "size": 10_000 },
    }));
    assert_resolved(&report, 200, "a stream of the code's own");
    let seen = origin.the_request();
    assert!(
        seen.chunked && seen.content_length.is_none(),
        "expected a stream sent chunked, with no content-length, got {}",
        seen.describe()
    );
    // The fixture's stream chunks are `pattern(size, 0)`: byte i is `i % 251`.
    let mut expected = b"abcd".to_vec();
    for _ in 0..3 {
        expected.extend((0..10_000).map(|i| u8::try_from(i % 251).unwrap()));
    }
    assert_eq!(seen.body_sha256, sha256(&expected), "{}", seen.describe());
    assert!(seen.body_complete);
}

#[test]
fn a_request_given_as_the_input_is_read_for_its_method_headers_and_body() {
    let (o, origin) = listed();
    let report = o.call(&json!({
        "url": origin.url("/as-request?q=1"), "as": "request", "method": "PUT",
        "headers": { "x-from": "request" },
        "body": { "kind": "string", "text": "via a Request" },
    }));
    assert_resolved(&report, 200, "a Request as input");
    let seen = origin.the_request();
    assert_eq!(seen.method, "PUT");
    assert_eq!(seen.target, "/as-request?q=1");
    assert_eq!(seen.header("x-from"), Some("request"));
    assert_eq!(seen.text(), "via a Request");
}

#[test]
fn an_answer_of_any_status_resolves_with_its_headers_as_sent_but_for_the_framing_ones() {
    let (o, origin) = listed();
    for (status, text) in [
        (200, "ok"),
        (201, "made"),
        (404, "no such"),
        (500, "broke"),
        (418, "tea"),
    ] {
        origin.always(
            Reply::new(status)
                .header("x-one", "a")
                .header("x-two", "b")
                .header("set-cookie", "a=1")
                .header("set-cookie", "b=2")
                .header("content-type", "text/x-test")
                .body(text.as_bytes()),
        );
        let report = o.call(&json!({ "url": origin.url("/status") }));
        assert_resolved(&report, u64::from(status), "an answer of any status");
        let answer = &report["resolved"];
        assert_eq!(answer["text"], text, "{report}");
        assert_eq!(
            answer["bodyIsStream"], true,
            "expected the body a stream, got {report}"
        );
        assert_eq!(answer["redirected"], false, "{report}");
        assert_eq!(
            answer["url"],
            json!(origin.url("/status")),
            "expected `url` the address called, got {report}"
        );
        let headers: Vec<(String, String)> = answer["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_owned(),
                    h[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let value = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(value("x-one"), Some("a"), "{headers:?}");
        assert_eq!(value("x-two"), Some("b"), "{headers:?}");
        assert_eq!(value("content-type"), Some("text/x-test"), "{headers:?}");
        assert_eq!(
            value("content-length"),
            Some(text.len().to_string().as_str()),
            "expected `content-length` given as sent, got {headers:?}"
        );
        assert!(
            headers
                .iter()
                .any(|(n, v)| n == "set-cookie" && v.contains("a=1")),
            "expected `set-cookie` among the headers, got {headers:?}"
        );
    }
}

#[test]
fn the_framing_headers_of_an_answer_are_not_given_to_the_code() {
    let (o, origin) = listed();
    origin.always(
        Reply::new(200)
            .header("connection", "keep-alive")
            .header("keep-alive", "timeout=5")
            .header("x-kept", "yes")
            .chunked(&[b"one ", b"two"]),
    );
    let report = o.call(&json!({ "url": origin.url("/framing") }));
    assert_resolved(&report, 200, "a chunked answer");
    let names: Vec<&str> = report["resolved"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h[0].as_str().unwrap())
        .collect();
    for gone in ["connection", "transfer-encoding", "keep-alive"] {
        assert!(
            !names.contains(&gone),
            "expected `{gone}` not given to the code, got {names:?}"
        );
    }
    assert!(names.contains(&"x-kept"), "{names:?}");
    assert_eq!(report["resolved"]["text"], "one two", "{report}");
}

#[test]
fn an_answer_that_is_a_1xx_as_its_final_answer_is_an_unreachable_failure_protocol() {
    let (o, origin) = listed();
    origin.always(Reply::raw(
        b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\n\r\n",
        Duration::from_secs(20),
    ));
    let report = o.call(&json!({ "url": origin.url("/ws") }));
    assert_failure(&report, "unreachable", "a 101 as the final answer");
    assert_eq!(
        report["rejected"]["failure"]["why"], "protocol",
        "expected `why` `protocol`, got {report}"
    );
}

#[test]
fn the_connection_is_tls_1_2_or_1_3_http_1_1_with_the_servers_name_and_none_is_kept() {
    let (o, origin) = listed();
    for path in ["/first", "/second"] {
        let report = o.call(&json!({ "url": origin.url(path) }));
        assert_resolved(&report, 200, path);
    }
    let seen = origin.wait_requests(2, SOON);
    assert_eq!(seen[0].version, "HTTP/1.1");
    assert_ne!(
        seen[0].connection, seen[1].connection,
        "expected a connection of its own for each call"
    );
    let connections = origin.connections();
    assert_eq!(connections.len(), 2, "{connections:?}");
    for c in &connections {
        assert_eq!(
            c.requests, 1,
            "expected one request on a connection, got {c:?}"
        );
        assert_eq!(c.sni.as_deref(), Some("localhost"), "{c:?}");
        assert!(
            matches!(c.tls_version.as_deref(), Some("TLSv1_2" | "TLSv1_3")),
            "expected TLS 1.2 or 1.3, got {c:?}"
        );
    }
    origin
        .wait_peer_closed(std::time::Instant::now(), Duration::from_secs(10))
        .expect("expected the platform to close the connection after the call");
}
