//! What the tests of "The platform's part of an own database is out of a
//! migration's reach" share (CONTRACT.md; ticket "The Change history is
//! kept out of a Contributor's reach"):
//!
//! - [`start`]: a program whose platform role may only create databases
//!   and roles, as the contract says it needs;
//! - [`base`] and [`install_seeded`]: an Application whose first Version
//!   made captured tables (a partitioned one among them) and a sequence
//!   `stash` (a value written into a sequence outlives a rollback, so it is
//!   where a migration would carry something out of a refused Version), and
//!   whose tables then got rows, and `vm.history` rows, from operations;
//! - [`untouched`]: everything a hostile migration might change, as one
//!   text: the Application's part of the database ([`OwnDatabase::fingerprint`]:
//!   its objects, every table's rows, every sequence's state, its roles) and
//!   the platform's part (a digest of the keys, every row of every other
//!   table of `vm`, the history and the record of migrations among them;
//!   the owner, rights, settings and body of every function of `vm`; every
//!   relation and type of `vm`; every trigger of every table, by name,
//!   enabling, function and object id; the platform's role and its
//!   memberships). [`assert_untouched`] compares it and checks that no
//!   Version was recorded and Version 1 still serves;
//! - [`Case`] and [`run_cases`]: one fresh Application per hostile Version,
//!   so that an attack that succeeds today cannot spoil the next one, with
//!   every failure collected and reported together;
//! - the capture triggers read from the catalog ([`capture_triggers`]):
//!   the triggers of a table whose function is in schema `vm`, whatever the
//!   platform names them.

use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::Value;

use crate::api::assert_api_error;
use crate::history::history_standard;
use crate::http::{Response, User};
use crate::own_database::{Migration, Own, OwnDatabase, literal, own_execute};
use crate::postgres::Platform;
use crate::vm::VmOptions;

/// The User whose operations write the rows the hostile Versions must
/// leave alone.
pub const WRITER: &str = "alice";

/// The name of the migration the Version before a hostile one ends with.
pub const BASE_LAST: &str = "0003_reach_base";

/// A program on a fresh platform database whose role is `LOGIN CREATEDB
/// CREATEROLE` and nothing more (CONTRACT.md: "The platform still needs
/// only `CREATEDB` and `CREATEROLE`").
pub fn start() -> Own {
    Own::start_with(&VmOptions::new().platform(&Platform::fresh_with_limited_role()))
}

/// `<p>_app_<n>_platform`: the platform's own role for this database.
pub fn platform_role(db: &OwnDatabase) -> String {
    format!("{}_platform", db.name)
}

/// `0003_reach_base`: a sequence `stash`, and a partitioned table
/// `measures (id, k, v)` keyed on `(id, k)` with two partitions,
/// `measures_low` (k from 0 to 100) and `measures_high` (100 to 200).
pub fn reach_base() -> Migration {
    Migration::new(
        BASE_LAST,
        &[
            r#"CREATE SEQUENCE "stash";"#,
            r#"CREATE TABLE "measures" (
	"id" integer NOT NULL,
	"k" integer NOT NULL,
	"v" text,
	CONSTRAINT "measures_id_k_pk" PRIMARY KEY("id","k")
) PARTITION BY RANGE ("k");"#,
            r#"CREATE TABLE "measures_low" PARTITION OF "measures" FOR VALUES FROM (0) TO (100);"#,
            r#"CREATE TABLE "measures_high" PARTITION OF "measures" FOR VALUES FROM (100) TO (200);"#,
        ],
    )
}

/// The first Version's migrations: [`history_standard`] and [`reach_base`].
pub fn base() -> Vec<Migration> {
    let mut list = history_standard();
    list.push(reach_base());
    list
}

/// [`base`] followed by these pending migrations.
pub fn base_and(pending: &[Migration]) -> Vec<Migration> {
    let mut list = base();
    list.extend_from_slice(pending);
    list
}

