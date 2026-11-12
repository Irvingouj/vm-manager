//! What the runtime role cannot do in its own database (CONTRACT.md,
//! "Identity and Change history in the own database: What the runtime
//! role cannot do"; ticket "Identity in the own database, and Change
//! history", fourth acceptance criterion, the hostile fixture: "the runtime
//! role cannot forge identity, replay a signed value from another
//! transaction, call the attach function, read the secret, switch off or
//! drop the capture, or change or delete history rows").
//!
//! Each attack is made in the database itself as the runtime role (the
//! harness's superuser after `SET ROLE`, with no statement check), and,
//! where one statement that passes the statement check can make it, through
//! the program by Application SQL too (functions the Application's
//! migration made, which run SQL text the check does not see, and the
//! `ts_stat` path of `docs/research/what-postgres-sees.md`). The outcome
//! must always be the identity the host attached, or a failure: never
//! another identity, never a change left unrecorded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{
    APP, CAPTURED, NO_IDENTITY, assert_history_matches_operation_log, assert_nothing_attached,
    attempt, guessed_calls, history, history_count, history_standard, history_where, own_call,
    secret_values, session_as, snapshot, vm_functions, vm_relations,
};
use blackbox::own_database::{
    ACTORS, Own, OwnDatabase, assert_ok, ident, literal, own_execute, own_query,
};
use blackbox::scenario::lines;
use blackbox::shared_database::token;
use serde_json::{Value, json};

/// Makes a statement from a row's title.
type MakeSql = Box<dyn Fn(&str) -> String>;

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

fn installed() -> (Own, OwnDatabase) {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    (own, db)
}

/// The `vm.ctx` of an operation of `user` as `actor` (through `query`), its
/// backend's pid, and its request's id.
fn capture(own: &Own, user: &User, actor: &str) -> (String, i64, String) {
    let (response, answer) = own.one(
        user,
        APP,
        &own_query(
            actor,
            "select current_setting('vm.ctx') as ctx, pg_backend_pid() as pid",
        ),
    );
    assert_ok(&answer, "reading vm.ctx");
    let ctx = answer["ok"]["rows"][0][0]
        .as_str()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| panic!("expected vm.ctx to be non-empty text, got {answer}"))
        .to_owned();
    let pid = answer["ok"]["rows"][0][1].as_i64().unwrap();
    (ctx, pid, response.request_id())
}

/// What `vm.identity()` must give for this operation.
fn true_identity(user: &str, actor: &str, request: &str) -> Value {
    json!({ "user_id": user, "application": APP, "actor": actor, "request": request })
}

/// Fails unless `r`, what `identity_after` answered, is the true identity
/// or an error raised; returns whether it raised.
fn assert_true_or_raised(r: &Value, want: &Value, context: &str) -> bool {
    if r["identity"].is_object() {
        assert_eq!(
            &r["identity"], want,
            "{context}: vm.identity() answered an identity other than the one the host attached: {r}"
        );
        false
    } else {
        assert!(
            r["sqlstate"].is_string(),
            "{context}: expected the identity the host attached or an error, got {r}"
        );
        true
    }
}

/// The one value an `execute` or `query` of `select ... as r` answered.
fn the_value(answer: &Value, context: &str) -> Value {
    assert_ok(answer, context);
    answer["ok"]["rows"][0][0].clone()
}

/// Fails unless an attempt to write a row of `things` titled `title` either
/// failed and left neither the row nor a history row, or ran and left the
/// row and one history row with the true identity of its operation.
fn assert_written_truly_or_not_at_all(
    db: &OwnDatabase,
    answer: &Value,
    title: &str,
    want: &Value,
    context: &str,
) {
    let rows = db.column(&format!(
        "SELECT count(*)::text FROM things WHERE title = {}",
        literal(title)
    ));
    let present = rows.first().map(String::as_str) != Some("0");
    let recorded = history_where(db, &format!("h.new->>'title' = {}", literal(title)));
    if answer.get("ok").is_some() {
        assert!(
            present,
            "{context}: the statement ran but left no row titled {title:?}: {answer}"
        );
        assert_eq!(
            recorded.len(),
            1,
            "{context}: expected the row written to be recorded once, got:\n{}",
            lines(&recorded)
        );
        let row = &recorded[0];
        let got = json!({ "user_id": row["user_id"], "application": row["application"], "actor": row["actor"], "request": row["request"] });
        assert_eq!(
            &got, want,
            "{context}: the change was recorded under an identity the host did not attach: {row}"
        );
    } else {
        assert!(
            answer["error"].is_object(),
            "{context}: expected the statement to run or fail, got {answer}"
        );
        assert!(
            !present && recorded.is_empty(),
            "{context}: the statement failed but left a row or a history row: {}",
            lines(&recorded)
        );
    }
}

