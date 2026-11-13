//! The migration role's password (CONTRACT.md, "The platform's part of an
//! own database is out of a migration's reach: The migration role's
//! password"; research part F, "The migration role's password").
//!
//! A migration gives the owner role a password the test knows
//! (`ALTER ROLE CURRENT_USER PASSWORD '...'`). No login as the owner role
//! with it may succeed:
//!
//! - while the Version is applied, and after it is accepted or refused;
//! - while the platform's closing of the role is held up: the harness's
//!   superuser takes the role's catalog row `FOR UPDATE` (it waits for the
//!   migration's commit, then holds the row, so the platform's `ALTER ROLE`
//!   waits), as research F5 does. The Version is still answered `201`, the
//!   program's log has `own-database-migration-role-not-closed`, and the
//!   role is closed before the own database is next used. The hold lasts
//!   until the Version is answered, at most 15 s;
//! - when the program is killed (`SIGKILL`) during a slow migration: at the
//!   next start, before it serves, the role cannot log in and has no
//!   session; nothing of the Version is kept; the Version can be sent again;
//! - when the client that sent the Version goes away before the answer:
//!   within 15 s the role cannot log in and has no session, and the
//!   migrations were applied whole or not at all;
//! - two first Versions of one Application sent at once make one own
//!   database and one set of roles, are each answered `201` or with a
//!   refusal, never `500`, are numbered 1 and 2 when both are accepted, and
//!   leave the role unable to log in.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::history::APP;
use blackbox::http::{Request, Response};
use blackbox::out_of_reach::{base, base_and, can_log_in, install_seeded, start};
use blackbox::own_database::{
    FIXTURE, Migration, Own, OwnDatabase, comment_of, literal, own_databases_of, own_execute,
    try_login, version_body_with_migrations,
};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use blackbox::vm::{Vm, fixture_bundle};

/// The request that sends a Version of `application` with these
/// migrations, waiting up to `timeout` for the answer.
fn version_request(
    own: &Own,
    application: &str,
    migrations: &[Migration],
    timeout: Duration,
) -> Request {
    own.api()
        .request(&format!("/v1/applications/{application}/versions"))
        .method("POST")
        .json(&version_body_with_migrations(
            &fixture_bundle(FIXTURE),
            &[],
            migrations,
        ))
        .timeout(timeout)
}

/// A migration that gives the owner role `password` and then runs for
/// `seconds`.
fn password_then_sleep(password: &str, seconds: u32) -> Migration {
    Migration::new(
        "0004_password",
        &[
            &format!("ALTER ROLE CURRENT_USER PASSWORD {};", literal(password)),
            r#"CREATE TABLE "made_with_the_password" ("id" integer PRIMARY KEY NOT NULL);"#,
            &format!("SELECT pg_sleep({seconds});"),
        ],
    )
}

/// The sessions of `role` Postgres shows, from the harness's superuser.
fn sessions_of(db: &OwnDatabase, role: &str) -> Vec<String> {
    db.instance().column_of(&format!(
        "SELECT pid::text || ' ' || coalesce(state, '') || ' ' || left(coalesce(query, ''), 80)
           FROM pg_stat_activity WHERE usename = {}",
        literal(role)
    ))
}

trait Column {
    fn column_of(&self, sql: &str) -> Vec<String>;
}

impl Column for Sql {
    fn column_of(&self, sql: &str) -> Vec<String> {
        self.batch(sql)
            .into_iter()
            .map(|row| row.into_iter().next().flatten().unwrap_or_default())
            .collect()
    }
}

