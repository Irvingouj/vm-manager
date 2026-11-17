//! The stand-in's statements a pull answers and writes (CONTRACT.md, "The
//! local loop: one request as a test User: What a pull writes, from this
//! ticket on"; ticket "Local loop: run the Application on the Builder's
//! machine and send a request as a test User": no real row and no
//! credential of a real Resource reaches the Builder's machine, only its
//! structure).
//!
//! The shared database is the harness's fixture with the common shapes
//! (`blackbox/src/structure.rs`): enums, a domain, identity and serial
//! columns, a generated column, defaults, unique constraints, foreign keys,
//! a view, a materialized view, a partitioned table with partitions, a
//! second schema. The statements the API answers are run, each on its own,
//! in an empty database of the harness's own, and what they made is
//! compared with the shared database through the catalogs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blackbox::User;
use blackbox::local::{
    STANDIN_FIRST_LINE, standin_body, standin_file, standin_statements, statements_in_text,
};
use blackbox::project::{Tree, squashed};
use blackbox::scenario::{CONTACT, READERS};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use blackbox::structure::{IN_ANOTHER_ORDER, STOCK, Stock, body_with_structure};
use blackbox::tool::Machine;
use serde_json::json;

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

const MANIFEST: &str = r#"import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "stock-tool",
  description: "A project to pull into.",
  resources: { stock: { kind: "postgres", access: "read" } },
});
"#;

/// A project directory on the machine, with a `manifest.ts`.
fn project(machine: &Machine, name: &str) -> PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.ts"), MANIFEST).unwrap();
    dir
}

fn pull(machine: &Machine, dir: &Path, resource: &str) -> blackbox::tool::Run {
    machine.run(&[
        "pull",
        resource,
        "--project",
        dir.to_str().unwrap(),
        "--json",
    ])
}

/// Which of the four kinds of the contract's list a statement is: 1 a
/// schema or an enum, 2 a table, 3 a default, 4 a foreign key; `None` for
/// anything else.
fn rank(statement: &str) -> Option<u8> {
    let upper = squashed(statement).to_ascii_uppercase();
    if upper.starts_with("CREATE SCHEMA ") || upper.starts_with("CREATE TYPE ") {
        Some(1)
    } else if upper.starts_with("CREATE TABLE ") {
        Some(2)
    } else if upper.starts_with("ALTER TABLE ")
        && upper.contains(" ALTER COLUMN ")
        && upper.contains(" SET DEFAULT ")
    {
        Some(3)
    } else if upper.starts_with("ALTER TABLE ")
        && upper.contains(" ADD CONSTRAINT ")
        && upper.contains(" FOREIGN KEY")
    {
        Some(4)
    } else {
        None
    }
}

/// An empty database of the harness's own on the instance of the shared
/// databases, as its superuser.
fn empty_database() -> Sql {
    let server = blackbox::postgres::for_shared_databases();
    let name = format!("standin_check_{}", token());
    Sql::connect(&server.url("postgres"))
        .unwrap()
        .batch(&format!("CREATE DATABASE {name}"));
    Sql::connect(&server.url(&name)).unwrap()
}

/// Runs each statement on its own; fails the test at the first that
/// fails, naming it.
fn run_each(sql: &Sql, statements: &[String]) {
    for statement in statements {
        if let Err(problem) = sql.try_batch(statement) {
            panic!(
                "expected each statement of the stand-in to run on its own in an empty database, in order; this one failed: {problem}\nall of them:\n{}",
                statements.join("\n")
            );
        }
    }
}

/// The columns of every table, partitioned table, view and materialized
/// view outside Postgres's own schemas (no partition), in order: (relation,
/// column, type, not null, identity, generated).
const COLUMNS: &str = r#"
SELECT n.nspname || '.' || c.relname, a.attname, format_type(a.atttypid, a.atttypmod),
       a.attnotnull::text, a.attidentity::text, a.attgenerated::text
  FROM pg_attribute a
  JOIN pg_class c ON c.oid = a.attrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind IN ('r', 'p', 'v', 'm') AND NOT c.relispartition
   AND n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
   AND a.attnum > 0 AND NOT a.attisdropped
 ORDER BY (n.nspname || '.' || c.relname) COLLATE "C", a.attnum"#;

