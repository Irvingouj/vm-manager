//! Shipping the Change history from the own database's outbox to the
//! platform database's `change_history` (CONTRACT.md, "The Change history
//! in the platform database: Shipping"; ticket "The Change history lives in
//! the platform database, shipped from an outbox", second, third and fourth
//! acceptance criteria).
//!
//! Every committed change is in `change_history` exactly once: after
//! ordinary writes as both actors, transactions committed, rolled back,
//! aborted and left open, cascades, and a migration's changes; when
//! `change_history` is held up by a lock (the write is answered all the
//! same, the log says `history-not-shipped` and nothing of the row, and a
//! later shipment takes the change); when the program is killed between a
//! commit and its shipment, or in the middle of a shipment (the next start
//! ships it); and when many writes and reads of the history, of one
//! Application or of two with the same tables, ask for shipments at once.
//! Batches are bounded (1,000 changes, 4 MiB of `old` and `new` past the
//! first), a statement of 100,000 rows is shipped whole and its time
//! written down, a change is there within 10 seconds of its write's answer
//! and before a later read of the history answers, and a shipment has no
//! Operation log entry.
//!
//! `change_history` is read as the harness's superuser in the platform
//! database, as the contract says the tests read it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::changed_outside::stop;
use blackbox::history::{
    APP, SHIPPED_WITHIN, history_standard, history_where, outbox_count, platform_sql, ship,
    wait_shipped, wait_shipped_within,
};
use blackbox::history_op::{changes_of, history_call};
use blackbox::history_shipping::{
    BATCH_BYTES, BATCH_ROWS, Blocker, LONG_REQUEST, LockedHistory, assert_no_duplicates, batches,
    count_now, inserted_title, install_transactions, max_id_now, postgres_version, records_holding,
    report_path, run_steps, titled_like, titles_not_once, wait_count, wait_for_record_within,
};
use blackbox::own_database::{Migration, Own, OwnDatabase, assert_ok, literal, own_execute};
use blackbox::transactions::{Target, begin, commit, execute_in, rollback, step};
use blackbox::vm::INSTALLER;
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

/// A program and [`APP`] with [`history_standard`].
fn installed() -> (Own, OwnDatabase) {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    (own, db)
}

/// One `execute` in `application` as `actor` for `user`, failing unless it
/// ran; returns the request's id and the answer.
fn execute_in_app(
    own: &Own,
    user: &User,
    application: &str,
    actor: &str,
    sql: &str,
) -> (String, Value) {
    let (response, answer) = own.one(user, application, &own_execute(actor, sql));
    assert_ok(
        &answer,
        &format!("the harness's own check: {sql} as the {actor}"),
    );
    (response.request_id(), answer)
}

/// One `execute` in [`APP`].
fn execute_ok(own: &Own, user: &User, actor: &str, sql: &str) -> (String, Value) {
    execute_in_app(own, user, APP, actor, sql)
}

/// One `execute` of a statement that changes many rows, waiting up to
/// [`LONG_REQUEST`] for its answer; returns when it was answered.
fn execute_long(own: &Own, application: &str, sql: &str) -> Instant {
    let (_, answers) = own.run_body(
        &alice(),
        application,
        &json!({ "calls": [own_execute("user", sql)] }),
        LONG_REQUEST,
    );
    let answered = Instant::now();
    assert_ok(
        &answers[0],
        &format!("the harness's own check: {sql} as the user"),
    );
    answered
}

