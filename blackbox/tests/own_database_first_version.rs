//! An Application's first Version creates its own database (CONTRACT.md,
//! "The Application's own database"; ticket "An Application's own
//! database: created with its first Version, changed only by migrations",
//! first and last acceptance criteria).
//!
//! The database is there after the first Version, with or without
//! migrations, named `<p>_app_<n>` and commented `vm application <name>`,
//! with its two roles; the migrations' tables are there and owned by the
//! owner role. Application code reads and writes rows as the User and as
//! the Application; the Operation log says which, with `own_database` true,
//! `resource` null and the User in flight either way; the identity label
//! carries `vm.actor`. A User who holds no Grant on anything can use it.
//! The Version's record lists its migrations.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::api::sha256_hex;
use blackbox::own_database::{
    ACTORS, FIXTURE, Own, comment_of, first_tables, own_databases_of, own_execute, own_query,
    standard, version_body_with_raw_migrations,
};
use blackbox::scenario::{Expected, assert_entry, assert_executed, assert_rows, lines};
use blackbox::vm::fixture_bundle;
use serde_json::{Value, json};

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

#[test]
fn the_first_version_creates_the_own_database_named_and_commented_as_the_contract_says() {
    let own = Own::start();
    own.create("board");
    assert!(
        own.find_own_database("board").is_none(),
        "expected no own database before the first Version, found {:?}",
        own_databases_of(own.vm.platform())
    );
    own.send_ok("board", &standard());
    let db = own.own_database("board");
    let p = own.vm.platform().name();
    let n = db
        .name
        .strip_prefix(&format!("{p}_app_"))
        .expect("the name starts with <p>_app_");
    assert!(
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        "expected the own database to be named {p}_app_<n>, got {:?}",
        db.name
    );
    assert_eq!(
        db.rows(&format!(
            "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = '{}'",
            db.name
        )),
        vec![vec![Some(comment_of("board"))]],
        "expected the own database's comment to be `vm application board`"
    );
    for role in [&db.owner, &db.runtime] {
        assert!(
            db.role_exists(role),
            "expected the role {role:?} beside the own database {:?}",
            db.name
        );
    }
    for table in ["items", "notes", "seeded"] {
        assert!(
            db.has_table(table),
            "expected the table {table:?} the first migration made, got the tables {:?}",
            db.tables()
        );
    }
    assert_eq!(
        db.column("SELECT id || ':' || label FROM seeded"),
        vec!["1:seeded by 0000"],
        "expected the row the first migration inserted"
    );
    let not_owned = db.objects_not_owned_by_the_owner();
    assert!(
        not_owned.is_empty(),
        "expected the owner role {:?} to own every object in the own database, but these are owned by others:\n{}",
        db.owner,
        not_owned.join("\n")
    );
    let functions = db.column(
        "SELECT p.proname || ' ' || pg_get_userbyid(p.proowner) FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public' ORDER BY 1",
    );
    assert_eq!(
        functions,
        vec![
            format!("leave_session_state {}", db.owner),
            format!("run_sql {}", db.owner)
        ],
        "expected the second migration's functions, owned by the owner role"
    );
    // A second Application gets an own database of its own, under another
    // number.
    own.install("ledger", &[first_tables()]);
    let other = own.own_database("ledger");
    assert_ne!(other.name, db.name, "two Applications, one database");
    assert_eq!(
        own_databases_of(own.vm.platform()).len(),
        2,
        "expected one own database per Application"
    );
}

#[test]
fn a_first_version_without_migrations_also_creates_the_own_database() {
    let own = Own::start();
    // The field left out, and an empty list.
    own.create("bare");
    let sent = own.api().send_bundle("bare", &fixture_bundle(FIXTURE));
    sent.expect_status(201);
    own.create("empty");
    let sent = own
        .api()
        .send_version("empty", &version_body_with_raw_migrations(&json!([])));
    sent.expect_status(201);
    for name in ["bare", "empty"] {
        let db = own.own_database(name);
        assert!(
            db.tables().is_empty(),
            "expected no tables in the own database of {name:?}, which has no migrations, got {:?}",
            db.tables()
        );
        assert!(db.role_exists(&db.owner) && db.role_exists(&db.runtime));
        // Application code reaches it, as either actor.
        for actor in ACTORS {
            let (_, answer) = own.one(&ann(), name, &own_query(actor, "select 41 + 1 as n"));
            assert_rows(
                &answer,
                &["n"],
                &json!([[42]]),
                &format!(
                    "a query as {actor} on the own database of {name:?}, made with no migration"
                ),
            );
        }
    }
}

#[test]
fn application_code_reads_and_writes_rows_as_the_user_and_as_the_application() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let (_, answers) = own.run(
        &ann(),
        "board",
        &[
            own_execute(
                "user",
                "insert into items (title) values ('by the user') returning title, written_by, session_login",
            ),
            own_execute(
                "application",
                "insert into items (title) values ('by the application') returning title, written_by, session_login",
            ),
            own_query("user", "select title from items order by id"),
            own_query("application", "select title from items order by id"),
            own_execute(
                "application",
                "update items set title = title || ', changed' where title = 'by the user'",
            ),
            own_execute("user", "delete from items where title = 'by the application'"),
            own_query("application", "select title from items order by id"),
        ],
    );
    let runtime = &db.runtime;
    assert_executed(
        &answers[0],
        &["title", "written_by", "session_login"],
        &json!([["by the user", runtime, runtime]]),
        1,
        "an insert as the User, written as the runtime role",
    );
    assert_executed(
        &answers[1],
        &["title", "written_by", "session_login"],
        &json!([["by the application", runtime, runtime]]),
        1,
        "an insert as the Application, written as the runtime role",
    );
    for (i, actor) in [(2, "user"), (3, "application")] {
        assert_rows(
            &answers[i],
            &["title"],
            &json!([["by the user"], ["by the application"]]),
            &format!("a read as {actor} sees both rows"),
        );
    }
    assert_executed(
        &answers[4],
        &[],
        &json!([]),
        1,
        "an update as the Application",
    );
    assert_executed(&answers[5], &[], &json!([]), 1, "a delete as the User");
    assert_rows(
        &answers[6],
        &["title"],
        &json!([["by the user, changed"]]),
        "what is left, read as the Application",
    );
    assert_eq!(
        db.column("SELECT title || ' / ' || written_by FROM items ORDER BY id"),
        vec![format!("by the user, changed / {runtime}")],
        "expected the own database itself to hold what the operations committed"
    );
}

