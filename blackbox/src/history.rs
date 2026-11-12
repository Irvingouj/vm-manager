//! What the tests of "Identity in the own database, and Change history"
//! share (CONTRACT.md, "Identity and Change history in the own database"):
//!
//! - the migrations the tests send: [`history_tables`] (tables with and
//!   without a primary key, a cascade, a counter for `ON CONFLICT`, a
//!   composite key in a schema other than `public`) and [`attack_helpers`]
//!   (functions an Application's migration may make, `SECURITY INVOKER`,
//!   which set any setting and then ask `vm.identity()` or write, in one
//!   statement the statement check sees only as a `SELECT`);
//! - `vm.history` read from the harness's superuser connection, and checks
//!   of one row of it ([`Change`], [`assert_change`]);
//! - the global check that every history row of an Application's change
//!   names a request, User and actor the Operation log has
//!   ([`assert_history_matches_operation_log`]);
//! - sessions of the harness's own as a given role in an own database
//!   ([`session_as`]) and attempts inside a savepoint ([`attempt`]);
//! - the platform's schema `vm` enumerated from the catalog: its functions
//!   with guessed arguments, its relations, the values the runtime role
//!   cannot read ([`secret_values`]) and their encodings ([`encodings`]).

use serde_json::{Value, json};

use crate::own_database::{Migration, Own, OwnDatabase, helpers, ident, literal};
use crate::scenario::op;
use crate::sql::{DbError, Sql};

/// The Application the history tests install.
pub const APP: &str = "board";

/// The SQLSTATE `vm.identity()` and a change with no identity raise.
pub const NO_IDENTITY: &str = "VM001";

/// The tables [`history_tables`] makes, as `schema.table`: every one is
/// captured.
pub const CAPTURED: [&str; 6] = [
    "public.things",
    "public.parents",
    "public.children",
    "public.counters",
    "public.keyless",
    "inventory.stock",
];

/// `0000_history_tables`:
///
/// - `things (id serial PK, title text not null, size integer, tags jsonb)`;
/// - `parents (id PK, name)` and `children (id PK, parent_id → parents ON
///   DELETE CASCADE, name)`, with parent 1 and its child 10 put in by this
///   migration (so not recorded);
/// - `counters (name text PK, n integer)`, for `ON CONFLICT DO UPDATE`;
/// - `keyless (a text, b integer)`, with no primary key;
/// - `inventory.stock (sku, warehouse, qty)`, keyed on `(sku, warehouse)`,
///   in a schema of its own.
pub fn history_tables() -> Migration {
    Migration::new(
        "0000_history_tables",
        &[
            r#"CREATE TABLE "things" (
	"id" serial PRIMARY KEY NOT NULL,
	"title" text NOT NULL,
	"size" integer,
	"tags" jsonb
);"#,
            r#"CREATE TABLE "parents" (
	"id" integer PRIMARY KEY NOT NULL,
	"name" text NOT NULL
);"#,
            r#"CREATE TABLE "children" (
	"id" integer PRIMARY KEY NOT NULL,
	"parent_id" integer NOT NULL,
	"name" text NOT NULL
);"#,
            r#"ALTER TABLE "children" ADD CONSTRAINT "children_parent_id_parents_id_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."parents"("id") ON DELETE cascade ON UPDATE no action;"#,
            r#"CREATE TABLE "counters" (
	"name" text PRIMARY KEY NOT NULL,
	"n" integer NOT NULL
);"#,
            r#"CREATE TABLE "keyless" (
	"a" text,
	"b" integer
);"#,
            r#"CREATE SCHEMA "inventory";"#,
            r#"CREATE TABLE "inventory"."stock" (
	"sku" text NOT NULL,
	"warehouse" text NOT NULL,
	"qty" integer NOT NULL,
	CONSTRAINT "stock_sku_warehouse_pk" PRIMARY KEY("sku","warehouse")
);"#,
            r#"INSERT INTO "parents" VALUES (1, 'made by 0000');"#,
            r#"INSERT INTO "children" VALUES (10, 1, 'child of 1, made by 0000');"#,
        ],
    )
}

