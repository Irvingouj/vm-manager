//! Registering a database Resource is checked (CONTRACT.md, "Resources and
//! Grants in full: Registering a database, checked", and its bounds):
//!
//! - every login supplied is connected before anything is stored, and the
//!   probe on the read and the structure login reads `pg_catalog` only;
//! - a login that cannot be used is `422` `login-failed` with `failed`
//!   naming every failing login, in the order read, write, structure, each
//!   `why` one of `unreachable`, `refused`, `no-such-database`, `timed-out`;
//!   nothing is stored; no message, answer or log record holds the host,
//!   port, database, role or password;
//! - the order of refusals, the bounds of every field, the 64 KiB body,
//!   `"warnings": []` always there (a Storage registration too);
//! - the log records, and at most two checks at once (a third is `503`
//!   `busy`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::lost_answer::HeldPostgres;
use blackbox::resources::{
    Hanging, body, connection, connections_of, found_in, login_holding_nothing,
    login_that_may_not_connect, new_login, reader_body, secrets_of, sent_by_login,
    with_wrong_password,
};
use blackbox::scenario::{Scenario, administrator};
use blackbox::server_log::log_everything;
use blackbox::shared_database::{Login, SharedDatabase, find_port, token};
use blackbox::sql::Sql;
use blackbox::storage::{MARKETING, Storage, storage_body};
use blackbox::{Response, VmOptions, assert_api_error, postgres};
use serde_json::{Value, json};

fn post(s: &Scenario, body: &Value) -> Response {
    s.admin().create_resource(body)
}

