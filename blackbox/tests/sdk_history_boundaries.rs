//! Only its own, and only to read (CONTRACT.md, "Reading the Change
//! history: Only its own, and only to read" and "The call"; ticket "Read a
//! row's Change history through the SDK", criteria 3 and 4):
//!
//! - two Applications, each with a table `tasks` and a row of the same key,
//!   changed by different Users: each reads only its own changes;
//! - `history` forced past the types on a shared database's handle throws
//!   an `Error` saying the Change history is kept only in the own database,
//!   and nothing is sent: no host call, no Operation log entry, nothing from
//!   the program in the shared database's server log;
//! - an Application that declares `vm.history` as a Drizzle table of its
//!   own and inserts, updates or deletes through it gets `OperationFailed`
//!   of kind `database`, code `42501`, and the history is unchanged;
//! - `@vm/sdk` exports no table object, and the handles have no call about
//!   history but `history`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::{history, history_where};
use blackbox::sdk::{ann, d};
use blackbox::sdk_history::{
    CHRONICLE, CHRONICLE_TWIN, HistoryCall, Want, assert_changes, assert_nothing_sent, bob, carol,
    caught_of, chronicle, get_ok_on, install_twin, row_of, task_key,
};
use blackbox::transactions::assert_program_never_sent;
use serde_json::json;

const ACTORS: [&str; 2] = ["user", "application"];

#[test]
fn each_application_reads_only_its_own_history() {
    let sdk = chronicle();
    let twin_db = install_twin(&sdk);
    let (ann, bob, carol) = (ann(), bob(), carol());
    let (put, body) = sdk.get_ok(&ann, "/tasks/put?as=user&id=1&v=a");
    let a = row_of(&body);
    let (set, body) = sdk.get_ok(&ann, "/tasks/set?as=user&id=1&v=b");
    let b = row_of(&body);
    let (twin_put, _) = get_ok_on(&sdk, CHRONICLE_TWIN, &bob, "/tasks/put?id=1&title=twin");
    let (twin_set, _) = get_ok_on(
        &sdk,
        CHRONICLE_TWIN,
        &bob,
        "/tasks/set?id=1&title=twin%20again",
    );
    assert!(
        twin_db.name != sdk.own.name,
        "the harness's own check: two own databases"
    );

    for actor in ACTORS {
        let context = format!("{CHRONICLE} reads history(tasks, {{ id: 1 }}) as the {actor}");
        let (_, changes) = HistoryCall::of("tasks")
            .key(task_key(1))
            .as_actor(actor)
            .changes(&sdk, &carol, &context);
        assert_changes(
            &changes,
            &[
                Want::update(
                    &ann.id,
                    "user",
                    &set.request_id(),
                    &a,
                    &b,
                    &["title", "dueAt", "amount", "meta", "tags", "big"],
                ),
                Want::insert(&ann.id, "user", &put.request_id(), &a),
            ],
            &format!("{context}: only its own changes, none of {CHRONICLE_TWIN}'s"),
        );
    }

    let (_, body) = get_ok_on(&sdk, CHRONICLE_TWIN, &carol, "/history?id=1");
    let changes = body["changes"].as_array().cloned().unwrap_or_else(|| {
        panic!(
            "{CHRONICLE_TWIN}: expected history(tasks, {{ id: 1 }}) to answer changes, got {body}"
        )
    });
    let row = |title: &str| d::object(&[("id", d::number("1")), ("title", d::string(title))]);
    let expected = [
        json!({
            "kind": d::string("update"), "old": row("twin"), "new": row("twin again"),
            "userId": d::string(&bob.id), "application": d::string(CHRONICLE_TWIN), "actor": d::string("user"),
        }),
        json!({
            "kind": d::string("insert"), "old": d::null(), "new": row("twin"),
            "userId": d::string(&bob.id), "application": d::string(CHRONICLE_TWIN), "actor": d::string("user"),
        }),
    ];
    assert_eq!(
        changes,
        expected,
        "{CHRONICLE_TWIN} reads only its own changes (requests {} and {}), none of {CHRONICLE}'s",
        twin_set.request_id(),
        twin_put.request_id()
    );
}

#[test]
fn history_forced_on_a_shared_handle_throws_and_sends_nothing() {
    let sdk = chronicle();
    for resource in ["ops", "devops"] {
        let context = format!("history forced on the handle of {resource}");
        let (response, body) = sdk.get_ok(&ann(), &format!("/shared/history?resource={resource}"));
        let caught = caught_of(&body, &context);
        assert_eq!(caught["isError"], true, "{context}: an Error, got {caught}");
        let message = caught["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("Change history is kept only in the own database"),
            "{context}: expected the message to say `Change history is kept only in the own database`, got {message:?}"
        );
        assert_nothing_sent(&sdk, &response, &context);
    }
    assert_program_never_sent(
        &sdk.scenario.db,
        "history",
        "history forced on a shared handle",
    );
}

#[test]
fn writes_through_a_table_declared_over_vm_history_fail_and_change_nothing() {
    let sdk = chronicle();
    sdk.get_ok(&ann(), "/tasks/put?as=user&id=1&v=a");
    sdk.get_ok(&bob(), "/tasks/set?as=user&id=1&v=b");
    let before = history(&sdk.own);
    assert_eq!(
        history_where(&sdk.own, "h.table_name = 'tasks'").len(),
        2,
        "the harness's own check: two history rows of tasks to try to change"
    );
    for actor in ACTORS {
        for op in ["insert", "update", "delete"] {
            let context =
                format!("{op} through vm.history declared by the Application, as the {actor}");
            let (_, body) = sdk.get_ok(&ann(), &format!("/forged?as={actor}&op={op}"));
            let caught = caught_of(&body, &context);
            assert_eq!(
                caught["isOperationFailed"], true,
                "{context}: OperationFailed, got {caught}"
            );
            assert_eq!(caught["kind"], "database", "{context}: {caught}");
            assert_eq!(
                caught["code"], "42501",
                "{context}: the runtime role may only read vm.history: {caught}"
            );
        }
    }
    assert_eq!(history(&sdk.own), before, "the history is unchanged");
}

#[test]
fn the_sdk_offers_nothing_that_writes_history() {
    let sdk = chronicle();
    let (_, body) = sdk.get_ok(&ann(), "/exports");
    assert!(
        body["names"].as_array().is_some_and(|n| !n.is_empty()),
        "the harness's own check: the fixture lists what @vm/sdk exports: {body}"
    );
    assert_eq!(
        body["tables"],
        json!([]),
        "@vm/sdk exports no table object: {body}"
    );
    for handle in ["asUser", "asApplication"] {
        assert_eq!(
            body[handle],
            json!(["history"]),
            "the own database's handle ({handle}) has `history` and no other call about history: {body}"
        );
    }
}
