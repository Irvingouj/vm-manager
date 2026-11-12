//! The harness's shared databases: what the tests of "A User's read of a
//! shared database is authorized and logged" stand on. These tests check
//! the harness, not the program: the read login can only read, the careless
//! one can write, the server's own log shows what the database received
//! (and nothing of another database), and the fingerprint sees a change.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::postgres;
use blackbox::shared_database::SharedDatabase;
use blackbox::sql::Sql;

fn login_url(db: &SharedDatabase, role: &str, password: &str) -> String {
    format!(
        "postgres://{role}:{password}@{}:{}/{}",
        db.host(),
        db.port(),
        db.name()
    )
}

#[test]
fn the_read_login_reads_and_cannot_write_and_the_careless_one_can() {
    let db = SharedDatabase::create();
    let reader = Sql::connect(&login_url(&db, &db.reader.role, db.reader.password())).unwrap();
    assert_eq!(
        reader.value("select title from incidents where id = 3"),
        Some(db.row_marker.clone())
    );
    let refused = reader
        .try_batch("insert into incidents values (9, 'x', true, 1, now())")
        .unwrap_err();
    assert!(
        refused.contains("42501"),
        "expected permission denied, got {refused}"
    );
    assert!(reader.try_batch("select * from staff_only").is_err());
    let careless =
        Sql::connect(&login_url(&db, &db.careless.role, db.careless.password())).unwrap();
    let before = db.fingerprint();
    careless.batch("update incidents set title = 'changed' where id = 1");
    assert_ne!(
        db.fingerprint(),
        before,
        "expected the fingerprint to see a changed row"
    );
    let before = db.fingerprint();
    careless.batch("create table made_here (x int)");
    assert_ne!(
        db.fingerprint(),
        before,
        "expected the fingerprint to see a new table"
    );
    let before = db.fingerprint();
    careless.batch("select nextval('ticket_numbers')");
    assert_ne!(
        db.fingerprint(),
        before,
        "expected the fingerprint to see the sequence move"
    );
    let before = db.fingerprint();
    careless.batch("alter role current_user set work_mem = '8MB'");
    assert_ne!(
        db.fingerprint(),
        before,
        "expected the fingerprint to see a role's setting"
    );
}

#[test]
fn the_server_log_shows_what_this_database_received_and_nothing_of_another() {
    let db = SharedDatabase::create();
    let other = SharedDatabase::create();
    let marker = format!("probe-{}", blackbox::shared_database::token());
    let reader = Sql::connect(&login_url(&db, &db.reader.role, db.reader.password())).unwrap();
    reader.batch(&format!("select '{marker}' as probe"));
    db.assert_received(&marker, "a statement sent by the harness");
    other.assert_not_received(&marker, "a statement sent to another database");
    assert!(
        db.statements().iter().any(|s| s.contains(&db.reader.role)),
        "expected the log to name the role a statement came from, got {:?}",
        db.statements()
    );
}

#[test]
fn shared_databases_live_apart_from_the_platform_databases() {
    let db = SharedDatabase::create();
    assert_ne!(
        db.port(),
        postgres::shared().port(),
        "expected the shared databases on an instance of their own"
    );
    if cfg!(target_os = "linux") {
        assert!(
            db.host_is_distinct(),
            "expected the shared databases on a loopback address other than 127.0.0.1 on Linux, got {}",
            db.host()
        );
    }
    assert!(!format!("{:?} {:?}", db.reader, db.careless).contains(db.reader.password()));
}