/// `failed` of a `422` as `(login, why)`.
fn failed_of(answer: &Response) -> Vec<(String, String)> {
    answer.json()["error"]["failed"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `failed` to be a list, got {}", answer.describe()))
        .iter()
        .map(|f| {
            (
                f["login"].as_str().unwrap_or("?").to_owned(),
                f["why"].as_str().unwrap_or("?").to_owned(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

/// How many rows the platform database has for this Resource's name.
fn rows_of(s: &Scenario, name: &str) -> String {
    let platform = Sql::connect(s.vm.platform().url()).unwrap();
    platform
        .value(&format!(
            "SELECT count(*) FROM resources WHERE name = '{name}'"
        ))
        .unwrap_or_default()
}

/// Fails unless nothing of `name` is stored: the Resource is not there, the
/// list lacks it, the platform database has no row.
fn assert_nothing_stored(s: &Scenario, name: &str, context: &str) {
    s.admin().get_resource(name).expect_status(404);
    let listed = s.admin().list_resources().json();
    let names: Vec<&str> = listed["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        !names.contains(&name),
        "{context}: expected the list to lack {name:?}, got {names:?}"
    );
    assert_eq!(
        rows_of(s, name),
        "0",
        "{context}: expected no row for {name:?} in the platform database"
    );
}

/// Every text the program said that a connection's values may not be in:
/// the answers given and every line of its log.
fn assert_holds_nothing(
    s: &Scenario,
    db: &SharedDatabase,
    logins: &[&Login],
    answers: &[&Response],
    context: &str,
) {
    let secrets = secrets_of(db, logins);
    let mut texts: Vec<(String, String)> = answers
        .iter()
        .map(|a| (format!("the answer to {}", a.request()), a.describe()))
        .collect();
    texts.extend(
        s.vm.lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    for (whence, text) in &texts {
        let found = found_in(&secrets, text);
        assert!(
            found.is_empty(),
            "{context}: expected {whence} to hold nothing of the connection, but it holds {found:?}:\n{text}"
        );
        assert!(
            !find_port(text, db.port()),
            "{context}: expected {whence} not to hold the port:\n{text}"
        );
    }
}

fn records_of(s: &Scenario, event: &str, expected: usize) -> Vec<Value> {
    blackbox::resources::records_waited(&s.vm, event, expected)
}

#[test]
fn every_login_is_connected_and_a_clean_registration_has_no_warnings() {
    let s = Scenario::start();
    let db = &s.db;
    log_everything(db.server());
    let structure = login_holding_nothing(db);
    let before = db.fingerprint();
    let answer = post(
        &s,
        &body(
            "devops",
            &connection(db, &db.reader, Some(&db.writer), Some(&structure)),
        ),
    );
    answer.expect_status(201);
    assert_eq!(
        answer.json()["warnings"],
        json!([]),
        "expected `warnings` to be there and empty: {}",
        answer.describe()
    );
    for (what, login) in [
        ("read", &db.reader),
        ("write", &db.writer),
        ("structure", &structure),
    ] {
        assert!(
            connections_of(db, login) >= 1,
            "expected the {what} login to have been connected to the shared database"
        );
    }
    assert_eq!(
        before,
        db.fingerprint(),
        "expected nothing installed or changed in the shared database"
    );
    let record = records_of(&s, "resource-registered", 1);
    assert_eq!(record.len(), 1, "{record:?}");
    assert_eq!(record[0]["resource"], "devops");
    assert_eq!(record[0]["kind"], "postgres");
    assert_eq!(record[0]["by"], administrator().id);
    assert!(
        records_of(&s, "resource-login-failed", 0).is_empty()
            && records_of(&s, "resource-login-warning", 0).is_empty(),
        "expected no record of a failed login or a warning"
    );
}

#[test]
fn the_server_sees_the_connections_and_the_probe_and_nothing_else() {
    let s = Scenario::start();
    let db = &s.db;
    log_everything(db.server());
    let structure = login_holding_nothing(db);
    post(
        &s,
        &body(
            "devops",
            &connection(db, &db.reader, Some(&db.writer), Some(&structure)),
        ),
    )
    .expect_status(201);
    let all_roles = [&db.reader, &db.writer, &structure];
    for login in all_roles {
        let sent = sent_by_login(db, login);
        let text: String = sent
            .iter()
            .map(|x| x.sql.to_lowercase())
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "insert ",
            "update ",
            "delete ",
            "create ",
            "alter ",
            "drop ",
            "grant ",
            "truncate",
            "incidents",
            "kinds",
            "staff_only",
            "notes",
            "information_schema",
            "pg_stat",
            "from public.",
        ] {
            assert!(
                !text.contains(forbidden),
                "expected the statements of the check on {} to read pg_catalog only, found {forbidden:?} in:\n{text}",
                login.role
            );
        }
        assert!(
            !text.contains(&login.role.to_lowercase()),
            "expected the role to be a parameter of the probe, not in its text:\n{text}"
        );
        for statement in &sent {
            let lowered = statement.sql.to_lowercase();
            assert!(
                lowered.contains("pg_")
                    || lowered.starts_with("begin")
                    || lowered.starts_with("commit")
                    || lowered.starts_with("rollback")
                    || lowered.starts_with("set ")
                    || lowered.starts_with("select 1")
                    || lowered.starts_with("show"),
                "expected only the probe and the transaction around it, got {:?}",
                statement.sql
            );
        }
    }
    // The probe is on the read and the structure login, in a read-only
    // transaction; the write login is connected and asked nothing of
    // pg_catalog.
    for login in [&db.reader, &structure] {
        let text: String = sent_by_login(db, login)
            .iter()
            .map(|x| x.sql.to_lowercase())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("pg_roles") && text.contains("pg_class"),
            "expected the probe on {} to read pg_roles and pg_class, statements:\n{text}",
            login.role
        );
        assert!(
            text.contains("read only"),
            "expected the probe on {} in a read-only transaction, statements:\n{text}",
            login.role
        );
    }
    let write_text: String = sent_by_login(db, &db.writer)
        .iter()
        .map(|x| x.sql.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !write_text.contains("pg_roles"),
        "expected no probe on the write login, statements:\n{write_text}"
    );
}

/// What each way of failing is, with the connection to register: the
/// reader as read login against a database that behaves so.
struct Case {
    why: &'static str,
    connection: Value,
}

fn cases(db: &SharedDatabase) -> Vec<Case> {
    let mut nowhere = connection(db, &db.reader, None, None);
    nowhere["port"] = json!(postgres::free_port().unwrap());
    let mut no_database = connection(db, &db.reader, None, None);
    no_database["database"] = json!(format!("no_such_db_{}", token()));
    let wrong = with_wrong_password(&db.reader);
    let shut = login_that_may_not_connect(db);
    let missing_role = Login::new(&format!("no_such_role_{}", token()), "whatever-password");
    vec![
        Case {
            why: "unreachable",
            connection: nowhere,
        },
        Case {
            why: "refused",
            connection: connection(db, &wrong, None, None),
        },
        Case {
            why: "refused",
            connection: connection(db, &shut, None, None),
        },
        Case {
            why: "refused",
            connection: connection(db, &missing_role, None, None),
        },
        Case {
            why: "no-such-database",
            connection: no_database,
        },
    ]
}

#[test]
fn a_login_that_cannot_be_used_is_422_naming_why_and_nothing_is_stored() {
    let s = Scenario::start();
    let db = &s.db;
    let before = db.fingerprint();
    let mut answers: Vec<Response> = Vec::new();
    for (n, case) in cases(db).into_iter().enumerate() {
        let name = format!("refused_{n}");
        let answer = post(&s, &body(&name, &case.connection));
        assert_api_error(&answer, 422, "login-failed");
        assert_eq!(
            failed_of(&answer),
            pairs(&[("read", case.why)]),
            "case {n}: {}",
            answer.describe()
        );
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            message.contains("read"),
            "expected the message to say which login failed, got {message:?}"
        );
        assert_nothing_stored(&s, &name, &format!("case {n}"));
        answers.push(answer);
    }
    // The name of a refused registration is free.
    post(&s, &reader_body("refused_0", db)).expect_status(201);
    assert_eq!(before, db.fingerprint(), "the shared database is untouched");
    let failed_records = records_of(&s, "resource-login-failed", 5);
    assert_eq!(failed_records.len(), 5, "{failed_records:?}");
    for record in &failed_records {
        assert_eq!(record["login"], "read", "{record}");
        assert!(
            ["unreachable", "refused", "no-such-database"]
                .contains(&record["why"].as_str().unwrap_or("")),
            "{record}"
        );
        assert!(record["resource"].as_str().unwrap().starts_with("refused_"));
    }
    assert!(
        records_of(&s, "resource-registered", 1)
            .iter()
            .all(|r| r["resource"] == "refused_0"),
        "expected `resource-registered` only for the registration that succeeded"
    );
    let refs: Vec<&Response> = answers.iter().collect();
    // Every role the cases used: the reader, and those the cases made.
    assert_holds_nothing(&s, db, &[&db.reader], &refs, "refused registrations");
}

#[test]
fn every_failing_login_is_named_in_the_order_read_write_structure() {
    let s = Scenario::start();
    let db = &s.db;
    let good = &db.reader;
    let bad_write = with_wrong_password(&db.writer);
    let bad_structure = login_that_may_not_connect(db);
    let bad_read = with_wrong_password(&db.reader);
    let nothing = login_holding_nothing(db);
    let cases: Vec<(Value, Vec<(&str, &str)>)> = vec![
        (
            connection(db, good, Some(&bad_write), Some(&nothing)),
            vec![("write", "refused")],
        ),
        (
            connection(db, good, Some(&db.writer), Some(&bad_structure)),
            vec![("structure", "refused")],
        ),
        (
            connection(db, good, Some(&bad_write), Some(&bad_structure)),
            vec![("write", "refused"), ("structure", "refused")],
        ),
        (
            connection(db, &bad_read, Some(&bad_write), Some(&bad_structure)),
            vec![
                ("read", "refused"),
                ("write", "refused"),
                ("structure", "refused"),
            ],
        ),
        (
            connection(db, &bad_read, Some(&db.writer), Some(&nothing)),
            vec![("read", "refused")],
        ),
    ];
    for (n, (connection, expected)) in cases.into_iter().enumerate() {
        let name = format!("many_{n}");
        let answer = post(&s, &body(&name, &connection));
        assert_api_error(&answer, 422, "login-failed");
        assert_eq!(
            failed_of(&answer),
            pairs(&expected),
            "{}",
            answer.describe()
        );
        assert_nothing_stored(&s, &name, &format!("case {n}"));
    }
    // The same failure on every login when the database is the failing
    // part: all three, in order.
    let mut gone = connection(db, good, Some(&db.writer), Some(&nothing));
    gone["database"] = json!(format!("no_such_db_{}", token()));
    let answer = post(&s, &body("many_gone", &gone));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        failed_of(&answer),
        pairs(&[
            ("read", "no-such-database"),
            ("write", "no-such-database"),
            ("structure", "no-such-database")
        ])
    );
    let mut unreachable = connection(db, good, Some(&db.writer), Some(&nothing));
    unreachable["port"] = json!(postgres::free_port().unwrap());
    let answer = post(&s, &body("many_unreachable", &unreachable));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        failed_of(&answer),
        pairs(&[
            ("read", "unreachable"),
            ("write", "unreachable"),
            ("structure", "unreachable")
        ])
    );
}

#[test]
fn a_server_that_accepts_and_never_answers_is_timed_out_within_ten_seconds_for_every_login_at_once()
{
    let s = Scenario::start();
    let db = &s.db;
    let hanging = Hanging::start();
    let nothing = login_holding_nothing(db);
    let mut hung = connection(db, &db.reader, Some(&db.writer), Some(&nothing));
    hung["host"] = json!(hanging.host());
    hung["port"] = json!(hanging.port());
    let began = Instant::now();
    let answer = post(&s, &body("hung", &hung));
    let took = began.elapsed();
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        failed_of(&answer),
        pairs(&[
            ("read", "timed-out"),
            ("write", "timed-out"),
            ("structure", "timed-out")
        ])
    );
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(14),
        "expected the whole check to be given up at 10 seconds with the three at the same time, it took {took:?}"
    );
    assert_nothing_stored(&s, "hung", "a hanging server");
    assert!(
        !answer.describe().contains(&hanging.port().to_string()),
        "no port in the answer"
    );
}

