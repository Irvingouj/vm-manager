//! Who is allowed a Storage operation (CONTRACT.md, "Storage: Who is
//! allowed"; ticket: a User whose Groups hold no Grant, or one below what
//! the operation needs, is refused per operation with the reason, and
//! nothing is read or written; a Version that did not declare the
//! Resource, or declared `read`, cannot write).
//!
//! Each refusal with every field of its refusal, its Operation log entry,
//! and nothing touched on the directory; a kind mismatch both ways; the
//! order of the refusals; `no-request`, `log-unavailable` and
//! `application-disabled`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::admin::disable_ok;
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::scenario::{Expected, assert_refusal, lines};
use blackbox::storage::{
    FILE_READERS, FILES, FILES_CONTACT, MARKETING, Place, Storage, VIEWER, assert_storage_entry,
    delete, get, head, list, mtime, nobody, op, put, rita, statement_of, storage_expected, tree,
    wanda,
};
use blackbox::{User, VmOptions};
use serde_json::{Value, json};

/// The five operations on `key` of `resource`: (operation, statement, call).
fn five(resource: &str, key: &str) -> Vec<(&'static str, String, Value)> {
    vec![
        (
            "storage.head",
            statement_of("head", key),
            head(resource, key),
        ),
        ("storage.get", statement_of("get", key), get(resource, key)),
        (
            "storage.put",
            statement_of("put", key),
            put(resource, key, b"refused bytes", None),
        ),
        (
            "storage.delete",
            statement_of("delete", key),
            delete(resource, key),
        ),
        (
            "storage.list",
            "list \"\"".to_owned(),
            list(resource, &json!({})),
        ),
    ]
}

/// Fails unless the refused call left `dir` as it was: nothing made,
/// changed or removed (compared by the tree and the file's time).
fn assert_untouched(before: &[String], s: &Storage, context: &str) {
    assert_eq!(
        &tree(&s.place.base)[..],
        before,
        "{context}: expected a refused operation to touch nothing: no file read, made, changed or removed, no folder made"
    );
}

#[test]
fn a_user_whose_groups_hold_nothing_is_refused_every_operation_with_the_contact() {
    let s = Storage::ready();
    s.put_ok("kept/file.txt", b"kept", None);
    let file = s.dir(MARKETING).join("kept/file.txt");
    let time = mtime(&file);
    let before = tree(&s.place.base);
    for (operation, statement, call) in five(MARKETING, "kept/file.txt") {
        let (response, answer) = s.one(&nobody(), FILES, &call);
        let needed = if matches!(operation, "storage.put" | "storage.delete") {
            "write"
        } else {
            "read"
        };
        assert_refusal(
            &answer,
            "no-grant",
            &json!({ "resource": MARKETING, "user": "nobody", "needed": needed, "held": null, "contact": FILES_CONTACT }),
            operation,
        );
        assert!(
            answer["refused"]["fix"]
                .as_str()
                .is_some_and(|f| f.contains(FILES_CONTACT)),
            "{operation}: expected the fix to name the contact, got {answer}"
        );
        let id = response.request_id();
        assert_storage_entry(
            &s.entry_of(&response),
            &storage_expected(
                Expected {
                    resource: Some(MARKETING),
                    ..Expected::refused(&id, "nobody", FILES, "no-grant")
                },
                operation,
                &statement,
            ),
            operation,
        );
    }
    assert_untouched(&before, &s, "no Grant");
    assert_eq!(mtime(&file), time);
    assert_eq!(std::fs::read(&file).unwrap(), b"kept");
}

#[test]
fn a_reader_reads_and_is_refused_put_and_delete_with_what_they_hold() {
    let s = Storage::ready();
    s.put_ok("kept.txt", b"kept", None);
    let before = tree(&s.place.base);
    for (operation, statement, call) in five(MARKETING, "kept.txt") {
        let (response, answer) = s.one(&rita(), FILES, &call);
        let id = response.request_id();
        if matches!(operation, "storage.put" | "storage.delete") {
            assert_refusal(
                &answer,
                "no-grant",
                &json!({ "resource": MARKETING, "user": "rita", "needed": "write", "held": "read", "contact": FILES_CONTACT }),
                operation,
            );
            assert_storage_entry(
                &s.entry_of(&response),
                &storage_expected(
                    Expected {
                        resource: Some(MARKETING),
                        ..Expected::refused(&id, "rita", FILES, "no-grant")
                    },
                    operation,
                    &statement,
                ),
                operation,
            );
        } else {
            assert!(
                answer["ok"].is_object(),
                "{operation}: expected a reader allowed, got {answer}"
            );
        }
    }
    assert_untouched(&before, &s, "a reader's put and delete");
}

