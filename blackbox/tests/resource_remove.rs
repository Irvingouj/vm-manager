//! Removing a Resource (CONTRACT.md, "Resources and Grants in full:
//! Removing a Resource"): `DELETE /v1/resources/<name>` answers with
//! `grants_removed`, `declared_by` (the Applications whose serving Version
//! declares it, by name, with Version and level, at most 100,
//! `declared_by_more`) and `directory_cleaned`; from the `200` on
//! operations are refused `not-registered` in every Application and pulls
//! are `404`; the name is free again, with no Grant and not the old logins;
//! an Application declaring it stays deployed; a database's connections are
//! closed within 15 seconds and the shared database is not touched; a
//! Storage Resource's files are never touched; the Operation log's entries
//! stay; the log record `resource-removed`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::{log_page, platform_sql};
use blackbox::api::version_body_declaring;
use blackbox::deploy::signed_in;
use blackbox::project::{Report, check, checked_project, manifest_ts};
use blackbox::resources::{
    admin_resource, admin_resources, body, connection, delete_resource, location_body, login_json,
    not_served, patch_resource, put_connection, put_location, reader_of_its_own, records_waited,
    sessions_of, wait_for_no_sessions, writer_direct,
};
use blackbox::scenario::{
    READERS, RESOURCE, Scenario, WRITERS, administrator, assert_error_kind, assert_refusal,
    assert_rows, op, sql_on,
};
use blackbox::storage::{
    FILE_READERS, FILE_WRITERS, FILES, MARKETING, Storage, VIEWER, get, place, put, set_mode, tree,
    wanda,
};
use blackbox::{INSTALLER, Response, User, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

fn wendy() -> User {
    User::new("wendy").in_groups(&[WRITERS, READERS])
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn remove(s: &Scenario, name: &str) -> Response {
    delete_resource(&s.admin(), name)
}

fn query_one(s: &Scenario, application: &str, user: &User) -> Value {
    s.one(
        user,
        application,
        &op("query", &sql_on(RESOURCE, "select 1 as one")),
    )
    .1
}

#[test]
fn the_answer_names_the_grants_and_the_applications_that_declare_the_resource() {
    let s = Scenario::writable();
    let answer = remove(&s, RESOURCE);
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(
        keys(&body),
        [
            "declared_by",
            "declared_by_more",
            "directory_cleaned",
            "grants_removed",
            "kind",
            "name"
        ],
        "{body}"
    );
    assert_eq!(body["name"], json!(RESOURCE));
    assert_eq!(body["kind"], json!("postgres"));
    assert_eq!(body["grants_removed"], json!(2), "{body}");
    assert_eq!(
        body["declared_by"],
        json!([
            { "application": "board", "version": 1, "access": "write" },
            { "application": "viewer", "version": 1, "access": "read" },
        ]),
        "{body}"
    );
    assert_eq!(body["declared_by_more"], json!(0));
    assert_eq!(body["directory_cleaned"], Value::Null);
}

#[test]
fn only_the_serving_version_counts_and_with_its_number_and_level() {
    let s = Scenario::ready();
    // `second`'s Version 1 declares the Resource at `read`, its serving
    // Version 2 at `write`; `moved-on`'s Version 2 declares nothing.
    s.install("second", "operations.js", &[(RESOURCE, "read")]);
    s.install("moved-on", "operations.js", &[(RESOURCE, "read")]);
    let api = s.vm.api_as(&User::new(INSTALLER));
    api.send_version(
        "second",
        &version_body_declaring(&fixture_bundle("operations.js"), &[(RESOURCE, "write")]),
    )
    .expect_status(201);
    api.send_version(
        "moved-on",
        &version_body_declaring(&fixture_bundle("operations.js"), &[]),
    )
    .expect_status(201);
    let body = remove(&s, RESOURCE).json();
    assert_eq!(
        body["declared_by"],
        json!([
            { "application": "board", "version": 1, "access": "read" },
            { "application": "second", "version": 2, "access": "write" },
        ]),
        "expected the serving Versions only, by name: {body}"
    );
    assert_eq!(body["declared_by_more"], json!(0));
}

#[test]
fn declared_by_holds_100_by_name_and_counts_the_rest() {
    let s = Scenario::start();
    s.register(RESOURCE);
    for n in 0..102 {
        s.install(&format!("app{n:03}"), "query.js", &[(RESOURCE, "read")]);
    }
    let body = remove(&s, RESOURCE).json();
    let names: Vec<String> = body["declared_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["application"].as_str().unwrap().to_owned())
        .collect();
    let want: Vec<String> = (0..100).map(|n| format!("app{n:03}")).collect();
    assert_eq!(names, want, "the first 100 by name");
    assert_eq!(
        body["declared_by_more"],
        json!(2),
        "{}",
        keys(&body).join(",")
    );
}

#[test]
fn it_is_404_again_after_a_200_and_everything_that_named_it_is_gone() {
    let s = Scenario::writable();
    remove(&s, RESOURCE).expect_status(200);
    assert_api_error(&remove(&s, RESOURCE), 404, "not-found");
    assert_api_error(&s.admin().get_resource(RESOURCE), 404, "not-found");
    assert_api_error(&admin_resource(&s.admin(), RESOURCE), 404, "not-found");
    assert_api_error(&s.admin().list_grants(RESOURCE), 404, "not-found");
    let listed = s.admin().list_resources().json();
    assert!(
        listed["resources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != RESOURCE),
        "{listed}"
    );
    let admin_list = admin_resources(&s.admin(), "").json();
    assert!(
        admin_list["resources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != RESOURCE),
        "{admin_list}"
    );
    assert_api_error(
        &blackbox::structure::pull(&s.vm.api_as(&wendy()), RESOURCE),
        404,
        "not-found",
    );
}

#[test]
fn from_the_200_on_operations_are_refused_not_registered_in_every_application() {
    let s = Scenario::writable();
    // Before: both Applications work.
    for application in ["board", "viewer"] {
        let answer = query_one(&s, application, &wendy());
        assert_rows(&answer, &["one"], &json!([[1]]), "before the removal");
    }
    remove(&s, RESOURCE).expect_status(200);
    for application in ["board", "viewer"] {
        for user in [wendy(), alice()] {
            for operation in ["query", "execute"] {
                let answer = s
                    .one(
                        &user,
                        application,
                        &op(operation, &sql_on(RESOURCE, "select 1 as one")),
                    )
                    .1;
                assert_refusal(
                    &answer,
                    "not-registered",
                    &json!({ "resource": RESOURCE }),
                    &format!("{operation} by {} through {application}", user.id),
                );
            }
        }
    }
    // The Applications stay deployed.
    for application in ["board", "viewer"] {
        let listed = s.admin().get_application(application);
        listed.expect_status(200);
        assert_eq!(
            listed.json()["serving_version"],
            json!(1),
            "{application} stays deployed"
        );
    }
}

#[test]
fn the_check_of_a_project_that_declares_it_says_what_it_says_of_a_name_not_registered() {
    let s = Scenario::ready();
    let machine = signed_in(&s.vm);
    let project = checked_project("removed-resource-check");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "removed-check",
            "{ devops: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    remove(&s, RESOURCE).expect_status(200);
    let run = check(&machine, &project.dir);
    let report = Report::failed(&run);
    assert!(
        !report.problems_for("resource-not-registered").is_empty(),
        "expected `resource-not-registered` for a Resource that was removed\n{}",
        run.describe()
    );
}

#[test]
fn the_name_is_free_again_with_no_grant_and_the_old_logins_are_not_used() {
    let s = Scenario::start();
    let db = &s.db;
    let reader = reader_of_its_own(db);
    let writer = writer_direct(db);
    s.register_body(
        RESOURCE,
        &body(RESOURCE, &connection(db, &reader, Some(&writer), None)),
    );
    s.grant(RESOURCE, WRITERS, "write");
    s.install("board", "operations.js", &[(RESOURCE, "write")]);
    let who = sql_on(RESOURCE, "select current_user as who");
    let (_, before) = s.one(&wendy(), "board", &op("query", &who));
    assert_rows(
        &before,
        &["who"],
        &json!([[reader.role]]),
        "the old read login",
    );
    remove(&s, RESOURCE).expect_status(200);

    // Registered again with another read login and no write login.
    let again = reader_of_its_own(db);
    s.register_body(
        RESOURCE,
        &body(RESOURCE, &connection(db, &again, None, None)),
    );
    let grants = s.admin().list_grants(RESOURCE).json();
    assert_eq!(
        grants["grants"],
        json!([]),
        "a Group's old Grant does not come back: {grants}"
    );
    let (_, refused) = s.one(&wendy(), "board", &op("query", &who));
    assert_refusal(
        &refused,
        "no-grant",
        &json!({
            "resource": RESOURCE,
            "user": "wendy",
            "needed": "read",
            "held": null,
            "contact": blackbox::scenario::CONTACT,
        }),
        "the old Grant does not come back",
    );
    s.grant(RESOURCE, WRITERS, "write");
    let (_, now) = s.one(&wendy(), "board", &op("query", &who));
    assert_rows(
        &now,
        &["who"],
        &json!([[again.role]]),
        "the new read login, not the old one",
    );
    let (_, execute) = s.one(
        &wendy(),
        "board",
        &op(
            "execute",
            &sql_on(RESOURCE, "insert into notes (body) values ('x')"),
        ),
    );
    assert_error_kind(
        &execute,
        "read-only-resource",
        "the old write login is not used by a Resource registered again",
    );
}

#[test]
fn a_database_connections_are_closed_within_15_seconds_and_the_shared_database_is_not_touched() {
    let s = Scenario::start();
    let db = &s.db;
    let reader = reader_of_its_own(db);
    let writer = writer_direct(db);
    s.register_body(
        RESOURCE,
        &body(RESOURCE, &connection(db, &reader, Some(&writer), None)),
    );
    s.grant(RESOURCE, WRITERS, "write");
    s.install("board", "operations.js", &[(RESOURCE, "write")]);
    let (_, read) = s.one(
        &wendy(),
        "board",
        &op("query", &sql_on(RESOURCE, "select 1 as one")),
    );
    assert_rows(&read, &["one"], &json!([[1]]), "the harness's own check");
    let (_, written) = s.one(
        &wendy(),
        "board",
        &op(
            "execute",
            &sql_on(RESOURCE, "insert into notes (body) values ('before')"),
        ),
    );
    assert!(written["ok"].is_object(), "{written}");
    assert!(
        sessions_of(db, &reader) > 0 && sessions_of(db, &writer) > 0,
        "the harness's own check: expected the logins to have sessions before the removal"
    );
    let before = db.fingerprint();
    remove(&s, RESOURCE).expect_status(200);
    for (login, what) in [(&reader, "read"), (&writer, "write")] {
        let left = wait_for_no_sessions(db, login, Duration::from_secs(16));
        assert_eq!(
            left, 0,
            "expected the {what} login to have no session at the shared database 15 seconds after the removal"
        );
    }
    assert_eq!(before, db.fingerprint(), "the shared database is untouched");
    assert_eq!(
        db.superuser()
            .value("select count(*) from notes where body = 'before'")
            .as_deref(),
        Some("1"),
        "its rows are as they were"
    );
}

#[test]
fn a_resource_that_is_not_served_is_removed_all_the_same() {
    for problem in ["no-secret-key", "does-not-open", "not-sealed"] {
        let s = not_served(problem);
        let answer = delete_resource(&s.admin(), "devops");
        assert_eq!(
            answer.status,
            200,
            "{problem}: expected a Resource that is not served to be removed, got\n{}",
            answer.describe()
        );
        let body = answer.json();
        assert_eq!(body["kind"], json!("postgres"), "{problem}: {body}");
        assert_eq!(
            body["declared_by"][0]["application"],
            json!("board"),
            "{problem}: {body}"
        );
        assert_api_error(&delete_resource(&s.admin(), "devops"), 404, "not-found");
        assert_api_error(&admin_resource(&s.admin(), "devops"), 404, "not-found");
    }
}

#[test]
fn the_operation_logs_entries_stay_and_the_filter_answers_for_a_name_no_longer_registered() {
    let s = Scenario::writable();
    for user in [wendy(), alice()] {
        query_one(&s, "board", &user);
    }
    let before = log_page(&s.vm, &format!("?resource={RESOURCE}&limit=100"));
    let before_ids: Vec<u64> = before["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_u64().unwrap())
        .collect();
    assert!(before_ids.len() >= 2, "the harness's own check: {before}");
    remove(&s, RESOURCE).expect_status(200);
    let after = log_page(&s.vm, &format!("?resource={RESOURCE}&limit=100"));
    let after_entries = after["entries"].as_array().unwrap();
    for id in &before_ids {
        assert!(
            after_entries.iter().any(|e| e["id"] == json!(id)),
            "expected the entry {id} to stay after the removal: {after}"
        );
    }
    // Registered again, the new one shares them.
    s.register(RESOURCE);
    let shared = log_page(&s.vm, &format!("?resource={RESOURCE}&limit=100"));
    for id in &before_ids {
        assert!(
            shared["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == json!(id)),
            "expected a Resource registered again under the name to share the entries"
        );
    }
}

#[test]
fn the_log_record_names_the_resource_and_who_and_no_login() {
    let s = Scenario::writable();
    remove(&s, RESOURCE).expect_status(200);
    let records = records_waited(&s.vm, "resource-removed", 1);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["resource"], json!(RESOURCE));
    assert_eq!(records[0]["by"], json!(administrator().id));
    let found = blackbox::resources::found_in(
        &blackbox::resources::secrets_of(&s.db, &[&s.db.reader, &s.db.writer]),
        &records[0].to_string(),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_body_is_not_looked_at_and_only_an_administrator_removes() {
    let s = Scenario::ready();
    s.register("second");
    let path = "/v1/resources/second";
    // Someone who is not an administrator is refused and nothing is removed.
    let refused = s.vm.api_as(&alice()).request(path).method("DELETE").send();
    assert_api_error(&refused, 403, "not-an-administrator");
    s.admin().get_resource("second").expect_status(200);
    let refused =
        s.vm.api_as(&alice())
            .request("/v1/resources/ghost")
            .method("DELETE")
            .send();
    assert_api_error(&refused, 403, "not-an-administrator");
    assert_api_error(&delete_resource(&s.admin(), "ghost"), 404, "not-found");
    assert_api_error(&delete_resource(&s.admin(), "Not_A_Name"), 404, "not-found");
    // A body, whatever it is, is not looked at.
    let answer = s
        .admin()
        .request(path)
        .method("DELETE")
        .header("content-type", "application/json")
        .body("this is not json {")
        .send();
    answer.expect_status(200);
    s.admin().get_resource("second").expect_status(404);
    s.admin().get_resource(RESOURCE).expect_status(200);
}

// ---------------------------------------------------------------------------
// A Storage Resource
// ---------------------------------------------------------------------------

fn removed_storage(s: &Storage) -> Value {
    let answer = delete_resource(&s.admin(), MARKETING);
    answer.expect_status(200);
    answer.json()
}

#[test]
fn a_storage_resources_answer_says_its_directory_was_cleaned_and_its_files_are_never_touched() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("a.txt", "kept", None);
    place(&dir.join("folder/b.txt"), b"also kept");
    let body = removed_storage(&s);
    assert_eq!(
        keys(&body),
        [
            "declared_by",
            "declared_by_more",
            "directory_cleaned",
            "grants_removed",
            "kind",
            "name"
        ],
        "{body}"
    );
    assert_eq!(body["kind"], json!("storage"));
    assert_eq!(body["directory_cleaned"], json!(true), "{body}");
    assert_eq!(body["grants_removed"], json!(2));
    assert_eq!(
        body["declared_by"],
        json!([
            { "application": FILES, "version": 1, "access": "write" },
            { "application": VIEWER, "version": 1, "access": "read" },
        ]),
        "{body}"
    );
    assert_eq!(
        tree(&dir),
        vec![
            "a.txt (4 bytes)".to_owned(),
            "folder/".to_owned(),
            "folder/b.txt (9 bytes)".to_owned()
        ],
        "expected the files of a removed Resource never touched, and neither `.vibe-resource` nor an empty `.vibe-staging/` left"
    );
    // Every operation is refused, in every Application.
    for (application, user) in [(FILES, wanda()), (VIEWER, blackbox::storage::rita())] {
        for call in [get(MARKETING, "a.txt"), put(MARKETING, "x.txt", "x", None)] {
            let (_, answer) = s.one(&user, application, &call);
            assert_refusal(
                &answer,
                "not-registered",
                // "Storage": a Storage operation refused `not-registered`
                // has `"kind": "storage"` beside `resource`.
                &json!({ "kind": "storage", "resource": MARKETING }),
                "an operation on a removed Storage Resource",
            );
        }
    }
    let _ = (FILE_READERS, FILE_WRITERS);
}

#[test]
fn the_directory_can_be_registered_again_under_any_name_after_it_was_cleaned() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    place(&dir.join("kept.txt"), b"kept");
    let body = removed_storage(&s);
    assert_eq!(body["directory_cleaned"], json!(true));
    s.register_on("another", &dir);
    assert_eq!(
        std::fs::read(dir.join("kept.txt")).unwrap(),
        b"kept",
        "the files are the new Resource's to have"
    );
    delete_resource(&s.admin(), "another").expect_status(200);
    // The same name again too.
    s.register_on(MARKETING, &dir);
}

#[test]
fn a_directory_that_could_not_be_cleaned_is_in_use_under_another_name_until_by_hand() {
    if blackbox::storage::my_uid() == 0 {
        return;
    }
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    set_mode(&dir, 0o555);
    let answer = delete_resource(&s.admin(), MARKETING);
    set_mode(&dir, 0o755);
    answer.expect_status(200);
    assert_eq!(answer.json()["directory_cleaned"], json!(false));
    assert!(dir.join(".vibe-resource").exists());
    // The same name is taken either way; another name is not, until the
    // marker is removed by hand.
    let other = s.try_register("another", &dir.to_string_lossy());
    assert_api_error(&other, 409, "directory-in-use");
    s.register_on(MARKETING, &dir);
    delete_resource(&s.admin(), MARKETING).expect_status(200);
    std::fs::remove_file(dir.join(".vibe-resource")).ok();
    s.register_on("another", &dir);
}

/// "A `.vibe-staging/` that holds something is left as it is and does not
/// make `directory_cleaned` false": `.vibe-resource` is gone, so it is true.
#[test]
fn a_staging_folder_that_holds_something_is_left_and_the_directory_is_cleaned_all_the_same() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    place(&dir.join(".vibe-staging/half-written"), b"half");
    place(&dir.join("kept.txt"), b"kept");
    let body = removed_storage(&s);
    assert_eq!(body["directory_cleaned"], json!(true), "{body}");
    assert!(
        !dir.join(".vibe-resource").exists(),
        "expected `.vibe-resource` gone: {:?}",
        tree(&dir)
    );
    assert_eq!(
        std::fs::read(dir.join(".vibe-staging/half-written")).unwrap(),
        b"half",
        "expected what the staging folder holds left as it is"
    );
    assert_eq!(std::fs::read(dir.join("kept.txt")).unwrap(), b"kept");
    // Nothing in the registration's rules refuses a directory with a
    // non-empty `.vibe-staging/`: it can be registered under another name.
    s.register_on("another", &dir);
    assert_eq!(
        std::fs::read(dir.join(".vibe-staging/half-written")).unwrap(),
        b"half"
    );
}

/// Removes `MARKETING`, which must be answered `200` within 2 seconds (a
/// directory that cannot be opened is not waited for: nothing is under
/// way, so the answer comes at once, not after the 5 seconds), then checks
/// `not-registered`, `404` and the administrators' listing, which lists
/// `other` within 2 seconds. Returns the answer's `directory_cleaned`.
fn removed_at_once_and_gone(s: &Storage) -> Value {
    let began = Instant::now();
    let answer = delete_resource(&s.admin(), MARKETING);
    let took = began.elapsed();
    answer.expect_status(200);
    assert!(
        took < Duration::from_secs(2),
        "expected the removal to be answered within 2 seconds: a directory that cannot be opened is not waited for, nothing is under way; it took {took:?}"
    );
    let cleaned = answer.json()["directory_cleaned"].clone();
    let (_, refused) = s.one(&wanda(), FILES, &get(MARKETING, "a.txt"));
    assert_refusal(
        &refused,
        "not-registered",
        &json!({ "kind": "storage", "resource": MARKETING }),
        "an operation after the removal",
    );
    assert_api_error(
        &s.vm.api_as(&wanda()).get_resource(MARKETING),
        404,
        "not-found",
    );
    // The installation's other Resources are listed at once too.
    let began = Instant::now();
    let listed = admin_resources(&s.admin(), "");
    let took = began.elapsed();
    listed.expect_status(200);
    assert!(listed.text().contains("\"other\""), "{}", listed.describe());
    assert!(
        took < Duration::from_secs(2),
        "expected the listing of the other Resources within 2 seconds, it took {took:?}"
    );
    cleaned
}

/// The program has the directory open (it was used once) and it is removed
/// by hand while the program runs: what the program holds open decides
/// `directory_cleaned`, which is a boolean.
#[test]
fn a_removal_of_a_storage_resource_whose_directory_was_removed_by_hand_is_answered_at_once() {
    let s = Storage::ready();
    s.register("other");
    s.put_ok("a.txt", "kept", None);
    std::fs::remove_dir_all(s.dir(MARKETING)).unwrap();
    let cleaned = removed_at_once_and_gone(&s);
    assert!(
        cleaned.is_boolean(),
        "expected `directory_cleaned` to be true or false, got {cleaned}"
    );
}

/// The directory cannot be opened at all: it was removed by hand while the
/// program was stopped, so the program never opened it. `directory_cleaned`
/// is false ("false when it could not look").
#[test]
fn a_removal_of_a_storage_resource_whose_directory_cannot_be_opened_is_false_and_at_once() {
    let s = Storage::ready();
    s.register("other");
    s.put_ok("a.txt", "kept", None);
    let dir = s.dir(MARKETING);
    let Storage { vm, place } = s;
    let stopped = blackbox::storage::terminate(vm);
    std::fs::remove_dir_all(&dir).unwrap();
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    let cleaned = removed_at_once_and_gone(&s);
    assert_eq!(
        cleaned,
        json!(false),
        "expected `directory_cleaned` false for a directory that could not be opened"
    );
}

#[test]
fn a_storage_resource_registered_again_has_no_grant_and_serves_the_same_files() {
    let s = Storage::ready();
    s.put_ok("a.txt", "kept", None);
    removed_storage(&s);
    s.register(MARKETING);
    let grants = s.admin().list_grants(MARKETING).json();
    assert_eq!(grants["grants"], json!([]), "{grants}");
    let (_, answer) = s.one(&wanda(), FILES, &get(MARKETING, "a.txt"));
    assert_refusal(
        &answer,
        "no-grant",
        &json!({
            "resource": MARKETING,
            "user": "wanda",
            "needed": "read",
            "held": null,
            "contact": blackbox::storage::FILES_CONTACT,
        }),
        "a Group's old Grant does not come back",
    );
    s.grant(MARKETING, FILE_WRITERS, "write");
    let (_, answer) = s.one(&wanda(), FILES, &get(MARKETING, "a.txt"));
    assert_eq!(answer["ok"]["size"], json!(4), "{answer}");
}

// ---------------------------------------------------------------------------
// A write that finds no row ("A write whose answer is lost")
// ---------------------------------------------------------------------------

/// Deletes the Resource's row, and the rows that hold it (its Grants), at
/// the platform database as another session, behind the program's back:
/// the program is not told.
fn delete_row_behind_the_programs_back(vm: &blackbox::Vm, name: &str) {
    let platform = platform_sql(vm);
    let id = platform
        .value(&format!(
            "SELECT id::text FROM resources WHERE name = '{name}'"
        ))
        .unwrap_or_else(|| panic!("the harness's own check: no row for {name:?}"));
    // Whatever refers to the row, found in the catalogs, goes first.
    let holders = platform.batch(
        "SELECT c.conrelid::regclass::text, a.attname::text
           FROM pg_constraint c
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
          WHERE c.contype = 'f' AND c.confrelid = 'resources'::regclass",
    );
    for holder in holders {
        let (Some(table), Some(column)) = (
            holder.first().cloned().flatten(),
            holder.get(1).cloned().flatten(),
        ) else {
            continue;
        };
        platform.batch(&format!(
            "DELETE FROM {table} WHERE {column}::text = '{id}'"
        ));
    }
    platform.batch(&format!("DELETE FROM resources WHERE id::text = '{id}'"));
    assert_eq!(
        platform.value(&format!(
            "SELECT count(*)::text FROM resources WHERE name = '{name}'"
        )),
        Some("0".to_owned()),
        "the harness's own check: the row of {name:?} is deleted"
    );
}

/// After a write that found no row: the program has let the database
/// Resource go.
fn assert_database_resource_let_go(s: &Scenario) {
    let refused = query_one(s, "board", &wendy());
    assert_refusal(
        &refused,
        "not-registered",
        &json!({ "resource": RESOURCE }),
        "an operation after a write that found no row",
    );
    assert_api_error(
        &s.vm.api_as(&wendy()).get_resource(RESOURCE),
        404,
        "not-found",
    );
    assert_api_error(&admin_resource(&s.admin(), RESOURCE), 404, "not-found");
}

/// As [`assert_database_resource_let_go`], for the Storage Resource.
fn assert_storage_resource_let_go(s: &Storage) {
    let (_, refused) = s.one(&wanda(), FILES, &get(MARKETING, "a.txt"));
    assert_refusal(
        &refused,
        "not-registered",
        &json!({ "kind": "storage", "resource": MARKETING }),
        "an operation after a write that found no row",
    );
    assert_api_error(
        &s.vm.api_as(&wanda()).get_resource(MARKETING),
        404,
        "not-found",
    );
    assert_api_error(&admin_resource(&s.admin(), MARKETING), 404, "not-found");
}

#[test]
fn a_grant_written_for_a_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Scenario::writable();
    delete_row_behind_the_programs_back(&s.vm, RESOURCE);
    assert_api_error(
        &s.admin().grant(RESOURCE, "extra", "read"),
        404,
        "not-found",
    );
    assert_database_resource_let_go(&s);
}

#[test]
fn a_change_of_description_owner_or_contact_of_a_resource_whose_row_is_gone_is_not_found_and_lets_it_go()
 {
    let s = Scenario::writable();
    delete_row_behind_the_programs_back(&s.vm, RESOURCE);
    assert_api_error(
        &patch_resource(&s.admin(), RESOURCE, &json!({ "description": "changed" })),
        404,
        "not-found",
    );
    assert_database_resource_let_go(&s);
}

#[test]
fn a_replacement_of_the_logins_of_a_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Scenario::writable();
    delete_row_behind_the_programs_back(&s.vm, RESOURCE);
    // The logins given connect, as the checks come before the write.
    let answer = put_connection(
        &s.admin(),
        RESOURCE,
        &json!({ "read": login_json(&s.db.reader) }),
    );
    assert_api_error(&answer, 404, "not-found");
    assert_database_resource_let_go(&s);
}

#[test]
fn a_removal_of_a_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Scenario::writable();
    delete_row_behind_the_programs_back(&s.vm, RESOURCE);
    assert_api_error(&remove(&s, RESOURCE), 404, "not-found");
    assert_database_resource_let_go(&s);
    // And asked again, it is still not found.
    assert_api_error(&remove(&s, RESOURCE), 404, "not-found");
}

#[test]
fn a_change_of_location_of_a_storage_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Storage::ready();
    s.put_ok("a.txt", "kept", None);
    let elsewhere = s.dir("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    delete_row_behind_the_programs_back(&s.vm, MARKETING);
    let answer = put_location(
        &s.admin(),
        MARKETING,
        &location_body(&elsewhere.to_string_lossy()),
    );
    assert_api_error(&answer, 404, "not-found");
    assert_storage_resource_let_go(&s);
}

#[test]
fn a_change_of_a_storage_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Storage::ready();
    s.put_ok("a.txt", "kept", None);
    delete_row_behind_the_programs_back(&s.vm, MARKETING);
    assert_api_error(
        &patch_resource(&s.admin(), MARKETING, &json!({ "contact": "someone else" })),
        404,
        "not-found",
    );
    assert_storage_resource_let_go(&s);
}

#[test]
fn a_removal_of_a_storage_resource_whose_row_is_gone_is_not_found_and_lets_it_go() {
    let s = Storage::ready();
    s.put_ok("a.txt", "kept", None);
    delete_row_behind_the_programs_back(&s.vm, MARKETING);
    assert_api_error(&delete_resource(&s.admin(), MARKETING), 404, "not-found");
    assert_storage_resource_let_go(&s);
}