/// Fails unless, once `vm.outbox` is empty (everything committed shipped,
/// within [`SHIPPED_WITHIN`] of the writes' answers), `db`'s Application
/// has exactly `want` rows of `change_history` satisfying `condition`.
fn assert_exactly(db: &OwnDatabase, condition: &str, want: i64, context: &str) {
    wait_shipped(db);
    let settled = count_now(db, condition);
    assert!(
        settled == want,
        "{context}: expected exactly {want} rows of `change_history` where {condition} once vm.outbox is empty (CONTRACT.md, \"Shipping: Exactly once\"), got {settled}; the rows:\n{}",
        history_where(db, condition)
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The index of `column` in an `ok` answer of `history`.
fn column_index(answer: &Value, column: &str) -> usize {
    answer["ok"]["columns"]
        .as_array()
        .and_then(|cs| cs.iter().position(|c| c == column))
        .unwrap_or_else(|| panic!("expected the column {column:?} among the `columns` of {answer}"))
}

#[test]
fn every_change_of_ordinary_writes_as_both_actors_is_shipped_exactly_once() {
    let (own, db) = installed();
    // Per request, the number of changes it makes.
    let mut expected: Vec<(String, i64)> = Vec::new();
    for (actor, user, base) in [("user", alice(), 1000), ("application", bob(), 2000)] {
        let calls: [(String, i64); 7] = [
            (
                format!(
                    "insert into things (id, title, size) values ({}, 'ordinary-{actor}-1', 1), ({}, 'ordinary-{actor}-2', 2)",
                    base + 1,
                    base + 2
                ),
                2,
            ),
            (
                format!(
                    "update things set size = size + 10 where id in ({}, {})",
                    base + 1,
                    base + 2
                ),
                2,
            ),
            (format!("delete from things where id = {}", base + 2), 1),
            (
                format!(
                    "insert into counters (name, n) values ('ordinary-{actor}', 1) on conflict (name) do update set n = counters.n + 1"
                ),
                1,
            ),
            (
                format!(
                    "insert into counters (name, n) values ('ordinary-{actor}', 1) on conflict (name) do update set n = counters.n + 1"
                ),
                1,
            ),
            (
                format!("insert into keyless (a, b) values ('ordinary-{actor}', {base})"),
                1,
            ),
            (
                format!(
                    "insert into inventory.stock (sku, warehouse, qty) values ('ordinary-{actor}', 'north', 1)"
                ),
                1,
            ),
        ];
        for (sql, changes) in calls {
            let (request, _) = execute_ok(&own, &user, actor, &sql);
            expected.push((request, changes));
        }
    }
    let total: i64 = expected.iter().map(|(_, n)| n).sum();
    assert_exactly(&db, "true", total, "the ordinary writes of both actors");
    let mut wrong = Vec::new();
    for (request, want) in &expected {
        let got = count_now(&db, &format!("h.request = {}", literal(request)));
        if got != *want {
            wrong.push(format!("  request {request}: {got} rows, expected {want}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected each request's changes in `change_history` exactly once:\n{}",
        wrong.join("\n")
    );
    assert_no_duplicates(&db, "the ordinary writes");
    // A later shipment (forced by a read of the history) adds nothing.
    ship(&own, &alice(), APP, "public", "things");
    wait_shipped(&db);
    assert_eq!(
        count_now(&db, "true"),
        total,
        "expected a later shipment to add nothing to what was shipped"
    );
}

#[test]
fn committed_transactions_are_shipped_once_and_rolled_back_aborted_or_left_open_ones_never() {
    let own = Own::start();
    let db = install_transactions(&own, "txboard");
    let (_, answers) = run_steps(
        &own,
        &alice(),
        "txboard",
        &[
            begin(Target::Own("user"), "write", "a"),
            execute_in(
                "a",
                "insert into things (id, title) values (3001, 'tx-committed-1')",
            ),
            execute_in("a", "update things set size = 5 where id = 3001"),
            commit("a"),
            begin(Target::Own("application"), "write", "b"),
            execute_in(
                "b",
                "insert into things (id, title) values (3002, 'tx-rolled-back')",
            ),
            rollback("b"),
            begin(Target::Own("user"), "write", "c"),
            execute_in(
                "c",
                "insert into things (id, title) values (3003, 'tx-aborted')",
            ),
            execute_in("c", "select 1 / 0"),
            commit("c"),
            begin(Target::Own("application"), "write", "d"),
            execute_in(
                "d",
                "insert into things (id, title) values (3004, 'tx-left-open')",
            ),
        ],
    );
    assert_eq!(
        answers[3],
        json!({ "ok": {} }),
        "the harness's own check: expected the first transaction committed"
    );
    assert_eq!(
        answers[6],
        json!({ "ok": {} }),
        "the harness's own check: expected the second transaction rolled back"
    );
    let (_, answers) = run_steps(
        &own,
        &bob(),
        "txboard",
        &[
            begin(Target::Own("application"), "write", "e"),
            execute_in(
                "e",
                "insert into things (id, title) values (3005, 'tx-committed-2')",
            ),
            commit("e"),
        ],
    );
    assert_eq!(
        answers[2],
        json!({ "ok": {} }),
        "the harness's own check: expected the third transaction committed"
    );
    // A read of the history ships whatever is left.
    let (_, answers) = run_steps(
        &own,
        &alice(),
        "txboard",
        &[step(
            "history",
            json!({ "own": true, "as": "user", "table": "things", "limit": 1 }),
        )],
    );
    assert!(
        answers[0]["ok"].is_object(),
        "expected a read of the history answered, got {}",
        answers[0]
    );
    assert_exactly(
        &db,
        &inserted_title("tx-committed-1"),
        1,
        "the insert of a committed transaction",
    );
    assert_exactly(
        &db,
        "h.kind = 'update' AND h.key = '{\"id\": 3001}'::jsonb",
        1,
        "the update of a committed transaction",
    );
    assert_exactly(
        &db,
        &inserted_title("tx-committed-2"),
        1,
        "a transaction committed as the Application",
    );
    for gone in ["tx-rolled-back", "tx-aborted", "tx-left-open"] {
        assert_eq!(
            count_now(&db, &titled_like(gone)),
            0,
            "expected no row of `change_history` for {gone:?}, a change that was rolled back (CONTRACT.md: \"a change that was rolled back is never there\")"
        );
    }
    assert_eq!(
        count_now(&db, "true"),
        3,
        "expected the three changes of the committed transactions and nothing else"
    );
    assert_no_duplicates(&db, "the transactions");
}

#[test]
fn a_cascade_is_shipped_exactly_once() {
    let (own, db) = installed();
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into parents (id, name) values (2, 'second parent')",
    );
    execute_ok(
        &own,
        &alice(),
        "application",
        "insert into children (id, parent_id, name) values (20, 2, 'child of 2')",
    );
    // Parent 1 and its child 10 were put in by the first migration.
    let (one, _) = execute_ok(&own, &alice(), "user", "delete from parents where id = 1");
    let (two, _) = execute_ok(
        &own,
        &bob(),
        "application",
        "delete from parents where id = 2",
    );
    assert_exactly(&db, "true", 6, "two inserts and two cascading deletes");
    for (request, table, id) in [
        (&one, "parents", 1),
        (&one, "children", 10),
        (&two, "parents", 2),
        (&two, "children", 20),
    ] {
        assert_eq!(
            count_now(
                &db,
                &format!(
                    "h.kind = 'delete' AND h.table_name = '{table}' AND h.key = '{{\"id\": {id}}}'::jsonb AND h.request = {}",
                    literal(request)
                )
            ),
            1,
            "expected the delete of {table} {id} in `change_history` exactly once"
        );
    }
    assert_no_duplicates(&db, "the cascades");
}

#[test]
fn a_migrations_changes_are_shipped_once_after_its_version_is_answered() {
    let (own, db) = installed();
    for i in 1..=3 {
        execute_ok(
            &own,
            &alice(),
            "user",
            &format!(
                "insert into things (id, title, size) values ({}, 'mig-{i}', 1)",
                4000 + i
            ),
        );
    }
    assert_exactly(
        &db,
        &titled_like("mig-"),
        3,
        "the inserts before the migration",
    );
    let touch = Migration::new(
        "0003_touch_things",
        &[r#"UPDATE "things" SET "size" = 42 WHERE "title" LIKE 'mig-%';"#],
    );
    let mut list = history_standard();
    list.push(touch);
    own.send_ok(APP, &list);
    let condition = format!(
        "h.kind = 'update' AND h.actor = 'migration' AND h.migration = '0003_touch_things' AND h.user_id = {} AND h.request IS NULL",
        literal(INSTALLER)
    );
    // No read of the history and no write: the Version's answer is what
    // ships them.
    assert_exactly(
        &db,
        &condition,
        3,
        "the rows a migration changed, after its Version was answered",
    );
    for i in 1..=3 {
        assert_eq!(
            count_now(
                &db,
                &format!("{condition} AND h.key = '{{\"id\": {}}}'::jsonb", 4000 + i)
            ),
            1,
            "expected the migration's update of row {} once",
            4000 + i
        );
    }
    assert_no_duplicates(&db, "a migration's changes");
}

#[test]
fn a_shipment_has_no_operation_log_entry() {
    let (own, db) = installed();
    let before = own
        .log()
        .iter()
        .filter_map(|e| e["id"].as_i64())
        .max()
        .unwrap_or(0);
    let mut requests = HashSet::new();
    for (i, actor) in ["user", "application", "user"].iter().enumerate() {
        let (request, _) = execute_ok(
            &own,
            &alice(),
            actor,
            &format!("insert into things (title) values ('logged-{i}')"),
        );
        requests.insert(request);
    }
    assert_exactly(&db, &titled_like("logged-"), 3, "the writes");
    let (response, answer) = own.one(
        &alice(),
        APP,
        &history_call("user", "public", "things", &json!({})),
    );
    assert!(
        answer["ok"].is_object(),
        "expected a read of the history answered, got {answer}"
    );
    requests.insert(response.request_id());
    wait_shipped(&db);
    let after: Vec<Value> = own
        .log()
        .into_iter()
        .filter(|e| e["id"].as_i64().is_some_and(|id| id > before))
        .collect();
    let strangers: Vec<&Value> = after
        .iter()
        .filter(|e| {
            !e["request"].as_str().is_some_and(|r| requests.contains(r))
                || !matches!(e["operation"].as_str(), Some("execute" | "history"))
        })
        .collect();
    assert!(
        strangers.is_empty() && after.len() == 4,
        "expected the Operation log to have one entry per operation (three `execute`, one `history`) and none for the shipments (CONTRACT.md: \"A shipment has no Operation log entry\"); the entries since the writes:\n{}",
        after
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn a_write_is_answered_while_change_history_is_locked_and_its_change_is_shipped_once_after() {
    let (own, db) = installed();
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (title) values ('before-the-lock')",
    );
    assert_exactly(&db, &inserted_title("before-the-lock"), 1, "a first write");
    let marker = "held-up-marker-7f3a9c51";
    let lock = LockedHistory::lock(&db);
    let sent = Instant::now();
    execute_ok(
        &own,
        &alice(),
        "user",
        &format!(
            "insert into things (title, tags) values ('{marker}', '{{\"note\": \"{marker}\"}}')"
        ),
    );
    let answered_in = sent.elapsed();
    assert!(
        answered_in < Duration::from_secs(5),
        "expected the write answered without waiting for its shipment, held up by the lock on `change_history` (CONTRACT.md: \"the write that caused it was answered all the same\"); it took {answered_in:?}"
    );
    let record = wait_for_record_within(&own.vm, Duration::from_secs(25), |r| {
        r["event"] == "history-not-shipped"
    });
    let Some(record) = record else {
        lock.release();
        own.vm.fail(
            "expected a `history-not-shipped` record in the program's log within 25 s of a write while `change_history` is locked (CONTRACT.md: \"The program's log has a `history-not-shipped` record with `application`\")",
        );
    };
    assert_eq!(
        record["application"], APP,
        "expected the `history-not-shipped` record to name the Application, got {record}"
    );
    let held = outbox_count(&db);
    assert_eq!(
        held, 1,
        "expected the change held up by the lock to stay in the outbox (CONTRACT.md: \"the changes stay in the outbox\"): vm.outbox has {held} row(s)"
    );
    let leaked = records_holding(&own.vm, marker);
    assert!(
        leaked.is_empty(),
        "expected nothing of any row in the program's log (CONTRACT.md: \"nothing of any row\"), but these lines hold a value of the row written:\n{}",
        leaked.join("\n")
    );
    lock.release();
    // The next shipment takes it: a read of the history ships first.
    ship(&own, &alice(), APP, "public", "things");
    assert_eq!(
        count_now(&db, &inserted_title(marker)),
        1,
        "expected the change held up by the lock in `change_history` once, before a read of the history that began after the lock was released answered"
    );
    wait_shipped(&db);
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (title) values ('after-the-lock')",
    );
    assert_exactly(
        &db,
        &inserted_title("after-the-lock"),
        1,
        "a write after the lock",
    );
    assert_eq!(
        count_now(&db, &inserted_title(marker)),
        1,
        "expected the change held up by the lock still there once"
    );
    assert_no_duplicates(&db, "the changes held up by a lock");
}

#[test]
fn a_change_not_yet_shipped_when_the_program_is_killed_is_shipped_once_at_the_next_start() {
    let (own, db) = installed();
    let marker = "killed-before-shipping";
    let lock = LockedHistory::lock(&db);
    execute_ok(
        &own,
        &alice(),
        "user",
        &format!("insert into things (title) values ('{marker}')"),
    );
    // The shipment after the write is waiting on the lock (half a second
    // for it to have begun; the test holds without it too).
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        outbox_count(&db),
        1,
        "the harness's own check: expected the change, not shipped, in vm.outbox when the program is killed"
    );
    let stopped = stop(own);
    lock.release();
    let own = stopped.start();
    // No request: the shipment at start ships it.
    let took = wait_shipped_within(&db, SHIPPED_WITHIN);
    assert_eq!(
        count_now(&db, &inserted_title(marker)),
        1,
        "expected the change committed before the program was killed in `change_history` once, vm.outbox empty {took:?} after the next start with no request (CONTRACT.md: shipping runs \"for every own database when the program starts\")"
    );
    ship(&own, &alice(), APP, "public", "things");
    assert_eq!(
        count_now(&db, &inserted_title(marker)),
        1,
        "expected the change there exactly once"
    );
    assert_no_duplicates(&db, "a change shipped after a kill");
}

#[test]
fn a_shipment_killed_in_its_middle_is_finished_once_at_the_next_start() {
    const ROWS: i64 = 30_000;
    let (own, db) = installed();
    let base = max_id_now(&db);
    let before = count_now(&db, "true");
    // Holds the batch that would hold the change in the middle.
    let blocker = Blocker::hold(&db, base + ROWS / 2).unwrap_or_else(|e| {
        panic!("the harness's own check: cannot write a row of `change_history` to hold a shipment in its middle: {e}")
    });
    execute_long(
        &own,
        APP,
        &format!("insert into things (title) select 'mid-' || g from generate_series(1, {ROWS}) g"),
    );
    // Waits for the shipment to have shipped part and to stand still.
    let sql = platform_sql(&db);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = -1;
    let mut since = Instant::now();
    let shipped = loop {
        let n = blackbox::history_shipping::count_with(&sql, APP, "true") - before;
        if n != last {
            last = n;
            since = Instant::now();
        } else if n > 0 && since.elapsed() >= Duration::from_millis(1500) {
            break n;
        }
        if Instant::now() >= deadline {
            break n;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stopped = stop(own);
    blocker.release();
    assert!(
        shipped > 0 && shipped < ROWS,
        "the harness's own check: expected the shipment of {ROWS} changes held in its middle by a row with id {} (some shipped, not all), but {shipped} were shipped when the program was killed",
        base + ROWS / 2
    );
    let own = stopped.start();
    // 60 s, longer than the 10 s of "How soon", for so many.
    let took = wait_shipped_within(&db, Duration::from_secs(60));
    println!(
        "the rest of a shipment killed in its middle was shipped {took:?} after the next start"
    );
    ship(&own, &alice(), APP, "public", "things");
    let wrong = titles_not_once(&db, "mid-", ROWS);
    assert!(
        wrong.is_empty(),
        "expected each of the {ROWS} changes exactly once after a shipment killed in its middle; these are not (at most 20 shown):\n{}",
        wrong.join("\n")
    );
    assert_eq!(
        count_now(&db, "h.request = 'harness-blocker'"),
        0,
        "the harness's own check: its blocking row was rolled back"
    );
    assert_no_duplicates(&db, "a shipment killed in its middle");
}

/// The threads and requests per thread of the tests of shipments at once.
const THREADS: usize = 8;
const PER_THREAD: usize = 25;

/// From `THREADS` threads at once, `PER_THREAD` requests each, in the
/// Application `applications[thread % len]`: each writes a row of `things`
/// with its own id and title `<application>-<thread>-<i>` and reads that
/// row's history in the same request. Returns what went wrong.
fn writes_and_reads_at_once(own: &Own, applications: &[&str]) -> Vec<String> {
    let start = std::sync::Barrier::new(THREADS);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|thread| {
                let start = &start;
                scope.spawn(move || {
                    let application = applications[thread % applications.len()];
                    let slot = thread / applications.len();
                    let mut wrong = Vec::new();
                    start.wait();
                    for i in 0..PER_THREAD {
                        let id = 10_000 + slot * 100 + i;
                        let title = format!("{application}-{thread}-{i}");
                        let (_, answers) = own.run_body(
                            &User::new(&format!("writer-{thread}")),
                            application,
                            &json!({ "calls": [
                                own_execute(
                                    if i % 2 == 0 { "user" } else { "application" },
                                    &format!("insert into things (id, title) values ({id}, '{title}')"),
                                ),
                                history_call("user", "public", "things", &json!({ "key": { "id": id } })),
                            ] }),
                            Duration::from_secs(30),
                        );
                        if !answers[0]["ok"].is_object() {
                            wrong.push(format!("the write of {title}: {}", answers[0]));
                            continue;
                        }
                        let read = &answers[1];
                        if !read["ok"].is_object() {
                            wrong.push(format!("the read after {title}: {read}"));
                            continue;
                        }
                        let title_at = column_index(read, "title");
                        let changes = changes_of(read);
                        let fine = changes.len() == 1
                            && changes[0]["kind"] == "insert"
                            && changes[0]["application"] == application
                            && changes[0]["new"][title_at] == title.as_str();
                        if !fine {
                            wrong.push(format!(
                                "the read of row {id} after writing {title} in the same request, expected that one insert: {read}"
                            ));
                        }
                    }
                    wrong
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| {
                h.join()
                    .unwrap_or_else(|_| vec!["a request thread failed".to_owned()])
            })
            .collect()
    })
}

/// Fails unless each title `<application>-<thread>-<i>` of the threads
/// that wrote to `db`'s Application is in its `change_history` exactly
/// once, none is there of another Application, and the ids of each
/// thread's changes increase in the order it made them.
fn assert_shipped_at_once(db: &OwnDatabase, threads: &[usize], context: &str) {
    let want = i64::try_from(threads.len() * PER_THREAD).unwrap();
    assert_exactly(db, "true", want, context);
    let prefix = format!("{}-", db.application);
    assert_eq!(
        count_now(db, &format!("NOT ({})", titled_like(&prefix))),
        0,
        "{context}: expected no change of another Application under {:?} (CONTRACT.md: one row \"per Application and change\")",
        db.application
    );
    let rows = history_where(db, "true");
    let ids: HashMap<String, i64> = rows
        .iter()
        .map(|r| {
            (
                r["new"]["title"].as_str().unwrap_or_default().to_owned(),
                r["id"].as_i64().unwrap_or(-1),
            )
        })
        .collect();
    let mut wrong = Vec::new();
    for &thread in threads {
        let mut last = i64::MIN;
        for i in 0..PER_THREAD {
            let title = format!("{prefix}{thread}-{i}");
            let found = rows
                .iter()
                .filter(|r| r["new"]["title"] == title.as_str())
                .count();
            if found != 1 {
                wrong.push(format!("  {title}: {found} times"));
                continue;
            }
            let id = ids[&title];
            if id <= last {
                wrong.push(format!(
                    "  {title}: id {id}, not greater than the id {last} of the thread's change before"
                ));
            }
            last = id;
        }
    }
    assert!(
        wrong.is_empty(),
        "{context}: expected each change once, with ids increasing in the order each thread made them:\n{}",
        wrong.join("\n")
    );
    assert_no_duplicates(db, context);
}

#[test]
fn writes_and_reads_at_once_in_one_application_ship_each_change_once() {
    let (own, db) = installed();
    let wrong = writes_and_reads_at_once(&own, &[APP]);
    assert!(
        wrong.is_empty(),
        "expected every write and every read of the history answered, each read holding the change written before it in the same request:\n{}",
        wrong.join("\n")
    );
    let threads: Vec<usize> = (0..THREADS).collect();
    assert_shipped_at_once(&db, &threads, "writes and reads at once in one Application");
}

#[test]
fn writes_and_reads_at_once_in_two_applications_ship_each_change_once_under_its_own() {
    let own = Own::start();
    let first = own.install(APP, &history_standard());
    let second = own.install("board-two", &history_standard());
    let wrong = writes_and_reads_at_once(&own, &[APP, "board-two"]);
    assert!(
        wrong.is_empty(),
        "expected every write and every read of the history answered, each read holding only its own Application's change to that key:\n{}",
        wrong.join("\n")
    );
    let even: Vec<usize> = (0..THREADS).filter(|t| t % 2 == 0).collect();
    let odd: Vec<usize> = (0..THREADS).filter(|t| t % 2 == 1).collect();
    assert_shipped_at_once(&first, &even, "the first of two Applications at once");
    assert_shipped_at_once(&second, &odd, "the second of two Applications at once");
}

/// How far past 4 MiB the bytes of `old` and `new` of a batch, measured as
/// their text (`jsonb` printed), may be: CONTRACT.md says "4 MiB of `old`
/// and `new` as text", and a text of the same values made otherwise (no
/// space after each `:` and `,`) is a little shorter.
const BYTES_SLACK: i64 = 64 * 1024;

#[test]
fn batches_hold_at_most_1000_changes_and_4_mib_past_their_first() {
    let (own, db) = installed();
    execute_long(
        &own,
        APP,
        "insert into things (title) select 'small-' || g from generate_series(1, 2500) g",
    );
    execute_long(
        &own,
        APP,
        "insert into things (title) select 'big-' || g || '-' || repeat('x', 10000) from generate_series(1, 3000) g",
    );
    // 60 s, longer than the 10 s of "How soon", for 30 MB.
    wait_shipped_within(&db, Duration::from_secs(60));
    let n = count_now(&db, "true");
    assert_eq!(
        n, 5500,
        "expected the 5,500 changes of two statements in `change_history` once vm.outbox is empty, got {n}"
    );
    let found = batches(&db, "true");
    let too_many: Vec<String> = found
        .iter()
        .filter(|b| b.rows > BATCH_ROWS)
        .map(|b| format!("  xmin {}: {} changes", b.xmin, b.rows))
        .collect();
    assert!(
        too_many.is_empty(),
        "expected each batch (the rows one transaction wrote: CONTRACT.md, \"each batch is one transaction in the platform database\") to hold at most {BATCH_ROWS} changes:\n{}",
        too_many.join("\n")
    );
    let too_big: Vec<String> = found
        .iter()
        .filter(|b| b.past_first() > BATCH_BYTES + BYTES_SLACK)
        .map(|b| {
            format!(
                "  xmin {}: {} changes, {} bytes past the first",
                b.xmin,
                b.rows,
                b.past_first()
            )
        })
        .collect();
    assert!(
        too_big.is_empty(),
        "expected each batch to hold at most 4 MiB of `old` and `new` past its first change:\n{}",
        too_big.join("\n")
    );
    let big = batches(&db, &titled_like("big-"));
    assert!(
        big.len() >= 8,
        "expected the 3,000 changes of 10 KB (30 MB) in at least 8 batches of at most 4 MiB, got {}",
        big.len()
    );
    assert_no_duplicates(&db, "the batches");
}

#[test]
fn a_statement_of_100000_rows_is_shipped_whole_and_the_time_is_written_down() {
    const ROWS: i64 = 100_000;
    let (own, db) = installed();
    let sent = Instant::now();
    let answered = execute_long(
        &own,
        APP,
        &format!(
            "insert into things (title) select 'hundred-k-' || g from generate_series(1, {ROWS}) g"
        ),
    );
    let statement = answered.duration_since(sent);
    // A generous wait: the contract's 10 s is for a change, and this is a
    // hundred batches or more. Shipped whole: vm.outbox empty.
    wait_shipped_within(&db, Duration::from_secs(300));
    let shipped_in = answered.elapsed();
    let condition = titled_like("hundred-k-");
    let n = count_now(&db, &condition);
    assert_eq!(
        n, ROWS,
        "expected all {ROWS} changes of one statement in `change_history` once vm.outbox is empty ({shipped_in:?} after its answer); there are {n}"
    );
    let wrong = titles_not_once(&db, "hundred-k-", ROWS);
    assert!(
        wrong.is_empty(),
        "expected each of the {ROWS} changes exactly once; these are not (at most 20 shown):\n{}",
        wrong.join("\n")
    );
    let found = batches(&db, &condition);
    let largest = found.iter().map(|b| b.rows).max().unwrap_or(0);
    let version = postgres_version(&db);
    let report = format!(
        "ticket #62: one `execute` inserting {ROWS} rows into `things`, shipped to `change_history`\n\
         postgres: {version}\n\
         rows: {ROWS}\n\
         statement answered after: {:.3} s\n\
         shipped whole after the answer: {:.3} s\n\
         batches (rows by xmin): {} (largest {largest} rows)\n",
        statement.as_secs_f64(),
        shipped_in.as_secs_f64(),
        found.len()
    );
    println!("{report}");
    let path = report_path();
    std::fs::write(&path, &report)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    assert!(
        largest <= BATCH_ROWS,
        "expected batches of at most {BATCH_ROWS} changes, the largest has {largest}"
    );
    assert_no_duplicates(&db, "a statement of 100,000 rows");
}

#[test]
fn a_change_is_in_change_history_within_10_seconds_and_before_a_later_read_answers() {
    let (own, db) = installed();
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (id, title) values (5001, 'soon-1')",
    );
    let (n, took) = wait_count(&db, &inserted_title("soon-1"), 1, SHIPPED_WITHIN);
    println!("a change was in change_history {took:?} after its write's answer");
    assert_eq!(
        n, 1,
        "expected the change in `change_history` within {SHIPPED_WITHIN:?} of its write's answer (CONTRACT.md, \"How soon\"); after {took:?} there are {n}"
    );
    // The next request reads it.
    execute_ok(
        &own,
        &alice(),
        "application",
        "insert into things (id, title) values (5002, 'soon-2')",
    );
    let (_, read) = own.one(
        &bob(),
        APP,
        &history_call(
            "user",
            "public",
            "things",
            &json!({ "key": { "id": 5002 } }),
        ),
    );
    let changes = changes_of(&read);
    assert!(
        changes.len() == 1
            && changes[0]["kind"] == "insert"
            && changes[0]["new"][column_index(&read, "title")] == "soon-2",
        "expected a read of the history in the next request to hold the change written in the one before, got {read}"
    );
    // The same request reads it.
    let (_, answers) = own.run(
        &alice(),
        APP,
        &[
            own_execute(
                "user",
                "insert into things (id, title) values (5003, 'soon-3')",
            ),
            own_execute("user", "update things set size = 3 where id = 5003"),
            history_call(
                "application",
                "public",
                "things",
                &json!({ "key": { "id": 5003 } }),
            ),
        ],
    );
    assert_ok(&answers[0], "the harness's own check: the insert");
    assert_ok(&answers[1], "the harness's own check: the update");
    // The read shipped first, and everything committed is of `things`: the
    // outbox is empty as soon as the read is answered, with no wait.
    let left = outbox_count(&db);
    assert_eq!(
        left, 0,
        "expected vm.outbox empty once a read of the history that ships first has answered (CONTRACT.md: \"It ships first\"); {left} row(s) left"
    );
    let changes = changes_of(&answers[2]);
    let kinds: Vec<&str> = changes
        .iter()
        .map(|c| c["kind"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        kinds,
        ["update", "insert"],
        "expected a read of the history after a write in the same request to hold its changes, newest first, got {}",
        answers[2]
    );
    assert_no_duplicates(&db, "changes read soon after");
}

// ---------------------------------------------------------------------------
// Nobody waits for what is not theirs.
// ---------------------------------------------------------------------------

/// Rows of each statement of the writer that does not pause.
const FLOOD_ROWS: usize = 20_000;

/// How long the writer writes.
const FLOOD_FOR: Duration = Duration::from_secs(6);

/// The readers, each starting 0.4 s after the one before.
const READERS: u32 = 5;

/// What a read's backlog must need, at the measured rate, for it to be
/// excused when it is ended at the `time` limit: the relay's rate while
/// the read runs may fall below the rate measured over the whole flood, so
/// a read needing much more than half its 10 seconds may fairly reach
/// them.
const EXCUSED_FROM_SECONDS: f64 = 6.0;

/// One reader's read, as the test saw it.
struct Read {
    began: Duration,
    /// The rows of `vm.outbox` (everything committed and not yet shipped)
    /// just before the read began.
    backlog: i64,
    took: Duration,
    answer: Value,
}

/// CONTRACT.md, "Nobody waits for what is not theirs": "A read of the
/// history waits only for the changes committed before it began: writes
/// made after that, however many and however fast, do not hold it up."
///
/// One thread writes statements of 20,000 rows into `things` without pause
/// for 6 seconds (it writes faster than the relay ships, so the outbox is
/// never empty meanwhile). Meanwhile five readers, 0.4 s apart, each count
/// `vm.outbox` (what was committed before their read and is not shipped:
/// what the read must ship first), then write a row of their own and read
/// that row's history in one request. The relay's rate is measured over the
/// same seconds (changes added to `change_history` while the outbox was
/// never empty). A read must answer its own row; it may be ended at the
/// `time` limit only when what was before it would take the relay more
/// than 6 seconds to ship at that rate. A read that waited for what was
/// committed after it began would be ended at 10 seconds with little before
/// it.
#[test]
fn reads_of_the_history_do_not_wait_for_writes_made_after_they_began() {
    let (own, db) = installed();
    let stop_writing = std::sync::atomic::AtomicBool::new(false);
    let shipped_at_start = count_now(&db, "true");
    let start = Instant::now();
    let (statements, shipped_in_flood, flood_took, mut reads) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let mut n = 0_usize;
            while start.elapsed() < FLOOD_FOR
                && !stop_writing.load(std::sync::atomic::Ordering::Relaxed)
            {
                let (_, answers) = own.run_body(
                    &bob(),
                    APP,
                    &json!({ "calls": [own_execute(
                        "application",
                        &format!("insert into things (title) select 'flood-{n}-' || g from generate_series(1, {FLOOD_ROWS}) g"),
                    )] }),
                    LONG_REQUEST,
                );
                assert_ok(&answers[0], "the harness's own check: a statement of the writer");
                n += 1;
            }
            let shipped = count_now(&db, "true") - shipped_at_start;
            (n, shipped, start.elapsed())
        });
        let readers: Vec<_> = (0..READERS)
            .map(|k| {
                let (own, db, start) = (&own, &db, start);
                scope.spawn(move || {
                    let at = Duration::from_millis(300) + Duration::from_millis(400) * k;
                    if let Some(wait) = at.checked_sub(start.elapsed()) {
                        std::thread::sleep(wait);
                    }
                    let id = 900_000 + i64::from(k);
                    let backlog = outbox_count(db);
                    let began = start.elapsed();
                    let clock = Instant::now();
                    let (_, answers) = own.run_body(
                        &alice(),
                        APP,
                        &json!({ "calls": [
                            own_execute("user", &format!("insert into things (id, title) values ({id}, 'reader-{k}')")),
                            history_call("user", "public", "things", &json!({ "key": { "id": id } })),
                        ] }),
                        Duration::from_secs(30),
                    );
                    Read {
                        began,
                        backlog,
                        took: clock.elapsed(),
                        answer: answers[1].clone(),
                    }
                })
            })
            .collect();
        let reads: Vec<Read> = readers.into_iter().map(|r| r.join().unwrap()).collect();
        stop_writing.store(true, std::sync::atomic::Ordering::Relaxed);
        let (n, shipped, took) = writer.join().unwrap();
        (n, shipped, took, reads)
    });
    reads.sort_by_key(|r| r.began);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a rate for a message and a bound, not an exact count"
    )]
    let rate = shipped_in_flood as f64 / flood_took.as_secs_f64();
    println!(
        "the writer wrote {statements} statements of {FLOOD_ROWS} rows in {flood_took:?}; the relay shipped {shipped_in_flood} changes meanwhile ({rate:.0} a second)"
    );
    let mut wrong = Vec::new();
    for read in &reads {
        #[expect(clippy::cast_precision_loss, reason = "an estimate in seconds")]
        let needs = read.backlog as f64 / rate.max(1.0);
        let holds = read.answer["ok"]["changes"]
            .as_array()
            .is_some_and(|c| c.len() == 1 && c[0]["kind"] == "insert");
        let excused = read.answer["error"]["limit"] == "time" && needs > EXCUSED_FROM_SECONDS;
        println!(
            "  read begun at {:.3} s with {} changes before it (about {needs:.1} s of shipping): answered in {:.3} s: {}",
            read.began.as_secs_f64(),
            read.backlog,
            read.took.as_secs_f64(),
            if holds {
                "its own row"
            } else if excused {
                "ended at the time limit, excused"
            } else {
                "WRONG"
            }
        );
        if !holds && !excused {
            wrong.push(format!(
                "  the read begun at {:?}, with {} changes before it (about {needs:.1} s of shipping at {rate:.0} a second), answered after {:?}: {}",
                read.began, read.backlog, read.took, read.answer
            ));
        }
    }
    assert!(
        statements >= 2
            && reads.iter().any(|r| r.answer["ok"].is_object() || {
                #[expect(clippy::cast_precision_loss, reason = "an estimate in seconds")]
                let needs = r.backlog as f64 / rate.max(1.0);
                needs < EXCUSED_FROM_SECONDS
            }),
        "the harness's own check: the writer wrote while the reads were made, and some read had little before it"
    );
    assert!(
        wrong.is_empty(),
        "expected each read made while one writer keeps writing to answer what was committed before it began (CONTRACT.md: \"writes made after that, however many and however fast, do not hold it up\"):\n{}",
        wrong.join("\n")
    );
    wait_shipped_within(&db, Duration::from_secs(300));
}

