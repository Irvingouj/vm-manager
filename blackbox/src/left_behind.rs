//! What the tests of "What a migration leaves behind" share (CONTRACT.md,
//! "The platform's part of an own database is out of a migration's reach:
//! What a migration leaves behind"; research part F):
//!
//! - [`left_behind`]: a migration that leaves code which Postgres runs later
//!   with the migration role's rights, though no migration is running: a
//!   `BEFORE DELETE` row trigger on the child of an `ON DELETE CASCADE`
//!   foreign key (`pa`/`ca`), a `CHECK` on the child of an `ON UPDATE
//!   CASCADE` one (`pb`/`cb`), a stored generated column there (`pc`/`cc`),
//!   a default under `ON DELETE SET DEFAULT` (`pd`/`cd`, which obeys the
//!   rows of `orders`). Each does what the row that sets it off says
//!   (`action`: statements separated by `;;`), so that the same Application
//!   also works honestly (an empty action does nothing);
//! - [`install`]: an Application with `fixtures/transactions.js` as the
//!   bundle of every Version (so that a test drives `execute`, `query`,
//!   `tx.begin` and `tx.commit` on the own database), [`base`] and
//!   [`left_behind`] as its first Version;
//! - [`steps`]: a request of these steps, and the step builders;
//! - [`capture_catalog`]: the catalog rows of the capture triggers (object
//!   id, row version, enabling), which a switch-off put back changes.

use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{Response, User};
use crate::out_of_reach::{WRITER, base};
use crate::own_database::{Migration, Own, OwnDatabase, version_body_with_migrations};
use crate::vm::fixture_bundle;

/// The fixture every Version of these Applications carries.
pub const BUNDLE: &str = "transactions.js";