/// The rows of every relation in schema `vm` but `vm.history`, read by
/// the superuser: to show the runtime role changed nothing there.
fn vm_rows(db: &OwnDatabase) -> String {
    let mut text = String::new();
    for relation in vm_relations(db) {
        if relation.name == "history" || !["r", "p", "m"].contains(&relation.kind.as_str()) {
            continue;
        }
        let rows = db
            .superuser()
            .value(&format!(
                "SELECT coalesce(string_agg(t::text, '|' ORDER BY t::text), '') FROM vm.{} t",
                ident(&relation.name)
            ))
            .unwrap_or_default();
        text.push_str(&format!("vm.{}: {rows}\n", relation.name));
    }
    text
}

/// Everything public about `board` that a table of `vm` may hold without
/// it being a secret: what the API answers about its Versions (migration
/// names and hashes, the commit), the migrations' text and statements, the
/// names of its database and roles, the Users.
fn public_values(own: &Own, db: &OwnDatabase) -> Vec<String> {
    let mut public: Vec<String> = own
        .api()
        .versions(APP)
        .iter()
        .map(Value::to_string)
        .collect();
    for migration in history_standard() {
        public.push(migration.name.clone());
        public.push(migration.sql.clone());
    }
    public.extend([
        APP.to_owned(),
        db.name.clone(),
        db.owner.clone(),
        db.runtime.clone(),
        db.platform.clone(),
        blackbox::INSTALLER.to_owned(),
    ]);
    public
}

#[test]
fn a_forged_or_replayed_vm_ctx_attaches_nothing_in_the_database() {
    let (own, db) = installed();
    let (alice_ctx, _, alice_request) = capture(&own, &alice(), "user");
    let (bob_ctx, _, bob_request) = capture(&own, &bob(), "application");
    let before = snapshot(&db);
    let mut flipped = alice_ctx.clone();
    let last = flipped.pop().unwrap();
    flipped.push(if last == 'A' { 'B' } else { 'A' });
    let half: String = alice_ctx
        .chars()
        .take(alice_ctx.chars().count() / 2)
        .collect();
    let forged_json =
        json!({ "user": "mallory", "user_id": "mallory", "application": APP, "app": APP,
                              "actor": "user", "request": alice_request, "tag": "forged" })
        .to_string();
    let variants = [
        ("alice's vm.ctx, replayed", alice_ctx.clone()),
        ("bob's vm.ctx as the Application, replayed", bob_ctx.clone()),
        (
            "alice's vm.ctx naming mallory",
            alice_ctx.replace("alice", "mallory"),
        ),
        (
            "alice's vm.ctx with bob's request",
            alice_ctx.replace(&alice_request, &bob_request),
        ),
        ("alice's vm.ctx with its last character changed", flipped),
        ("the first half of alice's vm.ctx", half),
        ("an empty vm.ctx", String::new()),
        ("a vm.ctx made up as JSON", forged_json),
        (
            "alice's and bob's vm.ctx together",
            format!("{alice_ctx}{bob_ctx}"),
        ),
    ];
    let sql = session_as(&db, Some(&db.runtime));
    let mut set = 0;
    for (what, value) in &variants {
        let lit = literal(value);
        for how in [
            format!("SELECT set_config('vm.ctx', {lit}, true)"),
            format!("SELECT set_config('vm.ctx', {lit}, false)"),
            format!("SET vm.ctx = {lit}"),
            format!("SET LOCAL vm.ctx = {lit}"),
        ] {
            sql.batch("BEGIN");
            if attempt(&sql, &how).is_ok() {
                set += 1;
            }
            assert_nothing_attached(&sql, &format!("{what}, by `{how}`, as the runtime role"));
            sql.batch("ROLLBACK");
        }
        // Set for the whole session, then a transaction of its own.
        drop(sql.try_coded(&format!("SET vm.ctx = {lit}")));
        sql.batch("BEGIN");
        assert_nothing_attached(&sql, &format!("{what}, set for the session"));
        sql.batch("ROLLBACK");
        drop(sql.try_coded("RESET vm.ctx"));
    }
    // The label's settings, set to a real identity.
    sql.batch("BEGIN");
    for (setting, value) in [
        ("vm.user", "alice"),
        ("vm.application", APP),
        ("vm.actor", "user"),
        ("vm.request", alice_request.as_str()),
        ("vm.ctx", alice_ctx.as_str()),
    ] {
        let set = attempt(
            &sql,
            &format!(
                "SELECT set_config({}, {}, true)",
                literal(setting),
                literal(value)
            ),
        );
        assert!(
            set.is_ok(),
            "the harness's own check: setting {setting}: {set:?}"
        );
    }
    assert_nothing_attached(&sql, "the label and alice's vm.ctx set together");
    sql.batch("ROLLBACK");
    drop(sql);
    assert!(
        set > 0,
        "the harness's own check: the runtime role must be able to set vm.ctx at all"
    );
    assert_eq!(
        snapshot(&db),
        before,
        "expected nothing written by the attempts"
    );
}

