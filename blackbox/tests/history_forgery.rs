//! No role of an own database can forge an entry of the Change history
//! (CONTRACT.md, "The Change history in the platform database: What the
//! roles of an own database can and cannot do": "Neither role can make an
//! entry of the history other than by changing a row. Whatever a migration
//! or the runtime role calls in `vm`, with whatever arguments (the
//! functions the platform itself calls to record a migration's changes of
//! structure among them), `change_history` afterwards holds for that
//! Application only the changes of rows that were made, under the identity
//! they were made with, and the `structure` entries of changes of
//! structure that were made, with the migration that made them. A Version
//! whose migration tries is refused or fails, or applies with nothing
//! forged").
//!
//! The forger does not know the platform's names. `forge_all()`, a function
//! the first Version makes, finds every function and procedure of schema
//! `vm` in the catalog when it runs and calls each many times, each call in
//! a subtransaction of its own so that one refusal stops nothing: every
//! text argument by turns a schema, a table (`things`, `parents`, one that
//! does not exist), a kind, an actor, another User (`mallory-forged`),
//! another migration (`0099_forged`), another Application, a made-up
//! `structure` object or a whole made-up change; every `jsonb` or `json`
//! argument a made-up `structure` object or change; numbers a table's oid
//! or a made-up key; and once all nulls. It then tries `vm.outbox` itself.
//! It is run:
//!
//! - by the runtime role through the program, as the User and as the
//!   Application, with `execute` and `query`;
//! - by the runtime role in a session of the harness's own (`SET ROLE`),
//!   committed;
//! - by a migration of a later Version, beside a row it changes and a
//!   column it drops.
//!
//! Beside it, the same calls with names the test reads from the catalog
//! now (and the guesses of `identity_hostile.rs`) are sent through
//! `execute` and `query` as both actors, run in the runtime role's session,
//! and made the statements of one more Version (which may be refused, fail
//! or apply).
//!
//! Afterwards, once `vm.outbox` is empty, `change_history` holds for the
//! Application exactly the changes the test made: the rows it wrote, each
//! under the User, actor and request that wrote it; the migration's update
//! and the `structure` entry of its drop, with that migration's name. The
//! whole set is compared, not a count.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use serde_json::{Value, json};

use blackbox::history::{
    APP, VmFunction, guessed_calls, history, history_standard, session_as, vm_functions,
    wait_shipped,
};
use blackbox::own_database::{Migration, Own, OwnDatabase, literal, own_execute};
use blackbox::scenario::op;
use blackbox::vm::INSTALLER;
use blackbox::{Response, User};

/// The migration of the first Version that makes `forge_all()`.
const FORGER: &str = "0003_forger";

/// The later Version's migration that changes a row, drops a column and
/// runs `forge_all()`.
const FORGING: &str = "0004_forging";

/// The Version whose statements are the calls themselves.
const PLAIN: &str = "0005_plain_calls";

/// The texts a forger feeds to text arguments, by turns.
const TEXTS: [&str; 15] = [
    "public",
    "things",
    "parents",
    "forged_table",
    "structure",
    "insert",
    "migration",
    "user",
    "mallory-forged",
    "0099_forged",
    "twin",
    r#"{"change": "table-dropped"}"#,
    r#"{"change": "column-dropped", "column": "title"}"#,
    r#"{"kind": "structure", "schema_name": "public", "table_name": "parents", "user_id": "mallory-forged", "actor": "migration", "migration": "0099_forged", "structure": {"change": "table-replaced"}}"#,
    r#"{"kind": "insert", "schema_name": "public", "table_name": "things", "key": {"id": 4242}, "new": {"id": 4242, "title": "forged"}, "user_id": "mallory-forged", "actor": "user", "request": "forged-request"}"#,
];

/// The documents a forger feeds to `jsonb` and `json` arguments, by turns.
const DOCS: [&str; 6] = [
    r#"{"change": "table-dropped"}"#,
    r#"{"change": "column-dropped", "column": "title"}"#,
    r#"{"change": "column-type", "column": "title", "from": "text", "to": "integer"}"#,
    r#"{"change": "enum-value-renamed", "type": "public.mood", "from": "a", "to": "b", "columns": ["title"]}"#,
    r#"{"kind": "structure", "schema_name": "public", "table_name": "parents", "user_id": "mallory-forged", "actor": "migration", "migration": "0099_forged", "structure": {"change": "table-replaced"}}"#,
    r#"{"kind": "insert", "schema_name": "public", "table_name": "things", "key": {"id": 4242}, "new": {"id": 4242, "title": "forged"}, "user_id": "mallory-forged", "actor": "user", "request": "forged-request"}"#,
];

