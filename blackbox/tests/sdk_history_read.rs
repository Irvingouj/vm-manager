//! Reading a row's Change history through the SDK (CONTRACT.md, "Reading
//! the Change history": "The call", "A change", "An operation like any
//! other"; ticket "Read a row's Change history through the SDK", criteria 1,
//! 2 and 4), with the fixture project `fixtures/sdk/chronicle`:
//!
//! - after two Users changed a row (an insert, updates as the User and as
//!   the Application, a delete), `history(table, key)` answers each change
//!   newest first, naming the right User, Application, actor and request,
//!   at a time within the test, with `old` and `new` equal to what
//!   `select()` gave at that time (a BigInt beyond 2^53, a numeric with
//!   every digit, a timestamp, jsonb, an array), and `changed` by property
//!   names (`dueAt`, not `due_at`);
//! - read through `ctx.user().ownDb()` and `ctx.app().ownDb()` alike, each
//!   call one `query` on the own database as the handle's actor, with one
//!   Operation log entry whose statement reads `vm.history` and holds no
//!   value;
//! - a composite key, a table in another schema, the history of a whole
//!   table, the table without a primary key, a key that was updated, and
//!   paging with `limit` and `before`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::sdk::{Sdk, ann, d};
use blackbox::sdk_history::{
    HistoryCall, Want, assert_at_within, assert_changes, assert_one_history_entry, bob, carol,
    chronicle, epoch_ms, id_text, row_of, task_key,
};
use serde_json::{Value, json};

const ACTORS: [&str; 2] = ["user", "application"];

/// A key no statement would hold by chance.
const KEY: i64 = 7_340_291;

/// The `n`-th row of an `/events/*` answer.
fn event_row(body: &Value, n: usize) -> Value {
    body["rows"]["items"][n].clone()
}

#[test]
fn two_users_change_a_row_and_its_history_names_each_of_them_newest_first() {
    let sdk = chronicle();
    let (ann, bob, carol) = (ann(), bob(), carol());
    let from = epoch_ms(&sdk.own);
    let (put, body) = sdk.get_ok(&ann, &format!("/tasks/put?as=user&id={KEY}&v=a"));
    let a = row_of(&body);
    let (set_b, body) = sdk.get_ok(&bob, &format!("/tasks/set?as=user&id={KEY}&v=b"));
    let b = row_of(&body);
    let (set_c, body) = sdk.get_ok(&ann, &format!("/tasks/set?as=application&id={KEY}&v=c"));
    let c = row_of(&body);
    let (dropped, body) = sdk.get_ok(&bob, &format!("/tasks/drop?as=application&id={KEY}"));
    assert_eq!(
        body["row"],
        d::null(),
        "the harness's own check: the row is gone"
    );
    // Another row, whose changes are not this row's.
    let (other, body) = sdk.get_ok(&ann, &format!("/tasks/put?as=user&id={}&v=n", KEY + 1));
    let n = row_of(&body);
    let to = epoch_ms(&sdk.own);

    // What select() gave, as the fixture describes it: the harness's own
    // checks that the values are the ones the test means.
    let a_entries = &a["entries"];
    assert_eq!(a_entries["big"], d::bigint("9007199254740993"), "{a}");
    assert_eq!(
        a_entries["amount"],
        d::string("12345678901234567890.012345678901234567890"),
        "{a}"
    );
    assert_eq!(
        a_entries["dueAt"],
        d::date("2026-01-02T03:04:05.678Z"),
        "{a}"
    );
    assert_eq!(b["entries"]["meta"], d::string("123"), "{b}");
    assert_eq!(n["entries"]["big"], d::null(), "{n}");

    let wants = [
        Want::delete(&bob.id, "application", &dropped.request_id(), &c),
        Want::update(
            &ann.id,
            "application",
            &set_c.request_id(),
            &b,
            &c,
            &["dueAt", "meta"],
        ),
        Want::update(
            &bob.id,
            "user",
            &set_b.request_id(),
            &a,
            &b,
            &["title", "dueAt", "amount", "meta", "tags", "big"],
        ),
        Want::insert(&ann.id, "user", &put.request_id(), &a),
    ];
    for actor in ACTORS {
        let context = format!("history(tasks, {{ id: {KEY} }}) read by carol as the {actor}");
        let (response, changes) = HistoryCall::of("tasks")
            .key(task_key(KEY))
            .as_actor(actor)
            .changes(&sdk, &carol, &context);
        assert_changes(&changes, &wants, &context);
        assert_at_within(&changes, from, to, &context);
        assert_one_history_entry(
            &sdk,
            &response,
            &carol,
            actor,
            wants.len(),
            &[&KEY.to_string()],
            &context,
        );
    }
    let (_, changes) =
        HistoryCall::of("tasks")
            .key(task_key(KEY + 1))
            .changes(&sdk, &carol, "the other row");
    assert_changes(
        &changes,
        &[Want::insert(&ann.id, "user", &other.request_id(), &n)],
        "the other row, its nulls read back as nulls",
    );
}