/// `0004_left_behind`: see the module's header. Also:
///
/// - `obey(action)`: runs each statement of `action` (separated by `;;`)
///   with `EXECUTE`, as whoever calls it;
/// - `each_capture(table, template)`: runs `format(template, name)` for the
///   name of each capture trigger of `table` (a trigger whose function is
///   in `vm`), found in the catalog;
/// - `try_then_undo(action)`: obeys `action`, then raises and catches, so
///   that nothing of it is kept;
/// - `late_fn()`: a trigger function that switches the capture of
///   `counters` off, inserts a row and switches it on again;
/// - `cascade_now()`: deletes `pa` row 9, whose child `ca` row 9 switches
///   the capture of `parents` off, changes a row and switches it on again;
/// - `mv`: a materialized view over `parents`, `MAINTAIN` granted to
///   `PUBLIC` on Postgres 17 and later ([`maintain_grant`]);
/// - schema `scratch`, where `PUBLIC` (the runtime role among it) may create.
pub fn left_behind() -> Migration {
    Migration::new(
        "0004_left_behind",
        &[
            r#"CREATE FUNCTION "obey"("action" text) RETURNS boolean LANGUAGE plpgsql AS $f$
DECLARE s text;
BEGIN
  IF "action" IS NULL OR "action" = '' THEN RETURN true; END IF;
  FOREACH s IN ARRAY string_to_array("action", ';;') LOOP
    EXECUTE s;
  END LOOP;
  RETURN true;
END $f$;"#,
            r#"CREATE FUNCTION "each_capture"("tbl" text, "template" text) RETURNS integer LANGUAGE plpgsql AS $f$
DECLARE t record; n integer := 0;
BEGIN
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = "tbl"::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace ORDER BY tg.tgname LOOP
    EXECUTE format("template", t.tgname);
    n := n + 1;
  END LOOP;
  RETURN n;
END $f$;"#,
            r#"CREATE FUNCTION "try_then_undo"("action" text) RETURNS boolean LANGUAGE plpgsql AS $f$
BEGIN
  BEGIN
    PERFORM public.obey("action");
    RAISE EXCEPTION 'undone on purpose';
  EXCEPTION WHEN others THEN
    NULL;
  END;
  RETURN true;
END $f$;"#,
            r#"CREATE TABLE "orders" ("id" integer PRIMARY KEY NOT NULL, "action" text NOT NULL);"#,
            r#"CREATE FUNCTION "obey_orders"() RETURNS integer LANGUAGE plpgsql AS $f$
DECLARE o record;
BEGIN
  FOR o IN SELECT * FROM public.orders ORDER BY id LOOP
    PERFORM public.obey(o.action);
  END LOOP;
  RETURN NULL;
END $f$;"#,
            r#"CREATE TABLE "pa" ("id" integer PRIMARY KEY NOT NULL);"#,
            r#"CREATE TABLE "ca" ("id" integer PRIMARY KEY NOT NULL, "p" integer REFERENCES "pa" ("id") ON DELETE CASCADE, "action" text);"#,
            r#"CREATE FUNCTION "ca_before"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN PERFORM public.obey(OLD."action"); RETURN OLD; END $f$;"#,
            r#"CREATE TRIGGER "ca_before" BEFORE DELETE ON "ca" FOR EACH ROW EXECUTE FUNCTION "ca_before"();"#,
            r#"CREATE TABLE "pb" ("id" integer PRIMARY KEY NOT NULL);"#,
            r#"CREATE FUNCTION "obey_over"("action" text, "p" integer) RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN IF "p" >= 1000 THEN PERFORM public.obey("action"); END IF; RETURN true; END $f$;"#,
            r#"CREATE TABLE "cb" ("id" integer PRIMARY KEY NOT NULL, "p" integer REFERENCES "pb" ("id") ON UPDATE CASCADE, "action" text, CONSTRAINT "cb_obeys" CHECK ("obey_over"("action", "p")));"#,
            r#"CREATE FUNCTION "obey_text"("action" text, "p" integer) RETURNS text LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN IF "p" >= 1000 THEN PERFORM public.obey("action"); END IF; RETURN 'generated'; END $f$;"#,
            r#"CREATE TABLE "pc" ("id" integer PRIMARY KEY NOT NULL);"#,
            r#"CREATE TABLE "cc" ("id" integer PRIMARY KEY NOT NULL, "p" integer REFERENCES "pc" ("id") ON UPDATE CASCADE, "action" text, "g" text GENERATED ALWAYS AS ("obey_text"("action", "p")) STORED);"#,
            r#"CREATE TABLE "pd" ("id" integer PRIMARY KEY NOT NULL);"#,
            r#"CREATE TABLE "cd" ("id" integer PRIMARY KEY NOT NULL, "p" integer DEFAULT "obey_orders"() REFERENCES "pd" ("id") ON DELETE SET DEFAULT);"#,
            r#"CREATE FUNCTION "late_fn"() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
  PERFORM public.each_capture('public.counters', 'ALTER TABLE public.counters DISABLE TRIGGER %I');
  INSERT INTO public.counters ("name", "n") VALUES ('unseen at the commit', 1) ON CONFLICT DO NOTHING;
  PERFORM public.each_capture('public.counters', 'ALTER TABLE public.counters ENABLE TRIGGER %I');
  RETURN NULL;
END $f$;"#,
            r#"CREATE FUNCTION "cascade_now"() RETURNS integer LANGUAGE plpgsql AS $f$ BEGIN DELETE FROM public.pa WHERE "id" = 9; RETURN 1; END $f$;"#,
            r#"CREATE MATERIALIZED VIEW "mv" AS SELECT "id", "name" FROM "parents";"#,
            r#"CREATE SCHEMA "scratch";"#,
            r#"GRANT USAGE, CREATE ON SCHEMA "scratch" TO PUBLIC;"#,
            r#"INSERT INTO "pa" SELECT g FROM generate_series(1, 20) g;"#,
            r#"INSERT INTO "pb" SELECT g FROM generate_series(1, 20) g;"#,
            r#"INSERT INTO "pc" SELECT g FROM generate_series(1, 20) g;"#,
            r#"INSERT INTO "pd" SELECT g FROM generate_series(1, 20) g;"#,
            "INSERT INTO \"ca\" VALUES (9, 9, 'SELECT public.each_capture(''public.parents'', ''ALTER TABLE public.parents DISABLE TRIGGER %I'');;UPDATE public.parents SET name = ''changed by a held cursor'' WHERE id = 1;;SELECT public.each_capture(''public.parents'', ''ALTER TABLE public.parents ENABLE TRIGGER %I'')');",
        ],
    )
}

