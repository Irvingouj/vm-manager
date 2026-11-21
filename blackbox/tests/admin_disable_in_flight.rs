//! Requests in flight when an Application is disabled (CONTRACT.md, "What
//! Platform administrators see and stop: Requests in flight"): within 1
//! second of the disable's `200` every request in flight is answered as a
//! disabled Application answers, whatever its code was doing; its code is
//! stopped; a transaction it left open is rolled back with a `rollback`
//! entry whose `error` is `application-disabled`; a statement still
//! running is cancelled, its entry `failed` with `application-disabled`;
//! no operation is allowed after the `200`; its entry in the logs has
//! `status` `503` and what it printed until it was stopped; it is counted.
//!
//! The promise is 1 second. On a loaded machine (the gate starts many
//! programs at once) the measurement is given one second more: an answer
//! is taken within [`IN_FLIGHT_ALLOWED`] (2 s) of the `200`, and the
//! request's code was set to hold it for 8 s or more, so that an answer
//! within that is not the code's own.

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{
    self, IN_FLIGHT_ALLOWED, WAITS, administrator, assert_disabled_page, assert_disabled_plain,
    disable, wait_for_printed, wait_for_usage,
};
use blackbox::browser::NAVIGATION_ACCEPT;
use blackbox::logs::wait_for_entry;
use blackbox::records::{event, request_of};
use blackbox::scenario::{RESOURCE, lines};
use blackbox::transactions::{
    APP, Target, Txs, begin, execute_in, insert_marker, outside, slow_select, step, wanda,
};
use blackbox::{Response, User, Vm, fixture_bundle};
use serde_json::{Value, json};

/// How long the code of a request in flight is set to hold it.
const HELD_MS: u64 = 8000;

/// How long the harness waits for the answer of a request in flight.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(40);

fn bob() -> User {
    User::new("bob")
}