/// Rows each of the five Applications writes in one statement.
const LARGE_OUTBOX: usize = 150_000;

/// CONTRACT.md, "Nobody waits for what is not theirs": "One Application's
/// shipments do not hold up another's: the relay takes turns of one batch
/// each, so while several Applications ship large outboxes, a write of
/// another Application is shipped and a read of its history answered as
/// soon as their own changes are." Five Applications each write 150,000
/// rows at once (750,000 changes, some 20 seconds of shipping at the rate
/// the 100,000-row test measured); as soon as they are answered, a sixth
/// writes one row, and its change must be in `change_history`, and a read
/// of its history answered, within 3 seconds (well inside the read's 10),
/// while the five still have changes in their outboxes. A relay that
/// shipped one outbox after another would make the sixth wait for them.
#[test]
fn one_applications_large_shipments_do_not_hold_up_another_applications() {
    let own = Own::start();
    let busy: Vec<String> = (1..=5).map(|i| format!("busy-{i}")).collect();
    let busy_dbs: Vec<OwnDatabase> = busy
        .iter()
        .map(|name| own.install(name, &history_standard()))
        .collect();
    let quiet = own.install("quiet", &history_standard());
    let began = Instant::now();
    std::thread::scope(|scope| {
        for name in &busy {
            let own = &own;
            scope.spawn(move || {
                execute_long(
                    own,
                    name,
                    &format!("insert into things (title) select 'large-' || g from generate_series(1, {LARGE_OUTBOX}) g"),
                )
            });
        }
    });
    let written = began.elapsed();
    let left_before: i64 = busy_dbs.iter().map(outbox_count).sum();
    // A write, then at once a read of its history.
    let (_, answer) = own.one(
        &alice(),
        "quiet",
        &own_execute(
            "user",
            "insert into things (id, title) values (1, 'quiet one')",
        ),
    );
    assert_ok(
        &answer,
        "the harness's own check: the sixth Application's write",
    );
    let read_began = Instant::now();
    let (_, read) = own.one(
        &alice(),
        "quiet",
        &history_call("user", "public", "things", &json!({ "key": { "id": 1 } })),
    );
    let read_took = read_began.elapsed();
    let left_at_read: i64 = busy_dbs.iter().map(outbox_count).sum();
    // Another write, shipped by the relay alone.
    let (_, answer) = own.one(
        &alice(),
        "quiet",
        &own_execute(
            "user",
            "insert into things (id, title) values (2, 'quiet two')",
        ),
    );
    assert_ok(
        &answer,
        "the harness's own check: the sixth Application's second write",
    );
    let (n, shipped_after) = wait_count(&quiet, &inserted_title("quiet two"), 1, SHIPPED_WITHIN);
    let left_at_ship: i64 = busy_dbs.iter().map(outbox_count).sum();
    let all_shipped = Instant::now();
    for db in &busy_dbs {
        wait_shipped_within(db, Duration::from_secs(300));
    }
    println!(
        "five statements of {LARGE_OUTBOX} rows answered after {written:?}, {left_before} changes then in their outboxes; the sixth Application's read after its write answered in {read_took:?} ({left_at_read} changes of the five left then); its second write in change_history {shipped_after:?} after it ({left_at_ship} left then); the five shipped whole {:?} later",
        all_shipped.elapsed()
    );
    let changes = read["ok"]["changes"].as_array();
    assert!(
        changes.is_some_and(|c| c.len() == 1) && read_took < Duration::from_secs(3),
        "expected the sixth Application's read answered with its change within 3 s, well inside its 10 s, while five others ship large outboxes (CONTRACT.md: \"a read of its history answered as soon as their own changes are\"); it took {read_took:?} and answered {read}"
    );
    assert!(
        left_at_read > 0,
        "the harness's own check: the five still had changes to ship when the sixth's read answered; make LARGE_OUTBOX larger"
    );
    assert!(
        n == 1 && shipped_after < Duration::from_secs(3),
        "expected the sixth Application's write in change_history within 3 s while five others ship large outboxes (CONTRACT.md: \"a write of another Application is shipped ... as soon as their own changes are\"); after {shipped_after:?} it has {n}"
    );
    assert!(
        left_at_ship > 0,
        "the harness's own check: the five still had changes to ship when the sixth's write was shipped; make LARGE_OUTBOX larger"
    );
}