/// Writes rows through `application` as [`WRITER`], so that its tables and
/// `vm.history` hold rows a hostile Version must leave alone. Every write
/// names its key, so that no sequence of the Application's moves.
pub fn seed(own: &Own, application: &str) {
    let calls = [
        "insert into things (id, title, size, tags) values (101, 'first thing', 1, '{\"a\": 1}'), (102, 'second thing', 2, null)",
        "update things set size = 3 where id = 102",
        "insert into parents (id, name) values (2, 'second parent')",
        "insert into children (id, parent_id, name) values (20, 2, 'child of 2')",
        "insert into counters (name, n) values ('hits', 1)",
        "insert into keyless (a, b) values ('kept', 1), ('gone', 2)",
        "delete from keyless where a = 'gone'",
        "insert into inventory.stock (sku, warehouse, qty) values ('sku-1', 'north', 5)",
        "insert into measures (id, k, v) values (1, 5, 'low'), (2, 150, 'high')",
    ];
    let calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, sql)| own_execute(if i % 2 == 0 { "user" } else { "application" }, sql))
        .collect();
    let (_, answers) = own.run(&User::new(WRITER), application, &calls);
    for (answer, call) in answers.iter().zip(&calls) {
        assert!(
            answer["ok"].is_object(),
            "the harness's own seeding of {application:?}: expected {call} to run, got {answer}"
        );
    }
}

/// Creates `application`, sends [`base`] as its first Version (`201`), and
/// [`seed`]s it; returns its own database.
pub fn install_seeded(own: &Own, application: &str) -> OwnDatabase {
    let db = own.install(application, &base());
    seed(own, application);
    db
}

/// A capture trigger: a trigger of a table outside `vm` whose function is
/// in schema `vm`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTrigger {
    /// The table, as `regclass` prints it (`things`, `inventory.stock`).
    pub table: String,
    pub name: String,
    /// `pg_trigger.tgtype`.
    pub tgtype: i64,
    /// `pg_get_triggerdef`: the statement that makes it.
    pub definition: String,
    /// The function, as `regprocedure` prints it (`vm.capture()`).
    pub function: String,
}

/// `tgtype` bits of the events a trigger fires on.
pub const ON_INSERT: i64 = 4;
pub const ON_DELETE: i64 = 8;
pub const ON_UPDATE: i64 = 16;
pub const ON_TRUNCATE: i64 = 32;

/// Every capture trigger in `db`, by table and name.
pub fn capture_triggers(db: &OwnDatabase) -> Vec<CaptureTrigger> {
    db.rows(
        "SELECT t.tgrelid::regclass::text, t.tgname::text, t.tgtype::text,
                pg_get_triggerdef(t.oid), t.tgfoid::regprocedure::text
           FROM pg_trigger t
           JOIN pg_class c ON c.oid = t.tgrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_proc p ON p.oid = t.tgfoid
          WHERE NOT t.tgisinternal AND n.nspname <> 'vm'
            AND p.pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'vm')
          ORDER BY 1, 2",
    )
    .into_iter()
    .map(|row| {
        let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        CaptureTrigger {
            table: text(0),
            name: text(1),
            tgtype: text(2).parse().unwrap_or(0),
            definition: text(3),
            function: text(4),
        }
    })
    .collect()
}

/// The capture trigger of `table` (as `regclass` prints it) that fires on
/// `event` (one of the `ON_` bits). Fails if there is none.
pub fn capture_trigger(db: &OwnDatabase, table: &str, event: i64) -> CaptureTrigger {
    let all = capture_triggers(db);
    all.iter()
        .find(|t| t.table == table && t.tgtype & event != 0)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the harness's own check: expected a capture trigger of {table} firing on the event {event} (a trigger whose function is in schema vm), the capture triggers are {all:#?}"
            )
        })
}

