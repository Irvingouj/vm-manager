//! What `history(...)` refuses itself, before anything reaches the host
//! (CONTRACT.md, "Reading the Change history: The call"): a key with a
//! property missing, a property that is not of the primary key, or given
//! for a table without one throws a `TypeError` that names the primary
//! key's properties; a `limit` that is not an integer from 1 to 1000 throws
//! a `TypeError`. Nothing is sent to the host: no host call, no Operation
//! log entry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::sdk::{Sdk, ann};
use blackbox::sdk_history::{
    HistoryCall, assert_nothing_sent, assert_type_error, caught_of, chronicle,
};

/// Fails unless these calls, the right ones beside the wrong ones, answer
/// changes: so that a wrong call's `TypeError` is the SDK's check, not a
/// `history` that is missing.
fn assert_the_right_calls_answer(sdk: &Sdk, calls: &[HistoryCall<'_>]) {
    for call in calls {
        call.changes(sdk, &ann(), "a right call, beside the wrong ones");
    }
}
use serde_json::{Value, json};

const ACTORS: [&str; 2] = ["user", "application"];

fn assert_refused_by_the_sdk(sdk: &Sdk, call: &HistoryCall<'_>, names: &[&str], context: &str) {
    let context = format!("{context} ({call:?})");
    let (response, body) = call.send(sdk, &ann());
    let caught = caught_of(&body, &context);
    assert_type_error(caught, names, &context);
    assert_nothing_sent(sdk, &response, &context);
}

#[test]
fn a_key_that_is_not_the_primary_key_throws_a_type_error_and_sends_nothing() {
    let sdk = chronicle();
    assert_the_right_calls_answer(
        &sdk,
        &[
            HistoryCall::of("tasks").key(json!({ "id": 1 })),
            HistoryCall::of("lines").key(json!({ "orderId": 3, "line": 2 })),
            HistoryCall::of("events"),
        ],
    );
    let cases: [(&str, Value, &[&str], &str); 7] = [
        (
            "tasks",
            json!({}),
            &["id"],
            "a key with the property missing",
        ),
        (
            "tasks",
            json!({ "nope": 1 }),
            &["id"],
            "a property that is not of the primary key",
        ),
        (
            "tasks",
            json!({ "id": 1, "title": "x" }),
            &["id"],
            "a property beside the primary key's",
        ),
        (
            "lines",
            json!({ "orderId": 3 }),
            &["orderId", "line"],
            "one of a composite key's properties missing",
        ),
        (
            "lines",
            json!({ "order_id": 3, "line": 2 }),
            &["orderId", "line"],
            "a column's database name instead of its property name",
        ),
        (
            "lines",
            json!({ "orderId": 3, "line": 2, "item": "bolts" }),
            &["orderId", "line"],
            "a property beside a composite key's",
        ),
        (
            "events",
            json!({ "what": "alpha" }),
            &[],
            "a key for a table without a primary key",
        ),
    ];
    for actor in ACTORS {
        for (table, key, names, context) in &cases {
            assert_refused_by_the_sdk(
                &sdk,
                &HistoryCall::of(table).key(key.clone()).as_actor(actor),
                names,
                &format!("{context}, as the {actor}"),
            );
        }
    }
}

#[test]
fn a_limit_that_is_not_an_integer_from_1_to_1000_throws_a_type_error_and_sends_nothing() {
    let sdk = chronicle();
    assert_the_right_calls_answer(
        &sdk,
        &[
            HistoryCall::of("tasks")
                .key(json!({ "id": 1 }))
                .options(json!({ "limit": 1 })),
            HistoryCall::of("events").options(json!({ "limit": 1000 })),
        ],
    );
    let limits = [
        json!(0),
        json!(-1),
        json!(1001),
        json!(1.5),
        json!("5"),
        json!(true),
        json!(null),
    ];
    for actor in ACTORS {
        for limit in &limits {
            assert_refused_by_the_sdk(
                &sdk,
                &HistoryCall::of("tasks")
                    .key(json!({ "id": 1 }))
                    .options(json!({ "limit": limit }))
                    .as_actor(actor),
                &[],
                &format!("limit {limit}, as the {actor}"),
            );
            assert_refused_by_the_sdk(
                &sdk,
                &HistoryCall::of("events")
                    .options(json!({ "limit": limit }))
                    .as_actor(actor),
                &[],
                &format!("limit {limit} without a key, as the {actor}"),
            );
        }
    }
}
