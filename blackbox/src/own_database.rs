//! What the tests of "An Application's own database: created with its first
//! Version, changed only by migrations" share (CONTRACT.md, "The
//! Application's own database"):
//!
//! - [`Migration`]s as `drizzle-kit` generates them (statements separated by
//!   its `--> statement-breakpoint` line), and the standard lists the tests
//!   send ([`first_tables`], [`helpers`], [`more_tables`]);
//! - [`Own`], a program with the stand-in sign-in and a Platform
//!   administrators' Group, with which a test creates Applications, sends
//!   Versions with migrations and runs `query` and `execute` on the own
//!   database through `fixtures/operations.js`, as the User or as the
//!   Application;
//! - [`OwnDatabase`], an Application's own database looked at from the
//!   harness's own superuser connection, never the program's: found by the
//!   name and comment the contract gives, its tables, rows, owners and
//!   privileges, its roles' attributes, a fingerprint of everything in it,
//!   and statements run as its runtime role (`SET ROLE` from the superuser)
//!   to ask Postgres itself what that role may do.

use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{Api, SOME_GIT_COMMIT};
use crate::http::{REQUEST_TIMEOUT, Response, User};
use crate::postgres::{Platform, shared};
use crate::scenario::{ADMINISTRATORS, administrator, lines, op};
use crate::sql::{DbError, Sql};
use crate::vm::{INSTALLER, Vm, VmOptions, fixture_bundle};

/// The line `drizzle-kit` puts between the statements of a migration file.
pub const BREAKPOINT: &str = "--> statement-breakpoint";

/// The fixture every own-database test sends as its bundle.
pub const FIXTURE: &str = "operations.js";

/// The two actors of an operation on the own database.
pub const ACTORS: [&str; 2] = ["user", "application"];

/// One migration: its name and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    pub name: String,
    pub sql: String,
}

impl Migration {
    /// A migration of these statements, in the form `drizzle-kit` writes:
    /// each statement on its own, separated by a line `--> statement-breakpoint`.
    pub fn new(name: &str, statements: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            sql: statements.join(&format!("\n{BREAKPOINT}\n")) + "\n",
        }
    }

    /// The same migration with another text.
    #[must_use]
    pub fn with_sql(&self, sql: &str) -> Self {
        Self {
            name: self.name.clone(),
            sql: sql.to_owned(),
        }
    }

    /// The same migration under another name.
    #[must_use]
    pub fn renamed(&self, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            sql: self.sql.clone(),
        }
    }

    /// `{ name, sql }`, as the API takes it.
    pub fn to_json(&self) -> Value {
        json!({ "name": self.name, "sql": self.sql })
    }
}

/// `[ { name, sql }, ... ]`.
pub fn migrations_json(migrations: &[Migration]) -> Value {
    Value::Array(migrations.iter().map(Migration::to_json).collect())
}

/// The body of a Version: this bundle, these Resources declared as (name,
/// access level), [`SOME_GIT_COMMIT`], and these migrations.
pub fn version_body_with_migrations(
    bundle: &str,
    declared: &[(&str, &str)],
    migrations: &[Migration],
) -> Value {
    let mut body = crate::api::version_body_declaring(bundle, declared);
    body["migrations"] = migrations_json(migrations);
    body
}

/// `0000_first_tables`: `items` (whose rows record who wrote them, as
/// `current_user` and `session_user`), `notes` (for the session
/// observations of [`crate::shared_database::observe_insert`]) and
/// `seeded`, holding one row. Running it twice fails: its tables exist, and
/// its row's key is taken.
pub fn first_tables() -> Migration {
    Migration::new(
        "0000_first_tables",
        &[
            r#"CREATE TABLE "items" (
	"id" serial PRIMARY KEY NOT NULL,
	"title" text NOT NULL,
	"written_by" text DEFAULT current_user NOT NULL,
	"session_login" text DEFAULT session_user NOT NULL
);"#,
            r#"CREATE TABLE "notes" (
	"id" serial PRIMARY KEY NOT NULL,
	"body" text NOT NULL
);"#,
            r#"CREATE TABLE "seeded" (
	"id" integer PRIMARY KEY NOT NULL,
	"label" text NOT NULL
);"#,
            r#"INSERT INTO "seeded" VALUES (1, 'seeded by 0000');"#,
        ],
    )
}

