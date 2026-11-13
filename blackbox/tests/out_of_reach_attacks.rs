//! What a migration cannot do (CONTRACT.md, "The platform's part of an own
//! database is out of a migration's reach: What a migration cannot do";
//! ticket "The Change history is kept out of a Contributor's reach", first
//! acceptance criterion: "A Version whose migration reads the keys, changes
//! the history or the record of migrations, or creates, alters or drops
//! anything in `vm`, is refused; the history, the keys and the capture are
//! as before").
//!
//! The catalogue of hostile migrations of
//! `docs/research/change-history-out-of-reach/a-attacks.sql`, each sent as
//! the migration of a second Version of an Application whose first Version
//! made captured tables and whose operations then left rows and history
//! rows. Each must be answered `422 migration-failed` with the SQLSTATE the
//! contract gives, and leave everything as it was: the keys' digest, every
//! row of `vm` (the history and the platform's record of migrations among
//! them), every function of `vm` (owner, rights, settings, body), every
//! trigger of every table, every table's rows, every sequence; no Version
//! recorded, Version 1 still serving, the owner role unable to log in.
//!
//! The platform's record of migrations is named `vm.migrations` as in the
//! contract's earlier section ("Identity and Change history in the own
//! database: What this does not close").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history_after, last_history_id, vm_functions};
use blackbox::out_of_reach::{
    Answers, Case, ON_TRUNCATE, WRITER, base_and, capture_trigger, case, failed, fill,
    install_seeded, run_cases, start,
};
use blackbox::own_database::{Migration, ident};
use blackbox::scenario::lines;
use serde_json::json;

const DENIED: Answers = &[failed("42501")];
const TRUNCATE_REFUSED: Answers = &[failed("VM002")];

