//! A login limited to some tables (CONTRACT.md, "Resources and Grants in
//! full: A login limited to some tables"): a query of a table the login
//! holds no privilege on fails `42501`; a pull lists a relation when the
//! Resource's read login or write login holds a privilege on it or one of
//! its columns (directly, through membership, through `PUBLIC`), all the
//! columns of a listed table, the enums of listed columns, no foreign key to
//! a relation that is not listed (in neither the definition nor the
//! fingerprint), the empty module for a database nothing is reached of;
//! `hidden` counts what is not listed, partitions not counted, and is not
//! in the fingerprint; the fingerprint is the same whichever login pulled;
//! a structure login's own privileges play no part; a role dropped at the
//! shared database holds nothing and the pull does not fail.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::resources::{
    body, connection, drop_role, login_holding_nothing, login_with_privileges, new_login,
    payroll_shape,
};
use blackbox::scenario::{
    Expected, READERS, Scenario, assert_database_error, assert_entry, assert_rows, op, sql_on,
};
use blackbox::shared_database::Login;
use blackbox::structure::{assert_fingerprint, pull, without_second_line};
use blackbox::{User, assert_api_error};
use serde_json::Value;

const LIM: &str = "lim";

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn crew() -> User {
    User::new("crew-user").in_groups(&["crew"])
}

/// A program and a shared database of `incidents`, `audit` and `payroll`.
fn world() -> Scenario {
    let s = Scenario::start();
    payroll_shape(&s.db);
    s
}

/// Registers `name` with these logins and gives [`READERS`] `read` on it.
fn register(
    s: &Scenario,
    name: &str,
    read: &Login,
    write: Option<&Login>,
    structure: Option<&Login>,
) -> Value {
    let created = s.register_body(
        name,
        &body(name, &connection(&s.db, read, write, structure)),
    );
    s.grant(name, READERS, "read");
    created
}

/// `GET /v1/resources/<name>/structure` as [`rita`], which must be `200`.
fn pulled(s: &Scenario, name: &str) -> Value {
    let answer = pull(&s.vm.api_as(&rita()), name);
    answer.expect_status(200);
    answer.json()
}

fn schema(answer: &Value) -> String {
    answer["schema_ts"].as_str().unwrap().to_owned()
}

fn has_table(answer: &Value, table: &str) -> bool {
    schema(answer).contains(&format!("pgTable(\"{table}\""))
}

fn hidden(answer: &Value) -> u64 {
    answer["hidden"]
        .as_u64()
        .unwrap_or_else(|| panic!("expected `hidden` a number in {}", keys(answer)))
}

