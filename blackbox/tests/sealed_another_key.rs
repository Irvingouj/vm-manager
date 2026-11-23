//! With another key than the one the values were sealed under, what does not
//! open is not served (CONTRACT.md, "Resources and Grants in full: At
//! start"): `does-not-open` for a database Resource, which stays as it is; an
//! own database is given new passwords at its next use, with the record
//! `own-database-passwords-replaced`, and nothing of its data or Change
//! history is touched. A key that is not 32 bytes in base64 is still a
//! start-up failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use blackbox::api::assert_api_error;
use blackbox::connection::SecretKey;
use blackbox::own_database::{OwnDatabase, first_tables, own_execute, own_query};
use blackbox::scenario::{Expected, assert_entry, assert_error_kind};
use blackbox::sealed::{BOARD, DEVOPS, SHOP, Sealed, Stored, rita};
use blackbox::{Vm, VmOptions};
use serde_json::{Value, json};

/// A platform database made with the harness's key: DEVOPS, BOARD and SHOP
/// with a row of its own and a change in its history.
fn sealed() -> (Sealed, OwnDatabase) {
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    let own = s.with_shop(&[first_tables()]);
    let (_, answer) = s.call(
        &rita(),
        SHOP,
        &own_execute("user", "INSERT INTO items (title) VALUES ('kept')"),
    );
    assert!(answer["ok"].is_object(), "the harness's own step: {answer}");
    (s, own)
}

fn with_key(key: &SecretKey) -> VmOptions {
    VmOptions::new().env("VIBE_SECRET_KEY", key.expose())
}

fn records(s: &Sealed, event: &str, field: &str, name: &str) -> Vec<Value> {
    s.logged(event)
        .into_iter()
        .filter(|r| r[field] == name)
        .collect()
}

#[test]
fn with_another_key_a_database_resource_is_not_served_and_is_left_as_it_is() {
    let (s, _own) = sealed();
    let stored = Stored::of(s.platform());
    let before = stored.connection(DEVOPS).unwrap();
    let s = s.restarted_with(&with_key(&SecretKey::fresh()));
    let said = records(&s, "resource-unusable", "resource", DEVOPS);
    assert_eq!(
        said.len(),
        1,
        "expected one record for {DEVOPS:?}, got {said:?}"
    );
    assert_eq!(said[0]["problem"], "does-not-open", "{}", said[0]);
    std::thread::sleep(Duration::from_millis(500));
    let seen = s.db.server_log().len();
    let (response, answer) = s.read_devops();
    assert_error_kind(
        &answer,
        "unavailable",
        "a Resource sealed under another key",
    );
    let entries: Vec<Value> = s
        .admin()
        .operation_log_entries()
        .into_iter()
        .filter(|e| e["request"].as_str() == Some(response.request_id().as_str()))
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_entry(
        &entries[0],
        &Expected::failed(&response.request_id(), "rita", BOARD, "unavailable"),
        "the query that failed under another key",
    );
    let pulled =
        s.vm.api_as(&rita())
            .get(&format!("/v1/resources/{DEVOPS}/structure"));
    assert_api_error(&pulled, 503, "structure-unavailable");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        s.db.server_log().len(),
        seen,
        "expected nothing to be sent to the shared database"
    );
    assert_eq!(
        stored.connection(DEVOPS).unwrap(),
        before,
        "expected a value that does not open to be left as it is, not sealed again under the other key"
    );
    assert_eq!(s.logged("credentials-sealed"), Vec::<Value>::new());
}

#[test]
fn the_key_given_back_serves_the_resource_again() {
    let (s, _own) = sealed();
    let s = s.restarted_with(&with_key(&SecretKey::fresh()));
    assert_eq!(s.logged("resource-unusable").len(), 1);
    let s = s.restarted_with(&VmOptions::new());
    assert_eq!(
        s.logged("resource-unusable"),
        Vec::<Value>::new(),
        "expected nothing unusable under the key the values were sealed under"
    );
    assert_eq!(s.read_devops().1["ok"]["rows"], json!([[1]]));
}

