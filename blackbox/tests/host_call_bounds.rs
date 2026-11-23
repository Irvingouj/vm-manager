//! The sizes and the answers of "A host call's arguments are bounded"
//! (CONTRACT.md): one call at most 8 MiB (JSON text as UTF-8, or 32 bytes a
//! value when that is more), the count of values, and what the answer
//! carries: `{ error: { kind: "limit", limit: "arguments" } }`, no Operation
//! log entry, the `host-call` record `refused` with reason `limit`, the
//! request going on. The fixture (`fixtures/host-call-bounds.js`) builds
//! arguments of an exact size and measures them itself.
//!
//! Pins that hold before the ticket too (exactly 8 MiB is taken, the order
//! of an unknown operation within the bound, a put's string body): the
//! tests `exactly_8_mib...` first halves, `a_put_of_a_string_...` first
//! half. The rest is red until the bounds are there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::host_call::{
    CALL_BOUND, Calls, assert_limit, assert_measured, assert_taken, call, padded, plain, quick,
    short, sized, unknown,
};
use blackbox::scenario::lines;
use serde_json::json;

/// The contract's own number of values a call may have: 8 MiB at 32 bytes.
const VALUES: usize = CALL_BOUND / 32;

#[test]
fn arguments_of_exactly_8_mib_are_taken_and_one_byte_more_is_limit_arguments() {
    let h = Calls::start();
    // The same pin for an operation the host knows (`query`, `whoami`) and
    // for one it does not, and for text of one byte a character and of
    // characters of 2, 3 and 4 bytes (the size is bytes, not characters).
    for multibyte in [false, true] {
        for op in ["query", "whoami", "nonesuch"] {
            let (base, path) = if op == "query" {
                (
                    json!({ "own": true, "as": "application", "sql": "select length($1::text) as n" }),
                    "params",
                )
            } else {
                (json!({}), "pad")
            };
            let context = format!("{op}, multibyte {multibyte}");
            let (response, results) = h.seq(&[
                padded(op, &base, path, CALL_BOUND, multibyte),
                padded(op, &base, path, CALL_BOUND + 1, multibyte),
            ]);
            assert_eq!(results.len(), 2, "{context}");
            assert_measured(&results[0], CALL_BOUND, &context);
            assert_measured(&results[1], CALL_BOUND + 1, &context);
            assert_taken(
                &results[0],
                op,
                &format!("{context}: arguments of exactly 8,388,608 bytes"),
            );
            assert_limit(
                &results[1],
                "arguments",
                &format!("{context}: arguments of 8,388,609 bytes"),
            );
            let refused = h.refused_records(&response);
            assert_eq!(
                refused.iter().filter(|r| r["reason"] == "limit").count(),
                1,
                "{context}: expected one `host-call` record `refused` with reason `limit`, got {refused:?}"
            );
        }
    }
}

#[test]
fn an_unknown_operation_is_unknown_within_the_bound_and_limit_past_it() {
    // Decided before anything else about the call, whatever the operation's
    // name: the size comes first.
    let h = Calls::start();
    let (_, results) = h.seq(&[
        sized("nonesuch", 1024),
        sized("nonesuch", CALL_BOUND + 1),
        sized("", CALL_BOUND + 1),
    ]);
    assert_taken(
        &results[0],
        "nonesuch",
        "a small call of an unknown operation",
    );
    assert_limit(&results[1], "arguments", "an unknown operation, 8 MiB + 1");
    assert_limit(
        &results[2],
        "arguments",
        "an operation with no name, 8 MiB + 1",
    );
}

#[test]
fn arguments_not_given_are_of_size_0_and_a_value_is_32_bytes_at_least() {
    let h = Calls::start();
    let (_, results) = h.seq(&[
        call("whoami", json!({ "undefined": true })),
        call("nonesuch", json!({ "undefined": true })),
        call("nonesuch", plain(&json!(1))),
    ]);
    assert_taken(&results[0], "whoami", "whoami with no arguments");
    assert_eq!(results[0]["bytes"], json!(0), "{}", short(&results[0]));
    assert_taken(
        &results[1],
        "nonesuch",
        "an unknown operation with no arguments",
    );
    assert_taken(&results[2], "nonesuch", "the number 1 as arguments");
}