#[test]
fn the_operation_log_says_which_actor_and_names_the_user_in_flight_for_both() {
    let own = Own::start();
    own.install("board", &standard());
    for actor in ACTORS {
        for (operation, sql, rows) in [
            ("query", "select id from seeded", 1),
            (
                "execute",
                "insert into notes (body) values ('logged') returning id",
                1,
            ),
        ] {
            let call = if operation == "query" {
                own_query(actor, sql)
            } else {
                own_execute(actor, sql)
            };
            let (response, answer) = own.one(&ann(), "board", &call);
            assert!(answer["ok"].is_object(), "{operation} as {actor}: {answer}");
            let id = response.request_id();
            let entry = own.entry_of(&response);
            assert_entry(
                &entry,
                &Expected {
                    rows: Some(rows),
                    ..Expected::own(&id, "ann", "board", actor, operation)
                },
                &format!("the entry of a {operation} as {actor} on the own database"),
            );
            assert!(
                entry.get("resource").is_some(),
                "expected the entry to carry `resource`, null for the own database, got {entry}"
            );
        }
    }
}

#[test]
fn the_label_has_vm_actor_and_vm_user_is_the_user_in_flight_for_both_actors() {
    let own = Own::start();
    own.install("board", &standard());
    let label = "select current_setting('vm.user') as u, current_setting('vm.application') as a, \
                 current_setting('vm.request') as r, current_setting('vm.actor') as actor";
    for actor in ACTORS {
        for call in [own_query(actor, label), own_execute(actor, label)] {
            let (response, answer) = own.one(&ann(), "board", &call);
            assert_eq!(
                answer["ok"]["rows"],
                json!([["ann", "board", response.request_id(), actor]]),
                "expected the label (vm.user, vm.application, vm.request, vm.actor) of {call} to name the User in flight, the Application, the request and the actor, got {answer}"
            );
        }
    }
}

#[test]
fn a_user_who_holds_no_grant_at_all_can_use_the_own_database() {
    // No Resource is registered and no Grant exists anywhere; the User is
    // in no Group.
    let own = Own::start();
    let db = own.install("board", &standard());
    let nobody = User::new("nobody");
    let (response, answers) = own.run(
        &nobody,
        "board",
        &[
            own_execute(
                "user",
                "insert into items (title) values ('from nobody') returning id",
            ),
            own_query("user", "select title from items"),
            own_execute(
                "application",
                "insert into notes (body) values ('for nobody') returning body",
            ),
            own_query("application", "select body from notes"),
        ],
    );
    for (i, answer) in answers.iter().enumerate() {
        assert!(
            answer["ok"].is_object(),
            "expected call {i} of a User with no Grant on the own database to run, got {answer}"
        );
    }
    assert_rows(
        &answers[1],
        &["title"],
        &json!([["from nobody"]]),
        "a read by a User with no Grant",
    );
    assert_eq!(db.column("SELECT title FROM items"), vec!["from nobody"]);
    let entries = own.entries_of(&response);
    assert_eq!(entries.len(), 4, "{}", lines(&entries));
    assert!(
        entries
            .iter()
            .all(|e| e["outcome"] == "allowed" && e["user"] == "nobody"),
        "expected four allowed entries naming the User, got:\n{}",
        lines(&entries)
    );
    assert_eq!(
        own.vm
            .api_as(&blackbox::scenario::administrator())
            .list_resources()
            .json()["resources"],
        json!([]),
        "the test's own check: no Resource is registered"
    );
}

#[test]
fn the_version_record_lists_its_migrations_with_their_sha256() {
    let own = Own::start();
    own.create("board");
    let migrations = standard();
    let record = own.send_ok("board", &migrations);
    let want: Vec<Value> = migrations
        .iter()
        .map(|m| json!({ "name": m.name, "sha256": sha256_hex(m.sql.as_bytes()) }))
        .collect();
    let got: Vec<Value> = record["migrations"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("expected the Version's record to carry `migrations`, got {record}")
        })
        .iter()
        .map(|m| {
            json!({
                "name": m["name"],
                "sha256": m["sha256"].as_str().map(str::to_ascii_lowercase),
            })
        })
        .collect();
    assert_eq!(
        got, want,
        "expected `migrations` to list each migration's name and the SHA-256 of its text, in order, in {record}"
    );
    assert_eq!(
        own.api().get_version("board", 1).expect_status(200).json(),
        record,
        "expected GET of the Version to answer what the 201 did"
    );
    // A Version sent without the field lists none.
    own.create("bare");
    let bare = own
        .api()
        .send_bundle("bare", &fixture_bundle(FIXTURE))
        .expect_status(201)
        .json();
    assert_eq!(
        bare["migrations"],
        json!([]),
        "expected a Version sent without migrations to list none, in {bare}"
    );
}