/// `0005_maintain`: `GRANT MAINTAIN ON mv TO PUBLIC` (Postgres 17 and
/// later; the privilege does not exist on 16).
pub fn maintain_grant() -> Migration {
    Migration::new("0005_maintain", &[r#"GRANT MAINTAIN ON "mv" TO PUBLIC;"#])
}

/// The first Version's migrations: [`base`], [`left_behind`], and on
/// Postgres 17 and later [`maintain_grant`].
pub fn first_version() -> Vec<Migration> {
    let mut list = base();
    list.push(left_behind());
    if crate::out_of_reach::server_version_num() >= 170_000 {
        list.push(maintain_grant());
    }
    list
}

/// Sends a Version of `application` with [`BUNDLE`] and these migrations.
pub fn send(own: &Own, application: &str, migrations: &[Migration]) -> Response {
    own.send_body(
        application,
        &version_body_with_migrations(&fixture_bundle(BUNDLE), &[], migrations),
    )
}

/// Creates `application` and sends [`first_version`], failing unless both
/// are `201`; returns its own database.
pub fn install(own: &Own, application: &str) -> OwnDatabase {
    own.create(application);
    let sent = send(own, application, &first_version());
    if sent.status != 201 {
        own.vm.fail(&format!(
            "the harness's own install of {application:?}: expected `201`, got\n{}",
            sent.describe()
        ));
    }
    own.own_database(application)
}

/// How long a request of these tests may take.
pub const STEPS_TIMEOUT: Duration = Duration::from_secs(40);

/// Runs these steps (see `fixtures/transactions.js`) in `application` as
/// [`WRITER`]; fails unless `200`. Returns the request's answer and each
/// step's answer.
pub fn steps(own: &Own, application: &str, steps: &[Value]) -> (Response, Vec<Value>) {
    let response = own
        .vm
        .request(application, "/")
        .as_user(&User::new(WRITER))
        .method("POST")
        .json(&json!({ "steps": steps }))
        .timeout(STEPS_TIMEOUT)
        .send();
    if response.status != 200 {
        own.vm.fail(&format!(
            "expected `200` from {application:?}, got\n{}",
            response.describe()
        ));
    }
    let answers = response.json()["answers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (response, answers)
}

/// `operation` (`execute` or `query`) on the own database as the User,
/// outside any transaction, with these parameters.
pub fn outside(operation: &str, sql: &str, params: &[&str]) -> Value {
    crate::transactions::step(
        operation,
        json!({ "own": true, "as": "user", "sql": sql, "params": params }),
    )
}

/// `execute` in the transaction kept under `name`, with these parameters.
pub fn inside(name: &str, sql: &str, params: &[&str]) -> Value {
    crate::transactions::step(
        "execute",
        json!({ "tx": crate::transactions::tx(name), "sql": sql, "params": params }),
    )
}

/// `tx.begin` on the own database as the User at `write`, kept under `name`,
/// with this isolation when given.
pub fn begin_write(name: &str, isolation: Option<&str>) -> Value {
    let mut args = json!({ "own": true, "as": "user", "access": "write" });
    if let Some(level) = isolation {
        args["isolation"] = json!(level);
    }
    crate::transactions::begin_with(args, name)
}

/// The catalog rows of every capture trigger: table, name, object id, row
/// version (`xmin`) and enabling. A trigger disabled and enabled again is
/// the same trigger in a new row version.
pub fn capture_catalog(db: &OwnDatabase) -> Vec<String> {
    db.column(
        "SELECT t.tgrelid::regclass::text || '.' || t.tgname || ' oid=' || t.oid::text
                || ' xmin=' || t.xmin::text || ' enabled=' || t.tgenabled::text
           FROM pg_trigger t JOIN pg_proc p ON p.oid = t.tgfoid
          WHERE NOT t.tgisinternal AND p.pronamespace = 'vm'::regnamespace
          ORDER BY 1",
    )
}

/// Fails unless `answer` is `{ error: { kind: "database", code: "VM004",
/// message } }` with a message that speaks of migrations.
pub fn assert_vm004(answer: &Value, context: &str) {
    assert!(
        answer["error"]["kind"] == "database" && answer["error"]["code"] == "VM004",
        "{context}: expected {{ error: {{ kind: \"database\", code: \"VM004\" }} }}, got {answer}"
    );
    let message = answer["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.to_lowercase().contains("migration"),
        "{context}: expected the message to say that an Application's structure changes in migrations only, got {message:?}"
    );
}
