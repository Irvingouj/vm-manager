//! A disable or an enable whose answer is lost (CONTRACT.md, "What
//! Platform administrators see and stop: A write whose answer is lost",
//! and "A write whose answer is lost: The rule"), on a platform database on
//! a Postgres of each test's own whose commits the harness holds
//! ([`HeldPostgres`]): the disable is answered `503` `unavailable`;
//! meanwhile every request to that Application's address is `503` with
//! `retry-after`, at once, and none of its code runs; once the commit goes
//! through, requests are answered as the database says (disabled, or
//! served). Nobody is served, and nobody told it is disabled, on a guess.
//!
//! A disable or an enable has its own bound of 10 seconds, whatever longer
//! bound another request of the API has: the `503` must come within
//! [`OWN_BOUND`] and a margin, not at some other request's limit.
//!
//! And a disable made while the program is finding out an Application's
//! lists after a change of them lost its answer still stops the request in
//! flight (CONTRACT.md, "What Platform administrators see and stop:
//! Requests in flight": "whatever the program then knew of the
//! Application").

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{
    self, IN_FLIGHT_ALLOWED, administrator, assert_disabled_plain, disable, disable_ok,
};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, QUICK, SETTLE, assert_503_retry};
use blackbox::scenario::{READERS, RESOURCE, Scenario, WRITERS, lines};
use blackbox::sharing::{TRACED, add, user};
use blackbox::transactions::{
    self, APP, TX_REQUEST_TIMEOUT, Target, begin, commit, execute_in, insert_marker, outside,
    sleep, wanda,
};
use blackbox::{INSTALLER, Response, User, Vm, VmOptions, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

fn bob() -> User {
    User::new("bob")
}

/// A held Postgres, and the program on a platform database there with
/// `board` (`fixtures/honest.js`) shared with everyone.
fn with_board(name: &str) -> (HeldPostgres, Vm) {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform(name);
    let vm = Vm::start(&admin::options().platform(&platform));
    vm.api_as(&User::new("alice"))
        .create_with_bundle("board", &fixture_bundle(TRACED));
    (postgres, vm)
}

fn open(vm: &Vm) -> Response {
    vm.request("board", "/hello")
        .as_user(&bob())
        .header("accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
}

/// `open`, which must be answered within [`QUICK`].
fn open_quickly(vm: &Vm, context: &str) -> Response {
    let began = Instant::now();
    let response = open(vm);
    assert!(
        began.elapsed() < QUICK,
        "{context}: expected an answer at once, it took {:?}\n{}",
        began.elapsed(),
        response.describe()
    );
    response
}

/// The bound of a disable's or an enable's write (CONTRACT.md, "What
/// Platform administrators see and stop").
const OWN_BOUND: Duration = Duration::from_secs(10);

/// What a `503` may come after [`OWN_BOUND`]: the request's way there and
/// back on a busy machine.
const MARGIN: Duration = Duration::from_secs(3);

/// Sends `action` as the administrator while commits are held: answered
/// `503` `unavailable`, within [`OWN_BOUND`] and [`MARGIN`]. The request's
/// own timeout is only an outer cap so that a program that never answers
/// fails the test rather than hanging it.
fn lost(vm: &Vm, action: &str) {
    let began = Instant::now();
    let answer = vm
        .api_as(&administrator())
        .request(&format!("/v1/applications/board/{action}"))
        .method("POST")
        .json(&json!({}))
        .timeout(Duration::from_secs(60))
        .send();
    let took = began.elapsed();
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        took < OWN_BOUND + MARGIN,
        "expected the {action}'s `503` within its own {OWN_BOUND:?} (and {MARGIN:?}), it took {took:?}"
    );
}

/// For a few seconds: every request is the `503` with `retry-after`, at
/// once, and none of the Application's code runs.
fn meanwhile(vm: &Vm, context: &str) {
    for round in 0..5 {
        let response = open_quickly(vm, &format!("{context}, round {round}"));
        assert_503_retry(&response, &format!("{context}, round {round}"));
        vm.records_of(&response).assert_no_application_code_ran();
        thread::sleep(Duration::from_millis(300));
    }
}

#[test]
fn a_disable_whose_answer_is_lost_serves_nobody_on_a_guess_and_settles_disabled() {
    let (postgres, vm) = with_board("vm_platform_lost_disable");
    open(&vm).expect_status(200);
    postgres.hold();
    lost(&vm, "disable");
    assert!(
        postgres.waiting() > 0,
        "the disable's commit is still on its way"
    );
    meanwhile(&vm, "while the disable's commit waits");
    postgres.let_through();
    let deadline = Instant::now() + SETTLE;
    loop {
        let response = open_quickly(&vm, "once commits go through");
        assert!(
            response.status == 503,
            "expected `503` until settled and then the disabled answer; never served\n{}",
            response.describe()
        );
        if response.header("retry-after").is_none() {
            assert_disabled_plain(&response, "board", "settled");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the disable settled within {SETTLE:?}\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
    let read = vm.api_as(&administrator()).application("board");
    assert_eq!(read["disabled"]["by"], administrator().id, "{read}");
}

#[test]
fn an_enable_whose_answer_is_lost_is_not_refused_for_ever_and_settles_served() {
    let (postgres, vm) = with_board("vm_platform_lost_enable");
    disable_ok(&vm, "board");
    postgres.hold();
    lost(&vm, "enable");
    meanwhile(&vm, "while the enable's commit waits");
    postgres.let_through();
    let deadline = Instant::now() + SETTLE;
    loop {
        let response = open_quickly(&vm, "once commits go through");
        if response.status == 200 {
            assert!(
                response.text().contains("Hello, bob!"),
                "{}",
                response.describe()
            );
            break;
        }
        assert_503_retry(&response, "until the enable is settled");
        assert!(
            Instant::now() < deadline,
            "expected the enable settled within {SETTLE:?}\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert!(vm.api_as(&administrator()).application("board")["disabled"].is_null());
}

/// How long the request in flight waits in its code, after its first
/// query, before it begins its transaction: past the lost change's `503`
/// (the program gives up on a write after 20 s) and the harness letting
/// commits through, so that the transaction is begun and written while the
/// program has not yet found out the lists, and is open for less than the
/// 10 s a transaction may be; and short of the 30 s a request is given.
const BEGIN_AFTER_MS: u64 = 23_000;

/// The request's whole time: what an answer must come before for it not to
/// be the program's answer to a request past its time.
const REQUEST_TIME: Duration = Duration::from_secs(30);

/// Waits until the Operation log satisfies `done`.
fn wait_for_log(s: &Scenario, within: Duration, done: impl Fn(&[Value]) -> bool, context: &str) {
    let deadline = Instant::now() + within;
    loop {
        let entries = s.log();
        if done(&entries) {
            return;
        }
        if Instant::now() >= deadline {
            s.vm.fail(&format!(
                "the harness's own wait, {context}: the Operation log did not come to it within {within:?}:\n{}",
                lines(&entries)
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_disable_while_the_program_finds_out_lost_lists_still_stops_the_request_in_flight() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_lists_disable");
    let scenario = Scenario::start_with(&VmOptions::new().platform(&platform));
    scenario.register_writable(RESOURCE);
    scenario.grant(RESOURCE, READERS, "read");
    scenario.grant(RESOURCE, WRITERS, "write");
    transactions::install(&scenario.vm, APP, &[(RESOURCE, "write")]);
    let s = &scenario;
    let vm = &s.vm;
    let devops = Target::Shared(RESOURCE);
    let marker = format!("lost-lists-{}", blackbox::shared_database::token());
    let steps = vec![
        outside("query", devops, "select 1 as one"),
        sleep(BEGIN_AFTER_MS),
        begin(devops, "write", "t"),
        execute_in("t", &insert_marker(devops, &marker)),
        sleep(20_000),
        commit("t"),
    ];
    let is_wandas = |e: &Value, operation: &str| {
        e["user"] == "wanda"
            && e["application"] == APP
            && e["operation"] == operation
            && e["outcome"] == "allowed"
    };
    let started = Instant::now();
    let (answer, answered_at, disabled_at) = thread::scope(|scope| {
        let running = scope.spawn(|| {
            let r = vm
                .request(APP, "/")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "steps": steps }))
                .timeout(TX_REQUEST_TIMEOUT)
                .send();
            (r, Instant::now())
        });
        // The request is in its code: its first query's entry is written,
        // before commits are held.
        wait_for_log(
            s,
            Duration::from_secs(10),
            |entries| entries.iter().any(|e| is_wandas(e, "query")),
            "the request's first query",
        );
        // A change of the Application's lists loses its answer.
        postgres.hold();
        let lost = vm
            .api_as(&User::new(INSTALLER))
            .request(&format!("/v1/applications/{APP}/sharing"))
            .method("POST")
            .json(&add(&[user("carol")]))
            .timeout(LOST_WRITE_TIMEOUT)
            .send();
        assert_api_error(&lost, 503, "unavailable");
        postgres.assert_waiting(1, "after the sharing change's `503`");
        postgres.let_through();
        // The harness's own check of the timing: the request's code has
        // not begun its transaction yet.
        let through = started.elapsed();
        assert!(
            through + Duration::from_millis(500) < Duration::from_millis(BEGIN_AFTER_MS),
            "the harness's own check: commits were let through {through:?} after the request began, too late for its transaction ({BEGIN_AFTER_MS} ms)"
        );
        // Nothing has been sent to the Application's address since: the
        // program has not had to find out its lists. The transaction is
        // begun and has written.
        wait_for_log(
            s,
            Duration::from_millis(BEGIN_AFTER_MS) + Duration::from_secs(5),
            |entries| entries.iter().any(|e| is_wandas(e, "execute")),
            "the request's transaction written",
        );
        let answer = disable(vm, &administrator(), APP);
        let disabled_at = Instant::now();
        if answer.status != 200 {
            vm.fail(&format!(
                "expected `200` disabling {APP:?} while the program finds out its lists after a change whose answer was lost, got\n{}",
                answer.describe()
            ));
        }
        let (response, at) = running.join().unwrap();
        (response, at, disabled_at)
    });
    let after = answered_at.saturating_duration_since(disabled_at);
    assert!(
        after <= IN_FLIGHT_ALLOWED && answered_at.duration_since(started) < REQUEST_TIME,
        "expected the request in flight answered within 1 s of the disable's `200` ({IN_FLIGHT_ALLOWED:?} allowed on a loaded machine), whatever the program then knew of the Application; it was answered {after:?} after it\n{}",
        answer.describe()
    );
    assert_disabled_plain(&answer, APP, "the request in flight");
    // Its transaction is rolled back by the host, `error`
    // `application-disabled`; its row is not kept.
    let deadline = Instant::now() + Duration::from_secs(5);
    let entries = loop {
        let entries = s.entries_of(&answer);
        if entries.iter().any(|e| e["operation"] == "rollback") || Instant::now() >= deadline {
            break entries;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let began = entries
        .iter()
        .find(|e| e["operation"] == "begin")
        .unwrap_or_else(|| vm.fail(&format!("no `begin` entry:\n{}", lines(&entries))));
    let id = began["transaction"].as_str().unwrap_or_default().to_owned();
    let rollback = transactions::the_rollback(&entries, &id, "the disabled request");
    assert_eq!(
        rollback["error"], "application-disabled",
        "expected the host's rollback to say why: {rollback}"
    );
    assert!(
        !entries
            .iter()
            .any(|e| e["operation"] == "commit" && e["outcome"] == "allowed"),
        "nothing it had not committed is kept:\n{}",
        lines(&entries)
    );
    let kept = s.db.superuser().value(&format!(
        "SELECT count(*) FROM notes WHERE body LIKE '%{marker}%'"
    ));
    assert_eq!(
        kept.as_deref(),
        Some("0"),
        "the row written in the transaction is not kept"
    );
}
