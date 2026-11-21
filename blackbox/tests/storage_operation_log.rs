//! Storage operations in the Operation log (CONTRACT.md, "Storage: The
//! Operation log"; ticket: every operation, allowed, refused or failed, has
//! an entry with the User, the Application, the Resource, the operation and
//! the path, and never the contents).
//!
//! Each field for each operation and outcome; the statement with the key
//! as a JSON string, cut after 1,024 bytes; no field holding a file's
//! contents, content type or size, nor a cursor; the `host-call` record;
//! the entries found by `?resource=` and by `vibe admin operation-log
//! --resource`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::symlink;

use blackbox::scenario::{Expected, administrator, lines};
use blackbox::storage::{
    FILES, MARKETING, Storage, assert_host_calls, assert_storage_entry, delete, get, head, list,
    put, statement_of, storage_expected, wanda,
};
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// One call as wanda through `files`; its answer and its one entry.
fn logged(s: &Storage, call: &Value) -> (String, Value, Value) {
    let (response, answer) = s.one(&wanda(), FILES, call);
    let entry = s.entry_of(&response);
    (response.request_id(), answer, entry)
}

fn allowed<'a>(id: &'a str) -> Expected<'a> {
    Expected {
        resource: Some(MARKETING),
        ..Expected::allowed(id, "wanda", FILES)
    }
}

#[test]
fn each_allowed_operation_has_its_entry_with_every_field() {
    let s = Storage::ready();
    let (id, _, entry) = logged(&s, &put(MARKETING, "reports/q4.pdf", b"pdf bytes", None));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(1),
                ..allowed(&id)
            },
            "storage.put",
            "put \"reports/q4.pdf\"",
        ),
        "an allowed put",
    );
    let (id, _, entry) = logged(&s, &head(MARKETING, "reports/q4.pdf"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(1),
                ..allowed(&id)
            },
            "storage.head",
            "head \"reports/q4.pdf\"",
        ),
        "a head that answered a file",
    );
    let (id, _, entry) = logged(&s, &head(MARKETING, "reports/none.pdf"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(0),
                ..allowed(&id)
            },
            "storage.head",
            "head \"reports/none.pdf\"",
        ),
        "a head that answered null",
    );
    let (id, _, entry) = logged(&s, &get(MARKETING, "reports/q4.pdf"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(1),
                ..allowed(&id)
            },
            "storage.get",
            "get \"reports/q4.pdf\"",
        ),
        "a get that answered a file",
    );
    let (id, _, entry) = logged(&s, &get(MARKETING, "reports/none.pdf"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(0),
                ..allowed(&id)
            },
            "storage.get",
            "get \"reports/none.pdf\"",
        ),
        "a get that answered null",
    );
    s.put_ok("reports/2025/a.pdf", b"x", None);
    s.put_ok("top.txt", b"x", None);
    let (id, _, entry) = logged(&s, &list(MARKETING, &json!({ "prefix": "reports/" })));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(2),
                ..allowed(&id)
            },
            "storage.list",
            "list \"reports/\"",
        ),
        "a list without a delimiter",
    );
    let (id, _, entry) = logged(&s, &list(MARKETING, &json!({ "delimiter": "/" })));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                rows: Some(2),
                ..allowed(&id)
            },
            "storage.list",
            "list \"\" delimiter \"/\"",
        ),
        "a list with a delimiter: one folder and one file",
    );
    let (id, _, entry) = logged(&s, &delete(MARKETING, "reports/q4.pdf"));
    assert_storage_entry(
        &entry,
        &storage_expected(allowed(&id), "storage.delete", "delete \"reports/q4.pdf\""),
        "an allowed delete",
    );
    assert!(
        entry["rows"].is_null(),
        "a delete's `rows` is null: {entry}"
    );
}

#[test]
fn a_failure_after_the_directory_was_touched_has_a_duration_and_one_before_has_none() {
    let s = Storage::ready();
    symlink(s.place.outside(), s.dir(MARKETING).join("etc")).unwrap();
    s.put_ok("a", b"x", None);
    // invalid-key: the directory never touched.
    let (id, _, entry) = logged(&s, &get(MARKETING, "../secret"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                resource: Some(MARKETING),
                ran: Some(false),
                ..Expected::failed(&id, "wanda", FILES, "invalid-key")
            },
            "storage.get",
            "get \"../secret\"",
        ),
        "an invalid key",
    );
    // not-reachable and key-conflict: the directory touched.
    for (call, statement, error) in [
        (
            get(MARKETING, "etc/passwd"),
            "get \"etc/passwd\"",
            "not-reachable",
        ),
        (
            put(MARKETING, "a/b", b"x", None),
            "put \"a/b\"",
            "key-conflict",
        ),
    ] {
        let (id, _, entry) = logged(&s, &call);
        let operation = if statement.starts_with("get") {
            "storage.get"
        } else {
            "storage.put"
        };
        assert_storage_entry(
            &entry,
            &storage_expected(
                Expected {
                    resource: Some(MARKETING),
                    ran: None,
                    ..Expected::failed(&id, "wanda", FILES, error)
                },
                operation,
                statement,
            ),
            error,
        );
        assert!(
            entry["duration_ms"].is_u64(),
            "{error}: expected `duration_ms`, from when the operation was handed to the directory, got {entry}"
        );
        assert!(
            entry["rows"].is_null(),
            "{error}: `rows` null unless it ended: {entry}"
        );
    }
}