/// Disables `name` as the administrator, failing unless `200`; returns
/// when its `200` was received.
fn disable_now(vm: &Vm, name: &str) -> Instant {
    let answer = disable(vm, &administrator(), name);
    let at = Instant::now();
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` disabling {name:?}, got\n{}",
            answer.describe()
        ));
    }
    at
}

/// The records of request `id` in the program's log so far.
fn records_of(vm: &Vm, id: &str) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| request_of(r) == Some(id))
        .collect()
}

#[test]
fn requests_held_by_their_code_are_answered_disabled_within_a_second_and_their_code_is_stopped() {
    let vm = admin::start();
    vm.api_as(&User::new("alice"))
        .create_with_bundle("board", &fixture_bundle(WAITS));
    let plain = vm
        .request("board", &format!("/plain?ms={HELD_MS}"))
        .as_user(&bob())
        .header("accept", "application/json")
        .timeout(ANSWER_TIMEOUT);
    let page = vm
        .request("board", &format!("/page?ms={HELD_MS}"))
        .as_user(&bob())
        .header("accept", NAVIGATION_ACCEPT)
        .timeout(ANSWER_TIMEOUT);
    let started = Instant::now();
    let (plain_answer, page_answer, disabled_at) = thread::scope(|scope| {
        let plain = scope.spawn(|| {
            let r = plain.send();
            (r, Instant::now())
        });
        let page = scope.spawn(|| {
            let r = page.send();
            (r, Instant::now())
        });
        // Both are in their code: each has printed its first line.
        wait_until(&vm, Duration::from_secs(5), |records| {
            ["/plain", "/page"].iter().all(|path| {
                records.iter().any(|r| {
                    event(r) == "printed"
                        && r["message"].as_str() == Some(&format!("waits: began {path}"))
                })
            })
        });
        let disabled_at = disable_now(&vm, "board");
        (plain.join().unwrap(), page.join().unwrap(), disabled_at)
    });
    let held = Duration::from_millis(HELD_MS);
    for (what, (response, at)) in [("plain", &plain_answer), ("page", &page_answer)] {
        let after = at.saturating_duration_since(disabled_at);
        assert!(
            after <= IN_FLIGHT_ALLOWED && at.duration_since(started) < held,
            "{what}: expected the request in flight answered within 1 s of the disable's `200` ({IN_FLIGHT_ALLOWED:?} allowed on a loaded machine), it was answered {after:?} after it\n{}",
            response.describe()
        );
    }
    assert_disabled_plain(
        &plain_answer.0,
        "board",
        "the request in flight, not a page",
    );
    assert_disabled_page(
        &page_answer.0,
        "board",
        &["bob"],
        &[&administrator().id],
        "the request in flight, a page",
    );
    // Its code was stopped: nothing it would do after its wait was done,
    // and no operation was allowed. Look again a while after its code
    // would have gone on.
    thread::sleep(held);
    for response in [&plain_answer.0, &page_answer.0] {
        let id = response.request_id();
        let records = records_of(&vm, &id);
        let late: Vec<&Value> = records
            .iter()
            .filter(|r| {
                (event(r) == "printed"
                    && r["message"].as_str().is_some_and(|m| {
                        m.starts_with("waits: done") || m.starts_with("waits: after")
                    }))
                    || (event(r) == "host-call" && r["outcome"] == "allowed")
            })
            .collect();
        assert!(
            late.is_empty(),
            "expected the code of request {id} stopped: nothing printed after its wait, no operation allowed; found\n{}",
            lines(&late.into_iter().cloned().collect::<Vec<_>>())
        );
        // A `whoami` it still called, if it got that far, is refused
        // `application-disabled`.
        for call in records.iter().filter(|r| event(r) == "host-call") {
            assert_eq!(call["outcome"], "refused", "{call}");
            assert_eq!(call["reason"], "application-disabled", "{call}");
        }
        let request = records
            .iter()
            .find(|r| event(r) == "request")
            .unwrap_or_else(|| vm.fail(&format!("no `request` record for {id}")));
        assert_eq!(request["status"], 503, "{request}");
    }
    // Its entry in the logs: `503`, and what it printed until it was
    // stopped.
    let alice = vm.api_as(&User::new("alice"));
    for (response, path) in [(&plain_answer.0, "/plain"), (&page_answer.0, "/page")] {
        let (entry, _) = wait_for_entry(&alice, "board", &response.request_id());
        assert_eq!(entry["status"], 503, "{entry}");
        let printed: Vec<&str> = entry["printed"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["message"].as_str())
            .collect();
        assert_eq!(
            printed,
            [format!("waits: began {path}").as_str()],
            "expected what its code printed until it was stopped, and no more: {entry}"
        );
    }
    // Both are counted: their code was given them.
    wait_for_usage(&vm, "board", 2, 1);
    // A new request is answered disabled at once, its code not given it.
    let after = vm
        .request("board", "/now")
        .as_user(&bob())
        .header("accept", "application/json")
        .send();
    assert_disabled_plain(&after, "board", "a request after the disable");
}

/// Waits until the program's log so far satisfies `done`.
fn wait_until(vm: &Vm, within: Duration, done: impl Fn(&[Value]) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        if done(&vm.records()) {
            return;
        }
        if Instant::now() >= deadline {
            vm.fail(&format!(
                "the harness's own wait: what it waited for in the log did not come within {within:?}"
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_open_transaction_is_rolled_back_and_a_running_statement_cancelled() {
    let t = Txs::start();
    let vm = t.vm();
    let marker = format!("in-flight-{}", blackbox::shared_database::token());
    let devops = Target::Shared(RESOURCE);
    // Request A: a transaction with a written row, then a long wait in
    // its code. Request B: a statement that runs for 8 s.
    let a_steps = vec![
        begin(devops, "write", "t"),
        execute_in("t", &insert_marker(devops, &marker)),
        json!({ "sleep": HELD_MS }),
        step("tx.commit", json!({ "tx": { "$ref": "t" } })),
    ];
    let b_steps = vec![outside("query", devops, &slow_select(8.0, &marker))];
    let started = Instant::now();
    let (a, b, disabled_at) = thread::scope(|scope| {
        let a = scope.spawn(|| {
            let r = t.send(&wanda(), APP, &a_steps);
            (r, Instant::now())
        });
        let b = scope.spawn(|| {
            let r = t.send(&wanda(), APP, &b_steps);
            (r, Instant::now())
        });
        // A has begun and written; B's statement runs.
        wait_for_log(&t, Duration::from_secs(10), |entries| {
            let written = entries
                .iter()
                .any(|e| e["operation"] == "execute" && e["outcome"] == "allowed");
            let running = entries
                .iter()
                .any(|e| e["operation"] == "query" && e["outcome"] == "unfinished");
            written && running
        });
        let disabled_at = disable_now(vm, APP);
        (a.join().unwrap(), b.join().unwrap(), disabled_at)
    });
    for (what, (response, at)) in [("the transaction", &a), ("the statement", &b)] {
        let after = at.saturating_duration_since(disabled_at);
        assert!(
            after <= IN_FLIGHT_ALLOWED
                && at.duration_since(started) < Duration::from_millis(HELD_MS),
            "{what}: expected it answered within 1 s of the disable's `200` ({IN_FLIGHT_ALLOWED:?} allowed), it took {after:?}\n{}",
            response.describe()
        );
        assert_disabled_plain(response, APP, what);
    }
    // A's transaction: rolled back by the host, `error`
    // `application-disabled`; its row is not kept; no commit.
    let a_entries = t.entries_eventually(&a.0, 3);
    let began = a_entries
        .iter()
        .find(|e| e["operation"] == "begin")
        .unwrap_or_else(|| vm.fail(&format!("no `begin` entry:\n{}", lines(&a_entries))));
    let id = began["transaction"].as_str().unwrap().to_owned();
    let rollback = blackbox::transactions::the_rollback(&a_entries, &id, "the disabled request");
    assert_eq!(
        rollback["error"], "application-disabled",
        "expected the host's rollback to say why: {rollback}"
    );
    assert!(
        !a_entries
            .iter()
            .any(|e| e["operation"] == "commit" && e["outcome"] == "allowed"),
        "nothing it had not committed is kept:\n{}",
        lines(&a_entries)
    );
    assert_eq!(
        t.rows_with(devops, &marker),
        0,
        "the row written in the transaction is not kept"
    );
    assert!(
        t.sessions_in_transaction(devops, Duration::from_secs(5))
            .is_empty(),
        "no session left in a transaction"
    );
    // B's statement: cancelled, its entry `failed` `application-disabled`.
    let deadline = Instant::now() + Duration::from_secs(10);
    let entry = loop {
        let entries = t.entries_of(&b.0);
        let found = entries.iter().find(|e| e["operation"] == "query").cloned();
        if let Some(entry) = found.filter(|e| e["outcome"] != "unfinished") {
            break entry;
        }
        assert!(
            Instant::now() < deadline,
            "expected the running statement's entry completed:\n{}",
            lines(&entries)
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(entry["outcome"], "failed", "{entry}");
    assert_eq!(entry["error"], "application-disabled", "{entry}");
    // After the `200`: no operation of the Application is allowed.
    let after: Vec<Value> = t
        .log()
        .into_iter()
        .filter(|e| e["application"] == APP)
        .filter(|e| {
            e["time"]
                .as_str()
                .and_then(admin::micros)
                .zip(rollback["time"].as_str().and_then(admin::micros))
                .is_some_and(|(at, rolled)| at > rolled)
        })
        .filter(|e| e["outcome"] == "allowed" && e["operation"] != "rollback")
        .collect();
    assert!(
        after.is_empty(),
        "expected no operation allowed after the disable:\n{}",
        lines(&after)
    );
    // A later request's operation is not given to its code at all.
    let later = t.send(
        &wanda(),
        APP,
        &[outside("query", devops, "select 1 as one")],
    );
    assert_disabled_plain(&later, APP, "a later request");
}

/// Waits until the Operation log satisfies `done`.
fn wait_for_log(t: &Txs, within: Duration, done: impl Fn(&[Value]) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        let entries = t.log();
        if done(&entries) {
            return;
        }
        if Instant::now() >= deadline {
            t.vm().fail(&format!(
                "the harness's own wait: the Operation log did not come to what it waited for within {within:?}:\n{}",
                lines(&entries)
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_request_in_flight_on_another_application_goes_on() {
    let vm = admin::start();
    let alice = vm.api_as(&User::new("alice"));
    alice.create_with_bundle("board", &fixture_bundle(WAITS));
    alice.create_with_bundle("other", &fixture_bundle(WAITS));
    let other = vm
        .request("other", "/?ms=3000")
        .as_user(&bob())
        .header("accept", "application/json")
        .timeout(ANSWER_TIMEOUT);
    let answer: Response = thread::scope(|scope| {
        let running = scope.spawn(|| other.send());
        wait_until(&vm, Duration::from_secs(5), |records| {
            records
                .iter()
                .any(|r| event(r) == "printed" && r["application"] == "other")
        });
        disable_now(&vm, "board");
        running.join().unwrap()
    });
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(body["whoami"]["ok"]["application"], "other", "{body}");
    let id = answer.request_id();
    wait_for_printed(&vm, &id, "waits: after whoami", Duration::from_secs(5));
}
