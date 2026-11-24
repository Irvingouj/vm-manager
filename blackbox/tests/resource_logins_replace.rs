//! Replacing a Resource's logins (CONTRACT.md, "Resources and Grants in
//! full: Replacing a Resource's logins", with "At start" for a Resource that
//! is not served and "A write whose answer is lost"):
//!
//! - each member is optional; `null` removes `write` or `structure`;
//!   `read: null` is `400` `malformed`; at least one member is given;
//! - what is connected: the logins given, and every login when `host`,
//!   `port` or `database` is given; the same refusals and warnings;
//! - it takes effect for every operation after the `200`, in every
//!   Application, with nothing deployed again; the old role has no session
//!   15 seconds after it; an old role dropped or with another password does
//!   not matter; removing the write login ends writes, but an open
//!   transaction finishes;
//! - a Resource that is not served: `no-secret-key` is `409`; `does-not-open`
//!   and `not-sealed` need a whole connection (`409` `connection-incomplete`)
//!   and are then served at once;
//! - `404`, `409` `wrong-kind`, two replacements at once (`409`
//!   `changed-meanwhile`), Grants, Declared access and the Operation log
//!   untouched, and the log record `resource-logins-replaced`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::Response;
use blackbox::api::assert_api_error;
use blackbox::outbound_surface::DbProxy;
use blackbox::resources::{
    Hanging, admin_resource, change_password, connections_of, drop_role, login_holding_nothing,
    login_json, put_connection, reader_direct, sessions_of, wait_for_no_sessions, writer_direct,
};
use blackbox::scenario::assert_error_kind;
use blackbox::sealed::{DEVOPS, Sealed, Stored};
use blackbox::server_log::log_everything;
use blackbox::shared_database::SharedDatabase;
use blackbox::sql::Sql;
use blackbox::storage::{MARKETING, Storage};
use blackbox::transactions::{
    APP, Target, Txs, VIEWER, assert_began, assert_ended, begin, commit, execute_in, outside, rita,
    sleep, wanda,
};
use serde_json::{Value, json};

const RESOURCE: &str = "devops";

fn put(t: &Txs, body: &Value) -> Response {
    put_connection(&t.scenario.admin(), RESOURCE, body)
}

/// Who a read operation on `devops` runs as, through `board` or `viewer`.
fn who_reads(t: &Txs, application: &str) -> String {
    let (_, answers) = t.run(
        &rita(),
        application,
        &[outside(
            "query",
            Target::Shared(RESOURCE),
            "select current_user::text as who",
        )],
    );
    answers[0]["ok"]["rows"][0][0]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "expected a read to answer who it ran as, got {}",
                answers[0]
            )
        })
        .to_owned()
}

/// Who a write on `devops` runs as through `board`, or the answer.
fn who_writes(t: &Txs) -> Result<String, Value> {
    let (_, answers) = t.run(
        &wanda(),
        APP,
        &[outside(
            "execute",
            Target::Shared(RESOURCE),
            "insert into notes (body) values ('who') returning written_by",
        )],
    );
    match answers[0]["ok"]["rows"][0][0].as_str() {
        Some(who) => Ok(who.to_owned()),
        None => Err(answers[0].clone()),
    }
}

fn logins_of(t: &Txs) -> Value {
    let answer = admin_resource(&t.scenario.admin(), RESOURCE);
    answer.expect_status(200);
    answer.json()["logins"].clone()
}

fn records(t: &Txs, event: &str, expected: usize) -> Vec<Value> {
    blackbox::resources::records_waited(t.vm(), event, expected)
}

