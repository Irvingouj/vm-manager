//! Checks of the harness itself for the own database (`own_database`), with
//! no program: an own database made by hand on the harness's Postgres the
//! way CONTRACT.md describes one ("The Application's own database: What it
//! looks like in Postgres"), so that the harness's queries are known to
//! work, and the tests' demands are known to be satisfiable by Postgres's
//! ordinary privileges, before any program makes one.
//!
//! These pass before the program has own databases: they test the harness,
//! not the program.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::Platform;
use blackbox::own_database::{
    BREAKPOINT, OwnDatabase, comment_of, forbidden_statements, ident, literal, more_tables,
    own_databases_of, runnable_by_run_sql, standard, try_login,
};
use blackbox::postgres::shared;
use blackbox::sql::Sql;

/// An own database made by hand for `application`, as number 1 of a fresh
/// platform database's name: the database owned by the owner role and
/// commented, CONNECT for its two roles only, the platform's schema `vm`
/// (owned by the owner, usable by the runtime role), the runtime role
/// given rows in what the owner makes, and the standard migrations applied
/// as the owner. Returns it and the runtime role's password.
fn made_by_hand(application: &str) -> (OwnDatabase, String) {
    let platform = Platform::fresh();
    let name = format!("{}_app_1", platform.name());
    let owner = format!("{name}_owner");
    let runtime = format!("{name}_runtime");
    let runtime_pw = format!("pw-rt-{}", blackbox::shared_database::token());
    let instance = Sql::connect(&shared().url("postgres")).unwrap();
    instance.batch(&format!(
        "CREATE ROLE {o} LOGIN PASSWORD 'pw-owner-unused';
         CREATE ROLE {r} LOGIN PASSWORD {rp};",
        o = ident(&owner),
        r = ident(&runtime),
        rp = literal(&runtime_pw),
    ));
    instance.batch(&format!(
        "CREATE DATABASE {} OWNER {}",
        ident(&name),
        ident(&owner)
    ));
    instance.batch(&format!(
        "COMMENT ON DATABASE {n} IS {c};
         REVOKE ALL ON DATABASE {n} FROM PUBLIC;
         GRANT CONNECT ON DATABASE {n} TO {o}, {r};",
        n = ident(&name),
        c = literal(&comment_of(application)),
        o = ident(&owner),
        r = ident(&runtime),
    ));
    let db = Sql::connect(&shared().url(&name)).unwrap();
    db.batch(&format!(
        "REVOKE ALL ON SCHEMA public FROM PUBLIC;
         GRANT USAGE ON SCHEMA public TO {r};
         CREATE SCHEMA vm AUTHORIZATION {o};
         GRANT USAGE ON SCHEMA vm TO {r};
         ALTER DEFAULT PRIVILEGES FOR ROLE {o} IN SCHEMA public
           GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {r};
         ALTER DEFAULT PRIVILEGES FOR ROLE {o} IN SCHEMA public
           GRANT USAGE ON SEQUENCES TO {r};
         SET ROLE {o};",
        o = ident(&owner),
        r = ident(&runtime),
    ));
    let mut migrations = standard();
    migrations.push(more_tables());
    for migration in &migrations {
        for statement in migration.sql.split(BREAKPOINT) {
            db.batch(statement);
        }
    }
    drop(db);
    let found = OwnDatabase::find(&platform, application).expect("found by name and comment");
    assert_eq!(found.name, name);
    assert_eq!(found.owner, owner);
    assert_eq!(found.runtime, runtime);
    assert_eq!(own_databases_of(&platform).len(), 1);
    (found, runtime_pw)
}

