//! What the probe of a registration says about the logins (CONTRACT.md,
//! "Resources and Grants in full: Registering a database, checked"): the
//! warnings `read-login-can-write` (evidence `superuser`, `membership`,
//! `table`, `sequence`, `schema`, in that order, naming no table, schema or
//! role), `structure-login-can-read`, `login-sees-no-table` and
//! `probe-incomplete`; a clean read login and a structure login that holds
//! nothing give `[]`; the probe reads `pg_catalog` and nothing else.
//!
//! `probe-incomplete` is made by a lock the test holds on `pg_namespace` of
//! the shared database: the probe reads it, the connection does not need it
//! (the test checks that before it relies on it).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::resources::{
    Evidence, body, connection, limited_reader, login_holding_nothing, new_login, reader_body,
    reading_login, sent_by_login, writing_login,
};
use blackbox::scenario::Scenario;
use blackbox::server_log::log_everything;
use blackbox::shared_database::{Login, SharedDatabase, token};
use blackbox::sql::Sql;
use blackbox::{Response, assert_api_error};
use serde_json::{Value, json};

fn register(s: &Scenario, name: &str, connection: &Value) -> Response {
    s.admin().create_resource(&body(name, connection))
}

fn warnings(answer: &Response) -> Vec<Value> {
    answer.json()["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `warnings` in {}", answer.describe()))
        .clone()
}

fn evidence(warning: &Value) -> Vec<String> {
    warning["evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `evidence` in {warning}"))
        .iter()
        .map(|e| e.as_str().unwrap().to_owned())
        .collect()
}

/// The one warning of this reason and login, failing unless there is
/// exactly one warning in all.
fn the_only_warning(answer: &Response, reason: &str, login: &str) -> Value {
    answer.expect_status(201);
    let all = warnings(answer);
    assert_eq!(
        all.len(),
        1,
        "expected one warning, got {}",
        answer.describe()
    );
    assert_eq!(all[0]["reason"], reason, "{}", answer.describe());
    assert_eq!(all[0]["login"], login, "{}", answer.describe());
    all[0].clone()
}

fn assert_in_order(kinds: &[String]) {
    let canon: Vec<&str> = Evidence::ALL.iter().map(|e| e.word()).collect();
    let mut last = None;
    for kind in kinds {
        let at = canon
            .iter()
            .position(|c| c == kind)
            .unwrap_or_else(|| panic!("an evidence kind of the contract, got {kind:?}"));
        assert!(
            last.is_none_or(|l| at > l),
            "expected evidence in the order {canon:?}, each once, got {kinds:?}"
        );
        last = Some(at);
    }
}

fn names_nothing(answer: &Response, db: &SharedDatabase, logins: &[&Login]) {
    let text = answer.describe();
    for forbidden in [
        "incidents",
        "kinds",
        "notes",
        "staff_only",
        "ticket_numbers",
        "pg_write_all_data",
        "pg_read_all_data",
        db.name(),
    ] {
        assert!(
            !text.contains(forbidden),
            "expected no table, schema or role's name in the answer, found {forbidden:?}:\n{text}"
        );
    }
    for login in logins {
        assert!(!text.contains(&login.role), "no role's name in the answer");
        assert!(
            !text.contains(login.password()),
            "no password in the answer"
        );
    }
}