#[test]
fn a_composite_key_names_one_row_and_without_a_key_every_row_of_the_table() {
    let sdk = chronicle();
    let (ann, bob) = (ann(), bob());
    let line = |user: &blackbox::User, route: &str, order: i64, line: i64, item: &str| {
        let (response, body) = sdk.get_ok(
            user,
            &format!("/lines/{route}?as=user&order={order}&line={line}&item={item}"),
        );
        (response.request_id(), row_of(&body))
    };
    let (put_2, l2) = line(&ann, "put", 3, 2, "bolts");
    let (set_2, l2b) = line(&bob, "set", 3, 2, "nuts");
    let (put_1, l1) = line(&ann, "put", 3, 1, "washers");
    let (drop_1, gone) = line(&bob, "drop", 3, 1, "");
    assert_eq!(gone, d::null(), "the harness's own check: the line is gone");
    assert_eq!(
        l2["entries"]["orderId"],
        d::number("3"),
        "the harness's own check: {l2}"
    );

    let context = "history(orderLines, { orderId: 3, line: 2 })";
    let (response, changes) = HistoryCall::of("lines")
        .key(json!({ "orderId": 3, "line": 2 }))
        .changes(&sdk, &ann, context);
    assert_changes(
        &changes,
        &[
            Want::update(&bob.id, "user", &set_2, &l2, &l2b, &["item"]),
            Want::insert(&ann.id, "user", &put_2, &l2),
        ],
        context,
    );
    assert_one_history_entry(&sdk, &response, &ann, "user", 2, &[], context);

    let every = [
        Want::delete(&bob.id, "user", &drop_1, &l1),
        Want::insert(&ann.id, "user", &put_1, &l1),
        Want::update(&bob.id, "user", &set_2, &l2, &l2b, &["item"]),
        Want::insert(&ann.id, "user", &put_2, &l2),
    ];
    let context = "history(orderLines), every row of the table";
    let (response, changes) = HistoryCall::of("lines").changes(&sdk, &ann, context);
    assert_changes(&changes, &every, context);
    assert_one_history_entry(&sdk, &response, &ann, "user", 4, &[], context);
    let context = "history(orderLines, undefined)";
    let (_, changes) = HistoryCall::of("lines")
        .undefined_key()
        .changes(&sdk, &ann, context);
    assert_changes(&changes, &every, context);
}