#[test]
fn a_registration_that_fails_its_check_is_answered_while_the_platform_database_holds_commits() {
    let held = HeldPostgres::start();
    let platform = held.platform("vm_platform_register_checked");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    let db = &s.db;
    held.hold();
    let began = Instant::now();
    let answer = post(
        &s,
        &body(
            "late",
            &connection(db, &with_wrong_password(&db.reader), None, None),
        ),
    );
    assert_api_error(&answer, 422, "login-failed");
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "expected the check to refuse before any write to the platform database, took {:?}",
        began.elapsed()
    );
    held.let_through();
}

#[test]
fn the_order_of_refusals_is_malformed_then_no_key_then_taken_then_the_checks() {
    // No key: a malformed body and a bad name are refused first, then the
    // key, and a name taken comes before the checks.
    let keyless = Scenario::start_with(&VmOptions::new().without_secret_key());
    let good = reader_body("devops", &keyless.db);
    let mut malformed = good.clone();
    malformed["description"] = json!(7);
    assert_api_error(&post(&keyless, &malformed), 400, "malformed");
    let mut bad_name = good.clone();
    bad_name["name"] = json!("Not Valid");
    assert_api_error(&post(&keyless, &bad_name), 400, "invalid-name");
    assert_api_error(&post(&keyless, &good), 409, "no-secret-key");
    let mut unreachable = good;
    unreachable["connection"]["port"] = json!(postgres::free_port().unwrap());
    assert_api_error(&post(&keyless, &unreachable), 409, "no-secret-key");

    // With the key: taken before the checks, the checks before the write.
    let s = Scenario::start();
    post(&s, &reader_body("devops", &s.db)).expect_status(201);
    let mut taken = reader_body("devops", &s.db);
    taken["connection"]["port"] = json!(postgres::free_port().unwrap());
    assert_api_error(&post(&s, &taken), 409, "name-taken");
    let mut bad_and_taken = taken.clone();
    bad_and_taken["description"] = json!(7);
    assert_api_error(&post(&s, &bad_and_taken), 400, "malformed");
    let mut bad_and_unreachable = reader_body("other", &s.db);
    bad_and_unreachable["connection"]["port"] = json!(postgres::free_port().unwrap());
    bad_and_unreachable["owner"] = json!("");
    assert_api_error(&post(&s, &bad_and_unreachable), 400, "malformed");
}