#[test]
fn arguments_of_262144_values_are_taken_and_262145_are_limit_arguments_though_their_text_is_small()
{
    let h = Calls::start();
    // `{ z: [0, ...] }`: the object, its key, the array and the zeros.
    let (response, results) = h.seq(&[
        unknown(json!({ "zeros": VALUES })),
        unknown(json!({ "zeros": VALUES + 1 })),
        // The values of a call that waits count the same: a known operation.
        call("query", json!({ "zeros": VALUES + 1 })),
    ]);
    assert_eq!(
        results[0]["values"],
        json!(VALUES),
        "{}",
        short(&results[0])
    );
    assert_eq!(results[1]["values"], json!(VALUES + 1));
    for result in &results[..2] {
        let text = result["bytes"].as_u64().unwrap_or(u64::MAX);
        assert!(
            text < 2 * 1024 * 1024,
            "the fixture's own check: the text of the arguments is far below 8 MiB, it is {text}"
        );
    }
    assert_taken(
        &results[0],
        "nonesuch",
        "arguments of exactly 262,144 values (8 MiB at 32 bytes each)",
    );
    assert_limit(
        &results[1],
        "arguments",
        "arguments of 262,145 values (8 MiB and 32 bytes by their values)",
    );
    assert_limit(
        &results[2],
        "arguments",
        "a `query` with 262,145 values: the size comes before the malformed check",
    );
    assert_eq!(
        h.refused_records(&response)
            .iter()
            .filter(|r| r["reason"] == "limit")
            .count(),
        2
    );
}

#[test]
fn the_keys_of_an_object_count_as_values() {
    let h = Calls::start();
    // An object of n keys holding 0 each is 1 + 2n values.
    let (_, results) = h.seq(&[
        unknown(json!({ "keys": VALUES - 1 })),
        unknown(json!({ "keys": VALUES + 1 })),
    ]);
    assert_eq!(results[0]["values"], json!(VALUES - 1));
    assert_eq!(results[1]["values"], json!(VALUES + 1));
    assert_taken(
        &results[0],
        "nonesuch",
        "an object of 131,071 keys (262,143 values)",
    );
    assert_limit(
        &results[1],
        "arguments",
        "an object of 131,072 keys (262,145 values): keys count as values",
    );
}

#[test]
fn two_million_zeros_are_refused_as_too_large() {
    let h = Calls::start();
    let (_, results) = h.seq(&[
        unknown(json!({ "array0": 2_000_000 })),
        call("query", json!({ "array0": 2_000_000 })),
        call("whoami", json!({ "array0": 2_000_000 })),
    ]);
    let text = results[0]["bytes"].as_u64().unwrap_or(0);
    assert!(
        (3_000_000..8_000_000).contains(&text),
        "the fixture's own check: the text of two million zeros is about 4 MiB, it is {text}"
    );
    for (n, result) in results.iter().enumerate() {
        assert_limit(
            result,
            "arguments",
            &format!("two million `0`s (4 MiB of text, 64 MiB by their values), call {n}"),
        );
    }
}

#[test]
fn a_call_past_the_bound_has_no_operation_log_entry_a_refused_record_and_the_request_goes_on() {
    let h = Calls::start();
    let before = h.log_len();
    let (response, results) = h.seq(&[
        sized("nonesuch", CALL_BOUND + 1),
        quick(1024),
        // A known operation that would have an entry, were it decided.
        padded(
            "query",
            &json!({ "own": true, "as": "user", "sql": "select 7 as marker" }),
            "params",
            CALL_BOUND + 1,
            false,
        ),
        // The next call within the bounds is served, as the request goes on.
        call("whoami", json!({ "undefined": true })),
        sized("nonesuch", 200),
    ]);
    assert_limit(
        &results[0],
        "arguments",
        "the unknown operation past the bound",
    );
    assert_taken(&results[1], "query", "a quick query after the refused call");
    assert_limit(&results[2], "arguments", "the query past the bound");
    assert_taken(&results[3], "whoami", "whoami after the refused calls");
    assert_taken(
        &results[4],
        "nonesuch",
        "a small call after the refused calls",
    );
    // The request's answer is what the code returned: `200`, not a 500.
    assert_eq!(response.status, 200, "{}", response.describe());
    let entries = h.own.entries_of(&response);
    assert_eq!(
        entries.len(),
        1,
        "expected one Operation log entry for the request (the quick query), none for the refused calls, got\n{}",
        lines(&entries)
    );
    assert!(
        !entries[0]["statement"]
            .as_str()
            .unwrap_or("")
            .contains("marker"),
        "expected the one entry to be the quick query's, not the refused one: {}",
        entries[0]
    );
    assert_eq!(
        h.log_len(),
        before + 1,
        "expected the Operation log to grow by exactly one entry"
    );
    let refused = h.refused_records(&response);
    let reasons: Vec<&str> = refused
        .iter()
        .filter_map(|r| r["reason"].as_str())
        .collect();
    assert_eq!(
        reasons.iter().filter(|r| **r == "limit").count(),
        2,
        "expected two `host-call` records `refused` with reason `limit` (one a call of `nonesuch`, one of `query`), got\n{refused:?}"
    );
    // Refused for the size, not for the name: of the two calls of
    // `nonesuch`, the one past the bound is recorded with `limit` and only
    // the small one after it with `unknown-operation`.
    let nonesuch: Vec<&str> = refused
        .iter()
        .filter(|r| r["operation"] == "nonesuch")
        .filter_map(|r| r["reason"].as_str())
        .collect();
    assert_eq!(
        nonesuch,
        ["limit", "unknown-operation"],
        "expected the call of `nonesuch` past the bound recorded with `limit` and the small one after it with `unknown-operation`: {refused:?}"
    );
}