#[test]
fn the_table_without_a_primary_key_is_read_whole() {
    let sdk = chronicle();
    let (ann, bob) = (ann(), bob());
    let (alpha, body) = sdk.get_ok(&ann, "/events/put?as=user&what=alpha&n=1");
    let alpha_1 = event_row(&body, 0);
    let (beta, body) = sdk.get_ok(&bob, "/events/put?as=user&what=beta&n=2");
    let beta_2 = event_row(&body, 0);
    let (set, body) = sdk.get_ok(&bob, "/events/set?as=application&what=alpha&n=3");
    let alpha_3 = event_row(&body, 0);
    assert_eq!(
        alpha_3,
        d::object(&[("what", d::string("alpha")), ("n", d::number("3"))]),
        "the harness's own check"
    );
    let wants = [
        Want::update(
            &bob.id,
            "application",
            &set.request_id(),
            &alpha_1,
            &alpha_3,
            &["n"],
        ),
        Want::insert(&bob.id, "user", &beta.request_id(), &beta_2),
        Want::insert(&ann.id, "user", &alpha.request_id(), &alpha_1),
    ];
    for actor in ACTORS {
        let context = format!("history(events) as the {actor}");
        let (response, changes) = HistoryCall::of("events")
            .as_actor(actor)
            .changes(&sdk, &ann, &context);
        assert_changes(&changes, &wants, &context);
        assert_one_history_entry(&sdk, &response, &ann, actor, 3, &[], &context);
    }
}

#[test]
fn a_table_in_another_schema_has_its_history_too() {
    let sdk = chronicle();
    let (ann, bob) = (ann(), bob());
    let (put, body) = sdk.get_ok(&ann, "/archive/put?as=user&id=5&body=kept");
    let kept = row_of(&body);
    let (set, body) = sdk.get_ok(&bob, "/archive/set?as=user&id=5&body=rewritten");
    let rewritten = row_of(&body);
    let context = "history(archivedNotes, { id: 5 }), a table in schema archive";
    let (_, changes) = HistoryCall::of("archive")
        .key(json!({ "id": 5 }))
        .changes(&sdk, &ann, context);
    assert_changes(
        &changes,
        &[
            Want::update(
                &bob.id,
                "user",
                &set.request_id(),
                &kept,
                &rewritten,
                &["body"],
            ),
            Want::insert(&ann.id, "user", &put.request_id(), &kept),
        ],
        context,
    );
}

#[test]
fn a_row_whose_key_was_updated_has_its_earlier_changes_under_the_old_key() {
    let sdk = chronicle();
    let (ann, bob) = (ann(), bob());
    let (put, body) = sdk.get_ok(&ann, "/tasks/put?as=user&id=11&v=a");
    let a = row_of(&body);
    let (set_b, body) = sdk.get_ok(&bob, "/tasks/set?as=user&id=11&v=b");
    let b = row_of(&body);
    let (rekey, body) = sdk.get_ok(&ann, "/tasks/rekey?as=application&id=11&to=12");
    let b12 = row_of(&body);
    assert_eq!(
        b12["entries"]["id"],
        d::number("12"),
        "the harness's own check: {b12}"
    );
    let (set_c, body) = sdk.get_ok(&bob, "/tasks/set?as=user&id=12&v=c");
    let c12 = row_of(&body);

    let context = "history(tasks, { id: 11 }), the old key";
    let (_, changes) = HistoryCall::of("tasks")
        .key(task_key(11))
        .changes(&sdk, &ann, context);
    assert_changes(
        &changes,
        &[
            Want::update(
                &ann.id,
                "application",
                &rekey.request_id(),
                &b,
                &b12,
                &["id"],
            ),
            Want::update(
                &bob.id,
                "user",
                &set_b.request_id(),
                &a,
                &b,
                &["title", "dueAt", "amount", "meta", "tags", "big"],
            ),
            Want::insert(&ann.id, "user", &put.request_id(), &a),
        ],
        context,
    );
    let context = "history(tasks, { id: 12 }), the new key";
    let (_, changes) = HistoryCall::of("tasks")
        .key(task_key(12))
        .changes(&sdk, &ann, context);
    assert_changes(
        &changes,
        &[Want::update(
            &bob.id,
            "user",
            &set_c.request_id(),
            &b12,
            &c12,
            &["dueAt", "meta"],
        )],
        context,
    );
}

