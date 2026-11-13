//! Where the Change history is kept, and who can reach it (CONTRACT.md,
//! "The Change history in the platform database: Where it is kept" and
//! "What the roles of an own database can and cannot do"; ticket "The
//! Change history lives in the platform database, shipped from an outbox",
//! first acceptance criterion: "No role of an own database can read or
//! change the Change history where it is kept, nor connect to the platform
//! database").
//!
//! - There is no `vm.history` in an own database: a statement naming it
//!   fails with `42P01` for every role (the platform's role, the migration
//!   role, the runtime role; and the superuser). (A migration's text naming
//!   it is in `out_of_reach_attacks.rs`.)
//! - The outbox, whatever its name, is out of the migration role's reach:
//!   reading, writing, locking or truncating any table of `vm` fails with
//!   `42501`, as the role in a session and from a migration's text, one
//!   hostile Version per table and attack. (The runtime role's is in
//!   `identity_hostile.rs`.) A migration cannot install `dblink` or
//!   `postgres_fdw` to reach another database.
//! - `change_history`'s `(application, id)` is unique.
//! - No role of an own database may connect to the platform database: not
//!   by its rights, not when it logs in; the program takes `CONNECT` from
//!   `PUBLIC` when it starts, and does not start with a
//!   `platform-database-open` record when IT owns the database and `PUBLIC`
//!   still has it.
//! - No API for Platform administrators reads the history: the Operation
//!   log shows that a history was read and of which table, never a value
//!   of a row, and no route under `/v1` answers changes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::{APP, history_standard, own_call, platform_sql, wait_shipped};
use blackbox::history_kept::{
    attacks_on, it_made_platform, public_connect, records_in, refusals_as, roles_of, serve_to_exit,
    statements_on_vm_history, vm_tables,
};
use blackbox::history_op::{changes_of, history_call};
use blackbox::out_of_reach::{ANY_CODE, Answers, Case, failed, install_seeded, run_cases, start};
use blackbox::own_database::{ACTORS, Own, assert_ok, ident, literal, try_login};
use blackbox::postgres::shared;
use blackbox::scenario::{administrator, lines};
use blackbox::sql::Sql;
use blackbox::{User, VmOptions};
use serde_json::{Value, json};

const DENIED: Answers = &[failed("42501")];

fn alice() -> User {
    User::new("alice")
}

/// CONTRACT.md: "In the own database there is no `vm.history`: for every
/// role, a statement that names it fails with `42P01`." Each of the three
/// roles of an own database (and the harness's superuser) tries to read,
/// write, lock and truncate it in a session of its own.
#[test]
fn vm_history_is_not_there_for_any_role_of_an_own_database() {
    let own = start();
    let db = install_seeded(&own, APP);
    let statements = statements_on_vm_history();
    let mut wrong = Vec::new();
    for (what, role) in roles_of(&db) {
        let found = refusals_as(&db, Some(&role), &statements, "42P01");
        if !found.is_empty() {
            wrong.push(format!("{what} ({role}):"));
            wrong.extend(found);
        }
    }
    wrong.extend(refusals_as(&db, None, &statements, "42P01"));
    let relation = db
        .superuser()
        .value("SELECT to_regclass('vm.history')::text");
    assert!(
        wrong.is_empty() && relation.is_none(),
        "expected no vm.history in the own database (to_regclass gives {relation:?}), and every statement naming it to fail with 42P01, but:\n{}",
        wrong.join("\n")
    );
}

/// CONTRACT.md: "For the migration role and the runtime role, reading it,
/// writing it, locking it or truncating it fails with `42501`, as for
/// anything else in `vm` they were not given." The migration role (the
/// harness's superuser become it), on every table of `vm`, the outbox
/// among them.
#[test]
fn the_migration_role_cannot_read_write_lock_or_truncate_any_table_of_vm_in_a_session() {
    let own = start();
    let db = install_seeded(&own, APP);
    // A change in the outbox or on its way, so that there is something to
    // read.
    let (_, answer) = own.one(
        &alice(),
        APP,
        &own_call(
            "execute",
            "user",
            "insert into things (id, title) values ($1, $2)",
            &json!([501, "in the outbox"]),
        ),
    );
    assert_ok(&answer, "an insert");
    let statements: Vec<String> = vm_tables(&db)
        .iter()
        .flat_map(|t| attacks_on(t).into_iter().map(|(_, s)| s))
        .collect();
    let wrong = refusals_as(&db, Some(&db.owner), &statements, "42501");
    assert!(
        wrong.is_empty(),
        "expected Postgres to refuse each of these to the migration role {} with 42501, but:\n{}",
        db.owner,
        wrong.join("\n")
    );
}