#[test]
fn each_member_is_optional_and_null_removes_write_or_structure() {
    let t = Txs::start();
    let db = t.db();
    let structure = login_holding_nothing(db);
    // One member at a time: the others stay.
    let answer = put(&t, &json!({ "structure": login_json(&structure) }));
    answer.expect_status(200);
    assert_eq!(
        answer.json()["logins"],
        json!({ "read": true, "write": true, "structure": true }),
        "{}",
        answer.describe()
    );
    assert_eq!(
        answer.json()["warnings"],
        json!([]),
        "{}",
        answer.describe()
    );
    assert_eq!(answer.json()["name"], RESOURCE);
    let answer = put(&t, &json!({ "write": null }));
    answer.expect_status(200);
    assert_eq!(
        answer.json()["logins"],
        json!({ "read": true, "write": false, "structure": true })
    );
    let answer = put(&t, &json!({ "structure": null }));
    answer.expect_status(200);
    assert_eq!(
        answer.json()["logins"],
        json!({ "read": true, "write": false, "structure": false })
    );
    // Removing what is not there is asked again and answered the same.
    put(&t, &json!({ "write": null, "structure": null })).expect_status(200);
    // A write login added again.
    let answer = put(&t, &json!({ "write": login_json(&db.writer) }));
    answer.expect_status(200);
    assert_eq!(
        answer.json()["logins"],
        json!({ "read": true, "write": true, "structure": false })
    );
    // The same members again: `200`.
    put(&t, &json!({ "write": login_json(&db.writer) })).expect_status(200);
    // `read` given alone, and a new password for the same role.
    put(&t, &json!({ "read": login_json(&db.reader) })).expect_status(200);
    // Members the body does not have are ignored, if one is given.
    put(
        &t,
        &json!({ "unknown": 1, "name": "other", "kind": "storage", "read": login_json(&db.reader) }),
    )
    .expect_status(200);
    assert_eq!(
        logins_of(&t),
        json!({ "read": true, "write": true, "structure": false })
    );
}

#[test]
fn the_body_must_give_one_member_and_read_cannot_be_removed() {
    let t = Txs::start();
    let db = t.db();
    let before = logins_of(&t);
    let malformed: Vec<(&str, Value)> = vec![
        ("an empty object", json!({})),
        ("only members it does not have", json!({ "colour": "red" })),
        (
            "only `name` and `kind`",
            json!({ "name": "devops", "kind": "postgres" }),
        ),
        ("read null", json!({ "read": null })),
        (
            "read null with another",
            json!({ "read": null, "write": null }),
        ),
        ("read as text", json!({ "read": "reader" })),
        ("read without password", json!({ "read": { "role": "r" } })),
        ("read without role", json!({ "read": { "password": "p" } })),
        ("write as a number", json!({ "write": 7 })),
        ("host as a number", json!({ "host": 5 })),
        ("port as text", json!({ "port": "5432" })),
        ("an array", json!([1])),
        ("a string", json!("x")),
        ("port 0", json!({ "port": 0 })),
        ("port 65536", json!({ "port": 65536 })),
        ("host empty", json!({ "host": "" })),
        ("host 254 bytes", json!({ "host": "h".repeat(254) })),
        ("database empty", json!({ "database": "" })),
        ("database 64 bytes", json!({ "database": "d".repeat(64) })),
        (
            "role 64 bytes",
            json!({ "write": { "role": "r".repeat(64), "password": "p" } }),
        ),
        (
            "password empty",
            json!({ "write": { "role": db.writer.role, "password": "" } }),
        ),
        (
            "password 1025 bytes",
            json!({ "structure": { "role": "r", "password": "p".repeat(1025) } }),
        ),
        (
            "NUL in a password",
            json!({ "write": { "role": db.writer.role, "password": "a\u{0}b" } }),
        ),
    ];
    for (what, body) in &malformed {
        let answer = put(&t, body);
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            !message.contains(&"h".repeat(60)) && !message.contains(&"p".repeat(60)),
            "{what}: the message never repeats what was sent"
        );
    }
    let answer = t
        .scenario
        .admin()
        .request(&format!("/v1/resources/{RESOURCE}/connection"))
        .method("PUT")
        .body("{not json")
        .header("content-type", "application/json")
        .send();
    assert_api_error(&answer, 400, "malformed");
    // Past 64 KiB.
    let mut huge = json!({ "read": login_json(&db.reader) }).to_string();
    huge.push_str(&" ".repeat(65537));
    let answer = t
        .scenario
        .admin()
        .request(&format!("/v1/resources/{RESOURCE}/connection"))
        .method("PUT")
        .header("content-type", "application/json")
        .body(huge)
        .send();
    assert_api_error(&answer, 413, "too-large");
    assert_eq!(logins_of(&t), before, "nothing changed by a refusal");
    assert!(records(&t, "resource-logins-replaced", 0).is_empty());
}