#[test]
fn a_vm_ctx_replayed_by_application_sql_on_the_same_or_another_connection_attaches_nothing() {
    let (own, db) = installed();
    // The control: with nothing set, identity_after answers the truth.
    let (response, answer) = own.one(
        &bob(),
        APP,
        &own_execute("user", "select identity_after(null, null) as r"),
    );
    let r = the_value(&answer, "identity_after with nothing set");
    assert_eq!(
        r["identity"],
        true_identity("bob", "user", &response.request_id()),
        "the control: with nothing set, identity_after must answer vm.identity() as the host attached it: {r}"
    );
    let (alice_ctx, alice_pid, _) = capture(&own, &alice(), "user");
    let shapes = [
        ("execute", "user"),
        ("query", "user"),
        ("execute", "application"),
        ("query", "application"),
    ];
    let mut same_connection = 0;
    let mut other_connection = 0;
    for attempt_number in 0..40 {
        let (operation, actor) = shapes[attempt_number % shapes.len()];
        let (response, answer) = own.one(
            &bob(),
            APP,
            &own_call(
                operation,
                actor,
                "select identity_after('vm.ctx', $1) as r",
                &json!([alice_ctx]),
            ),
        );
        let r = the_value(&answer, "identity_after with alice's vm.ctx");
        assert_true_or_raised(
            &r,
            &true_identity("bob", actor, &response.request_id()),
            &format!("alice's vm.ctx replayed by bob's {operation} as the {actor}"),
        );
        if r["pid"].as_i64() == Some(alice_pid) {
            same_connection += 1;
        } else {
            other_connection += 1;
        }
        if same_connection >= 2 && other_connection >= 1 && attempt_number >= shapes.len() {
            break;
        }
    }
    assert!(
        same_connection >= 1,
        "the harness could not replay alice's vm.ctx on the connection it was attached on (pid {alice_pid}) in 40 tries; {other_connection} tries landed on another"
    );
    // A write after the replay, in the same statement.
    for (i, actor) in ACTORS.iter().cycle().take(8).enumerate() {
        let title = format!("replayed {i} {}", token());
        let (response, answer) = own.one(
            &bob(),
            APP,
            &own_call(
                "execute",
                actor,
                "select insert_after('vm.ctx', $1, $2) as pid",
                &json!([alice_ctx, title]),
            ),
        );
        assert_written_truly_or_not_at_all(
            &db,
            &answer,
            &title,
            &true_identity("bob", actor, &response.request_id()),
            &format!("a write after alice's vm.ctx replayed as the {actor}"),
        );
    }
    assert_history_matches_operation_log(&own, &db, "after replays through the Application");
}

#[test]
fn application_sql_that_forges_inside_the_hosts_own_transaction_never_gets_another_identity() {
    let (own, db) = installed();
    let (alice_ctx, _, _) = capture(&own, &alice(), "user");
    let forged = json!({ "user": "mallory", "user_id": "mallory", "application": APP, "actor": "user", "request": "forged" }).to_string();
    let forgeries: Vec<(&str, String)> = vec![
        (
            "its own vm.ctx naming mallory",
            "identity_after('vm.ctx', replace(current_setting('vm.ctx'), 'alice', 'mallory'))".to_owned(),
        ),
        (
            "its own vm.ctx with its last character changed",
            "identity_after('vm.ctx', overlay(current_setting('vm.ctx') placing 'A' from length(current_setting('vm.ctx')) for 1))".to_owned(),
        ),
        (
            "its own vm.ctx twice",
            "identity_after('vm.ctx', current_setting('vm.ctx') || current_setting('vm.ctx'))".to_owned(),
        ),
        ("an empty vm.ctx", "identity_after('vm.ctx', '')".to_owned()),
        (
            "a vm.ctx made up as JSON",
            format!("identity_after('vm.ctx', {})", literal(&forged)),
        ),
        (
            "an earlier operation's vm.ctx",
            format!("identity_after('vm.ctx', {})", literal(&alice_ctx)),
        ),
        ("vm.user", "identity_after('vm.user', 'mallory')".to_owned()),
        ("vm.actor", "identity_after('vm.actor', 'application')".to_owned()),
        ("vm.request", "identity_after('vm.request', 'forged')".to_owned()),
        ("vm.application", "identity_after('vm.application', 'another')".to_owned()),
    ];
    for actor in ACTORS {
        for operation in ["query", "execute"] {
            for (what, call) in &forgeries {
                let sql = format!("select {call} as r");
                let (response, answer) =
                    own.one(&bob(), APP, &own_call(operation, actor, &sql, &json!([])));
                let r = the_value(&answer, &sql);
                assert_true_or_raised(
                    &r,
                    &true_identity("bob", actor, &response.request_id()),
                    &format!("{what}, set by bob's {operation} as the {actor}"),
                );
            }
        }
        // Writes after a forgery, each in one statement that passes the
        // statement check.
        let writes: Vec<(&str, MakeSql)> = vec![
            (
                "insert_after with its own vm.ctx naming mallory",
                Box::new(|t: &str| {
                    format!(
                        "select insert_after('vm.ctx', replace(current_setting('vm.ctx'), 'bob', 'mallory'), {}) as pid",
                        literal(t)
                    )
                }),
            ),
            (
                "insert_after with an earlier operation's vm.ctx",
                Box::new({
                    let ctx = alice_ctx.clone();
                    move |t: &str| {
                        format!(
                            "select insert_after('vm.ctx', {}, {}) as pid",
                            literal(&ctx),
                            literal(t)
                        )
                    }
                }),
            ),
            (
                "insert_after with vm.user set to mallory",
                Box::new(|t: &str| {
                    format!(
                        "select insert_after('vm.user', 'mallory', {}) as pid",
                        literal(t)
                    )
                }),
            ),
            (
                "ts_stat setting a made-up vm.ctx, then an insert",
                Box::new(|t: &str| {
                    format!(
                        "with f as (select * from ts_stat($q$select to_tsvector('simple', set_config('vm.ctx', 'forged-by-ts-stat', true))$q$)),
                              i as (insert into things (title) select {} from f limit 1 returning id)
                         select id from i",
                        literal(t)
                    )
                }),
            ),
            (
                "ts_stat setting an earlier operation's vm.ctx, then an insert",
                Box::new({
                    let ctx = alice_ctx.clone();
                    move |t: &str| {
                        format!(
                            "with f as (select * from ts_stat($q$select to_tsvector('simple', set_config('vm.ctx', {}, true))$q$)),
                                  i as (insert into things (title) select {} from f limit 1 returning id)
                             select id from i",
                            literal(&ctx),
                            literal(t)
                        )
                    }
                }),
            ),
            (
                "run_sql running a DO block that forges and inserts",
                Box::new(|t: &str| {
                    let block = format!(
                        "DO $d$ BEGIN PERFORM set_config('vm.ctx', 'forged-in-do', true); INSERT INTO things (title) VALUES ({}); END $d$",
                        literal(t)
                    );
                    format!("select run_sql({}) as done", literal(&block))
                }),
            ),
            (
                "run_sql turning off triggers for the transaction, then an insert",
                Box::new(|t: &str| {
                    format!(
                        "with a as (select run_sql('SET LOCAL session_replication_role = replica') as x),
                              i as (insert into things (title) select {} from a returning id)
                         select id from i",
                        literal(t)
                    )
                }),
            ),
        ];
        for (what, make) in &writes {
            let title = format!("{what} {}", token());
            let (response, answer) = own.one(&bob(), APP, &own_execute(actor, &make(&title)));
            assert_written_truly_or_not_at_all(
                &db,
                &answer,
                &title,
                &true_identity("bob", actor, &response.request_id()),
                &format!("{what}, as the {actor}"),
            );
        }
    }
    let mallory = history_where(&db, "h.user_id = 'mallory' OR h.request = 'forged'");
    assert!(mallory.is_empty(), "{}", lines(&mallory));
    assert_history_matches_operation_log(&own, &db, "after forgeries through the Application");
}