#[test]
fn with_another_key_an_own_database_is_not_served_until_used_and_then_gets_new_passwords() {
    let (s, _own) = sealed();
    let stored = Stored::of(s.platform());
    let passwords = stored.own_passwords(SHOP);
    // The Change history is shipped from an outbox: wait for the change.
    let history_now = || {
        stored
            .admin()
            .value("SELECT count(*) FROM change_history")
            .unwrap()
    };
    let began = std::time::Instant::now();
    while history_now() == "0" && began.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let history = history_now();
    assert_ne!(
        history, "0",
        "the harness's own check: the change is in the history"
    );
    let s = s.restarted_with(&with_key(&SecretKey::fresh()));
    let said = records(&s, "own-database-unusable", "application", SHOP);
    assert_eq!(
        said.len(),
        1,
        "expected one record for {SHOP:?}, got {said:?}"
    );
    assert_eq!(said[0]["problem"], "does-not-open", "{}", said[0]);
    assert_eq!(
        records(&s, "own-database-passwords-replaced", "application", SHOP).len(),
        0,
        "expected new passwords only at the next use"
    );
    let (_, answer) = s.read_shop();
    assert_eq!(
        answer["ok"]["rows"],
        json!([["seeded by 0000"]]),
        "expected the own database to be served after its passwords were replaced, got {answer}"
    );
    let replaced = records(&s, "own-database-passwords-replaced", "application", SHOP);
    assert_eq!(replaced.len(), 1, "expected one record, got {replaced:?}");
    assert_eq!(replaced[0]["reason"], "does-not-open", "{}", replaced[0]);
    assert_ne!(
        stored.own_passwords(SHOP),
        passwords,
        "expected new passwords to be stored"
    );
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(history_now(), history, "the Change history");
    let (_, rows) = s.call(
        &rita(),
        SHOP,
        &own_query("user", "SELECT title FROM items ORDER BY id"),
    );
    assert_eq!(
        rows["ok"]["rows"],
        json!([["kept"]]),
        "the row the Application wrote before, read through a request: {rows}"
    );
    // Sealed under the new key: a start again says nothing and changes nothing.
    let after = stored.own_passwords(SHOP);
    let s = s.restarted();
    // `board`'s own database was never used under the other key: still unusable.
    assert_eq!(
        records(&s, "own-database-unusable", "application", SHOP),
        Vec::<Value>::new()
    );
    assert_eq!(
        s.logged("own-database-passwords-replaced"),
        Vec::<Value>::new()
    );
    assert_eq!(s.read_shop().1["ok"]["rows"], json!([["seeded by 0000"]]));
    assert_eq!(stored.own_passwords(SHOP), after);
}

#[test]
fn a_key_that_is_not_32_bytes_in_base64_is_still_a_start_up_failure_with_resources_registered() {
    let (s, _own) = sealed();
    let platform = s.platform().clone();
    drop(s);
    for (case, key) in [
        ("31 bytes", STANDARD.encode([7_u8; 31])),
        ("33 bytes", STANDARD.encode([7_u8; 33])),
        (
            "not base64",
            "this is not base64 at all, not one bit!!".to_owned(),
        ),
    ] {
        let options = VmOptions::new()
            .stand_in_sign_in()
            .platform(&platform)
            .env("VIBE_SECRET_KEY", &key);
        let started = Vm::try_start(&options, Duration::from_secs(20));
        let message = started.err().unwrap_or_else(|| {
            panic!("expected a start with a key of {case} to fail, but it listened")
        });
        assert!(
            message.contains("exited (") && !message.contains(&key),
            "expected the program to end, never showing the key ({case}), got {message}"
        );
    }
}

#[test]
fn with_another_key_a_version_sent_is_a_use_the_passwords_are_replaced_and_the_data_stays() {
    let (s, _own) = sealed();
    let s = s.restarted_with(&with_key(&SecretKey::fresh()));
    assert_eq!(
        records(&s, "own-database-unusable", "application", SHOP).len(),
        1,
        "the harness's own check: the own database does not open"
    );
    let body = blackbox::own_database::version_body_with_migrations(
        &blackbox::fixture_bundle(blackbox::own_database::FIXTURE),
        &[],
        &[first_tables()],
    );
    let sent =
        s.vm.api_as(&blackbox::User::new(blackbox::INSTALLER))
            .send_version(SHOP, &body);
    assert_eq!(sent.status, 201, "{}", sent.describe());
    let replaced = records(&s, "own-database-passwords-replaced", "application", SHOP);
    assert_eq!(replaced.len(), 1, "expected one record, got {replaced:?}");
    assert_eq!(replaced[0]["reason"], "does-not-open", "{}", replaced[0]);
    let (_, rows) = s.call(
        &rita(),
        SHOP,
        &own_query("user", "SELECT title FROM items ORDER BY id"),
    );
    assert_eq!(rows["ok"]["rows"], json!([["kept"]]), "{rows}");
}