#[test]
fn only_what_is_given_is_connected_and_everything_when_the_address_changes() {
    let t = Txs::start();
    let db = t.db();
    log_everything(db.server());
    let new_write = writer_direct(db);
    let structure = login_holding_nothing(db);
    // Put a structure login in place first.
    put(&t, &json!({ "structure": login_json(&structure) })).expect_status(200);
    let count = |login: &blackbox::shared_database::Login| connections_of(db, login);
    let (r0, w0, s0, nw0) = (
        count(&db.reader),
        count(&db.writer),
        count(&structure),
        count(&new_write),
    );
    // A new write login alone: only it is connected.
    put(&t, &json!({ "write": login_json(&new_write) })).expect_status(200);
    assert!(count(&new_write) > nw0, "the given login is connected");
    assert_eq!(
        count(&db.reader),
        r0,
        "the read login was not given: not connected"
    );
    assert_eq!(
        count(&db.writer),
        w0,
        "the old write login is not connected"
    );
    assert_eq!(
        count(&structure),
        s0,
        "the structure login was not given: not connected"
    );
    // The address given (the same address): every login of the Resource is
    // connected, the ones left out too.
    let (r1, w1, s1) = (count(&db.reader), count(&new_write), count(&structure));
    put(&t, &json!({ "host": db.host() })).expect_status(200);
    assert!(
        count(&db.reader) > r1,
        "host given: the read login is connected"
    );
    assert!(
        count(&new_write) > w1,
        "host given: the write login is connected"
    );
    assert!(
        count(&structure) > s1,
        "host given: the structure login is connected"
    );
    let (r2, w2, s2) = (count(&db.reader), count(&new_write), count(&structure));
    put(&t, &json!({ "port": db.port() })).expect_status(200);
    assert!(
        count(&db.reader) > r2 && count(&new_write) > w2 && count(&structure) > s2,
        "port given"
    );
    let (r3, w3, s3) = (count(&db.reader), count(&new_write), count(&structure));
    put(&t, &json!({ "database": db.name() })).expect_status(200);
    assert!(
        count(&db.reader) > r3 && count(&new_write) > w3 && count(&structure) > s3,
        "database given"
    );
}

#[test]
fn a_login_that_fails_refuses_the_replacement_and_changes_nothing() {
    let t = Txs::start();
    let db = t.db();
    let before = logins_of(&t);
    let who_before = who_reads(&t, APP);
    let wrong = blackbox::resources::with_wrong_password(&db.writer);
    let answer = put(&t, &json!({ "write": login_json(&wrong) }));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        answer.json()["error"]["failed"],
        json!([{ "login": "write", "why": "refused" }]),
        "{}",
        answer.describe()
    );
    // A new address on which a login left out cannot connect: the database
    // exists, the reader may not connect to it.
    let server = Sql::connect(&db.server().url("postgres")).unwrap();
    let shut = format!("shut_db_{}", blackbox::shared_database::token());
    server.batch(&format!("CREATE DATABASE {shut}"));
    server.batch(&format!("REVOKE CONNECT ON DATABASE {shut} FROM PUBLIC"));
    let answer = put(&t, &json!({ "database": shut }));
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        answer.json()["error"]["failed"],
        json!([
            { "login": "read", "why": "refused" },
            { "login": "write", "why": "refused" }
        ]),
        "every login the Resource would have is connected when the database changes, in order: {}",
        answer.describe()
    );
    let mut unreachable = json!({ "port": blackbox::postgres::free_port().unwrap() });
    let answer = put(&t, &unreachable);
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        answer.json()["error"]["failed"],
        json!([
            { "login": "read", "why": "unreachable" },
            { "login": "write", "why": "unreachable" }
        ])
    );
    unreachable =
        json!({ "database": format!("no_such_db_{}", blackbox::shared_database::token()) });
    let answer = put(&t, &unreachable);
    assert_eq!(
        answer.json()["error"]["failed"],
        json!([
            { "login": "read", "why": "no-such-database" },
            { "login": "write", "why": "no-such-database" }
        ])
    );
    assert_eq!(logins_of(&t), before);
    assert_eq!(who_reads(&t, APP), who_before, "the old logins still serve");
    assert!(who_writes(&t).is_ok(), "the old write login still serves");
    assert!(records(&t, "resource-logins-replaced", 0).is_empty());
    let failed = records(&t, "resource-login-failed", 7);
    assert_eq!(
        failed.len(),
        1 + 2 + 2 + 2,
        "one record for each login that failed: {failed:?}"
    );
    // Nothing of the connection in the answers or the log.
    let text: String = t
        .vm()
        .lines()
        .into_iter()
        .map(|l| l.raw)
        .collect::<Vec<_>>()
        .join("\n");
    for secret in [db.name(), db.writer.password(), &shut] {
        assert!(
            !text.contains(secret),
            "no part of a connection in the program's log"
        );
    }
}