#[test]
fn a_migration_cannot_read_the_keys_or_carry_them_out() {
    let own = start();
    let cases = vec![
        case("read-keys", DENIED, &["SELECT * FROM vm.keys"]),
        case(
            "keys-into-a-table",
            DENIED,
            &[r#"CREATE TABLE "loot" AS SELECT * FROM vm.keys"#],
        ),
        // A sequence keeps a value through a rollback: `stash` was made by
        // the first Version, so a value set into it would outlive the
        // refusal. The fingerprint holds every sequence's state.
        case(
            "keys-into-a-sequence",
            DENIED,
            &["SELECT setval('stash', (SELECT length(k::text) + 1000 FROM vm.keys k LIMIT 1))"],
        ),
        case(
            "keys-into-a-sequence-of-a-captured-table",
            DENIED,
            &[
                "SELECT setval('things_id_seq', (SELECT length(k::text) + 5000 FROM vm.keys k LIMIT 1))",
            ],
        ),
        case(
            "keys-through-a-view",
            DENIED,
            &[
                r#"CREATE VIEW "peek" AS SELECT * FROM vm.keys"#,
                r#"SELECT * FROM "peek""#,
            ],
        ),
        case(
            "keys-through-a-definer-function",
            DENIED,
            &[
                r#"CREATE FUNCTION "peek"() RETURNS text LANGUAGE sql SECURITY DEFINER AS $f$ SELECT k::text FROM vm.keys k $f$"#,
                r#"SELECT "peek"()"#,
            ],
        ),
        case(
            "keys-through-dynamic-sql",
            DENIED,
            &[r#"SELECT "run_sql"('CREATE TABLE loot AS SELECT * FROM vm.keys')"#],
        ),
        case(
            "grant-itself-the-keys",
            DENIED,
            &["GRANT SELECT ON vm.keys TO {o}"],
        ),
    ];
    run_cases(&own, "keys", cases);
}

#[test]
fn a_migration_cannot_change_lock_or_hang_anything_on_the_history_or_the_record_of_migrations() {
    let own = start();
    let mut cases = vec![
        case(
            "insert-history",
            DENIED,
            &[
                r#"INSERT INTO vm.history (at, schema_name, table_name, kind, user_id, application, actor)
VALUES (now(), 'public', 'things', 'insert', 'mallory', 'board', 'user')"#,
            ],
        ),
        case(
            "update-history",
            DENIED,
            &["UPDATE vm.history SET user_id = 'mallory'"],
        ),
        case("delete-history", DENIED, &["DELETE FROM vm.history"]),
        case("truncate-history", DENIED, &["TRUNCATE vm.history"]),
        case(
            "lock-history",
            DENIED,
            &["LOCK TABLE vm.history IN ACCESS EXCLUSIVE MODE"],
        ),
        case(
            "history-sequence",
            DENIED,
            &["SELECT setval(pg_get_serial_sequence('vm.history', 'id'), 1)"],
        ),
        case(
            "rule-on-history",
            DENIED,
            &[r#"CREATE RULE "quiet" AS ON INSERT TO vm.history DO INSTEAD NOTHING"#],
        ),
        case(
            "trigger-on-history",
            DENIED,
            &[
                r#"CREATE FUNCTION "nop"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NULL; END $f$"#,
                r#"CREATE TRIGGER "quiet" BEFORE INSERT ON vm.history FOR EACH ROW EXECUTE FUNCTION "nop"()"#,
            ],
        ),
        case(
            "child-of-history",
            DENIED,
            &[r#"CREATE TABLE "kid" () INHERITS (vm.history)"#],
        ),
        case(
            "policy-on-history",
            DENIED,
            &["ALTER TABLE vm.history ENABLE ROW LEVEL SECURITY"],
        ),
        case(
            "index-on-history",
            DENIED,
            &[r#"CREATE INDEX "mine" ON vm.history (user_id)"#],
        ),
        case(
            "reference-the-history",
            DENIED,
            &[r#"CREATE TABLE "pins" ("id" bigint REFERENCES vm.history ("id"))"#],
        ),
        case(
            "delete-history-through-dynamic-sql",
            DENIED,
            &[r#"SELECT "run_sql"('DELETE FROM vm.history')"#],
        ),
        case(
            "insert-migrations",
            DENIED,
            &["INSERT INTO vm.migrations DEFAULT VALUES"],
        ),
        case("delete-migrations", DENIED, &["DELETE FROM vm.migrations"]),
        case("truncate-migrations", DENIED, &["TRUNCATE vm.migrations"]),
        case(
            "lock-migrations",
            DENIED,
            &["LOCK TABLE vm.migrations IN ACCESS EXCLUSIVE MODE"],
        ),
        case(
            "rule-on-migrations",
            DENIED,
            &[r#"CREATE RULE "quiet" AS ON INSERT TO vm.migrations DO INSTEAD NOTHING"#],
        ),
        case(
            "child-of-migrations",
            DENIED,
            &[r#"CREATE TABLE "kid" () INHERITS (vm.migrations)"#],
        ),
    ];
    // An update of the record of migrations, by a column the catalog names.
    cases.push(Case::statements("update-migrations", DENIED, |db| {
        let column = db
            .superuser()
            .value(
                "SELECT attname::text FROM pg_attribute
                  WHERE attrelid = 'vm.migrations'::regclass AND attnum > 0 AND NOT attisdropped
                  ORDER BY attnum LIMIT 1",
            )
            .unwrap_or_else(|| "name".to_owned());
        let column = ident(&column);
        vec![format!("UPDATE vm.migrations SET {column} = {column}")]
    }));
    run_cases(&own, "history", cases);
}

#[test]
fn a_migration_cannot_replace_alter_take_over_or_drop_a_function_of_vm() {
    let own = start();
    // The functions of vm are the same in every own database: read them
    // from one.
    let probe = install_seeded(&own, "probe");
    let functions = vm_functions(&probe);
    assert!(
        !functions.is_empty(),
        "the harness's own check: expected functions in schema vm (vm.identity() at least)"
    );
    let mut cases = Vec::new();
    for function in functions {
        let signature = function.signature.clone();
        let renamed = ident(&format!("{}_gone", function.name));
        let statements: Vec<(&'static str, String)> = vec![
            (
                "owner",
                format!("ALTER FUNCTION {signature} OWNER TO {{o}}"),
            ),
            ("drop", format!("DROP FUNCTION {signature} CASCADE")),
            (
                "rename",
                format!("ALTER FUNCTION {signature} RENAME TO {renamed}"),
            ),
            ("reset", format!("ALTER FUNCTION {signature} RESET ALL")),
            (
                "search-path",
                format!("ALTER FUNCTION {signature} SET search_path = public, pg_catalog"),
            ),
            (
                "invoker",
                format!("ALTER FUNCTION {signature} SECURITY INVOKER"),
            ),
            (
                "comment",
                format!("COMMENT ON FUNCTION {signature} IS 'mine'"),
            ),
        ];
        for (what, statement) in statements {
            let name: &'static str =
                Box::leak(format!("{what}-{}", function.name.replace('_', "-")).into_boxed_str());
            cases.push(Case::statements(name, DENIED, move |db| {
                vec![fill(&statement, db)]
            }));
        }
        let replacement = if function.returns == "trigger" {
            Some(format!(
                "CREATE OR REPLACE FUNCTION {signature} RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NULL; END $f$"
            ))
        } else if function.name == "identity" && function.arguments.is_empty() {
            Some("CREATE OR REPLACE FUNCTION vm.identity() RETURNS TABLE (user_id text, application text, actor text, request text) LANGUAGE sql AS $f$ SELECT 'mallory', 'board', 'user', 'forged' $f$".to_owned())
        } else {
            None
        };
        if let Some(statement) = replacement {
            let name: &'static str =
                Box::leak(format!("replace-{}", function.name.replace('_', "-")).into_boxed_str());
            cases.push(Case::statements(name, DENIED, move |_| {
                vec![statement.clone()]
            }));
        }
    }
    run_cases(&own, "functions", cases);
}

#[test]
fn a_migration_cannot_create_in_vm_move_into_it_or_change_what_is_there() {
    let own = start();
    let cases = vec![
        case(
            "create-table-in-vm",
            DENIED,
            &[r#"CREATE TABLE vm."mine" ("i" integer)"#],
        ),
        case(
            "create-function-in-vm",
            DENIED,
            &[r#"CREATE FUNCTION vm."mine"() RETURNS integer LANGUAGE sql AS 'SELECT 1'"#],
        ),
        case(
            "create-view-in-vm",
            DENIED,
            &[r#"CREATE VIEW vm."mine" AS SELECT 1 AS "i""#],
        ),
        case(
            "create-sequence-in-vm",
            DENIED,
            &[r#"CREATE SEQUENCE vm."mine""#],
        ),
        case(
            "create-type-in-vm",
            DENIED,
            &[r#"CREATE TYPE vm."mine" AS ("i" integer)"#],
        ),
        case(
            "create-domain-in-vm",
            DENIED,
            &[r#"CREATE DOMAIN vm."mine" AS integer"#],
        ),
        case(
            "create-table-like-history-in-vm",
            DENIED,
            &[r#"CREATE TABLE vm."history_copy" (LIKE vm.history)"#],
        ),
        case(
            "move-a-new-table-into-vm",
            DENIED,
            &[
                r#"CREATE TABLE "mover" ("i" integer)"#,
                r#"ALTER TABLE "mover" SET SCHEMA vm"#,
            ],
        ),
        case(
            "move-a-captured-table-into-vm",
            DENIED,
            &[r#"ALTER TABLE "keyless" SET SCHEMA vm"#],
        ),
        case(
            "move-a-function-into-vm",
            DENIED,
            &[r#"ALTER FUNCTION "run_sql"(text) SET SCHEMA vm"#],
        ),
        case(
            "move-a-type-into-vm",
            DENIED,
            &[
                r#"CREATE TYPE "mood" AS ENUM ('ok', 'sad')"#,
                r#"ALTER TYPE "mood" SET SCHEMA vm"#,
            ],
        ),
        case(
            "rename-history",
            DENIED,
            &[r#"ALTER TABLE vm.history RENAME TO "history_old""#],
        ),
        case(
            "take-over-history",
            DENIED,
            &["ALTER TABLE vm.history OWNER TO {o}"],
        ),
        case("drop-history", DENIED, &["DROP TABLE vm.history CASCADE"]),
        case(
            "add-a-column-to-history",
            DENIED,
            &[r#"ALTER TABLE vm.history ADD COLUMN "x" integer"#],
        ),
        case(
            "disable-the-triggers-of-history",
            DENIED,
            &["ALTER TABLE vm.history DISABLE TRIGGER ALL"],
        ),
        case(
            "comment-on-history",
            DENIED,
            &["COMMENT ON TABLE vm.history IS 'mine'"],
        ),
        case(
            "take-over-keys",
            DENIED,
            &["ALTER TABLE vm.keys OWNER TO {o}"],
        ),
        case("drop-keys", DENIED, &["DROP TABLE vm.keys CASCADE"]),
        case(
            "rename-migrations",
            DENIED,
            &[r#"ALTER TABLE vm.migrations RENAME TO "m2""#],
        ),
        case(
            "drop-migrations",
            DENIED,
            &["DROP TABLE vm.migrations CASCADE"],
        ),
        case(
            "restart-the-history-sequence",
            DENIED,
            &[
                "DO $f$ BEGIN EXECUTE format('ALTER SEQUENCE %s RESTART', pg_get_serial_sequence('vm.history', 'id')); END $f$",
            ],
        ),
        case(
            "rename-schema-vm",
            DENIED,
            &[r#"ALTER SCHEMA vm RENAME TO "vm_old""#],
        ),
        case(
            "take-over-schema-vm",
            DENIED,
            &["ALTER SCHEMA vm OWNER TO {o}"],
        ),
        case("drop-schema-vm", DENIED, &["DROP SCHEMA vm CASCADE"]),
        case(
            "comment-on-schema-vm",
            DENIED,
            &["COMMENT ON SCHEMA vm IS 'mine'"],
        ),
        case(
            "reassign-the-platform-s-objects",
            DENIED,
            &["REASSIGN OWNED BY {p} TO {o}"],
        ),
        case(
            "drop-the-platform-s-objects",
            DENIED,
            &["DROP OWNED BY {p}"],
        ),
        case(
            "give-a-table-to-the-platform-s-role",
            DENIED,
            &[r#"ALTER TABLE "keyless" OWNER TO {p}"#],
        ),
    ];
    run_cases(&own, "objects", cases);
}

#[test]
fn a_migration_cannot_become_another_role_give_rights_or_change_the_database_or_the_roles() {
    let own = start();
    let cases = vec![
        case("set-role-platform", DENIED, &["SET ROLE {p}"]),
        case("set-local-role-platform", DENIED, &["SET LOCAL ROLE {p}"]),
        case("set-role-runtime", DENIED, &["SET ROLE {r}"]),
        case(
            "set-session-authorization",
            DENIED,
            &["SET SESSION AUTHORIZATION {p}"],
        ),
        case(
            "set-config-role",
            DENIED,
            &["SELECT set_config('role', {p_literal}, false)"],
        ),
        case("grant-platform-to-owner", DENIED, &["GRANT {p} TO {o}"]),
        case("grant-runtime-to-owner", DENIED, &["GRANT {r} TO {o}"]),
        case("grant-owner-to-runtime", DENIED, &["GRANT {o} TO {r}"]),
        case(
            "revoke-owner-from-platform",
            DENIED,
            &["REVOKE {o} FROM {p}"],
        ),
        case(
            "default-privileges-of-the-platform-s-role",
            DENIED,
            &["ALTER DEFAULT PRIVILEGES FOR ROLE {p} GRANT SELECT ON TABLES TO {o}"],
        ),
        case(
            "database-settings",
            DENIED,
            &["ALTER DATABASE {d} SET search_path = public, pg_catalog"],
        ),
        case(
            "database-session-replication-role",
            DENIED,
            &["ALTER DATABASE {d} SET session_replication_role = replica"],
        ),
        case(
            "database-owner",
            DENIED,
            &["ALTER DATABASE {d} OWNER TO {o}"],
        ),
        case(
            "database-rename",
            DENIED,
            &[r#"ALTER DATABASE {d} RENAME TO "elsewhere""#],
        ),
        case(
            "database-connection-limit",
            DENIED,
            &["ALTER DATABASE {d} CONNECTION LIMIT 1"],
        ),
        case(
            "database-comment",
            DENIED,
            &["COMMENT ON DATABASE {d} IS 'mine'"],
        ),
        case(
            "platform-role-password",
            DENIED,
            &["ALTER ROLE {p} PASSWORD 'pwned'"],
        ),
        case(
            "runtime-role-password",
            DENIED,
            &["ALTER ROLE {r} PASSWORD 'pwned'"],
        ),
        case(
            "runtime-role-settings",
            DENIED,
            &["ALTER ROLE {r} SET extra_float_digits = -15"],
        ),
        case("runtime-role-no-login", DENIED, &["ALTER ROLE {r} NOLOGIN"]),
        case(
            "owner-gives-itself-login",
            DENIED,
            &["ALTER ROLE {o} LOGIN"],
        ),
        case(
            "owner-gives-itself-createrole",
            DENIED,
            &["ALTER ROLE {o} CREATEROLE"],
        ),
        case(
            "owner-gives-itself-valid-until",
            DENIED,
            &["ALTER ROLE {o} VALID UNTIL 'infinity'"],
        ),
        case(
            "create-a-role",
            DENIED,
            &[r#"CREATE ROLE "intruder" LOGIN"#],
        ),
        case("drop-the-runtime-role", DENIED, &["DROP ROLE {r}"]),
        case(
            "session-replication-role",
            DENIED,
            &["SET session_replication_role = replica"],
        ),
        case(
            "local-session-replication-role",
            DENIED,
            &["SET LOCAL session_replication_role = replica"],
        ),
        case(
            "session-replication-role-by-set-config",
            DENIED,
            &["SELECT set_config('session_replication_role', 'replica', true)"],
        ),
    ];
    run_cases(&own, "roles", cases);
}

#[test]
fn a_migration_cannot_truncate_a_table_that_has_history() {
    let own = start();
    let mut cases = vec![
        case("truncate", TRUNCATE_REFUSED, &[r#"TRUNCATE "things""#]),
        case(
            "truncate-only",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE ONLY "things""#],
        ),
        case(
            "truncate-restart-identity",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "things" RESTART IDENTITY"#],
        ),
        case(
            "truncate-keyless",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "keyless""#],
        ),
        case(
            "truncate-another-schema",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "inventory"."stock""#],
        ),
        // children has rows and is truncated only through the cascade.
        case(
            "truncate-cascade",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "parents" CASCADE"#],
        ),
        case(
            "truncate-a-partitioned-table",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "measures""#],
        ),
        case(
            "truncate-a-partition",
            TRUNCATE_REFUSED,
            &[r#"TRUNCATE "measures_low""#],
        ),
        case(
            "truncate-through-a-function",
            TRUNCATE_REFUSED,
            &[r#"SELECT "run_sql"('TRUNCATE things')"#],
        ),
        case(
            "truncate-in-a-do-block",
            TRUNCATE_REFUSED,
            &["DO $f$ BEGIN TRUNCATE things; END $f$"],
        ),
    ];
    // The partitioned table's own capture trigger for TRUNCATE dropped
    // first: the partitions' still refuse.
    cases.push(Case::statements(
        "truncate-a-partitioned-table-without-its-own-trigger",
        TRUNCATE_REFUSED,
        |db| {
            let mut statements = Vec::new();
            if let Some(trigger) = blackbox::out_of_reach::capture_triggers(db)
                .into_iter()
                .find(|t| t.table == "measures" && t.tgtype & ON_TRUNCATE != 0)
            {
                statements.push(format!(
                    r#"DROP TRIGGER {} ON "measures""#,
                    ident(&trigger.name)
                ));
            }
            statements.push(r#"TRUNCATE "measures""#.to_owned());
            statements
        },
    ));
    for case in &mut cases {
        case.mentions = Some("DELETE");
    }
    run_cases(&own, "truncate", cases);
}

/// CONTRACT.md: "It can still read `vm.identity()` and call what the
/// runtime role may call": a migration that reads `vm.history` (which the
/// runtime role may) and `vm.identity()` applies.
#[test]
fn a_migration_can_still_read_the_identity_and_the_history() {
    let own = start();
    let db = install_seeded(&own, APP);
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_reads",
            &[
                r#"CREATE TABLE "history_seen" AS SELECT count(*) AS "n" FROM vm.history"#,
                r#"CREATE TABLE "identity_seen" AS SELECT * FROM vm.identity()"#,
            ],
        )]),
    );
    assert_eq!(
        sent.status,
        201,
        "expected a migration that reads vm.history and vm.identity() to apply, got\n{}",
        sent.describe()
    );
    assert!(db.has_table("history_seen") && db.has_table("identity_seen"));
    let _ = capture_trigger(&db, "history_seen", blackbox::out_of_reach::ON_INSERT);
}

/// a-attacks.sql A70 to A75: functions named like what the capture calls,
/// put in `public` and first on the search path, then a captured write.
/// The write is recorded truly: with the real `to_jsonb`, the real time and
/// the real key.
#[test]
fn shadows_of_what_the_capture_calls_do_not_change_what_is_recorded() {
    let own = start();
    let db = install_seeded(&own, APP);
    let last = last_history_id(&db);
    let before = db
        .superuser()
        .value("SELECT clock_timestamp()::text")
        .unwrap();
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_shadows",
            &[
                r#"CREATE FUNCTION "to_jsonb"(anyelement) RETURNS jsonb LANGUAGE sql AS $f$ SELECT '"forged"'::jsonb $f$"#,
                r#"CREATE FUNCTION "to_json"(anyelement) RETURNS json LANGUAGE sql AS $f$ SELECT '"forged"'::json $f$"#,
                r#"CREATE FUNCTION "jsonb_build_object"(VARIADIC text[]) RETURNS jsonb LANGUAGE sql AS $f$ SELECT '{"forged": true}'::jsonb $f$"#,
                r#"CREATE FUNCTION "jsonb_agg"(anyelement) RETURNS jsonb LANGUAGE sql AS $f$ SELECT '["forged"]'::jsonb $f$"#,
                r#"CREATE FUNCTION "jsonb_object_agg"(text, jsonb) RETURNS jsonb LANGUAGE sql AS $f$ SELECT '{}'::jsonb $f$"#,
                r#"CREATE FUNCTION "clock_timestamp"() RETURNS timestamptz LANGUAGE sql AS $f$ SELECT 'epoch'::timestamptz $f$"#,
                r#"CREATE FUNCTION "now"() RETURNS timestamptz LANGUAGE sql AS $f$ SELECT 'epoch'::timestamptz $f$"#,
                r#"CREATE FUNCTION "current_setting"(text) RETURNS text LANGUAGE sql AS $f$ SELECT 'forged' $f$"#,
                "SET LOCAL search_path = public, pg_catalog",
                r#"UPDATE "things" SET "title" = 'shadowed' WHERE "id" = 101"#,
            ],
        )]),
    );
    assert_eq!(
        sent.status,
        201,
        "expected the shadows' Version to apply, got\n{}",
        sent.describe()
    );
    let rows = history_after(&db, last);
    assert_eq!(
        rows.len(),
        1,
        "expected one history row, got:\n{}",
        lines(&rows)
    );
    let row = &rows[0];
    assert!(
        !row.to_string().contains("forged"),
        "expected the history row written with the real functions, got {row}"
    );
    assert_eq!(row["actor"], "migration", "{row}");
    assert_eq!(row["migration"], "0004_shadows", "{row}");
    assert_eq!(row["user_id"], blackbox::INSTALLER, "{row}");
    assert_eq!(row["key"], json!({ "id": 101 }), "{row}");
    assert_eq!(row["new"]["title"], "shadowed", "{row}");
    assert_eq!(row["old"]["title"], "first thing", "{row}");
    assert_eq!(row["changed"], json!(["title"]), "{row}");
    let recent = db
        .superuser()
        .value(&format!(
            "SELECT (at >= '{before}'::timestamptz AND at <= clock_timestamp())::text FROM vm.history WHERE id = {}",
            row["id"]
        ))
        .unwrap_or_default();
    assert_eq!(
        recent, "true",
        "expected the real time of the change, not a shadow's, got {row}"
    );
    // And an operation afterwards is recorded truly as well.
    let (_, answers) = own.run(
        &User::new(WRITER),
        APP,
        &[blackbox::own_database::own_execute(
            "user",
            "update things set title = 'after the shadows' where id = 101",
        )],
    );
    assert!(answers[0]["ok"].is_object(), "{}", answers[0]);
    let rows = history_after(&db, row["id"].as_i64().unwrap());
    assert_eq!(rows.len(), 1, "{}", lines(&rows));
    assert_eq!(rows[0]["new"]["title"], "after the shadows");
    assert_eq!(rows[0]["user_id"], WRITER);
}