#[test]
fn a_version_that_declared_read_is_refused_put_and_delete_even_for_a_writer() {
    let s = Storage::ready();
    s.put_ok("kept.txt", b"kept", None);
    let before = tree(&s.place.base);
    for (operation, statement, call) in five(MARKETING, "kept.txt") {
        let (response, answer) = s.one(&wanda(), VIEWER, &call);
        if matches!(operation, "storage.put" | "storage.delete") {
            assert_refusal(
                &answer,
                "declared-lower",
                &json!({ "resource": MARKETING, "declared": "read", "needed": "write" }),
                operation,
            );
            let id = response.request_id();
            assert_storage_entry(
                &s.entry_of(&response),
                &storage_expected(
                    Expected {
                        resource: Some(MARKETING),
                        ..Expected::refused(&id, "wanda", VIEWER, "declared-lower")
                    },
                    operation,
                    &statement,
                ),
                operation,
            );
        } else {
            assert!(answer["ok"].is_object(), "{operation}: {answer}");
        }
    }
    assert_untouched(&before, &s, "a Version declared at read");
}

#[test]
fn declared_lower_comes_before_no_grant_and_not_declared_before_everything_about_the_resource() {
    let s = Storage::ready();
    let answer = s.one(&nobody(), VIEWER, &put(MARKETING, "x", b"x", None)).1;
    assert_refusal(
        &answer,
        "declared-lower",
        &json!({ "resource": MARKETING, "declared": "read", "needed": "write" }),
        "a Version at read, a User with nothing",
    );
    s.install("bare", "operations.js", &[]);
    let answer = s.one(&nobody(), "bare", &head(MARKETING, "x")).1;
    assert_refusal(
        &answer,
        "not-declared",
        &json!({ "resource": MARKETING, "kind": "storage" }),
        "a name not declared",
    );
    assert!(
        answer["refused"]["fix"]
            .as_str()
            .is_some_and(|f| f.contains("storage")),
        "expected the fix to say to declare it with kind \"storage\", got {answer}"
    );
    // not-own-database comes right after no-request, before anything about
    // the Resource.
    let answer = s
        .one(
            &nobody(),
            "bare",
            &op(
                "storage.get",
                &json!({ "resource": "nothing-here", "key": "x", "as": "application" }),
            ),
        )
        .1;
    assert_refusal(
        &answer,
        "not-own-database",
        &json!({ "resource": "nothing-here" }),
        "as the Application on a name nobody declared",
    );
}

/// A program with: `marketing` (storage), `devops` (postgres) and
/// `ledger` (postgres) registered; `filesdb` (storage) registered; an
/// Application `mixed` (`operations.js`) declaring `marketing` storage
/// write, `devops` postgres read, `ghost` storage write (registered as
/// nothing), `ledger` storage write (registered as a database), and
/// `filesdb` postgres read (registered as Storage).
fn mixed() -> Storage {
    let s = Storage::ready();
    s.register_postgres("devops");
    s.register_postgres("ledger");
    s.register("filesdb");
    for name in ["devops", "ledger", "filesdb"] {
        s.grant(name, "everyone", "write");
    }
    s.install(
        "mixed",
        "operations.js",
        &[
            (MARKETING, "storage", "write"),
            ("devops", "postgres", "read"),
            ("ghost", "storage", "write"),
            ("ledger", "storage", "write"),
            ("filesdb", "postgres", "read"),
        ],
    );
    s
}

fn everyone() -> User {
    User::new("eve").in_groups(&["everyone", "file-writers"])
}

