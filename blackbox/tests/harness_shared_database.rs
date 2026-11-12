//! The harness's shared databases: what the tests of "A User's read of a
//! shared database is authorized and logged" and "Write access, and the
//! database role as the boundary" stand on. These tests check the harness,
//! not the program: the read login can only read, the careless one can
//! write, the write login writes what it was granted and no more, the
//! server's own log shows what the database received and from which login
//! (and nothing of another database), and the fingerprint sees a change.
//!
//! They also show, on the Postgres the suite runs against, that what the
//! boundary tests rely on is real: SQL hidden in `ts_stat` runs what a
//! statement check never sees and reaches a stronger role under `SET ROLE`;
//! IT's function leaves state that [`OBSERVE_SESSION`] sees and `DISCARD
//! ALL` clears; the owner's trigger reads the identity label. So a boundary
//! test that passes, passes because the program holds the boundary, not
//! because the attack never worked.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::postgres;
use blackbox::shared_database::{
    CLOSE_EVERY_INCIDENT, LEAVE_SESSION_STATE, LEFT_SEARCH_PATH, OBSERVE_SESSION, Session,
    SharedDatabase,
};
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

#[test]
fn the_write_login_writes_what_it_was_granted_and_no_more() {
    let db = SharedDatabase::create();
    let writer = db.connect_as(&db.writer);
    let row = writer
        .batch("insert into notes (body) values ('hello') returning written_by, session_login");
    let row: Vec<Vec<Option<&str>>> = row
        .iter()
        .map(|r| r.iter().map(Option::as_deref).collect())
        .collect();
    assert_eq!(
        row,
        vec![vec![
            Some(db.writer.role.as_str()),
            Some(db.writer.role.as_str())
        ]],
        "expected a row of notes to record the login that wrote it"
    );
    writer.batch("update incidents set open = false where id = 1");
    writer.batch("delete from kinds where label = 'nulls'");
    writer.batch("create temp table scratch (x int)");
    for denied in [
        "select * from staff_only",
        "create table made_here (x int)",
        "truncate incidents",
        "select nextval('ticket_numbers')",
        "alter table incidents add column extra int",
    ] {
        let refused = writer.try_batch(denied);
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("42501")),
            "expected the write login to be refused {denied:?} (42501), got {refused:?}"
        );
    }
    assert!(!format!("{:?}", db.writer).contains(db.writer.password()));
}

#[test]
fn the_server_log_names_the_login_each_statement_came_from() {
    let db = SharedDatabase::create();
    let marker = format!("login-{}", blackbox::shared_database::token());
    db.connect_as(&db.writer)
        .batch(&format!("select '{marker}-w' as probe"));
    db.connect_as(&db.reader)
        .batch(&format!("select '{marker}-r' as probe"));
    db.assert_marker_came_from(
        &format!("{marker}-w"),
        &db.writer,
        "the write login's probe",
    );
    db.assert_marker_came_from(&format!("{marker}-r"), &db.reader, "the read login's probe");
}

#[test]
fn sql_hidden_in_ts_stat_runs_unseen_and_escapes_set_role() {
    let db = SharedDatabase::create();
    // A login of the database's own that may become the read login: the
    // one-login-and-SET-ROLE design ADR 0013 rejects.
    let su = db.superuser();
    su.batch(&format!("GRANT {} TO {}", db.reader.role, db.writer.role));
    let host = db.connect_as(&db.writer);
    host.batch(&format!("SET ROLE {}", db.reader.role));
    let escaped = host.batch(
        "select current_user::text, session_user::text
           from ts_stat($$select to_tsvector(set_config('role', 'none', false))$$)",
    );
    let escaped: Vec<Vec<Option<&str>>> = escaped
        .iter()
        .map(|row| row.iter().map(Option::as_deref).collect())
        .collect();
    assert_eq!(
        escaped,
        vec![vec![
            Some(db.writer.role.as_str()),
            Some(db.writer.role.as_str())
        ]],
        "expected set_config('role', 'none') hidden in ts_stat to reach the stronger login under SET ROLE"
    );
    // Logged in directly, there is nowhere to go.
    let direct = db.connect_as(&db.reader);
    let stayed = direct.batch(
        "select current_user::text, session_user::text
           from ts_stat($$select to_tsvector(set_config('role', 'none', false))$$)",
    );
    let stayed: Vec<Vec<Option<&str>>> = stayed
        .iter()
        .map(|row| row.iter().map(Option::as_deref).collect())
        .collect();
    assert_eq!(
        stayed,
        vec![vec![
            Some(db.reader.role.as_str()),
            Some(db.reader.role.as_str())
        ]]
    );
    // A setting changed for the session from inside ts_stat stays.
    direct.batch("select * from ts_stat($$select to_tsvector(set_config('statement_timeout', '0', false))$$)");
    direct.batch("select * from ts_stat($$select to_tsvector(set_config('search_path', 'elsewhere', false))$$)");
    assert_eq!(
        direct.value("show search_path").as_deref(),
        Some("elsewhere")
    );
}