/// The kind of each relation of [`COLUMNS`]: `r`, `p`, `v` or `m`.
const KINDS: &str = r#"
SELECT n.nspname || '.' || c.relname, c.relkind::text
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind IN ('r', 'p', 'v', 'm') AND NOT c.relispartition
   AND n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
 ORDER BY (n.nspname || '.' || c.relname) COLLATE "C""#;

/// Every enum with its labels in order.
const ENUMS: &str = r#"
SELECT n.nspname || '.' || t.typname, string_agg(e.enumlabel, ',' ORDER BY e.enumsortorder)
  FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid JOIN pg_namespace n ON n.oid = t.typnamespace
 GROUP BY 1 ORDER BY (n.nspname || '.' || t.typname) COLLATE "C""#;

/// Every default and generation expression of a relation that is no
/// partition.
const DEFAULTS: &str = r#"
SELECT n.nspname || '.' || c.relname, a.attname, pg_get_expr(d.adbin, d.adrelid)
  FROM pg_attrdef d
  JOIN pg_class c ON c.oid = d.adrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
  JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
 WHERE NOT c.relispartition AND n.nspname NOT LIKE 'pg\_%'
 ORDER BY (n.nspname || '.' || c.relname) COLLATE "C", a.attname::text COLLATE "C""#;

/// Every primary key, unique constraint and foreign key of a relation that
/// is no partition, without its name: (relation, kind, definition).
const CONSTRAINTS: &str = r#"
SELECT n.nspname || '.' || c.relname, k.contype::text, pg_get_constraintdef(k.oid)
  FROM pg_constraint k
  JOIN pg_class c ON c.oid = k.conrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE k.contype IN ('p', 'u', 'f') AND NOT c.relispartition AND n.nspname NOT LIKE 'pg\_%'
 ORDER BY (n.nspname || '.' || c.relname) COLLATE "C", k.contype, pg_get_constraintdef(k.oid) COLLATE "C""#;

fn rows(sql: &Sql, query: &str) -> Vec<Vec<String>> {
    sql.batch(query)
        .into_iter()
        .map(|row| row.into_iter().map(Option::unwrap_or_default).collect())
        .collect()
}

#[test]
fn the_structure_answer_has_the_standin_statements_and_the_resources_contact() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    assert_eq!(
        answer["contact"],
        json!(CONTACT),
        "expected `contact`, the Resource's contact, in the answer: {answer}"
    );
    let statements = standin_statements(&answer["standin_sql"]);
    assert!(
        !statements.is_empty(),
        "expected the stand-in's statements in `standin_sql`, got {}",
        answer["standin_sql"]
    );
    // A Builder whose Grant is `structure` gets them too.
    stock.scenario.grant(STOCK, "viewers", "structure");
    let viewer = User::new("vera").in_groups(&["viewers"]);
    let as_viewer = stock.pull_ok(&viewer, STOCK);
    assert_eq!(
        as_viewer["standin_sql"], answer["standin_sql"],
        "expected the same stand-in for a Builder who may read the structure only"
    );
    assert_eq!(as_viewer["contact"], json!(CONTACT));
}