// ---------------------------------------------------------------------------
// Only a read takes a place to wait; at most 32 relay connections.
// ---------------------------------------------------------------------------

/// Rows of the statement that leaves the large outbox of the test below.
const BACKLOG_ROWS: usize = 150_000;

/// How long the small writes are spread over.
const SMALL_WRITES_OVER: Duration = Duration::from_millis(1500);

/// What one run of the scenario below measured.
struct PlaceRun {
    small_writes: usize,
    rate: f64,
    backlog: i64,
    /// What the backlog needs at the rate, in seconds.
    needs: f64,
    took: Duration,
    read: Value,
    flooded: usize,
}

/// The scenario of the test below in `application`, with `small_writes`
/// small write requests spread over [`SMALL_WRITES_OVER`].
fn place_run(own: &Own, application: &str, db: &OwnDatabase, small_writes: usize) -> PlaceRun {
    let shipped_before = count_now(db, "true");
    let answered = execute_long(
        own,
        application,
        &format!(
            "insert into things (title) select 'backlog-' || g from generate_series(1, {BACKLOG_ROWS}) g"
        ),
    );
    let gap = SMALL_WRITES_OVER / u32::try_from(small_writes).unwrap();
    for i in 0..small_writes {
        let (_, answers) = own.run(
            &bob(),
            application,
            &[own_execute(
                "application",
                &format!("insert into counters (name, n) values ('small-{i}', {i})"),
            )],
        );
        assert_ok(&answers[0], "the harness's own check: a small write");
        std::thread::sleep(gap);
    }
    let backlog = outbox_count(db);
    let shipped = count_now(db, "true") - shipped_before;
    #[expect(clippy::cast_precision_loss, reason = "a rate for a bound")]
    let rate = shipped as f64 / answered.elapsed().as_secs_f64();
    let stop_flood = std::sync::atomic::AtomicBool::new(false);
    let (took, read, flooded) = std::thread::scope(|scope| {
        let flood = scope.spawn(|| {
            let begun = Instant::now();
            let mut n = 0_usize;
            while begun.elapsed() < Duration::from_secs(6)
                && !stop_flood.load(std::sync::atomic::Ordering::Relaxed)
            {
                let (_, answers) = own.run_body(
                    &bob(),
                    application,
                    &json!({ "calls": [own_execute(
                        "application",
                        &format!("insert into things (title) select 'flood-{n}-' || g from generate_series(1, 20000) g"),
                    )] }),
                    LONG_REQUEST,
                );
                assert_ok(&answers[0], "the harness's own check: a statement of the flood");
                n += 1;
            }
            n
        });
        let began = Instant::now();
        let (_, answers) = own.run_body(
            &alice(),
            application,
            &json!({ "calls": [
                own_execute("user", "insert into things (id, title) values (990001, 'the reader')"),
                history_call("user", "public", "things", &json!({ "key": { "id": 990001 } })),
            ] }),
            Duration::from_secs(30),
        );
        let took = began.elapsed();
        stop_flood.store(true, std::sync::atomic::Ordering::Relaxed);
        (took, answers[1].clone(), flood.join().unwrap())
    });
    wait_shipped_within(db, Duration::from_secs(300));
    #[expect(clippy::cast_precision_loss, reason = "an estimate in seconds")]
    let needs = backlog as f64 / rate.max(1.0);
    let run = PlaceRun {
        small_writes,
        rate,
        backlog,
        needs,
        took,
        read,
        flooded,
    };
    println!(
        "{application}: a statement of {BACKLOG_ROWS} rows, then {} small writes over {SMALL_WRITES_OVER:?}: the relay shipped {:.0} changes a second; the read began with {} changes before it (about {:.1} s of shipping) and answered in {:?} while {} flood statements of 20,000 rows were written: {}",
        run.small_writes,
        run.rate,
        run.backlog,
        run.needs,
        run.took,
        run.flooded,
        if run.read["ok"].is_object() {
            "ok"
        } else {
            "not ok"
        }
    );
    run
}

