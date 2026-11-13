//! What is checked before a Version's migrations are committed (CONTRACT.md,
//! "The platform's part of an own database is out of a migration's reach:
//! What is checked before a Version's migrations are committed", and "The
//! capture itself" on `VM003`; ticket "The Change history is kept out of a
//! Contributor's reach", acceptance criteria 2, 4 and 5: a capture trigger
//! disabled and enabled again, dropped, renamed or replaced; a deferred
//! trigger or a cursor `WITH HOLD` left to change rows at the commit; a
//! cast to `json` made by a migration).
//!
//! The capture triggers are found in the catalog (the triggers of a table
//! whose function is in schema `vm`), never by a name the platform might
//! choose. Each refused Version must leave the database exactly as it was
//! (`out_of_reach::untouched`): the rows the hostile migration changed
//! while the capture was off are as before.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history, history_count};
use blackbox::out_of_reach::{
    ANY_MIGRATION, Answers, Case, ON_DELETE, ON_INSERT, ON_UPDATE, WRITER, base_and,
    capture_trigger, capture_triggers, failed, install_seeded, refused, run_cases, start,
    with_function,
};
use blackbox::own_database::{Migration, OwnDatabase, assert_did_not_run, ident, own_execute};
use blackbox::scenario::lines;

const CAPTURE_CHANGED: Answers = &[refused("capture-changed")];
const CHANGED_OR_MISSING: Answers = &[refused("capture-changed"), refused("capture-missing")];
const DEFERRABLE: Answers = &[refused("deferrable")];
const JSON_CAST: Answers = &[refused("json-cast")];
const IN_VM: Answers = &[refused("in-vm")];
// CONTRACT.md: a migration may not "make a trigger of its own that calls a
// function of `vm` (`42501`)".
const DENIED: Answers = &[failed("42501")];
const DEFERRABLE_OR_CHANGED: Answers = &[refused("deferrable"), refused("capture-changed")];
const FOREIGN_KEY_FAILED: Answers = &[failed("23503")];
const VM003_OR_JSON_CAST: Answers = &[failed("VM003"), refused("json-cast")];

/// A PL/pgSQL body that disables every capture trigger of `table`, runs
/// `change`, and enables them again, finding them in the catalog; `ret` is
/// put before its last `END`.
fn off_change_on_body(table: &str, change: &str, ret: &str) -> String {
    format!(
        "
DECLARE t record;
BEGIN
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = '{table}'::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace LOOP
    EXECUTE format('ALTER TABLE {table} DISABLE TRIGGER %I', t.tgname);
  END LOOP;
  {change};
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = '{table}'::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace LOOP
    EXECUTE format('ALTER TABLE {table} ENABLE TRIGGER %I', t.tgname);
  END LOOP;
  {ret}
END "
    )
}

/// A DO block of [`off_change_on_body`].
fn off_change_on(table: &str, change: &str) -> String {
    format!("DO $f${}$f$", off_change_on_body(table, change, ""))
}

/// `definition` of a trigger on `from` made for `to` under `name`.
fn moved(definition: &str, old_name: &str, name: &str, from: &str, to: &str) -> String {
    definition
        .replacen(
            &format!("TRIGGER {old_name} "),
            &format!("TRIGGER {name} "),
            1,
        )
        .replacen(
            &format!(" ON public.{from} "),
            &format!(" ON public.{to} "),
            1,
        )
        .replacen(&format!(" ON {from} "), &format!(" ON public.{to} "), 1)
}

/// The update capture trigger of `things`, quoted.
fn things_update(db: &OwnDatabase) -> String {
    ident(&capture_trigger(db, "things", ON_UPDATE).name)
}