#[test]
fn the_statements_are_one_kind_after_another_in_the_stated_order() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let statements = standin_statements(&answer["standin_sql"]);
    let mut last = 0;
    for statement in &statements {
        let rank = rank(statement).unwrap_or_else(|| {
            panic!(
                "expected each statement to make a schema, an enum or a table, or to set one default, or to add one foreign key; got {statement:?}"
            )
        });
        assert!(
            rank >= last,
            "expected schemas and enums, then tables, then defaults, then foreign keys; {statement:?} comes after a later kind\n{}",
            statements.join(";\n")
        );
        last = rank;
    }
    let count = |r: u8| statements.iter().filter(|s| rank(s) == Some(r)).count();
    // As many tables as the shared database has relations listed (tables,
    // views, materialized views, partitioned tables; no partition), counted
    // from its catalog.
    let listed = rows(&stock.db().superuser(), KINDS).len();
    assert!(
        listed >= 20,
        "the harness's own check: the fixture's 20 relations at least, got {listed}"
    );
    assert_eq!(
        count(2),
        listed,
        "expected one CREATE TABLE for each table, view, materialized view and partitioned table listed, and none for a partition\n{}",
        statements.join(";\n")
    );
    // Seven foreign keys between listed tables, each as one statement.
    assert_eq!(
        count(4),
        7,
        "expected each foreign key as one ALTER TABLE ... ADD CONSTRAINT\n{}",
        statements.join(";\n")
    );
    // The defaults of the fixture, each as one ALTER TABLE ... SET DEFAULT.
    let defaults: Vec<&String> = statements.iter().filter(|s| rank(s) == Some(3)).collect();
    for (table, column) in [
        ("sites", "active"),
        ("items", "status"),
        ("items", "qty"),
        ("items", "added_at"),
        ("accounts", "id"),
        ("accounts", "tier"),
    ] {
        assert!(
            defaults
                .iter()
                .any(|s| s.contains(table) && s.contains(column)),
            "expected the default of {table}.{column} as one ALTER TABLE ... ALTER COLUMN ... SET DEFAULT, got {defaults:?}"
        );
    }
    for statement in &statements {
        let upper = statement.to_ascii_uppercase();
        assert!(
            !(upper.starts_with("CREATE TABLE")
                && upper.contains(" DEFAULT ")
                && !upper.contains("GENERATED")),
            "expected no default inside a CREATE TABLE (each is a statement of its own): {statement:?}"
        );
        assert!(
            !(upper.starts_with("CREATE TABLE") && upper.contains("REFERENCES")),
            "expected no foreign key inside a CREATE TABLE (each is a statement of its own): {statement:?}"
        );
    }
}

#[test]
fn the_statements_make_an_empty_standin_with_the_structures_columns_keys_and_defaults() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let statements = standin_statements(&answer["standin_sql"]);
    let standin = empty_database();
    run_each(&standin, &statements);
    let real = stock.db().superuser();

    // Every relation is an ordinary table: views, materialized views and
    // the partitioned table included; partitions are not there.
    let kinds = rows(&standin, KINDS);
    let real_kinds = rows(&real, KINDS);
    let names = |k: &[Vec<String>]| -> Vec<String> { k.iter().map(|r| r[0].clone()).collect() };
    assert_eq!(
        names(&kinds),
        names(&real_kinds),
        "expected the stand-in to have exactly the structure's relations"
    );
    for row in &kinds {
        assert_eq!(
            row[1], "r",
            "expected {} to be an ordinary table in the stand-in",
            row[0]
        );
    }
    assert!(!names(&kinds).iter().any(|n| n.contains("readings_20")));

    // The same columns in the same order, each with its type (a domain as
    // text), its NOT NULL, its identity and its generation.
    let real_columns: Vec<Vec<String>> = rows(&real, COLUMNS)
        .into_iter()
        .map(|mut row| {
            if row[2] == "positive_int" {
                row[2] = "text".to_owned();
            }
            row
        })
        .collect();
    assert_eq!(
        rows(&standin, COLUMNS),
        real_columns,
        "expected every column of the structure in the stand-in, in order, with its type (a domain as text), NOT NULL, identity and generation"
    );
    // A serial column is one in the stand-in too: it owns a sequence.
    assert!(
        standin
            .value("SELECT pg_get_serial_sequence('movements', 'serial_no')")
            .is_some(),
        "expected movements.serial_no a serial column in the stand-in"
    );
    assert_eq!(
        rows(&standin, ENUMS),
        rows(&real, ENUMS),
        "expected each enum with its labels in order"
    );
    assert_eq!(
        rows(&standin, DEFAULTS),
        rows(&real, DEFAULTS),
        "expected every default and generated column's expression"
    );
    assert_eq!(
        rows(&standin, CONSTRAINTS),
        rows(&real, CONSTRAINTS),
        "expected the primary keys, unique constraints and foreign keys between listed tables"
    );
    // No row.
    for name in names(&kinds) {
        let (schema, table) = name.split_once('.').unwrap();
        let count = standin.value(&format!(
            "SELECT count(*) FROM {}.{}",
            blackbox::structure::ident(schema),
            blackbox::structure::ident(table)
        ));
        assert_eq!(
            count.as_deref(),
            Some("0"),
            "expected no row in the stand-in's {name}"
        );
    }
}