/// `0002_attack_helpers`: functions that run as their caller:
///
/// - `identity_after(setting, value)` sets `setting` to `value` for the
///   transaction (nothing when `setting` is null), then answers `{ pid,
///   identity }` with what `vm.identity()` gives, or `{ pid, sqlstate,
///   message }` if anything raised;
/// - `insert_after(setting, value, title)` sets `setting` to `value` for
///   the transaction, then inserts a row of `things` with this title, and
///   answers its backend's pid.
pub fn attack_helpers() -> Migration {
    Migration::new(
        "0002_attack_helpers",
        &[
            r#"CREATE FUNCTION "identity_after"(setting text, value text) RETURNS jsonb LANGUAGE plpgsql AS $f$
DECLARE
  r record;
BEGIN
  IF setting IS NOT NULL THEN
    PERFORM set_config(setting, value, true);
  END IF;
  SELECT * INTO r FROM vm.identity();
  RETURN jsonb_build_object('pid', pg_backend_pid(), 'identity', to_jsonb(r));
EXCEPTION WHEN OTHERS THEN
  RETURN jsonb_build_object('pid', pg_backend_pid(), 'sqlstate', SQLSTATE, 'message', SQLERRM);
END
$f$;"#,
            r#"CREATE FUNCTION "insert_after"(setting text, value text, title text) RETURNS integer LANGUAGE plpgsql AS $f$
BEGIN
  PERFORM set_config(setting, value, true);
  INSERT INTO "things" ("title") VALUES (title);
  RETURN pg_backend_pid();
END
$f$;"#,
        ],
    )
}

/// The list the history tests send with the first Version:
/// [`history_tables`], [`helpers`] (`run_sql`), [`attack_helpers`].
pub fn history_standard() -> Vec<Migration> {
    vec![history_tables(), helpers(), attack_helpers()]
}

/// A call of `operation` on the own database as `actor`, with parameters.
pub fn own_call(operation: &str, actor: &str, sql: &str, params: &Value) -> Value {
    op(
        operation,
        &json!({ "own": true, "as": actor, "sql": sql, "params": params }),
    )
}

/// Every row of `vm.history`, oldest first, as `to_jsonb` of the row (so
/// with every column the table has, by name).
pub fn history(db: &OwnDatabase) -> Vec<Value> {
    history_where(db, "true")
}

/// The rows of `vm.history` that satisfy `condition`, oldest first.
pub fn history_where(db: &OwnDatabase, condition: &str) -> Vec<Value> {
    db.column(&format!(
        "SELECT to_jsonb(h)::text FROM vm.history h WHERE {condition} ORDER BY h.id"
    ))
    .iter()
    .map(|text| {
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("a row of vm.history is not JSON ({e}): {text}"))
    })
    .collect()
}

/// The rows of `vm.history` recorded after the one whose `id` is `after`.
pub fn history_after(db: &OwnDatabase, after: i64) -> Vec<Value> {
    history_where(db, &format!("h.id > {after}"))
}

/// The largest `id` in `vm.history`, 0 when it is empty.
pub fn last_history_id(db: &OwnDatabase) -> i64 {
    db.superuser()
        .value("SELECT coalesce(max(id), 0)::text FROM vm.history")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("cannot read max(id) of vm.history in {}", db.name))
}

/// The number of rows in `vm.history`.
pub fn history_count(db: &OwnDatabase) -> i64 {
    db.superuser()
        .value("SELECT count(*)::text FROM vm.history")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("cannot count vm.history in {}", db.name))
}

/// The time of the test Postgres now, as text it reads back: one end of a
/// test's window.
pub fn db_now(db: &OwnDatabase) -> String {
    db.superuser()
        .value("SELECT clock_timestamp()::text")
        .unwrap_or_default()
}

/// Fails unless every row of `vm.history` with these ids has an `at`
/// between `before` and `after` (both from [`db_now`]).
pub fn assert_at_within(
    db: &OwnDatabase,
    rows: &[Value],
    before: &str,
    after: &str,
    context: &str,
) {
    for row in rows {
        let id = &row["id"];
        let inside = db.superuser().value(&format!(
            "SELECT (at >= {}::timestamptz AND at <= {}::timestamptz)::text FROM vm.history WHERE id = {id}",
            literal(before),
            literal(after)
        ));
        assert_eq!(
            inside.as_deref(),
            Some("true"),
            "{context}: expected the history row's `at` within the test's window [{before}, {after}], got {row}"
        );
    }
}

/// A row of a table as `to_jsonb` gives it, read by the superuser; `Null`
/// when there is no such row.
pub fn row_json(db: &OwnDatabase, table: &str, condition: &str) -> Value {
    db.superuser()
        .value(&format!(
            "SELECT to_jsonb(t)::text FROM {table} t WHERE {condition}"
        ))
        .map_or(Value::Null, |text| serde_json::from_str(&text).unwrap())
}

