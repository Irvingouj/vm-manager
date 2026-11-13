//! What is said and what is kept (CONTRACT.md, "The platform's part of an
//! own database is out of a migration's reach: What a migration leaves
//! behind: What something else changed"): "the entry and what the platform
//! keeps are written together", "a comparison that cannot be made or
//! finished", and "the program stopped between a Version's commit and what
//! is kept".
//!
//! The harness's superuser stands for the faults, in the platform database
//! and in the own database, as it stands for what something else changed:
//!
//! - a trigger on the platform database's `operation_log` that refuses the
//!   `capture-check` entry: the change is not said then, and is said once
//!   at the next occasion (a start, a Version) after the trigger is gone;
//! - an `ACCESS EXCLUSIVE` lock on a table whose capture was left disabled,
//!   held in an open transaction: an operation on another table is not held
//!   up more than 20 s, the program's log has `own-database-not-compared`,
//!   and once the lock is gone the next occasion says the change and puts
//!   the capture back;
//! - the owner role's catalog row held `FOR UPDATE` while a Version's
//!   migration sleeps (research F5), so that the host's last statement
//!   before its commit waits, and the program stopped (`SIGKILL`): nothing
//!   is found changed at the next start, nothing of the Version is kept, it
//!   can be sent again;
//! - a trigger on the platform database's `own_database_states` that sleeps
//!   on the `UPDATE` that settles what is kept (the one that sets `pending`
//!   to null), and the program stopped once the Version's migrations are
//!   committed: nothing is found changed at the next start, and a change
//!   made outside afterwards is found as always;
//! - the same trigger raising instead: the Version is still answered `201`,
//!   the program's log has `own-database-structure-not-kept`, nothing is
//!   found changed afterwards, and a change made outside afterwards is
//!   found as always.
//!
//! What the platform keeps is the contract's to leave open; these tests
//! rely only on what the platform database shows its superuser: a table
//! `operation_log` with a column `operation`, and a table
//! `own_database_states`, one row per own database, with a column `pending`
//! that holds what a Version will make until it is settled.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::changed_outside::{
    assert_capture_check_entry, capture_checks, capture_checks_eventually, capture_shape,
    leave_update_capture_of_things_disabled, platform_superuser, stop, wait_for_session,
};
use blackbox::history::{history_after, last_history_id};
use blackbox::http::Request;
use blackbox::out_of_reach::{WRITER, base, base_and, install_seeded, start};
use blackbox::own_database::{
    FIXTURE, Migration, Own, OwnDatabase, literal, own_execute, own_query,
    version_body_with_migrations,
};
use blackbox::postgres::shared;
use blackbox::scenario::lines;
use blackbox::sql::Sql;
use blackbox::vm::fixture_bundle;
use serde_json::{Value, json};

/// A write through the Application; its answer.
fn write(own: &Own, application: &str, id: i64) -> Value {
    own.one(
        &User::new(WRITER),
        application,
        &own_execute(
            "user",
            &format!("insert into things (id, title) values ({id}, 'a write')"),
        ),
    )
    .1
}

/// A write that must run and be recorded in the history.
fn recorded(own: &Own, db: &OwnDatabase, application: &str, id: i64, context: &str) {
    let last = last_history_id(db);
    let answer = write(own, application, id);
    assert!(
        answer["ok"].is_object(),
        "{context}: expected the write to run, got {answer}"
    );
    let rows = history_after(db, last);
    assert!(
        rows.iter()
            .any(|r| r["table_name"] == "things" && r["new"]["id"] == id),
        "{context}: expected the write recorded, got:\n{}",
        lines(&rows)
    );
}

/// Fails unless the capture is as `shape` was.
fn assert_capture_whole(db: &OwnDatabase, shape: &[String], context: &str) {
    assert_eq!(
        capture_shape(db),
        shape,
        "{context}: expected the capture whole again: every capture trigger there and enabled as before"
    );
}

/// Fails unless the program's log has a record `event` naming
/// `application`.
fn assert_record(own: &Own, event: &str, application: &str) -> Value {
    own.vm.wait_for_record(
        &format!("a `{event}` record with `application` {application:?}"),
        |r| r["event"] == event && r["application"] == application,
    )
}

/// Fails unless there are exactly `count` `capture-check` entries for
/// `application`, waiting up to 5 s for one when `count` is not 0.
fn assert_entries(own: &Own, application: &str, count: usize, context: &str) -> Vec<Value> {
    let entries = if count == 0 {
        capture_checks(own, application)
    } else {
        capture_checks_eventually(own, application)
    };
    assert_eq!(
        entries.len(),
        count,
        "{context}: expected {count} capture-check entr(y/ies) for {application:?}, got:\n{}",
        lines(&entries)
    );
    entries
}