/// `definition` (a `CREATE TRIGGER` statement) with its function replaced
/// by `function` (`public.fake()`).
pub fn with_function(definition: &str, function: &str) -> String {
    let at = definition
        .find(" EXECUTE FUNCTION ")
        .or_else(|| definition.find(" EXECUTE PROCEDURE "))
        .unwrap_or_else(|| panic!("a trigger definition without EXECUTE: {definition}"));
    format!(
        "{} EXECUTE FUNCTION {function}",
        definition.get(..at).unwrap_or_default()
    )
}

/// Runs `sql` as the superuser and gives its first value, empty when none.
fn one(db: &OwnDatabase, sql: &str) -> String {
    db.superuser().value(sql).unwrap_or_default()
}

/// The platform's part of the database as one text: see the module's
/// header. The keys appear only as a digest, so that a failing test never
/// prints them.
pub fn platform_part(db: &OwnDatabase) -> String {
    platform_part_skipping(db, None)
}

/// [`platform_part`] without the rows of the table `vm.<skip>` (for a
/// Version that applies, the record of migrations grows).
pub fn platform_part_skipping(db: &OwnDatabase, skip: Option<&str>) -> String {
    let p = literal(&platform_role(db));
    let mut parts: Vec<(String, String)> = vec![
        (
            "schema vm".to_owned(),
            one(
                db,
                "SELECT string_agg(pg_get_userbyid(n.nspowner) || ' acl=' || coalesce(array_to_string(n.nspacl, ';'), '')
                                   || ' comment=' || coalesce(obj_description(n.oid, 'pg_namespace'), ''), ',')
                   FROM pg_namespace n WHERE n.nspname = 'vm'",
            ),
        ),
        (
            "relations of vm".to_owned(),
            one(
                db,
                "SELECT string_agg(c.relname || ':' || c.relkind::text || ' owner=' || pg_get_userbyid(c.relowner)
                                   || ' acl=' || coalesce(array_to_string(c.relacl, ';'), '')
                                   || ' rules=' || c.relhasrules::text || ' triggers=' || c.relhastriggers::text
                                   || ' children=' || c.relhassubclass::text
                                   || ' comment=' || coalesce(obj_description(c.oid, 'pg_class'), ''), ','
                                   ORDER BY c.relname)
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'vm'",
            ),
        ),
        (
            "columns of vm".to_owned(),
            one(
                db,
                "SELECT string_agg(c.relname || '.' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod)
                                   || ' acl=' || coalesce(array_to_string(a.attacl, ';'), ''), ','
                                   ORDER BY c.relname, a.attnum)
                   FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
                   JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname = 'vm' AND a.attnum > 0 AND NOT a.attisdropped",
            ),
        ),
        (
            "functions of vm".to_owned(),
            one(
                db,
                "SELECT string_agg(p.oid::regprocedure::text || ' oid=' || p.oid::text
                                   || ' owner=' || pg_get_userbyid(p.proowner)
                                   || ' definer=' || p.prosecdef::text || ' volatile=' || p.provolatile::text
                                   || ' language=' || p.prolang::text
                                   || ' config=' || coalesce(array_to_string(p.proconfig, ';'), '')
                                   || ' acl=' || coalesce(array_to_string(p.proacl, ';'), '')
                                   || ' result=' || format_type(p.prorettype, NULL)
                                   || ' src=' || md5(coalesce(p.prosrc, '')), ','
                                   ORDER BY p.oid::regprocedure::text)
                   FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'vm'",
            ),
        ),
        (
            "types of vm".to_owned(),
            one(
                db,
                "SELECT string_agg(t.typname || ' owner=' || pg_get_userbyid(t.typowner), ',' ORDER BY t.typname)
                   FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = 'vm'",
            ),
        ),
        (
            "other objects in vm".to_owned(),
            one(
                db,
                "SELECT string_agg(what, ',' ORDER BY what) FROM (
                   SELECT 'operator ' || o.oid::regoperator::text AS what FROM pg_operator o
                    WHERE o.oprnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'vm')
                   UNION ALL
                   SELECT 'extension ' || e.extname FROM pg_extension e
                    WHERE e.extnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'vm')
                   UNION ALL
                   SELECT 'collation ' || c.collname FROM pg_collation c
                    WHERE c.collnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'vm')
                   UNION ALL
                   SELECT 'conversion ' || c.conname FROM pg_conversion c
                    WHERE c.connamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'vm')) x",
            ),
        ),
        (
            "triggers".to_owned(),
            one(
                db,
                "SELECT string_agg(t.tgrelid::regclass::text || '.' || t.tgname || ' oid=' || t.oid::text
                                   || ' enabled=' || t.tgenabled::text || ' type=' || t.tgtype::text
                                   || ' function=' || t.tgfoid::regprocedure::text
                                   || ' deferrable=' || t.tgdeferrable::text || ' deferred=' || t.tginitdeferred::text
                                   || ' old=' || coalesce(t.tgoldtable::text, '') || ' new=' || coalesce(t.tgnewtable::text, '')
                                   || ' args=' || t.tgnargs::text || ' when=' || (t.tgqual IS NOT NULL)::text, ','
                                   ORDER BY t.tgrelid::regclass::text, t.tgname)
                   FROM pg_trigger t WHERE NOT t.tgisinternal",
            ),
        ),
        (
            "casts".to_owned(),
            one(
                db,
                "SELECT string_agg(format_type(c.castsource, NULL) || '->' || format_type(c.casttarget, NULL)
                                   || ' ' || c.castfunc::text, ',' ORDER BY c.oid)
                   FROM pg_cast c WHERE c.oid >= 16384",
            ),
        ),
        (
            "the platform's role".to_owned(),
            one(
                db,
                &format!(
                    "SELECT string_agg(rolname || ' super=' || rolsuper::text || ' createdb=' || rolcreatedb::text
                                       || ' createrole=' || rolcreaterole::text || ' login=' || rolcanlogin::text
                                       || ' inherit=' || rolinherit::text
                                       || ' config=' || coalesce(array_to_string(rolconfig, ';'), ''), ',')
                       FROM pg_roles WHERE rolname = {p}"
                ),
            ),
        ),
        (
            "memberships of the platform's role".to_owned(),
            one(
                db,
                &format!(
                    "SELECT string_agg(r.rolname || '>' || m.rolname, ',' ORDER BY r.rolname, m.rolname)
                       FROM pg_auth_members a JOIN pg_roles r ON r.oid = a.roleid JOIN pg_roles m ON m.oid = a.member
                      WHERE m.rolname = {p} OR r.rolname = {p}"
                ),
            ),
        ),
    ];
    // The rows of every table of vm; the keys as a digest only.
    let tables = db.column(
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p', 'm') ORDER BY 1",
    );
    for table in tables {
        if Some(table.as_str()) == skip {
            continue;
        }
        let name = crate::own_database::ident(&table);
        let rows = if table == "keys" {
            one(
                db,
                &format!(
                    "SELECT count(*)::text || ' rows, md5 ' || md5(coalesce(string_agg(t::text, '|' ORDER BY t::text), '')) FROM vm.{name} t"
                ),
            )
        } else {
            one(
                db,
                &format!(
                    "SELECT coalesce(string_agg(t::text, E'\\n    ' ORDER BY t::text), '') FROM vm.{name} t"
                ),
            )
        };
        parts.push((format!("rows of vm.{table}"), rows));
    }
    parts
        .into_iter()
        .map(|(what, value)| format!("{what}: {value}\n"))
        .collect()
}