/// `0001_helpers`: functions an Application's migrations may make, which
/// run as their caller (`SECURITY INVOKER`, the default):
///
/// - `run_sql(text)` runs any SQL text with `EXECUTE`: a statement the
///   statement check sees only as a `SELECT` of a function, so that what
///   the runtime role may do is shown by Postgres, not by the check;
/// - `leave_session_state()` leaves in its session what a session can hold
///   (as the shared databases' function of the same name).
pub fn helpers() -> Migration {
    Migration::new(
        "0001_helpers",
        &[
            r#"CREATE FUNCTION "run_sql"(statement text) RETURNS integer LANGUAGE plpgsql AS $f$
BEGIN
  EXECUTE statement;
  RETURN 1;
END
$f$;"#,
            r#"CREATE FUNCTION "leave_session_state"() RETURNS integer LANGUAGE plpgsql AS $f$
BEGIN
  BEGIN EXECUTE 'CREATE TEMP TABLE left_table (x int)'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'PREPARE left_prepared AS SELECT 1'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'DECLARE left_cursor CURSOR WITH HOLD FOR SELECT 1'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'LISTEN left_channel'; EXCEPTION WHEN others THEN NULL; END;
  PERFORM pg_try_advisory_lock(71, 43);
  PERFORM set_config('search_path', 'left_schema, public', false);
  PERFORM set_config('statement_timeout', '4321ms', false);
  PERFORM set_config('work_mem', '7777kB', false);
  PERFORM set_config('leftover.setting', 'left-behind', false);
  RETURN pg_backend_pid();
END
$f$;"#,
        ],
    )
}

/// `0002_more_tables`: a table `extras` with a sequence, a column `extra`
/// added to `items`, and a second row in `seeded`. Running it twice fails.
pub fn more_tables() -> Migration {
    Migration::new(
        "0002_more_tables",
        &[
            r#"CREATE TABLE "extras" (
	"id" serial PRIMARY KEY NOT NULL,
	"note" text NOT NULL
);"#,
            r#"ALTER TABLE "items" ADD COLUMN "extra" text;"#,
            r#"INSERT INTO "seeded" VALUES (2, 'seeded by 0002');"#,
        ],
    )
}

/// The list most tests send with the first Version: [`first_tables`] and
/// [`helpers`].
pub fn standard() -> Vec<Migration> {
    vec![first_tables(), helpers()]
}

/// `{ own: true, as: <actor>, sql }`.
pub fn own_args(actor: &str, sql: &str) -> Value {
    json!({ "own": true, "as": actor, "sql": sql })
}

/// A call of `query` on the own database as `actor`.
pub fn own_query(actor: &str, sql: &str) -> Value {
    op("query", &own_args(actor, sql))
}

/// A call of `execute` on the own database as `actor`.
pub fn own_execute(actor: &str, sql: &str) -> Value {
    op("execute", &own_args(actor, sql))
}

/// An identifier quoted for Postgres.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A string literal for Postgres.
pub fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The comment CONTRACT.md gives an own database: `vm application <name>`.
pub fn comment_of(application: &str) -> String {
    format!("vm application {application}")
}