#[test]
fn the_same_structure_gives_the_same_statements_however_it_was_made() {
    let first = Stock::ready();
    let second = Stock::start_ordered(&IN_ANOTHER_ORDER);
    second.register(STOCK);
    second.scenario.grant(STOCK, READERS, "read");
    let one = first.pull_ok(&rita(), STOCK);
    let again = first.pull_ok(&rita(), STOCK);
    let other = second.pull_ok(&rita(), STOCK);
    assert!(
        !standin_statements(&one["standin_sql"]).is_empty(),
        "expected the stand-in's statements, got {}",
        one["standin_sql"]
    );
    assert_eq!(
        one["standin_sql"], again["standin_sql"],
        "expected the same text pulled twice"
    );
    assert_eq!(
        one["standin_sql"], other["standin_sql"],
        "expected the same text for the same structure made in another order"
    );
}

#[test]
fn the_statements_hold_no_row_privilege_owner_comment_or_connection() {
    let stock = Stock::ready();
    stock
        .db()
        .superuser()
        .batch("COMMENT ON TABLE sites IS 'a comment of the database''s'");
    let answer = stock.pull_ok(&rita(), STOCK);
    let text = standin_statements(&answer["standin_sql"]).join("\n");
    let mut markers = stock.markers();
    markers.push(("a row's value", stock.db().row_marker.clone()));
    for value in ["Bolts", "North depot", "Acme", "Globex"] {
        markers.push(("a row's value", value.to_owned()));
    }
    markers.push(("the comment", "a comment of the database".to_owned()));
    blackbox::scenario::assert_none_holds(
        &[("standin_sql".to_owned(), text.clone())],
        &markers,
        "the stand-in's statements",
    );
    let upper = text.to_ascii_uppercase();
    for word in [
        "GRANT ",
        "REVOKE ",
        "OWNER",
        "COMMENT ON",
        "INSERT ",
        "COPY ",
        "CREATE FUNCTION",
        "TRIGGER",
        "CREATE ROLE",
        "SET ROLE",
        "READINGS_2025",
    ] {
        assert!(
            !upper.contains(word),
            "expected no {word:?} in the stand-in's statements\n{text}"
        );
    }
}

#[test]
fn the_pull_writes_standin_sql_with_its_three_first_lines_and_the_statements() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let printed = pull(&machine, &dir, STOCK).ok_json();
    let files = printed["files"].as_array().unwrap();
    assert!(
        files.contains(&json!({ "path": "resources/stock/standin.sql", "changed": true })),
        "expected resources/stock/standin.sql in `files`, changed, got {printed}"
    );
    let text = fs::read_to_string(dir.join("resources/stock/standin.sql")).unwrap();
    let expected = standin_file(
        STOCK,
        answer["fingerprint"].as_str().unwrap(),
        CONTACT,
        &standin_statements(&answer["standin_sql"]),
    );
    assert_eq!(
        text, expected,
        "expected the three first lines, an empty line, then each statement of `standin_sql` in order with an empty line after it"
    );
}