/// Everything a hostile migration might change: the Application's part
/// ([`OwnDatabase::fingerprint`]) and the platform's ([`platform_part`]).
pub fn untouched(db: &OwnDatabase) -> String {
    format!(
        "{}--- the platform's part ---\n{}",
        db.fingerprint(),
        platform_part(db)
    )
}

/// The lines of `after` that are not in `before` and the other way round.
pub fn differences(before: &str, after: &str) -> String {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let mut out = String::new();
    for line in &b {
        if !a.contains(line) {
            out.push_str(&format!("  before: {line}\n"));
        }
    }
    for line in &a {
        if !b.contains(line) {
            out.push_str(&format!("  after:  {line}\n"));
        }
    }
    out
}

/// Fails unless the database is exactly as `before` ([`untouched`]), no
/// Version but 1 is recorded, and Version 1 still serves `application`.
pub fn assert_untouched(
    own: &Own,
    db: &OwnDatabase,
    application: &str,
    before: &str,
    context: &str,
) {
    let after = untouched(db);
    assert!(
        after == before,
        "{context}: expected the own database exactly as before (the platform's part and the Application's), but these differ:\n{}",
        differences(before, &after)
    );
    assert_eq!(
        own.version_numbers(application),
        vec![1],
        "{context}: expected no Version recorded"
    );
    assert_eq!(
        own.serving(&User::new(WRITER), application),
        1,
        "{context}: expected Version 1 still serving"
    );
}