/// "What the fields take": a refusal is `400` `malformed`, names the field,
/// never echoes what was sent, and registers nothing.
#[test]
fn a_text_field_past_its_bound_or_with_a_control_character_is_malformed_naming_it_and_never_echoing_it()
 {
    let s = Scenario::start();
    let cases: Vec<(&str, String)> = vec![
        ("contact", format!("{}-contact-marker", "c".repeat(250))),
        ("owner", String::new()),
        (
            "description",
            format!("{}-description-marker", "d".repeat(2040)),
        ),
        ("description", "mark-desc\u{7}er-bell".to_owned()),
        ("description", "mark-desc\u{1b}[2Jer-escape".to_owned()),
        ("owner", "mark-own\u{1b}[2Jer-escape".to_owned()),
        ("contact", "mark-con\u{7f}er-delete".to_owned()),
        ("contact", "mark-con\ner-line-end".to_owned()),
    ];
    for (n, (field, value)) in cases.iter().enumerate() {
        let name = format!("bounded_{n}");
        let mut body = reader_body(&name, &s.db);
        body[*field] = json!(value);
        let answer = post(&s, &body);
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.to_lowercase().contains(field),
            "expected the message to name `{field}`, got {message:?}"
        );
        for part in ["marker", "mark-", "-contact-", "cccc", "dddd"] {
            assert!(
                !message.contains(part),
                "the message must not echo what was sent ({part:?}), got {message:?}"
            );
        }
        assert_nothing_stored(&s, &name, field);
    }
}