/// A Postgres array literal of `items`, as SQL text.
fn sql_array(items: &[&str]) -> String {
    format!(
        "ARRAY[{}]::text[]",
        items
            .iter()
            .map(|t| literal(t))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `0003_forger`: `forge_all()`, which calls everything in `vm` it finds
/// with the forger's arguments (module doc) and returns how many calls
/// Postgres let through.
fn forger() -> Migration {
    let body = format!(
        r#"CREATE FUNCTION "forge_all"() RETURNS integer LANGUAGE plpgsql AS $f$
DECLARE
  f record;
  t oid;
  v text;
  args text[];
  n integer := 0;
  texts text[] := {texts};
  docs text[] := {docs};
BEGIN
  FOR f IN SELECT p.proname::text AS name, p.prokind::text AS kind,
                  ARRAY(SELECT u.t FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY AS u(t, o) ORDER BY u.o) AS types
             FROM pg_proc p JOIN pg_namespace ns ON ns.oid = p.pronamespace
            WHERE ns.nspname = 'vm' AND p.prokind IN ('f', 'p')
  LOOP
    FOR s IN 0..array_length(texts, 1) LOOP
      args := ARRAY[]::text[];
      FOR i IN 1..coalesce(array_length(f.types, 1), 0) LOOP
        t := f.types[i];
        IF s = array_length(texts, 1) THEN
          v := 'NULL::' || format_type(t, NULL);
        ELSIF t IN ('text'::regtype::oid, 'character varying'::regtype::oid, 'name'::regtype::oid, 'bpchar'::regtype::oid) THEN
          v := quote_literal(texts[1 + (s + i - 1) % array_length(texts, 1)]) || '::' || format_type(t, NULL);
        ELSIF t IN ('jsonb'::regtype::oid, 'json'::regtype::oid) THEN
          v := quote_literal(docs[1 + (s + i - 1) % array_length(docs, 1)]) || '::' || format_type(t, NULL);
        ELSIF t = 'text[]'::regtype::oid THEN
          v := quote_literal(ARRAY[texts[1 + s % array_length(texts, 1)], 'title']::text) || '::text[]';
        ELSIF t IN ('integer'::regtype::oid, 'bigint'::regtype::oid, 'oid'::regtype::oid) THEN
          IF s % 2 = 0 THEN
            v := '''public.parents''::regclass::oid::bigint::' || format_type(t, NULL);
          ELSE
            v := '4242::' || format_type(t, NULL);
          END IF;
        ELSIF t = 'smallint'::regtype::oid THEN
          v := '1::smallint';
        ELSIF t = 'regclass'::regtype::oid THEN
          IF s % 2 = 0 THEN v := '''public.parents''::regclass'; ELSE v := '''public.things''::regclass'; END IF;
        ELSIF t = 'boolean'::regtype::oid THEN
          v := 'true';
        ELSIF t IN ('timestamp with time zone'::regtype::oid, 'timestamp without time zone'::regtype::oid) THEN
          v := 'now()::' || format_type(t, NULL);
        ELSIF t = 'uuid'::regtype::oid THEN
          v := '''00000000-0000-0000-0000-000000000001''::uuid';
        ELSE
          v := 'NULL::' || format_type(t, NULL);
        END IF;
        args := args || v;
      END LOOP;
      BEGIN
        IF f.kind = 'p' THEN
          EXECUTE format('CALL vm.%I(%s)', f.name, array_to_string(args, ', '));
        ELSE
          EXECUTE format('SELECT vm.%I(%s)', f.name, array_to_string(args, ', '));
        END IF;
        n := n + 1;
      EXCEPTION WHEN OTHERS THEN NULL;
      END;
    END LOOP;
  END LOOP;
  BEGIN
    EXECUTE 'INSERT INTO vm.outbox DEFAULT VALUES';
    n := n + 1;
  EXCEPTION WHEN OTHERS THEN NULL;
  END;
  BEGIN
    EXECUTE 'INSERT INTO vm.outbox SELECT * FROM vm.outbox';
    n := n + 1;
  EXCEPTION WHEN OTHERS THEN NULL;
  END;
  RETURN n;
END
$f$;"#,
        texts = sql_array(&TEXTS),
        docs = sql_array(&DOCS),
    );
    Migration::new(FORGER, &[body.as_str()])
}

/// A value of type `ty` for argument `i` of a call in set `set`, as the
/// forger gives it (the Rust twin of `forge_all()`'s choice).
fn forged_value(ty: &str, i: usize, set: usize) -> String {
    let text = TEXTS[(set + i) % TEXTS.len()];
    let doc = DOCS[(set + i) % DOCS.len()];
    match ty {
        "text" | "character varying" | "name" | "character" => {
            format!("{}::{ty}", literal(text))
        }
        "jsonb" | "json" => format!("{}::{ty}", literal(doc)),
        "text[]" => format!("ARRAY[{}, 'title']::text[]", literal(text)),
        "integer" | "bigint" | "oid" => {
            if set.is_multiple_of(2) {
                format!("'public.parents'::regclass::oid::bigint::{ty}")
            } else {
                format!("4242::{ty}")
            }
        }
        "smallint" => "1::smallint".to_owned(),
        "regclass" => "'public.parents'::regclass".to_owned(),
        "boolean" => "true".to_owned(),
        "uuid" => "'00000000-0000-0000-0000-000000000001'::uuid".to_owned(),
        "timestamp with time zone" | "timestamp without time zone" => format!("now()::{ty}"),
        other => format!("NULL::{other}"),
    }
}

/// Every call of every function of `vm` the test sees in the catalog now:
/// the forger's sets, then the guesses of `identity_hostile.rs`.
fn static_calls(functions: &[VmFunction]) -> Vec<String> {
    let mut calls = Vec::new();
    for function in functions {
        for set in 0..TEXTS.len() {
            let arguments: Vec<String> = function
                .arguments
                .iter()
                .enumerate()
                .map(|(i, ty)| forged_value(ty, i, set))
                .collect();
            calls.push(format!(
                "SELECT vm.{}({})",
                blackbox::own_database::ident(&function.name),
                arguments.join(", ")
            ));
        }
        calls.extend(guessed_calls(function, "forged-ctx"));
    }
    calls
}

/// One entry of `change_history` as the test compares it: everything but
/// `id`, `at`, `old`, `new` and `changed`.
fn entry(row: &Value) -> Value {
    json!({
        "application": row["application"],
        "schema_name": row["schema_name"],
        "table_name": row["table_name"],
        "kind": row["kind"],
        "key": row["key"],
        "user_id": row["user_id"],
        "actor": row["actor"],
        "request": row["request"],
        "migration": row["migration"],
        "structure": row["structure"],
    })
}

/// An entry the test expects.
#[expect(
    clippy::too_many_arguments,
    reason = "one argument per column compared"
)]
fn expected(
    table: &str,
    kind: &str,
    key: &Value,
    user: &str,
    actor: &str,
    request: Option<&str>,
    migration: Option<&str>,
    structure: &Value,
) -> Value {
    json!({
        "application": APP,
        "schema_name": "public",
        "table_name": table,
        "kind": kind,
        "key": key,
        "user_id": user,
        "actor": actor,
        "request": request,
        "migration": migration,
        "structure": structure,
    })
}

/// Runs `calls` in [`APP`] as `user`, in requests of at most 40 calls.
fn send_calls(own: &Own, user: &User, calls: &[Value]) -> usize {
    let mut answered = 0;
    for chunk in calls.chunks(40) {
        let (_, answers) = own.run(user, APP, chunk);
        answered += answers.len();
    }
    answered
}

/// One write through the program; returns its request's id.
fn write(own: &Own, user: &User, actor: &str, sql: &str) -> String {
    let (response, answer): (Response, Value) = own.one(user, APP, &own_execute(actor, sql));
    assert!(
        answer["ok"].is_object(),
        "the harness's own check: `{sql}` as the {actor}, got {answer}"
    );
    response.request_id()
}

/// The whole set of `change_history` entries of `db`'s Application, once
/// `vm.outbox` is empty, sorted.
fn entries_now(db: &OwnDatabase) -> Vec<Value> {
    wait_shipped(db);
    let mut rows: Vec<Value> = history(db).iter().map(entry).collect();
    rows.sort_by_key(Value::to_string);
    rows
}

#[test]
fn neither_role_can_make_an_entry_of_the_history_other_than_by_changing_a_row() {
    let own = Own::start();
    let mut sent = history_standard();
    sent.push(forger());
    let db = own.install(APP, &sent);
    let alice = User::new("alice");
    let bob = User::new("bob");
    let mut want = Vec::new();

    // The changes the test makes, as the User and as the Application.
    let inserted = write(
        &own,
        &alice,
        "user",
        "insert into things (id, title) values (900, 'written by alice')",
    );
    want.push(expected(
        "things",
        "insert",
        &json!({ "id": 900 }),
        "alice",
        "user",
        Some(&inserted),
        None,
        &Value::Null,
    ));
    let updated = write(
        &own,
        &bob,
        "application",
        "update things set title = 'changed by the application' where id = 900",
    );
    want.push(expected(
        "things",
        "update",
        &json!({ "id": 900 }),
        "bob",
        "application",
        Some(&updated),
        None,
        &Value::Null,
    ));

    // The runtime role, through the program: forge_all() and the calls by
    // the names the catalog gives now, with `execute` and `query`.
    let functions = vm_functions(&db);
    let calls = static_calls(&functions);
    for (user, actor) in [(&alice, "user"), (&bob, "application")] {
        let mut through = vec![
            own_execute(actor, "select forge_all()"),
            op(
                "query",
                &json!({ "own": true, "as": actor, "sql": "select forge_all()" }),
            ),
        ];
        for call in &calls {
            through.push(own_execute(actor, call));
            through.push(op(
                "query",
                &json!({ "own": true, "as": actor, "sql": call }),
            ));
        }
        let answered = send_calls(&own, user, &through);
        assert_eq!(
            answered,
            through.len(),
            "the harness's own check: one answer per call as the {actor}"
        );
    }

    // The runtime role in a session of its own, each statement committed.
    let session = session_as(&db, Some(&db.runtime));
    let mut let_through = usize::from(session.try_coded("SELECT forge_all()").is_ok());
    for call in &calls {
        let_through += usize::from(session.try_coded(call).is_ok());
    }
    println!(
        "the runtime role's session: {let_through} of {} statements let through",
        calls.len() + 1
    );
    drop(session);

    // A migration: a row changed, a column dropped, and forge_all().
    let forging = Migration::new(
        FORGING,
        &[
            r#"UPDATE "things" SET "size" = 5 WHERE "id" = 900;"#,
            r#"ALTER TABLE "parents" DROP COLUMN "name";"#,
            r#"SELECT "forge_all"();"#,
        ],
    );
    let mut list = sent.clone();
    list.push(forging);
    let answer = own.send(APP, &list);
    match answer.status {
        201 => {
            sent = list;
            want.push(expected(
                "things",
                "update",
                &json!({ "id": 900 }),
                INSTALLER,
                "migration",
                None,
                Some(FORGING),
                &Value::Null,
            ));
            want.push(expected(
                "parents",
                "structure",
                &Value::Null,
                INSTALLER,
                "migration",
                None,
                Some(FORGING),
                &json!({ "change": "column-dropped", "column": "name" }),
            ));
        }
        422 => {}
        other => panic!(
            "expected the Version whose migration forges to be refused or fail (422) or apply (201), got {other}:\n{}",
            answer.describe()
        ),
    }

    // A migration whose statements are the calls themselves.
    let statements: Vec<String> = calls.iter().map(|c| format!("{c};")).collect();
    let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
    let mut list = sent.clone();
    list.push(Migration::new(PLAIN, &refs));
    let answer = own.send(APP, &list);
    assert!(
        answer.status == 201 || answer.status == 422,
        "expected the Version of plain calls refused or failed (422) or applied (201), got {}:\n{}",
        answer.status,
        answer.describe()
    );

    // What is kept: exactly the changes made.
    want.sort_by_key(Value::to_string);
    let got = entries_now(&db);
    let extra: Vec<&Value> = got.iter().filter(|g| !want.contains(g)).collect();
    let missing: Vec<&Value> = want.iter().filter(|w| !got.contains(w)).collect();
    assert_eq!(
        got,
        want,
        "expected change_history of {APP:?} to hold exactly the changes the test made (CONTRACT.md: \"Neither role can make an entry of the history other than by changing a row\"); forged or wrong:\n{}\nmissing:\n{}",
        extra
            .iter()
            .map(|v| format!("  {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
        missing
            .iter()
            .map(|v| format!("  {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
