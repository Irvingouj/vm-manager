//! A revoke holds at once (CONTRACT.md, "Resources and Grants in full:
//! Revoking a Grant"): the program is started with `--session-renewal
//! 86400`, so that nothing of a User's Groups is read again in the time of
//! the test, and the operation made after the `200` is refused `no-grant`
//! all the same, in every Application; a User who holds another Group's
//! lower Grant keeps it; the later operations of a request in flight are
//! refused; a statement under way is not cut; a transaction already open
//! has its next statement refused and is not rolled back by the revoke
//! itself.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::lost_answer::host_call_of;
use blackbox::records::{event, user_of};
use blackbox::resources::revoke;
use blackbox::scenario::{
    CONTACT, Expected, RESOURCE, Scenario, assert_entry, assert_refusal, assert_rows, op, sql_on,
};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, Target, Txs, begin, query_in, rollback, sleep, slow_select, wanda,
};
use blackbox::{User, VmOptions};
use serde_json::{Value, json};

const CREW: &str = "crew";
const LOW: &str = "low-readers";

fn who() -> Value {
    sql_on(RESOURCE, "select current_user as who")
}

fn write() -> Value {
    sql_on(
        RESOURCE,
        "insert into notes (body) values ('revoke-at-once') returning body",
    )
}

/// The program with a renewal of a day, `devops` with a read and a write
/// login, [`CREW`] granted `write` and [`LOW`] `read`, the Applications
/// `board` (declares `write`) and `viewer` (declares `read`).
fn world() -> Scenario {
    let s = Scenario::start_with(&VmOptions::new().args_extra(&["--session-renewal", "86400"]));
    s.register_writable(RESOURCE);
    s.grant(RESOURCE, CREW, "write");
    s.grant(RESOURCE, LOW, "read");
    s.install("board", "operations.js", &[(RESOURCE, "write")]);
    s.install("viewer", "operations.js", &[(RESOURCE, "read")]);
    s
}

#[test]
fn after_the_200_an_operation_is_no_grant_with_held_null_in_two_applications() {
    let s = world();
    let carl = User::new("carl").in_groups(&[CREW]);
    // The harness's own check: carl reads and writes before.
    let (_, answer) = s.one(&carl, "viewer", &op("query", &who()));
    assert!(answer["ok"].is_object(), "before the revoke: {answer}");
    let (_, answer) = s.one(&carl, "board", &op("execute", &write()));
    assert!(answer["ok"].is_object(), "before the revoke: {answer}");

    let answer = revoke(&s.admin(), RESOURCE, CREW);
    answer.expect_status(200);
    assert_eq!(answer.json()["changed"], json!(true));

    for (application, call, needed) in [
        ("board", op("execute", &write()), "write"),
        ("viewer", op("query", &who()), "read"),
    ] {
        let (response, answer) = s.one(&carl, application, &call);
        assert_refusal(
            &answer,
            "no-grant",
            &json!({ "resource": RESOURCE, "user": "carl", "needed": needed, "held": null, "contact": CONTACT }),
            &format!(
                "an operation of {application} right after the revoke, with no renewal of the session"
            ),
        );
        let id = response.request_id();
        assert_entry(
            &s.entry_of(&response),
            &Expected {
                operation: if needed == "write" {
                    "execute"
                } else {
                    "query"
                },
                ..Expected::refused(&id, "carl", application, "no-grant")
            },
            "the Operation log entry of the refused operation",
        );
    }
}

#[test]
fn a_user_with_another_groups_lower_grant_keeps_it() {
    let s = world();
    let both = User::new("both").in_groups(&[CREW, LOW]);
    revoke(&s.admin(), RESOURCE, CREW).expect_status(200);
    let (_, refused) = s.one(&both, "board", &op("execute", &write()));
    assert_refusal(
        &refused,
        "no-grant",
        &json!({ "resource": RESOURCE, "user": "both", "needed": "write", "held": "read", "contact": CONTACT }),
        "a write by a User whose other Group holds `read`",
    );
    let (_, allowed) = s.one(&both, "viewer", &op("query", &who()));
    assert_rows(
        &allowed,
        &["who"],
        &json!([[s.db.reader.role]]),
        "a read by the same User",
    );
}