#[test]
fn the_warnings_of_a_registration_come_with_a_replacement() {
    let t = Txs::start();
    let db = t.db();
    let answer = put(&t, &json!({ "read": login_json(&db.careless) }));
    answer.expect_status(200);
    let warnings = answer.json()["warnings"].as_array().unwrap().clone();
    assert_eq!(warnings.len(), 1, "{}", answer.describe());
    assert_eq!(warnings[0]["reason"], "read-login-can-write");
    assert_eq!(warnings[0]["login"], "read");
    assert_eq!(
        warnings[0]["evidence"],
        json!(["table", "sequence", "schema"])
    );
    let nothing = login_holding_nothing(db);
    let answer = put(&t, &json!({ "read": login_json(&nothing) }));
    answer.expect_status(200);
    assert_eq!(
        answer.json()["warnings"][0]["reason"],
        "login-sees-no-table"
    );
    let reading = blackbox::resources::reading_login(db, &[blackbox::resources::Evidence::Table]);
    let answer = put(
        &t,
        &json!({ "read": login_json(&db.reader), "structure": login_json(&reading) }),
    );
    answer.expect_status(200);
    assert_eq!(
        answer.json()["warnings"][0]["reason"],
        "structure-login-can-read"
    );
    let kinds: Vec<String> = records(&t, "resource-login-warning", 3)
        .iter()
        .map(|r| r["reason"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "read-login-can-write",
            "login-sees-no-table",
            "structure-login-can-read"
        ]
    );
}

#[test]
fn it_takes_effect_for_every_operation_after_the_200_in_every_application_without_a_deploy() {
    let t = Txs::start();
    let db = t.db();
    let versions_before = |app: &str| t.scenario.admin().list_versions(app).json();
    let boards = versions_before(APP);
    let viewers = versions_before(VIEWER);
    assert_eq!(who_reads(&t, APP), db.reader.role);
    assert_eq!(who_reads(&t, VIEWER), db.reader.role);
    assert_eq!(who_writes(&t), Ok(db.writer.role.clone()));
    let reader = reader_direct(db);
    let writer = writer_direct(db);
    put(
        &t,
        &json!({ "read": login_json(&reader), "write": login_json(&writer) }),
    )
    .expect_status(200);
    for _ in 0..3 {
        assert_eq!(
            who_reads(&t, APP),
            reader.role,
            "board reads as the new read login"
        );
        assert_eq!(
            who_reads(&t, VIEWER),
            reader.role,
            "viewer reads as the new read login"
        );
        assert_eq!(
            who_writes(&t),
            Ok(writer.role.clone()),
            "board writes as the new write login"
        );
    }
    assert_eq!(
        versions_before(APP),
        boards,
        "no Application was deployed again"
    );
    assert_eq!(versions_before(VIEWER), viewers);
}