/// Waits up to `timeout` until the owner role runs `pg_sleep` (its
/// migration is being applied).
fn wait_for_the_slow_migration(db: &OwnDatabase, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let running = db.instance().column_of(&format!(
            "SELECT pid::text FROM pg_stat_activity WHERE usename = {} AND query LIKE '%pg_sleep%'",
            literal(&db.owner)
        ));
        if !running.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the harness's own check: expected the owner role's session to run the slow migration within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fails if a login as the owner role with `password` succeeds, to the own
/// database or to `postgres`.
fn assert_no_login(db: &OwnDatabase, password: &str, context: &str) {
    for database in [db.name.as_str(), "postgres"] {
        assert!(
            try_login(&db.owner, password, database).is_err(),
            "{context}: a login as the owner role {} to {database} with the password a migration gave it succeeded",
            db.owner
        );
    }
}

/// Whether the owner role has a password, as `pg_authid` says.
fn has_password(db: &OwnDatabase) -> bool {
    db.instance()
        .value(&format!(
            "SELECT (rolpassword IS NOT NULL)::text FROM pg_authid WHERE rolname = {}",
            literal(&db.owner)
        ))
        .as_deref()
        == Some("true")
}

#[test]
fn no_login_with_a_password_a_migration_set_while_applied_accepted_or_refused() {
    let own = start();
    let db = install_seeded(&own, APP);
    // Accepted, the password set while a slow migration runs.
    let password = format!("known-{}", token());
    let applied = base_and(&[password_then_sleep(&password, 2)]);
    let request = version_request(&own, APP, &applied, Duration::from_secs(60));
    let sender = std::thread::spawn(move || request.send());
    wait_for_the_slow_migration(&db, Duration::from_secs(20));
    assert_no_login(&db, &password, "while the Version is applied");
    let sent = sender.join().unwrap();
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_no_login(&db, &password, "after the Version was accepted");
    assert!(
        !has_password(&db),
        "expected the owner role without a password once the migrations are committed (CONTRACT.md: \"From the commit on, the role has no password\")"
    );
    assert!(!can_log_in(&db, &db.owner));
    // Refused: failed in a later statement, and refused by a rule.
    for (name, last) in [
        ("0005_fails", "SELECT 1 / 0;"),
        (
            "0005_refused",
            r#"ALTER TABLE "things" ADD CONSTRAINT "things_title_unique" UNIQUE ("title") DEFERRABLE;"#,
        ),
    ] {
        let again = format!("known-{}", token());
        let set = format!("ALTER ROLE CURRENT_USER PASSWORD {};", literal(&again));
        let mut list = applied.clone();
        list.push(Migration::new(name, &[&set, last]));
        let sent = own.send(APP, &list);
        assert_eq!(sent.status, 422, "{}", sent.describe());
        let context = format!("after the Version ending with {name} was refused");
        assert_no_login(&db, &again, &context);
        assert_no_login(&db, &password, &context);
        assert!(!can_log_in(&db, &db.owner), "{context}");
    }
}

/// What the locker thread says.
enum Locker {
    Held,
    Failed(String),
}

#[test]
fn no_login_with_a_migration_s_password_while_closing_the_role_is_held_up() {
    let own = start();
    let db = install_seeded(&own, APP);
    let password = format!("known-{}", token());
    let request = version_request(
        &own,
        APP,
        &base_and(&[password_then_sleep(&password, 2)]),
        Duration::from_secs(90),
    );
    let (answered_tx, answered_rx) = mpsc::sync_channel::<Response>(1);
    let sender = std::thread::spawn(move || {
        let sent = request.send();
        drop(answered_tx.send(sent));
    });
    wait_for_the_slow_migration(&db, Duration::from_secs(20));
    assert_no_login(&db, &password, "while the Version is applied");
    // The superuser takes the owner role's row: it waits for the
    // migration's commit, then holds the row until released.
    let (held_tx, held_rx) = mpsc::sync_channel::<Locker>(1);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
    let url = blackbox::postgres::shared().url("postgres");
    let owner = db.owner.clone();
    let locker = std::thread::spawn(move || {
        let sql = match Sql::connect(&url) {
            Ok(sql) => sql,
            Err(e) => {
                drop(held_tx.send(Locker::Failed(e)));
                return;
            }
        };
        sql.batch("BEGIN");
        match sql.try_batch(&format!(
            "SELECT 1 FROM pg_catalog.pg_authid WHERE rolname = {} FOR UPDATE",
            literal(&owner)
        )) {
            Ok(_) => drop(held_tx.send(Locker::Held)),
            Err(e) => drop(held_tx.send(Locker::Failed(e))),
        }
        release_rx.recv_timeout(Duration::from_secs(60)).ok();
        sql.batch("ROLLBACK");
    });
    match held_rx.recv_timeout(Duration::from_secs(25)) {
        Ok(Locker::Held) => {}
        Ok(Locker::Failed(e)) => panic!("the harness's own lock on the role's row failed: {e}"),
        Err(e) => panic!("the harness's own lock on the role's row was not taken: {e}"),
    }
    let held_at = Instant::now();
    // Held: the commit was made, the platform's closing waits.
    assert_no_login(&db, &password, "while closing the role is held up");
    assert!(
        !has_password(&db),
        "expected the owner role without a password from the commit on, also while closing it is held up"
    );
    let answered = answered_rx.recv_timeout(Duration::from_secs(15));
    let waited = held_at.elapsed();
    assert_no_login(
        &db,
        &password,
        "while closing the role is held up, after waiting for the answer",
    );
    release_tx.send(()).ok();
    locker.join().unwrap();
    let sent = match answered {
        Ok(sent) => sent,
        Err(_) => {
            panic!(
                "expected the Version answered while closing the role was held up (CONTRACT.md: \"the Version is still answered as its migrations ended\"); after {waited:?} of holding it was not"
            );
        }
    };
    sender.join().unwrap();
    assert_eq!(sent.status, 201, "{}", sent.describe());
    eprintln!("the Version was answered {waited:?} after the role's row was held");
    own.vm.wait_for_record(
        "an `own-database-migration-role-not-closed` record naming the Application",
        |r| r["event"] == "own-database-migration-role-not-closed" && r["application"] == APP,
    );
    // Closed before the own database is next used.
    let (_, answer) = own.one(
        &User::new(blackbox::out_of_reach::WRITER),
        APP,
        &own_execute(
            "user",
            "insert into things (id, title) values (601, 'after the hold')",
        ),
    );
    assert!(answer["ok"].is_object(), "{answer}");
    assert!(
        !can_log_in(&db, &db.owner),
        "expected the owner role closed before the own database was next used"
    );
    assert_no_login(&db, &password, "after the hold");
}

#[test]
fn a_program_killed_during_a_slow_migration_leaves_the_role_closed_at_the_next_start() {
    let own = start();
    let db = install_seeded(&own, APP);
    let password = format!("known-{}", token());
    let list = base_and(&[password_then_sleep(&password, 8)]);
    let request = version_request(&own, APP, &list, Duration::from_secs(30));
    let sender = std::thread::spawn(move || request.try_send());
    wait_for_the_slow_migration(&db, Duration::from_secs(20));
    let Own { vm } = own;
    let platform = vm.platform().clone();
    let options = vm.options().clone();
    drop(vm); // SIGKILL
    drop(sender.join());
    let own = Own {
        vm: Vm::start(&options.platform(&platform)),
    };
    // Before it serves: at once after the program says it listens.
    assert!(
        !can_log_in(&db, &db.owner),
        "expected the owner role unable to log in at the next start, before the program serves"
    );
    assert_eq!(
        sessions_of(&db, &db.owner),
        Vec::<String>::new(),
        "expected the owner role to have no session at the next start, before the program serves"
    );
    assert_no_login(
        &db,
        &password,
        "after the program was killed and started again",
    );
    assert_eq!(
        own.version_numbers(APP),
        vec![1],
        "expected nothing of the Version kept"
    );
    assert_eq!(
        db.column("SELECT count(*)::text FROM pg_class WHERE relname = 'made_with_the_password'"),
        vec!["0".to_owned()],
        "expected nothing of the Version kept"
    );
    // The same Version can be sent again.
    let again = base_and(&[password_then_sleep(&password, 1)]);
    let sent = own.send(APP, &again);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_no_login(&db, &password, "after the Version was sent again");
    assert!(!can_log_in(&db, &db.owner));
}

#[test]
fn a_client_that_goes_away_leaves_the_role_closed_within_15_seconds() {
    let own = start();
    let db = install_seeded(&own, APP);
    let password = format!("known-{}", token());
    let request = version_request(
        &own,
        APP,
        &base_and(&[password_then_sleep(&password, 4)]),
        Duration::from_millis(1500),
    );
    let gone = request.try_send();
    assert!(
        gone.is_err(),
        "the harness's own check: expected the client to give up before the answer, got {:?}",
        gone.map(|r| r.describe())
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let closed = !can_log_in(&db, &db.owner) && sessions_of(&db, &db.owner).is_empty();
        if closed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected within 15 s of the client going away the owner role unable to log in and without a session; it can log in: {}, its sessions: {:?}",
            can_log_in(&db, &db.owner),
            sessions_of(&db, &db.owner)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_no_login(&db, &password, "after the client went away");
    let versions = own.version_numbers(APP);
    let made =
        db.column("SELECT count(*)::text FROM pg_class WHERE relname = 'made_with_the_password'");
    assert!(
        (versions == vec![1] && made == vec!["0".to_owned()])
            || (versions == vec![1, 2] && made == vec!["1".to_owned()]),
        "expected the migrations applied whole or not at all: Versions {versions:?}, the migration's table there: {made:?}"
    );
}

#[test]
fn two_first_versions_sent_at_once_make_one_own_database_and_one_set_of_roles() {
    let own = start();
    let mut wrong = Vec::new();
    for (i, differ) in [false, false, true, true].into_iter().enumerate() {
        let application = format!("twice-{i}");
        own.create(&application);
        let first = base();
        let mut second = base();
        if differ {
            second.push(Migration::new(
                "0004_more",
                &[r#"CREATE TABLE "more" ("id" integer PRIMARY KEY NOT NULL);"#],
            ));
        }
        let requests = [
            version_request(&own, &application, &first, Duration::from_secs(60)),
            version_request(&own, &application, &second, Duration::from_secs(60)),
        ];
        let barrier = Barrier::new(2);
        let answers: Vec<Response> = std::thread::scope(|scope| {
            let handles: Vec<_> = requests
                .iter()
                .map(|r| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        r.send()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let accepted = answers.iter().filter(|a| a.status == 201).count();
        for a in &answers {
            if a.status != 201
                && !(a.status >= 400
                    && a.status < 500
                    && a.json()["error"]["reason"].as_str().is_some())
            {
                wrong.push(format!(
                    "  {application}: expected `201` or a refusal of the platform's API, got\n{}",
                    a.describe()
                ));
            }
        }
        let numbers = own.version_numbers(&application);
        let expected: Vec<u64> = (1..=u64::try_from(accepted).unwrap()).collect();
        if numbers != expected {
            wrong.push(format!(
                "  {application}: {accepted} accepted, expected the Versions numbered {expected:?}, got {numbers:?}"
            ));
        }
        let databases: Vec<(String, String)> = own_databases_of(own.vm.platform())
            .into_iter()
            .filter(|(_, c)| *c == comment_of(&application))
            .collect();
        if databases.len() != 1 {
            wrong.push(format!(
                "  {application}: expected one own database, found {databases:?}"
            ));
            continue;
        }
        let db = own.own_database(&application);
        let roles = db.instance().column_of(&format!(
            "SELECT rolname::text FROM pg_roles WHERE rolname LIKE {} ORDER BY 1",
            literal(&format!("{}\\_%", db.name))
        ));
        let expected_roles = vec![
            format!("{}_owner", db.name),
            format!("{}_platform", db.name),
            format!("{}_runtime", db.name),
        ];
        if roles != expected_roles {
            wrong.push(format!(
                "  {application}: expected one set of roles {expected_roles:?}, got {roles:?}"
            ));
        }
        if can_log_in(&db, &db.owner) {
            wrong.push(format!(
                "  {application}: the owner role can log in afterwards"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "two first Versions sent at once:\n{}",
        wrong.join("\n")
    );
}