/// Every row of a table, as `to_jsonb` texts ordered: to show it unchanged.
pub fn table_rows(db: &OwnDatabase, table: &str) -> Vec<String> {
    db.column(&format!(
        "SELECT to_jsonb(t)::text FROM {table} t ORDER BY 1"
    ))
}

/// Every row of every captured table and of `vm.history`, as one text.
pub fn snapshot(db: &OwnDatabase) -> String {
    let mut text = String::new();
    for table in CAPTURED.iter().copied().chain(["vm.history"]) {
        text.push_str(&format!("{table}: {:?}\n", table_rows(db, table)));
    }
    text
}

/// What one row of `vm.history` must say. `at` and `id` are checked
/// elsewhere.
#[derive(Clone, Debug)]
pub struct Change<'a> {
    pub schema: &'a str,
    pub table: &'a str,
    pub kind: &'a str,
    pub key: Value,
    pub old: Value,
    pub new: Value,
    pub changed: Value,
    pub user: &'a str,
    pub application: &'a str,
    pub actor: &'a str,
    pub request: Option<&'a str>,
    pub migration: Option<&'a str>,
}

impl<'a> Change<'a> {
    /// A change by an Application's operation: `user` in flight, `actor`
    /// `user` or `application`, this request.
    pub fn by(user: &'a str, actor: &'a str, request: &'a str) -> Self {
        Self {
            schema: "public",
            table: "things",
            kind: "insert",
            key: Value::Null,
            old: Value::Null,
            new: Value::Null,
            changed: Value::Null,
            user,
            application: APP,
            actor,
            request: Some(request),
            migration: None,
        }
    }

    /// A change by a migration of a Version `contributor` sent.
    pub fn by_migration(contributor: &'a str, migration: &'a str) -> Self {
        Self {
            actor: "migration",
            request: None,
            migration: Some(migration),
            ..Self::by(contributor, "migration", "")
        }
    }

    #[must_use]
    pub fn on(mut self, schema: &'a str, table: &'a str) -> Self {
        self.schema = schema;
        self.table = table;
        self
    }

    /// An insert of the row `new`, keyed `key`.
    #[must_use]
    pub fn insert(mut self, key: Value, new: Value) -> Self {
        self.kind = "insert";
        self.key = key;
        self.old = Value::Null;
        self.new = new;
        self.changed = Value::Null;
        self
    }

    /// An update from `old` to `new` of the row keyed `key` before it,
    /// these columns differing.
    #[must_use]
    pub fn update(mut self, key: Value, old: Value, new: Value, changed: Value) -> Self {
        self.kind = "update";
        self.key = key;
        self.old = old;
        self.new = new;
        self.changed = changed;
        self
    }

    /// A delete of the row `old`, keyed `key`.
    #[must_use]
    pub fn delete(mut self, key: Value, old: Value) -> Self {
        self.kind = "delete";
        self.key = key;
        self.old = old;
        self.new = Value::Null;
        self.changed = Value::Null;
        self
    }
}

/// Fails unless `row` of `vm.history` says what `want` says, column by
/// column as CONTRACT.md's table gives them.
pub fn assert_change(row: &Value, want: &Change<'_>, context: &str) {
    let mut wrong = Vec::new();
    let mut check = |column: &str, expected: Value| {
        if row.get(column) != Some(&expected) {
            wrong.push(format!(
                "  `{column}`: expected {expected}, got {}",
                row.get(column)
                    .map_or("(no such column)".to_owned(), Value::to_string)
            ));
        }
    };
    check("schema_name", json!(want.schema));
    check("table_name", json!(want.table));
    check("kind", json!(want.kind));
    check("key", want.key.clone());
    check("old", want.old.clone());
    check("new", want.new.clone());
    check("changed", want.changed.clone());
    check("user_id", json!(want.user));
    check("application", json!(want.application));
    check("actor", json!(want.actor));
    check("request", json!(want.request));
    check("migration", json!(want.migration));
    if !row["id"].is_i64() {
        wrong.push(format!("  `id`: expected a number, got {}", row["id"]));
    }
    if !row["at"].is_string() {
        wrong.push(format!("  `at`: expected a time, got {}", row["at"]));
    }
    assert!(
        wrong.is_empty(),
        "{context}: the history row is not the one expected:\n{}\nthe row: {row}",
        wrong.join("\n")
    );
}