#[test]
fn the_fields_take_their_bounds_at_the_edge_and_refuse_past_it() {
    let s = Scenario::start();
    let db = &s.db;
    let mut n = 0;
    let mut next = || {
        n += 1;
        format!("edge_{n}")
    };
    // A message names the field and never what was sent.
    let refused = |s: &Scenario, name: &str, mutate: &dyn Fn(&mut Value), field: &str| {
        let mut b = reader_body(name, &s.db);
        mutate(&mut b);
        let answer = post(s, &b);
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            message.contains(field),
            "expected the message to name `{field}`, got {message:?}"
        );
        assert_nothing_stored(s, name, field);
    };
    // description: 2,048 bytes at most; a line end is allowed, a control
    // character or a NUL is not.
    let mut b = reader_body(&next(), db);
    b["description"] = json!("d".repeat(2048));
    post(&s, &b).expect_status(201);
    let mut b = reader_body(&next(), db);
    b["description"] = json!("one line\nanother line");
    post(&s, &b).expect_status(201);
    let mut b = reader_body(&next(), db);
    b["description"] = json!("");
    post(&s, &b).expect_status(201);
    refused(
        &s,
        &next(),
        &|b| b["description"] = json!("d".repeat(2049)),
        "description",
    );
    refused(
        &s,
        &next(),
        &|b| b["description"] = json!("a\u{0}b"),
        "description",
    );
    refused(
        &s,
        &next(),
        &|b| b["description"] = json!("a\u{7}b"),
        "description",
    );
    // owner and contact: 1 to 256 bytes.
    for field in ["owner", "contact"] {
        let mut b = reader_body(&next(), db);
        b[field] = json!("o".repeat(256));
        post(&s, &b).expect_status(201);
        let mut b = reader_body(&next(), db);
        b[field] = json!("o");
        post(&s, &b).expect_status(201);
        refused(&s, &next(), &|b| b[field] = json!("o".repeat(257)), field);
        refused(&s, &next(), &|b| b[field] = json!(""), field);
        refused(&s, &next(), &|b| b[field] = json!("a\u{0}b"), field);
        // 257 bytes in 129 characters: bytes are counted.
        refused(&s, &next(), &|b| b[field] = json!("é".repeat(129)), field);
    }
    // host: 1 to 253 bytes (an edge host does not resolve: it is refused
    // for that, not as malformed), a NUL never.
    let mut b = reader_body(&next(), db);
    b["connection"]["host"] = json!(format!("{}.example.test", "h".repeat(60)));
    assert_ne!(
        post(&s, &b).status,
        400,
        "a host within its bound is not malformed"
    );
    let long_host = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    assert_eq!(long_host.len(), 253);
    let mut b = reader_body(&next(), db);
    b["connection"]["host"] = json!(long_host);
    assert_ne!(post(&s, &b).status, 400, "253 bytes is within the bound");
    refused(
        &s,
        &next(),
        &|b| b["connection"]["host"] = json!("h".repeat(254)),
        "host",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["host"] = json!(""),
        "host",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["host"] = json!("a\u{0}b"),
        "host",
    );
    // port: 1 to 65535.
    for port in [0, 65536, -1, 100_000] {
        refused(
            &s,
            &next(),
            &|b| b["connection"]["port"] = json!(port),
            "port",
        );
    }
    for port in [1, 65535] {
        let mut b = reader_body(&next(), db);
        b["connection"]["port"] = json!(port);
        assert_ne!(post(&s, &b).status, 400, "port {port} is within the bound");
    }
    // database and role: 1 to 63 bytes; password: 1 to 1,024.
    refused(
        &s,
        &next(),
        &|b| b["connection"]["database"] = json!("d".repeat(64)),
        "database",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["database"] = json!(""),
        "database",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["database"] = json!("a\u{0}b"),
        "database",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["role"] = json!("r".repeat(64)),
        "role",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["role"] = json!(""),
        "role",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["role"] = json!("a\u{0}b"),
        "role",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["password"] = json!("p".repeat(1025)),
        "password",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["password"] = json!(""),
        "password",
    );
    refused(
        &s,
        &next(),
        &|b| b["connection"]["read"]["password"] = json!("a\u{0}b"),
        "password",
    );
    for member in ["write", "structure"] {
        refused(
            &s,
            &next(),
            &|b| b["connection"][member] = json!({ "role": "r".repeat(64), "password": "x" }),
            "role",
        );
        refused(
            &s,
            &next(),
            &|b| b["connection"][member] = json!({ "role": "r", "password": "p".repeat(1025) }),
            "password",
        );
    }
}