#[test]
fn a_contact_of_several_lines_is_written_on_one_line() {
    let stock = Stock::start();
    let mut body = body_with_structure("stock", stock.db(), &stock.structure);
    body["contact"] = json!("The stock desk\nroom 4, second floor");
    stock.scenario.register_body("stock", &body);
    stock.scenario.grant("stock", READERS, "read");
    let answer = stock.pull_ok(&rita(), STOCK);
    assert_eq!(
        answer["contact"],
        json!("The stock desk\nroom 4, second floor")
    );
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    pull(&machine, &dir, STOCK).ok_json();
    let text = fs::read_to_string(dir.join("resources/stock/standin.sql")).unwrap();
    let third = text.lines().nth(2).unwrap_or_default();
    assert!(
        third.starts_with("-- Contact: ")
            && third.contains("The stock desk")
            && third.contains("room 4, second floor"),
        "expected the whole contact on the third line, got {third:?}\n{text}"
    );
    let after_three: String = text.split_inclusive('\n').skip(3).collect();
    assert_eq!(
        after_three,
        standin_body(&standin_statements(&answer["standin_sql"])),
        "expected the contact's line end to break no statement: after the third line, the statements as answered"
    );
}

#[test]
fn pulling_twice_leaves_standin_sql_as_it_was_and_a_changed_structure_rewrites_it() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    pull(&machine, &dir, STOCK).ok_json();
    let before = Tree::of(&dir);
    std::thread::sleep(Duration::from_millis(1100));
    let second = pull(&machine, &dir, STOCK).ok_json();
    assert!(
        second["files"]
            .as_array()
            .unwrap()
            .contains(&json!({ "path": "resources/stock/standin.sql", "changed": false })),
        "expected standin.sql unchanged, got {second}"
    );
    assert_eq!(
        before.differences(&Tree::of(&dir)),
        Vec::<String>::new(),
        "expected the second pull of an unchanged structure to write no file"
    );
    stock
        .db()
        .superuser()
        .batch("ALTER TABLE sites ADD COLUMN region text");
    let third = pull(&machine, &dir, STOCK).ok_json();
    assert!(
        third["files"]
            .as_array()
            .unwrap()
            .contains(&json!({ "path": "resources/stock/standin.sql", "changed": true })),
        "expected standin.sql rewritten for a changed structure, got {third}"
    );
    let text = fs::read_to_string(dir.join("resources/stock/standin.sql")).unwrap();
    assert!(
        statements_in_text(&text)
            .iter()
            .any(|s| s.contains("CREATE TABLE") && s.contains("sites") && s.contains("region")),
        "expected the new column in the stand-in's table\n{text}"
    );
}

#[test]
fn a_standin_sql_the_tool_did_not_write_is_not_overwritten_and_nothing_is_written() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "hand");
    fs::create_dir_all(dir.join("resources/stock")).unwrap();
    let hand_written = "-- Written by hand.\nCREATE TABLE mine (id integer);\n";
    fs::write(dir.join("resources/stock/standin.sql"), hand_written).unwrap();
    let before = Tree::of(&dir);
    let message = pull(&machine, &dir, STOCK).json_error(1, "not-generated");
    assert!(
        message.contains("resources/stock/standin.sql"),
        "expected the message to name the file, got {message:?}"
    );
    assert_eq!(
        before.differences(&Tree::of(&dir)),
        Vec::<String>::new(),
        "expected none of the files written, vm.dev.json included"
    );
}

#[test]
fn a_standin_sql_that_is_a_link_is_not_written_through() {
    use std::os::unix::fs::symlink;
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "linked");
    let outside = machine.root().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let target = outside.join("standin.sql");
    fs::write(
        &target,
        format!("{STANDIN_FIRST_LINE}\n-- outside the project\n"),
    )
    .unwrap();
    fs::create_dir_all(dir.join("resources/stock")).unwrap();
    symlink(&target, dir.join("resources/stock/standin.sql")).unwrap();
    let before = Tree::of(machine.root());
    let message = pull(&machine, &dir, STOCK).json_error(1, "not-a-project");
    assert!(
        message.contains("resources/stock/standin.sql"),
        "expected the message to name the link, got {message:?}"
    );
    assert_eq!(
        before.differences(&Tree::of(machine.root())),
        Vec::<String>::new(),
        "expected nothing written or changed, inside the project or outside"
    );
}

