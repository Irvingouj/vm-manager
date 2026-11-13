//! An own database in the earlier form is brought to this one when the
//! program starts (CONTRACT.md, "The platform's part of an own database is
//! out of a migration's reach: Three roles": "An own database in the
//! earlier form (its `vm` owned by `_owner`) is brought to this form when
//! the program starts, before it serves; its history, its keys and its
//! tables' rows are unchanged").
//!
//! The harness's superuser gives the schema `vm` and everything the
//! platform's own role owns back to the owner role (`REASSIGN OWNED`, which
//! also gives it the database) and lets the owner role log in: the earlier
//! form. The program is then started again on the same platform database.
//! Before it serves, the three roles stand as CONTRACT.md says, and the
//! keys, the history, the record of migrations and every table's rows are
//! as they were; Version 1 still serves and a change is still recorded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history_after, last_history_id};
use blackbox::out_of_reach::{
    WRITER, assert_three_roles, can_log_in, differences, install_seeded, platform_role, start,
};
use blackbox::own_database::{Own, OwnDatabase, assert_ok, ident, own_execute};

/// What must be unchanged: the rows of every table of `vm` (the keys as a
/// digest) and of every table of the Application's, and every sequence of
/// the Application's.
fn data(db: &OwnDatabase) -> String {
    let mut text = String::new();
    for table in db.column(
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p') ORDER BY 1",
    ) {
        let query = if table == "keys" {
            format!(
                "SELECT md5(coalesce(string_agg(t::text, '|' ORDER BY t::text), '')) FROM vm.{} t",
                ident(&table)
            )
        } else {
            format!(
                "SELECT coalesce(string_agg(t::text, E'\\n    ' ORDER BY t::text), '') FROM vm.{} t",
                ident(&table)
            )
        };
        text.push_str(&format!(
            "vm.{table}: {}\n",
            db.superuser().value(&query).unwrap_or_default()
        ));
    }
    for line in db.fingerprint().lines() {
        if line.starts_with("rows of ") {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

#[test]
fn an_own_database_in_the_earlier_form_is_brought_to_three_roles_at_start_with_its_data_unchanged()
{
    let own = start();
    let db = install_seeded(&own, APP);
    let before = data(&db);
    assert!(
        before.contains("vm.keys: ") && before.contains("vm.history: "),
        "the harness's own check: expected vm.keys and vm.history, got\n{before}"
    );
    // The earlier form: everything of the platform's back to the owner
    // role, which may log in.
    let platform = platform_role(&db);
    let superuser = db.superuser();
    if db.role_exists(&platform) {
        superuser.batch(&format!(
            "REASSIGN OWNED BY {} TO {}",
            ident(&platform),
            ident(&db.owner)
        ));
    }
    superuser.batch(&format!(
        "ALTER DATABASE {} OWNER TO {o}; ALTER SCHEMA vm OWNER TO {o}; ALTER ROLE {o} LOGIN",
        ident(&db.name),
        o = ident(&db.owner)
    ));
    drop(superuser);
    assert_eq!(
        db.column(
            "SELECT DISTINCT pg_get_userbyid(owner) FROM (
               SELECT nspowner AS owner FROM pg_namespace WHERE nspname = 'vm'
               UNION ALL SELECT relowner FROM pg_class WHERE relnamespace = 'vm'::regnamespace
               UNION ALL SELECT proowner FROM pg_proc WHERE pronamespace = 'vm'::regnamespace
               UNION ALL SELECT datdba FROM pg_database WHERE datname = current_database()) x"
        ),
        vec![db.owner.clone()],
        "the harness's own check: expected the earlier form, vm and the database the owner role's"
    );
    assert!(can_log_in(&db, &db.owner), "the harness's own check");
    assert_eq!(
        data(&db),
        before,
        "the harness's own check: the data unchanged by the harness"
    );
    // The program started again on the same platform database.
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    assert_three_roles(
        &own,
        &db,
        "after a start on an own database in the earlier form",
    );
    let after = data(&db);
    assert!(
        after == before,
        "expected the keys, the history, the record of migrations and every table's rows unchanged by bringing the own database to three roles, but:\n{}",
        differences(&before, &after)
    );
    assert_eq!(own.serving(&User::new(WRITER), APP), 1);
    let last = last_history_id(&db);
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "update things set title = 'after the start' where id = 101",
        ),
    );
    assert_ok(&answer, "a change after the start");
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 1, "expected the change recorded, got {rows:?}");
    assert_eq!(rows[0]["new"]["title"], "after the start");
    assert_eq!(rows[0]["user_id"], WRITER);
    // And a later Version still applies, and its table is captured.
    let mut list = blackbox::out_of_reach::base();
    list.push(blackbox::own_database::Migration::new(
        "0004_after_the_start",
        &[r#"CREATE TABLE "later" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    ));
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_three_roles(&own, &db, "after a Version sent to the brought database");
}