#[test]
fn every_function_in_vm_called_with_guessed_arguments_attaches_nothing_and_shows_no_secret() {
    let (own, db) = installed();
    let (alice_ctx, _, _) = capture(&own, &alice(), "user");
    let functions = vm_functions(&db);
    assert!(
        functions.iter().any(|f| f.name == "identity"),
        "expected vm.identity() among the functions of schema vm, got {functions:#?}"
    );
    let secrets = secret_values(&db, &public_values(&own, &db));
    let vm_before = vm_rows(&db);
    let platform_before = db.platform_schema();
    let before = snapshot(&db);
    let sql = session_as(&db, Some(&db.runtime));
    let mut leaked = Vec::new();
    let mut calls_made = 0;
    for function in &functions {
        for call in guessed_calls(function, &alice_ctx) {
            calls_made += 1;
            sql.batch("BEGIN");
            if let Ok(rows) = attempt(&sql, &call) {
                let text = format!("{rows:?}");
                for secret in &secrets {
                    if text.contains(secret.as_str()) {
                        leaked.push(format!("  {call} answered a secret value"));
                    }
                }
            }
            assert_nothing_attached(&sql, &format!("after `{call}` as the runtime role"));
            sql.batch("ROLLBACK");
        }
    }
    assert!(calls_made > 0);
    assert!(leaked.is_empty(), "{}", leaked.join("\n"));
    drop(sql);
    // Through the program: the call, then vm.identity(), in one statement.
    for actor in ACTORS {
        for function in &functions {
            for call in guessed_calls(function, &alice_ctx) {
                let expression = call.trim_start_matches("SELECT ");
                let statement = format!(
                    "select identity_after(null, null) as r, a.s is null as n
                       from (select string_agg(x.v::text, ',') as s from (select {expression} as v) x) a"
                );
                let (response, answer) = own.one(&bob(), APP, &own_execute(actor, &statement));
                for secret in &secrets {
                    assert!(
                        !answer.to_string().contains(secret.as_str()),
                        "`{call}` through the program as the {actor} answered a secret value"
                    );
                }
                if answer.get("ok").is_some() {
                    assert_true_or_raised(
                        &answer["ok"]["rows"][0][0],
                        &true_identity("bob", actor, &response.request_id()),
                        &format!("after `{call}` through the program as the {actor}"),
                    );
                } else {
                    assert!(answer["error"].is_object(), "{answer}");
                }
            }
        }
    }
    assert_eq!(
        vm_rows(&db),
        vm_before,
        "expected nothing in vm changed by the calls"
    );
    assert_eq!(db.platform_schema(), platform_before);
    assert_eq!(
        snapshot(&db),
        before,
        "expected no row and no history written by the calls"
    );
}

