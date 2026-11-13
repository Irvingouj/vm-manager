//! What a migration may not leave behind, refused when it is applied
//! (CONTRACT.md, "The platform's part of an own database is out of a
//! migration's reach": "What a migration cannot do", the trigger of its own
//! that calls a function of `vm`; "Rules checked on the text",
//! `copy-stream`; "What a migration leaves behind: Refused when the
//! migration is applied", `security-definer` and `index-code`).
//!
//! Each hostile Version is sent to a fresh Application whose first Version
//! made captured tables and whose operations left rows and history rows;
//! each must be answered as the contract says and leave the own database
//! exactly as it was (`out_of_reach::untouched`), Version 1 serving and the
//! owner role unable to log in.
//!
//! An index on a function, an operator or a domain the runtime role made
//! (where an earlier Version let it create) is refused `index-code` too,
//! and so is one on a function, an operator or a domain a migration added
//! to the extension it installed (`ALTER EXTENSION pg_trgm ADD ...`), in
//! the same Version or an earlier one; a `SECURITY DEFINER` function so
//! added is refused `security-definer`. A range type whose
//! `subtype_diff` is a function of the Application's is refused
//! `type-code`, with or without a column of it, the function made earlier
//! or added to an extension; one with no `subtype_diff`, with `float8mi`,
//! and Postgres's own range types are allowed.
//!
//! What the contract allows beside them is sent too, each to a fresh
//! Application: an invoker function, indexes on Postgres's own functions and
//! operators, and the trusted extensions `pg_trgm`, `citext` and `unaccent`
//! with indexes on their functions and operators (their functions are not
//! the migration role's). Each must apply (`201`) and leave the tables it
//! makes captured: a row written through the Application is in the history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use blackbox::User;
use blackbox::history::{history_after, last_history_id};
use blackbox::out_of_reach::{
    ANY_CODE, ANY_MIGRATION, Answers, CaptureTrigger, Case, ON_DELETE, ON_INSERT, ON_TRUNCATE,
    ON_UPDATE, WRITER, assert_answer, base_and, can_log_in, capture_triggers, failed,
    install_seeded, panic_text, refused, run_cases, start,
};
use blackbox::own_database::{Migration, Own, OwnDatabase, ident, own_execute, own_query};
use blackbox::scenario::lines;

/// CONTRACT.md: "Make a trigger of its own that calls a function of `vm`
/// (`42501`)".
const DENIED: Answers = &[failed("42501")];
const SECURITY_DEFINER: Answers = &[refused("security-definer")];
const INDEX_CODE: Answers = &[refused("index-code")];
const COPY_STREAM: Answers = &[refused("copy-stream")];
/// `COPY ... FROM STDIN` followed by its rows: the rows are no statement,
/// so the text may be found unreadable first.
const COPY_STREAM_OR_UNPARSEABLE: Answers = &[refused("copy-stream"), refused("unparseable")];
/// CONTRACT.md: "`COPY t FROM '/file'`-style forms are Postgres's to
/// refuse" (the coordinator's brief); so is COPY to the client inside a
/// function body, which the text does not show.
const POSTGRES_REFUSES: Answers = &[failed(ANY_CODE)];

/// `definition` (a capture trigger's `CREATE TRIGGER` statement) made on
/// `to` under the name `name`.
fn moved(trigger: &CaptureTrigger, name: &str, to: &str) -> String {
    let from = &trigger.table;
    trigger
        .definition
        .replacen(
            &format!("TRIGGER {} ", trigger.name),
            &format!("TRIGGER {} ", ident(name)),
            1,
        )
        .replacen(&format!(" ON public.{from} "), &format!(" ON {to} "), 1)
        .replacen(&format!(" ON {from} "), &format!(" ON {to} "), 1)
}

/// The capture triggers of `things`, one per event it fires on.
fn things_triggers(db: &OwnDatabase) -> Vec<CaptureTrigger> {
    let all: Vec<CaptureTrigger> = capture_triggers(db)
        .into_iter()
        .filter(|t| t.table == "things")
        .collect();
    assert!(
        [ON_INSERT, ON_UPDATE, ON_DELETE]
            .iter()
            .all(|e| all.iter().any(|t| t.tgtype & e != 0)),
        "the harness's own check: expected capture triggers of things on insert, update and delete, got {all:#?}"
    );
    all
}

/// The names of the transition tables of a capture trigger, if it has any.
fn transition_tables(
    db: &OwnDatabase,
    trigger: &CaptureTrigger,
) -> (Option<String>, Option<String>) {
    let rows = db.rows(&format!(
        "SELECT tgoldtable::text, tgnewtable::text FROM pg_trigger
          WHERE tgrelid = {}::regclass AND tgname = {}",
        blackbox::own_database::literal(&trigger.table),
        blackbox::own_database::literal(&trigger.name)
    ));
    let row = rows.first().cloned().unwrap_or_default();
    (
        row.first().cloned().flatten(),
        row.get(1).cloned().flatten(),
    )
}

/// An `AFTER UPDATE` statement trigger on `table` calling `function` with
/// its old and new transition tables named `old_as` and `new_as`.
fn update_trigger(name: &str, table: &str, old_as: &str, new_as: &str, function: &str) -> String {
    format!(
        "CREATE TRIGGER {} AFTER UPDATE ON {table} REFERENCING OLD TABLE AS {} NEW TABLE AS {} FOR EACH STATEMENT EXECUTE FUNCTION {function}",
        ident(name),
        ident(old_as),
        ident(new_as)
    )
}