/// The value of a sequence the harness made in the platform database.
fn fired(platform: &Sql, sequence: &str) -> i64 {
    platform
        .value(&format!(
            "SELECT CASE WHEN is_called THEN last_value ELSE 0 END::text FROM public.{sequence}"
        ))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// A trigger on the platform database's `operation_log` that refuses
/// every `capture-check` entry, counting each refusal in
/// `harness_entry_refused`.
const REFUSE_THE_ENTRY: &str = r"
CREATE SEQUENCE public.harness_entry_refused;
GRANT USAGE, SELECT, UPDATE ON SEQUENCE public.harness_entry_refused TO PUBLIC;
CREATE FUNCTION public.harness_refuse_entry() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
  PERFORM nextval('public.harness_entry_refused');
  RAISE EXCEPTION 'the harness refuses this capture-check entry';
END $f$;
CREATE TRIGGER harness_refuse_entry BEFORE INSERT ON public.operation_log
  FOR EACH ROW WHEN (NEW.operation = 'capture-check')
  EXECUTE FUNCTION public.harness_refuse_entry();
";

const ALLOW_THE_ENTRY: &str = "DROP TRIGGER harness_refuse_entry ON public.operation_log";

/// Waits up to 10 s for the platform to have tried to write a refused
/// `capture-check` entry.
fn assert_entry_refused(platform: &Sql, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while fired(platform, "harness_entry_refused") == 0 {
        assert!(
            Instant::now() < deadline,
            "{context}: expected the platform to find the change and try to write its capture-check entry (the harness's trigger on `operation_log` saw none within 10 s)"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The entry cannot be written when the change is found at the first use
/// after a start: it is written at the next start, once.
#[test]
fn a_change_whose_entry_could_not_be_written_at_the_first_use_is_said_once_at_the_next_start() {
    let own = start();
    let application = "unsaid-at-start";
    let db = install_seeded(&own, application);
    let shape = capture_shape(&db);
    let stopped = stop(own);
    leave_update_capture_of_things_disabled(&db);
    let own = stopped.start();
    let platform = platform_superuser(&own);
    platform.batch(REFUSE_THE_ENTRY);
    // The first use: what it is answered is not this test's business.
    let _ = write(&own, application, 7001);
    assert_entry_refused(&platform, "at the first use after the start");
    assert_entries(&own, application, 0, "while the entry is refused");
    platform.batch(ALLOW_THE_ENTRY);
    // The next occasion: a start.
    let own = Own {
        vm: own.vm.restart(),
    };
    let _ = write(&own, application, 7002);
    let entries = assert_entries(
        &own,
        application,
        1,
        "at the first use after the next start (CONTRACT.md: \"when the entry cannot be written, nothing is kept and the change is found and said at the next occasion\")",
    );
    assert_capture_check_entry(&entries[0], application);
    assert_record(&own, "capture-changed-outside", application);
    assert_capture_whole(&db, &shape, "once the change was said");
    recorded(
        &own,
        &db,
        application,
        7003,
        "a write after the change was said",
    );
    // Said once.
    let own = Own {
        vm: own.vm.restart(),
    };
    let _ = write(&own, application, 7004);
    let _ = write(&own, application, 7005);
    assert_entries(&own, application, 1, "after one more start and two writes");
}

/// The entry cannot be written when a Version finds the change: it is
/// written when the next Version is sent, once.
#[test]
fn a_change_whose_entry_could_not_be_written_when_a_version_was_sent_is_said_once_at_the_next_version()
 {
    let own = start();
    let application = "unsaid-at-version";
    let db = install_seeded(&own, application);
    let shape = capture_shape(&db);
    leave_update_capture_of_things_disabled(&db);
    let platform = platform_superuser(&own);
    platform.batch(REFUSE_THE_ENTRY);
    // A Version with none to apply; what it is answered while the entry
    // cannot be written is not this test's business.
    let _ = own.send(application, &base());
    assert_entry_refused(&platform, "when a Version was sent");
    assert_entries(&own, application, 0, "while the entry is refused");
    platform.batch(ALLOW_THE_ENTRY);
    let sent = own.send(application, &base());
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let entries = assert_entries(
        &own,
        application,
        1,
        "when the next Version was sent (CONTRACT.md: \"when the entry cannot be written, nothing is kept and the change is found and said at the next occasion\")",
    );
    assert_capture_check_entry(&entries[0], application);
    assert_record(&own, "capture-changed-outside", application);
    assert_capture_whole(&db, &shape, "once the change was said");
    recorded(
        &own,
        &db,
        application,
        7011,
        "a write after the change was said",
    );
    let sent = own.send(application, &base());
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let own = Own {
        vm: own.vm.restart(),
    };
    let _ = write(&own, application, 7012);
    assert_entries(
        &own,
        application,
        1,
        "after one more Version, a start and a write",
    );
}

/// What the locker thread says.
enum Locker {
    Held,
    Failed(String),
}

/// Takes `lock` (statements after `BEGIN`) on `url` in a thread, holding it
/// until released or 90 s have passed.
fn hold(url: String, lock: String) -> (mpsc::SyncSender<()>, std::thread::JoinHandle<()>) {
    let (held_tx, held_rx) = mpsc::sync_channel::<Locker>(1);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
    let locker = std::thread::spawn(move || {
        let sql = match Sql::connect(&url) {
            Ok(sql) => sql,
            Err(e) => {
                drop(held_tx.send(Locker::Failed(e)));
                return;
            }
        };
        sql.batch("BEGIN");
        match sql.try_batch(&lock) {
            Ok(_) => drop(held_tx.send(Locker::Held)),
            Err(e) => {
                drop(held_tx.send(Locker::Failed(e)));
                return;
            }
        }
        release_rx.recv_timeout(Duration::from_secs(90)).ok();
        sql.batch("ROLLBACK");
    });
    match held_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Locker::Held) => {}
        Ok(Locker::Failed(e)) => panic!("the harness's own lock failed: {e}"),
        Err(e) => panic!("the harness's own lock was not taken within 5 s: {e}"),
    }
    (release_tx, locker)
}

/// A comparison that cannot be finished: a lock is in the way of putting
/// the capture back.
#[test]
fn a_comparison_a_lock_keeps_from_finishing_holds_nothing_up_and_is_made_at_the_next_occasion() {
    let own = start();
    let application = "not-compared";
    let db = install_seeded(&own, application);
    let shape = capture_shape(&db);
    let stopped = stop(own);
    leave_update_capture_of_things_disabled(&db);
    let (release, locker) = hold(
        shared().url(&db.name),
        "LOCK TABLE public.things IN ACCESS EXCLUSIVE MODE".to_owned(),
    );
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let own = stopped.start();
        // The first use: a `query` on another table.
        let asked = Instant::now();
        let (response, answers) = own.run_body(
            &User::new(WRITER),
            application,
            &json!({ "calls": [own_query("user", "select count(*)::int4 as n from counters")] }),
            Duration::from_secs(40),
        );
        let took = asked.elapsed();
        eprintln!("the query on another table was answered after {took:?}");
        assert!(
            took <= Duration::from_secs(25),
            "expected the query answered within 25 s while the comparison could not be finished (CONTRACT.md: \"holds no operation up for more than 20 seconds\"); it took {took:?}"
        );
        let answer = answers.into_iter().next().unwrap_or(Value::Null);
        assert!(
            answer["ok"].is_object(),
            "expected the query on another table answered, got {answer} ({})",
            response.describe()
        );
        assert_record(&own, "own-database-not-compared", application);
        own
    }));
    release.send(()).ok();
    locker.join().unwrap();
    let own = match outcome {
        Ok(own) => own,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    // The next occasion: a Version.
    let sent = own.send(application, &base());
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let entries = assert_entries(
        &own,
        application,
        1,
        "when a Version was sent once the lock was gone (CONTRACT.md: \"is made again at the next occasion\")",
    );
    assert_capture_check_entry(&entries[0], application);
    assert_record(&own, "capture-changed-outside", application);
    assert_capture_whole(&db, &shape, "once the lock was gone and a Version sent");
    recorded(
        &own,
        &db,
        application,
        7021,
        "a write after the change was said",
    );
}

