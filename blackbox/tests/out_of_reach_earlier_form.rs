//! An own database in the earlier form is brought to this one when the
//! program starts (CONTRACT.md, "The platform's part of an own database is
//! out of a migration's reach: Three roles": "An own database in the
//! earlier form (its `vm` owned by `_owner`) is brought to this form when
//! the program starts, before it serves; its history, its keys and its
//! tables' rows are unchanged").
//!
//! The harness's superuser gives the schema `vm` and everything the
//! platform's own role owns back to the owner role (`REASSIGN OWNED`, which
//! also gives it the database), lets the owner role log in, and drops the
//! platform's own role: the earlier form, as a program from before the
//! ticket left it. A second variant leaves that role in place. The program is then started again on the same platform database.
//! Before it serves, the three roles stand as CONTRACT.md says, and the
//! keys, the history, the record of migrations and every table's rows are
//! as they were; Version 1 still serves and a change is still recorded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history_after, last_history_id};
use blackbox::out_of_reach::{
    WRITER, assert_three_roles, can_log_in, differences, install_seeded, platform_role, start,
};
use blackbox::own_database::{Own, OwnDatabase, assert_ok, ident, own_execute};

/// What must be unchanged: the rows of every table of `vm` (the keys as a
/// digest) and of every table of the Application's, and every sequence of
/// the Application's.
fn data(db: &OwnDatabase) -> String {
    let mut text = String::new();
    for table in db.column(
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'vm' AND c.relkind IN ('r', 'p') ORDER BY 1",
    ) {
        let query = if table == "keys" {
            format!(
                "SELECT md5(coalesce(string_agg(t::text, '|' ORDER BY t::text), '')) FROM vm.{} t",
                ident(&table)
            )
        } else {
            format!(
                "SELECT coalesce(string_agg(t::text, E'\\n    ' ORDER BY t::text), '') FROM vm.{} t",
                ident(&table)
            )
        };
        text.push_str(&format!(
            "vm.{table}: {}\n",
            db.superuser().value(&query).unwrap_or_default()
        ));
    }
    for line in db.fingerprint().lines() {
        if line.starts_with("rows of ") {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

/// What a program from before the ticket left: no platform's own role at
/// all.
#[test]
fn an_own_database_in_the_earlier_form_is_brought_to_three_roles_at_start_with_its_data_unchanged()
{
    earlier_form_brought_to_three_roles(true);
}

/// The same with the platform's own role still there (owning nothing).
#[test]
fn an_own_database_in_the_earlier_form_with_the_platform_s_role_left_is_brought_to_three_roles() {
    earlier_form_brought_to_three_roles(false);
}

fn earlier_form_brought_to_three_roles(drop_the_role: bool) {
    let own = start();
    let db = install_seeded(&own, APP);
    let before = data(&db);
    assert!(
        before.contains("vm.keys: ") && before.contains("vm.history: "),
        "the harness's own check: expected vm.keys and vm.history, got\n{before}"
    );
    // The earlier form: everything of the platform's back to the owner
    // role, which may log in.
    let platform = platform_role(&db);
    let superuser = db.superuser();
    if db.role_exists(&platform) {
        superuser.batch(&format!(
            "REASSIGN OWNED BY {} TO {}",
            ident(&platform),
            ident(&db.owner)
        ));
    }
    superuser.batch(&format!(
        "ALTER DATABASE {} OWNER TO {o}; ALTER SCHEMA vm OWNER TO {o}; ALTER ROLE {o} LOGIN",
        ident(&db.name),
        o = ident(&db.owner)
    ));
    if drop_the_role && db.role_exists(&platform) {
        superuser.batch(&format!("DROP OWNED BY {}", ident(&platform)));
        db.instance()
            .batch(&format!("DROP ROLE {}", ident(&platform)));
        assert!(
            !db.role_exists(&platform),
            "the harness's own check: the platform's own role dropped"
        );
    }
    drop(superuser);
    assert_eq!(
        db.column(
            "SELECT DISTINCT pg_get_userbyid(owner) FROM (
               SELECT nspowner AS owner FROM pg_namespace WHERE nspname = 'vm'
               UNION ALL SELECT relowner FROM pg_class WHERE relnamespace = 'vm'::regnamespace
               UNION ALL SELECT proowner FROM pg_proc WHERE pronamespace = 'vm'::regnamespace
               UNION ALL SELECT datdba FROM pg_database WHERE datname = current_database()) x"
        ),
        vec![db.owner.clone()],
        "the harness's own check: expected the earlier form, vm and the database the owner role's"
    );
    assert!(can_log_in(&db, &db.owner), "the harness's own check");
    assert_eq!(
        data(&db),
        before,
        "the harness's own check: the data unchanged by the harness"
    );
    // The program started again on the same platform database.
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    assert_three_roles(
        &own,
        &db,
        "after a start on an own database in the earlier form",
    );
    let after = data(&db);
    assert!(
        after == before,
        "expected the keys, the history, the record of migrations and every table's rows unchanged by bringing the own database to three roles, but:\n{}",
        differences(&before, &after)
    );
    assert_eq!(own.serving(&User::new(WRITER), APP), 1);
    let last = last_history_id(&db);
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "update things set title = 'after the start' where id = 101",
        ),
    );
    assert_ok(&answer, "a change after the start");
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 1, "expected the change recorded, got {rows:?}");
    assert_eq!(rows[0]["new"]["title"], "after the start");
    assert_eq!(rows[0]["user_id"], WRITER);
    // And a later Version still applies, and its table is captured.
    let mut list = blackbox::out_of_reach::base();
    list.push(blackbox::own_database::Migration::new(
        "0004_after_the_start",
        &[r#"CREATE TABLE "later" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    ));
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_three_roles(&own, &db, "after a Version sent to the brought database");
}

/// The statements that plant shadows of common built-ins in `public`: each
/// raises a warning holding `token`, its `session_user` and `current_user`
/// (a warning reaches the server's log also from a read-only transaction),
/// then does what the built-in does.
fn shadows(token: &str) -> Vec<String> {
    let mut statements = vec![format!(
        r#"CREATE FUNCTION public."vm_shadow_ran"("what" text) RETURNS void LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $f$
BEGIN
  RAISE WARNING 'vm-shadow-ran {token} fn=% session=% current=%', "what", session_user, current_user;
END $f$"#
    )];
    // (signature, result, body after the warning)
    let functions: [(&str, &str, &str); 16] = [
        (
            "format(text, text)",
            "text",
            "RETURN pg_catalog.format($1, $2);",
        ),
        (
            "format(text, name)",
            "text",
            "RETURN pg_catalog.format($1, $2);",
        ),
        (
            "format(text, text, text)",
            "text",
            "RETURN pg_catalog.format($1, $2, $3);",
        ),
        (
            "format(text, name, name)",
            "text",
            "RETURN pg_catalog.format($1, $2, $3);",
        ),
        (
            "quote_ident(name)",
            "text",
            "RETURN pg_catalog.quote_ident($1::text);",
        ),
        (
            "quote_ident(character varying)",
            "text",
            "RETURN pg_catalog.quote_ident($1::text);",
        ),
        (
            "quote_literal(name)",
            "text",
            "RETURN pg_catalog.quote_literal($1::text);",
        ),
        (
            "set_config(name, text, boolean)",
            "text",
            "RETURN pg_catalog.set_config($1::text, $2, $3);",
        ),
        (
            "current_setting(name)",
            "text",
            "RETURN pg_catalog.current_setting($1::text);",
        ),
        ("md5(name)", "text", "RETURN pg_catalog.md5($1::text);"),
        (
            "md5(character varying)",
            "text",
            "RETURN pg_catalog.md5($1::text);",
        ),
        ("lower(name)", "text", "RETURN pg_catalog.lower($1::text);"),
        ("upper(name)", "text", "RETURN pg_catalog.upper($1::text);"),
        (
            "length(name)",
            "integer",
            "RETURN pg_catalog.length($1::text);",
        ),
        (
            "has_database_privilege(name, name, text)",
            "boolean",
            "RETURN pg_catalog.has_database_privilege($1, $2::text, $3);",
        ),
        (
            "pg_get_userbyid(integer)",
            "name",
            "RETURN pg_catalog.pg_get_userbyid($1::oid);",
        ),
    ];
    for (signature, result, body) in functions {
        let name = signature.split('(').next().unwrap_or_default();
        statements.push(format!(
            "CREATE FUNCTION public.{signature} RETURNS {result} LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $f$ BEGIN PERFORM public.vm_shadow_ran('{name}'); {body} END $f$"
        ));
    }
    // Operators, each through a function of its own.
    let operators: [(&str, &str, &str, &str, &str); 7] = [
        (
            "eq_oid_regclass",
            "=",
            "oid",
            "regclass",
            "RETURN $1 = $2::oid;",
        ),
        (
            "eq_regclass_oid",
            "=",
            "regclass",
            "oid",
            "RETURN $1::oid = $2;",
        ),
        ("eq_text_name", "=", "text", "name", "RETURN $1 = $2::text;"),
        (
            "cat_text_name",
            "||",
            "text",
            "name",
            "RETURN $1 || $2::text;",
        ),
        (
            "cat_name_text",
            "||",
            "name",
            "text",
            "RETURN $1::text || $2;",
        ),
        (
            "cat_name_name",
            "||",
            "name",
            "name",
            "RETURN $1::text || $2::text;",
        ),
        (
            "like_name_text",
            "~~",
            "name",
            "text",
            "RETURN $1::text LIKE $2;",
        ),
    ];
    for (function, operator, left, right, body) in operators {
        let result = if operator == "||" { "text" } else { "boolean" };
        statements.push(format!(
            "CREATE FUNCTION public.shadow_{function}({left}, {right}) RETURNS {result} LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $f$ BEGIN PERFORM public.vm_shadow_ran('{operator} ({left}, {right})'); {body} END $f$"
        ));
        statements.push(format!(
            "CREATE OPERATOR public.{operator} (LEFTARG = {left}, RIGHTARG = {right}, FUNCTION = public.shadow_{function})"
        ));
    }
    // An aggregate.
    statements.push(
        "CREATE FUNCTION public.shadow_agg_step(text, name, text) RETURNS text LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $f$ BEGIN PERFORM public.vm_shadow_ran('string_agg(name, text)'); RETURN CASE WHEN $1 IS NULL THEN $2::text ELSE $1 || $3 || $2::text END; END $f$".to_owned(),
    );
    statements.push(
        "CREATE AGGREGATE public.string_agg(name, text) (SFUNC = public.shadow_agg_step, STYPE = text)"
            .to_owned(),
    );
    statements
}

/// The warnings of the shadows planted with `token`, from the server's log:
/// each `fn=... session=... current=...`.
fn shadow_calls(token: &str) -> Vec<String> {
    let needle = format!("vm-shadow-ran {token} ");
    blackbox::postgres::shared()
        .server_log()
        .lines()
        .filter_map(|line| {
            line.find(&needle)
                .and_then(|at| line.get(at + needle.len()..).map(str::to_owned))
        })
        .collect()
}

/// The shadows' calls made with a role of the platform's as the session's
/// or the current user: `<db>_platform` or the platform's own login.
fn platform_calls(token: &str, db: &OwnDatabase, own: &Own) -> Vec<String> {
    let platform = platform_role(db);
    let login = own.vm.platform().role().to_owned();
    shadow_calls(token)
        .into_iter()
        .filter(|call| {
            call.split_whitespace().any(|word| {
                word == format!("session={platform}")
                    || word == format!("current={platform}")
                    || word == format!("session={login}")
                    || word == format!("current={login}")
            })
        })
        .collect()
}

/// CONTRACT.md: "The platform's own sessions run nothing of the
/// Application's. Whatever a migration left in `public` or any schema of its
/// own (functions, operators, types of any name), no session the platform
/// opens as `_platform` in the own database calls it: not when the database
/// is brought from an earlier form, not when the keys are read, not when the
/// migration role is closed." In the earlier form the old owner also owned
/// the database, so it could set the database's `search_path` to put
/// `public` first; that is planted too.
#[test]
fn the_platform_s_sessions_call_no_shadow_the_old_owner_planted_in_the_earlier_form() {
    let own = start();
    let db = install_seeded(&own, APP);
    let platform = platform_role(&db);
    let superuser = db.superuser();
    if db.role_exists(&platform) {
        superuser.batch(&format!(
            "REASSIGN OWNED BY {} TO {}",
            ident(&platform),
            ident(&db.owner)
        ));
    }
    superuser.batch(&format!(
        "ALTER DATABASE {} OWNER TO {o}; ALTER SCHEMA vm OWNER TO {o}; ALTER ROLE {o} LOGIN",
        ident(&db.name),
        o = ident(&db.owner)
    ));
    if db.role_exists(&platform) {
        superuser.batch(&format!("DROP OWNED BY {}", ident(&platform)));
        db.instance()
            .batch(&format!("DROP ROLE {}", ident(&platform)));
    }
    let token = blackbox::shared_database::token();
    superuser.batch(&format!("SET ROLE {}", ident(&db.owner)));
    for statement in shadows(&token) {
        superuser.batch(&statement);
    }
    superuser.batch(&format!(
        "ALTER DATABASE {} SET search_path = public, pg_catalog",
        ident(&db.name)
    ));
    drop(superuser);
    // The harness's own check: a new session picks the shadows.
    db.superuser()
        .batch("SELECT quote_ident('x'::name), 'a'::name || 'b'::name");
    assert!(
        !shadow_calls(&token).is_empty(),
        "the harness's own check: a new session's search_path puts the shadows first"
    );
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    // Brought over; then the keys read (a change made with an identity),
    // and a Version applied (the migration role opened and closed).
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "update things set title = 'after the shadows' where id = 101",
        ),
    );
    assert_ok(&answer, "a change after the start");
    let mut list = blackbox::out_of_reach::base();
    list.push(blackbox::own_database::Migration::new(
        "0004_after_the_shadows",
        &[r#"CREATE TABLE "later" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    ));
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "application",
            "insert into later (id, v) values (1, 'after')",
        ),
    );
    assert_ok(&answer, "a write after the Version");
    let called = platform_calls(&token, &db, &own);
    assert!(
        called.is_empty(),
        "expected no session of the platform's to call a shadow the old owner planted, got:\n{}",
        called.join("\n")
    );
}

/// The same, planted by a migration of today's form, with the migration
/// role's own `search_path` set to put `public` first.
#[test]
fn the_platform_s_sessions_call_no_shadow_a_migration_planted() {
    let own = start();
    let db = install_seeded(&own, APP);
    let token = blackbox::shared_database::token();
    let mut statements: Vec<String> = shadows(&token)
        .into_iter()
        .map(|s| format!("{s};"))
        .collect();
    statements.push("ALTER ROLE CURRENT_USER SET search_path = public, pg_catalog;".to_owned());
    let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
    let mut list = blackbox::out_of_reach::base();
    list.push(blackbox::own_database::Migration::new(
        "0004_shadows",
        &refs,
    ));
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    // The harness's own check: with public first, the shadows are picked.
    db.superuser().batch(
        "SET search_path = public, pg_catalog; SELECT quote_ident('x'::name), 'a'::name || 'b'::name",
    );
    assert!(
        !shadow_calls(&token).is_empty(),
        "the harness's own check: with public first the shadows are called"
    );
    list.push(blackbox::own_database::Migration::new(
        "0005_after_the_shadows",
        &[r#"CREATE TABLE "later" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
    ));
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute("user", "insert into later (id, v) values (1, 'after')"),
    );
    assert_ok(&answer, "a write after the Versions");
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "insert into later (id, v) values (2, 'after a restart')",
        ),
    );
    assert_ok(&answer, "a write after a restart");
    let called = platform_calls(&token, &db, &own);
    assert!(
        called.is_empty(),
        "expected no session of the platform's to call a shadow a migration planted, got:\n{}",
        called.join("\n")
    );
}