#[test]
fn a_migration_cannot_make_a_trigger_of_its_own_that_calls_a_function_of_vm() {
    let own = start();
    let mut cases = Vec::new();
    // One case per capture trigger of `things` (one per event, whatever
    // the platform names them), moved onto `keyless`, an older table, and
    // onto a table the same migration makes.
    for (event, label_old, label_new) in [
        (
            ON_INSERT,
            "insert-trigger-on-an-old-table",
            "insert-trigger-on-a-new-table",
        ),
        (
            ON_UPDATE,
            "update-trigger-on-an-old-table",
            "update-trigger-on-a-new-table",
        ),
        (
            ON_DELETE,
            "delete-trigger-on-an-old-table",
            "delete-trigger-on-a-new-table",
        ),
    ] {
        cases.push(Case::statements(label_old, DENIED, move |db| {
            things_triggers(db)
                .iter()
                .filter(|t| t.tgtype & event != 0)
                .map(|t| moved(t, "mine", "public.keyless"))
                .collect()
        }));
        cases.push(Case::statements(label_new, DENIED, move |db| {
            let mut statements = vec![
                r#"CREATE TABLE "fresh" ("id" integer PRIMARY KEY NOT NULL, "v" text)"#.to_owned(),
            ];
            statements.extend(
                things_triggers(db)
                    .iter()
                    .filter(|t| t.tgtype & event != 0)
                    .map(|t| moved(t, "mine", "public.fresh")),
            );
            statements
        }));
    }
    // The truncate one, when the platform's capture has one.
    cases.push(Case::statements("truncate-trigger-on-an-old-table", DENIED, |db| {
        let truncating: Vec<String> = capture_triggers(db)
            .iter()
            .filter(|t| t.tgtype & ON_TRUNCATE != 0)
            .take(1)
            .map(|t| moved(t, "mine", "public.keyless"))
            .collect();
        if truncating.is_empty() {
            // No truncate trigger calls a function of vm: the same with the
            // update capture's function on TRUNCATE.
            let f = things_triggers(db)
                .into_iter()
                .find(|t| t.tgtype & ON_UPDATE != 0)
                .map(|t| t.function)
                .unwrap_or_default();
            return vec![format!(
                "CREATE TRIGGER \"mine\" AFTER TRUNCATE ON public.keyless FOR EACH STATEMENT EXECUTE FUNCTION {f}"
            )];
        }
        truncating
    }));
    // Old and new exchanged, on an old table, under a name of its own.
    cases.push(Case::statements(
        "exchanged-on-an-old-table",
        DENIED,
        |db| {
            let update = things_triggers(db)
                .into_iter()
                .find(|t| t.tgtype & ON_UPDATE != 0)
                .unwrap();
            let (old, new) = transition_tables(db, &update);
            let old = old.unwrap_or_else(|| "vm_old".to_owned());
            let new = new.unwrap_or_else(|| "vm_new".to_owned());
            vec![update_trigger(
                "mine",
                "public.keyless",
                &new,
                &old,
                &update.function,
            )]
        },
    ));
    // The coordinator's literal form: `REFERENCING OLD TABLE AS vm_new NEW
    // TABLE AS vm_old`, on a table the migration makes.
    cases.push(Case::statements(
        "vm-new-and-vm-old-exchanged-on-a-new-table",
        DENIED,
        |db| {
            let update = things_triggers(db)
                .into_iter()
                .find(|t| t.tgtype & ON_UPDATE != 0)
                .unwrap();
            vec![
                r#"CREATE TABLE "fresh" ("id" integer PRIMARY KEY NOT NULL, "v" text)"#.to_owned(),
                update_trigger("mine", "public.fresh", "vm_new", "vm_old", &update.function),
            ]
        },
    ));
    // A capture trigger dropped, then made again by the migration under its
    // own name with old and new exchanged.
    cases.push(Case::statements(
        "dropped-and-made-again-exchanged",
        DENIED,
        |db| {
            let update = things_triggers(db)
                .into_iter()
                .find(|t| t.tgtype & ON_UPDATE != 0)
                .unwrap();
            let (old, new) = transition_tables(db, &update);
            let old = old.unwrap_or_else(|| "vm_old".to_owned());
            let new = new.unwrap_or_else(|| "vm_new".to_owned());
            vec![
                format!("DROP TRIGGER {} ON public.things", ident(&update.name)),
                update_trigger(&update.name, "public.things", &new, &old, &update.function),
            ]
        },
    ));
    // Made through dynamic SQL in a DO block.
    cases.push(Case::statements("through-a-do-block", DENIED, |db| {
        let insert = things_triggers(db)
            .into_iter()
            .find(|t| t.tgtype & ON_INSERT != 0)
            .unwrap();
        let statement = moved(&insert, "mine", "public.keyless").replace('\'', "''");
        vec![format!("DO $f$ BEGIN EXECUTE '{statement}'; END $f$")]
    }));
    run_cases(&own, "vm-trigger", cases);
}

/// A plain function of the migration's: `f(integer) RETURNS integer`.
const PLPGSQL_F: &str = r#"CREATE FUNCTION "f"("x" integer) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN "x"; END $f$"#;
const SQL_F: &str = r#"CREATE FUNCTION "g"("x" integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT "x" + 1 $f$"#;

fn statements(templates: &'static [&'static str]) -> impl Fn(&OwnDatabase) -> Vec<String> {
    move |_| templates.iter().map(|t| (*t).to_owned()).collect()
}

