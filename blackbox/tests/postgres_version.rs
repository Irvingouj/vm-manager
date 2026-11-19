//! One Postgres version: 18 (CONTRACT.md, "One Postgres version"; ticket
//! "One Postgres version: 18").
//!
//! An installation: `vibe serve` and `vibe setup` against a Postgres 17 end
//! as a start-up that cannot work (exit code not `0`, within 15 seconds, a
//! message on standard error saying the version the server has and that 18
//! is needed; for `serve` the log line `postgres-version-unsupported`), and
//! nothing was written to that server: no platform database made, and in
//! one that was there, empty, nothing made. Against the harness's own
//! Postgres 18, `vibe serve` starts.
//!
//! The local run: `VIBE_LOCAL_POSTGRES_BIN` naming a real Postgres 17's
//! `bin` folder ends `postgres-unavailable`, saying the version found, that
//! 18 is needed and to unset `VIBE_LOCAL_POSTGRES_BIN`. (Stand-ins that print
//! a made-up version line are in `local_postgres.rs`.)
//!
//! The Postgres 17 is the harness's (`postgres::other_binaries`): each test
//! starts an instance of its own and stops it on drop, also when it fails.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use blackbox::connection::{SETUP_PREFIX, masked, run_setup};
use blackbox::local::{Local, Mirror, OUTSIDER, Serving, local_project};
use blackbox::postgres::{self, Platform, PrivatePostgres};
use blackbox::program::{Finished, run_vm_to_exit};
use blackbox::sql::Sql;
use blackbox::{DOMAIN, Vm};
use serde_json::Value;

/// The contract's 15 seconds for a start-up that cannot work.
const WITHIN: Duration = Duration::from_secs(15);

/// A Postgres 17 of this test's own, stopped when dropped.
fn postgres_17() -> PrivatePostgres {
    let other = PrivatePostgres::start_other_version()
        .unwrap_or_else(|problem| panic!("the harness's own Postgres 17: {problem}"));
    let version = server_version(&other);
    assert!(
        version.starts_with(&format!("{}.", postgres::OTHER_MAJOR)),
        "the harness's own check: expected a Postgres {}, it says {version:?}",
        postgres::OTHER_MAJOR
    );
    other
}

/// `SHOW server_version` on this instance: the version as the server gives
/// it.
fn server_version(on: &PrivatePostgres) -> String {
    Sql::connect(&on.url("postgres"))
        .unwrap()
        .value("SHOW server_version")
        .unwrap_or_default()
}

/// What the program could have written to the whole server: its databases
/// (name, owner, rights, comment) and its roles (name and attributes; never
/// a password).
fn server_wide(on: &PrivatePostgres) -> Vec<Vec<Option<String>>> {
    Sql::connect(&on.url("postgres")).unwrap().batch(
        "SELECT 'database', datname::text, pg_get_userbyid(datdba)::text, \
                coalesce(datacl::text, ''), coalesce(shobj_description(oid, 'pg_database'), '') \
           FROM pg_database \
         UNION ALL \
         SELECT 'role', rolname::text, \
                concat_ws(',', rolsuper, rolinherit, rolcreaterole, rolcreatedb, rolcanlogin, \
                          rolreplication, rolbypassrls, rolconnlimit, rolvaliduntil), \
                coalesce(array_to_string(rolconfig, ','), ''), \
                coalesce(shobj_description(oid, 'pg_authid'), '') \
           FROM pg_roles \
         ORDER BY 1, 2",
    )
}

/// What a database holds beyond its catalog: its schemas, every relation,
/// function, type and extension outside `pg_catalog`, `information_schema`
/// and `pg_toast`, its event triggers and its default privileges.
fn inside(on: &PrivatePostgres, database: &str) -> Vec<Vec<Option<String>>> {
    Sql::connect(&on.url(database)).unwrap().batch(
        "SELECT 'schema', nspname::text, pg_get_userbyid(nspowner)::text, coalesce(nspacl::text, '') \
           FROM pg_namespace \
         UNION ALL \
         SELECT 'relation', n.nspname || '.' || c.relname, c.relkind::text, pg_get_userbyid(c.relowner)::text \
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
         UNION ALL \
         SELECT 'function', n.nspname || '.' || p.proname, pg_get_function_identity_arguments(p.oid), '' \
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
         UNION ALL \
         SELECT 'type', n.nspname || '.' || t.typname, t.typtype::text, '' \
           FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
          WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
         UNION ALL \
         SELECT 'extension', extname::text, extversion, '' FROM pg_extension \
         UNION ALL \
         SELECT 'event trigger', evtname::text, evtevent::text, '' FROM pg_event_trigger \
         UNION ALL \
         SELECT 'default privileges', pg_get_userbyid(defaclrole)::text, defaclobjtype::text, defaclacl::text \
           FROM pg_default_acl \
         ORDER BY 1, 2, 3",
    )
}

