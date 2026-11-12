//! The runtime role is the boundary of the own database (CONTRACT.md, "The
//! Application's own database: What it looks like in Postgres"; ticket "An
//! Application's own database", fourth and fifth acceptance criteria).
//!
//! What Application code runs as owns nothing and may only read and change
//! rows: it cannot create, alter or drop anything, not even a temporary
//! table, nor `TRUNCATE`; it cannot connect to any other database, and no
//! other Application's roles can connect to this one. Every assertion is on
//! Postgres itself: its catalogs and `has_*_privilege`, statements run as
//! the runtime role from the harness's own connection (`SET ROLE` as the
//! superuser), logins tried with a password the harness sets, and the own
//! database's fingerprint after every attempt through the program. The
//! statement check is hygiene, so the attempts through the program also
//! take a form it lets pass: a `SELECT` of `run_sql(...)`, a function the
//! Application's own migration made, which runs any SQL text as its caller.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;

use blackbox::User;
use blackbox::own_database::{
    ACTORS, Own, OwnDatabase, assert_did_not_run, first_tables, forbidden_statements, ident,
    literal, more_tables, own_execute, own_query, runnable_by_run_sql, standard, try_login,
};
use blackbox::scenario::op;
use blackbox::shared_database::{Session, observe_insert, observe_select};
use serde_json::{Value, json};

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