/// The request that sends a Version of `application` with these
/// migrations.
fn version_request(own: &Own, application: &str, migrations: &[Migration]) -> Request {
    own.api()
        .request(&format!("/v1/applications/{application}/versions"))
        .method("POST")
        .json(&version_body_with_migrations(
            &fixture_bundle(FIXTURE),
            &[],
            migrations,
        ))
        .timeout(Duration::from_secs(60))
}

/// Whether the own database shows a table named `name`.
fn has_table(db: &OwnDatabase, name: &str) -> bool {
    db.column(&format!(
        "SELECT count(*)::text FROM pg_class WHERE relname = {}",
        literal(name)
    )) == vec!["1".to_owned()]
}

/// Stopped before the commit, after the platform wrote what will be: the
/// owner role's row held while the migration sleeps, so that the host's
/// last statement before the commit (research F5: the role's password
/// made null) waits on it.
#[test]
fn a_program_stopped_before_a_version_s_commit_finds_nothing_changed_and_the_version_can_be_sent_again()
 {
    let own = start();
    let application = "stopped-before-commit";
    let db = install_seeded(&own, application);
    let list = base_and(&[Migration::new(
        "0004_before",
        &[
            r#"CREATE TABLE "made_before_the_stop" ("id" integer PRIMARY KEY NOT NULL);"#,
            "SELECT pg_sleep(3);",
        ],
    )]);
    let request = version_request(&own, application, &list);
    let sender = std::thread::spawn(move || request.try_send());
    assert!(
        wait_for_session(&db.name, "query LIKE '%pg_sleep%'", Duration::from_secs(20)),
        "the harness's own check: expected the Version's slow migration running within 20 s"
    );
    // The migration has not touched its role: the row is the harness's at
    // once.
    let (release, locker) = hold(
        shared().url("postgres"),
        format!(
            "SELECT 1 FROM pg_catalog.pg_authid WHERE rolname = {} FOR UPDATE",
            literal(&db.owner)
        ),
    );
    let waited = wait_for_session(
        &db.name,
        "wait_event_type = 'Lock'",
        Duration::from_secs(15),
    );
    let stopped = stop(own);
    drop(sender.join());
    release.send(()).ok();
    locker.join().unwrap();
    assert!(
        waited,
        "the harness's own check: expected the host's last statement before the commit to wait on the owner role's row (research F5); no session of the own database waited on a lock within 15 s"
    );
    let own = stopped.start();
    let _ = write(&own, application, 7031);
    let found = capture_checks_eventually(&own, application);
    assert!(
        found.is_empty(),
        "expected nothing found changed after a stop before the commit (CONTRACT.md: \"after a stop before the commit, nothing is found changed\"), got:\n{}",
        lines(&found)
    );
    assert!(
        !has_table(&db, "made_before_the_stop"),
        "expected nothing of the Version kept"
    );
    assert_eq!(own.version_numbers(application), vec![1]);
    recorded(&own, &db, application, 7032, "a write after the start");
    let sent = own.send(application, &list);
    assert_eq!(
        sent.status,
        201,
        "expected the same Version applied when sent again, got\n{}",
        sent.describe()
    );
    assert!(has_table(&db, "made_before_the_stop"));
    let _ = write(&own, application, 7033);
    assert_entries(&own, application, 0, "after the Version was sent again");
}