/// As above, from a migration's text: one hostile Version per table of
/// `vm` and attack, each answered `422 migration-failed` with `42501`,
/// the database untouched, Version 1 still serving.
#[test]
fn a_migration_cannot_read_write_lock_or_truncate_any_table_of_vm() {
    let own = start();
    // The tables of vm, as an Application's own database has them.
    let probe = install_seeded(&own, "probe");
    let tables = vm_tables(&probe);
    let mut cases = Vec::new();
    for table in &tables {
        for (what, statement) in attacks_on(table) {
            let name: &'static str =
                Box::leak(format!("{}-{what}", table.name.replace('_', "-")).into_boxed_str());
            cases.push(Case::statements(name, DENIED, move |_| {
                vec![format!("{statement};")]
            }));
        }
    }
    run_cases(&own, "vm-table", cases);
}

/// A migration's SQL cannot connect anywhere by itself; an extension
/// such as `dblink` or `postgres_fdw` could, and neither is trusted, so
/// the migration role cannot install one: the Version is refused (by
/// Postgres, `42501`, or by the platform), and nothing is changed.
#[test]
fn a_migration_cannot_install_an_extension_that_connects_to_another_database() {
    let own = start();
    let probe = install_seeded(&own, "probe");
    for extension in ["dblink", "postgres_fdw"] {
        let available = probe.superuser().value(&format!(
            "SELECT count(*)::text FROM pg_available_extensions WHERE name = {}",
            literal(extension)
        ));
        assert_eq!(
            available.as_deref(),
            Some("1"),
            "the harness's own check: expected {extension} available on the test Postgres, so that refusing it means something"
        );
    }
    const REFUSED_ANY: Answers = &[failed("42501"), ("migration-refused", ANY_CODE)];
    let cases = vec![
        Case::statements("dblink", REFUSED_ANY, |_| {
            vec!["CREATE EXTENSION dblink;".to_owned()]
        }),
        Case::statements("postgres-fdw", REFUSED_ANY, |_| {
            vec!["CREATE EXTENSION postgres_fdw;".to_owned()]
        }),
        Case::statements("dblink-in-public", REFUSED_ANY, |_| {
            vec!["CREATE EXTENSION IF NOT EXISTS dblink SCHEMA public;".to_owned()]
        }),
    ];
    run_cases(&own, "extension", cases);
}

/// CONTRACT.md: "`(application, id)` is unique." In the catalog: a unique
/// index of `change_history` on exactly those two columns, for every row;
/// and a copy of a row, inserted by the administrator, fails with
/// `23505` (rolled back).
#[test]
fn change_history_is_unique_on_application_and_id() {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    for (i, actor) in ACTORS.iter().enumerate() {
        let (_, answer) = own.one(
            &alice(),
            APP,
            &own_call(
                "execute",
                actor,
                "insert into things (id, title) values ($1, $2)",
                &json!([600 + i, "unique"]),
            ),
        );
        assert_ok(&answer, "an insert");
    }
    wait_shipped(&db, 2);
    let sql = platform_sql(&db);
    let indexes = sql.batch(
        "SELECT i.indexrelid::regclass::text,
                (SELECT string_agg(a.attname::text, ',' ORDER BY a.attname)
                   FROM generate_series(0, i.indnkeyatts - 1) AS n
                   JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[n]),
                (i.indpred IS NULL AND i.indexprs IS NULL)::text
           FROM pg_index i
          WHERE i.indrelid = to_regclass('change_history') AND i.indisunique",
    );
    assert!(
        indexes.iter().any(|row| {
            row.get(1).cloned().flatten().as_deref() == Some("application,id")
                && row.get(2).cloned().flatten().as_deref() == Some("true")
        }),
        "expected a unique index of change_history on exactly (application, id), with no predicate and no expression; its unique indexes (name, key columns, whole) are {indexes:?}"
    );
    sql.batch("BEGIN");
    let copy = sql.try_coded(&format!(
        "INSERT INTO change_history OVERRIDING SYSTEM VALUE
         SELECT * FROM change_history WHERE application = {} ORDER BY id LIMIT 1",
        literal(APP)
    ));
    sql.batch("ROLLBACK");
    assert!(
        matches!(&copy, Err(e) if e.code == "23505"),
        "expected a second row with the same (application, id) to fail with 23505, got {copy:?}"
    );
}