#[test]
fn the_runtime_role_reads_vm_history_identity_and_ctx_and_nothing_else_in_vm() {
    let (own, db) = installed();
    let (_, answer) = own.one(
        &alice(),
        APP,
        &own_execute(
            "user",
            "insert into things (title) values ('history to read')",
        ),
    );
    assert_ok(&answer, "an insert");
    let usage = db.superuser().value(&format!(
        "SELECT has_schema_privilege({}, 'vm', 'USAGE')::text",
        literal(&db.runtime)
    ));
    assert_eq!(usage.as_deref(), Some("true"), "USAGE on schema vm");
    let relations = vm_relations(&db);
    assert!(
        relations.iter().any(|r| r.name == "history"),
        "expected vm.history, got {relations:#?}"
    );
    let sql = session_as(&db, Some(&db.runtime));
    let history_read = sql.try_coded("SELECT * FROM vm.history");
    assert!(
        matches!(&history_read, Ok(rows) if rows.len() == 1),
        "expected the runtime role to read vm.history, got {history_read:?}"
    );
    let ctx = sql.try_coded("SELECT current_setting('vm.ctx', true)");
    assert!(ctx.is_ok(), "expected vm.ctx readable, got {ctx:?}");
    let identity = sql.try_coded("SELECT * FROM vm.identity()");
    assert!(
        matches!(&identity, Err(e) if e.code == NO_IDENTITY),
        "expected the runtime role to be able to call vm.identity() (raising {NO_IDENTITY} with nothing attached), got {identity:?}"
    );
    let mut readable = Vec::new();
    let mut unreadable_tables = 0;
    for relation in &relations {
        if relation.name == "history" || relation.kind == "S" {
            continue;
        }
        let name = format!("vm.{}", ident(&relation.name));
        let mut tries = vec![format!("SELECT * FROM {name}")];
        for column in &relation.columns {
            tries.push(format!("SELECT {} FROM {name}", ident(column)));
        }
        tries.push(format!("SELECT count(*) FROM {name}"));
        for statement in tries {
            match sql.try_coded(&statement) {
                Err(e) if e.code == "42501" => {}
                other => readable.push(format!("  {statement}: {other:?}")),
            }
        }
        if ["r", "p"].contains(&relation.kind.as_str()) {
            unreadable_tables += 1;
        }
        // Through the program too.
        for actor in ACTORS {
            let (_, answer) = own.one(
                &alice(),
                APP,
                &own_query(actor, &format!("select * from {name}")),
            );
            if answer.get("ok").is_some() {
                readable.push(format!(
                    "  select * from {name} through query as the {actor}: {answer}"
                ));
            }
        }
    }
    assert!(
        readable.is_empty(),
        "expected the runtime role to read nothing in vm but vm.history (CONTRACT.md: it can read vm.identity(), vm.ctx, vm.history), but:\n{}",
        readable.join("\n")
    );
    assert!(
        unreadable_tables >= 1,
        "expected the platform's secret in a table of schema vm the runtime role cannot read; the relations of vm are {relations:#?}"
    );
    let secrets = secret_values(&db, &public_values(&own, &db));
    assert!(
        !secrets.is_empty(),
        "expected a value of at least 12 characters in a table of vm the runtime role cannot read (the platform's secret)"
    );
}