#[test]
fn a_key_is_written_as_a_json_string_and_cut_after_1024_bytes() {
    let s = Storage::ready();
    for key in [
        "a\u{0}b",
        "bell\u{7}",
        "quote\"d",
        "back\\slash",
        "报告/第三季度.pdf",
        "a\u{202e}b",
        "del\u{7f}",
        "c1\u{85}\u{9f}",
        "lrm\u{200e}\u{61c}\u{2066}",
    ] {
        let (_, _, entry) = logged(&s, &head(MARKETING, key));
        assert_eq!(
            entry["statement"],
            json!(statement_of("head", key)),
            "expected the key written as a JSON string, every character the rule `character` names as `\\uXXXX`: {entry}"
        );
    }
    // The escapes, written out.
    assert_eq!(statement_of("head", "a\u{0}b"), "head \"a\\u0000b\"");
    assert_eq!(statement_of("head", "del\u{7f}"), "head \"del\\u007f\"");
    assert_eq!(
        statement_of("head", "c1\u{85}\u{9f}"),
        "head \"c1\\u0085\\u009f\""
    );
    assert_eq!(statement_of("head", "a\u{202e}b"), "head \"a\\u202eb\"");
    assert_eq!(
        statement_of("head", "quote\"d\\"),
        "head \"quote\\\"d\\\\\""
    );
    assert_eq!(
        statement_of("head", "报告/第三季度.pdf"),
        "head \"报告/第三季度.pdf\""
    );
    // The cut: `put "<the first 1,024 bytes>"...`.
    let long = "a".repeat(1025);
    let (_, _, entry) = logged(&s, &put(MARKETING, &long, b"x", None));
    assert_eq!(
        entry["statement"].as_str(),
        Some(format!("put \"{}\"...", "a".repeat(1024)).as_str()),
        "expected the key cut after its first 1,024 bytes, `...` after the closing quote"
    );
    // Cut at a character's end: 1,023 bytes and then a character of two.
    let wide = format!("{}éb", "a".repeat(1023));
    let (_, _, entry) = logged(&s, &put(MARKETING, &wide, b"x", None));
    assert_eq!(
        entry["statement"].as_str(),
        Some(format!("put \"{}\"...", "a".repeat(1023)).as_str()),
        "expected the key cut at the end of a character, before `é`"
    );
}

#[test]
fn no_field_holds_a_files_contents_content_type_or_size_nor_a_cursor() {
    let s = Storage::ready();
    let marker = format!("contents-{}", blackbox::shared_database::token());
    let content_type = format!("application/x-{marker}-type");
    let bytes = format!("{marker} {}", "z".repeat(4321));
    s.put_ok("secret/plan.txt", bytes.as_bytes(), Some(&content_type));
    s.as_writer(&get(MARKETING, "secret/plan.txt"));
    s.as_writer(&head(MARKETING, "secret/plan.txt"));
    for n in 0..3 {
        s.put_ok(&format!("secret/{n}.txt"), b"x", None);
    }
    let first = s.as_writer(&list(MARKETING, &json!({ "limit": 1 })));
    let cursor = first["ok"]["cursor"].as_str().unwrap().to_owned();
    s.as_writer(&list(MARKETING, &json!({ "limit": 1, "cursor": cursor })));
    let entries: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| e["resource"] == MARKETING)
        .collect();
    let size = bytes.len().to_string();
    let in_base64: String = blackbox::storage::b64(bytes.as_bytes())
        .chars()
        .take(40)
        .collect();
    for entry in &entries {
        let text = entry.to_string();
        for (what, needle) in [
            ("the contents", marker.as_str()),
            ("the contents in base64", in_base64.as_str()),
            ("the content type", content_type.as_str()),
            ("the cursor", cursor.as_str()),
        ] {
            assert!(
                !text.contains(needle),
                "expected no field to hold {what}: {entry}"
            );
        }
        for (key, value) in entry.as_object().unwrap() {
            assert!(
                !["size", "content_type", "body", "cursor"].contains(&key.as_str()),
                "expected no field `{key}` in an entry: {entry}"
            );
            if key != "id" && key != "rows" && key != "duration_ms" && key != "version" {
                assert_ne!(
                    value.to_string(),
                    size,
                    "expected no field to hold the size: {entry}"
                );
            }
        }
    }
    assert!(entries.len() >= 8, "{}", lines(&entries));
}