/// Every database on the shared Postgres named `<p>_app_<digits>` for this
/// platform database `<p>`, with its comment (empty when none).
pub fn own_databases_of(platform: &Platform) -> Vec<(String, String)> {
    let superuser = Sql::connect(&shared().url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
    let prefix = format!("{}_app_", platform.name());
    superuser
        .batch(
            "SELECT datname, coalesce(shobj_description(oid, 'pg_database'), '')
               FROM pg_database ORDER BY datname",
        )
        .into_iter()
        .filter_map(|row| {
            let name = row.first().cloned().flatten()?;
            let comment = row.get(1).cloned().flatten().unwrap_or_default();
            let number = name.strip_prefix(&prefix)?;
            (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
                .then_some((name, comment))
        })
        .collect()
}

/// An Application's own database, seen from outside the program.
#[derive(Clone, Debug)]
pub struct OwnDatabase {
    /// `<p>_app_<n>`.
    pub name: String,
    /// `<p>_app_<n>_owner`.
    pub owner: String,
    /// `<p>_app_<n>_runtime`.
    pub runtime: String,
    pub application: String,
    /// `<p>`, the platform database's name.
    pub platform: String,
}

impl OwnDatabase {
    /// The own database of `application` of the program on `platform`, if
    /// there is one: the database `<p>_app_<n>` whose comment is `vm
    /// application <name>`. Fails if there are several.
    pub fn find(platform: &Platform, application: &str) -> Option<Self> {
        let comment = comment_of(application);
        let found: Vec<String> = own_databases_of(platform)
            .into_iter()
            .filter(|(_, c)| *c == comment)
            .map(|(name, _)| name)
            .collect();
        match found.as_slice() {
            [] => None,
            [name] => Some(Self {
                owner: format!("{name}_owner"),
                runtime: format!("{name}_runtime"),
                name: name.clone(),
                application: application.to_owned(),
                platform: platform.name().to_owned(),
            }),
            several => panic!(
                "expected one own database of {application:?} (named {}_app_<n>, comment {comment:?}), found several: {several:?}",
                platform.name()
            ),
        }
    }

    /// The harness's superuser, connected to this database.
    pub fn superuser(&self) -> Sql {
        Sql::connect(&shared().url(&self.name))
            .unwrap_or_else(|e| panic!("cannot reach the own database {}: {e}", self.name))
    }

    /// The harness's superuser, connected to `postgres`: for questions about
    /// the whole instance.
    pub fn instance(&self) -> Sql {
        Sql::connect(&shared().url("postgres"))
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"))
    }

    /// Runs `sql` as the superuser in this database; values as text.
    pub fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        self.superuser().batch(sql)
    }

    /// The rows of `sql` as one value per row, `None` as `NULL`.
    pub fn column(&self, sql: &str) -> Vec<String> {
        self.rows(sql)
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .next()
                    .flatten()
                    .unwrap_or_else(|| "NULL".to_owned())
            })
            .collect()
    }

    /// Whether `role` exists on the instance.
    pub fn role_exists(&self, role: &str) -> bool {
        self.instance()
            .value(&format!(
                "SELECT count(*) FROM pg_roles WHERE rolname = {}",
                literal(role)
            ))
            .as_deref()
            == Some("1")
    }

    /// The tables and partitioned tables in this database outside Postgres's
    /// own schemas, as `schema.table`, ordered.
    pub fn tables(&self) -> Vec<String> {
        self.column(
            "SELECT n.nspname || '.' || c.relname FROM pg_class c
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE c.relkind IN ('r', 'p')
                AND n.nspname NOT IN ('pg_catalog', 'information_schema')
                AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%'
              ORDER BY 1",
        )
    }

    /// Whether `public.<table>` is there.
    pub fn has_table(&self, table: &str) -> bool {
        self.tables().contains(&format!("public.{table}"))
    }

    /// Every object in this database outside Postgres's own schemas that
    /// is NOT owned by the owner role, as `kind name owner`: relations,
    /// schemas made by migrations, functions, types. Large objects are left
    /// out: any role may create one, which CONTRACT.md names as not closed.
    pub fn objects_not_owned_by_the_owner(&self) -> Vec<String> {
        let owner = literal(&self.owner);
        self.column(&format!(
            "SELECT 'relation ' || n.nspname || '.' || c.relname || ' ' || pg_get_userbyid(c.relowner)
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
                AND n.nspname NOT LIKE 'pg\\_toast%'
                AND pg_get_userbyid(c.relowner) <> {owner}
             UNION ALL
             SELECT 'function ' || p.oid::regprocedure::text || ' ' || pg_get_userbyid(p.proowner)
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
                AND pg_get_userbyid(p.proowner) <> {owner}
             UNION ALL
             SELECT 'type ' || n.nspname || '.' || t.typname || ' ' || pg_get_userbyid(t.typowner)
               FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
                AND n.nspname NOT LIKE 'pg\\_toast%'
                AND pg_get_userbyid(t.typowner) <> {owner}
             UNION ALL
             SELECT 'schema ' || n.nspname || ' ' || pg_get_userbyid(n.nspowner)
               FROM pg_namespace n
              WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'public')
                AND n.nspname NOT LIKE 'pg\\_%'
                AND pg_get_userbyid(n.nspowner) <> {owner}
             ORDER BY 1"
        ))
    }

    /// Everything `role` owns in this database (relations, functions,
    /// types, schemas) and whether it owns the database itself, as `kind
    /// name`. Large objects are left out: any role may create one, which
    /// CONTRACT.md names as not closed.
    pub fn owned_by(&self, role: &str) -> Vec<String> {
        let role = literal(role);
        self.column(&format!(
            "SELECT 'relation ' || c.oid::regclass::text FROM pg_class c WHERE pg_get_userbyid(c.relowner) = {role}
             UNION ALL
             SELECT 'function ' || p.oid::regprocedure::text FROM pg_proc p WHERE pg_get_userbyid(p.proowner) = {role}
             UNION ALL
             SELECT 'type ' || t.oid::regtype::text FROM pg_type t WHERE pg_get_userbyid(t.typowner) = {role}
             UNION ALL
             SELECT 'schema ' || n.nspname FROM pg_namespace n WHERE pg_get_userbyid(n.nspowner) = {role}
             UNION ALL
             SELECT 'database ' || datname FROM pg_database WHERE pg_get_userbyid(datdba) = {role}
             ORDER BY 1"
        ))
    }

    /// Everything a statement could change in this database, as one text:
    /// the database's owner, privileges, comment and settings; its schemas,
    /// relations (with kind, owner and privileges), columns, constraints,
    /// indexes, functions, types, triggers, rules, default privileges,
    /// extensions, large objects; the rows of every table and the state of
    /// every sequence; the owner and runtime roles' attributes, settings
    /// and memberships.
    pub fn fingerprint(&self) -> String {
        let sql = self.superuser();
        let roles = format!("{}, {}", literal(&self.owner), literal(&self.runtime));
        let user_schemas = "n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_toast%'";
        let parts: Vec<(&str, String)> = vec![
            (
                "database",
                "SELECT d.datname || ' owner=' || pg_get_userbyid(d.datdba)
                        || ' acl=' || coalesce(array_to_string(d.datacl, ';'), '')
                        || ' comment=' || coalesce(shobj_description(d.oid, 'pg_database'), '')
                   FROM pg_database d WHERE d.datname = current_database()"
                    .to_owned(),
            ),
            (
                "settings",
                format!(
                    "SELECT string_agg(coalesce(r.rolname, '*') || '@' || s.setdatabase::text || '=' || array_to_string(s.setconfig, ';'), ','
                                       ORDER BY coalesce(r.rolname, '*'), s.setdatabase)
                       FROM pg_db_role_setting s LEFT JOIN pg_roles r ON r.oid = s.setrole
                      WHERE s.setdatabase = (SELECT oid FROM pg_database WHERE datname = current_database())
                         OR r.rolname IN ({roles})"
                ),
            ),
            (
                "schemas",
                "SELECT string_agg(n.nspname || ' owner=' || pg_get_userbyid(n.nspowner)
                                   || ' acl=' || coalesce(array_to_string(n.nspacl, ';'), ''), ',' ORDER BY n.nspname)
                   FROM pg_namespace n
                  WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'"
                    .to_owned(),
            ),
            (
                "relations",
                format!(
                    "SELECT string_agg(n.nspname || '.' || c.relname || ':' || c.relkind::text
                                       || ' owner=' || pg_get_userbyid(c.relowner)
                                       || ' acl=' || coalesce(array_to_string(c.relacl, ';'), ''), ','
                                       ORDER BY n.nspname, c.relname)
                       FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE {user_schemas}"
                ),
            ),
            (
                "columns",
                format!(
                    "SELECT string_agg(n.nspname || '.' || c.relname || '.' || a.attname || ' '
                                       || format_type(a.atttypid, a.atttypmod)
                                       || CASE WHEN a.attnotnull THEN ' not null' ELSE '' END
                                       || coalesce(' default ' || pg_get_expr(ad.adbin, ad.adrelid), '')
                                       || ' acl=' || coalesce(array_to_string(a.attacl, ';'), ''), ','
                                       ORDER BY n.nspname, c.relname, a.attnum)
                       FROM pg_attribute a
                       JOIN pg_class c ON c.oid = a.attrelid
                       JOIN pg_namespace n ON n.oid = c.relnamespace
                       LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
                      WHERE a.attnum > 0 AND NOT a.attisdropped AND {user_schemas}"
                ),
            ),
            (
                "constraints",
                "SELECT string_agg(co.conrelid::regclass::text || '.' || co.conname || ' ' || pg_get_constraintdef(co.oid), ','
                                   ORDER BY co.conrelid::regclass::text, co.conname)
                   FROM pg_constraint co JOIN pg_namespace n ON n.oid = co.connamespace
                  WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')"
                    .to_owned(),
            ),
            (
                "indexes",
                format!(
                    "SELECT string_agg(pg_get_indexdef(i.indexrelid), ',' ORDER BY pg_get_indexdef(i.indexrelid))
                       FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid
                       JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE {user_schemas}"
                ),
            ),
            (
                "functions",
                "SELECT string_agg(p.oid::regprocedure::text || ' owner=' || pg_get_userbyid(p.proowner)
                                   || ' definer=' || p.prosecdef::text
                                   || ' acl=' || coalesce(array_to_string(p.proacl, ';'), '')
                                   || ' src=' || md5(coalesce(p.prosrc, '')), ','
                                   ORDER BY p.oid::regprocedure::text)
                   FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                  WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')"
                    .to_owned(),
            ),
            (
                "types",
                format!(
                    "SELECT string_agg(n.nspname || '.' || t.typname || ' owner=' || pg_get_userbyid(t.typowner), ','
                                       ORDER BY n.nspname, t.typname)
                       FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                      WHERE {user_schemas}"
                ),
            ),
            (
                "triggers",
                "SELECT string_agg(tgrelid::regclass::text || '.' || tgname || ' ' || tgenabled::text, ','
                                   ORDER BY tgrelid::regclass::text, tgname)
                   FROM pg_trigger WHERE NOT tgisinternal"
                    .to_owned(),
            ),
            (
                "rules",
                "SELECT string_agg(schemaname || '.' || tablename || '.' || rulename, ',' ORDER BY schemaname, tablename, rulename)
                   FROM pg_rules WHERE schemaname NOT IN ('pg_catalog', 'information_schema')"
                    .to_owned(),
            ),
            (
                "default privileges",
                "SELECT string_agg(pg_get_userbyid(defaclrole) || ':' || defaclnamespace::text || ':' || defaclobjtype::text
                                   || ':' || array_to_string(defaclacl, ';'), ','
                                   ORDER BY pg_get_userbyid(defaclrole), defaclnamespace, defaclobjtype)
                   FROM pg_default_acl"
                    .to_owned(),
            ),
            (
                "extensions",
                "SELECT string_agg(extname || ' ' || extversion, ',' ORDER BY extname) FROM pg_extension".to_owned(),
            ),
            (
                "event triggers",
                "SELECT string_agg(evtname, ',' ORDER BY evtname) FROM pg_event_trigger".to_owned(),
            ),
            (
                "large objects",
                "SELECT count(*)::text || ' ' || coalesce(string_agg(oid::text || ':' || pg_get_userbyid(lomowner), ',' ORDER BY oid), '')
                   FROM pg_largeobject_metadata"
                    .to_owned(),
            ),
            (
                "roles",
                format!(
                    "SELECT string_agg(rolname || ' super=' || rolsuper::text || ' createdb=' || rolcreatedb::text
                                       || ' createrole=' || rolcreaterole::text || ' login=' || rolcanlogin::text
                                       || ' replication=' || rolreplication::text || ' bypassrls=' || rolbypassrls::text
                                       || ' inherit=' || rolinherit::text || ' connlimit=' || rolconnlimit::text
                                       || ' config=' || coalesce(array_to_string(rolconfig, ';'), ''), ','
                                       ORDER BY rolname)
                       FROM pg_roles WHERE rolname IN ({roles})"
                ),
            ),
            (
                "memberships",
                format!(
                    "SELECT string_agg(r.rolname || '>' || m.rolname, ',' ORDER BY r.rolname, m.rolname)
                       FROM pg_auth_members a JOIN pg_roles r ON r.oid = a.roleid JOIN pg_roles m ON m.oid = a.member
                      WHERE m.rolname IN ({roles}) OR r.rolname IN ({roles})"
                ),
            ),
        ];
        let mut text = String::new();
        for (what, query) in parts {
            let value = sql.value(&query).unwrap_or_default();
            text.push_str(&format!("{what}: {value}\n"));
        }
        // The rows of every table, and every sequence's state.
        let relations = sql.batch(
            "SELECT format('%I.%I', n.nspname, c.relname), c.relkind::text FROM pg_class c
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE c.relkind IN ('r', 'p', 'S', 'm')
                AND n.nspname NOT IN ('pg_catalog', 'information_schema')
                AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%'
              ORDER BY 1",
        );
        for row in relations {
            let (Some(Some(name)), Some(Some(kind))) = (row.first(), row.get(1)) else {
                continue;
            };
            let query = if kind == "S" {
                format!("SELECT last_value::text || ' ' || is_called::text FROM {name}")
            } else {
                format!(
                    "SELECT coalesce(string_agg(t::text, '|' ORDER BY t::text), '') FROM {name} t"
                )
            };
            let value = sql.value(&query).unwrap_or_default();
            text.push_str(&format!("rows of {name}: {value}\n"));
        }
        text
    }

    /// Runs each statement in this database as the runtime role (the
    /// superuser's connection after `SET ROLE`), each on its own, and
    /// returns what Postgres answered to each. Fails if the connection is
    /// not the runtime role before every statement: Postgres itself, not the
    /// program's statement check, says what the role may do.
    pub fn as_runtime(
        &self,
        statements: &[String],
    ) -> Vec<Result<Vec<Vec<Option<String>>>, DbError>> {
        let sql = self.superuser();
        sql.batch(&format!("SET ROLE {}", ident(&self.runtime)));
        statements
            .iter()
            .map(|statement| {
                let who = sql.value("SELECT current_user::text");
                assert_eq!(
                    who.as_deref(),
                    Some(self.runtime.as_str()),
                    "the harness's own check: before {statement:?} the connection must be the runtime role"
                );
                sql.try_coded(statement)
            })
            .collect()
    }

    /// Whether `role` has `privilege` on the database `database`, as
    /// Postgres's `has_database_privilege` says.
    pub fn has_database_privilege(&self, role: &str, database: &str, privilege: &str) -> bool {
        self.instance()
            .value(&format!(
                "SELECT has_database_privilege({}, {}, {})::text",
                literal(role),
                literal(database),
                literal(privilege)
            ))
            .as_deref()
            == Some("true")
    }

    /// Sets a password the harness knows on `role`, so that a test can try
    /// to log in as it. The program's own connections already open are not
    /// affected; new ones of the program as that role will fail, so a test
    /// does this last.
    pub fn set_known_password(&self, role: &str) -> String {
        let password = format!("pw-known-{}", crate::shared_database::token());
        self.instance().batch(&format!(
            "ALTER ROLE {} PASSWORD {}",
            ident(role),
            literal(&password)
        ));
        password
    }
}