#[test]
fn a_read_login_that_can_write_is_a_warning_with_the_kind_of_evidence() {
    let s = Scenario::start();
    let db = &s.db;
    for kind in [Evidence::Table, Evidence::Sequence, Evidence::Schema] {
        let login = writing_login(db, &[kind]);
        let answer = register(
            &s,
            &format!("w_{}", kind.word()),
            &connection(db, &login, None, None),
        );
        answer.expect_status(201);
        let all = warnings(&answer);
        let first = all.first().cloned().unwrap_or(Value::Null);
        assert_eq!(
            first["reason"],
            "read-login-can-write",
            "{}",
            answer.describe()
        );
        assert_eq!(first["login"], "read", "{}", answer.describe());
        let warning = first;
        assert_eq!(
            evidence(&warning),
            vec![kind.word().to_owned()],
            "{}",
            answer.describe()
        );
        // A login with a privilege on a table sees one; with only a
        // sequence or a schema it sees none, which is a warning too.
        let others: Vec<&str> = all[1..]
            .iter()
            .map(|w| w["reason"].as_str().unwrap_or("?"))
            .collect();
        let expected: &[&str] = if kind == Evidence::Table {
            &[]
        } else {
            &["login-sees-no-table"]
        };
        assert_eq!(others, expected, "{}", answer.describe());
        let message = warning["message"].as_str().unwrap().to_lowercase();
        assert!(
            message.contains("read-only"),
            "expected the message to say `query` runs in a read-only transaction whatever the login may do, got {message:?}"
        );
        names_nothing(&answer, db, &[&login]);
    }
    // Membership of each of the three roles that can write beyond the
    // database: the evidence starts with `membership`.
    for (n, role) in [
        "pg_write_all_data",
        "pg_execute_server_program",
        "pg_write_server_files",
    ]
    .into_iter()
    .enumerate()
    {
        let login = blackbox::resources::new_login(db, "member", "");
        db.superuser()
            .batch(&format!("GRANT {role} TO {}", login.role));
        let answer = register(
            &s,
            &format!("member_{n}"),
            &connection(db, &login, None, None),
        );
        answer.expect_status(201);
        let all = warnings(&answer);
        let found: Vec<&Value> = all
            .iter()
            .filter(|w| w["reason"] == "read-login-can-write")
            .collect();
        assert_eq!(found.len(), 1, "{role}: {}", answer.describe());
        let kinds = evidence(found[0]);
        assert_eq!(kinds[0], "membership", "{role}: {kinds:?}");
        assert_in_order(&kinds);
        names_nothing(&answer, db, &[&login]);
    }
    // Through another role: a member of a member.
    let outer = blackbox::resources::new_login(db, "outer", "");
    let inner = blackbox::resources::new_login(db, "inner", "");
    db.superuser().batch(&format!(
        "GRANT pg_write_all_data TO {}; GRANT {} TO {}",
        inner.role, inner.role, outer.role
    ));
    let answer = register(&s, "m_through", &connection(db, &outer, None, None));
    let all = warnings(&answer);
    let found: Vec<&Value> = all
        .iter()
        .filter(|w| w["reason"] == "read-login-can-write")
        .collect();
    assert_eq!(found.len(), 1, "{}", answer.describe());
    assert_eq!(evidence(found[0])[0], "membership");
}

#[test]
fn a_superuser_read_login_gives_superuser_first_and_every_kind_is_named_once_in_order() {
    let s = Scenario::start();
    let db = &s.db;
    let login = writing_login(db, &[Evidence::Superuser]);
    let answer = register(&s, "super", &connection(db, &login, None, None));
    answer.expect_status(201);
    let all = warnings(&answer);
    let found: Vec<&Value> = all
        .iter()
        .filter(|w| w["reason"] == "read-login-can-write")
        .collect();
    assert_eq!(found.len(), 1, "{}", answer.describe());
    let kinds = evidence(found[0]);
    assert_eq!(kinds[0], "superuser", "{kinds:?}");
    assert_in_order(&kinds);
    assert_eq!(
        kinds,
        Evidence::ALL
            .iter()
            .map(|e| e.word().to_owned())
            .collect::<Vec<_>>(),
        "a superuser can do everything the other kinds name: {}",
        answer.describe()
    );
    names_nothing(&answer, db, &[&login]);
}