/// Whether `role` may log in, as `pg_roles.rolcanlogin` says.
pub fn can_log_in(db: &OwnDatabase, role: &str) -> bool {
    db.instance()
        .value(&format!(
            "SELECT rolcanlogin::text FROM pg_roles WHERE rolname = {}",
            literal(role)
        ))
        .as_deref()
        == Some("true")
}

/// What a hostile Version must be answered with: `422` and one of these
/// (reason, code or rule) pairs, `code` for `migration-failed`, `rule` for
/// `migration-refused`.
pub type Answers = &'static [(&'static str, &'static str)];

/// For [`failed`]: any SQLSTATE (CONTRACT.md: "`42501`, or another
/// refusal of Postgres's own").
pub const ANY_CODE: &str = "*";

/// `422 migration-failed` with this SQLSTATE.
pub const fn failed(code: &'static str) -> (&'static str, &'static str) {
    ("migration-failed", code)
}

/// `422 migration-refused` with this rule.
pub const fn refused(rule: &'static str) -> (&'static str, &'static str) {
    ("migration-refused", rule)
}

/// Fails unless `sent` is `422` with one of `answers`, names `migration`,
/// and, when given, has a message (`message`, and for `migration-failed`
/// also `database_message`) that mentions `mentions`.
pub fn assert_answer(sent: &Response, answers: Answers, migration: &str, mentions: Option<&str>) {
    let body = sent.json();
    let reason = body["error"]["reason"].as_str().unwrap_or_default();
    let expected_reason = answers
        .iter()
        .find(|(r, _)| *r == reason)
        .map_or(answers.first().map_or("", |(r, _)| *r), |(r, _)| *r);
    assert_api_error(sent, 422, expected_reason);
    let error = &body["error"];
    let field = if reason == "migration-failed" {
        "code"
    } else {
        "rule"
    };
    let got = error[field].as_str().unwrap_or_default();
    assert!(
        answers
            .iter()
            .any(|(r, c)| *r == reason && (*c == got || (*c == ANY_CODE && !got.is_empty()))),
        "expected one of {answers:?} (reason, then `code` for migration-failed or `rule` for migration-refused), got {error}"
    );
    if migration == ANY_MIGRATION {
        assert!(
            error["migration"].as_str().is_some_and(|m| !m.is_empty()),
            "expected the error to name a migration, got {error}"
        );
    } else {
        assert_eq!(
            error["migration"].as_str(),
            Some(migration),
            "expected the error to name the migration {migration:?}, got {error}"
        );
    }
    if let Some(word) = mentions {
        let text = format!(
            "{} {}",
            error["message"].as_str().unwrap_or_default(),
            error["database_message"].as_str().unwrap_or_default()
        );
        assert!(
            text.contains(word),
            "expected the error's message to mention {word:?}, got {error}"
        );
    }
}