#[test]
fn the_runtime_role_owns_nothing_and_holds_no_power_beyond_rows() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let owned = db.owned_by(&db.runtime);
    assert!(
        owned.is_empty(),
        "expected the runtime role {:?} to own nothing in its own database, but it owns:\n{}",
        db.runtime,
        owned.join("\n")
    );
    let attributes = db.instance().batch(&format!(
        "SELECT rolsuper::text, rolcreatedb::text, rolcreaterole::text, rolreplication::text,
                rolbypassrls::text, rolcanlogin::text
           FROM pg_roles WHERE rolname = {}",
        literal(&db.runtime)
    ));
    assert_eq!(
        attributes,
        vec![vec![
            Some("false".to_owned()),
            Some("false".to_owned()),
            Some("false".to_owned()),
            Some("false".to_owned()),
            Some("false".to_owned()),
            Some("true".to_owned()),
        ]],
        "expected the runtime role to log in and be no superuser, create no database or role, replicate nothing and bypass no row security (super, createdb, createrole, replication, bypassrls, login)"
    );
    // It is a member of no role that holds more: not the owner role, not
    // the platform's role, not any role that may create.
    let reachable = db.column(&format!(
        "SELECT r.rolname FROM pg_roles r
          WHERE r.rolname <> {rt} AND pg_has_role({rt}, r.oid, 'MEMBER')
          ORDER BY 1",
        rt = literal(&db.runtime)
    ));
    assert!(
        reachable.is_empty(),
        "expected the runtime role to be a member of no role (not the owner role {:?}, not the platform's role, no predefined role), but it is a member of {reachable:?}",
        db.owner
    );
    // Privileges, as Postgres reports them.
    let checks = [
        (
            "has_database_privilege",
            "current_database()".to_owned(),
            "CREATE",
            false,
        ),
        (
            "has_database_privilege",
            "current_database()".to_owned(),
            "TEMPORARY",
            false,
        ),
        (
            "has_database_privilege",
            "current_database()".to_owned(),
            "CONNECT",
            true,
        ),
        (
            "has_schema_privilege",
            "'public'".to_owned(),
            "CREATE",
            false,
        ),
        ("has_schema_privilege", "'public'".to_owned(), "USAGE", true),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "SELECT",
            true,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "INSERT",
            true,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "UPDATE",
            true,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "DELETE",
            true,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "TRUNCATE",
            false,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "REFERENCES",
            false,
        ),
        (
            "has_table_privilege",
            "'public.items'".to_owned(),
            "TRIGGER",
            false,
        ),
        (
            "has_table_privilege",
            "'public.seeded'".to_owned(),
            "TRUNCATE",
            false,
        ),
        (
            "has_sequence_privilege",
            "'public.items_id_seq'".to_owned(),
            "USAGE",
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for (function, object, privilege, want) in checks {
        let got = db
            .superuser()
            .value(&format!(
                "SELECT {function}({}, {object}, '{privilege}')::text",
                literal(&db.runtime)
            ))
            .unwrap_or_default();
        if got != want.to_string() {
            wrong.push(format!(
                "  {function}(runtime, {object}, {privilege}) is {got}, expected {want}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected the runtime role to read and change rows and do nothing else, but Postgres says:\n{}",
        wrong.join("\n")
    );
    // A table and a sequence a later migration makes: rows yes, structure
    // no, with nothing else done.
    let mut list = standard();
    list.push(more_tables());
    own.send_ok("board", &list);
    for (object, privilege, want) in [
        ("'public.extras'", "SELECT", true),
        ("'public.extras'", "INSERT", true),
        ("'public.extras'", "UPDATE", true),
        ("'public.extras'", "DELETE", true),
        ("'public.extras'", "TRUNCATE", false),
    ] {
        let got = db.superuser().value(&format!(
            "SELECT has_table_privilege({}, {object}, '{privilege}')::text",
            literal(&db.runtime)
        ));
        assert_eq!(
            got.as_deref(),
            Some(if want { "true" } else { "false" }),
            "{privilege} on {object}, made by a later migration"
        );
    }
    assert_eq!(
        db.superuser().value(&format!(
            "SELECT has_sequence_privilege({}, 'public.extras_id_seq', 'USAGE')::text",
            literal(&db.runtime)
        )),
        Some("true".to_owned()),
        "expected the runtime role to use the sequence of a table a later migration made"
    );
    assert!(db.owned_by(&db.runtime).is_empty());
}

#[test]
fn as_the_runtime_role_postgres_refuses_every_change_of_structure() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let before = db.fingerprint();
    let statements = forbidden_statements(&db);
    let results = db.as_runtime(&statements);
    let allowed: Vec<String> = statements
        .iter()
        .zip(&results)
        .filter(|(_, r)| r.is_ok())
        .map(|(s, _)| format!("  {s}"))
        .collect();
    assert!(
        allowed.is_empty(),
        "expected Postgres to refuse each of these to the runtime role {:?}, but it allowed:\n{}",
        db.runtime,
        allowed.join("\n")
    );
    assert_eq!(
        db.fingerprint(),
        before,
        "expected the own database unchanged after the runtime role's attempts"
    );
    // Rows, it may change.
    let rows = db.as_runtime(&[
        "INSERT INTO items (title) VALUES ('as the runtime role') RETURNING written_by".to_owned(),
        "UPDATE seeded SET label = label WHERE id = 1".to_owned(),
        "DELETE FROM items".to_owned(),
        "SELECT nextval('items_id_seq')".to_owned(),
    ]);
    for result in &rows {
        assert!(
            result.is_ok(),
            "expected the runtime role to read and change rows, got {result:?}"
        );
    }
}

#[test]
fn application_code_cannot_change_the_structure_through_the_program() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let before = db.fingerprint();
    let mut reached_the_database = BTreeSet::new();
    for actor in ACTORS {
        let mut calls: Vec<Value> = Vec::new();
        for statement in forbidden_statements(&db) {
            calls.push(own_execute(actor, &statement));
            calls.push(own_query(actor, &statement));
        }
        // Past the statement check: a SELECT of the migration's own
        // function, which runs the statement as the runtime role.
        for statement in runnable_by_run_sql(&db) {
            let call = format!("select run_sql({})", literal(&statement));
            calls.push(own_execute(actor, &call));
            calls.push(own_execute(
                actor,
                &format!("with h as ({call} as done) select done from h"),
            ));
        }
        let (_, answers) = own.run(&ann(), "board", &calls);
        assert_eq!(answers.len(), calls.len());
        for (call, answer) in calls.iter().zip(&answers) {
            if let Some(code) = assert_did_not_run(answer, &format!("{call} as the {actor}")) {
                reached_the_database.insert(code);
            }
        }
        assert_eq!(
            db.fingerprint(),
            before,
            "expected the own database unchanged after every attempt as the {actor}"
        );
    }
    assert!(
        reached_the_database.contains("42501"),
        "expected attempts that passed the statement check to fail in the database with insufficient privilege (42501); the SQLSTATEs seen were {reached_the_database:?}"
    );
    // The same function, on a row: allowed, which shows the attempts failed
    // for what they tried, not for the way they were made.
    let (_, answer) = own.one(
        &ann(),
        "board",
        &own_execute(
            "application",
            "select run_sql('insert into notes (body) values (''through run_sql'')') as done",
        ),
    );
    assert_eq!(answer["ok"]["rows"], json!([[1]]), "{answer}");
    assert_eq!(db.column("SELECT body FROM notes"), vec!["through run_sql"]);
}

/// Fails unless every session ran as the runtime role, `current_user` and
/// `session_user`, and was labelled with this actor.
fn assert_all_ran_as_runtime(sessions: &[Session], db: &OwnDatabase, actor: &str, context: &str) {
    let wrong: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.current_user != db.runtime
                || s.session_user != db.runtime
                || s.vm_actor.as_deref() != Some(actor)
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{context}: expected every operation as the {actor} to run as {} (current_user and session_user) with vm.actor {actor:?}, but these did not:\n{wrong:#?}",
        db.runtime
    );
}

#[test]
fn every_own_database_operation_runs_as_the_runtime_role_for_both_actors() {
    let own = Own::start();
    let db = own.install("board", &standard());
    // Attempts to become another role, direct and past the check.
    let mut attempts = Vec::new();
    for role in [
        db.owner.clone(),
        "none".to_owned(),
        blackbox::postgres::SUPERUSER.to_owned(),
    ] {
        attempts.push(format!("set role {}", ident(&role)));
        attempts.push(format!(
            "select set_config('role', {}, false)",
            literal(&role)
        ));
        attempts.push(format!(
            "select run_sql({})",
            literal(&format!("SET ROLE {}", ident(&role)))
        ));
        attempts.push(format!(
            "select run_sql({})",
            literal(&format!("SET SESSION AUTHORIZATION {}", ident(&role)))
        ));
        attempts.push(format!(
            "select * from ts_stat($vm$select to_tsvector('simple', 'set ' || set_config('role', {}, false))$vm$) limit 1",
            literal(&role)
        ));
    }
    for actor in ACTORS {
        let calls: Vec<Value> = attempts
            .iter()
            .flat_map(|sql| [own_query(actor, sql), own_execute(actor, sql)])
            .collect();
        let (_, answers) = own.run(&ann(), "board", &calls);
        for answer in &answers {
            if let Some(rows) = answer["ok"]["rows"].as_array() {
                let text = json!(rows).to_string();
                assert!(
                    !text.contains(&db.owner),
                    "expected no attempt to report the owner role, got {answer}"
                );
            }
        }
    }
    // Many operations at once, as both actors, through query and execute,
    // so that they take every pooled connection.
    for actor in ACTORS {
        for (operation, sql) in [
            ("query", observe_select(100)),
            ("execute", observe_insert(100)),
        ] {
            let requests: Vec<(User, &str, Value)> = (0..4)
                .map(|i| {
                    (
                        User::new(&format!("u{i}")),
                        "board",
                        json!({
                            "calls": vec![op(operation, &json!({ "own": true, "as": actor, "sql": sql })); 6],
                            "mode": "together",
                        }),
                    )
                })
                .collect();
            let sessions: Vec<Session> = own
                .at_once(&requests)
                .into_iter()
                .flat_map(|(_, answers)| answers)
                .map(|answer| {
                    Session::from_answer(&answer).unwrap_or_else(|| {
                        panic!("expected an observation of the session from {operation} as the {actor}, got {answer}")
                    })
                })
                .collect();
            assert_all_ran_as_runtime(
                &sessions,
                &db,
                actor,
                &format!("{operation} as the {actor}, 24 at once"),
            );
        }
    }
}

#[test]
fn the_runtime_role_cannot_connect_to_other_databases_and_no_other_application_s_roles_to_its_own()
{
    let own = Own::start();
    let board = own.install("board", &standard());
    let ledger = own.install("ledger", &[first_tables()]);
    let platform = own.vm.platform().name().to_owned();
    let mut wrong = Vec::new();
    // As Postgres's privileges say.
    for (role, database) in [
        (&board.runtime, &platform),
        (&board.runtime, &ledger.name),
        (&ledger.runtime, &platform),
        (&ledger.runtime, &board.name),
        (&ledger.owner, &board.name),
        (&board.owner, &ledger.name),
    ] {
        if board.has_database_privilege(role, database, "CONNECT") {
            wrong.push(format!("  {role} may CONNECT to {database}"));
        }
    }
    for (role, database) in [
        (&board.runtime, &board.name),
        (&ledger.runtime, &ledger.name),
    ] {
        if !board.has_database_privilege(role, database, "CONNECT") {
            wrong.push(format!(
                "  {role} may not CONNECT to its own database {database} (the harness's own check)"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected each Application's roles to connect to its own database alone, but Postgres says:\n{}",
        wrong.join("\n")
    );
    // By trying to log in, with passwords the harness sets now (last: the
    // program's new connections as these roles would fail from here on).
    let board_pw = board.set_known_password(&board.runtime);
    let ledger_pw = ledger.set_known_password(&ledger.runtime);
    assert_eq!(
        try_login(&board.runtime, &board_pw, &board.name),
        Ok(()),
        "the harness's own check: the runtime role logs in to its own database"
    );
    for (role, password, database) in [
        (&board.runtime, &board_pw, &platform),
        (&board.runtime, &board_pw, &ledger.name),
        (&ledger.runtime, &ledger_pw, &board.name),
        (&ledger.runtime, &ledger_pw, &platform),
    ] {
        let tried = try_login(role, password, database);
        match &tried {
            Err(e) if e.code == "42501" => {}
            _ => wrong.push(format!(
                "  {role} logging in to {database}: expected permission denied (42501), got {tried:?}"
            )),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_runtime_role_cannot_connect_to_postgres_or_template1() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let mut wrong = Vec::new();
    for database in ["postgres", "template1"] {
        if db.has_database_privilege(&db.runtime, database, "CONNECT") {
            wrong.push(format!(
                "  has_database_privilege says it may CONNECT to {database}"
            ));
        }
    }
    let password = db.set_known_password(&db.runtime);
    for database in ["postgres", "template1"] {
        let tried = try_login(&db.runtime, &password, database);
        if !matches!(&tried, Err(e) if e.code == "42501") {
            wrong.push(format!(
                "  logging in to {database}: expected permission denied (42501), got {tried:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected the runtime role {:?} to connect to no other database of the instance, but:\n{}",
        db.runtime,
        wrong.join("\n")
    );
}

#[test]
fn the_runtime_role_cannot_make_a_temporary_table_even_through_a_function() {
    let own = Own::start();
    let db = own.install("board", &standard());
    for actor in ACTORS {
        let (_, answers) = own.run(
            &ann(),
            "board",
            &[
                own_execute(actor, "select run_sql('create temp table t_through_run_sql (x int)')"),
                own_execute(
                    actor,
                    "select count(*)::int4 as n from pg_class where relnamespace = pg_my_temp_schema()",
                ),
            ],
        );
        assert_eq!(
            assert_did_not_run(&answers[0], "a temporary table through run_sql").as_deref(),
            Some("42501"),
            "expected Postgres to refuse a temporary table to the runtime role as the {actor}, got {}",
            answers[0]
        );
        assert_eq!(answers[1]["ok"]["rows"], json!([[0]]), "{}", answers[1]);
    }
    assert!(
        db.has_database_privilege(&db.runtime, &db.name, "CONNECT")
            && !db.has_database_privilege(&db.runtime, &db.name, "TEMPORARY"),
        "expected the runtime role to connect to its own database and hold no TEMPORARY there"
    );
}