#[test]
fn a_storage_operation_on_a_name_declared_or_registered_as_a_database_is_refused_with_the_kinds() {
    let s = mixed();
    let before = tree(&s.place.base);
    for (operation, _, call) in five("devops", "x.txt") {
        let (response, answer) = s.one(&everyone(), "mixed", &call);
        assert_refusal(
            &answer,
            "not-declared",
            &json!({ "resource": "devops", "kind": "storage", "declared_kind": "postgres" }),
            &format!("{operation} on a name declared postgres"),
        );
        assert!(
            answer["refused"]["fix"]
                .as_str()
                .is_some_and(|f| f.contains("storage")),
            "expected the fix to say to declare it with kind \"storage\", got {answer}"
        );
        let entry = s.entry_of(&response);
        assert_eq!(
            (entry["outcome"].as_str(), entry["reason"].as_str()),
            (Some("refused"), Some("not-declared")),
            "{entry}"
        );
    }
    for (operation, _, call) in five("ghost", "x.txt") {
        let answer = s.one(&everyone(), "mixed", &call).1;
        assert_refusal(
            &answer,
            "not-registered",
            &json!({ "resource": "ghost", "kind": "storage" }),
            &format!("{operation} on a name registered as nothing"),
        );
    }
    for (operation, _, call) in five("ledger", "x.txt") {
        let answer = s.one(&everyone(), "mixed", &call).1;
        assert_refusal(
            &answer,
            "not-registered",
            &json!({ "resource": "ledger", "kind": "storage", "registered_kind": "postgres" }),
            &format!("{operation} on a name registered as a database"),
        );
        assert!(
            answer["refused"]["fix"]
                .as_str()
                .is_some_and(|f| f.contains("database")),
            "expected the fix to say the name is registered as a database, got {answer}"
        );
    }
    assert_untouched(&before, &s, "kind mismatches");
}

#[test]
fn a_database_operation_on_a_name_declared_or_registered_as_storage_is_refused_with_the_kinds() {
    let s = mixed();
    let before = tree(&s.place.base);
    let sql = |resource: &str| json!({ "resource": resource, "sql": "select 1" });
    let tx = |resource: &str| json!({ "resource": resource, "access": "read" });
    for (operation, args) in [
        ("query", sql(MARKETING)),
        ("execute", sql(MARKETING)),
        ("tx.begin", tx(MARKETING)),
    ] {
        let answer = s.one(&everyone(), "mixed", &op(operation, &args)).1;
        assert_refusal(
            &answer,
            "not-declared",
            &json!({ "resource": MARKETING, "kind": "postgres", "declared_kind": "storage" }),
            &format!("{operation} on a name declared storage"),
        );
    }
    for (operation, args) in [
        ("query", sql("filesdb")),
        ("execute", sql("filesdb")),
        ("tx.begin", tx("filesdb")),
    ] {
        let answer = s.one(&everyone(), "mixed", &op(operation, &args)).1;
        assert_refusal(
            &answer,
            "not-registered",
            &json!({ "resource": "filesdb", "kind": "postgres", "registered_kind": "storage" }),
            &format!("{operation} on a name registered as storage"),
        );
    }
    assert_untouched(&before, &s, "database operations on Storage");
}

#[test]
fn with_no_request_in_flight_a_storage_operation_is_refused_and_never_lands() {
    let s = Storage::ready();
    s.install(
        "hostile",
        "storage-hostile.js",
        &[(MARKETING, "storage", "write")],
    );
    let (_, body) = s.post(&wanda(), "hostile", "/at-load", &json!({}));
    assert_refusal(
        &body["atLoad"],
        "no-request",
        &json!({}),
        "a put made while the script loaded",
    );
    s.post(
        &wanda(),
        "hostile",
        "/later",
        &json!({ "resource": MARKETING, "key": "later.txt", "delayMs": 100 }),
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !s.dir(MARKETING).join("at-load.txt").exists()
            && !s.dir(MARKETING).join("later.txt").exists(),
        "expected no put made with no request in flight to land: {:?}",
        tree(&s.dir(MARKETING))
    );
    let later: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| e["application"] == "hostile" && e["outcome"] != "refused")
        .collect();
    assert!(
        later.is_empty(),
        "expected every operation made with no request in flight refused, got\n{}",
        lines(&later)
    );
}