#[test]
fn a_security_definer_function_or_procedure_is_refused() {
    let own = start();
    let cases = vec![
        Case::statements("function-in-public", SECURITY_DEFINER, statements(&[
            r#"CREATE FUNCTION "owner_powers"() RETURNS integer LANGUAGE sql SECURITY DEFINER AS $f$ SELECT 1 $f$"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("plpgsql-function-with-a-search-path", SECURITY_DEFINER, statements(&[
            r#"CREATE FUNCTION "owner_powers"() RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $f$ BEGIN RETURN 1; END $f$"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("procedure-in-public", SECURITY_DEFINER, statements(&[
            r#"CREATE PROCEDURE "owner_proc"() LANGUAGE sql SECURITY DEFINER AS $f$ SELECT 1 $f$"#,
        ]))
        .mentioning("owner_proc"),
        Case::statements("function-in-a-schema-of-its-own", SECURITY_DEFINER, statements(&[
            r#"CREATE SCHEMA "tools""#,
            r#"CREATE FUNCTION "tools"."owner_powers"() RETURNS integer LANGUAGE sql SECURITY DEFINER AS $f$ SELECT 1 $f$"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("procedure-in-a-schema-of-its-own", SECURITY_DEFINER, statements(&[
            r#"CREATE SCHEMA "tools""#,
            r#"CREATE PROCEDURE "tools"."owner_proc"() LANGUAGE plpgsql SECURITY DEFINER AS $f$ BEGIN NULL; END $f$"#,
        ]))
        .mentioning("owner_proc"),
        Case::statements("made-definer-by-alter-function", SECURITY_DEFINER, statements(&[
            r#"CREATE FUNCTION "owner_powers"() RETURNS integer LANGUAGE sql AS $f$ SELECT 1 $f$"#,
            r#"ALTER FUNCTION "owner_powers"() SECURITY DEFINER"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("made-definer-by-alter-procedure", SECURITY_DEFINER, statements(&[
            r#"CREATE PROCEDURE "owner_proc"() LANGUAGE sql AS $f$ SELECT 1 $f$"#,
            r#"ALTER PROCEDURE "owner_proc"() SECURITY DEFINER"#,
        ]))
        .mentioning("owner_proc"),
        Case::statements("made-definer-by-alter-routine", SECURITY_DEFINER, statements(&[
            r#"CREATE FUNCTION "owner_powers"() RETURNS integer LANGUAGE sql AS $f$ SELECT 1 $f$"#,
            r#"ALTER ROUTINE "owner_powers"() EXTERNAL SECURITY DEFINER"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("an-earlier-version-s-function-replaced", SECURITY_DEFINER, statements(&[
            r#"CREATE OR REPLACE FUNCTION "run_sql"("statement" text) RETURNS integer LANGUAGE plpgsql SECURITY DEFINER AS $f$ BEGIN EXECUTE "statement"; RETURN 1; END $f$"#,
        ]))
        .mentioning("run_sql"),
        Case::statements("through-a-do-block", SECURITY_DEFINER, statements(&[
            r#"DO $f$ BEGIN EXECUTE 'CREATE FUNCTION owner_powers() RETURNS integer LANGUAGE sql SECURITY DEFINER AS ''SELECT 1'''; END $f$"#,
        ]))
        .mentioning("owner_powers"),
        Case::statements("a-trigger-function", SECURITY_DEFINER, statements(&[
            r#"CREATE FUNCTION "owner_trigger"() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $f$ BEGIN RETURN NEW; END $f$"#,
            r#"CREATE TRIGGER "owner_trigger" BEFORE INSERT ON "keyless" FOR EACH ROW EXECUTE FUNCTION "owner_trigger"()"#,
        ]))
        .mentioning("owner_trigger"),
        // Made invoker in one migration, altered in a later one of the same
        // Version.
        Case::migrations("altered-in-a-later-migration", SECURITY_DEFINER, |_| {
            vec![
                Migration::new(
                    "0004_invoker",
                    &[r#"CREATE FUNCTION "owner_powers"() RETURNS integer LANGUAGE sql AS $f$ SELECT 1 $f$;"#],
                ),
                Migration::new(
                    "0005_definer",
                    &[r#"ALTER FUNCTION "owner_powers"() SECURITY DEFINER;"#],
                ),
            ]
        })
        .naming(ANY_MIGRATION)
        .mentioning("owner_powers"),
    ];
    run_cases(&own, "definer", cases);
}

#[test]
fn an_index_or_a_statistic_calling_a_function_the_migrations_wrote_is_refused() {
    let own = start();
    let cases = vec![
        Case::statements("expression-on-a-plpgsql-function", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE INDEX "by_f" ON "things" ("f"("size"))"#,
        ]))
        .mentioning("by_f"),
        Case::statements("expression-on-a-sql-function", INDEX_CODE, statements(&[
            SQL_F,
            r#"CREATE INDEX "by_g" ON "things" ("g"("size"))"#,
        ]))
        .mentioning("by_g"),
        Case::statements("in-the-where", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE INDEX "partial_f" ON "things" ("id") WHERE "f"("size") > 0"#,
        ]))
        .mentioning("partial_f"),
        Case::statements("through-an-operator", INDEX_CODE, statements(&[
            r#"CREATE FUNCTION "plus"(integer, integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT $1 + $2 $f$"#,
            r#"CREATE OPERATOR @+@ (LEFTARG = integer, RIGHTARG = integer, FUNCTION = "plus")"#,
            r#"CREATE INDEX "by_op" ON "things" (("size" @+@ 1))"#,
        ]))
        .mentioning("by_op"),
        Case::statements("through-a-cast", INDEX_CODE, statements(&[
            r#"CREATE TYPE "mood" AS ENUM ('ok', 'sad')"#,
            r#"CREATE FUNCTION "int_mood"(integer) RETURNS "mood" LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN CASE WHEN $1 > 1 THEN 'sad' ELSE 'ok' END; END $f$"#,
            r#"CREATE CAST (integer AS "mood") WITH FUNCTION "int_mood"(integer)"#,
            r#"CREATE INDEX "by_cast" ON "things" ((("size")::"mood"))"#,
        ]))
        .mentioning("by_cast"),
        Case::statements("through-a-domain-s-check", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE DOMAIN "posint" AS integer CHECK ("f"(VALUE) > 0)"#,
            r#"CREATE INDEX "by_domain" ON "things" ((("size")::"posint"))"#,
        ]))
        .mentioning("by_domain"),
        // CONTRACT.md: "or a domain of theirs (a domain's constraint can be
        // given a function later)".
        Case::statements("through-a-domain-without-a-constraint", INDEX_CODE, statements(&[
            r#"CREATE DOMAIN "plainint" AS integer"#,
            r#"CREATE INDEX "by_plain_domain" ON "things" ((("size")::"plainint"))"#,
        ]))
        .mentioning("by_plain_domain"),
        Case::statements("an-extended-statistic", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE STATISTICS "st_f" ON ("f"("size")), ("id") FROM "things""#,
        ]))
        .mentioning("st_f"),
        Case::statements("an-exclusion-constraint", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"ALTER TABLE "counters" ADD CONSTRAINT "one_f" EXCLUDE USING btree (("f"("n")) WITH =)"#,
        ])),
        Case::statements("on-a-partitioned-table", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE INDEX "measures_by_f" ON "measures" ("f"("k"))"#,
        ]))
        .mentioning("measures_by_f"),
        Case::statements("a-function-in-a-schema-of-its-own", INDEX_CODE, statements(&[
            r#"CREATE SCHEMA "tools""#,
            r#"CREATE FUNCTION "tools"."f"("x" integer) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN "x"; END $f$"#,
            r#"CREATE INDEX "by_tools_f" ON "things" ("tools"."f"("size"))"#,
        ]))
        .mentioning("by_tools_f"),
        Case::statements("on-a-table-the-migration-makes", INDEX_CODE, statements(&[
            PLPGSQL_F,
            r#"CREATE TABLE "fresh" ("id" integer PRIMARY KEY NOT NULL, "n" integer)"#,
            r#"CREATE INDEX "fresh_by_f" ON "fresh" ("f"("n"))"#,
        ]))
        .mentioning("fresh_by_f"),
        // The index made by one migration, its function by an earlier one.
        Case::migrations("function-and-index-in-two-migrations", INDEX_CODE, |_| {
            vec![
                Migration::new("0004_function", &[&format!("{PLPGSQL_F};")]),
                Migration::new(
                    "0005_index",
                    &[r#"CREATE INDEX "by_f" ON "things" ("f"("size"));"#],
                ),
            ]
        })
        .naming(ANY_MIGRATION)
        .mentioning("by_f"),
    ];
    run_cases(&own, "index-code", cases);
}