#[test]
fn the_runtime_role_cannot_stop_the_capture() {
    let (own, db) = installed();
    let triggers = db.rows(&format!(
        "SELECT tgrelid::regclass::text, tgname::text FROM pg_trigger
          WHERE NOT tgisinternal AND tgrelid::regclass::text IN ({})
          ORDER BY 1, 2",
        CAPTURED
            .iter()
            .map(|t| literal(t.strip_prefix("public.").unwrap_or(t)))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let triggers: Vec<(String, String)> = triggers
        .into_iter()
        .map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap()))
        .collect();
    for table in CAPTURED {
        let short = table.strip_prefix("public.").unwrap_or(table);
        assert!(
            triggers.iter().any(|(t, _)| t == short),
            "expected a capture trigger on {table} (the ticket: \"A trigger on every table records each change\"); the triggers are {triggers:?}"
        );
    }
    let functions = vm_functions(&db);
    assert!(!functions.is_empty(), "expected functions in schema vm");
    let mut statements: Vec<String> = Vec::new();
    for (table, trigger) in &triggers {
        let t = ident_path(table);
        let g = ident(trigger);
        statements.extend([
            format!("ALTER TABLE {t} DISABLE TRIGGER {g}"),
            format!("ALTER TABLE {t} DISABLE TRIGGER ALL"),
            format!("ALTER TABLE {t} DISABLE TRIGGER USER"),
            format!("ALTER TABLE {t} ENABLE REPLICA TRIGGER {g}"),
            format!("DROP TRIGGER {g} ON {t}"),
            format!("DROP TRIGGER {g} ON {t} CASCADE"),
            format!("ALTER TRIGGER {g} ON {t} RENAME TO renamed_capture"),
            format!(
                "CREATE OR REPLACE TRIGGER {g} AFTER INSERT ON {t} FOR EACH ROW EXECUTE FUNCTION run_sql()"
            ),
            format!("TRUNCATE {t}"),
            format!("TRUNCATE {t} CASCADE"),
        ]);
    }
    for function in &functions {
        let sig = &function.signature;
        let ret = if function.returns_set {
            format!("SETOF {}", function.returns)
        } else {
            function.returns.clone()
        };
        let args = function.arguments.join(", ");
        statements.extend([
            format!("DROP FUNCTION {sig}"),
            format!("DROP FUNCTION {sig} CASCADE"),
            format!("ALTER FUNCTION {sig} RENAME TO hijacked"),
            format!("ALTER FUNCTION {sig} SECURITY INVOKER"),
            format!("ALTER FUNCTION {sig} OWNER TO CURRENT_USER"),
            format!("ALTER FUNCTION {sig} SET search_path = pg_temp, public"),
            format!("ALTER FUNCTION {sig} SET SCHEMA public"),
            format!(
                "CREATE OR REPLACE FUNCTION vm.{}({args}) RETURNS {ret} LANGUAGE plpgsql AS $x$ BEGIN RETURN NULL; END $x$",
                ident(&function.name)
            ),
        ]);
    }
    statements.extend([
        "CREATE OR REPLACE FUNCTION vm.identity() RETURNS TABLE(user_id text, application text, actor text, request text) LANGUAGE sql AS $x$ SELECT 'mallory', 'board', 'user', 'forged' $x$".to_owned(),
        "SET session_replication_role = replica".to_owned(),
        "SET LOCAL session_replication_role = replica".to_owned(),
        "SELECT set_config('session_replication_role', 'replica', true)".to_owned(),
        "SELECT set_config('session_replication_role', 'replica', false)".to_owned(),
        format!("ALTER ROLE {} SET session_replication_role = replica", ident(&db.runtime)),
        "TRUNCATE vm.history".to_owned(),
        "DROP TABLE vm.history".to_owned(),
        "ALTER TABLE vm.history RENAME TO old_history".to_owned(),
        "ALTER TABLE vm.history DISABLE TRIGGER ALL".to_owned(),
        "DROP SCHEMA vm CASCADE".to_owned(),
        "ALTER SCHEMA vm RENAME TO not_vm".to_owned(),
        "ALTER TABLE things RENAME TO things_uncaptured".to_owned(),
        "ALTER TABLE things SET SCHEMA vm".to_owned(),
        "CREATE TABLE things_copy AS SELECT * FROM things".to_owned(),
    ]);
    let vm_before = vm_rows(&db);
    let platform_before = db.platform_schema();
    let fingerprint_before = db.fingerprint();
    let before = snapshot(&db);
    let sql = session_as(&db, Some(&db.runtime));
    let mut allowed = Vec::new();
    for statement in &statements {
        sql.batch("BEGIN");
        if attempt(&sql, statement).is_ok() {
            allowed.push(format!("  {statement}"));
        }
        // Whatever it did, a change in the same transaction still has no
        // identity and fails.
        assert_nothing_attached(&sql, &format!("after `{statement}`"));
        sql.batch("ROLLBACK");
    }
    let setting = sql.value("SELECT current_setting('session_replication_role')");
    drop(sql);
    assert!(
        allowed.is_empty(),
        "expected Postgres to refuse each of these to the runtime role, but it allowed:\n{}",
        allowed.join("\n")
    );
    assert_eq!(setting.as_deref(), Some("origin"));
    assert_eq!(vm_rows(&db), vm_before);
    assert_eq!(db.platform_schema(), platform_before);
    assert_eq!(
        db.fingerprint(),
        fingerprint_before,
        "expected the capture triggers unchanged"
    );
    assert_eq!(snapshot(&db), before);
    // Through the program, past the statement check.
    for actor in ACTORS {
        let mut calls = Vec::new();
        for statement in &statements {
            if statement.starts_with("SET ") {
                continue;
            }
            calls.push(own_execute(
                actor,
                &format!("select run_sql({}) as done", literal(statement)),
            ));
            calls.push(own_execute(
                actor,
                &format!(
                    "select * from ts_stat({})",
                    literal(&format!(
                        "select to_tsvector('simple', run_sql({})::text)",
                        literal(statement)
                    ))
                ),
            ));
        }
        let answers = run_in_chunks(&own, &alice(), &calls);
        for (call, answer) in calls.iter().zip(&answers) {
            assert!(
                answer.get("ok").is_none(),
                "expected {call} as the {actor} to fail, got {answer}"
            );
        }
    }
    assert_eq!(vm_rows(&db), vm_before);
    assert_eq!(db.platform_schema(), platform_before);
    assert_eq!(db.fingerprint(), fingerprint_before);
    assert_eq!(snapshot(&db), before);
    // The capture still works.
    let count = history_count(&db);
    let (_, answer) = own.one(
        &alice(),
        APP,
        &own_execute(
            "user",
            "insert into things (title) values ('still captured')",
        ),
    );
    assert_ok(&answer, "an insert after the attempts");
    assert_eq!(
        history_count(&db),
        count + 1,
        "expected the capture to still record"
    );
}