/// A trigger on `own_database_states` for the `UPDATE` that settles what is
/// kept (sets `pending` to null), doing `body` and counting each time in
/// `harness_settle_fired`.
fn on_settling(body: &str) -> String {
    format!(
        r"
CREATE SEQUENCE public.harness_settle_fired;
GRANT USAGE, SELECT, UPDATE ON SEQUENCE public.harness_settle_fired TO PUBLIC;
CREATE FUNCTION public.harness_on_settling() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
  PERFORM nextval('public.harness_settle_fired');
  {body}
  RETURN NEW;
END $f$;
CREATE TRIGGER harness_on_settling BEFORE UPDATE ON public.own_database_states
  FOR EACH ROW WHEN (OLD.pending IS NOT NULL AND NEW.pending IS NULL)
  EXECUTE FUNCTION public.harness_on_settling();
"
    )
}

const SETTLE_AGAIN: &str = "DROP TRIGGER harness_on_settling ON public.own_database_states";

/// Stopped (`SIGKILL`), the table left disabled, started; then the first
/// use must find the change and say it once.
fn assert_a_later_change_is_found(own: Own, db: &OwnDatabase, application: &str, id: i64) -> Own {
    let shape = capture_shape(db);
    let stopped = stop(own);
    leave_update_capture_of_things_disabled(db);
    let own = stopped.start();
    let _ = write(&own, application, id);
    let entries = assert_entries(
        &own,
        application,
        1,
        "a change made outside afterwards, at the first use after a start (CONTRACT.md: \"a change made outside later is found as always\")",
    );
    assert_capture_check_entry(&entries[0], application);
    assert_record(&own, "capture-changed-outside", application);
    assert_capture_whole(db, &shape, "once the later change was said");
    own
}