#[test]
fn an_own_database_made_as_the_contract_says_is_found_and_read() {
    let (db, _) = made_by_hand("sim");
    assert_eq!(
        db.tables(),
        vec![
            "public.extras",
            "public.items",
            "public.notes",
            "public.seeded"
        ]
    );
    assert!(
        db.objects_not_owned_by_the_owner().is_empty(),
        "{:?}",
        db.objects_not_owned_by_the_owner()
    );
    assert!(
        db.owned_by(&db.runtime).is_empty(),
        "{:?}",
        db.owned_by(&db.runtime)
    );
    assert!(!db.owned_by(&db.owner).is_empty());
    assert!(OwnDatabase::find(&Platform::fresh(), "sim").is_none());
    // The platform's schema `vm` is not the Application's: its tables are
    // left out of the Application's tables and fingerprint, and what the
    // runtime role may do there is reported.
    let before = db.fingerprint();
    let platform_before = db.platform_schema();
    db.superuser().batch(&format!(
        "CREATE TABLE vm.platform_notes (x int);
         ALTER TABLE vm.platform_notes OWNER TO {o};
         INSERT INTO vm.platform_notes VALUES (1);
         GRANT SELECT ON vm.platform_notes TO {r};",
        o = ident(&db.owner),
        r = ident(&db.runtime),
    ));
    assert_eq!(
        db.fingerprint(),
        before,
        "vm is left out of the fingerprint"
    );
    assert!(
        !db.tables().iter().any(|t| t.starts_with("vm.")),
        "{:?}",
        db.tables()
    );
    assert!(db.objects_not_owned_by_the_owner().is_empty());
    assert_ne!(
        db.platform_schema(),
        platform_before,
        "a table in vm is seen"
    );
    assert!(db.powers_in_platform_schema(&db.runtime).is_empty());
    db.superuser().batch(&format!(
        "GRANT INSERT ON vm.platform_notes TO {}",
        ident(&db.runtime)
    ));
    assert_eq!(
        db.powers_in_platform_schema(&db.runtime),
        vec!["INSERT on vm.platform_notes"]
    );
}

#[test]
fn the_fingerprint_is_stable_and_sees_rows_structure_and_privileges_change() {
    let (db, _) = made_by_hand("fp");
    let before = db.fingerprint();
    assert_eq!(db.fingerprint(), before, "the same twice");
    for change in [
        "INSERT INTO seeded VALUES (9, 'nine')",
        "ALTER TABLE items ADD COLUMN later text",
        "GRANT TRUNCATE ON seeded TO PUBLIC",
        "CREATE INDEX later_idx ON items (title)",
        "SELECT nextval('extras_id_seq')",
        "SELECT lo_from_bytea(0, 'x')",
        "CREATE TABLE pg_temp.t (x int)",
    ] {
        let sql = db.superuser();
        let was = db.fingerprint();
        sql.batch(change);
        // A temporary table lives as long as its session: look while the
        // session is open.
        let now = db.fingerprint();
        assert_ne!(now, was, "the fingerprint did not see {change:?}");
    }
}

#[test]
fn postgres_refuses_every_forbidden_statement_to_a_runtime_role_set_up_as_the_contract_says() {
    let (db, _) = made_by_hand("rt");
    let before = db.fingerprint();
    let statements = forbidden_statements(&db);
    let results = db.as_runtime(&statements);
    let allowed: Vec<&String> = statements
        .iter()
        .zip(&results)
        .filter(|(_, r)| r.is_ok())
        .map(|(s, _)| s)
        .collect();
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
    assert_eq!(db.fingerprint(), before);
    let through: Vec<String> = runnable_by_run_sql(&db)
        .iter()
        .map(|s| format!("SELECT run_sql({})", literal(s)))
        .collect();
    let results = db.as_runtime(&through);
    for (statement, result) in through.iter().zip(&results) {
        assert!(
            matches!(result, Err(e) if e.code == "42501"),
            "{statement}: {result:?}"
        );
    }
    assert_eq!(db.fingerprint(), before);
    let rows = db.as_runtime(&[
        "INSERT INTO items (title) VALUES ('x')".to_owned(),
        "INSERT INTO extras (note) VALUES ('x')".to_owned(),
        "SELECT run_sql('DELETE FROM items')".to_owned(),
    ]);
    assert!(rows.iter().all(Result::is_ok), "{rows:?}");
}

#[test]
fn logins_and_connect_privileges_are_read_from_postgres() {
    let (db, runtime_pw) = made_by_hand("login");
    assert_eq!(try_login(&db.runtime, &runtime_pw, &db.name), Ok(()));
    let wrong = try_login(&db.runtime, "not-the-password", &db.name);
    assert!(matches!(&wrong, Err(e) if e.code == "28P01"), "{wrong:?}");
    let (other, other_pw) = made_by_hand("login-other");
    assert!(!db.has_database_privilege(&other.runtime, &db.name, "CONNECT"));
    let tried = try_login(&other.runtime, &other_pw, &db.name);
    assert!(matches!(&tried, Err(e) if e.code == "42501"), "{tried:?}");
    let password = db.set_known_password(&db.runtime);
    assert_eq!(try_login(&db.runtime, &password, &db.name), Ok(()));
}