/// The schemas a database just made has, on this instance.
fn schemas_of_a_new_database(on: &PrivatePostgres) -> Vec<Vec<Option<String>>> {
    let name = "vibe_t69_new_database_for_comparison";
    let superuser = Sql::connect(&on.url("postgres")).unwrap();
    superuser.batch(&format!("CREATE DATABASE {name}"));
    let schemas = Sql::connect(&on.url(name))
        .unwrap()
        .batch("SELECT nspname::text FROM pg_namespace ORDER BY 1");
    superuser.batch(&format!("DROP DATABASE {name}"));
    schemas
}

/// `finished` as a test may print it: with `secrets`, and any setup or
/// tool credential, masked.
fn described(what: &str, finished: &Finished, took: Duration, secrets: &[&str]) -> String {
    format!(
        "{what}\nexit: {} after {took:?}\nstandard output:\n{}\nstandard error:\n{}",
        finished.status_text(),
        masked(&finished.stdout, secrets),
        masked(&finished.stderr, secrets)
    )
}

/// The JSON records of the program's log on standard error.
fn records(stderr: &str) -> Vec<Value> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

/// Fails unless `finished` is the contract's refusal of a server of
/// version `found`: exit code not `0` within 15 seconds, a message on
/// standard error, beside the log line, naming `found` and saying 18 is
/// needed, and none of `secrets` anywhere.
fn assert_refused(what: &str, finished: &Finished, took: Duration, found: &str, secrets: &[&str]) {
    let shown = described(what, finished, took, secrets);
    for secret in secrets {
        assert!(
            !finished.stdout.contains(secret) && !finished.stderr.contains(secret),
            "expected the password in none of the program's output\n{shown}"
        );
    }
    assert!(
        finished.status.is_some_and(|s| !s.success()) && took <= WITHIN + Duration::from_secs(1),
        "expected an exit code that is not 0 within {WITHIN:?}\n{shown}"
    );
    // The message: a line of standard error other than the log line
    // `postgres-version-unsupported`, saying the version the server has and
    // that 18 is needed.
    let message_lines: Vec<&str> = finished
        .stderr
        .lines()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .map_or(true, |v| v["event"] != "postgres-version-unsupported")
        })
        .collect();
    assert!(
        message_lines.iter().any(|line| {
            line.contains(found)
                && line
                    .replace(found, "")
                    .contains(&postgres::MAJOR.to_string())
        }),
        "expected a message on standard error saying the version the server has ({found}) and that Postgres {} is needed\n{shown}",
        postgres::MAJOR
    );
}

/// `vibe serve` against this platform database; how long it took.
fn serve(platform: &Platform) -> (Finished, Duration) {
    let args = [
        "serve",
        "--domain",
        DOMAIN,
        "--listen",
        "127.0.0.1:0",
        "--stand-in-sign-in",
    ];
    let began = Instant::now();
    let finished = run_vm_to_exit(args, Some(platform.url()));
    (finished, began.elapsed())
}

/// Fails unless `vibe serve` was refused as the contract says, with its
/// log line and no `listening`.
fn assert_serve_refused(finished: &Finished, took: Duration, found: &str, secrets: &[&str]) {
    let what = "vibe serve against a Postgres 17";
    assert_refused(what, finished, took, found, secrets);
    let shown = described(what, finished, took, secrets);
    let records = records(&finished.stderr);
    assert!(
        !records.iter().any(|r| r["event"] == "listening"),
        "expected no `listening` record\n{shown}"
    );
    let unsupported: Vec<&Value> = records
        .iter()
        .filter(|r| r["event"] == "postgres-version-unsupported")
        .collect();
    assert!(
        unsupported.len() == 1
            && unsupported[0]["found"] == found
            && unsupported[0]["needed"] == "18",
        "expected one log line {{\"event\":\"postgres-version-unsupported\",\"found\":{found:?},\"needed\":\"18\"}}\n{shown}"
    );
}

#[test]
fn serve_against_a_postgres_17_is_refused_and_no_platform_database_is_made() {
    let other = postgres_17();
    let found = server_version(&other);
    let platform = Platform::on(&other, "vibe_t69_platform");
    let before = server_wide(&other);
    assert!(
        !before
            .iter()
            .any(|row| row[1].as_deref() == Some(platform.name())),
        "the harness's own check: the platform database does not exist yet"
    );
    let (finished, took) = serve(&platform);
    let password = other.password().expose();
    assert_serve_refused(&finished, took, &found, &[password]);
    let after = server_wide(&other);
    assert_eq!(
        after, before,
        "expected nothing written to the Postgres 17: its databases and roles as before"
    );
}