/// Makes a [`Case`]'s pending migrations once its own database is known.
pub type MakePending = Box<dyn Fn(&OwnDatabase) -> Vec<Migration>>;

/// For [`Case::naming`]: the answer may name any of the pending
/// migrations (CONTRACT.md does not say which one a check made after the
/// last migration names).
pub const ANY_MIGRATION: &str = "*";

/// A hostile Version: the migrations it adds to [`base`], made once the
/// Application's own database is known (some name its roles), and what it
/// must be answered.
pub struct Case {
    pub name: &'static str,
    pub pending: MakePending,
    pub answers: Answers,
    /// The migration the answer names: the last pending one when `None`;
    /// any when [`ANY_MIGRATION`].
    pub names: Option<&'static str>,
    /// A word the answer's message must hold (the table, for `capture-changed`).
    pub mentions: Option<&'static str>,
}

impl Case {
    /// One pending migration `0004_<name>` of these statements, each its
    /// own breakpoint part.
    pub fn statements(
        name: &'static str,
        answers: Answers,
        statements: impl Fn(&OwnDatabase) -> Vec<String> + 'static,
    ) -> Self {
        Self {
            name,
            pending: Box::new(move |db| {
                let statements = statements(db);
                let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
                vec![Migration::new(&format!("0004_{name}"), &refs)]
            }),
            answers,
            names: None,
            mentions: None,
        }
    }

    /// These pending migrations, as given.
    pub fn migrations(
        name: &'static str,
        answers: Answers,
        pending: impl Fn(&OwnDatabase) -> Vec<Migration> + 'static,
    ) -> Self {
        Self {
            name,
            pending: Box::new(pending),
            answers,
            names: None,
            mentions: None,
        }
    }

    /// One pending migration `0004_<name>` of this whole text.
    pub fn text(name: &'static str, answers: Answers, sql: &'static str) -> Self {
        Self::migrations(name, answers, move |_| {
            vec![Migration {
                name: format!("0004_{name}"),
                sql: sql.to_owned(),
            }]
        })
    }

    /// The answer must name this migration.
    #[must_use]
    pub fn naming(mut self, migration: &'static str) -> Self {
        self.names = Some(migration);
        self
    }

    /// The answer's message must hold this word.
    #[must_use]
    pub fn mentioning(mut self, word: &'static str) -> Self {
        self.mentions = Some(word);
        self
    }
}

/// The text of a panic's payload.
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "(a panic without a message)".to_owned())
}