#[test]
fn a_refused_pull_writes_no_standin_and_leaves_an_earlier_one_as_it_was() {
    let stock = Stock::ready();
    let nora = User::new("nora").in_groups(&["elsewhere"]);
    let rita_machine = Machine::new();
    rita_machine.sign_in(&stock.scenario.vm, &rita());
    let nora_machine = Machine::new();
    nora_machine.sign_in(&stock.scenario.vm, &nora);

    let fresh = project(&nora_machine, "fresh");
    let before = Tree::of(&fresh);
    pull(&nora_machine, &fresh, STOCK).json_error(3, "no-grant");
    assert_eq!(
        before.differences(&Tree::of(&fresh)),
        Vec::<String>::new(),
        "expected nothing written by a refused pull: no standin.sql, no vm.dev.json"
    );

    let pulled = project(&rita_machine, "pulled");
    pull(&rita_machine, &pulled, STOCK).ok_json();
    for written in ["resources/stock/standin.sql", "vm.dev.json"] {
        assert!(
            pulled.join(written).is_file(),
            "expected the granted pull to have written {written}, for the refused one to leave"
        );
    }
    stock
        .db()
        .superuser()
        .batch("ALTER TABLE sites ADD COLUMN region text");
    let before = Tree::of(&pulled);
    std::thread::sleep(Duration::from_millis(50));
    pull(&nora_machine, &pulled, STOCK).json_error(3, "no-grant");
    assert_eq!(
        before.differences(&Tree::of(&pulled)),
        Vec::<String>::new(),
        "expected an earlier pull's standin.sql and vm.dev.json left as they were by a refused pull"
    );
}

#[test]
fn statements_are_read_from_a_text_and_standin_sql_must_be_an_array_of_them() {
    let text = "-- Generated by vm pull. Do not edit.\n-- Resource: x  Fingerprint: sha256:00\n-- Contact: a; b\nCREATE SCHEMA crm;\nCREATE TABLE t (a text DEFAULT ';');\n";
    // The third line holds a `;` inside a comment: no statement.
    assert_eq!(
        statements_in_text(text),
        ["CREATE SCHEMA crm", "CREATE TABLE t (a text DEFAULT ';')"]
    );
    let answered = standin_statements(&json!(["CREATE SCHEMA crm;", "CREATE TABLE t (a text);"]));
    assert_eq!(answered, ["CREATE SCHEMA crm;", "CREATE TABLE t (a text);"]);
    assert_eq!(
        standin_file("x", "sha256:00", "a", &answered),
        "-- Generated by vm pull. Do not edit.\n-- Resource: x  Fingerprint: sha256:00\n-- Contact: a\n\nCREATE SCHEMA crm;\n\nCREATE TABLE t (a text);\n\n"
    );
    for wrong in [
        json!("CREATE SCHEMA crm;"),
        json!(["CREATE SCHEMA crm"]),
        json!(["CREATE SCHEMA crm; CREATE SCHEMA b;"]),
        json!([1]),
    ] {
        assert!(
            std::panic::catch_unwind(|| standin_statements(&wrong)).is_err(),
            "the harness's own check: {wrong} is not an array of statements each ending with `;`"
        );
    }
    let set: BTreeSet<Option<u8>> = ["CREATE TYPE s AS ENUM ('a')", "GRANT SELECT ON t TO r"]
        .iter()
        .map(|s| rank(s))
        .collect();
    assert_eq!(set, BTreeSet::from([None, Some(1)]));
}