/// CONTRACT.md, "Nobody waits for what is not theirs": "Only a read is
/// given a place to wait for a shipment, never a write: writes, however
/// many requests they are and whenever they are made, take no place and
/// move no read's."
///
/// The scenario, run twice, each time in an Application of its own: (a)
/// one statement of 200,000 rows leaves a large outbox; (b) while it ships,
/// small write requests of the same Application, one by one, spread over
/// 4 seconds; (c) then a request that writes a row and reads its history,
/// `vm.outbox` counted just before it (its backlog: what it must ship
/// first), and from that moment a flood of 20,000-row statements, which
/// outruns the relay, for up to 6 seconds. The relay's rate is measured
/// from the statement's answer to the read's start.
///
/// The control makes 8 small writes; the test 120, more than the 64 places
/// a read may wait in. If writes took places, the test's read would find
/// none free and wait for flood changes committed until one came free. The
/// control's read gives the machine's slowness now: how many times what its
/// backlog needs at the measured rate it took (at least 1). The test's read
/// must answer its own row within 1.5 times its own backlog's need times
/// that slowness, plus 1 second; a `time` limit is excused only when its
/// backlog alone needs more than 9 seconds.
#[test]
fn small_writes_take_no_place_to_wait_and_move_no_reads() {
    let own = Own::start();
    let control_db = own.install("control", &history_standard());
    let db = own.install(APP, &history_standard());
    let control = place_run(&own, "control", &control_db, 8);
    let run = place_run(&own, APP, &db, 120);
    assert!(
        control.read["ok"].is_object(),
        "the harness's own check: the control's read answered, got {}",
        control.read
    );
    // How much longer than its backlog's need, at the measured rate, a read
    // took here with few writes: the machine's own slowness now.
    let slowness = (control.took.as_secs_f64() / control.needs.max(0.1)).max(1.0);
    let bound = Duration::from_secs_f64(run.needs * slowness * 1.5 + 1.0);
    println!("the control's read took {slowness:.2} times what its backlog needs");
    println!("the bound on the test's read: {bound:?}");
    let holds = run.read["ok"]["changes"]
        .as_array()
        .is_some_and(|c| c.len() == 1 && c[0]["kind"] == "insert");
    let excused = run.read["error"]["limit"] == "time" && run.needs > 9.0;
    assert!(
        excused || (holds && run.took <= bound),
        "expected the read after {} small writes to answer its own row in about the time its backlog needs ({:.1} s at {:.0} a second), as slow as the control's read after 8 small writes was ({:?}); bound {bound:?}, not that and the flood's (CONTRACT.md: \"writes, however many requests they are and whenever they are made, take no place and move no read's\"); it answered after {:?}: {}",
        run.small_writes,
        run.needs,
        run.rate,
        control.took,
        run.took,
        run.read
    );
}