fn keys(answer: &Value) -> String {
    answer
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// Fails unless nothing of `payroll` is in the whole answer.
fn assert_no_payroll(answer: &Value, context: &str) {
    let text = answer.to_string();
    for word in ["payroll", "salary", "owner_id_fkey", "incidents_owner_fk"] {
        assert!(
            !text.contains(word),
            "{context}: expected nothing of an unlisted table in the answer, found {word:?}:\n{text}"
        );
    }
}

#[test]
fn the_contracts_test_a_limited_read_login_a_write_login_on_one_table_and_a_structure_login_of_nothing()
 {
    let s = world();
    let db = &s.db;
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    let write = login_with_privileges(db, &["INSERT ON audit"]);
    let structure = login_holding_nothing(db);
    register(&s, LIM, &read, Some(&write), Some(&structure));
    s.grant(LIM, "crew", "write");
    s.install("board", "operations.js", &[(LIM, "write")]);

    let answer = pulled(&s, LIM);
    assert_fingerprint(&answer["fingerprint"], "a limited Resource's fingerprint");
    assert!(has_table(&answer, "incidents"), "{}", schema(&answer));
    assert!(has_table(&answer, "audit"), "{}", schema(&answer));
    for column in ["id", "title", "open", "severity", "reported_at", "owner_id"] {
        assert!(
            schema(&answer).contains(&format!("\"{column}\"")),
            "expected every column of a listed table, missing {column:?}:\n{}",
            schema(&answer)
        );
    }
    assert!(
        schema(&answer).contains("\"what\""),
        "the columns of `audit`"
    );
    assert_eq!(hidden(&answer), 1, "{}", keys(&answer));
    assert_no_payroll(&answer, "the pull of a limited Resource");

    // The queries: the table the read login holds nothing on fails 42501.
    let (response, refused) = s.one(
        &crew(),
        "board",
        &op("query", &sql_on(LIM, "select salary from payroll")),
    );
    assert_database_error(&refused, "42501", "a query of `payroll` as the read login");
    let entry = s.entry_of(&response);
    let id = response.request_id();
    assert_entry(
        &entry,
        &Expected {
            resource: Some(LIM),
            ..Expected::failed(&id, "crew-user", "board", "42501")
        },
        "the Operation log entry of the query of `payroll`",
    );
    let (_, ran) = s.one(
        &crew(),
        "board",
        &op(
            "query",
            &sql_on(LIM, "select title from incidents where id = 1"),
        ),
    );
    assert_rows(
        &ran,
        &["title"],
        &serde_json::json!([["Disk full on db-7"]]),
        "a query of `incidents` as the read login",
    );

    // Registered without the structure login, the read login pulls: the
    // same definition and fingerprint.
    register(&s, "lim_two", &read, Some(&write), None);
    let again = pulled(&s, "lim_two");
    assert_eq!(again["fingerprint"], answer["fingerprint"]);
    assert_eq!(
        without_second_line(&schema(&again)),
        without_second_line(&schema(&answer)),
        "expected the same definition whichever login pulled"
    );
    assert_eq!(hidden(&again), 1);

    // Granting the read login `SELECT` on `payroll` makes the next pull
    // list it, with no change to the Resource.
    db.superuser()
        .batch(&format!("GRANT SELECT ON payroll TO {}", read.role));
    let widened = pulled(&s, LIM);
    assert!(has_table(&widened, "payroll"), "{}", schema(&widened));
    assert!(schema(&widened).contains("\"salary\""));
    assert_eq!(hidden(&widened), 0);
    assert_ne!(
        widened["fingerprint"], answer["fingerprint"],
        "a listed table is in the fingerprint"
    );
}

#[test]
fn a_foreign_key_to_a_table_no_login_reaches_is_in_neither_the_definition_nor_the_fingerprint() {
    let s = world();
    let db = &s.db;
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    register(&s, LIM, &read, None, None);
    let with_key = pulled(&s, LIM);
    assert_no_payroll(&with_key, "a pull with a foreign key to an unlisted table");
    // Drop the constraint itself: nothing the logins reach changes, so the
    // fingerprint and the definition do not.
    db.superuser()
        .batch("ALTER TABLE incidents DROP CONSTRAINT incidents_owner_fk");
    let without_key = pulled(&s, LIM);
    assert_eq!(
        without_key["fingerprint"], with_key["fingerprint"],
        "expected a foreign key to a table no login reaches to be in no fingerprint"
    );
    assert_eq!(
        schema(&without_key),
        schema(&with_key).replace(
            &with_key["fingerprint"].as_str().unwrap().to_owned(),
            without_key["fingerprint"].as_str().unwrap()
        )
    );
}

#[test]
fn a_privilege_on_one_column_lists_the_table_with_all_its_columns() {
    let s = world();
    let db = &s.db;
    let by_select = login_with_privileges(db, &["SELECT (title) ON incidents"]);
    register(&s, "by_select", &by_select, None, None);
    let answer = pulled(&s, "by_select");
    assert!(has_table(&answer, "incidents"), "{}", schema(&answer));
    for column in ["severity", "reported_at", "owner_id"] {
        assert!(
            schema(&answer).contains(&format!("\"{column}\"")),
            "expected the columns of a table listed through one column, missing {column:?}"
        );
    }
    assert!(!has_table(&answer, "audit") && !has_table(&answer, "payroll"));
    assert_eq!(hidden(&answer), 2);
    // `INSERT` and `UPDATE` on a column list the table too (write login).
    let reads_nothing = login_holding_nothing(db);
    for (n, grant) in ["INSERT (what) ON audit", "UPDATE (what) ON audit"]
        .iter()
        .enumerate()
    {
        let write = login_with_privileges(db, &[grant]);
        let name = format!("by_column_{n}");
        register(&s, &name, &reads_nothing, Some(&write), None);
        let answer = pulled(&s, &name);
        assert!(
            has_table(&answer, "audit"),
            "expected `{grant}` of the write login to list `audit`:\n{}",
            schema(&answer)
        );
        assert!(!has_table(&answer, "incidents"));
        assert_eq!(hidden(&answer), 2);
    }
}

#[test]
fn every_privilege_of_the_contract_lists_a_table() {
    let s = world();
    let db = &s.db;
    for (n, privilege) in ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE"]
        .iter()
        .enumerate()
    {
        let read = login_with_privileges(db, &[&format!("{privilege} ON audit")]);
        let name = format!("by_privilege_{n}");
        register(&s, &name, &read, None, None);
        let answer = pulled(&s, &name);
        assert!(
            has_table(&answer, "audit"),
            "expected `{privilege}` on a table to list it:\n{}",
            schema(&answer)
        );
        assert_eq!(hidden(&answer), 2, "{privilege}");
    }
}

#[test]
fn a_privilege_through_membership_or_public_lists_a_table() {
    let s = world();
    let db = &s.db;
    // Membership: a role that cannot log in holds `SELECT` on `audit`, the
    // read login is a member of it.
    let group = new_login(db, "audit_readers", "");
    db.superuser().batch(&format!(
        "ALTER ROLE {g} NOLOGIN; GRANT SELECT ON audit TO {g}",
        g = group.role
    ));
    let member = login_with_privileges(db, &[]);
    db.superuser()
        .batch(&format!("GRANT {} TO {}", group.role, member.role));
    register(&s, "by_member", &member, None, None);
    let answer = pulled(&s, "by_member");
    assert!(
        has_table(&answer, "audit"),
        "expected a privilege through membership to list the table:\n{}",
        schema(&answer)
    );
    assert!(!has_table(&answer, "payroll"));
    // A member of a role through two others.
    let outer = new_login(db, "outer", "");
    db.superuser().batch(&format!(
        "ALTER ROLE {o} NOLOGIN; GRANT {g} TO {o}",
        o = outer.role,
        g = group.role
    ));
    let deep = login_with_privileges(db, &[]);
    db.superuser()
        .batch(&format!("GRANT {} TO {}", outer.role, deep.role));
    register(&s, "by_deep_member", &deep, None, None);
    assert!(has_table(&pulled(&s, "by_deep_member"), "audit"));
    // `PUBLIC`.
    let alone = login_with_privileges(db, &[]);
    register(&s, "before_public", &alone, None, None);
    assert!(!has_table(&pulled(&s, "before_public"), "audit"));
    db.superuser().batch("GRANT SELECT ON audit TO PUBLIC");
    let answer = pulled(&s, "before_public");
    assert!(
        has_table(&answer, "audit"),
        "expected a privilege through `PUBLIC` to list the table:\n{}",
        schema(&answer)
    );
}

#[test]
fn a_view_and_a_materialized_view_are_listed_by_their_own_privileges() {
    let s = world();
    let db = &s.db;
    db.superuser().batch(
        "CREATE VIEW open_incidents AS SELECT id, title FROM incidents WHERE open;
         CREATE MATERIALIZED VIEW incident_counts AS SELECT severity, count(*) AS n FROM incidents GROUP BY severity;
         CREATE VIEW payroll_view AS SELECT id FROM payroll",
    );
    let read = login_with_privileges(
        db,
        &["SELECT ON open_incidents", "SELECT ON incident_counts"],
    );
    register(&s, LIM, &read, None, None);
    let answer = pulled(&s, LIM);
    assert!(
        schema(&answer).contains("pgView(\"open_incidents\""),
        "{}",
        schema(&answer)
    );
    assert!(
        schema(&answer).contains("pgMaterializedView(\"incident_counts\""),
        "{}",
        schema(&answer)
    );
    assert!(!has_table(&answer, "incidents"));
    assert!(!schema(&answer).contains("payroll_view"));
    // Hidden: `incidents`, `audit`, `payroll`, `payroll_view`.
    assert_eq!(hidden(&answer), 4, "{}", keys(&answer));
}

#[test]
fn partitions_are_not_counted_in_hidden_and_hidden_is_not_in_the_fingerprint() {
    let s = world();
    let db = &s.db;
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    register(&s, LIM, &read, None, None);
    let before = pulled(&s, LIM);
    assert_eq!(hidden(&before), 2, "`audit` and `payroll`");
    db.superuser().batch(
        "CREATE TABLE readings (id int4, at date) PARTITION BY RANGE (at);
         CREATE TABLE readings_2025 PARTITION OF readings FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
         CREATE TABLE readings_2026 PARTITION OF readings FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')",
    );
    let after = pulled(&s, LIM);
    assert_eq!(
        hidden(&after),
        3,
        "expected the partitioned table counted once and its two partitions not at all, got {}",
        hidden(&after)
    );
    assert_eq!(
        after["fingerprint"], before["fingerprint"],
        "expected `hidden` not to be part of the fingerprint"
    );
    assert_eq!(
        schema(&after),
        schema(&before),
        "expected the definition not to change for tables nobody reaches"
    );
}

#[test]
fn an_enum_is_listed_with_a_listed_column_and_not_for_a_hidden_one() {
    let s = world();
    let db = &s.db;
    db.superuser().batch(
        "CREATE TYPE mood_kind AS ENUM ('calm', 'busy');
         CREATE TYPE pay_kind AS ENUM ('monthly', 'weekly');
         ALTER TABLE incidents ADD COLUMN mood mood_kind;
         ALTER TABLE payroll ADD COLUMN cadence pay_kind",
    );
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    register(&s, LIM, &read, None, None);
    let answer = pulled(&s, LIM);
    assert!(
        schema(&answer).contains("pgEnum(\"mood_kind\""),
        "{}",
        schema(&answer)
    );
    assert!(
        !schema(&answer).contains("pay_kind") && !schema(&answer).contains("weekly"),
        "expected no enum that only a hidden column uses:\n{}",
        schema(&answer)
    );
}

#[test]
fn a_structure_logins_own_privileges_play_no_part_in_what_is_listed() {
    let s = world();
    let db = &s.db;
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    let structure = login_with_privileges(db, &["SELECT ON payroll", "SELECT ON audit"]);
    register(&s, LIM, &read, None, Some(&structure));
    let answer = pulled(&s, LIM);
    assert!(has_table(&answer, "incidents"));
    assert!(!has_table(&answer, "payroll") && !has_table(&answer, "audit"));
    assert_eq!(hidden(&answer), 2, "{}", keys(&answer));
    assert_no_payroll(
        &answer,
        "a structure login that holds privileges of its own",
    );
}

#[test]
fn a_database_the_logins_reach_nothing_of_is_the_empty_module_not_an_error() {
    let s = world();
    let db = &s.db;
    let read = login_holding_nothing(db);
    let created = register(&s, LIM, &read, None, None);
    // The registration says so.
    let warnings = created["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["reason"] == "login-sees-no-table" && w["login"] == "read"),
        "expected `login-sees-no-table`, got {created}"
    );
    let answer = pulled(&s, LIM);
    assert_fingerprint(&answer["fingerprint"], "the empty module's fingerprint");
    for table in ["incidents", "audit", "payroll"] {
        assert!(!has_table(&answer, table));
    }
    assert_eq!(hidden(&answer), 3);
    assert_eq!(answer["unsupported"], serde_json::json!([]));
    // A fingerprint of its own: not that of a module with a table.
    let other = login_with_privileges(db, &["SELECT ON audit"]);
    register(&s, "lim_audit", &other, None, None);
    assert_ne!(
        pulled(&s, "lim_audit")["fingerprint"],
        answer["fingerprint"]
    );
}

#[test]
fn a_role_dropped_at_the_shared_database_holds_nothing_and_the_pull_does_not_fail() {
    let s = world();
    let db = &s.db;
    let read = login_with_privileges(db, &["SELECT ON incidents"]);
    let write = login_with_privileges(db, &["INSERT ON audit"]);
    let structure = login_holding_nothing(db);
    register(&s, LIM, &read, Some(&write), Some(&structure));
    let before = pulled(&s, LIM);
    assert!(has_table(&before, "audit"));
    drop_role(db, &write);
    let after = pulled(&s, LIM);
    assert!(has_table(&after, "incidents"), "{}", schema(&after));
    assert!(
        !has_table(&after, "audit"),
        "expected a dropped role to hold nothing:\n{}",
        schema(&after)
    );
    assert_eq!(hidden(&after), 2);
    // The read login's role dropped too: the pull still answers `200`
    // through the structure login, listing nothing.
    drop_role(db, &read);
    let none = pull(&s.vm.api_as(&rita()), LIM);
    none.expect_status(200);
    assert!(!has_table(&none.json(), "incidents"));
}

#[test]
fn no_login_of_a_resource_that_does_not_exist_changes_what_is_refused() {
    // A pull of a Resource nobody registered stays `404`; a limited
    // Resource's pull needs the same Grant as ever.
    let s = world();
    let read = login_with_privileges(&s.db, &["SELECT ON incidents"]);
    register(&s, LIM, &read, None, None);
    assert_api_error(&pull(&s.vm.api_as(&rita()), "ghost"), 404, "not-found");
    let nora = User::new("nora").in_groups(&["elsewhere"]);
    assert_api_error(&pull(&s.vm.api_as(&nora), LIM), 403, "no-grant");
}