/// CONTRACT.md: "No role of an own database can connect to the platform
/// database (the platform's role for that database, the migration role,
/// the runtime role: each attempt fails with `42501`)". By their rights
/// (`has_database_privilege`), and by logging in: once the rest is
/// asserted, the administrator gives each a password the harness knows
/// (and the migration role, which may not log in at all, `LOGIN`, so that
/// what is tried is the connection, not the login), and each tries.
#[test]
fn no_role_of_an_own_database_can_connect_to_the_platform_database() {
    let own = start();
    let db = own.install(APP, &history_standard());
    let platform = db.platform.clone();
    let mut wrong = Vec::new();
    for (what, role) in roles_of(&db) {
        if db.has_database_privilege(&role, &platform, "CONNECT") {
            wrong.push(format!(
                "  {what} {role} has CONNECT on the platform database {platform}"
            ));
        }
    }
    // Last: new connections of the program as these roles fail from here.
    let instance = db.instance();
    for (what, role) in roles_of(&db) {
        let password = format!("pw-kept-{}", blackbox::shared_database::token());
        instance.batch(&format!(
            "ALTER ROLE {} LOGIN PASSWORD {}",
            ident(&role),
            literal(&password)
        ));
        match try_login(&role, &password, &platform) {
            Err(e) if e.code == "42501" => {}
            other => wrong.push(format!(
                "  {what} {role} logging in to {platform}: expected 42501, got {other:?}"
            )),
        }
        // The harness's own check: the password is right, so what was
        // refused above is the connection to the platform database.
        if role != db.owner {
            let own_db = try_login(&role, &password, &db.name);
            assert!(
                own_db.is_ok(),
                "the harness's own check: expected {what} {role} able to log in to its own database {} with the password just set, got {own_db:?}",
                db.name
            );
        }
    }
    assert!(
        wrong.is_empty(),
        "expected no role of the own database able to connect to the platform database, but:\n{}",
        wrong.join("\n")
    );
}

/// CONTRACT.md: "The program takes `CONNECT` on the platform database
/// from `PUBLIC` when it starts." Both for a platform database the program
/// made as a role that may only create databases and roles, and for one it
/// made as a superuser: its `datacl` is set (null is Postgres's default,
/// which gives `PUBLIC` `CONNECT`) and grants `PUBLIC` no `CONNECT`.
#[test]
fn the_program_takes_connect_on_the_platform_database_from_public() {
    for (what, own) in [
        ("a role that may create databases and roles", start()),
        ("a superuser", Own::start()),
    ] {
        let platform = own.vm.platform().name().to_owned();
        let (acl, public) = public_connect(&platform);
        assert!(
            acl.is_some() && !public,
            "the program started as {what}: expected the platform database {platform}'s datacl set and granting no CONNECT to PUBLIC, got datacl {acl:?}"
        );
    }
}

/// CONTRACT.md: "When it may not (IT made the database and owns it) and
/// `PUBLIC` still has it, the program does not start: it exits with a
/// status other than 0 and a `platform-database-open` record that gives
/// the statement to run (`REVOKE CONNECT ON DATABASE ... FROM PUBLIC`)."
#[test]
fn a_platform_database_it_does_not_own_and_public_may_connect_to_stops_the_start() {
    let platform = it_made_platform();
    let finished = serve_to_exit(&platform);
    let shown = format!(
        "exit: {}\nstandard error:\n{}",
        finished.status_text(),
        finished.stderr
    );
    assert!(
        finished.status.is_some_and(|s| !s.success()),
        "expected `vm serve` to exit with a status other than 0 by itself\n{shown}"
    );
    assert!(
        !finished.stderr.contains("\"listening\""),
        "expected no `listening` record\n{shown}"
    );
    let records = records_in(&finished.stderr);
    let open: Vec<&Value> = records
        .iter()
        .filter(|r| r["event"] == "platform-database-open")
        .collect();
    assert!(
        open.len() == 1,
        "expected one `platform-database-open` record\n{shown}"
    );
    let text = open[0].to_string();
    for word in ["REVOKE CONNECT ON DATABASE", "FROM PUBLIC", platform.name()] {
        assert!(
            text.contains(word),
            "expected the `platform-database-open` record to give the statement to run, holding {word:?}, got {text}"
        );
    }
    let (_, public) = public_connect(platform.name());
    assert!(
        public,
        "the harness's own check: PUBLIC still has CONNECT on {}",
        platform.name()
    );
}