#[test]
fn a_put_of_a_string_body_is_not_arguments_but_its_other_arguments_are() {
    // The put's body is a body, counted as a body ("Storage", "Bounds"): a
    // string of 20 MiB with characters of more than one byte is written as
    // its UTF-8. See `host_call_storage.rs`; here, with no Storage
    // Resource, only that the size of the put's other arguments is decided
    // first. A `storage.put` with a `body` is decided as ever: refused
    // `not-declared` (the Application declares no Storage Resource), not
    // `limit`, with 20 MiB of body.
    let h = Calls::start();
    let (_, results) = h.seq(&[
        padded(
            "storage.put",
            &json!({ "resource": "nothing", "key": "a.txt" }),
            "body",
            20 * 1024 * 1024,
            true,
        ),
        padded(
            "storage.put",
            &json!({ "resource": "nothing", "body": "x" }),
            "key",
            CALL_BOUND + 1,
            false,
        ),
    ]);
    assert_eq!(
        results[0]["answer"]["refused"]["refusal"]["reason"].as_str(),
        Some("not-declared"),
        "expected a put with a string body of 20 MiB to be decided as a put, not refused for its size, got {}",
        short(&results[0])
    );
    assert_limit(
        &results[1],
        "arguments",
        "a put whose key (not its body) makes the arguments larger than 8 MiB",
    );
}

/// What `/getter` makes: the outer call's arguments have a getter, which
/// `JSON.stringify` runs while it reads them, and which makes another call
/// with arguments of its own. Each call is decided by its own size.
fn getter_case(outer_big: bool, inner_big: bool) {
    let h = Calls::start();
    let context = format!("outer {outer_big}, inner {inner_big}");
    let (response, body) = h.post(
        "/getter",
        &json!({
            "outer": if outer_big { "big" } else { "small" },
            "inner": if inner_big { "big" } else { "small" },
        }),
    );
    let inners = h.results(&body, "inners");
    assert!(
        !inners.is_empty(),
        "{context}: the getter was to make its call at least once, got {}",
        short(&body)
    );
    let check = |result: &serde_json::Value, big: bool, what: &str| {
        if big {
            assert_limit(
                &json!({ "answer": result["answer"] }),
                "arguments",
                &format!("{context}: the {what} call (past the bound)"),
            );
        } else {
            assert_eq!(
                result["answer"]["refused"]["refusal"]["reason"].as_str(),
                Some("unknown-operation"),
                "{context}: expected the {what} call (small) served as what it is, got {}",
                short(result)
            );
        }
    };
    check(&body["outer"], outer_big, "outer");
    for inner in &inners {
        check(inner, inner_big, "inner");
    }
    let reasons: Vec<String> = h
        .refused_records(&response)
        .iter()
        .filter(|r| r["operation"] == "nonesuch")
        .filter_map(|r| r["reason"].as_str().map(str::to_owned))
        .collect();
    let count = |reason: &str| reasons.iter().filter(|r| *r == reason).count();
    let limits = usize::from(outer_big) + if inner_big { inners.len() } else { 0 };
    let unknowns = usize::from(!outer_big) + if inner_big { 0 } else { inners.len() };
    assert_eq!(
        (count("limit"), count("unknown-operation")),
        (limits, unknowns),
        "{context}: expected the `host-call` records to say `limit` {limits} times and `unknown-operation` {unknowns} times (one record for each call, by its own size), got {reasons:?}"
    );
}

#[test]
fn a_call_made_from_a_getter_while_another_calls_arguments_are_read_is_decided_by_its_own_size() {
    getter_case(false, true);
    getter_case(true, false);
    getter_case(false, false);
}

#[test]
fn arguments_with_lone_surrogates_are_counted_as_json_stringify_writes_them() {
    // A lone surrogate is `\ud800` in JSON text: six bytes.
    let h = Calls::start();
    let lone = |bytes: usize| {
        call(
            "nonesuch",
            json!({ "pad": { "bytes": bytes, "base": {}, "path": "pad", "multibyte": "lone" } }),
        )
    };
    let (_, results) = h.seq(&[lone(CALL_BOUND), lone(CALL_BOUND + 1)]);
    assert_measured(&results[0], CALL_BOUND, "lone surrogates");
    assert_measured(&results[1], CALL_BOUND + 1, "lone surrogates");
    assert_taken(
        &results[0],
        "nonesuch",
        "arguments of exactly 8,388,608 bytes of JSON text holding lone surrogates",
    );
    assert_limit(
        &results[1],
        "arguments",
        "arguments of 8,388,609 bytes of JSON text holding lone surrogates",
    );
}