/// A database, a role and a password each at the longest the bound allows,
/// made at the server, and the connection to them.
#[test]
fn a_database_a_role_and_a_password_at_their_longest_connect_and_register() {
    let s = Scenario::start();
    let db = &s.db;
    let t = token();
    let database = format!("{}{t}", "d".repeat(63 - t.len()));
    let role = format!("{}{t}", "r".repeat(63 - t.len()));
    let password = format!("{}{t}", "p".repeat(1024 - t.len()));
    assert_eq!((database.len(), role.len(), password.len()), (63, 63, 1024));
    let server = Sql::connect(&db.server().url("postgres")).unwrap();
    server.batch(&format!("CREATE ROLE {role} LOGIN PASSWORD '{password}'"));
    server.batch(&format!("CREATE DATABASE {database}"));
    let answer = post(
        &s,
        &json!({
            "name": "longest", "kind": "postgres", "description": "x", "owner": "o", "contact": "c",
            "connection": {
                "host": db.host(), "port": db.port(), "database": database,
                "read": { "role": role, "password": password },
            },
        }),
    );
    answer.expect_status(201);
    assert_eq!(
        answer.json()["warnings"],
        json!([]),
        "{}",
        answer.describe()
    );
}

#[test]
fn the_body_is_at_most_64_kib() {
    let s = Scenario::start();
    let good = reader_body("sized", &s.db).to_string();
    let admin = s.admin();
    // Exactly 64 KiB, padded with white space after the object: taken.
    let mut padded = good.clone();
    padded.push_str(&" ".repeat(65536 - good.len()));
    assert_eq!(padded.len(), 65536);
    admin.post_raw("/v1/resources", &padded).expect_status(201);
    // One byte more: too large, and nothing stored.
    let mut over = reader_body("sized_over", &s.db).to_string();
    let len = over.len();
    over.push_str(&" ".repeat(65537 - len));
    assert_eq!(over.len(), 65537);
    let answer = admin.post_raw("/v1/resources", &over);
    assert_api_error(&answer, 413, "too-large");
    assert_nothing_stored(&s, "sized_over", "a body past 64 KiB");
    // Far past it, still answered.
    let huge = "x".repeat(2 * 1024 * 1024);
    assert_api_error(&admin.post_raw("/v1/resources", &huge), 413, "too-large");
}