/// The other side of the test above: in a platform database IT made and
/// owns, once the administrator has taken `CONNECT` from `PUBLIC`, the
/// program starts and serves, and no role of an own database it makes may
/// connect to the platform database.
#[test]
fn a_platform_database_it_does_not_own_without_connect_for_public_starts() {
    let platform = it_made_platform();
    let instance = Sql::connect(&shared().url("postgres")).unwrap();
    instance.batch(&format!(
        "REVOKE CONNECT ON DATABASE {} FROM PUBLIC",
        ident(platform.name())
    ));
    let own = Own::start_with(&VmOptions::new().platform(&platform));
    let db = own.install(APP, &history_standard());
    for (what, role) in roles_of(&db) {
        assert!(
            !db.has_database_privilege(&role, platform.name(), "CONNECT"),
            "expected {what} {role} unable to connect to the platform database {}",
            platform.name()
        );
    }
}

/// CONTRACT.md: "No API for Platform administrators reads the history.
/// `GET /v1/operation-log` shows that a history was read and of which
/// table, never a row of it; no route under `/v1` answers changes." Rows
/// holding a marker are written and their history read; the Operation log
/// has the `history` entries and the marker nowhere; no route an
/// administrator might try answers changes, and no answer under `/v1`
/// holds the marker.
#[test]
fn no_api_for_platform_administrators_reads_the_history() {
    let own = Own::start();
    own.install(APP, &history_standard());
    let marker = format!("kept-marker-{}", blackbox::shared_database::token());
    let mut calls = Vec::new();
    for (i, actor) in ACTORS.iter().enumerate() {
        calls.push(own_call(
            "execute",
            actor,
            "insert into things (id, title, tags) values ($1, $2, $3::jsonb)",
            &json!([
                700 + i,
                format!("{marker} title"),
                json!({ "m": marker }).to_string()
            ]),
        ));
        calls.push(own_call(
            "execute",
            actor,
            "update things set title = $1 where id = $2",
            &json!([format!("{marker} again"), 700 + i]),
        ));
    }
    calls.push(history_call("user", "public", "things", &json!({})));
    calls.push(history_call(
        "application",
        "public",
        "things",
        &json!({ "key": { "id": 700 }, "limit": 7 }),
    ));
    let (_, answers) = own.run(&alice(), APP, &calls);
    for answer in answers.iter().take(4) {
        assert_ok(answer, "a write with the marker");
    }
    let read = changes_of(&answers[4]);
    assert!(
        read.iter().any(|c| c.to_string().contains(&marker)),
        "the harness's own check: expected the history read to hold the marker, got {}",
        lines(&read)
    );
    changes_of(&answers[5]);
    let log = own.log();
    let reads: Vec<&Value> = log.iter().filter(|e| e["operation"] == "history").collect();
    assert_eq!(
        reads.len(),
        2,
        "expected two `history` entries in the Operation log:\n{}",
        lines(&log)
    );
    for entry in &reads {
        assert_eq!(
            entry["statement"], "history \"public\".\"things\"",
            "expected the entry to say which table's history was read, and nothing more: {entry}"
        );
    }
    let api = own.vm.api_as(&administrator());
    let log_text = api.operation_log(Some("1000")).text();
    assert!(
        !log_text.contains(&marker),
        "expected no value of a row in the Operation log, but the marker is there:\n{log_text}"
    );
    let mut wrong = Vec::new();
    for route in [
        format!("/v1/applications/{APP}/history"),
        format!("/v1/applications/{APP}/history?schema=public&table=things"),
        format!("/v1/applications/{APP}/changes"),
        format!("/v1/applications/{APP}/change-history"),
        format!("/v1/applications/{APP}/tables/things/history"),
        "/v1/history".to_owned(),
        "/v1/changes".to_owned(),
        "/v1/change-history".to_owned(),
        format!("/v1/change-history?application={APP}"),
    ] {
        let answer = api.get(&route);
        if (200..300).contains(&answer.status) || answer.text().contains(&marker) {
            wrong.push(format!(
                "  GET {route}: {} {}",
                answer.status,
                answer.text()
            ));
        }
    }
    for route in [
        "/v1/applications".to_owned(),
        format!("/v1/applications/{APP}"),
        format!("/v1/applications/{APP}/versions"),
        format!("/v1/applications/{APP}/versions/1"),
        "/v1/resources".to_owned(),
        "/v1/operation-log".to_owned(),
    ] {
        let answer = api.get(&route);
        if answer.text().contains(&marker) {
            wrong.push(format!("  GET {route} holds the marker: {}", answer.text()));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected no route under /v1 to answer changes, nor any answer to hold a value of a row:\n{}",
        wrong.join("\n")
    );
}