#[test]
fn several_kinds_are_named_together_in_the_order_of_the_contract() {
    let s = Scenario::start();
    let db = &s.db;
    // The shared database's own "careless" login: tables, a sequence and
    // `CREATE` on the schema.
    let answer = register(&s, "careless", &connection(db, &db.careless, None, None));
    let warning = the_only_warning(&answer, "read-login-can-write", "read");
    assert_eq!(
        evidence(&warning),
        vec!["table", "sequence", "schema"],
        "{}",
        answer.describe()
    );
    let login = writing_login(db, &[Evidence::Schema, Evidence::Table]);
    let answer = register(&s, "two", &connection(db, &login, None, None));
    let warning = the_only_warning(&answer, "read-login-can-write", "read");
    assert_eq!(evidence(&warning), vec!["table", "schema"]);
    let records = blackbox::resources::records_waited(&s.vm, "resource-login-warning", 2);
    assert_eq!(records.len(), 2, "one record for each warning: {records:?}");
    for record in &records {
        assert_eq!(record["login"], "read", "{record}");
        assert_eq!(record["reason"], "read-login-can-write", "{record}");
        assert!(
            record.get("evidence").is_none() || record["evidence"].is_array(),
            "{record}"
        );
    }
}

#[test]
fn a_write_login_that_can_write_is_not_a_warning_and_a_clean_read_login_has_none() {
    let s = Scenario::start();
    let db = &s.db;
    let nothing = login_holding_nothing(db);
    let answer = register(
        &s,
        "clean",
        &connection(db, &db.reader, Some(&db.careless), Some(&nothing)),
    );
    answer.expect_status(201);
    assert_eq!(
        warnings(&answer),
        Vec::<Value>::new(),
        "{}",
        answer.describe()
    );
    let limited = limited_reader(db, &["incidents"]);
    let answer = register(&s, "limited", &connection(db, &limited, None, None));
    answer.expect_status(201);
    assert_eq!(
        warnings(&answer),
        Vec::<Value>::new(),
        "{}",
        answer.describe()
    );
    // The same in the answer of a read.
    let listed = s.admin().get_resource("clean");
    listed.expect_status(200);
    assert!(
        listed.json().get("warnings").is_none(),
        "warnings are the registration's answer only"
    );
}

#[test]
fn a_structure_login_that_can_read_rows_is_a_warning() {
    let s = Scenario::start();
    let db = &s.db;
    let login = reading_login(db, &[Evidence::Table]);
    let answer = register(
        &s,
        "reads_table",
        &connection(db, &db.reader, None, Some(&login)),
    );
    let warning = the_only_warning(&answer, "structure-login-can-read", "structure");
    assert_eq!(evidence(&warning), vec!["table"], "{}", answer.describe());
    let message = warning["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("structure") && message.contains("rows"),
        "expected the message to say the structure login is there so that pulls never use a login that can read rows, got {message:?}"
    );
    names_nothing(&answer, db, &[&login]);
    let member = reading_login(db, &[Evidence::Membership]);
    let answer = register(
        &s,
        "reads_member",
        &connection(db, &db.reader, None, Some(&member)),
    );
    answer.expect_status(201);
    let all = warnings(&answer);
    let found: Vec<&Value> = all
        .iter()
        .filter(|w| w["reason"] == "structure-login-can-read")
        .collect();
    assert_eq!(found.len(), 1, "{}", answer.describe());
    let kinds = evidence(found[0]);
    assert_eq!(kinds[0], "membership", "{kinds:?}");
    let superuser = reading_login(db, &[Evidence::Superuser]);
    let answer = register(
        &s,
        "reads_super",
        &connection(db, &db.reader, None, Some(&superuser)),
    );
    answer.expect_status(201);
    let all = warnings(&answer);
    let found: Vec<&Value> = all
        .iter()
        .filter(|w| w["reason"] == "structure-login-can-read")
        .collect();
    assert_eq!(found.len(), 1, "{}", answer.describe());
    assert_eq!(evidence(found[0])[0], "superuser");
    // A structure login that holds nothing, and a write login that reads,
    // are not warned about.
    let nothing = login_holding_nothing(db);
    let answer = register(
        &s,
        "structure_nothing",
        &connection(db, &db.reader, Some(&db.writer), Some(&nothing)),
    );
    answer.expect_status(201);
    assert_eq!(warnings(&answer), Vec::<Value>::new());
}

