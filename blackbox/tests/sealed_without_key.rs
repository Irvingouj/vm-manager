//! Without the installation's key the program serves all the same, and every
//! database Resource and every own database is not served (CONTRACT.md,
//! "Resources and Grants in full: At start"): `resource-unusable` and
//! `own-database-unusable` records with `no-secret-key`; an allowed operation
//! on a database Resource fails `unavailable`, with nothing sent to any
//! database; the structure pull is `503` `structure-unavailable`; the lists
//! still show the Resource; Storage, Grants and an Application without an own
//! database work; registering a database Resource is `409` `no-secret-key`
//! and registers nothing; a request that needs an own database fails; a
//! Resource still in clear is not used and not sealed.
//!
//! The platform database is made by a program with the key, which is then
//! started again without it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::api::assert_api_error;
use blackbox::own_database::{FIXTURE, first_tables, helpers, version_body_with_migrations};
use blackbox::scenario::{Expected, assert_entry, assert_error_kind, resource_body};
use blackbox::sealed::{BOARD, Before, DEVOPS, SHOP, Sealed, rita};
use blackbox::storage::{FILE_READERS, FILE_WRITERS, FILES, MARKETING, Place, Storage};
use blackbox::{INSTALLER, User, Vm, VmOptions, fixture_bundle};
use serde_json::{Value, json};

