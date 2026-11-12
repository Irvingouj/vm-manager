//! Later Versions and their migrations (CONTRACT.md, "The Application's own
//! database: Migrations travel with the Version", "What happens when a
//! Version is sent"; ticket "An Application's own database", second
//! acceptance criterion).
//!
//! A Version's list is the whole history and must begin with exactly the
//! previous Version's list: one more migration applies only that one (the
//! earlier ones would fail if run again, and rows written meanwhile are
//! still there); the same list changes nothing; a list that does not
//! continue the earlier one is `409 migrations-diverged` and changes
//! nothing; a malformed list is `400 malformed` and changes nothing. Tables
//! made by a later migration are usable at once by both actors. A restart
//! keeps the own database and which migrations were applied.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::own_database::{
    ACTORS, Migration, Own, first_tables, helpers, more_tables, own_execute, own_query, standard,
    version_body_with_raw_migrations,
};
use blackbox::scenario::{assert_executed, assert_rows};
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

/// Writes a row into `items` as each actor, and returns their titles.
fn write_rows(own: &Own, application: &str, tag: &str) -> Vec<String> {
    let titles: Vec<String> = ACTORS
        .iter()
        .map(|actor| format!("{tag} by the {actor}"))
        .collect();
    let calls: Vec<Value> = ACTORS
        .iter()
        .zip(&titles)
        .map(|(actor, title)| {
            own_execute(
                actor,
                &format!("insert into items (title) values ('{title}') returning id"),
            )
        })
        .collect();
    let (_, answers) = own.run(&ann(), application, &calls);
    for answer in &answers {
        assert!(answer["ok"].is_object(), "writing a row: {answer}");
    }
    titles
}

#[test]
fn a_second_version_with_one_more_migration_applies_only_that_one() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let titles = write_rows(&own, "board", "under Version 1");
    let mut list = standard();
    list.push(more_tables());
    // `first_tables` would fail if it ran again (its tables exist, its row's
    // key is taken): a 201 shows only the new migration ran.
    let record = own.send_ok("board", &list);
    assert_eq!(record["number"], 2, "in {record}");
    assert!(
        db.has_table("extras"),
        "expected the new migration's table, got {:?}",
        db.tables()
    );
    assert_eq!(
        db.column("SELECT id || ':' || label FROM seeded ORDER BY id"),
        vec!["1:seeded by 0000", "2:seeded by 0002"],
        "expected the first migration's row once, and the new one's"
    );
    assert_eq!(
        db.column("SELECT title FROM items ORDER BY id"),
        titles,
        "expected the rows written under Version 1 to be there still"
    );
    assert_eq!(
        db.column(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'items' ORDER BY ordinal_position"
        ),
        vec!["id", "title", "written_by", "session_login", "extra"],
        "expected the new migration's column on items"
    );
    assert_eq!(own.serving(&ann(), "board"), 2);
    // And again: Version 3 adds one more, in two steps of two.
    let fourth = Migration::new(
        "0003_flags",
        &[r#"CREATE TABLE "flags" ("name" text PRIMARY KEY NOT NULL);"#],
    );
    list.push(fourth);
    own.send_ok("board", &list);
    assert!(db.has_table("flags"), "got {:?}", db.tables());
    assert_eq!(
        db.column("SELECT count(*) FROM seeded"),
        vec!["2"],
        "expected no earlier migration run again by Version 3"
    );
}

#[test]
fn several_pending_migrations_are_all_applied_in_order() {
    let own = Own::start();
    let db = own.install("board", &[first_tables()]);
    // `more_tables` adds a column to items; `0003` uses it, so the order
    // matters.
    let uses_it = Migration::new(
        "0003_uses_extra",
        &[
            r#"UPDATE "items" SET "extra" = 'filled';"#,
            r#"ALTER TABLE "items" ALTER COLUMN "extra" SET DEFAULT 'filled';"#,
        ],
    );
    own.send_ok(
        "board",
        &[first_tables(), helpers(), more_tables(), uses_it],
    );
    assert!(db.has_table("extras"), "got {:?}", db.tables());
    assert_eq!(
        db.column(
            "SELECT count(*) FROM pg_proc WHERE proname IN ('run_sql', 'leave_session_state')"
        ),
        vec!["2"],
        "expected the helpers migration applied too"
    );
    assert_eq!(
        db.column(
            "SELECT column_default FROM information_schema.columns WHERE table_name = 'items' AND column_name = 'extra'"
        ),
        vec!["'filled'::text"],
        "expected the last migration applied after the one it depends on"
    );
}

