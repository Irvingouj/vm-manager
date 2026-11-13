//! What the tests of "The Change history in the platform database: Where it
//! is kept" and "What the roles of an own database can and cannot do"
//! share (CONTRACT.md; ticket "The Change history lives in the platform
//! database, shipped from an outbox"):
//!
//! - [`roles_of`]: the three roles of an own database, by what they are;
//! - [`vm_tables`] and [`attacks_on`]: every table of schema `vm` (the
//!   outbox's name is the platform's own, so every one is tried) and the
//!   statements that read, write, lock or truncate one;
//! - [`refusals_as`]: statements run as a role in a session of its own
//!   (the harness's superuser become the role with `SET ROLE`), each in a
//!   transaction rolled back, and what Postgres answered that was not the
//!   expected SQLSTATE;
//! - [`public_connect`]: what `pg_database.datacl` of a database says of
//!   `CONNECT` for `PUBLIC`;
//! - [`it_made_platform`]: a platform database made by IT (the harness's
//!   superuser), owned by it, with what the program's role needs granted
//!   and `CONNECT` left to `PUBLIC`; [`serve_to_exit`] runs the program
//!   against it until it exits, and [`records_in`] reads its records.

use serde_json::Value;

use crate::DOMAIN;
use crate::history::session_as;
use crate::own_database::{OwnDatabase, ident, literal};
use crate::postgres::{Platform, shared};
use crate::program::{Finished, run_vm_to_exit};
use crate::sql::Sql;

/// The platform's role, the migration role (the owner) and the runtime
/// role of `db`, each with what it is called in CONTRACT.md.
pub fn roles_of(db: &OwnDatabase) -> Vec<(&'static str, String)> {
    vec![
        (
            "the platform's role",
            crate::out_of_reach::platform_role(db),
        ),
        ("the migration role", db.owner.clone()),
        ("the runtime role", db.runtime.clone()),
    ]
}

/// A table of schema `vm`: its name and its first column that is neither
/// an identity nor generated (for an `UPDATE` that changes nothing: one
/// that sets an identity column fails with `428C9` before the rights are
/// checked).
#[derive(Clone, Debug)]
pub struct VmTable {
    pub name: String,
    pub first_column: String,
}

impl VmTable {
    /// `vm."<name>"`.
    pub fn qualified(&self) -> String {
        format!("vm.{}", ident(&self.name))
    }
}

/// Every table of schema `vm` (ordinary, partitioned, and partitions),
/// as the catalog says. Fails unless there is one at least: the outbox is
/// there (the harness's own check).
pub fn vm_tables(db: &OwnDatabase) -> Vec<VmTable> {
    let tables: Vec<VmTable> = db
        .rows(
            "SELECT c.relname::text,
                    coalesce((SELECT a.attname::text FROM pg_attribute a
                               WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                                 AND a.attidentity = '' AND a.attgenerated = ''
                               ORDER BY a.attnum LIMIT 1), '')
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p')
              ORDER BY 1",
        )
        .into_iter()
        .map(|row| VmTable {
            name: row.first().cloned().flatten().unwrap_or_default(),
            first_column: row.get(1).cloned().flatten().unwrap_or_default(),
        })
        .collect();
    assert!(
        !tables.is_empty(),
        "the harness's own check: expected tables in schema vm, the outbox among them"
    );
    tables
}

/// What CONTRACT.md says no role of an own database but the platform's may
/// do to the outbox ("reading it, writing it, locking it or truncating it
/// fails with `42501`"), as (what, statement) on `table`.
pub fn attacks_on(table: &VmTable) -> Vec<(&'static str, String)> {
    let name = table.qualified();
    let column = if table.first_column.is_empty() {
        "x".to_owned()
    } else {
        ident(&table.first_column)
    };
    vec![
        ("read", format!("SELECT * FROM {name}")),
        ("insert", format!("INSERT INTO {name} DEFAULT VALUES")),
        ("update", format!("UPDATE {name} SET {column} = {column}")),
        ("delete", format!("DELETE FROM {name}")),
        ("lock", format!("LOCK TABLE {name} IN ACCESS SHARE MODE")),
        ("lock-rows", format!("SELECT 1 FROM {name} FOR UPDATE")),
        ("truncate", format!("TRUNCATE {name}")),
    ]
}