/// Runs every case on `own` (see [`start`]), each on a fresh Application
/// `<prefix>-<i>` installed by [`install_seeded`]: sends [`base`] and the
/// case's migrations, and checks the answer, that the database is
/// [`untouched`], that Version 1 still serves, and that the owner role
/// cannot log in. Every case runs whatever the others did; the test fails
/// at the end with every case that failed.
pub fn run_cases(own: &Own, prefix: &str, cases: Vec<Case>) {
    let mut failures = Vec::new();
    let total = cases.len();
    for (i, case) in cases.into_iter().enumerate() {
        let application = format!("{prefix}-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install_seeded(own, &application);
            let before = untouched(&db);
            let pending = (case.pending)(&db);
            let migration = case
                .names
                .map(str::to_owned)
                .or_else(|| pending.last().map(|m| m.name.clone()))
                .unwrap_or_default();
            let sent = own.send(&application, &base_and(&pending));
            assert_answer(&sent, case.answers, &migration, case.mentions);
            assert_untouched(own, &db, &application, &before, "after the refused Version");
            assert!(
                !can_log_in(&db, &db.owner),
                "expected the owner role {} unable to log in once the Version was answered",
                db.owner
            );
        }));
        if let Err(payload) = outcome {
            // The program's whole log is in some messages: the first part
            // is what says why.
            let why: String = panic_text(payload.as_ref()).chars().take(3000).collect();
            failures.push(format!(
                "--- case {:?} (Application {application}) ---\n{why}",
                case.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} hostile Versions were not answered and kept out as CONTRACT.md says:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `template` with `{p}`, `{o}`, `{r}` and `{d}` replaced by the quoted
/// names of the platform's role, the owner role, the runtime role and the
/// database, and `{p_literal}` by the platform's role as a string literal.
pub fn fill(template: &str, db: &OwnDatabase) -> String {
    use crate::own_database::ident;
    template
        .replace("{p_literal}", &literal(&platform_role(db)))
        .replace("{p}", &ident(&platform_role(db)))
        .replace("{o}", &ident(&db.owner))
        .replace("{r}", &ident(&db.runtime))
        .replace("{d}", &ident(&db.name))
}

/// A [`Case`] of one pending migration `0004_<name>` whose statements are
/// these templates ([`fill`]), each its own breakpoint part.
pub fn case(name: &'static str, answers: Answers, templates: &'static [&'static str]) -> Case {
    Case::statements(name, answers, move |db| {
        templates.iter().map(|t| fill(t, db)).collect()
    })
}

/// Fails unless the three roles stand as CONTRACT.md says.
pub fn assert_three_roles(own: &Own, db: &OwnDatabase, context: &str) {
    let platform = platform_role(db);
    assert!(
        db.role_exists(&platform),
        "{context}: expected the platform's own role {platform:?} beside the own database"
    );
    assert_eq!(
        db.column(&format!(
            "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = {}",
            literal(&db.name)
        )),
        vec![platform.clone()],
        "{context}: expected the platform's own role to own the database"
    );
    let not_the_platform_s = db.column(&format!(
        "SELECT what FROM (
           SELECT 'schema vm ' || pg_get_userbyid(nspowner) AS what, nspowner AS owner FROM pg_namespace WHERE nspname = 'vm'
           UNION ALL
           SELECT 'relation vm.' || c.relname || ' ' || pg_get_userbyid(c.relowner), c.relowner
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'vm'
           UNION ALL
           SELECT 'function ' || p.oid::regprocedure::text || ' ' || pg_get_userbyid(p.proowner), p.proowner
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'vm'
           UNION ALL
           SELECT 'type vm.' || t.typname || ' ' || pg_get_userbyid(t.typowner), t.typowner
             FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = 'vm'
         ) x WHERE pg_get_userbyid(owner) <> {}
         ORDER BY 1",
        literal(&platform)
    ));
    assert!(
        not_the_platform_s.is_empty(),
        "{context}: expected the platform's own role to own the schema vm and everything in it, but these are another's:\n{}",
        not_the_platform_s.join("\n")
    );
    assert!(
        !db.column("SELECT 1 FROM pg_namespace WHERE nspname = 'vm'")
            .is_empty(),
        "{context}: expected the schema vm"
    );
    let platform_s_owned_by_owner: Vec<String> = db
        .owned_by(&db.owner)
        .into_iter()
        .filter(|o| o.contains(" vm.") || o == "schema vm" || o.starts_with("database "))
        .collect();
    assert!(
        platform_s_owned_by_owner.is_empty(),
        "{context}: expected the owner role to own nothing of the platform's, but it owns:\n{}",
        platform_s_owned_by_owner.join("\n")
    );
    assert!(
        db.objects_not_owned_by_the_owner()
            .iter()
            .all(|o| !o.ends_with(&format!(" {}", db.runtime))),
        "{context}: the runtime role owns something: {:?}",
        db.objects_not_owned_by_the_owner()
    );
    assert!(
        db.owned_by(&db.runtime).is_empty(),
        "{context}: expected the runtime role to own nothing, it owns {:?}",
        db.owned_by(&db.runtime)
    );
    assert!(
        !can_log_in(db, &db.owner),
        "{context}: expected the owner role {} unable to log in (rolcanlogin false)",
        db.owner
    );
    assert!(
        can_log_in(db, &db.runtime),
        "{context}: expected the runtime role {} to log in",
        db.runtime
    );
    let limited = own.vm.platform().role();
    assert_eq!(
        db.instance().batch(&format!(
            "SELECT rolsuper::text, rolcreatedb::text, rolcreaterole::text, rolreplication::text, rolbypassrls::text
               FROM pg_roles WHERE rolname = {}",
            literal(limited)
        )),
        vec![vec![
            Some("false".to_owned()),
            Some("true".to_owned()),
            Some("true".to_owned()),
            Some("false".to_owned()),
            Some("false".to_owned()),
        ]],
        "{context}: expected the platform's role {limited} still only CREATEDB and CREATEROLE"
    );
}

const READ_KEYS_DENIED: Answers = &[failed("42501")];

/// The rows of `vm.migrations`, each as text, ordered.
pub fn record_of_migrations(db: &OwnDatabase) -> Vec<String> {
    db.column("SELECT t::text FROM vm.migrations t ORDER BY 1")
}

/// Runs every case on `own`, each on a fresh Application, for attempts
/// whose outcome CONTRACT.md leaves to Postgres: the Version is `201` or
/// `422`, and either way the platform's part is as before. When `422`,
/// the whole database is untouched as for any failed Version. When `201`,
/// the platform's part but the record of migrations is as before, and the
/// record still holds every earlier row. Either way the owner role cannot
/// log in, and when `keys_after` a later migration still cannot read
/// `vm.keys` (`422 migration-failed 42501`).
pub fn run_lenient_cases(own: &Own, prefix: &str, cases: Vec<Case>, keys_after: bool) {
    let mut failures = Vec::new();
    let total = cases.len();
    for (i, case) in cases.into_iter().enumerate() {
        let application = format!("{prefix}-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install_seeded(own, &application);
            let before = untouched(&db);
            let platform_before = platform_part_skipping(&db, Some("migrations"));
            let record_before = record_of_migrations(&db);
            let pending = (case.pending)(&db);
            let mut list = base_and(&pending);
            let sent = own.send(&application, &list);
            match sent.status {
                422 => {
                    let reason = sent.json()["error"]["reason"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    assert!(
                        reason == "migration-failed" || reason == "migration-refused",
                        "expected migration-failed or migration-refused, got\n{}",
                        sent.describe()
                    );
                    assert_untouched(
                        own,
                        &db,
                        &application,
                        &before,
                        "after the Version that failed",
                    );
                }
                201 => {
                    let after = platform_part_skipping(&db, Some("migrations"));
                    assert!(
                        after == platform_before,
                        "the Version applied; expected the platform's part as before, but these differ:\n{}",
                        differences(&platform_before, &after)
                    );
                    let record = record_of_migrations(&db);
                    let lost: Vec<&String> = record_before
                        .iter()
                        .filter(|r| !record.contains(r))
                        .collect();
                    assert!(
                        lost.is_empty(),
                        "expected the record of migrations to keep every earlier row, these are gone or changed:\n{lost:#?}"
                    );
                }
                _ => panic!("expected `201` or `422`, got\n{}", sent.describe()),
            }
            assert!(
                !can_log_in(&db, &db.owner),
                "expected the owner role {} unable to log in once the Version was answered",
                db.owner
            );
            if keys_after {
                if sent.status != 201 {
                    list.truncate(list.len().saturating_sub(pending.len()));
                }
                list.push(Migration::new(
                    "0009_read_keys",
                    &["SELECT * FROM vm.keys;"],
                ));
                let read = own.send(&application, &list);
                assert_answer(&read, READ_KEYS_DENIED, "0009_read_keys", None);
            }
        }));
        if let Err(payload) = outcome {
            let why: String = panic_text(payload.as_ref()).chars().take(3000).collect();
            failures.push(format!(
                "--- case {:?} (Application {application}) ---\n{why}",
                case.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} Versions did not leave the platform's part as before:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