#[test]
fn a_read_only_transaction_stops_the_owner_s_writing_function_and_a_read_write_one_does_not() {
    let db = SharedDatabase::create();
    let careless = db.connect_as(&db.careless);
    let before = db.fingerprint();
    let refused = careless.try_batch(&format!(
        "begin read only; select 1; select {CLOSE_EVERY_INCIDENT}; commit"
    ));
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("25006")),
        "expected the SECURITY DEFINER write to fail in a read-only transaction, got {refused:?}"
    );
    drop(careless.try_batch("rollback"));
    assert_eq!(db.fingerprint(), before);
    careless.batch(&format!("select {CLOSE_EVERY_INCIDENT}"));
    assert_ne!(
        db.fingerprint(),
        before,
        "expected the fingerprint to see what IT's function writes"
    );
}

#[test]
fn leave_session_state_leaves_what_the_observation_sees_and_discard_all_clears_it() {
    let db = SharedDatabase::create();
    let observe = format!("select {OBSERVE_SESSION}");
    let session = |sql: &Sql| Session::parse(&sql.value(&observe).unwrap()).unwrap();

    let writer = db.connect_as(&db.writer);
    let clean = session(&writer);
    assert!(clean.leftovers().is_empty(), "{clean:?}");
    assert_eq!(clean.current_user, db.writer.role);
    writer.batch(&format!(
        "begin; select 1; select {LEAVE_SESSION_STATE}; commit"
    ));
    let dirty = session(&writer);
    assert_eq!(dirty.search_path, LEFT_SEARCH_PATH);
    assert_eq!(
        dirty.leftovers().len(),
        9,
        "expected the settings, a temporary table, a prepared statement, a cursor, a LISTEN and an advisory lock left in a read-write transaction, got {:?}",
        dirty.leftovers()
    );
    assert_eq!(db.advisory_locks_of(&[&db.writer.role]).len(), 1);
    writer.batch("discard all");
    let cleared = session(&writer);
    assert!(
        cleared.leftovers().is_empty(),
        "expected DISCARD ALL to clear it all, got {:?}",
        cleared.leftovers()
    );
    assert!(db.advisory_locks_of(&[&db.writer.role]).is_empty());

    let reader = db.connect_as(&db.reader);
    reader.batch(&format!(
        "begin read only; select 1; select {LEAVE_SESSION_STATE}; commit"
    ));
    let dirty = session(&reader);
    assert_eq!(
        dirty.leftovers().len(),
        8,
        "expected everything but the temporary table left in a read-only transaction, got {:?}",
        dirty.leftovers()
    );
    assert_eq!(dirty.temp_tables, 0);
}

#[test]
fn the_owner_s_trigger_records_the_identity_label_of_the_transaction() {
    let db = SharedDatabase::create();
    let writer = db.connect_as(&db.writer);
    let labelled = writer.batch(
        "begin;
         select set_config('vm.user', 'u-1', true), set_config('vm.application', 'app-1', true),
                set_config('vm.request', 'req-1', true);
         insert into notes (body) values ('x') returning label_user, label_application, label_request;
         commit",
    );
    assert_eq!(
        labelled.last().cloned(),
        Some(vec![
            Some("u-1".to_owned()),
            Some("app-1".to_owned()),
            Some("req-1".to_owned())
        ])
    );
    let after = writer.batch("insert into notes (body) values ('y') returning label_user");
    assert!(
        matches!(after.as_slice(), [row] if row[0].as_deref().is_none_or(str::is_empty)),
        "expected a transaction-local label gone in the next transaction, got {after:?}"
    );
}