#[test]
fn a_version_with_the_same_list_changes_nothing_in_the_database() {
    let own = Own::start();
    let db = own.install("board", &standard());
    write_rows(&own, "board", "before");
    let before = db.fingerprint();
    let record = own.send_ok("board", &standard());
    assert_eq!(record["number"], 2);
    assert_eq!(
        db.fingerprint(),
        before,
        "expected a Version with the same migrations to change nothing in the own database"
    );
    assert_eq!(own.serving(&ann(), "board"), 2);
}

/// Lists that do not begin with exactly `standard() + more_tables()`.
fn diverging(previous: &[Migration]) -> Vec<(&'static str, Vec<Migration>)> {
    let changed_text = {
        let mut list = previous.to_vec();
        list[0] = list[0].with_sql(
            &list[0]
                .sql
                .replace("\"title\" text", "\"title\" varchar(80)"),
        );
        list
    };
    let one_more_newline = {
        let mut list = previous.to_vec();
        list[1] = list[1].with_sql(&format!("{}\n", list[1].sql));
        list
    };
    let renamed = {
        let mut list = previous.to_vec();
        list[1] = list[1].renamed("0001_helpers_renamed");
        list
    };
    let removed_last = previous[..previous.len() - 1].to_vec();
    let removed_first = previous[1..].to_vec();
    let reordered = {
        let mut list = previous.to_vec();
        list.swap(1, 2);
        list
    };
    let inserted = {
        let mut list = previous.to_vec();
        list.insert(
            1,
            Migration::new("0000_b_between", &["CREATE TABLE \"between\" (x int);"]),
        );
        list
    };
    let changed_and_added = {
        let mut list = changed_text.clone();
        list.push(Migration::new(
            "0003_new",
            &["CREATE TABLE \"newer\" (x int);"],
        ));
        list
    };
    vec![
        ("the first migration's text changed", changed_text),
        (
            "a newline added to the second migration's text",
            one_more_newline,
        ),
        ("the second migration renamed", renamed),
        ("the last migration removed", removed_last),
        ("the first migration removed", removed_first),
        ("two migrations swapped", reordered),
        ("a migration inserted before earlier ones", inserted),
        (
            "an earlier text changed and a new one added",
            changed_and_added,
        ),
        ("an empty list", Vec::new()),
    ]
}

#[test]
fn a_list_that_does_not_continue_the_earlier_one_is_409_migrations_diverged_and_changes_nothing() {
    let own = Own::start();
    let mut previous = standard();
    previous.push(more_tables());
    let db = own.install("board", &previous);
    write_rows(&own, "board", "kept");
    let before = db.fingerprint();
    let application = own.api().application("board");
    let versions = own.api().versions("board");
    for (what, list) in diverging(&previous) {
        let sent = own.send("board", &list);
        assert_api_error(&sent, 409, "migrations-diverged");
        assert_eq!(
            db.fingerprint(),
            before,
            "expected nothing in the own database to change for a list with {what}"
        );
        assert_eq!(
            own.api().application("board"),
            application,
            "expected the Application unchanged after a list with {what}"
        );
        assert_eq!(
            own.api().versions("board"),
            versions,
            "expected no Version recorded for a list with {what}"
        );
    }
    // The field left out means an empty list: diverged too.
    let sent = own
        .api()
        .send_bundle("board", &blackbox::vm::fixture_bundle("operations.js"));
    assert_api_error(&sent, 409, "migrations-diverged");
    assert_eq!(
        db.fingerprint(),
        before,
        "after a Version without the field"
    );
    assert_eq!(own.api().versions("board"), versions);
    assert_eq!(own.serving(&ann(), "board"), 1);
    // A list that does continue it is still taken, as Version 2.
    let mut longer = previous.clone();
    longer.push(Migration::new(
        "0003_new",
        &["CREATE TABLE \"newer\" (x int);"],
    ));
    let record = own.send_ok("board", &longer);
    assert_eq!(
        record["number"], 2,
        "expected the next Version after the refused ones to be 2, in {record}"
    );
}