#[test]
fn a_read_login_that_reaches_no_table_is_a_warning_unless_the_database_has_none() {
    let s = Scenario::start();
    let db = &s.db;
    let nothing = login_holding_nothing(db);
    let answer = register(&s, "sees_none", &connection(db, &nothing, None, None));
    let warning = the_only_warning(&answer, "login-sees-no-table", "read");
    assert!(
        warning.get("evidence").is_none(),
        "login-sees-no-table has no evidence: {warning}"
    );
    assert!(warning["message"].as_str().is_some_and(|m| !m.is_empty()));
    names_nothing(&answer, db, &[&nothing]);
    // The write login holding something does not change it: it is the read
    // login that is looked at.
    let answer = register(
        &s,
        "sees_none_write",
        &connection(db, &nothing, Some(&db.writer), None),
    );
    the_only_warning(&answer, "login-sees-no-table", "read");
    // A structure login that holds nothing is never warned about.
    let answer = register(
        &s,
        "structure_only_nothing",
        &connection(db, &db.reader, None, Some(&nothing)),
    );
    assert_eq!(warnings(&answer), Vec::<Value>::new());
    // A database with no table of its own: nothing to see, nothing said.
    let server = Sql::connect(&db.server().url("postgres")).unwrap();
    let empty = format!("empty_db_{}", token());
    server.batch(&format!("CREATE DATABASE {empty}"));
    let mut c = connection(db, &nothing, None, None);
    c["database"] = json!(empty);
    server.batch(&format!(
        "GRANT CONNECT ON DATABASE {empty} TO {}",
        nothing.role
    ));
    let answer = register(&s, "empty", &c);
    answer.expect_status(201);
    assert_eq!(
        warnings(&answer),
        Vec::<Value>::new(),
        "{}",
        answer.describe()
    );
}