/// Fails unless these rows' ids increase strictly, in the order given.
pub fn assert_ids_increase(rows: &[Value], context: &str) {
    let ids: Vec<i64> = rows.iter().filter_map(|r| r["id"].as_i64()).collect();
    assert!(
        ids.len() == rows.len() && ids.windows(2).all(|w| w[0] < w[1]),
        "{context}: expected the ids of the history rows to increase in the order the changes were made, got {ids:?}"
    );
}

/// Fails unless every row of `vm.history` that is not a migration's names
/// a request, User, Application and actor that one entry of the Operation
/// log has (an own-database operation of that request, made by that User
/// in flight, acting as that actor), and none names anyone else: no change
/// is ever recorded under an identity the host did not attach.
pub fn assert_history_matches_operation_log(own: &Own, db: &OwnDatabase, context: &str) {
    let log = own.log();
    let mut wrong = Vec::new();
    for row in history(db) {
        if row["actor"] == "migration" {
            if !row["request"].is_null() || !row["migration"].is_string() {
                wrong.push(format!(
                    "  a migration's row with a request or no migration: {row}"
                ));
            }
            continue;
        }
        let found = log.iter().any(|e| {
            e["request"] == row["request"]
                && e["user"] == row["user_id"]
                && e["actor"] == row["actor"]
                && e["application"] == row["application"]
                && e["own_database"] == true
        });
        if !found || row["request"].is_null() {
            wrong.push(format!("  {row}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{context}: expected every history row of an operation to name the request, User in flight, Application and actor of an Operation log entry, but these do not:\n{}",
        wrong.join("\n")
    );
}

/// A connection of the harness's superuser to `db`, become `role` with
/// `SET ROLE` (or the superuser itself when `role` is `None`); fails
/// unless `current_user` is then that role.
pub fn session_as(db: &OwnDatabase, role: Option<&str>) -> Sql {
    let sql = db.superuser();
    if let Some(role) = role {
        sql.batch(&format!("SET ROLE {}", ident(role)));
        let who = sql.value("SELECT current_user::text");
        assert_eq!(
            who.as_deref(),
            Some(role),
            "the harness's own check: the session must be {role}"
        );
    }
    sql
}

/// Runs `statement` inside a savepoint of the open transaction, rolled
/// back to if it fails, so that the transaction goes on either way.
pub fn attempt(sql: &Sql, statement: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
    sql.batch("SAVEPOINT vm_blackbox_attempt");
    let result = sql.try_coded(statement);
    if result.is_err() {
        sql.batch("ROLLBACK TO SAVEPOINT vm_blackbox_attempt");
    } else {
        sql.batch("RELEASE SAVEPOINT vm_blackbox_attempt");
    }
    result
}

/// What `SELECT * FROM vm.identity()` gives in the open transaction.
pub fn identity_now(sql: &Sql) -> Result<Vec<Vec<Option<String>>>, DbError> {
    attempt(sql, "SELECT * FROM vm.identity()")
}

/// Fails unless `vm.identity()` raises `VM001` in this session's open
/// transaction, and an insert into `things` fails with `VM001`.
pub fn assert_nothing_attached(sql: &Sql, context: &str) {
    let identity = identity_now(sql);
    assert!(
        matches!(&identity, Err(e) if e.code == NO_IDENTITY),
        "{context}: expected vm.identity() to raise {NO_IDENTITY}, got {identity:?}"
    );
    let write = attempt(
        sql,
        // An id of its own, so that a failed attempt advances no sequence.
        "INSERT INTO things (id, title) VALUES (-1, 'written with nothing attached')",
    );
    assert!(
        matches!(&write, Err(e) if e.code == NO_IDENTITY),
        "{context}: expected a write to a captured table to fail with {NO_IDENTITY}, got {write:?}"
    );
}

/// A function in schema `vm`, from the catalog.
#[derive(Clone, Debug)]
pub struct VmFunction {
    /// `vm.name(type, ...)`, as `regprocedure` prints it.
    pub signature: String,
    pub name: String,
    /// The argument types, as `format_type` prints them.
    pub arguments: Vec<String>,
    /// The result type, as `format_type` prints it.
    pub returns: String,
    /// Whether it returns a set.
    pub returns_set: bool,
}

/// Every function in schema `vm`.
pub fn vm_functions(db: &OwnDatabase) -> Vec<VmFunction> {
    db.rows(
        "SELECT p.oid::regprocedure::text, p.proname::text,
                coalesce((SELECT string_agg(format_type(t, NULL), '|' ORDER BY o)
                            FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY u(t, o)), ''),
                format_type(p.prorettype, NULL), p.proretset::text
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'vm' ORDER BY 1",
    )
    .into_iter()
    .map(|row| {
        let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        VmFunction {
            signature: text(0),
            name: text(1),
            arguments: text(2)
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            returns: text(3),
            returns_set: text(4) == "true",
        }
    })
    .collect()
}

/// Calls of `function` with guessed arguments, each a SQL expression:
/// all nulls; plausible values by position (a User, the Application, an
/// actor, a request, a secret); `ctx` (a real `vm.ctx` from another
/// transaction) in every text position; long hex and base64 guesses.
pub fn guessed_calls(function: &VmFunction, ctx: &str) -> Vec<String> {
    let plausible = ["mallory", APP, "user", "forged-request", "guessed-secret"];
    let hex_guess = "a".repeat(64);
    let b64_guess = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";
    let value = |ty: &str, i: usize, set: usize| -> String {
        let text = match set {
            0 => return format!("NULL::{ty}"),
            1 => plausible[i % plausible.len()].to_owned(),
            2 => ctx.to_owned(),
            3 => hex_guess.clone(),
            _ => b64_guess.to_owned(),
        };
        match ty {
            "text" | "character varying" | "name" | "\"char\"" | "character" | "unknown" => {
                format!("{}::{ty}", literal(&text))
            }
            "bytea" => format!("convert_to({}, 'UTF8')", literal(&text)),
            "jsonb" | "json" => {
                let doc = json!({ "user": "mallory", "app": APP, "actor": "user", "request": "forged-request", "tag": text });
                format!("{}::{ty}", literal(&doc.to_string()))
            }
            "integer" | "bigint" | "smallint" | "oid" => {
                if set == 1 {
                    "pg_backend_pid()".to_owned()
                } else {
                    format!("1::{ty}")
                }
            }
            "boolean" => "true".to_owned(),
            "uuid" => "'00000000-0000-0000-0000-000000000001'::uuid".to_owned(),
            "timestamp with time zone" | "timestamp without time zone" => {
                format!("transaction_timestamp()::{ty}")
            }
            "xid8" => "pg_current_xact_id()".to_owned(),
            other => format!("NULL::{other}"),
        }
    };
    (0..5)
        .map(|set| {
            let arguments: Vec<String> = function
                .arguments
                .iter()
                .enumerate()
                .map(|(i, ty)| value(ty, i, set))
                .collect();
            format!(
                "SELECT vm.{}({})",
                ident(&function.name),
                arguments.join(", ")
            )
        })
        .collect()
}

/// A relation in schema `vm`: its name, kind (`r`, `p`, `v`, `m`, `f`,
/// `S`) and columns.
#[derive(Clone, Debug)]
pub struct VmRelation {
    pub name: String,
    pub kind: String,
    pub columns: Vec<String>,
}

/// Every relation in schema `vm` but indexes and composite types.
pub fn vm_relations(db: &OwnDatabase) -> Vec<VmRelation> {
    db.rows(
        "SELECT c.relname::text, c.relkind::text,
                coalesce((SELECT string_agg(a.attname::text, '|' ORDER BY a.attnum) FROM pg_attribute a
                           WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped), '')
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
          ORDER BY 1",
    )
    .into_iter()
    .map(|row| {
        let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        VmRelation {
            name: text(0),
            kind: text(1),
            columns: text(2)
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    })
    .collect()
}

/// The shortest value counted as possibly secret: shorter values (small
/// numbers, flags, kinds) appear everywhere by chance.
pub const SECRET_MIN_LEN: usize = 12;

/// Every value, of `SECRET_MIN_LEN` characters or more, in a column of
/// text, bytea, JSON or uuid type of a table or materialized view of schema
/// `vm` that the runtime role cannot `SELECT`, read by the superuser (a
/// bytea as its hex digits; a JSON value whole and each string in it).
/// Values listed in `public` (what the test sent or the API answered:
/// migrations, names, hashes) are left out: they are not the platform's
/// secret, however they are stored.
pub fn secret_values(db: &OwnDatabase, public: &[String]) -> Vec<String> {
    let columns = db.rows(&format!(
        "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, NULL)
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
          WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p', 'm')
            AND NOT has_table_privilege({}, c.oid, 'SELECT')
          ORDER BY 1, 2",
        literal(&db.runtime)
    ));
    let mut values = Vec::new();
    for row in columns {
        let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let (table, column, ty) = (text(0), text(1), text(2));
        let source = format!("vm.{}", ident(&table));
        let col = ident(&column);
        let query = match ty.as_str() {
            "bytea" => format!("SELECT encode({col}, 'hex') FROM {source} WHERE {col} IS NOT NULL"),
            "jsonb" | "json" => format!(
                "SELECT {col}::text FROM {source} WHERE {col} IS NOT NULL
                 UNION ALL
                 SELECT v #>> '{{}}' FROM {source}, jsonb_path_query({col}::jsonb, 'strict $.**') v
                  WHERE jsonb_typeof(v) = 'string'"
            ),
            "text" | "character varying" | "character" | "name" | "uuid" | "\"char\"" => {
                format!("SELECT {col}::text FROM {source} WHERE {col} IS NOT NULL")
            }
            _ => continue,
        };
        for value in db.column(&query) {
            if value.chars().count() >= SECRET_MIN_LEN
                && !public.iter().any(|p| p.contains(value.as_str()))
                && !values.contains(&value)
            {
                values.push(value);
            }
        }
    }
    values
}

/// The forms a value might take in a log: itself; the hex (lower and upper
/// case) and base64 (standard and URL-safe, with and without padding) of
/// its UTF-8 bytes; and when it is itself hex or base64, the hex and base64
/// of the bytes it encodes and those bytes as text if they are UTF-8.
pub fn encodings(value: &str) -> Vec<String> {
    let mut forms = vec![value.to_owned()];
    let of_bytes = |bytes: &[u8], forms: &mut Vec<String>| {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        forms.push(hex.to_ascii_uppercase());
        forms.push(hex);
        let b64 = base64(bytes, false);
        forms.push(b64.trim_end_matches('=').to_owned());
        forms.push(b64);
        let url = base64(bytes, true);
        forms.push(url.trim_end_matches('=').to_owned());
        if let Ok(text) = std::str::from_utf8(bytes)
            && text.chars().count() >= SECRET_MIN_LEN
            && !text.chars().any(char::is_control)
        {
            forms.push(text.to_owned());
        }
    };
    of_bytes(value.as_bytes(), &mut forms);
    if let Some(bytes) = from_hex(value.trim_start_matches("\\x")) {
        of_bytes(&bytes, &mut forms);
    }
    if let Some(bytes) = from_base64(value) {
        of_bytes(&bytes, &mut forms);
    }
    forms.retain(|f| f.chars().count() >= SECRET_MIN_LEN);
    forms.sort();
    forms.dedup();
    forms
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 of `bytes`, with padding; URL-safe (`-`, `_`) when asked.
pub fn base64(bytes: &[u8], url_safe: bool) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0);
                let c = char::from(B64[index]);
                out.push(match (url_safe, c) {
                    (true, '+') => '-',
                    (true, '/') => '_',
                    (_, c) => c,
                });
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes a hex text of even length encodes, if it is one (at least 8
/// digits).
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() < 8
        || !text.len().is_multiple_of(2)
        || !text.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The bytes a base64 text (standard or URL-safe, padded or not) encodes,
/// if it is one of at least 12 characters.
pub fn from_base64(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim_end_matches('=');
    if trimmed.len() < 12 || trimmed.len() % 4 == 1 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut count = 0;
    let mut out = Vec::new();
    for c in trimmed.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_hex_round_trip() {
        assert_eq!(base64(b"hello world!", false), "aGVsbG8gd29ybGQh");
        assert_eq!(base64(b"hello", false), "aGVsbG8=");
        assert_eq!(base64(&[0xfb, 0xff], true), "-_8=");
        assert_eq!(from_base64("aGVsbG8gd29ybGQh").unwrap(), b"hello world!");
        assert!(from_hex("68656c6c6f21").is_some());
        assert_eq!(from_hex("68656c6c").unwrap(), b"hell");
        assert!(from_hex("xyz").is_none());
    }

    #[test]
    fn encodings_hold_hex_and_base64_both_ways() {
        let secret = "0123456789abcdef0123456789abcdef";
        let forms = encodings(secret);
        assert!(forms.contains(&secret.to_owned()));
        // The bytes the hex encodes, as base64.
        let bytes = from_hex(secret).unwrap();
        assert!(forms.contains(&base64(&bytes, false)));
        // The text's own bytes as hex.
        let own_hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
        assert!(forms.contains(&own_hex));
    }
}