/// Changes of structure, and the other things the runtime role may not do,
/// each as one statement that Postgres answers with an error when the role
/// may not. Left out, because Postgres lets any role do them whatever the
/// platform sets up: `ALTER ROLE <itself> SET ...`, creating a large
/// object, and `GRANT`/`REVOKE` of privileges held without grant option
/// (a warning, not an error; the fingerprint shows they change nothing).
/// Nor `SET ROLE`: run from the harness's `SET ROLE` connection, Postgres
/// judges it by the session's user, the superuser; that the runtime role
/// is a member of no role is asked of `pg_has_role` instead.
pub fn forbidden_statements(db: &OwnDatabase) -> Vec<String> {
    vec![
        "CREATE TABLE sneaky (x int)".to_owned(),
        "CREATE TABLE public.sneaky (x int)".to_owned(),
        "CREATE TEMP TABLE sneaky_temp (x int)".to_owned(),
        "CREATE TEMPORARY TABLE pg_temp.sneaky_temp2 (x int)".to_owned(),
        "CREATE UNLOGGED TABLE sneaky_unlogged (x int)".to_owned(),
        "CREATE TABLE sneaky_copy AS SELECT * FROM items".to_owned(),
        "SELECT * INTO sneaky_into FROM items".to_owned(),
        "CREATE VIEW sneaky_view AS SELECT 1 AS x".to_owned(),
        "CREATE SEQUENCE sneaky_seq".to_owned(),
        "CREATE SCHEMA sneaky_schema".to_owned(),
        "CREATE FUNCTION sneaky_fn() RETURNS int LANGUAGE sql AS 'SELECT 1'".to_owned(),
        "CREATE FUNCTION pg_temp.sneaky_temp_fn() RETURNS int LANGUAGE sql AS 'SELECT 1'".to_owned(),
        "CREATE TYPE sneaky_type AS (x int)".to_owned(),
        "CREATE INDEX sneaky_idx ON items (title)".to_owned(),
        "ALTER TABLE items ADD COLUMN sneaky text".to_owned(),
        "ALTER TABLE items DROP COLUMN title".to_owned(),
        "ALTER TABLE items RENAME TO renamed_items".to_owned(),
        "ALTER TABLE items OWNER TO CURRENT_USER".to_owned(),
        "ALTER TABLE items DISABLE TRIGGER ALL".to_owned(),
        "ALTER SEQUENCE items_id_seq RESTART WITH 1000".to_owned(),
        "ALTER FUNCTION run_sql(text) SECURITY DEFINER".to_owned(),
        "DROP TABLE seeded".to_owned(),
        "DROP TABLE items CASCADE".to_owned(),
        "DROP FUNCTION run_sql(text)".to_owned(),
        "DROP SCHEMA public CASCADE".to_owned(),
        "TRUNCATE seeded".to_owned(),
        "TRUNCATE items RESTART IDENTITY CASCADE".to_owned(),
        "COMMENT ON TABLE items IS 'sneaky'".to_owned(),
        "CREATE POLICY sneaky_policy ON items USING (true)".to_owned(),
        "ALTER TABLE items ENABLE ROW LEVEL SECURITY".to_owned(),
        "CREATE TRIGGER sneaky_trigger BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION run_sql()".to_owned(),
        "CREATE RULE sneaky_rule AS ON INSERT TO items DO INSTEAD NOTHING".to_owned(),
        "CREATE EXTENSION IF NOT EXISTS pg_trgm".to_owned(),
        format!("ALTER DATABASE {} SET work_mem = '64MB'", ident(&db.name)),
        format!("ALTER ROLE {} CREATEDB", ident(&db.runtime)),
        format!("GRANT {} TO {}", ident(&db.owner), ident(&db.runtime)),
        "REINDEX TABLE items".to_owned(),
        "CLUSTER items USING items_pkey".to_owned(),
    ]
}