/// A platform database made with the key (DEVOPS, BOARD, SHOP and `plain`),
/// and the program started again on it without one.
fn without_the_key() -> Sealed {
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    s.with_shop(&[first_tables()]);
    s.install("plain", &[], &[]);
    s.restarted_with(&VmOptions::new().without_secret_key())
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn records_of(s: &Sealed, event: &str, field: &str, name: &str) -> Vec<Value> {
    s.logged(event)
        .into_iter()
        .filter(|r| r[field] == name)
        .collect()
}

#[test]
fn the_program_serves_and_logs_each_database_resource_and_own_database_as_unusable() {
    let s = without_the_key();
    assert_eq!(s.vm.listening()["event"], "listening");
    let resource = records_of(&s, "resource-unusable", "resource", DEVOPS);
    assert_eq!(
        resource.len(),
        1,
        "expected one `resource-unusable` record for {DEVOPS:?} at start, got {resource:?}"
    );
    assert_eq!(resource[0]["problem"], "no-secret-key", "{}", resource[0]);
    let own = records_of(&s, "own-database-unusable", "application", SHOP);
    assert_eq!(
        own.len(),
        1,
        "expected one `own-database-unusable` record for {SHOP:?} at start, got {own:?}"
    );
    assert_eq!(own[0]["problem"], "no-secret-key", "{}", own[0]);
    // Every Application with a Version has an own database, with or without
    // migrations: `board`, `shop` and `plain` each have one.
    for application in [BOARD, "plain"] {
        let said = records_of(&s, "own-database-unusable", "application", application);
        assert_eq!(
            said.len(),
            1,
            "expected one `own-database-unusable` record for {application:?}, got {said:?}"
        );
        assert_eq!(said[0]["problem"], "no-secret-key", "{}", said[0]);
    }
    let total = || s.logged("resource-unusable").len() + s.logged("own-database-unusable").len();
    assert_eq!(
        total(),
        4,
        "expected a record for the one database Resource and for each of the three Applications with an own database, and no other"
    );
    // At most one a minute for each while it lasts: nothing more within a
    // few seconds.
    pause(3000);
    assert_eq!(total(), 4, "expected no second record within seconds");
}

#[test]
fn an_allowed_operation_on_a_database_resource_fails_unavailable_and_nothing_is_sent_to_any_database()
 {
    let s = without_the_key();
    pause(500);
    let before = s.db.server_log().len();
    let (response, answer) = s.read_devops();
    assert_error_kind(&answer, "unavailable", "an allowed query without the key");
    let entry = s.admin().operation_log_entries();
    let entry = entry
        .iter()
        .find(|e| e["request"].as_str() == Some(response.request_id().as_str()))
        .unwrap_or_else(|| panic!("no Operation log entry of the request"));
    assert_entry(
        entry,
        &Expected::failed(&response.request_id(), "rita", BOARD, "unavailable"),
        "the entry of the query that failed without the key",
    );
    pause(500);
    assert_eq!(
        s.db.server_log().len(),
        before,
        "expected nothing to be sent to the shared database: its server log grew"
    );
    let text = answer.to_string();
    assert!(
        s.db.credentials_in(&text).is_empty(),
        "expected nothing of the connection in the answer"
    );
}

#[test]
fn the_structure_pull_is_503_structure_unavailable_and_its_entry_is_failed_unavailable() {
    let s = without_the_key();
    let pulled =
        s.vm.api_as(&rita())
            .get(&format!("/v1/resources/{DEVOPS}/structure"));
    assert_api_error(&pulled, 503, "structure-unavailable");
    let entries: Vec<Value> = s
        .admin()
        .operation_log_entries()
        .into_iter()
        .filter(|e| e["request"].as_str() == Some(pulled.request_id().as_str()))
        .collect();
    assert_eq!(entries.len(), 1, "one entry for the pull: {entries:?}");
    assert_eq!(entries[0]["outcome"], "failed", "{}", entries[0]);
    assert_eq!(entries[0]["error"], "unavailable", "{}", entries[0]);
}

#[test]
fn the_lists_still_show_the_resource() {
    let s = without_the_key();
    let list = s.vm.api_as(&rita()).list_resources();
    list.expect_status(200);
    let names: Vec<String> = list.json()["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(names, [DEVOPS], "the list");
    let one = s.vm.api_as(&rita()).get_resource(DEVOPS);
    one.expect_status(200);
    assert_eq!(one.json()["name"], DEVOPS);
}

#[test]
fn grants_work_and_an_application_serves_requests_that_do_not_use_its_own_database() {
    let s = without_the_key();
    let granted = s.admin().grant(DEVOPS, "auditors", "read");
    granted.expect_status(200);
    let listed = s.admin().list_grants(DEVOPS);
    listed.expect_status(200);
    let groups: Vec<String> = listed.json()["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["group"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        groups.contains(&"auditors".to_owned()),
        "expected the new Grant in the list, got {groups:?}"
    );
    let (_, answer) = s.call(
        &rita(),
        "plain",
        &blackbox::scenario::op("whoami", &json!({})),
    );
    assert!(
        answer["ok"]["version"].is_u64(),
        "expected an Application without an own database to serve, got {answer}"
    );
}

#[test]
fn a_storage_resource_is_registered_and_works() {
    // The Application gets its Version while the program has the key; the
    // program is then started again without it.
    let place = Place::new("sealed-no-key");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &VmOptions::new());
    s.register(MARKETING);
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    let Storage { vm, place } = s;
    let options = vm.options().clone().without_secret_key();
    let platform = vm.platform().clone();
    drop(vm);
    let s = Storage {
        vm: Vm::start(&options.platform(&platform)),
        place,
    };
    // A Storage Resource can be registered without the key, and works.
    s.register("second");
    let put = s.put_ok("a.txt", "hello", None);
    assert!(put.is_object(), "{put}");
}

#[test]
fn without_the_key_a_first_version_is_409_no_secret_key_and_nothing_of_it_is_kept() {
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    let s = s.restarted_with(&VmOptions::new().without_secret_key());
    let api = s.vm.api_as(&User::new(INSTALLER));
    let with_migrations =
        version_body_with_migrations(&fixture_bundle(FIXTURE), &[], &[first_tables()]);
    let without = version_body_with_migrations(&fixture_bundle(FIXTURE), &[], &[]);
    for (name, body) in [("with-migrations", &with_migrations), ("without", &without)] {
        api.create_application(name).expect_status(201);
        let sent = api.send_version(name, body);
        assert_api_error(&sent, 409, "no-secret-key");
        let listed = api.list_versions(name);
        listed.expect_status(200);
        assert_eq!(
            listed.json()["versions"],
            json!([]),
            "expected nothing of the Version kept for {name:?}"
        );
    }
    // With the key the same Versions are accepted.
    let s = s.restarted_with(&VmOptions::new());
    let api = s.vm.api_as(&User::new(INSTALLER));
    for (name, body) in [("with-migrations", &with_migrations), ("without", &without)] {
        let sent = api.send_version(name, body);
        assert_eq!(sent.status, 201, "{name}: {}", sent.describe());
    }
}

#[test]
fn registering_a_database_resource_is_409_no_secret_key_and_registers_nothing() {
    let s = without_the_key();
    let answer = s
        .admin()
        .create_resource(&resource_body("fresh", &s.db, &s.db.reader));
    assert_api_error(&answer, 409, "no-secret-key");
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("VIBE_SECRET_KEY"),
        "expected the message to say to start the program with VIBE_SECRET_KEY, got {message:?}"
    );
    let list = s.admin().list_resources();
    assert!(
        !list.text().contains("\"fresh\""),
        "expected nothing registered, got {}",
        list.text()
    );
    // And the name is free: with the key the same registration is 201.
    let s = s.restarted_with(&VmOptions::new());
    s.admin()
        .create_resource(&resource_body("fresh", &s.db, &s.db.reader))
        .expect_status(201);
}