/// The ids of these changes, as answered.
fn ids(changes: &[Value]) -> Vec<String> {
    changes.iter().map(id_text).collect()
}

/// `history(tasks, { id }, options)` as ann; the response and the changes.
fn page(sdk: &Sdk, id: i64, options: Value, context: &str) -> (blackbox::Response, Vec<Value>) {
    HistoryCall::of("tasks")
        .key(task_key(id))
        .options(options)
        .changes(sdk, &ann(), context)
}

#[test]
fn limit_and_before_page_back_through_a_long_history() {
    let sdk = chronicle();
    let user = ann();
    sdk.get_ok(&user, "/tasks/put?as=user&id=21&v=a");
    sdk.get_ok(&user, "/tasks/bump?as=user&id=21&n=5");
    let (_, full) = HistoryCall::of("tasks").key(task_key(21)).changes(
        &sdk,
        &user,
        "the whole history of row 21",
    );
    assert_eq!(
        full.len(),
        6,
        "an insert and five updates:\n{}",
        blackbox::scenario::lines(&full)
    );
    let all = ids(&full);

    let (response, first) = page(&sdk, 21, json!({ "limit": 2 }), "limit 2");
    assert_eq!(ids(&first), all[0..2], "limit 2: the two newest");
    assert_one_history_entry(&sdk, &response, &user, "user", 2, &[], "limit 2");
    let (_, second) = page(
        &sdk,
        21,
        json!({ "limit": 2, "before": all[1] }),
        "the second page",
    );
    assert_eq!(
        ids(&second),
        all[2..4],
        "limit 2, before the second change: the next two"
    );
    let (_, third) = page(
        &sdk,
        21,
        json!({ "limit": 2, "before": all[3] }),
        "the third page",
    );
    assert_eq!(ids(&third), all[4..6], "the last two");
    let (response, past) = page(
        &sdk,
        21,
        json!({ "limit": 2, "before": all[5] }),
        "before the oldest",
    );
    assert!(
        past.is_empty(),
        "nothing is older than the insert: {past:?}"
    );
    assert_one_history_entry(&sdk, &response, &user, "user", 0, &[], "before the oldest");
    let (_, older) = page(&sdk, 21, json!({ "before": all[2] }), "before alone");
    assert_eq!(
        ids(&older),
        all[3..6],
        "before alone: every change older than it"
    );
    // The limit is not written into the statement.
    let (response, many) = page(&sdk, 21, json!({ "limit": 977 }), "limit 977");
    assert_eq!(ids(&many), all, "limit 977");
    assert_one_history_entry(&sdk, &response, &user, "user", 6, &["977"], "limit 977");
    let (_, one) = page(&sdk, 21, json!({ "limit": 1 }), "limit 1");
    assert_eq!(ids(&one), all[0..1], "limit 1, the least allowed");
}

#[test]
fn the_limit_is_100_by_default_and_1000_at_most() {
    let sdk = chronicle();
    let user = ann();
    sdk.get_ok(&user, "/tasks/put?as=user&id=31&v=a");
    sdk.get_ok(&user, "/tasks/bump?as=user&id=31&n=120");
    let (_, all) = page(&sdk, 31, json!({ "limit": 1000 }), "limit 1000");
    assert_eq!(
        all.len(),
        121,
        "limit 1000, the most allowed: an insert and 120 updates"
    );
    assert_eq!(
        all.last().map(|c| c["kind"].clone()),
        Some(d::string("insert")),
        "the oldest is the insert"
    );
    let (response, default) = HistoryCall::of("tasks")
        .key(task_key(31))
        .changes(&sdk, &user, "no limit");
    assert_eq!(
        ids(&default),
        ids(&all)[0..100],
        "without a limit: the 100 newest"
    );
    assert_one_history_entry(&sdk, &response, &user, "user", 100, &[], "no limit");
}
