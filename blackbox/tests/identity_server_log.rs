//! The Postgres server log gives its reader nothing to attach an identity
//! with (CONTRACT.md, "Identity and Change history in the own database: The
//! server log"; ticket "Identity in the own database, and Change history",
//! sixth acceptance criterion: "With statement logging on, the secret does
//! not appear in the Postgres server log").
//!
//! This test binary turns statement logging fully on for its whole test
//! Postgres (`ALTER SYSTEM`) before the program starts, so that everything
//! from the own database's creation on is logged: every statement, every
//! duration, parameters whole. Then, after operations as both actors and a
//! second Version with a migration that changes rows:
//!
//! - no value of a table of schema `vm` that the runtime role cannot read
//!   (left out: values of fewer than 12 characters, and values that are
//!   public anyway, like the migrations' text and the hashes the API
//!   answers) appears anywhere in the server log, whole, as hex or as
//!   base64 (both ways);
//! - every statement the log shows sent on the runtime role's connections
//!   to the own database is sent again, with its logged parameters written
//!   in as literals, grouped as the log shows them by backend and
//!   transaction, by a new login as the runtime role in a new transaction:
//!   then `vm.identity()` raises `VM001` and a write to a captured table
//!   fails with `VM001`. (The nearest faithful replay from the log: the
//!   extended protocol's parameters are sent as literals; transaction
//!   control is left out so that the new transaction is read-write and the
//!   write is tried for real.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{
    APP, NO_IDENTITY, attempt, encodings, history_standard, own_call, secret_values,
};
use blackbox::own_database::{Migration, Own, assert_ok, own_execute, own_query};
use blackbox::postgres::shared;
use blackbox::server_log::{entries, log_everything, sent_by, transactions};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use serde_json::{Value, json};

#[test]
fn the_server_log_holds_no_secret_and_replaying_it_attaches_nothing() {
    log_everything(shared());
    let own = Own::start();
    let mut migrations = history_standard();
    let db = own.install(APP, &migrations);
    let marker = format!("param-marker-{}", token());
    let mut sent_sql: Vec<String> = Vec::new();
    for (user, actor) in [
        (User::new("alice"), "user"),
        (User::new("bob"), "application"),
        (User::new("alice"), "application"),
    ] {
        let calls = [
            own_query(actor, "select * from vm.identity()"),
            own_query(actor, "select current_setting('vm.ctx') as ctx"),
            own_execute(
                actor,
                "insert into things (title) values ('logged') returning id",
            ),
            own_call(
                "execute",
                actor,
                "insert into things (title) values ($1) returning id",
                &json!([marker]),
            ),
            own_execute(actor, "update things set size = 1"),
            own_query(
                actor,
                "select id, kind, user_id from vm.history order by id",
            ),
        ];
        for call in &calls {
            sent_sql.push(call["arguments"]["sql"].as_str().unwrap().to_owned());
        }
        let (_, answers) = own.run(&user, APP, &calls);
        for answer in &answers {
            assert_ok(answer, &format!("an operation as the {actor}"));
        }
    }
    // A migration that changes rows, so that the migration's identity is
    // attached too.
    migrations.push(Migration::new(
        "0003_logged_change",
        &[r#"UPDATE "parents" SET "name" = 'changed while logged' WHERE "id" = 1;"#],
    ));
    own.send_ok(APP, &migrations);
    // The harness's own check that the log holds what the program sent,
    // parameters included, on the runtime role's connections.
    let log = shared().server_log();
    assert!(
        log.contains(&marker),
        "the harness's own check: the server log must show the parameters of the statements sent (looked for {marker:?})"
    );
    let parsed = entries(shared());
    let sent = sent_by(&parsed, &db.name, &db.runtime);
    assert!(
        !sent.is_empty(),
        "the harness's own check: the server log must show statements sent on the runtime role's connections to {}",
        db.name
    );
    let by_host: Vec<&str> = sent
        .iter()
        .map(|s| s.sql.as_str())
        .filter(|s| !sent_sql.iter().any(|mine| s.trim() == mine.trim()))
        .collect();
    assert!(
        !by_host.is_empty(),
        "expected the log to show statements the host sent besides the Application's"
    );

    // (a) No secret, in any encoding, anywhere in the server log.
    let mut public: Vec<String> = own
        .api()
        .versions(APP)
        .iter()
        .map(Value::to_string)
        .collect();
    for migration in &migrations {
        public.push(migration.name.clone());
        public.push(migration.sql.clone());
    }
    public.extend([
        APP.to_owned(),
        db.name.clone(),
        db.owner.clone(),
        db.runtime.clone(),
        db.platform.clone(),
        blackbox::INSTALLER.to_owned(),
    ]);
    let secrets = secret_values(&db, &public);
    assert!(
        !secrets.is_empty(),
        "expected the platform's secret in a table of schema vm the runtime role cannot read"
    );
    let mut found = Vec::new();
    for secret in &secrets {
        for form in encodings(secret) {
            if log.contains(&form) {
                let line = log
                    .lines()
                    .find(|l| l.contains(&form))
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect::<String>();
                found.push(format!(
                    "  a value of vm ({} characters) as {form:?}, in: {line}",
                    secret.len()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "expected no value the platform keeps secret in vm in the server log, but:\n{}",
        found.join("\n")
    );

    // (b) Sending again what the log shows attaches nothing. A login of
    // the harness's own as the runtime role (set last: the program's new
    // connections as it fail from here on).
    let password = db.set_known_password(&db.runtime);
    let runtime = Sql::connect(&shared().url_as(&db.runtime, &password, &db.name))
        .unwrap_or_else(|e| panic!("cannot log in as the runtime role: {e}"));
    assert_eq!(
        runtime.value("SELECT current_user::text").as_deref(),
        Some(db.runtime.as_str())
    );
    let groups = transactions(&sent);
    assert!(!groups.is_empty());
    let mut wrong = Vec::new();
    for group in &groups {
        runtime.batch("BEGIN");
        for statement in group {
            drop(attempt(&runtime, statement));
        }
        let identity = attempt(&runtime, "SELECT * FROM vm.identity()");
        if !matches!(&identity, Err(e) if e.code == NO_IDENTITY) {
            wrong.push(format!(
                "  after replaying {group:#?}\n  vm.identity() gave {identity:?}"
            ));
        }
        let write = attempt(
            &runtime,
            "INSERT INTO things (title) VALUES ('written after a replay of the log')",
        );
        if !matches!(&write, Err(e) if e.code == NO_IDENTITY) {
            wrong.push(format!(
                "  after replaying {group:#?}\n  a write to things gave {write:?}"
            ));
        }
        runtime.batch("ROLLBACK");
    }
    // And everything the runtime role's connections sent, in one
    // transaction.
    runtime.batch("BEGIN");
    for statement in groups.iter().flatten() {
        drop(attempt(&runtime, statement));
    }
    let identity = attempt(&runtime, "SELECT * FROM vm.identity()");
    if !matches!(&identity, Err(e) if e.code == NO_IDENTITY) {
        wrong.push(format!(
            "  after replaying every statement: vm.identity() gave {identity:?}"
        ));
    }
    runtime.batch("ROLLBACK");
    assert!(
        wrong.is_empty(),
        "expected nothing replayed from the server log to attach an identity, but in {} of {} replays (the first few):\n{}",
        wrong.len(),
        groups.len() * 2 + 1,
        wrong.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
}