#[test]
fn a_capture_trigger_switched_off_dropped_renamed_or_replaced_is_refused_also_when_put_back() {
    let own = start();
    let cases = vec![
        Case::statements("disable-update-enable", CAPTURE_CHANGED, |db| {
            let u = things_update(db);
            vec![
                format!(r#"ALTER TABLE "things" DISABLE TRIGGER {u}"#),
                r#"UPDATE "things" SET "title" = 'changed unseen'"#.to_owned(),
                format!(r#"ALTER TABLE "things" ENABLE TRIGGER {u}"#),
            ]
        })
        .mentioning("things"),
        Case::statements("disable-user-triggers", CAPTURE_CHANGED, |_| {
            vec![
                r#"ALTER TABLE "things" DISABLE TRIGGER USER"#.to_owned(),
                r#"UPDATE "things" SET "size" = 0"#.to_owned(),
                r#"ALTER TABLE "things" ENABLE TRIGGER USER"#.to_owned(),
            ]
        })
        .mentioning("things"),
        Case::statements("disable-insert-and-insert", CAPTURE_CHANGED, |db| {
            let i = ident(&capture_trigger(db, "keyless", ON_INSERT).name);
            vec![
                format!(r#"ALTER TABLE "keyless" DISABLE TRIGGER {i}"#),
                r#"INSERT INTO "keyless" ("a", "b") VALUES ('unseen', 9)"#.to_owned(),
                format!(r#"ALTER TABLE "keyless" ENABLE TRIGGER {i}"#),
            ]
        })
        .mentioning("keyless"),
        Case::statements("disable-delete-and-delete", CAPTURE_CHANGED, |db| {
            let d = ident(&capture_trigger(db, "children", ON_DELETE).name);
            vec![
                format!(r#"ALTER TABLE "children" DISABLE TRIGGER {d}"#),
                r#"DELETE FROM "children" WHERE "id" = 20"#.to_owned(),
                format!(r#"ALTER TABLE "children" ENABLE TRIGGER {d}"#),
            ]
        })
        .mentioning("children"),
        Case::statements("drop-and-recreate-calling-its-own-function", CHANGED_OR_MISSING, |db| {
            let trigger = capture_trigger(db, "things", ON_UPDATE);
            vec![
                r#"CREATE FUNCTION "fake"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NULL; END $f$"#.to_owned(),
                format!(r#"DROP TRIGGER {} ON "things""#, ident(&trigger.name)),
                with_function(&trigger.definition, "public.fake()"),
                r#"UPDATE "things" SET "title" = 'changed under a fake capture'"#.to_owned(),
            ]
        })
        .mentioning("things"),
        // CONTRACT.md: "A migration that drops one and tries to make it
        // again fails at that statement with `42501`."
        Case::statements("drop-change-recreate-the-same", DENIED, |db| {
            let trigger = capture_trigger(db, "things", ON_UPDATE);
            vec![
                format!(r#"DROP TRIGGER {} ON "things""#, ident(&trigger.name)),
                r#"UPDATE "things" SET "title" = 'changed while dropped'"#.to_owned(),
                trigger.definition,
            ]
        }),
        Case::statements("rename-change-rename-back", CAPTURE_CHANGED, |db| {
            let u = things_update(db);
            vec![
                format!(r#"ALTER TRIGGER {u} ON "things" RENAME TO "elsewhere""#),
                r#"UPDATE "things" SET "title" = 'changed while renamed'"#.to_owned(),
                format!(r#"ALTER TRIGGER "elsewhere" ON "things" RENAME TO {u}"#),
            ]
        })
        .mentioning("things"),
        Case::statements("rename-and-back-alone", CAPTURE_CHANGED, |db| {
            let u = things_update(db);
            vec![
                format!(r#"ALTER TRIGGER {u} ON "things" RENAME TO "elsewhere""#),
                format!(r#"ALTER TRIGGER "elsewhere" ON "things" RENAME TO {u}"#),
            ]
        })
        .mentioning("things"),
        Case::statements("enable-replica-change-enable", CAPTURE_CHANGED, |db| {
            let u = things_update(db);
            vec![
                format!(r#"ALTER TABLE "things" ENABLE REPLICA TRIGGER {u}"#),
                r#"UPDATE "things" SET "title" = 'changed while replica-only'"#.to_owned(),
                format!(r#"ALTER TABLE "things" ENABLE TRIGGER {u}"#),
            ]
        })
        .mentioning("things"),
        Case::statements("do-block-with-dynamic-execute", CAPTURE_CHANGED, |_| {
            vec![off_change_on(
                "things",
                "UPDATE things SET title = 'changed by a DO block'",
            )]
        })
        .mentioning("things"),
        Case::statements("a-function-the-migration-makes-and-calls", CAPTURE_CHANGED, |_| {
            vec![
                format!(
                    r#"CREATE FUNCTION "sneak"() RETURNS integer LANGUAGE plpgsql AS $f${}$f$"#,
                    off_change_on_body(
                        "things",
                        "UPDATE things SET title = 'changed by a function'",
                        "RETURN 1;"
                    )
                ),
                r#"SELECT "sneak"()"#.to_owned(),
            ]
        })
        .mentioning("things"),
        Case::statements("inside-a-released-subtransaction", CAPTURE_CHANGED, |_| {
            vec![format!(
                "DO $g$ BEGIN BEGIN PERFORM 1; {}; EXCEPTION WHEN others THEN RAISE; END; END $g$",
                // An EXCEPTION block is a savepoint, released when nothing
                // raises.
                "EXECUTE 'ALTER TABLE things DISABLE TRIGGER USER'; UPDATE things SET title = 'changed in a subtransaction'; EXECUTE 'ALTER TABLE things ENABLE TRIGGER USER'"
            )]
        })
        .mentioning("things"),
        Case::statements("a-partition", CAPTURE_CHANGED, |_| {
            vec![off_change_on(
                "measures_low",
                "UPDATE measures SET v = 'changed in a partition' WHERE k = 5",
            )]
        })
        .mentioning("measures"),
        Case::statements("another-schema", CAPTURE_CHANGED, |_| {
            vec![off_change_on(
                "inventory.stock",
                "UPDATE inventory.stock SET qty = 0",
            )]
        })
        .mentioning("stock"),
    ];
    run_cases(&own, "changed", cases);
}

#[test]
fn a_table_without_its_whole_capture_at_the_end_or_with_another_trigger_on_a_vm_function_is_refused()
 {
    let own = start();
    let cases = vec![
        Case::statements("left-disabled", CHANGED_OR_MISSING, |db| {
            vec![format!(
                r#"ALTER TABLE "things" DISABLE TRIGGER {}"#,
                things_update(db)
            )]
        })
        .mentioning("things"),
        Case::statements("left-dropped", CHANGED_OR_MISSING, |db| {
            vec![format!(r#"DROP TRIGGER {} ON "things""#, things_update(db))]
        })
        .mentioning("things"),
        // A table made by the Version's first migration loses its capture
        // in the second: its capture existed before the second migration
        // and is missing at the end, so either rule is true.
        Case::migrations(
            "new-table-capture-dropped-later",
            CHANGED_OR_MISSING,
            |_| {
                vec![
                    Migration::new(
                        "0004_fresh",
                        &[r#"CREATE TABLE "fresh" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
                    ),
                    Migration::new(
                        "0005_uncaptured",
                        &["DO $f$
DECLARE t record;
BEGIN
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = 'fresh'::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace LOOP
    EXECUTE format('DROP TRIGGER %I ON fresh', t.tgname);
  END LOOP;
END $f$;"],
                    ),
                ]
            },
        )
        .naming(ANY_MIGRATION)
        .mentioning("fresh"),
        // a-attacks.sql A58: a trigger of its own on a vm function
        // (CONTRACT.md: `42501`, at the statement).
        Case::statements("own-trigger-calling-a-vm-function", DENIED, |db| {
            let trigger = capture_trigger(db, "things", ON_INSERT);
            vec![moved(
                &trigger.definition,
                &trigger.name,
                "mine",
                "things",
                "keyless",
            )]
        }),
        // a-attacks.sql A59: the capture trigger replaced with its old and
        // new rows swapped.
        Case::statements("capture-replaced-with-old-and-new-swapped", DENIED, |db| {
            let trigger = capture_trigger(db, "things", ON_UPDATE);
            let tables = db.rows(&format!(
                "SELECT tgoldtable::text, tgnewtable::text FROM pg_trigger
                      WHERE tgrelid = 'things'::regclass AND tgname = {}",
                blackbox::own_database::literal(&trigger.name)
            ));
            let (old, new) = match tables.first() {
                Some(row) => (
                    row.first().cloned().flatten(),
                    row.get(1).cloned().flatten(),
                ),
                None => (None, None),
            };
            let referencing = match (old, new) {
                (Some(old), Some(new)) => {
                    format!(
                        "REFERENCING OLD TABLE AS {} NEW TABLE AS {} ",
                        ident(&new),
                        ident(&old)
                    )
                }
                _ => String::new(),
            };
            vec![format!(
                "CREATE OR REPLACE TRIGGER {} AFTER UPDATE ON public.things {referencing}FOR EACH STATEMENT EXECUTE FUNCTION {}",
                ident(&trigger.name),
                trigger.function
            )]
        }),
    ];
    run_cases(&own, "missing", cases);
}

/// The body of a trigger function that switches the capture of `things`
/// off, changes its rows and switches it on again.
const SNEAK: &str = r#"CREATE FUNCTION "sneak"() RETURNS trigger LANGUAGE plpgsql AS $f$
DECLARE t record;
BEGIN
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = 'things'::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace LOOP
    EXECUTE format('ALTER TABLE things DISABLE TRIGGER %I', t.tgname);
  END LOOP;
  UPDATE things SET title = 'changed at commit';
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = 'things'::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace LOOP
    EXECUTE format('ALTER TABLE things ENABLE TRIGGER %I', t.tgname);
  END LOOP;
  RETURN NULL;
END $f$"#;

#[test]
fn nothing_deferrable_is_left_to_run_at_the_commit() {
    let own = start();
    let cases = vec![
        Case::statements("deferrable-constraint-trigger", DEFERRABLE, |_| {
            vec![
                r#"CREATE FUNCTION "nop"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NULL; END $f$"#.to_owned(),
                r#"CREATE CONSTRAINT TRIGGER "late" AFTER UPDATE ON "parents" DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION "nop"()"#.to_owned(),
            ]
        }),
        Case::statements("deferrable-initially-immediate-trigger", DEFERRABLE, |_| {
            vec![
                r#"CREATE FUNCTION "nop"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NULL; END $f$"#.to_owned(),
                r#"CREATE CONSTRAINT TRIGGER "late" AFTER UPDATE ON "parents" DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION "nop"()"#.to_owned(),
            ]
        }),
        // b-checks.sql B31: queued by an update, it would switch the
        // capture off, change rows and switch it on at the commit. Made
        // immediate before the checks, it runs before them and changes the
        // capture triggers too: either rule may be the one named.
        Case::statements(
            "deferred-trigger-switching-the-capture-off-at-commit",
            DEFERRABLE_OR_CHANGED,
            |_| {
                vec![
                    SNEAK.to_owned(),
                    r#"CREATE CONSTRAINT TRIGGER "sneak" AFTER UPDATE ON "parents" DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION "sneak"()"#.to_owned(),
                    r#"UPDATE "parents" SET "name" = "name" WHERE "id" = 1"#.to_owned(),
                ]
            },
        ),
        Case::statements("deferrable-foreign-key", DEFERRABLE, |_| {
            vec![r#"ALTER TABLE "children" ADD CONSTRAINT "children_parent_late_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."parents"("id") DEFERRABLE INITIALLY DEFERRED"#.to_owned()]
        }),
        Case::statements("deferrable-unique", DEFERRABLE, |_| {
            vec![r#"ALTER TABLE "things" ADD CONSTRAINT "things_title_unique" UNIQUE ("title") DEFERRABLE"#.to_owned()]
        }),
        Case::statements("deferrable-foreign-key-in-a-new-table", DEFERRABLE, |_| {
            vec![r#"CREATE TABLE "late_children" ("id" integer PRIMARY KEY NOT NULL, "parent_id" integer REFERENCES "parents"("id") DEFERRABLE INITIALLY DEFERRED)"#.to_owned()]
        }),
        // CONTRACT.md: "A constraint that failed when made immediate is
        // `migration-failed` with its own SQLSTATE."
        Case::statements("deferred-foreign-key-violated", FOREIGN_KEY_FAILED, |_| {
            vec![
                r#"CREATE TABLE "late_children" ("id" integer PRIMARY KEY NOT NULL, "parent_id" integer REFERENCES "parents"("id") DEFERRABLE INITIALLY DEFERRED)"#.to_owned(),
                r#"INSERT INTO "late_children" VALUES (1, 999)"#.to_owned(),
            ]
        }),
    ];
    run_cases(&own, "deferrable", cases);
}

/// b-checks.sql B33: a cursor `WITH HOLD` whose query would switch the
/// capture off, change rows and switch it on at the commit. CONTRACT.md: it
/// is closed by the platform before the checks and its query never runs.
/// No rule refuses a cursor, so the Version applies; the rows are
/// unchanged and the capture triggers are the very ones of before.
#[test]
fn a_cursor_with_hold_is_closed_before_the_commit_and_its_query_never_runs() {
    let own = start();
    let db = install_seeded(&own, APP);
    let things = db.column("SELECT to_jsonb(t)::text FROM things t ORDER BY id");
    let history_before = history(&db);
    let triggers_before = capture_triggers(&db);
    let oids = |db: &OwnDatabase| {
        db.column(
            "SELECT tgrelid::regclass::text || '.' || tgname || ' ' || oid::text || ' ' || tgenabled::text
               FROM pg_trigger WHERE NOT tgisinternal AND tgrelid = 'things'::regclass ORDER BY 1",
        )
    };
    let oids_before = oids(&db);
    let body = SNEAK
        .replace(
            "\"sneak\"() RETURNS trigger",
            "\"sneak2\"() RETURNS integer",
        )
        .replace("'changed at commit'", "'changed by a cursor at commit'")
        .replace("RETURN NULL;", "RETURN 1;");
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_cursor",
            &[
                &body,
                r#"DECLARE "c" CURSOR WITH HOLD FOR SELECT "sneak2"()"#,
            ],
        )]),
    );
    assert_eq!(
        sent.status,
        201,
        "expected the Version applied with its cursor closed (CONTRACT.md names no rule that refuses a cursor), got\n{}",
        sent.describe()
    );
    assert_eq!(
        db.column("SELECT to_jsonb(t)::text FROM things t ORDER BY id"),
        things,
        "expected the cursor's query never to run: the rows of `things` changed"
    );
    assert_eq!(
        history(&db),
        history_before,
        "expected no history row from the cursor's query"
    );
    assert_eq!(capture_triggers(&db), triggers_before);
    assert_eq!(
        oids(&db),
        oids_before,
        "expected the capture triggers of `things` to be the very same, enabled"
    );
}

/// a-attacks.sql A80 to A88, the casts of CONTRACT.md's `json-cast`.
#[test]
fn a_cast_to_json_made_by_a_migration_is_refused_and_never_runs_with_the_platform_s_rights() {
    let own = start();
    // The function a migration would have Postgres call inside the
    // capture: it carries a digest of the keys out through `stash` (a
    // sequence keeps its value through a rollback), deletes the history,
    // and notes who it ran as.
    const TROJAN: &str = r#"CREATE FUNCTION "mood_json"("m" "mood") RETURNS json LANGUAGE plpgsql AS $f$
BEGIN
  PERFORM setval('public.stash', (SELECT 1000 + length(k::text) FROM vm.keys k LIMIT 1));
  DELETE FROM vm.history;
  INSERT INTO public."loot" SELECT current_user, 'keys ' || length(k::text) FROM vm.keys k;
  RETURN to_json("m"::text);
END $f$"#;
    let cases = vec![
        Case::statements("cast-to-json", JSON_CAST, |_| {
            vec![
                r#"CREATE TYPE "mood" AS ENUM('ok', 'sad')"#.to_owned(),
                r#"CREATE FUNCTION "mood_json"("m" "mood") RETURNS json LANGUAGE sql AS $f$ SELECT to_json("m"::text) $f$"#.to_owned(),
                r#"CREATE CAST ("mood" AS json) WITH FUNCTION "mood_json"("mood")"#.to_owned(),
            ]
        }),
        Case::statements("cast-to-jsonb", JSON_CAST, |_| {
            vec![
                r#"CREATE TYPE "mood" AS ENUM('ok', 'sad')"#.to_owned(),
                r#"CREATE FUNCTION "mood_jsonb"("m" "mood") RETURNS jsonb LANGUAGE sql AS $f$ SELECT to_jsonb("m"::text) $f$"#.to_owned(),
                r#"CREATE CAST ("mood" AS jsonb) WITH FUNCTION "mood_jsonb"("mood")"#.to_owned(),
            ]
        }),
        Case::statements("cast-to-json-of-a-composite-type", JSON_CAST, |_| {
            vec![
                r#"CREATE TYPE "pair" AS ("a" integer, "b" text)"#.to_owned(),
                r#"CREATE FUNCTION "pair_json"("p" "pair") RETURNS json LANGUAGE sql AS $f$ SELECT json_build_object('a', ("p")."a") $f$"#.to_owned(),
                r#"CREATE CAST ("pair" AS json) WITH FUNCTION "pair_json"("pair")"#.to_owned(),
            ]
        }),
        // A81: the cast, a table of the enum made in one migration (so it
        // is captured), a row written to it in the next.
        Case::migrations(
            "trojan-cast-and-a-captured-write",
            VM003_OR_JSON_CAST,
            |_| {
                vec![
                    Migration::new(
                        "0004_trojan",
                        &[
                            r#"CREATE TYPE "mood" AS ENUM('ok', 'sad');"#,
                            r#"CREATE TABLE "loot" ("who" text, "what" text);"#,
                            TROJAN,
                            r#"CREATE CAST ("mood" AS json) WITH FUNCTION "mood_json"("mood");"#,
                            r#"CREATE TABLE "moods" ("id" integer PRIMARY KEY NOT NULL, "m" "mood");"#,
                        ],
                    ),
                    Migration::new(
                        "0005_trojan_write",
                        &[r#"INSERT INTO "moods" VALUES (1, 'sad');"#],
                    ),
                ]
            },
        )
        .naming(ANY_MIGRATION),
        // A86l: while the cast exists, a write to any captured table.
        Case::statements(
            "cast-then-a-write-to-a-captured-table",
            VM003_OR_JSON_CAST,
            |_| {
                vec![
                    r#"CREATE TYPE "mood" AS ENUM('ok', 'sad')"#.to_owned(),
                    r#"CREATE TABLE "loot" ("who" text, "what" text)"#.to_owned(),
                    TROJAN.to_owned(),
                    r#"CREATE CAST ("mood" AS json) WITH FUNCTION "mood_json"("mood")"#.to_owned(),
                    r#"ALTER TABLE "things" ADD COLUMN "m" "mood" DEFAULT 'sad'"#.to_owned(),
                    r#"UPDATE "things" SET "title" = "title" WHERE "id" = 101"#.to_owned(),
                ]
            },
        ),
    ];
    run_cases(&own, "casts", cases);
}

/// a-attacks.sql A48: Postgres lets a role with `CREATE` on the database
/// put a trusted extension into any schema, `vm` too.
#[test]
fn an_extension_put_into_vm_is_refused() {
    let own = start();
    let probe = install_seeded(&own, "probe");
    let available = probe
        .superuser()
        .value("SELECT count(*)::text FROM pg_available_extensions WHERE name = 'hstore'")
        .as_deref()
        == Some("1");
    if !available {
        eprintln!(
            "SKIPPED: the test Postgres has no hstore extension, so `CREATE EXTENSION hstore SCHEMA vm` cannot be tried"
        );
        return;
    }
    run_cases(
        &own,
        "in-vm",
        vec![
            Case::statements("hstore-into-vm", IN_VM, |_| {
                vec!["CREATE EXTENSION hstore SCHEMA vm".to_owned()]
            }),
            Case::statements("hstore-into-vm-if-not-exists", IN_VM, |_| {
                vec!["CREATE EXTENSION IF NOT EXISTS \"hstore\" WITH SCHEMA \"vm\"".to_owned()]
            }),
        ],
    );
}

/// CONTRACT.md, "The capture itself": "While a cast of the `json-cast`
/// kind exists, every change of a captured table fails with `VM003`,
/// whoever makes it." The cast is made by the harness's superuser, as the
/// owner role, outside any migration.
#[test]
fn while_a_user_made_cast_to_json_exists_every_change_of_a_captured_table_fails_with_vm003() {
    let own = start();
    let db = install_seeded(&own, APP);
    let sql = db.superuser();
    sql.batch(&format!("SET ROLE {}", ident(&db.owner)));
    sql.batch(
        r#"CREATE TYPE public."mood" AS ENUM ('ok', 'sad');
           CREATE FUNCTION public."mood_json"("m" public."mood") RETURNS json LANGUAGE sql AS $f$ SELECT to_json("m"::text) $f$;
           CREATE CAST (public."mood" AS json) WITH FUNCTION public."mood_json"(public."mood");"#,
    );
    drop(sql);
    let rows_before = db.column("SELECT to_jsonb(t)::text FROM things t ORDER BY id");
    let count_before = history_count(&db);
    let writes = [
        "insert into things (id, title) values (201, 'written while a cast exists')",
        "update things set title = 'updated while a cast exists' where id = 101",
        "delete from things where id = 102",
        "insert into keyless (a, b) values ('cast', 3)",
    ];
    let mut wrong = Vec::new();
    for actor in ["user", "application"] {
        for write in writes {
            let (_, answer) = own.one(&User::new(WRITER), APP, &own_execute(actor, write));
            let code = assert_did_not_run(&answer, write);
            if code.as_deref() != Some("VM003") || answer["error"]["kind"] != "database" {
                wrong.push(format!("  as {actor}: {write}: {answer}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "expected every change of a captured table to fail with {{ error: {{ kind: \"database\", code: \"VM003\" }} }} while a user-made cast to json exists, got:\n{}",
        wrong.join("\n")
    );
    assert_eq!(
        db.column("SELECT to_jsonb(t)::text FROM things t ORDER BY id"),
        rows_before,
        "expected nothing changed"
    );
    assert_eq!(
        history_count(&db),
        count_before,
        "expected nothing recorded"
    );
    // Without the cast, writes are recorded again.
    db.superuser().batch(r#"DROP CAST (public."mood" AS json)"#);
    let (_, answer) = own.one(&User::new(WRITER), APP, &own_execute("user", writes[0]));
    assert!(answer["ok"].is_object(), "{answer}");
    let recorded = history(&db);
    assert_eq!(
        recorded.len(),
        usize::try_from(count_before).unwrap() + 1,
        "{}",
        lines(&recorded)
    );
}