/// The statements of [`forbidden_statements`] that `run_sql` can run (not
/// `SELECT ... INTO`, which plpgsql's `EXECUTE` does not take).
pub fn runnable_by_run_sql(db: &OwnDatabase) -> Vec<String> {
    forbidden_statements(db)
        .into_iter()
        .filter(|s| !s.starts_with("SELECT * INTO"))
        .collect()
}

/// Tries to log in as `role` with `password` to `database` on the shared
/// Postgres, and runs `SELECT 1`. `Err` holds Postgres's SQLSTATE (`42501`
/// for "permission denied for database", `28P01` for a wrong password).
pub fn try_login(role: &str, password: &str, database: &str) -> Result<(), DbError> {
    let sql = Sql::connect_coded(&shared().url_as(role, password, database))?;
    sql.try_coded("SELECT 1").map(|_| ())
}

/// A program with the stand-in sign-in and [`ADMINISTRATORS`] as its
/// administrators' Group, in which the tests make Applications with own
/// databases.
pub struct Own {
    pub vm: Vm,
}

impl Own {
    /// A program on a fresh platform database, nothing installed.
    pub fn start() -> Self {
        Self::start_with(&VmOptions::new())
    }

    /// As [`Own::start`], from these options (a platform database of the
    /// test's choosing).
    pub fn start_with(options: &VmOptions) -> Self {
        let options = options
            .clone()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS);
        Self {
            vm: Vm::start(&options),
        }
    }

    /// The platform's API as [`INSTALLER`], the Contributor of every
    /// Application the tests make.
    pub fn api(&self) -> Api<'_> {
        self.vm.api_as(&User::new(INSTALLER))
    }

    /// Creates the Application `name`, failing unless `201`.
    pub fn create(&self, name: &str) {
        let created = self.api().create_application(name);
        if created.status != 201 {
            self.vm.fail(&format!(
                "expected `201` creating the Application {name:?}, got\n{}",
                created.describe()
            ));
        }
    }

    /// Sends a Version of `name`: [`FIXTURE`] as its bundle, nothing
    /// declared, these migrations.
    pub fn send(&self, name: &str, migrations: &[Migration]) -> Response {
        self.send_body(
            name,
            &version_body_with_migrations(&fixture_bundle(FIXTURE), &[], migrations),
        )
    }

    /// Sends this body as a Version of `name`, waiting long enough for slow
    /// migrations.
    pub fn send_body(&self, name: &str, body: &Value) -> Response {
        self.api()
            .request(&format!("/v1/applications/{name}/versions"))
            .method("POST")
            .json(body)
            .timeout(Duration::from_secs(60))
            .send()
    }

    /// As [`Own::send`], failing unless `201`; returns the Version's record.
    pub fn send_ok(&self, name: &str, migrations: &[Migration]) -> Value {
        let sent = self.send(name, migrations);
        if sent.status != 201 {
            self.vm.fail(&format!(
                "expected `201` sending a Version of {name:?} with the migrations {:?}, got\n{}",
                names(migrations),
                sent.describe()
            ));
        }
        sent.json()
    }

    /// Creates `name` and sends its first Version with these migrations,
    /// failing unless both are `201`; returns its own database.
    pub fn install(&self, name: &str, migrations: &[Migration]) -> OwnDatabase {
        self.create(name);
        self.send_ok(name, migrations);
        self.own_database(name)
    }

    /// The own database of `name`, which must be there.
    pub fn own_database(&self, name: &str) -> OwnDatabase {
        self.find_own_database(name).unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected the own database of {name:?}: a database {}_app_<n> whose comment is {:?}; the databases {}_app_<n> are {:?}",
                self.vm.platform().name(),
                comment_of(name),
                self.vm.platform().name(),
                own_databases_of(self.vm.platform())
            ))
        })
    }

    /// The own database of `name`, if there is one.
    pub fn find_own_database(&self, name: &str) -> Option<OwnDatabase> {
        OwnDatabase::find(self.vm.platform(), name)
    }

    /// Runs these calls through [`FIXTURE`] in `application` as `user`, one
    /// after another; fails unless the request is answered `200` with
    /// `{ answers }`.
    pub fn run(&self, user: &User, application: &str, calls: &[Value]) -> (Response, Vec<Value>) {
        self.run_body(
            user,
            application,
            &json!({ "calls": calls }),
            REQUEST_TIMEOUT,
        )
    }

    /// As [`Own::run`] with this whole body (see the fixture's header for
    /// `mode` and the rest), waiting up to `timeout`.
    pub fn run_body(
        &self,
        user: &User,
        application: &str,
        body: &Value,
        timeout: Duration,
    ) -> (Response, Vec<Value>) {
        let response = self
            .vm
            .request(application, "/")
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(timeout)
            .send();
        if response.status != 200 {
            self.vm.fail(&format!(
                "expected `200` from {application:?} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        let answers = json["answers"].as_array().cloned().unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected {{ answers: [...] }} from {application:?}, got {json}"
            ))
        });
        (response, answers)
    }

    /// Requests sent at once from as many threads, each `(user,
    /// application, body)` as for [`Own::run_body`]; returns each one's
    /// answer and the answers of its calls, in the order given.
    pub fn at_once(&self, requests: &[(User, &str, Value)]) -> Vec<(Response, Vec<Value>)> {
        let start = std::sync::Barrier::new(requests.len());
        std::thread::scope(|scope| {
            let handles: Vec<_> = requests
                .iter()
                .map(|(user, application, body)| {
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        self.run_body(user, application, body, Duration::from_secs(20))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| panic!("a request thread failed"))
                })
                .collect()
        })
    }

    /// One call; returns the request's answer and what the host answered.
    pub fn one(&self, user: &User, application: &str, call: &Value) -> (Response, Value) {
        let (response, answers) = self.run(user, application, std::slice::from_ref(call));
        (response, answers.into_iter().next().unwrap_or(Value::Null))
    }

    /// The number of the Version serving `application`, as `whoami` in a
    /// request says it.
    pub fn serving(&self, user: &User, application: &str) -> u64 {
        let (_, answer) = self.one(user, application, &op("whoami", &json!({})));
        answer["ok"]["version"].as_u64().unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected whoami to name the serving Version of {application:?}, got {answer}"
            ))
        })
    }

    /// The numbers of the Versions of `application`, in order.
    pub fn version_numbers(&self, application: &str) -> Vec<u64> {
        self.api()
            .versions(application)
            .iter()
            .filter_map(|v| v["number"].as_u64())
            .collect()
    }

    /// Every entry of the Operation log.
    pub fn log(&self) -> Vec<Value> {
        self.vm.api_as(&administrator()).operation_log_entries()
    }

    /// The entries of the Operation log for the request this answers.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        let id = response.request_id();
        self.log()
            .into_iter()
            .filter(|e| e["request"].as_str() == Some(id.as_str()))
            .collect()
    }

    /// The one entry of the request this answers.
    pub fn entry_of(&self, response: &Response) -> Value {
        let entries = self.entries_of(response);
        match entries.as_slice() {
            [entry] => entry.clone(),
            _ => self.vm.fail(&format!(
                "expected exactly one Operation log entry for request {}, got {}:\n{}",
                response.request_id(),
                entries.len(),
                lines(&entries)
            )),
        }
    }
}