#[test]
fn what_a_replacement_leaves_alone_is_the_grants_the_declared_access_and_the_log() {
    let t = Txs::start();
    let db = t.db();
    let admin = t.scenario.admin();
    let grants = admin.list_grants(RESOURCE).json();
    let log_before = t.scenario.log();
    let version = admin.get_version(APP, 1).json();
    who_reads(&t, APP);
    let log_mid = t.scenario.log();
    put(&t, &json!({ "read": login_json(&reader_direct(db)) })).expect_status(200);
    assert_eq!(admin.list_grants(RESOURCE).json(), grants);
    assert_eq!(admin.get_version(APP, 1).json(), version);
    let log_after = t.scenario.log();
    assert_eq!(
        log_after, log_mid,
        "no Operation log entry is added, removed or changed by it"
    );
    assert!(log_after.len() > log_before.len());
    let record = records(&t, "resource-logins-replaced", 1);
    assert_eq!(record.len(), 1, "{record:?}");
    assert_eq!(record[0]["resource"], RESOURCE);
    assert_eq!(record[0]["members"], json!(["read"]));
    assert_eq!(record[0]["by"], blackbox::scenario::administrator().id);
    put(&t, &json!({ "host": db.host(), "write": null })).expect_status(200);
    let record = records(&t, "resource-logins-replaced", 2);
    assert_eq!(record[1]["members"], json!(["host", "write"]), "{record:?}");
}

#[test]
fn the_old_role_has_no_session_after_fifteen_seconds_and_old_roles_gone_do_not_matter() {
    let t = Txs::start();
    let db = t.db();
    // Sessions of the old logins exist.
    who_reads(&t, APP);
    who_writes(&t).unwrap();
    assert!(sessions_of(db, &db.reader) >= 1 && sessions_of(db, &db.writer) >= 1);
    let reader = reader_direct(db);
    let writer = writer_direct(db);
    put(
        &t,
        &json!({ "read": login_json(&reader), "write": login_json(&writer) }),
    )
    .expect_status(200);
    let began = Instant::now();
    for old in [&db.reader, &db.writer] {
        let left = wait_for_no_sessions(
            db,
            old,
            Duration::from_secs(15).saturating_sub(began.elapsed()),
        );
        assert_eq!(
            left, 0,
            "expected the old role to have no session 15 s after the 200"
        );
    }
    // And again with the old roles broken first: the password of one
    // changed, the other dropped, then the logins replaced.
    let (reader2, writer2) = (reader_direct(db), writer_direct(db));
    change_password(db, &reader);
    drop_role(db, &writer);
    put(
        &t,
        &json!({ "read": login_json(&reader2), "write": login_json(&writer2) }),
    )
    .expect_status(200);
    assert_eq!(
        who_reads(&t, APP),
        reader2.role,
        "an operation after the 200 succeeds"
    );
    assert_eq!(who_writes(&t), Ok(writer2.role));
    let left = wait_for_no_sessions(db, &reader, Duration::from_secs(15));
    assert_eq!(left, 0);
}

#[test]
fn removing_the_write_login_ends_writes_but_an_open_transaction_finishes() {
    let t = Txs::start();
    let marker = format!("open-{}", blackbox::shared_database::token());
    let target = Target::Shared(RESOURCE);
    let admin = t.scenario.admin();
    let (open, replaced) = thread::scope(|scope| {
        let open = scope.spawn(|| {
            t.run(
                &wanda(),
                APP,
                &[
                    begin(target, "write", "w"),
                    execute_in(
                        "w",
                        &format!("insert into notes (body) values ('{marker}-1')"),
                    ),
                    sleep(2500),
                    execute_in(
                        "w",
                        &format!("insert into notes (body) values ('{marker}-2')"),
                    ),
                    commit("w"),
                ],
            )
        });
        thread::sleep(Duration::from_millis(1200));
        let replaced = put_connection(&admin, RESOURCE, &json!({ "write": null }));
        (open.join().unwrap(), replaced)
    });
    replaced.expect_status(200);
    let answers = open.1;
    assert_began(&answers[0], "the transaction begun before the replacement");
    assert!(answers[1]["ok"].is_object(), "{}", answers[1]);
    assert!(
        answers[3]["ok"].is_object(),
        "a statement of the open transaction after the replacement: {}",
        answers[3]
    );
    assert_ended(&answers[4], "the commit");
    assert_eq!(
        t.shared_notes_with(&marker),
        2,
        "the open transaction committed"
    );
    // After the 200 every write is refused, in every Application.
    let (_, answers) = t.run(
        &wanda(),
        APP,
        &[
            outside(
                "execute",
                target,
                "insert into notes (body) values ('after') returning id",
            ),
            begin(target, "write", "w2"),
            outside("query", target, "select 1 as one"),
        ],
    );
    assert_error_kind(
        &answers[0],
        "read-only-resource",
        "execute after the write login was removed",
    );
    assert_error_kind(
        &answers[1],
        "read-only-resource",
        "tx.begin at write after it was removed",
    );
    assert!(answers[2]["ok"].is_object(), "reads go on: {}", answers[2]);
    assert_eq!(t.shared_notes_with("after"), 0);
}