/// Runs these calls in `board` as `user`, a few per request so that each
/// request ends well within its time; returns every answer in order.
fn run_in_chunks(own: &Own, user: &User, calls: &[Value]) -> Vec<Value> {
    let mut answers = Vec::new();
    for chunk in calls.chunks(25) {
        let (_, mut some) = own.run(user, APP, chunk);
        assert_eq!(some.len(), chunk.len());
        answers.append(&mut some);
    }
    answers
}

/// `schema.table` or `table` (as `regclass` prints it) quoted.
fn ident_path(name: &str) -> String {
    name.split('.').map(ident).collect::<Vec<_>>().join(".")
}

#[test]
fn the_runtime_role_cannot_insert_update_or_delete_history_rows() {
    let (own, db) = installed();
    for actor in ACTORS {
        let (_, answer) = own.one(
            &alice(),
            APP,
            &own_execute(actor, "insert into things (title) values ('recorded')"),
        );
        assert_ok(&answer, "an insert");
    }
    let rows = history(&db);
    assert_eq!(rows.len(), 2, "{}", lines(&rows));
    let before = snapshot(&db);
    let forged_row = "INSERT INTO vm.history (at, schema_name, table_name, kind, key, old, new, changed, user_id, application, actor, request, migration)
                      VALUES (now(), 'public', 'things', 'insert', '{\"id\": 1}', NULL, '{\"id\": 1}', NULL, 'mallory', 'board', 'user', 'forged', NULL)";
    let statements = [
        forged_row.to_owned(),
        "INSERT INTO vm.history SELECT * FROM vm.history".to_owned(),
        "UPDATE vm.history SET user_id = 'mallory'".to_owned(),
        "UPDATE vm.history SET new = NULL".to_owned(),
        "DELETE FROM vm.history".to_owned(),
        format!("DELETE FROM vm.history WHERE id = {}", rows[0]["id"]),
        "TRUNCATE vm.history".to_owned(),
        "SELECT * FROM vm.history FOR UPDATE".to_owned(),
        "COPY vm.history FROM '/dev/null'".to_owned(),
        "SELECT run_sql('DELETE FROM vm.history')".to_owned(),
        "SELECT run_sql('UPDATE vm.history SET user_id = ''mallory''')".to_owned(),
    ];
    let sql = session_as(&db, Some(&db.runtime));
    let mut allowed = Vec::new();
    for statement in &statements {
        if sql.try_coded(statement).is_ok() {
            allowed.push(format!("  {statement}"));
        }
    }
    drop(sql);
    assert!(
        allowed.is_empty(),
        "expected Postgres to refuse each of these to the runtime role, but it allowed:\n{}",
        allowed.join("\n")
    );
    assert_eq!(snapshot(&db), before);
    // Through the program: directly (the statement check allows INSERT,
    // UPDATE and DELETE), through run_sql, and through ts_stat.
    for actor in ACTORS {
        let mut calls: Vec<Value> = [
            forged_row.to_owned(),
            "update vm.history set user_id = 'mallory'".to_owned(),
            "delete from vm.history".to_owned(),
            "with d as (delete from vm.history returning id) select id from d".to_owned(),
            "select run_sql('delete from vm.history') as done".to_owned(),
            "select run_sql('truncate vm.history') as done".to_owned(),
            "select run_sql('truncate things') as done".to_owned(),
            "select * from ts_stat($q$with d as (delete from vm.history returning id) select to_tsvector('simple', id::text) from d$q$)".to_owned(),
        ]
        .iter()
        .map(|s| own_execute(actor, s))
        .collect();
        calls.push(own_query(actor, "delete from vm.history"));
        let (_, answers) = own.run(&bob(), APP, &calls);
        for (call, answer) in calls.iter().zip(&answers) {
            assert!(
                answer.get("ok").is_none(),
                "expected {call} as the {actor} to fail, got {answer}"
            );
        }
        assert_eq!(
            snapshot(&db),
            before,
            "expected history unchanged after the attempts as the {actor}"
        );
    }
    // A history row changes only by changing a captured row.
    let (_, answer) = own.one(
        &alice(),
        APP,
        &own_execute("user", "update things set size = 1"),
    );
    assert_ok(&answer, "an update");
    assert_eq!(history(&db).len(), 4);
    assert_history_matches_operation_log(&own, &db, "after the attempts on vm.history");
}