/// The names of these migrations.
pub fn names(migrations: &[Migration]) -> Vec<&str> {
    migrations.iter().map(|m| m.name.as_str()).collect()
}

/// A body of [`SOME_GIT_COMMIT`] and the [`FIXTURE`] bundle with this
/// `migrations` value, whatever it is (for malformed lists).
pub fn version_body_with_raw_migrations(migrations: &Value) -> Value {
    json!({
        "bundle": fixture_bundle(FIXTURE),
        "declared_access": { "resources": {} },
        "git_commit": SOME_GIT_COMMIT,
        "migrations": migrations,
    })
}

/// Fails unless `answer` ran: `{ ok: ... }`.
pub fn assert_ok(answer: &Value, context: &str) {
    assert!(
        answer["ok"].is_object(),
        "{context}: expected the operation to run and answer {{ ok: ... }}, got {answer}"
    );
}

/// Fails unless `answer` did not change anything: a database error, a
/// refusal of the statement check, or a refusal. Returns the SQLSTATE when
/// it failed in the database.
pub fn assert_did_not_run(answer: &Value, context: &str) -> Option<String> {
    assert!(
        answer.get("ok").is_none(),
        "{context}: expected the statement to fail in the database or be refused, but it ran: {answer}"
    );
    assert!(
        answer["error"].is_object() || answer["refused"].is_object(),
        "{context}: expected {{ error }} or {{ refused }}, got {answer}"
    );
    (answer["error"]["kind"] == "database").then(|| {
        answer["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    })
}
