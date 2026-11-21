//! Bounds of the readings for Platform administrators (CONTRACT.md, "What
//! Platform administrators see and stop: Bounds and costs", "Reading the
//! Operation log"): at most 2 readings of `GET /v1/admin/applications` and
//! `GET /v1/operation-log` run at once, a third is answered `503`
//! `unavailable` at once, and requests to Applications are served
//! meanwhile; a reading of the Operation log that takes more than 10
//! seconds in the platform database is ended and answered `503`
//! `unavailable`, saying to narrow it.
//!
//! How the harness holds readings: as the Postgres administrator it takes
//! an `ACCESS EXCLUSIVE` lock on every table of the platform database (the
//! catalog says which there are) in a transaction it keeps open, so that a
//! reading waits on it; the program holds what Applications need in
//! memory and goes on serving them. The lock is let go by ending the
//! transaction.

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{self, LIST, OPERATION_LOG, administrator, platform_sql};
use blackbox::sharing::TRACED;
use blackbox::sql::Sql;
use blackbox::{Response, User, Vm, assert_api_error, fixture_bundle};

/// Locks every table of the platform database in a transaction of this
/// connection, which stays open until [`let_go`].
fn lock_everything(vm: &Vm) -> Sql {
    let sql = platform_sql(vm);
    let tables: Vec<String> = sql
        .batch(
            "SELECT quote_ident(schemaname) || '.' || quote_ident(tablename) FROM pg_tables \
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1",
        )
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    assert!(
        !tables.is_empty(),
        "the harness's own check: the platform database has tables"
    );
    sql.batch(&format!(
        "BEGIN; SET LOCAL lock_timeout = '10s'; LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        tables.join(", ")
    ));
    sql
}

fn let_go(sql: &Sql) {
    sql.batch("ROLLBACK");
}

/// How many sessions of the platform database wait on a lock.
fn waiting_on_locks(vm: &Vm) -> usize {
    platform_sql(vm)
        .value(&format!(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = '{}' AND wait_event_type = 'Lock'",
            vm.platform().name()
        ))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn read(vm: &Vm, target: &str) -> Response {
    vm.api_as(&administrator())
        .request(target)
        .timeout(Duration::from_secs(40))
        .send()
}

#[test]
fn a_third_reading_at_once_is_answered_503_at_once_and_applications_are_served_meanwhile() {
    let vm = admin::start();
    vm.api_as(&User::new("alice"))
        .create_with_bundle("board", &fixture_bundle(TRACED));
    // Warm: the program has read what it needs.
    read(&vm, LIST).expect_status(200);
    let lock = lock_everything(&vm);
    let (first, second, third, took, served) = thread::scope(|scope| {
        let first = scope.spawn(|| read(&vm, LIST));
        let second = scope.spawn(|| read(&vm, OPERATION_LOG));
        // Both are waiting in the platform database.
        let deadline = Instant::now() + Duration::from_secs(5);
        while waiting_on_locks(&vm) < 2 {
            assert!(
                Instant::now() < deadline,
                "the harness's own check: expected two readings waiting on the lock within 5 s, {} are",
                waiting_on_locks(&vm)
            );
            thread::sleep(Duration::from_millis(20));
        }
        let began = Instant::now();
        let third = read(&vm, &format!("{OPERATION_LOG}?limit=1"));
        let took = began.elapsed();
        let served = vm
            .request("board", "/hello")
            .as_user(&User::new("bob"))
            .header("accept", "application/json")
            .timeout(Duration::from_secs(5))
            .send();
        let_go(&lock);
        (
            first.join().unwrap(),
            second.join().unwrap(),
            third,
            took,
            served,
        )
    });
    assert_api_error(&third, 503, "unavailable");
    assert!(
        took < Duration::from_secs(2),
        "expected the third reading answered at once, it took {took:?}"
    );
    let message = third.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("again"),
        "expected the message to say readings for Platform administrators are running and to try again shortly, got {message:?}"
    );
    assert_eq!(
        served.status,
        200,
        "requests to Applications are served meanwhile\n{}",
        served.describe()
    );
    for response in [&first, &second] {
        assert_eq!(response.status, 200, "{}", response.describe());
    }
    // Once they are done, readings are taken again.
    read(&vm, LIST).expect_status(200);
    read(&vm, OPERATION_LOG).expect_status(200);
}

#[test]
fn a_reading_of_the_operation_log_past_10_seconds_is_ended_and_says_to_narrow_it() {
    let vm = admin::start();
    read(&vm, OPERATION_LOG).expect_status(200);
    let lock = lock_everything(&vm);
    let began = Instant::now();
    let answer = read(&vm, &format!("{OPERATION_LOG}?outcome=failed"));
    let took = began.elapsed();
    let_go(&lock);
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(16),
        "expected the reading ended after its 10 seconds (allowing 6 s on a loaded machine), it took {took:?}"
    );
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("too long") && (message.contains("since") || message.contains("narrow")),
        "expected the message to say the reading took too long and to narrow it, got {message:?}"
    );
}