#[test]
fn the_host_call_record_names_the_operation() {
    let s = Storage::ready();
    let (response, _) = s.calls(
        &wanda(),
        FILES,
        &[
            put(MARKETING, "a.txt", b"x", None),
            head(MARKETING, "a.txt"),
            get(MARKETING, "a.txt"),
            list(MARKETING, &json!({})),
            delete(MARKETING, "a.txt"),
        ],
    );
    assert_host_calls(
        &s.vm,
        &response,
        &[
            "storage.put",
            "storage.head",
            "storage.get",
            "storage.list",
            "storage.delete",
        ],
        "five operations",
    );
    assert_eq!(
        s.entries_eventually(&response, 5).len(),
        5,
        "one entry each"
    );
}

#[test]
fn the_entries_are_found_by_resource_on_the_api_and_with_the_tool() {
    let s = Storage::ready();
    s.register_postgres("devops");
    s.put_ok("found.txt", b"x", None);
    s.as_writer(&get(MARKETING, "found.txt"));
    let page = s
        .admin()
        .get(&format!("/v1/operation-log?resource={MARKETING}"));
    page.expect_status(200);
    let page = page.json();
    let found = page["entries"].as_array().unwrap();
    assert!(
        found.len() >= 2 && found.iter().all(|e| e["resource"] == MARKETING),
        "expected the Storage entries found by `?resource={MARKETING}`, got {page}"
    );
    let machine = Machine::new();
    machine.sign_in(&s.vm, &administrator());
    let run = machine.run(&["admin", "operation-log", "--resource", MARKETING, "--json"]);
    assert_eq!(run.code, Some(0), "{}", run.describe());
    let printed: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(
        printed, page,
        "expected `vibe admin operation-log --resource {MARKETING} --json` to print the API's page"
    );
}

/// `duration_ms` of `entry`: a number when `handed` (the operation was
/// handed to the directory), else null.
fn assert_duration(entry: &Value, handed: bool, what: &str) {
    if handed {
        assert!(
            entry["duration_ms"].is_u64(),
            "{what}: expected `duration_ms` a number, from when the operation was handed to the directory, got {entry}"
        );
    } else {
        assert!(
            entry.get("duration_ms").is_some_and(Value::is_null),
            "{what}: expected `duration_ms` null, the operation ended before it was handed to the directory, got {entry}"
        );
    }
}

#[test]
fn duration_ms_is_a_number_once_the_operation_is_handed_to_the_directory_and_null_before() {
    let s = Storage::ready();
    s.install(
        "bodies",
        "storage-bodies.js",
        &[(MARKETING, "storage", "write")],
    );
    symlink(s.place.outside(), s.dir(MARKETING).join("etc")).unwrap();
    s.put_ok("a", b"x", None);
    // Handed to the directory: allowed, key-conflict, not-reachable.
    for (call, what) in [
        (put(MARKETING, "b.txt", b"x", None), "an allowed put"),
        (get(MARKETING, "b.txt"), "an allowed get"),
        (head(MARKETING, "none"), "a head answered null"),
        (list(MARKETING, &json!({})), "an allowed list"),
        (delete(MARKETING, "b.txt"), "an allowed delete"),
        (
            put(MARKETING, "a/b", b"x", None),
            "a put failed `key-conflict`",
        ),
        (get(MARKETING, "etc/passwd"), "a get failed `not-reachable`"),
    ] {
        let (_, _, entry) = logged(&s, &call);
        assert_duration(&entry, true, what);
    }
    // Ended before: a refusal, invalid-key, a put's `limit` `bytes`.
    let (response, answer) = s.one(&blackbox::storage::nobody(), FILES, &head(MARKETING, "a"));
    assert!(
        answer["refused"].is_object(),
        "expected a refusal, got {answer}"
    );
    assert_duration(&s.entry_of(&response), false, "a refusal");
    let (_, _, entry) = logged(&s, &get(MARKETING, "../secret"));
    assert_duration(&entry, false, "`invalid-key`");
    let (response, body) = s.post(
        &wanda(),
        "bodies",
        "/put-made",
        &json!({ "resource": MARKETING, "key": "big.bin", "size": blackbox::storage::BODY_BOUND + 1, "byte": 65 }),
    );
    assert_eq!(
        body["answer"]["error"]["limit"], "bytes",
        "expected a put of 8 MiB and one byte `limit` `bytes`, got {}",
        body["answer"]["error"]
    );
    assert_duration(&s.entry_of(&response), false, "a put's `limit` `bytes`");
    // unavailable: handed to a directory that cannot be used.
    let dir = s.dir(MARKETING);
    let blackbox::storage::Storage { vm, place } = s;
    let stopped = blackbox::storage::terminate(vm);
    std::fs::rename(&dir, dir.with_file_name("marketing-aside")).unwrap();
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    let (_, answer, entry) = logged(&s, &head(MARKETING, "a"));
    assert_eq!(
        answer["error"]["kind"], "unavailable",
        "expected `unavailable` with the directory moved away, got {answer}"
    );
    assert_duration(&entry, true, "a head failed `unavailable`");
}