/// Stopped after the commit, before keeping was settled.
#[test]
fn a_program_stopped_after_a_version_s_commit_before_keeping_finds_nothing_changed_and_later_changes_still()
 {
    let own = start();
    let application = "stopped-after-commit";
    let db = install_seeded(&own, application);
    let platform = platform_superuser(&own);
    platform.batch(&on_settling("PERFORM pg_sleep(30);"));
    let list = base_and(&[Migration::new(
        "0004_after",
        &[r#"CREATE TABLE "made_after" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    )]);
    let request = version_request(&own, application, &list);
    let sender = std::thread::spawn(move || request.try_send());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !has_table(&db, "made_after") {
        assert!(
            Instant::now() < deadline,
            "the harness's own check: expected the Version's migrations committed within 20 s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let platform_name = own.vm.platform().name().to_owned();
    let settling = wait_for_session(
        &platform_name,
        "wait_event = 'PgSleep'",
        Duration::from_secs(15),
    );
    let stopped = stop(own);
    drop(sender.join());
    // The program's connections are gone with it; the sleeping one is
    // ended, as a lost connection would end it.
    platform.batch(&format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
          WHERE datname = {} AND pid <> pg_backend_pid()",
        literal(&platform_name)
    ));
    platform.batch(SETTLE_AGAIN);
    assert!(
        settling,
        "the harness's own check: expected the platform to settle what it keeps with an UPDATE of `own_database_states` that sets `pending` to null after the commit, held by the harness's trigger (fired {} time(s)); the names may differ from what the harness assumes",
        fired(&platform, "harness_settle_fired")
    );
    drop(platform);
    let own = stopped.start();
    let _ = write(&own, application, 7041);
    let found = capture_checks_eventually(&own, application);
    assert!(
        found.is_empty(),
        "expected nothing found changed after a stop after the commit (CONTRACT.md: \"after a stop or a failure after the commit, nothing is found changed either\"), got:\n{}",
        lines(&found)
    );
    recorded(&own, &db, application, 7042, "a write after the start");
    let own = Own {
        vm: own.vm.restart(),
    };
    let _ = write(&own, application, 7043);
    assert_entries(&own, application, 0, "after one more start");
    let own = assert_a_later_change_is_found(own, &db, application, 7044);
    recorded(
        &own,
        &db,
        application,
        7045,
        "a write after the later change was said",
    );
}

/// Keeping fails after the commit.
#[test]
fn keeping_that_fails_after_a_version_s_commit_is_said_and_nothing_false_is_said_afterwards() {
    let own = start();
    let application = "not-kept";
    let db = install_seeded(&own, application);
    let platform = platform_superuser(&own);
    platform.batch(&on_settling(
        "RAISE EXCEPTION 'the harness refuses to settle what is kept';",
    ));
    let list = base_and(&[Migration::new(
        "0004_after",
        &[r#"CREATE TABLE "made_after" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    )]);
    let sent = own.send(application, &list);
    let times = fired(&platform, "harness_settle_fired");
    platform.batch(SETTLE_AGAIN);
    assert!(
        times > 0,
        "the harness's own check: expected the platform to settle what it keeps with an UPDATE of `own_database_states` that sets `pending` to null; the harness's trigger never fired, so the names may differ from what it assumes"
    );
    assert_eq!(
        sent.status,
        201,
        "expected the Version answered `201` when keeping failed after the commit (CONTRACT.md: \"the Version is still answered `201`\"), got\n{}",
        sent.describe()
    );
    assert!(has_table(&db, "made_after"));
    assert_record(&own, "own-database-structure-not-kept", application);
    let _ = write(&own, application, 7051);
    let found = capture_checks_eventually(&own, application);
    assert!(
        found.is_empty(),
        "expected nothing found changed after keeping failed (CONTRACT.md: \"nothing false is said afterwards\"), got:\n{}",
        lines(&found)
    );
    recorded(&own, &db, application, 7052, "a write after keeping failed");
    let own = Own {
        vm: own.vm.restart(),
    };
    let _ = write(&own, application, 7053);
    assert_entries(&own, application, 0, "after a start");
    // A Version with none to apply, as an occasion of its own.
    let sent = own.send(application, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_entries(&own, application, 0, "after a Version with none to apply");
    let own = assert_a_later_change_is_found(own, &db, application, 7054);
    recorded(
        &own,
        &db,
        application,
        7055,
        "a write after the later change was said",
    );
}