#[test]
fn a_storage_registration_has_warnings_too_and_a_database_never_leaves_them_out() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let answer = s
        .admin()
        .create_resource(&storage_body(MARKETING, &dir.to_string_lossy()));
    answer.expect_status(201);
    let json = answer.json();
    assert_eq!(json["warnings"], json!([]), "{}", answer.describe());
    assert!(
        json.as_object().unwrap().contains_key("warnings"),
        "`warnings` is always there"
    );
}

#[test]
fn at_most_two_checks_run_at_once_and_a_third_is_busy() {
    let s = Scenario::start();
    let db = &s.db;
    let hanging = Hanging::start();
    let mut hung = connection(db, &db.reader, None, None);
    hung["host"] = json!(hanging.host());
    hung["port"] = json!(hanging.port());
    let first = body("hang_one", &hung);
    let second = body("hang_two", &hung);
    thread::scope(|scope| {
        let a = scope.spawn(|| post(&s, &first));
        let b = scope.spawn(|| post(&s, &second));
        // Both are held in their checks.
        thread::sleep(Duration::from_millis(2500));
        let began = Instant::now();
        let third = post(&s, &reader_body("third", db));
        assert_api_error(&third, 503, "busy");
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "expected a third check to be refused at once, it took {:?}",
            began.elapsed()
        );
        let message = third.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            message.contains("check") && (message.contains("again") || message.contains("shortly")),
            "expected the message to say other checks are running and to try again, got {message:?}"
        );
        assert_nothing_stored(&s, "third", "a registration refused as busy");
        // A replacement of logins is a check too: busy as well.
        for answer in [a.join().unwrap(), b.join().unwrap()] {
            assert_api_error(&answer, 422, "login-failed");
        }
    });
    // Once they end, a check goes through.
    post(&s, &reader_body("third", db)).expect_status(201);
}

#[test]
fn a_server_that_answers_too_many_connections_is_unreachable_and_nothing_is_stored() {
    let s = Scenario::start();
    let db = &s.db;
    // `CONNECTION LIMIT 0`: Postgres answers "too many connections for role".
    let full = new_login(db, "full", "CONNECTION LIMIT 0");
    let answer = post(&s, &body("too_many", &connection(db, &full, None, None)));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        failed_of(&answer),
        pairs(&[("read", "unreachable")]),
        "{}",
        answer.describe()
    );
    assert_nothing_stored(&s, "too_many", "a role that has no connection to give");
    assert_holds_nothing(&s, db, &[&full], &[&answer], "too many connections");
}

#[test]
fn a_role_that_may_not_log_in_is_refused_not_unreachable() {
    let s = Scenario::start();
    let db = &s.db;
    let login = new_login(db, "nologin", "");
    db.superuser()
        .batch(&format!("ALTER ROLE {} NOLOGIN", login.role));
    let answer = post(&s, &body("no_login", &connection(db, &login, None, None)));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        failed_of(&answer),
        pairs(&[("read", "refused")]),
        "{}",
        answer.describe()
    );
    assert_nothing_stored(&s, "no_login", "a role with NOLOGIN");
}