/// The Applications with large outboxes in the test below: more than the
/// relay's 32 connections.
const CROWD: usize = 36;

/// Rows of each one's outbox.
const CROWD_ROWS: usize = 25_000;

/// CONTRACT.md, "Nobody waits for what is not theirs": "The relay keeps at
/// most 32 connections to own databases at once (their `application_name`
/// is `vm relay`), and an own database keeps one no longer than one batch
/// while another waits for one. So the promise holds also when more than
/// 32 own databases have large outboxes to ship at once: a write of yet
/// another Application is shipped, and a read of its history answered,
/// after a number of batches that does not grow with the size of the
/// others' outboxes."
///
/// 36 Applications each write 25,000 rows while the administrator holds a
/// lock on `change_history` (their shipments wait, fail, and are not tried
/// again until asked); the program is stopped, the lock released, and the
/// program started again on the same databases, which asks for all 36
/// shipments at once (900,000 changes). At once, and at the same time,
/// one more Application writes a row left to the relay alone, and another
/// writes a row and reads its history. Meanwhile the administrator counts, every 20 ms, the
/// sessions named `vm relay` in these own databases: never more than 32.
///
/// The bound, from measurement: the rate at which the 37 outboxes are
/// shipped (all changes over the time until all are empty) gives the time
/// of one batch of 1,000 changes; the read and the write must each be done
/// within the time of 3 batches of each of the 38 Applications,
/// plus 2 seconds (the program's start and connections), while a relay
/// that shipped one outbox after another would make them wait for most of
/// the 900,000.
#[test]
fn more_than_32_own_databases_shipping_at_once_hold_up_no_other_and_use_at_most_32_relay_connections()
 {
    let setup = Instant::now();
    let own = Own::start();
    let names: Vec<String> = (1..=CROWD).map(|i| format!("crowd-{i:02}")).collect();
    let dbs: Vec<OwnDatabase> = std::thread::scope(|scope| {
        let handles: Vec<_> = names
            .chunks(6)
            .map(|chunk| {
                let own = &own;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|name| own.install(name, &history_standard()))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let lone = own.install("lone", &history_standard());
    let reader = own.install("lone-reader", &history_standard());
    let installed = setup.elapsed();
    let lock = LockedHistory::lock(&lone);
    std::thread::scope(|scope| {
        for chunk in names.chunks(6) {
            let own = &own;
            scope.spawn(move || {
                for name in chunk {
                    execute_long(
                        own,
                        name,
                        &format!("insert into things (title) select 'crowd-' || g from generate_series(1, {CROWD_ROWS}) g"),
                    );
                }
            });
        }
    });
    let stopped = stop(own);
    lock.release();
    let waiting: i64 = dbs.iter().map(outbox_count).sum();
    let written = setup.elapsed();
    assert_eq!(
        waiting,
        i64::try_from(CROWD * CROWD_ROWS).unwrap(),
        "the harness's own check: every change of the {CROWD} Applications still in their outboxes before the start"
    );
    let databases: Vec<String> = dbs
        .iter()
        .map(|d| d.name.clone())
        .chain([lone.name.clone(), reader.name])
        .collect();
    let in_list = databases
        .iter()
        .map(|n| literal(n))
        .collect::<Vec<_>>()
        .join(", ");
    let sampling = std::sync::atomic::AtomicBool::new(true);
    let restarted = Instant::now();
    let (own, most_relay, samples, lone_read, read_took, lone_shipped, all_shipped) =
        std::thread::scope(|scope| {
            let sampler = scope.spawn(|| {
                let sql = blackbox::sql::Sql::connect(&blackbox::postgres::shared().url("postgres"))
                    .unwrap();
                let mut most = 0_i64;
                let mut samples = 0_usize;
                while sampling.load(std::sync::atomic::Ordering::Relaxed) {
                    let n: i64 = sql
                        .value(&format!(
                            "SELECT count(*)::text FROM pg_stat_activity WHERE application_name = 'vm relay' AND datname IN ({in_list})"
                        ))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    most = most.max(n);
                    samples += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                (most, samples)
            });
            let own = stopped.start();
            let (shipped_after, (read, read_took)) = std::thread::scope(|inner| {
                // One Application's write left to the relay alone.
                let relay_alone = inner.spawn(|| {
                    let (_, answer) = own.one(
                        &alice(),
                        "lone",
                        &own_execute(
                            "user",
                            "insert into things (id, title) values (1, 'lone one')",
                        ),
                    );
                    assert_ok(
                        &answer,
                        "the harness's own check: the lone Application's write",
                    );
                    let (_, shipped_after) = wait_count(
                        &lone,
                        &inserted_title("lone one"),
                        1,
                        Duration::from_secs(60),
                    );
                    shipped_after
                });
                // Another's write, then at once a read of its history.
                let (_, answer) = own.one(
                    &alice(),
                    "lone-reader",
                    &own_execute(
                        "user",
                        "insert into things (id, title) values (1, 'reader one')",
                    ),
                );
                assert_ok(
                    &answer,
                    "the harness's own check: the reading Application's write",
                );
                let read_began = Instant::now();
                let (_, read) = own.one(
                    &alice(),
                    "lone-reader",
                    &history_call("user", "public", "things", &json!({ "key": { "id": 1 } })),
                );
                let read_took = read_began.elapsed();
                (relay_alone.join().unwrap(), (read, read_took))
            });
            for db in &dbs {
                wait_shipped_within(db, Duration::from_secs(300));
            }
            let all = restarted.elapsed();
            sampling.store(false, std::sync::atomic::Ordering::Relaxed);
            let (most, samples) = sampler.join().unwrap();
            (own, most, samples, read, read_took, shipped_after, all)
        });
    drop(own);
    #[expect(clippy::cast_precision_loss, reason = "a rate for a bound")]
    let rate = waiting as f64 / all_shipped.as_secs_f64();
    let batch = 1_000.0 / rate;
    #[expect(clippy::cast_precision_loss, reason = "a count of batches")]
    let bound = Duration::from_secs_f64(3.0 * (CROWD as f64 + 2.0) * batch + 2.0);
    println!(
        "{CROWD} Applications installed in {installed:?}, their {waiting} changes written by {written:?}; after the start all were shipped in {all_shipped:?} ({rate:.0} a second, a batch of 1,000 every {:.1} ms); the lone Application's write shipped by the relay {lone_shipped:?} after it, the other's read after its write answered in {read_took:?} (bound {bound:?}); at most {most_relay} `vm relay` sessions in {samples} samples",
        batch * 1_000.0
    );
    assert!(
        samples > 10 && most_relay > 0,
        "the harness's own check: the relay's sessions were seen ({samples} samples, at most {most_relay})"
    );
    assert!(
        most_relay <= 32,
        "expected at most 32 sessions named `vm relay` in these own databases at once (CONTRACT.md: \"The relay keeps at most 32 connections to own databases at once\"), saw {most_relay}"
    );
    let holds = lone_read["ok"]["changes"]
        .as_array()
        .is_some_and(|c| c.len() == 1);
    assert!(
        holds && read_took <= bound,
        "expected the reading Application's read answered with its change within {bound:?} (3 batches of each of the 38 Applications at the measured rate, plus 2 s), not after the others' outboxes; it took {read_took:?}: {lone_read}"
    );
    assert!(
        lone_shipped <= bound,
        "expected the lone Application's write shipped by the relay within {bound:?}, not after the others' outboxes; it took {lone_shipped:?}"
    );
}

/// The readers of the test below.
const FULL_READERS: usize = 100;

/// CONTRACT.md, "Nobody waits for what is not theirs": "One own database
/// has 64 such places for reads begun at different moments. While they are
/// all taken, a read that begins waits also for what was committed until a
/// place came free; it is still answered, or ended at its `time` limit, and
/// never answered without what was committed before it began."
///
/// The outbox is sized from the relay's rate, measured first in the same
/// test (a probe of 30,000 rows, timed from its answer until `vm.outbox`
/// is empty): about 3 seconds of shipping, so that the readers, begun 20 ms
/// apart (the 65th after 1.3 s, the 100th after 2 s), all begin while it
/// ships, and the first, which waits for all of it, needs some 3 of its 10
/// seconds even if the machine is three times slower than when measured.
/// While it ships, 100 requests of the same Application begin, each
/// writing a row of its own and reading that row's history. Every one must answer
/// `ok` with exactly its own row's insert, or be ended at the `time`
/// limit; never `ok` without its row, never another error. The harness
/// checks that more than 64 were in flight at once and that one at least
/// answered `ok`; at the end every row written is in `change_history`
/// exactly once.
#[test]
fn reads_begun_while_every_place_is_taken_answer_their_own_row_or_end_at_the_time_limit() {
    let (own, db) = installed();
    // The relay's rate, here and now.
    let probe = execute_long(
        &own,
        APP,
        "insert into things (title) select 'probe-' || g from generate_series(1, 30000) g",
    );
    wait_shipped_within(&db, Duration::from_secs(120));
    let probe_took = probe.elapsed().max(Duration::from_millis(1));
    let rate = 30_000.0 / probe_took.as_secs_f64();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive count of rows, clamped"
    )]
    let rows = ((rate * 3.0) as u64).clamp(30_000, 250_000);
    let answered = execute_long(
        &own,
        APP,
        &format!(
            "insert into things (title) select 'full-' || g from generate_series(1, {rows}) g"
        ),
    );
    let start = Instant::now();
    let results: Vec<(usize, Instant, Instant, Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..FULL_READERS)
            .map(|k| {
                let own = &own;
                scope.spawn(move || {
                    let at = Duration::from_millis(20) * u32::try_from(k).unwrap();
                    if let Some(wait) = at.checked_sub(start.elapsed()) {
                        std::thread::sleep(wait);
                    }
                    let id = 800_000 + k;
                    let began = Instant::now();
                    let (_, answers) = own.run_body(
                        &User::new(&format!("reader-{k}")),
                        APP,
                        &json!({ "calls": [
                            own_execute("user", &format!("insert into things (id, title) values ({id}, 'full-reader-{k}')")),
                            history_call("user", "public", "things", &json!({ "key": { "id": id } })),
                        ] }),
                        Duration::from_secs(40),
                    );
                    assert_ok(&answers[0], "the harness's own check: a reader's write");
                    (k, began, Instant::now(), answers[1].clone())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // The most requests in flight at once, from their begin and end times.
    let mut events: Vec<(Instant, i32)> = results
        .iter()
        .flat_map(|(_, b, e, _)| [(*b, 1), (*e, -1)])
        .collect();
    events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut now = 0;
    let mut most = 0;
    for (_, step) in &events {
        now += step;
        most = most.max(now);
    }
    let mut ok = 0;
    let mut time = 0;
    let mut wrong = Vec::new();
    for (k, _, _, answer) in &results {
        let id = 800_000 + k;
        let changes = answer["ok"]["changes"].as_array();
        if let Some(changes) = changes {
            let own_row = changes.len() == 1
                && changes[0]["kind"] == "insert"
                && changes[0]["key"] == json!({ "id": id })
                && changes[0]["user_id"] == format!("reader-{k}");
            if own_row {
                ok += 1;
            } else {
                wrong.push(format!(
                    "  reader {k}: `ok` without exactly its own row's insert: {answer}"
                ));
            }
        } else if answer["error"]["kind"] == "limit" && answer["error"]["limit"] == "time" {
            time += 1;
        } else {
            wrong.push(format!("  reader {k}: {answer}"));
        }
    }
    println!(
        "full places: the probe of 30,000 rows shipped in {probe_took:?} ({rate:.0} a second); a statement of {rows} rows answered {:?} before the first reader; {FULL_READERS} readers, at most {most} in flight at once; {ok} answered `ok` with their row, {time} ended at the time limit; all done {:?} after the first began",
        start.duration_since(answered),
        start.elapsed()
    );
    assert!(
        wrong.is_empty(),
        "expected every read begun while the places are taken to answer its own row or end at the time limit (CONTRACT.md: \"it is still answered, or ended at its `time` limit, and never answered without what was committed before it began\"):\n{}",
        wrong.join("\n")
    );
    assert!(
        most > 64 && ok >= 1,
        "the harness's own check: more than 64 reads in flight at once (at most {most}) and one at least answered `ok` ({ok})"
    );
    wait_shipped_within(&db, Duration::from_secs(300));
    let mut not_once = Vec::new();
    for k in 0..FULL_READERS {
        let n = count_now(
            &db,
            &format!("h.key = '{{\"id\": {}}}'::jsonb", 800_000 + k),
        );
        if n != 1 {
            not_once.push(format!("  row {}: {n} times", 800_000 + k));
        }
    }
    assert!(
        not_once.is_empty(),
        "expected every reader's row in change_history exactly once:\n{}",
        not_once.join("\n")
    );
}