fn unusable_records(s: &Sealed) -> usize {
    s.logged("resource-unusable")
        .into_iter()
        .filter(|r| r["resource"] == DEVOPS)
        .count()
}

#[test]
fn without_the_key_a_resource_cannot_have_its_logins_replaced() {
    let s = blackbox::resources::not_served("no-secret-key");
    let db = &s.db;
    let whole = json!({
        "host": db.host(), "port": db.port(), "database": db.name(),
        "read": login_json(&db.reader), "write": login_json(&db.writer),
    });
    for body in [
        whole,
        json!({ "write": null }),
        json!({ "read": login_json(&db.reader) }),
    ] {
        let answer = put_connection(&s.admin(), DEVOPS, &body);
        assert_api_error(&answer, 409, "no-secret-key");
    }
    // Still not served.
    assert_error_kind(&s.read_devops().1, "unavailable", "still not served");
}

#[test]
fn a_resource_that_does_not_open_or_is_not_sealed_needs_a_whole_connection_and_is_then_served_at_once()
 {
    for problem in ["does-not-open", "not-sealed"] {
        let s = blackbox::resources::not_served(problem);
        let db = &s.db;
        let said = unusable_records(&s);
        assert!(
            said >= 1,
            "{problem}: the harness's own check: `resource-unusable` was written"
        );
        assert_error_kind(&s.read_devops().1, "unavailable", problem);
        // An incomplete connection: `409`, naming what is missing, and
        // nothing changed.
        let stored = Stored::of(s.platform());
        let bytes = stored.connection(DEVOPS).unwrap();
        for (missing, body) in [
            (
                "host",
                json!({ "port": db.port(), "database": db.name(), "read": login_json(&db.reader) }),
            ),
            (
                "port",
                json!({ "host": db.host(), "database": db.name(), "read": login_json(&db.reader) }),
            ),
            (
                "database",
                json!({ "host": db.host(), "port": db.port(), "read": login_json(&db.reader) }),
            ),
            (
                "read",
                json!({ "host": db.host(), "port": db.port(), "database": db.name() }),
            ),
            ("host", json!({ "write": null })),
        ] {
            let answer = put_connection(&s.admin(), DEVOPS, &body);
            assert_api_error(&answer, 409, "connection-incomplete");
            let message = answer.json()["error"]["message"]
                .as_str()
                .unwrap()
                .to_lowercase();
            assert!(
                message.contains(missing),
                "{problem}: expected the message to name `{missing}`, got {message:?}"
            );
            assert_eq!(
                stored.connection(DEVOPS).unwrap(),
                bytes,
                "{problem}: nothing changed"
            );
        }
        assert_error_kind(&s.read_devops().1, "unavailable", problem);
        // A whole one: served at once, with no restart.
        let whole = json!({
            "host": db.host(), "port": db.port(), "database": db.name(),
            "read": login_json(&db.reader), "write": login_json(&db.writer),
        });
        let answer = put_connection(&s.admin(), DEVOPS, &whole);
        answer.expect_status(200);
        assert_eq!(
            answer.json()["usable"],
            json!(true),
            "{problem}: {}",
            answer.describe()
        );
        assert_eq!(answer.json()["problem"], Value::Null);
        assert_eq!(
            answer.json()["logins"],
            json!({ "read": true, "write": true, "structure": false })
        );
        let (_, read) = s.read_devops();
        assert_eq!(
            read["ok"]["rows"],
            json!([[1]]),
            "{problem}: served after the 200: {read}"
        );
        let (_, written) = s.call(
            &blackbox::scenario::administrator(),
            blackbox::sealed::BOARD,
            &blackbox::scenario::op(
                "query",
                &blackbox::scenario::sql_on(DEVOPS, "select 2 as two"),
            ),
        );
        assert!(
            written["ok"].is_object() || written["refused"].is_object(),
            "{written}"
        );
        let after = unusable_records(&s);
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(
            unusable_records(&s),
            after,
            "{problem}: the `resource-unusable` records stop"
        );
        let sealed = stored.connection(DEVOPS).unwrap();
        assert_ne!(sealed, bytes, "{problem}: what is stored was replaced");
        let list = admin_resource(&s.admin(), DEVOPS).json();
        assert_eq!(list["usable"], json!(true), "{list}");
    }
}