#[test]
fn the_earlier_list_is_the_previous_versions_not_the_first_ones() {
    let own = Own::start();
    let db = own.install("board", &[first_tables()]);
    own.send_ok("board", &standard());
    // Version 2's list was [first, helpers]: [first] alone no longer
    // continues it.
    let before = db.fingerprint();
    assert_api_error(
        &own.send("board", &[first_tables()]),
        409,
        "migrations-diverged",
    );
    assert_eq!(db.fingerprint(), before);
    assert_eq!(own.version_numbers("board"), vec![1, 2]);
}

/// Bodies whose `migrations` is malformed, with what is wrong.
fn malformed_lists() -> Vec<(&'static str, Value)> {
    let good = first_tables().to_json();
    vec![
        (
            "not an array",
            json!({ "0000_first_tables": "CREATE TABLE x (a int);" }),
        ),
        ("a string", json!("CREATE TABLE x (a int);")),
        (
            "an entry that is not an object",
            json!(["0000_first_tables"]),
        ),
        ("an entry without sql", json!([{ "name": "0000_x" }])),
        (
            "an entry without a name",
            json!([{ "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "sql that is not a string",
            json!([{ "name": "0000_x", "sql": 7 }]),
        ),
        (
            "a name that is not a string",
            json!([{ "name": 0, "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "an empty name",
            json!([{ "name": "", "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "a name of 201 characters",
            json!([{ "name": "a".repeat(201), "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "a name with a space",
            json!([{ "name": "0000 first", "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "a name with a dot",
            json!([{ "name": "0000_first.sql", "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "a name with a slash",
            json!([{ "name": "../0000_first", "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "a name with a letter that is not ASCII",
            json!([{ "name": "0000_fïrst", "sql": "CREATE TABLE x (a int);" }]),
        ),
        (
            "the same name twice",
            json!([good, { "name": "0000_first_tables", "sql": "CREATE TABLE y (a int);" }]),
        ),
    ]
}

#[test]
fn a_malformed_migrations_list_is_400_malformed_and_changes_nothing() {
    let own = Own::start();
    // As a first Version: nothing is recorded and no own database made.
    own.create("fresh");
    for (what, migrations) in malformed_lists() {
        let sent = own
            .api()
            .send_version("fresh", &version_body_with_raw_migrations(&migrations));
        assert_api_error(&sent, 400, "malformed");
        assert!(
            own.api().versions("fresh").is_empty(),
            "expected no Version recorded for migrations with {what}"
        );
        assert!(
            own.find_own_database("fresh").is_none(),
            "expected no own database made for a first Version whose migrations have {what}"
        );
    }
    // As a later Version: nothing changes.
    let db = own.install("board", &standard());
    let before = db.fingerprint();
    for (what, migrations) in malformed_lists() {
        // Each begins with nothing of the earlier list, but malformed is
        // checked first.
        let sent = own
            .api()
            .send_version("board", &version_body_with_raw_migrations(&migrations));
        assert_api_error(&sent, 400, "malformed");
        assert_eq!(db.fingerprint(), before, "after migrations with {what}");
        assert_eq!(
            own.version_numbers("board"),
            vec![1],
            "after migrations with {what}"
        );
    }
    // A key given twice in an entry.
    let twice = r#"{"bundle":"","declared_access":{"resources":{}},"git_commit":"0123456789abcdef0123456789abcdef01234567","migrations":[{"name":"0000_a","name":"0000_b","sql":"CREATE TABLE z (a int);"}]}"#;
    let sent = own.api().post_raw("/v1/applications/fresh/versions", twice);
    assert_api_error(&sent, 400, "malformed");
    assert!(own.find_own_database("fresh").is_none());
}

#[test]
fn names_of_letters_digits_underscores_and_hyphens_up_to_200_characters_are_taken() {
    let own = Own::start();
    let long = format!("{}{}", "0001-", "x".repeat(195));
    assert_eq!(long.len(), 200);
    let list = vec![
        Migration::new("A", &[r#"CREATE TABLE "a" (x int);"#]),
        Migration::new("0000_Mixed-Case_9", &[r#"CREATE TABLE "b" (x int);"#]),
        Migration::new(&long, &[r#"CREATE TABLE "c" (x int);"#]),
    ];
    own.create("board");
    let record = own.send_ok("board", &list);
    assert_eq!(
        record["migrations"]
            .as_array()
            .map(|m| m.iter().map(|e| e["name"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("A"), json!("0000_Mixed-Case_9"), json!(long)]),
        "in {record}"
    );
    let db = own.own_database("board");
    for table in ["a", "b", "c"] {
        assert!(db.has_table(table), "got {:?}", db.tables());
    }
}

#[test]
fn tables_made_by_a_later_migration_are_readable_and_writable_by_both_actors_at_once() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let mut list = standard();
    list.push(more_tables());
    own.send_ok("board", &list);
    for actor in ACTORS {
        let (_, answers) = own.run(
            &ann(),
            "board",
            &[
                own_execute(
                    actor,
                    &format!("insert into extras (note) values ('by the {actor}') returning note"),
                ),
                own_execute(
                    actor,
                    &format!("update items set extra = 'set by the {actor}' returning extra"),
                ),
                own_query(actor, "select note from extras order by id"),
                own_query(actor, "select label from seeded where id = 2"),
            ],
        );
        assert_executed(
            &answers[0],
            &["note"],
            &json!([[format!("by the {actor}")]]),
            1,
            &format!("an insert into the new table, using its sequence, as the {actor}"),
        );
        assert!(
            answers[1]["ok"].is_object(),
            "an update of the new column as the {actor}: {}",
            answers[1]
        );
        assert!(
            answers[2]["ok"]["row_count"].as_u64() >= Some(1),
            "{}",
            answers[2]
        );
        assert_rows(
            &answers[3],
            &["label"],
            &json!([["seeded by 0002"]]),
            &format!("a read of the new migration's row as the {actor}"),
        );
    }
    let (_, answer) = own.one(&ann(), "board", &own_execute("user", "delete from extras"));
    assert_executed(&answer, &[], &json!([]), 2, "a delete from the new table");
    assert_eq!(db.column("SELECT count(*) FROM extras"), vec!["0"]);
}

#[test]
fn a_restart_keeps_the_own_database_and_which_migrations_were_applied() {
    let own = Own::start();
    let db = own.install("board", &standard());
    let titles = write_rows(&own, "board", "before the restart");
    let before = db.fingerprint();
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    assert_eq!(
        own.own_database("board").name,
        db.name,
        "expected the same own database after a restart"
    );
    assert_eq!(
        db.fingerprint(),
        before,
        "expected a restart to change nothing in the own database"
    );
    let (_, answer) = own.one(
        &ann(),
        "board",
        &own_query("user", "select title from items order by id"),
    );
    assert_rows(
        &answer,
        &["title"],
        &json!(titles.iter().map(|t| json!([t])).collect::<Vec<_>>()),
        "the rows written before the restart, read after it",
    );
    // One more migration after the restart applies only that one.
    let mut list = standard();
    list.push(more_tables());
    let record = own.send_ok("board", &list);
    assert_eq!(record["number"], 2, "in {record}");
    assert!(db.has_table("extras"), "got {:?}", db.tables());
    assert_eq!(
        db.column("SELECT count(*) FROM seeded"),
        vec!["2"],
        "expected the earlier migrations not run again after the restart"
    );
    // And a list that does not continue the earlier one is still diverged.
    assert_api_error(&own.send("board", &standard()), 409, "migrations-diverged");
}