#[test]
fn a_later_operation_of_a_request_in_flight_is_refused() {
    let s = world();
    let flyer = User::new("flyer").in_groups(&[CREW]);
    let body = json!({
        "calls": [op("query", &who()), op("query", &who())],
        "pauseMs": 4000,
    });
    let (answers, revoked_at, answered_at) = thread::scope(|scope| {
        let sending = scope.spawn(|| {
            let (_, answers) = s.run_within(&flyer, "viewer", &body, Duration::from_secs(40));
            (answers, Instant::now())
        });
        let first = host_call_of(&s.vm, "flyer");
        assert!(
            first.is_some(),
            "the harness's own check: the first call was made"
        );
        let answer = revoke(&s.admin(), RESOURCE, CREW);
        answer.expect_status(200);
        let revoked_at = Instant::now();
        let (answers, answered_at) = sending.join().unwrap();
        (answers, revoked_at, answered_at)
    });
    assert!(answered_at > revoked_at);
    assert_rows(
        &answers[0],
        &["who"],
        &json!([[s.db.reader.role]]),
        "the operation made before the revoke",
    );
    assert_refusal(
        &answers[1],
        "no-grant",
        &json!({ "resource": RESOURCE, "user": "flyer", "needed": "read", "held": null, "contact": CONTACT }),
        "the later operation of the same request",
    );
}

#[test]
fn a_statement_under_way_is_not_cut() {
    let s = world();
    let runner = User::new("runner").in_groups(&[CREW]);
    let marker = format!("under-way-{}", token());
    let call = op("query", &sql_on(RESOURCE, &slow_select(4.0, &marker)));
    let answer = thread::scope(|scope| {
        let sending = scope.spawn(|| s.one(&runner, "viewer", &call).1);
        assert!(
            s.db.wait_until_received(&marker, Duration::from_secs(10)),
            "the harness's own check: the statement reached the shared database"
        );
        revoke(&s.admin(), RESOURCE, CREW).expect_status(200);
        sending.join().unwrap()
    });
    assert_eq!(
        answer["ok"]["rows"][0][0],
        json!(marker),
        "expected the statement that was running to finish with its rows, got {answer}"
    );
}

#[test]
fn a_transaction_already_open_has_its_next_statement_refused_and_is_not_rolled_back_by_the_revoke()
{
    let t = Txs::start_with(&VmOptions::new().args_extra(&["--session-renewal", "86400"]));
    let trent = User::new("trent").in_groups(&["writers"]);
    assert_ne!(trent.id, wanda().id);
    let target = Target::Shared(RESOURCE);
    let steps = [
        begin(target, "write", "t"),
        query_in("t", "select 1 as one"),
        sleep(5000),
        query_in("t", "select 2 as two"),
        rollback("t"),
    ];
    let (answers, still_open) = thread::scope(|scope| {
        let sending = scope.spawn(|| t.run(&trent, APP, &steps).1);
        // The begin and the first statement are decided.
        let deadline = Instant::now() + Duration::from_secs(10);
        while t
            .vm()
            .records()
            .iter()
            .filter(|r| event(r) == "host-call" && user_of(r) == Some("trent"))
            .count()
            < 2
        {
            assert!(
                Instant::now() < deadline,
                "the harness's own check: the first statement of the transaction was decided"
            );
            thread::sleep(Duration::from_millis(20));
        }
        revoke(&t.scenario.admin(), RESOURCE, "writers").expect_status(200);
        // The revoke itself rolls nothing back: the transaction is still
        // open at the shared database while the request sleeps.
        let still_open = t.sessions_in_transaction(target, Duration::from_millis(300));
        (sending.join().unwrap(), still_open)
    });
    assert!(
        !still_open.is_empty(),
        "expected the open transaction to be still open at the shared database after the revoke, there is none"
    );
    assert!(
        answers[1]["ok"].is_object(),
        "before the revoke: {}",
        answers[1]
    );
    assert_eq!(
        answers[3]["refused"]["refusal"]["reason"],
        json!("no-grant"),
        "expected the next statement of the open transaction refused `no-grant`, got {}",
        answers[3]
    );
}
