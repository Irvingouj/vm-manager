//! The platform needs no superuser (CONTRACT.md, "The Application's own
//! database: What it looks like in Postgres": it needs only what
//! `VM_DATABASE_URL`'s role was promised, creating databases and roles).
//!
//! The main path, with a platform role that is `LOGIN CREATEDB CREATEROLE`
//! and nothing more: the program creates its platform database, an
//! Application, a first Version with migrations (its own database, its two
//! roles, the runtime role's rights), operations as both actors, a second
//! Version with one more migration, a failing one, and a second
//! Application whose roles cannot reach the first one's database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::own_database::{
    ACTORS, Migration, Own, first_tables, literal, more_tables, own_execute, own_query, standard,
};
use blackbox::postgres::shared;
use blackbox::scenario::{Expected, assert_entry, assert_executed, assert_rows};
use blackbox::sql::Sql;
use blackbox::{Platform, User, VmOptions, assert_api_error};
use serde_json::json;

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

#[test]
fn the_main_path_works_with_a_platform_role_that_only_creates_databases_and_roles() {
    let platform = Platform::fresh_with_limited_role();
    let instance = Sql::connect(&shared().url("postgres")).unwrap();
    assert_eq!(
        instance.batch(&format!(
            "SELECT rolsuper::text, rolcreatedb::text, rolcreaterole::text FROM pg_roles WHERE rolname = {}",
            literal(platform.role())
        )),
        vec![vec![
            Some("false".to_owned()),
            Some("true".to_owned()),
            Some("true".to_owned())
        ]],
        "the harness's own check: the platform's role is no superuser and may create databases and roles"
    );
    let own = Own::start_with(&VmOptions::new().platform(&platform));
    let db = own.install("board", &standard());
    assert!(db.role_exists(&db.owner) && db.role_exists(&db.runtime));
    for table in ["items", "notes", "seeded"] {
        assert!(db.has_table(table), "got {:?}", db.tables());
    }
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
    // Operations as both actors.
    for actor in ACTORS {
        let (response, answers) = own.run(
            &ann(),
            "board",
            &[
                own_execute(
                    actor,
                    &format!("insert into items (title) values ('by the {actor}') returning title, written_by"),
                ),
                own_query(actor, &format!("select title from items where title = 'by the {actor}'")),
            ],
        );
        assert_executed(
            &answers[0],
            &["title", "written_by"],
            &json!([[format!("by the {actor}"), db.runtime]]),
            1,
            &format!("an insert as the {actor} with a platform role that is no superuser"),
        );
        assert_rows(
            &answers[1],
            &["title"],
            &json!([[format!("by the {actor}")]]),
            &format!("a read as the {actor}"),
        );
        let id = response.request_id();
        let entries = own.entries_of(&response);
        assert_eq!(entries.len(), 2);
        assert_entry(
            &entries[0],
            &Expected {
                rows: Some(1),
                ..Expected::own(&id, "ann", "board", actor, "execute")
            },
            "the entry of an insert with a platform role that is no superuser",
        );
    }
    // A later migration, and the runtime role's rights in its table.
    let mut list = standard();
    list.push(more_tables());
    let record = own.send_ok("board", &list);
    assert_eq!(record["number"], 2);
    let (_, answer) = own.one(
        &ann(),
        "board",
        &own_execute(
            "application",
            "insert into extras (note) values ('later') returning note",
        ),
    );
    assert_executed(
        &answer,
        &["note"],
        &json!([["later"]]),
        1,
        "a later migration's table",
    );
    // A failing migration changes nothing.
    let before = db.fingerprint();
    let mut broken = list.clone();
    broken.push(Migration::new(
        "0003_broken",
        &[
            r#"CREATE TABLE "half" ("x" integer);"#,
            r#"ALTER TABLE "nowhere" ADD COLUMN "y" integer;"#,
        ],
    ));
    assert_api_error(&own.send("board", &broken), 422, "migration-failed");
    assert_eq!(db.fingerprint(), before);
    assert_eq!(own.version_numbers("board"), vec![1, 2]);
    // A second Application: its roles cannot reach the first one's
    // database, nor either runtime role the platform database.
    let ledger = own.install("ledger", &[first_tables()]);
    for (role, database) in [
        (&ledger.runtime, &db.name),
        (&ledger.owner, &db.name),
        (&db.runtime, &ledger.name),
        (&db.runtime, &platform.name().to_owned()),
        (&ledger.runtime, &platform.name().to_owned()),
    ] {
        assert!(
            !db.has_database_privilege(role, database, "CONNECT"),
            "expected {role} not to be allowed to CONNECT to {database}"
        );
    }
    // The privileges hold for the runtime role in Postgres itself.
    let statements: Vec<String> = [
        "CREATE TABLE sneaky (x int)",
        "TRUNCATE items",
        "DROP TABLE seeded",
        "CREATE TEMP TABLE t (x int)",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let results = db.as_runtime(&statements);
    for (statement, result) in statements.iter().zip(&results) {
        assert!(
            matches!(result, Err(e) if e.code == "42501"),
            "expected Postgres to refuse {statement:?} to the runtime role, got {result:?}"
        );
    }
}