#[test]
fn a_probe_that_does_not_finish_is_a_warning_and_the_others_are_not_said() {
    let s = Scenario::start();
    let db = &s.db;
    let nothing = login_holding_nothing(db);
    // The rig: a lock on `pg_namespace` held by the test. Before relying on
    // it, a login of the shared database must still connect and a read of
    // `pg_namespace` must wait.
    let holder = db.superuser();
    holder.batch("BEGIN");
    holder.batch("LOCK TABLE pg_catalog.pg_namespace IN ACCESS EXCLUSIVE MODE");
    let probe = db.connect_as(&db.reader);
    probe.batch("SET statement_timeout = '700ms'");
    let began = Instant::now();
    let waited = probe.try_batch("SELECT count(*) FROM pg_catalog.pg_namespace");
    assert!(
        waited.is_err() && began.elapsed() >= Duration::from_millis(500),
        "the harness's own check: a read of pg_namespace must wait while the lock is held (got {waited:?} after {:?})",
        began.elapsed()
    );
    drop(probe);
    let began = Instant::now();
    // `careless` can write and would be warned about; with the probe
    // unfinished it is not.
    let answer = register(
        &s,
        "unfinished",
        &connection(db, &db.careless, None, Some(&nothing)),
    );
    let took = began.elapsed();
    holder.batch("ROLLBACK");
    answer.expect_status(201);
    let all = warnings(&answer);
    let mut seen: Vec<(String, String)> = all
        .iter()
        .map(|w| {
            (
                w["reason"].as_str().unwrap().to_owned(),
                w["login"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("probe-incomplete".to_owned(), "read".to_owned()),
            ("probe-incomplete".to_owned(), "structure".to_owned())
        ],
        "expected only `probe-incomplete` for the read and the structure login: {}",
        answer.describe()
    );
    for warning in &all {
        assert!(warning["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert!(warning.get("evidence").is_none(), "{warning}");
    }
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(12),
        "expected the probes to be given up at 5 seconds, together; took {took:?}"
    );
    // Nothing else was lost: the Resource is registered and served.
    s.admin().get_resource("unfinished").expect_status(200);
}

#[test]
fn the_probe_reads_pg_catalog_and_nothing_else_even_for_a_login_that_can_write() {
    let s = Scenario::start();
    let db = &s.db;
    log_everything(db.server());
    let nothing = login_holding_nothing(db);
    let answer = register(
        &s,
        "catalog",
        &connection(db, &db.careless, Some(&db.writer), Some(&nothing)),
    );
    answer.expect_status(201);
    for login in [&db.careless, &nothing] {
        let sent = sent_by_login(db, login);
        assert!(
            !sent.is_empty(),
            "expected the probe on the {} login in the server's log",
            login.role
        );
        for statement in sent {
            let lowered = statement.sql.to_lowercase();
            let words: Vec<&str> = lowered.split_whitespace().collect();
            for pair in words.windows(2) {
                if pair[0] == "from" || pair[0] == "join" {
                    let target = pair[1].trim_start_matches('(');
                    assert!(
                        target.is_empty()
                            || target.starts_with("pg_")
                            || target.starts_with("select")
                            || target.starts_with("unnest")
                            || target.starts_with("lateral")
                            || target.starts_with("generate_series"),
                        "expected the probe to read pg_catalog only, found `{} {}` in {:?}",
                        pair[0],
                        pair[1],
                        statement.sql
                    );
                }
            }
            assert!(
                !lowered.contains("information_schema"),
                "{:?}",
                statement.sql
            );
            assert!(
                !lowered.contains("pg_stat"),
                "no statistics: {:?}",
                statement.sql
            );
        }
    }
}

#[test]
fn nothing_is_refused_for_a_warning_and_a_refusal_has_no_warnings() {
    let s = Scenario::start();
    let db = &s.db;
    let bad = reader_body("bad", db);
    let mut bad = bad;
    bad["connection"]["read"]["password"] = json!("not-the-password");
    let answer = s.admin().create_resource(&bad);
    assert_api_error(&answer, 422, "login-failed");
    assert!(
        answer.json().get("warnings").is_none(),
        "a refusal says what failed, not warnings: {}",
        answer.describe()
    );
}

/// The warnings of this reason in an answer.
fn warnings_of(answer: &Response, reason: &str) -> Vec<Value> {
    warnings(answer)
        .into_iter()
        .filter(|w| w["reason"] == reason)
        .collect()
}

/// A structure login that holds `grant` (a privilege and its object, as
/// `INSERT ON incidents`) and nothing else.
fn structure_holding(db: &SharedDatabase, grant: &str) -> Login {
    let login = new_login(db, "struct_priv", "");
    db.superuser().batch(&format!(
        "GRANT USAGE ON SCHEMA public TO {role}; GRANT {grant} TO {role}",
        role = login.role
    ));
    login
}

#[test]
fn a_structure_login_that_can_only_write_a_table_is_not_a_structure_login_that_can_read() {
    let s = Scenario::start();
    let db = &s.db;
    for (n, grant) in [
        "INSERT ON incidents",
        "UPDATE (title) ON incidents",
        "INSERT (title) ON incidents",
        "DELETE ON incidents",
    ]
    .iter()
    .enumerate()
    {
        let login = structure_holding(db, grant);
        let answer = register(
            &s,
            &format!("writes_only_{n}"),
            &connection(db, &db.reader, None, Some(&login)),
        );
        answer.expect_status(201);
        assert_eq!(
            warnings_of(&answer, "structure-login-can-read"),
            Vec::<Value>::new(),
            "expected no `structure-login-can-read` for a structure login holding only `{grant}`: {}",
            answer.describe()
        );
    }
}

#[test]
fn a_structure_login_that_can_select_one_column_only_is_a_structure_login_that_can_read() {
    let s = Scenario::start();
    let db = &s.db;
    let login = structure_holding(db, "SELECT (title) ON incidents");
    let answer = register(
        &s,
        "reads_a_column",
        &connection(db, &db.reader, None, Some(&login)),
    );
    let warning = the_only_warning(&answer, "structure-login-can-read", "structure");
    assert_eq!(evidence(&warning), vec!["table"], "{}", answer.describe());
    names_nothing(&answer, db, &[&login]);
}

/// A role that takes 3 seconds to connect (a `login` event trigger of the
/// shared database that sleeps for this role's sessions only) and reads
/// `incidents`.
fn slow_to_connect(db: &SharedDatabase) -> Login {
    let login = new_login(db, "slow", "");
    let t = token();
    db.superuser().batch(&format!(
        "GRANT USAGE ON SCHEMA public TO {role};
         GRANT SELECT ON incidents TO {role};
         CREATE FUNCTION slow_login_{t}() RETURNS event_trigger LANGUAGE plpgsql AS $f$
         BEGIN
           IF session_user = '{role}' THEN PERFORM pg_sleep(3); END IF;
         END
         $f$;
         CREATE EVENT TRIGGER slow_login_{t} ON login EXECUTE FUNCTION slow_login_{t}()",
        role = login.role
    ));
    login
}

#[test]
fn a_login_that_connects_late_and_whose_probe_never_ends_is_probe_incomplete_not_timed_out() {
    let s = Scenario::start();
    let db = &s.db;
    let slow = slow_to_connect(db);
    // The harness's own check, with no lock held: the slow role connects
    // in about 3 seconds, and another login at once.
    let began = Instant::now();
    let connected = Sql::connect(&db.url_as(&slow));
    let took = began.elapsed();
    assert!(
        connected.is_ok() && took >= Duration::from_millis(2500) && took < Duration::from_secs(5),
        "the harness's own check: expected the slow role to connect in about 3 s (got {:?} after {took:?})",
        connected.as_ref().err()
    );
    drop(connected);
    let began = Instant::now();
    assert!(
        Sql::connect(&db.url_as(&db.writer)).is_ok() && began.elapsed() < Duration::from_secs(2),
        "the harness's own check: another login connects at once"
    );
    let holder = db.superuser();
    let began = Instant::now();
    let answer = std::thread::scope(|scope| {
        let request = scope.spawn(|| {
            register(
                &s,
                "late_probe",
                &connection(db, &slow, Some(&db.writer), None),
            )
        });
        // The slow login is inside its 3-second sleep by now; take the lock
        // then, so that only its probe, which reads `pg_namespace`, hangs.
        std::thread::sleep(Duration::from_millis(1500));
        holder.batch("BEGIN");
        holder.batch("LOCK TABLE pg_catalog.pg_namespace IN ACCESS EXCLUSIVE MODE");
        let answer = request.join().expect("the request thread ends");
        holder.batch("ROLLBACK");
        answer
    });
    let took = began.elapsed();
    answer.expect_status(201);
    let all = warnings(&answer);
    assert_eq!(
        all.len(),
        1,
        "expected one warning, `probe-incomplete` for the read login: {}",
        answer.describe()
    );
    assert_eq!(
        all[0]["reason"],
        "probe-incomplete",
        "{}",
        answer.describe()
    );
    assert_eq!(all[0]["login"], "read", "{}", answer.describe());
    assert!(
        answer.json().get("failed").is_none(),
        "no login is named failed: {}",
        answer.describe()
    );
    assert!(
        took >= Duration::from_secs(7) && took < Duration::from_secs(11),
        "expected the probe given up 5 seconds after the login connected (3 + 5 seconds), within 10 seconds of the request plus a small margin; it took {took:?}"
    );
    s.admin().get_resource("late_probe").expect_status(200);
}