/// The statements that read, write, lock or truncate `vm.history`.
pub fn statements_on_vm_history() -> Vec<String> {
    vec![
        "SELECT * FROM vm.history".to_owned(),
        "SELECT count(*) FROM vm.history".to_owned(),
        "INSERT INTO vm.history DEFAULT VALUES".to_owned(),
        "UPDATE vm.history SET id = id".to_owned(),
        "DELETE FROM vm.history".to_owned(),
        "TRUNCATE vm.history".to_owned(),
        "LOCK TABLE vm.history IN ACCESS SHARE MODE".to_owned(),
        "SELECT 1 FROM vm.history FOR UPDATE".to_owned(),
    ]
}

/// Runs each statement as `role` (`None`: the harness's superuser) in a
/// session of its own on `db`, each in a transaction rolled back
/// afterwards; returns a line for each that did not fail with `code`.
pub fn refusals_as(
    db: &OwnDatabase,
    role: Option<&str>,
    statements: &[String],
    code: &str,
) -> Vec<String> {
    let sql = session_as(db, role);
    let who = role.unwrap_or("the superuser");
    let mut wrong = Vec::new();
    for statement in statements {
        sql.batch("BEGIN");
        match sql.try_coded(statement) {
            Err(e) if e.code == code => {}
            other => wrong.push(format!(
                "  as {who}: {statement}: expected {code}, got {other:?}"
            )),
        }
        sql.batch("ROLLBACK");
    }
    wrong
}

/// What `pg_database.datacl` of `database` says: the ACL as text (`None`
/// when null, which means the default and so `CONNECT` for `PUBLIC`), and
/// whether an entry grants `CONNECT` to `PUBLIC` (grantee 0).
pub fn public_connect(database: &str) -> (Option<String>, bool) {
    let sql = Sql::connect(&shared().url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
    let rows = sql.batch(&format!(
        "SELECT datacl::text,
                coalesce((SELECT bool_or(a.grantee = 0 AND a.privilege_type = 'CONNECT')
                            FROM aclexplode(d.datacl) a), false)::text
           FROM pg_database d WHERE d.datname = {}",
        literal(database)
    ));
    let row = rows.into_iter().next().unwrap_or_else(|| {
        panic!("the harness's own check: expected the database {database:?} to exist")
    });
    let acl = row.first().cloned().flatten();
    let granted = row.get(1).cloned().flatten().as_deref() == Some("true");
    let public = granted || acl.is_none();
    (acl, public)
}

/// A platform database as IT would make one: a fresh role that may only
/// create databases and roles ([`Platform::fresh_with_limited_role`]), and
/// its database made beforehand by the harness's superuser, owned by the
/// superuser, with `CONNECT`, `CREATE` and `TEMPORARY` on it and all rights
/// on its schema `public` granted to the role, and `CONNECT` left to
/// `PUBLIC` as Postgres gives it.
pub fn it_made_platform() -> Platform {
    let platform = Platform::fresh_with_limited_role();
    let database = ident(platform.name());
    let role = ident(platform.role());
    let instance = Sql::connect(&shared().url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
    instance.batch(&format!("CREATE DATABASE {database}"));
    instance.batch(&format!(
        "GRANT CONNECT, CREATE, TEMPORARY ON DATABASE {database} TO {role}"
    ));
    let inside = Sql::connect(&shared().url(platform.name()))
        .unwrap_or_else(|e| panic!("cannot reach {}: {e}", platform.name()));
    inside.batch(&format!("GRANT ALL ON SCHEMA public TO {role}"));
    let (_, public) = public_connect(platform.name());
    assert!(
        public,
        "the harness's own check: expected PUBLIC to have CONNECT on {} as made",
        platform.name()
    );
    let owner = instance.value(&format!(
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = {}",
        literal(platform.name())
    ));
    assert_eq!(
        owner.as_deref(),
        Some(crate::postgres::SUPERUSER),
        "the harness's own check: expected the platform database owned by the administrator"
    );
    platform
}

/// Runs `vm serve` against `platform` until it exits by itself (killed at
/// [`crate::program::EXIT_TIMEOUT`]).
pub fn serve_to_exit(platform: &Platform) -> Finished {
    run_vm_to_exit(
        [
            "serve",
            "--domain",
            DOMAIN,
            "--listen",
            "127.0.0.1:0",
            "--stand-in-sign-in",
        ],
        Some(platform.url()),
    )
}

/// The records (JSON objects, one per line) of a program's standard
/// error.
pub fn records_in(stderr: &str) -> Vec<Value> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}