#[test]
fn a_replacement_of_what_is_not_there_or_not_a_database_is_404_or_wrong_kind() {
    let t = Txs::start();
    let db = t.db();
    let answer = put_connection(
        &t.scenario.admin(),
        "ghost_resource",
        &json!({ "read": login_json(&db.reader) }),
    );
    assert_api_error(&answer, 404, "not-found");
    // A malformed name is the same refusal as one nothing has.
    let answer = put_connection(
        &t.scenario.admin(),
        "Not-A-Name",
        &json!({ "read": login_json(&db.reader) }),
    );
    assert_api_error(&answer, 404, "not-found");
    // The body is not looked at for a missing Resource: 404 first.
    assert_api_error(
        &put_connection(&t.scenario.admin(), "ghost_resource", &json!({})),
        404,
        "not-found",
    );
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    let fresh = SharedDatabase::create();
    let answer = put_connection(
        &s.admin(),
        MARKETING,
        &json!({ "read": login_json(&fresh.reader) }),
    );
    assert_api_error(&answer, 409, "wrong-kind");
    assert!(
        answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("location"),
        "the message says its place is changed with `location`: {}",
        answer.describe()
    );
}

#[test]
fn two_replacements_at_once_make_one_changed_meanwhile() {
    let t = Txs::start();
    let db = t.db();
    let proxy = DbProxy::start(format!("{}:{}", db.host(), db.port()).parse().unwrap());
    // The first replacement is held in its probe (the proxy holds the
    // statement that reads `pg_roles`), a few seconds.
    proxy.arm("pg_roles");
    let admin = t.scenario.admin();
    let first = json!({ "host": "127.0.0.1", "port": proxy.address().port() });
    let new_write = writer_direct(db);
    thread::scope(|scope| {
        let held = scope.spawn(|| put_connection(&admin, RESOURCE, &first));
        let began = Instant::now();
        // Until the first is held, or answered (a refusal says why at once).
        while proxy.held() == 0 && !held.is_finished() {
            assert!(
                began.elapsed() < Duration::from_secs(8),
                "the harness's own check: the first check was never held at its probe"
            );
            thread::sleep(Duration::from_millis(50));
        }
        if held.is_finished() {
            let answer = held.join().unwrap();
            panic!(
                "expected the first replacement to be held in its probe, it was answered:\n{}",
                answer.describe()
            );
        }
        // The second lands while the first is held: a check holds no lock.
        let second = put_connection(
            &admin,
            RESOURCE,
            &json!({ "write": login_json(&new_write) }),
        );
        second.expect_status(200);
        proxy.release();
        let first = held.join().unwrap();
        assert_api_error(&first, 409, "changed-meanwhile");
    });
    // Nothing of the first was changed; the second holds; asking again works.
    assert_eq!(who_writes(&t), Ok(new_write.role));
    assert_eq!(
        logins_of(&t),
        json!({ "read": true, "write": true, "structure": false })
    );
    put_connection(&admin, RESOURCE, &json!({ "host": db.host() })).expect_status(200);
}

#[test]
fn a_check_that_hangs_gives_timed_out_and_changes_nothing() {
    let t = Txs::start();
    let db = t.db();
    let hanging = Hanging::start();
    let answer = put(
        &t,
        &json!({ "host": hanging.host(), "port": hanging.port() }),
    );
    assert_api_error(&answer, 422, "login-failed");
    assert_eq!(
        answer.json()["error"]["failed"],
        json!([{ "login": "read", "why": "timed-out" }, { "login": "write", "why": "timed-out" }])
    );
    assert_eq!(who_reads(&t, APP), db.reader.role);
}