#[test]
fn an_identity_claimed_in_the_arguments_changes_nothing() {
    let s = Storage::ready();
    s.install(
        "hostile",
        "storage-hostile.js",
        &[(MARKETING, "storage", "write")],
    );
    let (_, body) = s.post(
        &rita(),
        "hostile",
        "/claim",
        &json!({ "resource": MARKETING, "key": "claimed.txt" }),
    );
    assert_refusal(
        &body["answers"][0],
        "no-grant",
        &json!({ "resource": MARKETING, "user": "rita", "needed": "write", "held": "read", "contact": FILES_CONTACT }),
        "a put by a reader claiming to be an administrator who writes",
    );
    assert!(!s.dir(MARKETING).join("claimed.txt").exists());
}

#[test]
fn a_resource_name_that_tries_to_leave_is_not_declared() {
    let s = Storage::ready();
    s.install(
        "hostile",
        "storage-hostile.js",
        &[(MARKETING, "storage", "write")],
    );
    s.put_ok("x.txt", b"x", None);
    let (_, body) = s.post(&wanda(), "hostile", "/names", &json!({ "key": "x.txt" }));
    for (name, answer) in body["answers"].as_object().unwrap() {
        assert_eq!(
            answer["refused"]["refusal"]["reason"], "not-declared",
            "expected the Resource name {name:?} refused `not-declared`, got {answer}"
        );
    }
}

#[test]
fn when_the_entry_cannot_be_written_the_operation_is_refused_and_nothing_is_touched() {
    let mut platform_postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&platform_postgres, "vm_platform_storage_log");
    let place = Place::new("log-unavailable");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &VmOptions::new().platform(&platform));
    s.register(MARKETING);
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, "file-writers", "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    s.put_ok("kept.txt", b"kept", None);
    platform_postgres.stop();
    let before = tree(&s.place.base);
    for (operation, _, call) in five(MARKETING, "kept.txt") {
        let response =
            s.vm.request(FILES, "/")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "calls": [call] }))
                .timeout(Duration::from_secs(40))
                .send();
        assert_eq!(response.status, 200, "{}", response.describe());
        assert_refusal(
            &response.json()["answers"][0],
            "log-unavailable",
            &json!({ "resource": MARKETING }),
            &format!("{operation} whose entry cannot be written"),
        );
    }
    assert_untouched(&before, &s, "operations whose entries cannot be written");
}

#[test]
fn after_a_disable_no_storage_operation_of_the_application_is_allowed() {
    let s = Storage::ready();
    let request = s
        .vm
        .request(FILES, "/")
        .as_user(&wanda())
        .method("POST")
        .json(&json!({
            "calls": [put(MARKETING, "before.txt", b"x", None), put(MARKETING, "after.txt", b"x", None)],
            "pauseMs": 1500,
        }))
        .timeout(Duration::from_secs(20));
    let sender = std::thread::spawn(move || request.try_send());
    assert!(
        blackbox::storage::eventually(Duration::from_secs(5), || s
            .dir(MARKETING)
            .join("before.txt")
            .exists()),
        "expected the first put to land before the disable"
    );
    disable_ok(&s.vm, FILES);
    let answered = sender.join().unwrap();
    std::thread::sleep(Duration::from_millis(2000));
    assert!(
        !s.dir(MARKETING).join("after.txt").exists(),
        "expected no put of the disabled Application to land"
    );
    let late: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| e["statement"] == "put \"after.txt\"")
        .collect();
    for entry in &late {
        assert_eq!(
            (entry["outcome"].as_str(), entry["reason"].as_str()),
            (Some("refused"), Some("application-disabled")),
            "expected an operation after the disable refused `application-disabled`: {entry}"
        );
    }
    drop(answered);
    let fresh =
        s.vm.request(FILES, "/")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({ "calls": [head(MARKETING, "before.txt")] }))
            .send();
    assert_eq!(
        fresh.status,
        503,
        "expected a request to the disabled Application answered `503`, got\n{}",
        fresh.describe()
    );
}