#[test]
fn serve_against_a_postgres_17_writes_nothing_in_a_platform_database_that_is_there() {
    let other = postgres_17();
    let found = server_version(&other);
    let name = "vibe_t69_platform_there";
    Sql::connect(&other.url("postgres"))
        .unwrap()
        .batch(&format!("CREATE DATABASE {name}"));
    let platform = Platform::on(&other, name);
    let new_database = schemas_of_a_new_database(&other);
    let before_server = server_wide(&other);
    let before_inside = inside(&other, name);
    let (finished, took) = serve(&platform);
    let password = other.password().expose();
    assert_serve_refused(&finished, took, &found, &[password]);
    assert_eq!(
        server_wide(&other),
        before_server,
        "expected nothing written to the Postgres 17: its databases and roles as before"
    );
    let after_inside = inside(&other, name);
    assert_eq!(
        after_inside, before_inside,
        "expected nothing made in the platform database that was there"
    );
    let db = Sql::connect(&other.url(name)).unwrap();
    assert_eq!(
        db.value(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
              WHERE c.relkind IN ('r', 'p') \
                AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')"
        )
        .as_deref(),
        Some("0"),
        "expected no table in the platform database"
    );
    assert_eq!(
        db.batch("SELECT nspname::text FROM pg_namespace ORDER BY 1"),
        new_database,
        "expected no schema but the ones a new database has"
    );
}

#[test]
fn setup_against_a_postgres_17_is_refused_and_writes_nothing() {
    let other = postgres_17();
    let found = server_version(&other);
    let password = other.password().expose();

    // A platform database that is not there.
    let platform = Platform::on(&other, "vibe_t69_setup");
    let before = server_wide(&other);
    let began = Instant::now();
    let finished = run_setup(Some(platform.url()), &[]);
    let took = began.elapsed();
    assert_refused(
        "vibe setup against a Postgres 17",
        &finished,
        took,
        &found,
        &[password],
    );
    assert!(
        !finished.stdout.contains(SETUP_PREFIX),
        "expected no setup credential printed\n{}",
        described("vibe setup", &finished, took, &[password])
    );
    assert_eq!(
        server_wide(&other),
        before,
        "expected nothing written to the Postgres 17: its databases and roles as before"
    );

    // One that is there, empty, and with `--json`.
    let name = "vibe_t69_setup_there";
    Sql::connect(&other.url("postgres"))
        .unwrap()
        .batch(&format!("CREATE DATABASE {name}"));
    let platform = Platform::on(&other, name);
    let before_server = server_wide(&other);
    let before_inside = inside(&other, name);
    let began = Instant::now();
    let finished = run_setup(Some(platform.url()), &["--json"]);
    let took = began.elapsed();
    assert_refused(
        "vibe setup --json against a Postgres 17, the platform database there",
        &finished,
        took,
        &found,
        &[password],
    );
    assert!(
        !finished.stdout.contains(SETUP_PREFIX),
        "expected no setup credential printed\n{}",
        described("vibe setup --json", &finished, took, &[password])
    );
    assert_eq!(server_wide(&other), before_server);
    assert_eq!(
        inside(&other, name),
        before_inside,
        "expected nothing made in the platform database that was there"
    );
}

#[test]
fn serve_against_the_harnesss_postgres_18_starts() {
    let vm = Vm::start_on(&Platform::fresh());
    let records = vm.records();
    assert!(
        records.iter().any(|r| r["event"] == "listening"),
        "expected a `listening` record"
    );
    assert!(
        !records
            .iter()
            .any(|r| r["event"] == "postgres-version-unsupported"),
        "expected no `postgres-version-unsupported` against Postgres 18"
    );
}

/// The number after `(PostgreSQL)` in what this `postgres --version` says.
fn version_line_number(bin: &Path) -> String {
    let out = Command::new(bin.join("postgres"))
        .arg("--version")
        .output()
        .unwrap();
    let line = String::from_utf8(out.stdout).unwrap();
    line.split("(PostgreSQL)")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("the harness's own check: a version line, got {line:?}"))
        .to_owned()
}

#[test]
fn a_local_run_with_vibe_local_postgres_bin_naming_a_postgres_17_is_postgres_unavailable() {
    let bin = &postgres::other_binaries().bin;
    let found = version_line_number(bin);
    assert!(
        found.starts_with(&format!("{}.", postgres::OTHER_MAJOR)),
        "the harness's own check: a Postgres 17, it says {found:?}"
    );
    let bin_text = bin.to_str().unwrap();
    let mirror = Mirror::start(Serving::Archive);
    let local = Local::new()
        .with_bin(bin_text)
        .with_mirror(&mirror.base_url());
    let project = local_project(&local.machine, "pg-17", &[]);
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    let message = run.json_error(1, "postgres-unavailable");
    let rest = message.replace(bin_text, "").replace(&found, "");
    assert!(
        message.replace(bin_text, "").contains(&found),
        "expected the message to say the version found, {found}, got {message:?}"
    );
    assert!(
        rest.contains(&postgres::MAJOR.to_string()),
        "expected the message to say Postgres {} is needed, got {message:?}",
        postgres::MAJOR
    );
    assert!(
        message.contains("VIBE_LOCAL_POSTGRES_BIN") && message.to_lowercase().contains("unset"),
        "expected the message to say to unset VIBE_LOCAL_POSTGRES_BIN, got {message:?}"
    );
    assert!(
        mirror.requests().is_empty(),
        "expected nothing downloaded in place of the Postgres named, got {:?}",
        mirror.requests()
    );
    assert!(
        local.processes().is_empty(),
        "expected no process of the run left, got {:?}",
        local.processes()
    );
}