/// What the runtime role makes in a schema a migration let it create in,
/// and the index that uses it.
struct RuntimeMade {
    name: &'static str,
    /// Run as the runtime role (the superuser's `SET ROLE`); `{o}` is the
    /// owner role.
    as_runtime: &'static [&'static str],
    index: &'static str,
    /// The index or statistic the message must name.
    names_index: &'static str,
    /// The function the message must name, when there is one.
    names_function: Option<&'static str>,
}

/// CONTRACT.md, `index-code`: "a function that is neither Postgres's own
/// nor an extension's (one any role of the Application's owns: the
/// migration role, or the runtime role where a migration let it create)".
#[test]
fn an_index_or_a_statistic_calling_a_function_the_runtime_role_made_is_refused() {
    let own = start();
    let cases = [
        RuntimeMade {
            name: "a-function",
            as_runtime: &[
                r#"CREATE FUNCTION "rt"."rt_made_fn"("x" integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT "x" $f$"#,
                r#"GRANT EXECUTE ON FUNCTION "rt"."rt_made_fn"(integer) TO {o}"#,
            ],
            index: r#"CREATE INDEX "by_rt_fn" ON "things" ("rt"."rt_made_fn"("size"));"#,
            names_index: "by_rt_fn",
            names_function: Some("rt_made_fn"),
        },
        RuntimeMade {
            name: "a-function-in-the-where",
            as_runtime: &[
                r#"CREATE FUNCTION "rt"."rt_made_fn"("x" integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT "x" $f$"#,
                r#"GRANT EXECUTE ON FUNCTION "rt"."rt_made_fn"(integer) TO {o}"#,
            ],
            index: r#"CREATE INDEX "partial_rt_fn" ON "things" ("id") WHERE "rt"."rt_made_fn"("size") > 0;"#,
            names_index: "partial_rt_fn",
            names_function: Some("rt_made_fn"),
        },
        RuntimeMade {
            name: "an-operator",
            as_runtime: &[
                r#"CREATE FUNCTION "rt"."rt_plus"(integer, integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT $1 + $2 $f$"#,
                r#"GRANT EXECUTE ON FUNCTION "rt"."rt_plus"(integer, integer) TO {o}"#,
                r#"CREATE OPERATOR "rt".@+@ (LEFTARG = integer, RIGHTARG = integer, FUNCTION = "rt"."rt_plus")"#,
            ],
            index: r#"CREATE INDEX "by_rt_op" ON "things" (("size" OPERATOR("rt".@+@) 1));"#,
            names_index: "by_rt_op",
            names_function: Some("rt_plus"),
        },
        RuntimeMade {
            name: "a-domain",
            as_runtime: &[
                r#"CREATE DOMAIN "rt"."rt_domain" AS integer"#,
                r#"GRANT USAGE ON TYPE "rt"."rt_domain" TO {o}"#,
            ],
            index: r#"CREATE INDEX "by_rt_domain" ON "things" ((("size")::"rt"."rt_domain"));"#,
            names_index: "by_rt_domain",
            names_function: None,
        },
        RuntimeMade {
            name: "an-extended-statistic",
            as_runtime: &[
                r#"CREATE FUNCTION "rt"."rt_made_fn"("x" integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT "x" $f$"#,
                r#"GRANT EXECUTE ON FUNCTION "rt"."rt_made_fn"(integer) TO {o}"#,
            ],
            index: r#"CREATE STATISTICS "st_rt" ON ("rt"."rt_made_fn"("size")), ("id") FROM "things";"#,
            names_index: "st_rt",
            names_function: Some("rt_made_fn"),
        },
    ];
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let application = format!("runtime-made-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install_seeded(&own, &application);
            // A first Version lets the runtime role create in `rt`.
            let open = Migration::new(
                "0004_open",
                &[
                    r#"CREATE SCHEMA "rt";"#,
                    &format!(
                        r#"GRANT USAGE, CREATE ON SCHEMA "rt" TO {};"#,
                        ident(&db.runtime)
                    ),
                ],
            );
            let opened = own.send(&application, &base_and(std::slice::from_ref(&open)));
            assert_eq!(
                opened.status,
                201,
                "the harness's own check: expected the Version that lets the runtime role create in `rt` applied, got\n{}",
                opened.describe()
            );
            // The runtime role makes its code there.
            let sql = db.superuser();
            sql.batch(&format!("SET ROLE {}", ident(&db.runtime)));
            for statement in case.as_runtime {
                sql.batch(&statement.replace("{o}", &ident(&db.owner)));
            }
            sql.batch("RESET ROLE");
            drop(sql);
            let before = blackbox::out_of_reach::untouched(&db);
            let index = Migration::new("0005_index", &[case.index]);
            let sent = own.send(&application, &base_and(&[open, index]));
            assert_answer(&sent, INDEX_CODE, "0005_index", Some(case.names_index));
            if let Some(function) = case.names_function {
                let message = sent.json()["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                assert!(
                    message.contains(function),
                    "expected the message to name the function {function:?}, got {message:?}"
                );
            }
            let after = blackbox::out_of_reach::untouched(&db);
            assert!(
                after == before,
                "expected the own database exactly as before the refused Version; it differs:\n{}",
                blackbox::out_of_reach::differences(&before, &after)
            );
            assert_eq!(
                own.version_numbers(&application),
                vec![1, 2],
                "expected no Version recorded for the refused one"
            );
        }));
        if let Err(payload) = outcome {
            failures.push(format!(
                "--- case {:?} (Application {application}) ---\n{}",
                case.name,
                panic_text(payload.as_ref())
                    .chars()
                    .take(3000)
                    .collect::<String>()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} indexes on what the runtime role made were not refused `index-code`:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// `CREATE EXTENSION pg_trgm`: the migration role owns the extension it
/// installs and may add its own objects to it.
const TRGM: &str = r#"CREATE EXTENSION IF NOT EXISTS "pg_trgm""#;
const EVIL_FN: &str = r#"CREATE FUNCTION "evil_fn"("x" integer) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN "x"; END $f$"#;
const EVIL_FN_IN_TRGM: &str = r#"ALTER EXTENSION "pg_trgm" ADD FUNCTION "evil_fn"(integer)"#;

/// Whether the test Postgres has `pg_trgm` to install.
fn has_pg_trgm() -> bool {
    blackbox::sql::Sql::connect(&blackbox::postgres::shared().url("postgres"))
        .unwrap()
        .value("SELECT count(*)::text FROM pg_available_extensions WHERE name = 'pg_trgm'")
        .as_deref()
        == Some("1")
}

/// CONTRACT.md, `index-code`: "A function of the Application's that a
/// migration adds to an extension it installed (`ALTER EXTENSION ... ADD
/// FUNCTION`) is still the Application's, and refused".
#[test]
fn code_of_the_migrations_added_to_an_extension_they_installed_is_still_refused() {
    if !has_pg_trgm() {
        eprintln!("SKIPPED: the test Postgres has no pg_trgm");
        return;
    }
    let own = start();
    let cases = vec![
        Case::statements("index-on-a-function-added-to-pg-trgm", INDEX_CODE, statements(&[
            TRGM,
            EVIL_FN,
            EVIL_FN_IN_TRGM,
            r#"CREATE INDEX "by_member_fn" ON "things" (("evil_fn"("id")))"#,
        ]))
        .mentioning("by_member_fn"),
        Case::statements("where-on-a-function-added-to-pg-trgm", INDEX_CODE, statements(&[
            TRGM,
            EVIL_FN,
            EVIL_FN_IN_TRGM,
            r#"CREATE INDEX "partial_member_fn" ON "things" ("id") WHERE "evil_fn"("size") > 0"#,
        ]))
        .mentioning("partial_member_fn"),
        Case::statements("through-an-operator-added-to-pg-trgm", INDEX_CODE, statements(&[
            TRGM,
            r#"CREATE FUNCTION "evil_plus"(integer, integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT $1 + $2 $f$"#,
            r#"CREATE OPERATOR @+@ (LEFTARG = integer, RIGHTARG = integer, FUNCTION = "evil_plus")"#,
            r#"ALTER EXTENSION "pg_trgm" ADD FUNCTION "evil_plus"(integer, integer)"#,
            r#"ALTER EXTENSION "pg_trgm" ADD OPERATOR @+@ (integer, integer)"#,
            r#"CREATE INDEX "by_member_op" ON "things" (("size" @+@ 1))"#,
        ]))
        .mentioning("by_member_op"),
        Case::statements("through-a-domain-added-to-pg-trgm", INDEX_CODE, statements(&[
            TRGM,
            r#"CREATE DOMAIN "evil_domain" AS integer"#,
            r#"ALTER EXTENSION "pg_trgm" ADD DOMAIN "evil_domain""#,
            r#"CREATE INDEX "by_member_domain" ON "things" ((("size")::"evil_domain"))"#,
        ]))
        .mentioning("by_member_domain"),
        Case::statements("a-statistic-on-a-function-added-to-pg-trgm", INDEX_CODE, statements(&[
            TRGM,
            EVIL_FN,
            EVIL_FN_IN_TRGM,
            r#"CREATE STATISTICS "st_member_fn" ON ("evil_fn"("size")), ("id") FROM "things""#,
        ]))
        .mentioning("st_member_fn"),
        // The function added to the extension by one migration, the index
        // made by a later one.
        Case::migrations("function-added-then-index-in-a-later-migration", INDEX_CODE, |_| {
            vec![
                Migration::new(
                    "0004_member",
                    &[
                        &format!("{TRGM};"),
                        &format!("{EVIL_FN};"),
                        &format!("{EVIL_FN_IN_TRGM};"),
                    ],
                ),
                Migration::new(
                    "0005_index",
                    &[r#"CREATE INDEX "by_member_fn" ON "things" (("evil_fn"("id")));"#],
                ),
            ]
        })
        .naming(ANY_MIGRATION)
        .mentioning("by_member_fn"),
    ];
    run_cases(&own, "member-index-code", cases);
}

/// CONTRACT.md, `security-definer`: "a function or a procedure of the
/// migrations' marked `SECURITY DEFINER`, in any schema", one added to an
/// extension they installed among them.
#[test]
fn a_security_definer_function_added_to_an_extension_is_still_refused() {
    if !has_pg_trgm() {
        eprintln!("SKIPPED: the test Postgres has no pg_trgm");
        return;
    }
    let own = start();
    let definer = vec![
        Case::statements("definer-added-to-pg-trgm", SECURITY_DEFINER, statements(&[
            TRGM,
            r#"CREATE FUNCTION "member_powers"() RETURNS integer LANGUAGE sql SECURITY DEFINER AS $f$ SELECT 1 $f$"#,
            r#"ALTER EXTENSION "pg_trgm" ADD FUNCTION "member_powers"()"#,
        ]))
        .mentioning("member_powers"),
        Case::statements("added-to-pg-trgm-then-made-definer", SECURITY_DEFINER, statements(&[
            TRGM,
            r#"CREATE FUNCTION "member_powers"() RETURNS integer LANGUAGE sql AS $f$ SELECT 1 $f$"#,
            r#"ALTER EXTENSION "pg_trgm" ADD FUNCTION "member_powers"()"#,
            r#"ALTER FUNCTION "member_powers"() SECURITY DEFINER"#,
        ]))
        .mentioning("member_powers"),
    ];
    run_cases(&own, "member-definer", definer);
}

/// The function added to the extension in an earlier Version, the index on
/// it in a later one.
#[test]
fn an_index_on_a_function_an_earlier_version_added_to_an_extension_is_refused() {
    if !has_pg_trgm() {
        eprintln!("SKIPPED: the test Postgres has no pg_trgm");
        return;
    }
    let own = start();
    let application = "member-earlier";
    let db = install_seeded(&own, application);
    let member = Migration::new(
        "0004_member",
        &[
            &format!("{TRGM};"),
            &format!("{EVIL_FN};"),
            &format!("{EVIL_FN_IN_TRGM};"),
        ],
    );
    let first = own.send(application, &base_and(std::slice::from_ref(&member)));
    assert_eq!(
        first.status,
        201,
        "the harness's own check: expected the Version that adds a function of its own to pg_trgm applied, got\n{}",
        first.describe()
    );
    let before = blackbox::out_of_reach::untouched(&db);
    let index = Migration::new(
        "0005_index",
        &[r#"CREATE INDEX "by_member_fn" ON "things" (("evil_fn"("id")));"#],
    );
    let sent = own.send(application, &base_and(&[member, index]));
    assert_answer(&sent, INDEX_CODE, "0005_index", Some("by_member_fn"));
    let message = sent.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("evil_fn"),
        "expected the message to name the function \"evil_fn\", got {message:?}"
    );
    let after = blackbox::out_of_reach::untouched(&db);
    assert!(
        after == before,
        "expected the own database exactly as before the refused Version; it differs:\n{}",
        blackbox::out_of_reach::differences(&before, &after)
    );
    assert_eq!(own.version_numbers(application), vec![1, 2]);
}

const TYPE_CODE: Answers = &[refused("type-code")];
/// A function of the migrations', fit to be a range's `subtype_diff`.
const SPAN_DIFF: &str = r#"CREATE FUNCTION "span_diff_fn"("a" integer, "b" integer) RETURNS float8 LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN "a" - "b"; END $f$"#;
/// A range type over it.
const EVIL_SPAN: &str = r#"CREATE TYPE "evil_span" AS RANGE (subtype = integer, subtype_diff = "span_diff_fn", multirange_type_name = "evil_spans")"#;
/// What the message must name: the type and the function.
const NAMES_TYPE_AND_FUNCTION: &[&str] = &["evil_span", "span_diff_fn"];

/// CONTRACT.md, `type-code`: "a range type whose `subtype_diff` is a
/// function a role of the Application's owns, whether or not a table has a
/// column of it yet ... The function may come from an earlier migration or
/// an earlier Version than the type."
#[test]
fn a_range_type_whose_subtype_diff_is_the_application_s_is_refused() {
    let own = start();
    let mut cases = vec![
        Case::statements("type-and-function-no-table", TYPE_CODE, statements(&[
            SPAN_DIFF, EVIL_SPAN,
        ]))
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        Case::statements("a-column-of-the-type", TYPE_CODE, statements(&[
            SPAN_DIFF,
            EVIL_SPAN,
            r#"CREATE TABLE "spans" ("id" integer PRIMARY KEY NOT NULL, "s" "evil_span")"#,
        ]))
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        Case::statements("a-multirange-column", TYPE_CODE, statements(&[
            SPAN_DIFF,
            EVIL_SPAN,
            r#"CREATE TABLE "spans" ("id" integer PRIMARY KEY NOT NULL, "s" "evil_spans")"#,
        ]))
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        Case::statements("a-domain-over-the-range", TYPE_CODE, statements(&[
            SPAN_DIFF,
            EVIL_SPAN,
            r#"CREATE DOMAIN "span_domain" AS "evil_span""#,
            r#"CREATE TABLE "spans" ("id" integer PRIMARY KEY NOT NULL, "s" "span_domain")"#,
        ]))
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        Case::statements("a-column-added-to-a-table-of-the-first-version", TYPE_CODE, statements(&[
            SPAN_DIFF,
            EVIL_SPAN,
            r#"ALTER TABLE "things" ADD COLUMN "s" "evil_span""#,
        ]))
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        // The function in an earlier migration of the same Version.
        Case::migrations("function-in-an-earlier-migration", TYPE_CODE, |_| {
            vec![
                Migration::new("0004_diff", &[&format!("{SPAN_DIFF};")]),
                Migration::new(
                    "0005_span",
                    &[
                        &format!("{EVIL_SPAN};"),
                        r#"CREATE TABLE "spans" ("id" integer PRIMARY KEY NOT NULL, "s" "evil_span");"#,
                    ],
                ),
            ]
        })
        .naming(ANY_MIGRATION)
        .mentioning_also(NAMES_TYPE_AND_FUNCTION),
    ];
    if has_pg_trgm() {
        // The function added to an extension the migration installed.
        cases.push(
            Case::statements(
                "function-added-to-pg-trgm",
                TYPE_CODE,
                statements(&[
                    TRGM,
                    SPAN_DIFF,
                    r#"ALTER EXTENSION "pg_trgm" ADD FUNCTION "span_diff_fn"(integer, integer)"#,
                    EVIL_SPAN,
                ]),
            )
            .mentioning_also(NAMES_TYPE_AND_FUNCTION),
        );
    } else {
        eprintln!("SKIPPED function-added-to-pg-trgm: the test Postgres has no pg_trgm");
    }
    run_cases(&own, "type-code", cases);
}

/// The function in an earlier Version (accepted then), the type in the
/// next; after the refusal the Application still works and a later Version
/// without the type is accepted.
#[test]
fn a_range_type_over_a_function_an_earlier_version_made_is_refused_and_the_application_goes_on() {
    let own = start();
    let application = "type-code-earlier";
    let db = install_seeded(&own, application);
    let diff = Migration::new("0004_diff", &[&format!("{SPAN_DIFF};")]);
    let first = own.send(application, &base_and(std::slice::from_ref(&diff)));
    assert_eq!(
        first.status,
        201,
        "expected a Version with the function alone accepted (CONTRACT.md refuses the type, not the function), got\n{}",
        first.describe()
    );
    let before = blackbox::out_of_reach::untouched(&db);
    let span = Migration::new(
        "0005_span",
        &[
            &format!("{EVIL_SPAN};"),
            r#"CREATE TABLE "spans" ("id" integer PRIMARY KEY NOT NULL, "s" "evil_span");"#,
        ],
    );
    let sent = own.send(application, &base_and(&[diff.clone(), span]));
    assert_answer(&sent, TYPE_CODE, "0005_span", Some("evil_span"));
    let message = sent.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("span_diff_fn"),
        "expected the message to name the function \"span_diff_fn\", got {message:?}"
    );
    let after = blackbox::out_of_reach::untouched(&db);
    assert!(
        after == before,
        "expected nothing of the refused Version kept; the own database differs:\n{}",
        blackbox::out_of_reach::differences(&before, &after)
    );
    assert_eq!(own.version_numbers(application), vec![1, 2]);
    assert!(
        !can_log_in(&db, &db.owner),
        "expected the owner role unable to log in once the Version was answered"
    );
    // The Application still works: a write runs and is recorded.
    let last = last_history_id(&db);
    let (_, answer) = own.one(
        &User::new(WRITER),
        application,
        &own_execute(
            "user",
            "insert into things (id, title) values (1201, 'after type-code')",
        ),
    );
    assert!(
        answer["ok"].is_object(),
        "expected a write after the refusal to run, got {answer}"
    );
    assert!(
        history_after(&db, last)
            .iter()
            .any(|r| r["table_name"] == "things" && r["new"]["id"] == 1201),
        "expected the write after the refusal recorded"
    );
    // A later Version without the type is accepted.
    let later = Migration::new(
        "0005_plain",
        &[r#"CREATE TABLE "plain_after" ("id" integer PRIMARY KEY NOT NULL);"#],
    );
    let sent = own.send(application, &base_and(&[diff, later]));
    assert_eq!(
        sent.status,
        201,
        "expected a later Version without the type accepted, got\n{}",
        sent.describe()
    );
}

/// CONTRACT.md, `type-code`: "A range type with no `subtype_diff`, or with
/// one that belongs to a superuser, is allowed, and so are Postgres's own
/// range types."
#[test]
fn range_types_with_no_subtype_diff_or_postgres_s_own_are_allowed() {
    let own = start();
    let mut no_diff = allowed(
        "range-with-no-subtype-diff",
        &[
            r#"CREATE TYPE "plain_span" AS RANGE (subtype = integer);"#,
            r#"CREATE TABLE "plain_spans" ("id" integer PRIMARY KEY NOT NULL, "s" "plain_span" NOT NULL);"#,
        ],
        "insert into plain_spans (id, s) values (1, '[1,5)')",
        "plain_spans",
    );
    no_diff.read = Some("select s::text as s from plain_spans where id = 1");
    let mut float8mi = allowed(
        "range-with-float8mi",
        &[
            r#"CREATE TYPE "float_span" AS RANGE (subtype = float8, subtype_diff = float8mi);"#,
            r#"CREATE TABLE "float_spans" ("id" integer PRIMARY KEY NOT NULL, "s" "float_span" NOT NULL);"#,
            r#"CREATE INDEX "float_spans_s" ON "float_spans" USING gist ("s");"#,
        ],
        "insert into float_spans (id, s) values (1, '[1.5,2.5)')",
        "float_spans",
    );
    float8mi.read = Some("select s::text as s from float_spans where s @> 2.0::float8");
    let mut built_in = allowed(
        "built-in-ranges-with-gist-indexes",
        &[
            r#"CREATE TABLE "bookings" ("id" integer PRIMARY KEY NOT NULL, "during" tstzrange NOT NULL, "seats" int4range NOT NULL);"#,
            r#"CREATE INDEX "bookings_during" ON "bookings" USING gist ("during");"#,
            r#"CREATE INDEX "bookings_seats" ON "bookings" USING gist ("seats");"#,
        ],
        "insert into bookings (id, during, seats) values (1, '[2026-01-01 00:00+00,2026-01-02 00:00+00)', '[1,3)')",
        "bookings",
    );
    built_in.read = Some("select id from bookings where seats @> 2");
    run_allowed(&own, "type-allowed", vec![no_diff, float8mi, built_in]);
}

#[test]
fn copy_to_or_from_the_client_is_refused_before_anything_is_sent() {
    let own = start();
    let cases = vec![
        Case::statements(
            "copy-from-stdin",
            COPY_STREAM,
            statements(&[r#"COPY "keyless" ("a", "b") FROM STDIN"#]),
        ),
        Case::statements(
            "copy-to-stdout",
            COPY_STREAM,
            statements(&[r#"COPY "things" TO STDOUT"#]),
        ),
        Case::statements(
            "copy-a-query-to-stdout",
            COPY_STREAM,
            statements(&[r#"COPY (SELECT * FROM "things") TO STDOUT WITH (FORMAT csv)"#]),
        ),
        Case::statements(
            "lower-case-with-options",
            COPY_STREAM,
            statements(&[r#"copy keyless (a, b) from stdin with (format csv, header true)"#]),
        ),
        Case::statements(
            "after-an-ordinary-statement",
            COPY_STREAM,
            statements(&[
                r#"INSERT INTO "keyless" ("a", "b") VALUES ('before the copy', 1)"#,
                r#"COPY "keyless" FROM STDIN"#,
            ]),
        ),
        Case::text(
            "with-its-rows",
            COPY_STREAM_OR_UNPARSEABLE,
            "COPY \"keyless\" (\"a\", \"b\") FROM STDIN;\ncopied\t1\n\\.\n",
        ),
        // In a later migration: nothing of the Version is sent.
        Case::migrations("in-a-later-migration", COPY_STREAM, |_| {
            vec![
                Migration::new(
                    "0004_fine",
                    &[r#"CREATE TABLE "fresh" ("id" integer PRIMARY KEY NOT NULL);"#],
                ),
                Migration::new("0005_copy", &[r#"COPY "fresh" TO STDOUT;"#]),
            ]
        })
        .naming("0005_copy"),
        Case::statements(
            "copy-from-a-server-file",
            POSTGRES_REFUSES,
            statements(&[r#"COPY "keyless" FROM '/etc/hostname'"#]),
        ),
        Case::statements(
            "copy-to-stdout-inside-a-do-block",
            POSTGRES_REFUSES,
            statements(&[r#"DO $f$ BEGIN EXECUTE 'COPY things TO STDOUT'; END $f$"#]),
        ),
    ];
    run_cases(&own, "copy", cases);
}

/// A Version the contract allows: its pending migrations, then a write
/// through the Application (as the User) whose row must be recorded in
/// `table`.
struct Allowed {
    name: &'static str,
    pending: Vec<Migration>,
    write: &'static str,
    table: &'static str,
    /// The extensions it needs; skipped when the test Postgres lacks one.
    needs: &'static [&'static str],
    /// A `query` that must read back at least one row after the write.
    read: Option<&'static str>,
}

fn allowed(
    name: &'static str,
    statements: &[&str],
    write: &'static str,
    table: &'static str,
) -> Allowed {
    Allowed {
        name,
        pending: vec![Migration::new(&format!("0004_{name}"), statements)],
        write,
        table,
        needs: &[],
        read: None,
    }
}

fn run_allowed(own: &Own, prefix: &str, cases: Vec<Allowed>) {
    let mut failures = Vec::new();
    let total = cases.len();
    for (i, case) in cases.into_iter().enumerate() {
        let application = format!("{prefix}-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install_seeded(own, &application);
            let missing: Vec<&str> = case
                .needs
                .iter()
                .copied()
                .filter(|e| {
                    db.superuser()
                        .value(&format!(
                            "SELECT count(*)::text FROM pg_available_extensions WHERE name = '{e}'"
                        ))
                        .as_deref()
                        != Some("1")
                })
                .collect();
            if !missing.is_empty() {
                eprintln!(
                    "SKIPPED {:?}: the test Postgres has no extension {missing:?}",
                    case.name
                );
                return;
            }
            let sent = own.send(&application, &base_and(&case.pending));
            assert_eq!(
                sent.status,
                201,
                "expected the Version applied (CONTRACT.md allows it), got\n{}",
                sent.describe()
            );
            let last = last_history_id(&db);
            let (_, answer) = own.one(
                &User::new(WRITER),
                &application,
                &own_execute("user", case.write),
            );
            assert!(
                answer["ok"].is_object(),
                "expected the write {:?} to run, got {answer}",
                case.write
            );
            let rows = history_after(&db, last);
            assert!(
                rows.iter().any(|r| r["table_name"] == case.table),
                "expected the write to {} recorded (the table captured), got:\n{}",
                case.table,
                lines(&rows)
            );
            if let Some(read) = case.read {
                let (_, answer) =
                    own.one(&User::new(WRITER), &application, &own_query("user", read));
                assert!(
                    answer["ok"]["rows"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty()),
                    "expected {read:?} to read the row back, got {answer}"
                );
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
        "{} of {total} Versions CONTRACT.md allows were not applied with their tables captured:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn invoker_functions_indexes_on_postgres_s_own_functions_and_trusted_extensions_are_allowed() {
    let own = start();
    let mut cases = vec![
        allowed(
            "an-invoker-function",
            &[
                r#"CREATE FUNCTION "double"("x" integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $f$ SELECT "x" * 2 $f$;"#,
                r#"CREATE FUNCTION "touch"() RETURNS integer LANGUAGE plpgsql SECURITY INVOKER AS $f$ BEGIN RETURN 1; END $f$;"#,
            ],
            "insert into things (id, title) values (301, 'after an invoker function')",
            "things",
        ),
        allowed(
            "lower-of-a-column",
            &[r#"CREATE INDEX "things_lower_title" ON "things" (lower("title"));"#],
            "insert into things (id, title) values (302, 'after lower')",
            "things",
        ),
        allowed(
            "a-sum-of-columns",
            &[r#"CREATE INDEX "things_sum" ON "things" (("id" + "size"));"#],
            "insert into things (id, title, size) values (303, 'after a sum', 1)",
            "things",
        ),
        allowed(
            "a-partial-index",
            &[
                r#"ALTER TABLE "things" ADD COLUMN "deleted_at" timestamp with time zone;"#,
                r#"CREATE INDEX "things_live" ON "things" ("title") WHERE "deleted_at" IS NULL;"#,
            ],
            "insert into things (id, title) values (304, 'after a partial index')",
            "things",
        ),
        allowed(
            "a-statistic-on-columns",
            &[r#"CREATE STATISTICS "things_st" ON "id", "size" FROM "things";"#],
            "insert into things (id, title) values (305, 'after a statistic')",
            "things",
        ),
    ];
    let mut trgm = allowed(
        "pg-trgm-and-a-trigram-index",
        &[
            r#"CREATE EXTENSION IF NOT EXISTS "pg_trgm";"#,
            r#"CREATE TABLE "docs" ("id" integer PRIMARY KEY NOT NULL, "name" text NOT NULL);"#,
            r#"CREATE INDEX "docs_name_trgm" ON "docs" USING gin ("name" gin_trgm_ops);"#,
        ],
        "insert into docs (id, name) values (1, 'a trigram name')",
        "docs",
    );
    trgm.needs = &["pg_trgm"];
    cases.push(trgm);
    let mut citext = allowed(
        "citext-and-indexes-on-it",
        &[
            r#"CREATE EXTENSION IF NOT EXISTS "citext";"#,
            r#"CREATE TABLE "people" ("id" integer PRIMARY KEY NOT NULL, "email" citext NOT NULL);"#,
            r#"CREATE INDEX "people_email" ON "people" ("email");"#,
            r#"CREATE INDEX "people_email_lower" ON "people" (lower("email"));"#,
        ],
        "insert into people (id, email) values (1, 'Someone@Example.org')",
        "people",
    );
    citext.needs = &["citext"];
    cases.push(citext);
    let mut unaccent = allowed(
        "unaccent",
        &[
            r#"CREATE EXTENSION IF NOT EXISTS "unaccent";"#,
            r#"CREATE TABLE "words" ("id" integer PRIMARY KEY NOT NULL, "w" text NOT NULL);"#,
        ],
        "insert into words (id, w) values (1, 'café')",
        "words",
    );
    unaccent.needs = &["unaccent"];
    cases.push(unaccent);
    run_allowed(&own, "allowed", cases);
}