#[test]
fn a_request_that_needs_the_own_database_fails_unavailable_and_so_does_a_version_with_migrations() {
    let s = without_the_key();
    let (response, answer) = s.read_shop();
    assert_error_kind(&answer, "unavailable", "an own database without the key");
    let entries: Vec<Value> = s
        .admin()
        .operation_log_entries()
        .into_iter()
        .filter(|e| e["request"].as_str() == Some(response.request_id().as_str()))
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_entry(
        &entries[0],
        &Expected {
            outcome: "failed",
            error: Some("unavailable"),
            ran: None,
            ..Expected::own(&response.request_id(), "rita", SHOP, "user", "query")
        },
        "the own database's query without the key",
    );
    let body =
        version_body_with_migrations(&fixture_bundle(FIXTURE), &[], &[first_tables(), helpers()]);
    let sent = s.vm.api_as(&User::new(INSTALLER)).send_version(SHOP, &body);
    assert_api_error(&sent, 503, "unavailable");
}

#[test]
fn a_clear_row_of_before_is_not_used_and_not_sealed() {
    let before = Before::make();
    let (s, old) = before.start(&VmOptions::new().without_secret_key());
    let (_, answer) = s.read_devops();
    assert_error_kind(&answer, "unavailable", "a clear Resource without the key");
    let (_, answer) = s.read_shop();
    assert_error_kind(
        &answer,
        "unavailable",
        "a clear own database without the key",
    );
    let resource = records_of(&s, "resource-unusable", "resource", DEVOPS);
    assert_eq!(resource.len(), 1, "{resource:?}");
    assert_eq!(resource[0]["problem"], "no-secret-key", "{}", resource[0]);
    let own = records_of(&s, "own-database-unusable", "application", SHOP);
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!(own[0]["problem"], "no-secret-key", "{}", own[0]);
    assert_eq!(s.logged("credentials-sealed"), Vec::<Value>::new());
    assert!(
        old.is_still_clear(),
        "expected the clear values to be left as they were: not sealed without the key"
    );
}

/// A second Version of `shop`, whose own database is not served: with these
/// migrations (the whole list), 503 `unavailable`, not recorded, and the
/// first Version still serving.
fn assert_a_second_version_is_refused(migrations: &[blackbox::own_database::Migration]) {
    let s = Sealed::start(&VmOptions::new());
    s.with_shop(&[first_tables()]);
    let s = s.restarted_with(&VmOptions::new().without_secret_key());
    let api = s.vm.api_as(&User::new(INSTALLER));
    let body = version_body_with_migrations(&fixture_bundle(FIXTURE), &[], migrations);
    let sent = api.send_version(SHOP, &body);
    assert_api_error(&sent, 503, "unavailable");
    assert_eq!(
        api.versions(SHOP).len(),
        1,
        "expected the Version not to be recorded"
    );
    // The request that does not use the own database says which Version serves.
    let (_, answer) = s.call(&rita(), SHOP, &blackbox::scenario::op("whoami", &json!({})));
    assert_eq!(answer["ok"]["version"], json!(1), "{answer}");
}

#[test]
fn a_version_without_migrations_to_an_application_whose_own_database_is_not_served_is_503() {
    // No new migration: the whole history so far, as a Version must carry it.
    assert_a_second_version_is_refused(&[first_tables()]);
}

#[test]
fn a_version_with_migrations_to_an_application_whose_own_database_is_not_served_is_503() {
    assert_a_second_version_is_refused(&[first_tables(), helpers()]);
}

#[test]
fn on_a_resource_that_is_not_served_a_user_with_a_grant_gets_unavailable_and_one_without_gets_no_grant()
 {
    let s = without_the_key();
    let (_, with_grant) = s.read_devops();
    assert_error_kind(&with_grant, "unavailable", "a User with a Grant");
    let nora = User::new("nora").in_groups(&["elsewhere"]);
    let (_, without) = s.call(
        &nora,
        BOARD,
        &blackbox::scenario::op("query", &blackbox::scenario::sql_on(DEVOPS, "SELECT 1")),
    );
    assert_eq!(
        without["refused"]["refusal"]["reason"], "no-grant",
        "expected a User without a Grant to learn nothing new about the Resource, got {without}"
    );
    assert!(without.get("error").is_none(), "{without}");
}