#[test]
fn the_runtime_role_can_create_nothing_in_vm_nor_shadow_its_names() {
    let (own, db) = installed();
    let platform_before = db.platform_schema();
    let fingerprint_before = db.fingerprint();
    let identity_table = "TABLE(user_id text, application text, actor text, request text)";
    let statements = [
        "CREATE TABLE vm.sneaky (x int)".to_owned(),
        "CREATE VIEW vm.sneaky AS SELECT 1 AS x".to_owned(),
        "CREATE SEQUENCE vm.sneaky_seq".to_owned(),
        "CREATE TYPE vm.sneaky_type AS (x int)".to_owned(),
        "CREATE DOMAIN vm.sneaky_domain AS int".to_owned(),
        "CREATE FUNCTION vm.sneaky() RETURNS int LANGUAGE sql AS 'SELECT 1'".to_owned(),
        format!("CREATE FUNCTION vm.identity(forged text) RETURNS {identity_table} LANGUAGE sql AS $x$ SELECT 'mallory', 'board', 'user', 'forged' $x$"),
        format!("CREATE OR REPLACE FUNCTION vm.identity() RETURNS {identity_table} LANGUAGE sql AS $x$ SELECT 'mallory', 'board', 'user', 'forged' $x$"),
        "CREATE TEMP TABLE history (id bigint)".to_owned(),
        "CREATE TEMP VIEW history AS SELECT 1 AS id".to_owned(),
        format!("CREATE FUNCTION pg_temp.identity() RETURNS {identity_table} LANGUAGE sql AS $x$ SELECT 'mallory', 'board', 'user', 'forged' $x$"),
        "CREATE FUNCTION pg_temp.current_setting(text) RETURNS text LANGUAGE sql AS $x$ SELECT 'forged' $x$".to_owned(),
        "CREATE TABLE public.history (id bigint)".to_owned(),
        format!("CREATE FUNCTION public.identity() RETURNS {identity_table} LANGUAGE sql AS $x$ SELECT 'mallory', 'board', 'user', 'forged' $x$"),
        "CREATE FUNCTION public.current_setting(text) RETURNS text LANGUAGE sql AS $x$ SELECT 'forged' $x$".to_owned(),
        "CREATE FUNCTION public.current_setting(text, boolean) RETURNS text LANGUAGE sql AS $x$ SELECT 'forged' $x$".to_owned(),
        "CREATE FUNCTION public.set_config(text, text, boolean) RETURNS text LANGUAGE sql AS $x$ SELECT 'forged' $x$".to_owned(),
        "CREATE FUNCTION public.to_jsonb(anyelement) RETURNS jsonb LANGUAGE sql AS $x$ SELECT '{}'::jsonb $x$".to_owned(),
        "CREATE FUNCTION public.now() RETURNS timestamptz LANGUAGE sql AS $x$ SELECT 'epoch'::timestamptz $x$".to_owned(),
        "CREATE SCHEMA vm2".to_owned(),
        "CREATE SCHEMA pg_vm".to_owned(),
        "ALTER TABLE things SET SCHEMA vm".to_owned(),
        "ALTER FUNCTION run_sql(text) SET SCHEMA vm".to_owned(),
        "ALTER DEFAULT PRIVILEGES IN SCHEMA vm GRANT ALL ON TABLES TO PUBLIC".to_owned(),
        "GRANT ALL ON SCHEMA vm TO PUBLIC".to_owned(),
        "GRANT ALL ON ALL TABLES IN SCHEMA vm TO PUBLIC".to_owned(),
    ];
    let sql = session_as(&db, Some(&db.runtime));
    let mut allowed = Vec::new();
    for statement in &statements {
        sql.batch("BEGIN");
        let result = attempt(&sql, statement);
        // GRANT of what is not held is a warning, not an error: what counts
        // is that nothing changes, checked below.
        if result.is_ok()
            && !statement.starts_with("GRANT")
            && !statement.starts_with("ALTER DEFAULT")
        {
            allowed.push(format!("  {statement}"));
        }
        assert_nothing_attached(&sql, &format!("after `{statement}`"));
        sql.batch("COMMIT");
    }
    drop(sql);
    assert!(
        allowed.is_empty(),
        "expected Postgres to refuse each of these to the runtime role, but it allowed:\n{}",
        allowed.join("\n")
    );
    assert_eq!(db.platform_schema(), platform_before);
    assert_eq!(db.fingerprint(), fingerprint_before);
    let creatable = db.column(&format!(
        "SELECT nspname::text FROM pg_namespace WHERE has_schema_privilege({}, oid, 'CREATE') ORDER BY 1",
        literal(&db.runtime)
    ));
    assert!(
        creatable.is_empty(),
        "expected the runtime role to hold CREATE on no schema, got {creatable:?}"
    );
    // Through the program too.
    for actor in ACTORS {
        let calls: Vec<Value> = statements
            .iter()
            .map(|s| own_execute(actor, &format!("select run_sql({}) as done", literal(s))))
            .collect();
        let answers = run_in_chunks(&own, &alice(), &calls);
        for (call, answer) in calls.iter().zip(&answers) {
            if call["arguments"]["sql"]
                .as_str()
                .is_some_and(|s| s.contains("GRANT"))
            {
                continue;
            }
            assert!(
                answer.get("ok").is_none(),
                "expected {call} as the {actor} to fail, got {answer}"
            );
        }
    }
    assert_eq!(db.platform_schema(), platform_before);
    assert_eq!(db.fingerprint(), fingerprint_before);
}
