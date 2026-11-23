//! The five Storage operations through the raw interface (CONTRACT.md,
//! "Storage: Through the raw interface"): what each answers, a file's
//! metadata, a put that replaces whole, a delete answered the same whether
//! there was a file or not, content types given and by extension, what is
//! malformed (no entry, nothing touched), the order of failures, and
//! `list`: byte order, the delimiter, the limit and the cursor over pages
//! with files put and deleted between them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::HashSet;

use blackbox::api::assert_recent_rfc3339;
use blackbox::scenario::{assert_malformed, assert_refusal, lines};
use blackbox::storage::{
    FILES, MARKETING, Storage, assert_metadata, assert_not_reachable, assert_ok_metadata,
    assert_ok_null, assert_pages_hold_the_limit, assert_read, binary_with, delete, get, head, list,
    listed_names, mtime, op, place, put, tree, wanda,
};
use serde_json::{Value, json};

#[test]
fn put_head_get_and_delete_answer_as_the_contract_says() {
    let s = Storage::ready_streams();
    let bytes = binary_with("every byte");
    let written = s.put_hex_ok("reports/q3.pdf", &bytes, None);
    assert_ok_metadata(
        &written,
        "reports/q3.pdf",
        bytes.len(),
        "application/pdf",
        "the put",
    );
    assert_recent_rfc3339(&written["last_modified"], 120, "the put's last_modified");

    let headed = s.as_writer(&head(MARKETING, "reports/q3.pdf"));
    assert_ok_metadata(
        &headed,
        "reports/q3.pdf",
        bytes.len(),
        "application/pdf",
        "the head",
    );
    assert_eq!(
        headed["ok"], written,
        "expected head to answer the metadata the put answered"
    );
    let got = s.read("reports/q3.pdf");
    assert_read(&got, "reports/q3.pdf", &bytes, "application/pdf", "the get");
    assert_eq!(got["ok"]["last_modified"], written["last_modified"]);

    assert_ok_null(
        &s.as_writer(&head(MARKETING, "reports/q4.pdf")),
        "head of no file",
    );
    assert_ok_null(
        &s.as_writer(&get(MARKETING, "reports/q4.pdf")),
        "get of no file",
    );
    // `reports` is a folder (it holds `reports/q3.pdf`): not-a-file.
    assert_not_reachable(
        &s.as_writer(&get(MARKETING, "reports")),
        "not-a-file",
        "get of a key that is a folder",
    );

    let deleted = s.as_writer(&delete(MARKETING, "reports/q3.pdf"));
    assert_eq!(
        deleted,
        json!({ "ok": {} }),
        "expected a delete answered {{ ok: {{}} }}"
    );
    let again = s.as_writer(&delete(MARKETING, "reports/q3.pdf"));
    assert_eq!(
        again,
        json!({ "ok": {} }),
        "expected a delete of no file answered the same"
    );
    assert_ok_null(
        &s.as_writer(&head(MARKETING, "reports/q3.pdf")),
        "head after the delete",
    );
}

#[test]
fn a_put_replaces_the_file_whole_and_the_last_put_wins() {
    let s = Storage::ready_streams();
    s.put_ok("notes/a.txt", &"x".repeat(5000), Some("text/x-first"));
    let second = s.put_ok("notes/a.txt", "short", None);
    assert_metadata(
        &second,
        "notes/a.txt",
        5,
        "text/plain; charset=utf-8",
        "the second put",
    );
    assert_read(
        &s.read("notes/a.txt"),
        "notes/a.txt",
        b"short",
        "text/plain; charset=utf-8",
        "the file after a shorter put: nothing of the first left, its content type not kept",
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("notes/a.txt")).unwrap(),
        b"short"
    );
}

#[test]
fn a_content_type_given_is_kept_as_given() {
    let s = Storage::ready();
    let longest = format!("a/{}", "b".repeat(253));
    for given in [
        "Text/Plain; Charset=ISO-8859-1",
        "application/x.vendor+json",
        "image/png",
        "a/b",
        &longest,
        " / ",
    ] {
        let answer = s.as_writer(&put(MARKETING, "report.pdf", "x", Some(given)));
        assert_ok_metadata(&answer, "report.pdf", 1, given, given);
        assert_ok_metadata(
            &s.as_writer(&head(MARKETING, "report.pdf")),
            "report.pdf",
            1,
            given,
            given,
        );
    }
}

#[test]
fn a_content_type_of_the_wrong_shape_is_malformed() {
    let s = Storage::ready();
    let too_long = format!("a/{}", "b".repeat(254));
    for (what, given) in [
        ("empty", json!("")),
        ("no slash", json!("textplain")),
        ("256 bytes", json!(too_long)),
        ("a line end", json!("text/plain\n")),
        ("a tab", json!("text/\tplain")),
        ("not ASCII", json!("text/plaîn")),
        ("DEL", json!("text/plain\u{7f}")),
        ("a number", json!(5)),
        ("null", Value::Null),
    ] {
        let (response, answer) = s.one(
            &wanda(),
            FILES,
            &op(
                "storage.put",
                &json!({ "resource": MARKETING, "key": "c.txt", "body": "x", "content_type": given }),
            ),
        );
        assert_malformed(&answer, &format!("a content type: {what}"));
        assert!(s.entries_of(&response).is_empty(), "{what}: no entry");
    }
    assert!(!s.dir(MARKETING).join("c.txt").exists());
}

#[test]
fn without_a_content_type_the_keys_last_extension_gives_it_in_any_letter_case() {
    let s = Storage::ready_streams();
    let table = [
        ("a.txt", "text/plain; charset=utf-8"),
        ("a.html", "text/html; charset=utf-8"),
        ("a.htm", "text/html; charset=utf-8"),
        ("a.css", "text/css"),
        ("a.js", "text/javascript"),
        ("a.json", "application/json"),
        ("a.csv", "text/csv"),
        ("a.md", "text/markdown"),
        ("a.xml", "application/xml"),
        ("a.pdf", "application/pdf"),
        ("a.png", "image/png"),
        ("a.jpg", "image/jpeg"),
        ("a.jpeg", "image/jpeg"),
        ("a.gif", "image/gif"),
        ("a.webp", "image/webp"),
        ("a.svg", "image/svg+xml"),
        ("a.zip", "application/zip"),
        (
            "a.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            "a.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        (
            "a.pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        ("UPPER.PDF", "application/pdf"),
        ("Mixed.JsOn", "application/json"),
        ("archive.tar.gz", "application/octet-stream"),
        ("report.txt.pdf", "application/pdf"),
        ("no-extension", "application/octet-stream"),
        ("data.bin", "application/octet-stream"),
        ("folder.pdf/inside", "application/octet-stream"),
    ];
    for (key, wanted) in table {
        let answer = s.as_writer(&put(MARKETING, key, "x", None));
        assert_ok_metadata(&answer, key, 1, wanted, key);
        assert_read(&s.read(key), key, b"x", wanted, key);
    }
}

#[test]
fn the_metadata_time_is_the_files_time_of_last_change_to_the_second() {
    let s = Storage::ready();
    let written = s.put_ok("t.txt", "time", None);
    let file = s.dir(MARKETING).join("t.txt");
    let seconds = blackbox::api::parse_rfc3339(written["last_modified"].as_str().unwrap()).unwrap();
    assert_eq!(
        seconds,
        mtime(&file),
        "expected `last_modified` to be the file's time of modification, to the second"
    );
}

#[test]
fn arguments_of_the_wrong_shape_are_malformed_with_no_entry_and_nothing_touched() {
    let s = Storage::ready_streams();
    s.put_ok("kept.txt", "kept", None);
    let before = tree(&s.place.base);
    let key = |op_name: &str, more: Value| {
        let mut args = more;
        args["resource"] = json!(MARKETING);
        op(op_name, &args)
    };
    let mut cases: Vec<(String, Value)> = Vec::new();
    for operation in [
        "storage.head",
        "storage.get",
        "storage.put",
        "storage.delete",
        "storage.list",
    ] {
        for (what, args) in [
            ("arguments a string", json!("marketing")),
            ("arguments an array", json!([MARKETING, "kept.txt"])),
            ("arguments null", Value::Null),
            (
                "resource missing",
                json!({ "key": "kept.txt", "body": "x" }),
            ),
            (
                "resource a number",
                json!({ "resource": 1, "key": "kept.txt", "body": "x" }),
            ),
            (
                "own given",
                json!({ "resource": MARKETING, "own": true, "key": "kept.txt", "body": "x" }),
            ),
            (
                "own false given",
                json!({ "resource": MARKETING, "own": false, "key": "kept.txt", "body": "x" }),
            ),
            (
                "as another word",
                json!({ "resource": MARKETING, "as": "admin", "key": "kept.txt", "body": "x" }),
            ),
            (
                "as a number",
                json!({ "resource": MARKETING, "as": 1, "key": "kept.txt", "body": "x" }),
            ),
        ] {
            cases.push((format!("{operation}: {what}"), op(operation, &args)));
        }
    }
    for operation in [
        "storage.head",
        "storage.get",
        "storage.put",
        "storage.delete",
    ] {
        cases.push((
            format!("{operation}: key missing"),
            key(operation, json!({ "body": "x" })),
        ));
        cases.push((
            format!("{operation}: key a number"),
            key(operation, json!({ "key": 5, "body": "x" })),
        ));
        cases.push((
            format!("{operation}: key null"),
            key(operation, json!({ "key": null, "body": "x" })),
        ));
    }
    cases.push((
        "put: body missing".into(),
        key("storage.put", json!({ "key": "kept.txt" })),
    ));
    cases.push((
        "put: body a number".into(),
        key("storage.put", json!({ "key": "kept.txt", "body": 5 })),
    ));
    cases.push((
        "put: body an array of bytes".into(),
        key("storage.put", json!({ "key": "kept.txt", "body": [1, 2] })),
    ));
    for (what, more) in [
        ("prefix a number", json!({ "prefix": 5 })),
        ("delimiter a comma", json!({ "delimiter": "," })),
        ("delimiter empty", json!({ "delimiter": "" })),
        ("delimiter a number", json!({ "delimiter": 1 })),
        ("limit 0", json!({ "limit": 0 })),
        ("limit 1001", json!({ "limit": 1001 })),
        ("limit -1", json!({ "limit": -1 })),
        ("limit 1.5", json!({ "limit": 1.5 })),
        ("limit a string", json!({ "limit": "10" })),
        ("cursor a number", json!({ "cursor": 5 })),
        ("cursor not one answered", json!({ "cursor": "made-up" })),
        ("cursor empty", json!({ "cursor": "" })),
    ] {
        cases.push((format!("list: {what}"), list(MARKETING, &more)));
    }
    let calls: Vec<Value> = cases.iter().map(|(_, c)| c.clone()).collect();
    let (response, answers) = s.calls(&wanda(), FILES, &calls);
    for ((what, _), answer) in cases.iter().zip(&answers) {
        assert_malformed(answer, what);
    }
    let entries = s.entries_of(&response);
    assert!(
        entries.is_empty(),
        "expected no Operation log entry for a malformed operation, got\n{}",
        lines(&entries)
    );
    let records = s.vm.records_of(&response);
    for call in records.host_calls() {
        assert_eq!(
            (call["outcome"].as_str(), call["reason"].as_str()),
            (Some("refused"), Some("malformed")),
            "expected each malformed call's `host-call` record refused `malformed`: {call}"
        );
    }
    assert_eq!(records.host_calls().len(), cases.len());
    assert_eq!(tree(&s.place.base), before, "expected nothing touched");
    assert_read(
        &s.read("kept.txt"),
        "kept.txt",
        b"kept",
        "text/plain; charset=utf-8",
        "the file after the malformed calls",
    );
}

#[test]
fn a_cursor_is_good_only_for_the_list_of_this_resource_prefix_and_delimiter() {
    let s = Storage::ready();
    s.register("other");
    s.grant("other", blackbox::storage::FILE_WRITERS, "write");
    s.install(
        "both",
        "operations.js",
        &[
            (MARKETING, "storage", "write"),
            ("other", "storage", "write"),
        ],
    );
    for key in ["a/1", "a/2", "a/3", "b/1"] {
        s.put_ok(key, "x", None);
        s.one(&wanda(), "both", &put("other", key, "x", None));
    }
    let first = s.as_writer(&list(MARKETING, &json!({ "prefix": "a/", "limit": 1 })));
    let cursor = first["ok"]["cursor"].as_str().unwrap().to_owned();
    let good = s.as_writer(&list(
        MARKETING,
        &json!({ "prefix": "a/", "limit": 1, "cursor": cursor }),
    ));
    assert!(
        good["ok"].is_object(),
        "the cursor given back as it came: {good}"
    );
    for (what, call) in [
        (
            "another prefix",
            list(MARKETING, &json!({ "prefix": "b/", "cursor": cursor })),
        ),
        ("no prefix", list(MARKETING, &json!({ "cursor": cursor }))),
        (
            "with a delimiter",
            list(
                MARKETING,
                &json!({ "prefix": "a/", "delimiter": "/", "cursor": cursor }),
            ),
        ),
        (
            "changed by one character",
            list(
                MARKETING,
                &json!({ "prefix": "a/", "cursor": format!("{cursor}x") }),
            ),
        ),
    ] {
        assert_malformed(&s.as_writer(&call), &format!("a cursor with {what}"));
    }
    let other = s
        .one(
            &wanda(),
            "both",
            &list("other", &json!({ "prefix": "a/", "cursor": cursor })),
        )
        .1;
    assert_malformed(&other, "a cursor of another Resource");
}

#[test]
fn as_user_is_taken_and_as_application_is_not_own_database() {
    let s = Storage::ready();
    let mut args = json!({ "resource": MARKETING, "key": "u.txt", "as": "user", "body": "x" });
    assert_ok_metadata(
        &s.as_writer(&op("storage.put", &args)),
        "u.txt",
        1,
        "text/plain; charset=utf-8",
        "as: user",
    );
    for operation in [
        "storage.head",
        "storage.get",
        "storage.put",
        "storage.delete",
        "storage.list",
    ] {
        args["as"] = json!("application");
        args["key"] = json!("app.txt");
        let answer = s.as_writer(&op(operation, &args));
        assert_refusal(
            &answer,
            "not-own-database",
            &json!({ "resource": MARKETING }),
            &format!("{operation} as the Application"),
        );
    }
    assert!(!s.dir(MARKETING).join("app.txt").exists());
}

#[test]
fn arguments_an_operation_does_not_have_are_ignored() {
    let s = Storage::ready();
    let extra = json!({
        "resource": MARKETING, "key": "x.txt", "body": "x",
        "sql": "drop table x", "params": [1], "values": "text", "tx": "t",
        "directory": "/etc", "user": "it-admin", "limit": 3, "prefix": "zzz",
    });
    let answer = s.as_writer(&op("storage.put", &extra));
    assert_ok_metadata(
        &answer,
        "x.txt",
        1,
        "text/plain; charset=utf-8",
        "a put with more",
    );
    let answer = s.as_writer(&op("storage.head", &extra));
    assert_ok_metadata(
        &answer,
        "x.txt",
        1,
        "text/plain; charset=utf-8",
        "a head with more",
    );
    let listed = s.as_writer(&op(
        "storage.list",
        &json!({ "resource": MARKETING, "key": "../x", "body": 5, "content_type": 5 }),
    ));
    assert_eq!(
        listed_names(&listed["ok"]).0,
        ["x.txt"],
        "expected a list to ignore `key`, `body` and `content_type`, got {listed}"
    );
}

#[test]
fn failures_come_in_their_order() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    std::os::unix::fs::symlink(s.place.outside(), dir.join("etc")).unwrap();
    // malformed before a refusal: a User with no Grant, a body that is none
    // of the four.
    let answer = s
        .one(
            &blackbox::storage::nobody(),
            FILES,
            &op(
                "storage.put",
                &json!({ "resource": MARKETING, "key": "../x", "body": 5 }),
            ),
        )
        .1;
    assert_malformed(&answer, "malformed and refused");
    // A refusal before invalid-key.
    let answer = s
        .one(&blackbox::storage::nobody(), FILES, &get(MARKETING, "../x"))
        .1;
    assert_eq!(
        answer["refused"]["refusal"]["reason"], "no-grant",
        "expected a refusal before the key is looked at, got {answer}"
    );
    // A get or head of a folder is not-a-file, not null.
    s.put_ok("folder/file.txt", "f", None);
    for call in [get(MARKETING, "folder"), head(MARKETING, "folder")] {
        assert_not_reachable(
            &s.as_writer(&call),
            "not-a-file",
            "a get or head of a folder",
        );
    }
    assert!(tree(&s.place.outside()).is_empty());
}

/// Every key of a list's pages, files and folders, in the order answered.
fn every_name(pages: &[Value]) -> Vec<String> {
    let mut names = Vec::new();
    for page in pages {
        let (files, folders) = listed_names(page);
        let mut both: Vec<String> = files.into_iter().chain(folders).collect();
        both.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        names.extend(both);
    }
    names
}

#[test]
fn list_answers_keys_in_the_byte_order_of_their_utf8() {
    let s = Storage::ready();
    let keys = ["é", "a0", "a/b", "a.b/x", "a-b", "Z", "a/c/d", "b"];
    for key in keys {
        s.put_ok(key, key, None);
    }
    let listed = s.as_writer(&list(MARKETING, &json!({})));
    let ok = &listed["ok"];
    let (files, folders) = listed_names(ok);
    assert_eq!(
        files,
        ["Z", "a-b", "a.b/x", "a/b", "a/c/d", "a0", "b", "é"],
        "expected every key in the byte order of its UTF-8, got {listed}"
    );
    assert!(
        folders.is_empty(),
        "no folders without a delimiter: {listed}"
    );
    assert!(ok["cursor"].is_null(), "nothing follows: {listed}");
    for file in ok["files"].as_array().unwrap() {
        let key = file["key"].as_str().unwrap();
        assert_metadata(
            file,
            key,
            key.len(),
            "application/octet-stream",
            "a listed file",
        );
    }
    let reports = s.as_writer(&list(MARKETING, &json!({ "prefix": "a" })));
    assert_eq!(
        listed_names(&reports["ok"]).0,
        ["a-b", "a.b/x", "a/b", "a/c/d", "a0"],
        "a prefix that is not a folder: {reports}"
    );
    let none = s.as_writer(&list(MARKETING, &json!({ "prefix": "nothing/" })));
    assert_eq!(
        none,
        json!({ "ok": { "files": [], "folders": [], "cursor": null } }),
        "expected an empty page when nothing begins with the prefix"
    );
}

#[test]
fn with_the_delimiter_keys_roll_up_into_folders_listed_once_in_byte_order_with_the_files() {
    let s = Storage::ready();
    for key in [
        "reports/q3.pdf",
        "reports/2025/q1.pdf",
        "reports/2025/q2.pdf",
        "reports/a-b.txt",
        "reports/a/x.txt",
        "reports/a0.txt",
        "top.txt",
        "Zed/x",
    ] {
        s.put_ok(key, "x", None);
    }
    let root = s.as_writer(&list(MARKETING, &json!({ "delimiter": "/" })));
    let (files, folders) = listed_names(&root["ok"]);
    assert_eq!(files, ["top.txt"], "{root}");
    assert_eq!(folders, ["Zed/", "reports/"], "{root}");
    let inside = s.as_writer(&list(
        MARKETING,
        &json!({ "prefix": "reports/", "delimiter": "/" }),
    ));
    let (files, folders) = listed_names(&inside["ok"]);
    assert_eq!(
        files,
        ["reports/a-b.txt", "reports/a0.txt", "reports/q3.pdf"],
        "{inside}"
    );
    assert_eq!(folders, ["reports/2025/", "reports/a/"], "{inside}");
    // Folders and files together in byte order, each counting once.
    let pages = s.every_page(&json!({ "prefix": "reports/", "delimiter": "/" }), 2);
    assert_eq!(
        every_name(&pages),
        [
            "reports/2025/",
            "reports/a-b.txt",
            "reports/a/",
            "reports/a0.txt",
            "reports/q3.pdf"
        ],
        "expected folders and files in byte order across pages of 2:\n{}",
        lines(&pages)
    );
    for page in &pages {
        let (f, d) = listed_names(page);
        assert!(
            f.len() + d.len() <= 2,
            "a page holds at most `limit` names: {page}"
        );
    }
    let partial = s.as_writer(&list(
        MARKETING,
        &json!({ "prefix": "rep", "delimiter": "/" }),
    ));
    assert_eq!(
        listed_names(&partial["ok"]),
        (vec![], vec!["reports/".to_owned()]),
        "a prefix inside a segment rolls up to `prefix` up to and with the `/`: {partial}"
    );
}

#[test]
fn the_limit_is_100_without_it_and_a_page_holds_at_most_limit() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    for n in 0..105 {
        place(&dir.join(format!("f{n:03}.txt")), b"x");
    }
    // At most `limit`, the cursor null only at the end; a page answered
    // in under 4 seconds while more follow holds exactly the limit (only a
    // list that stopped looking after 5 seconds is short).
    let pages = s.timed_pages(&json!({}), None);
    assert!(
        pages[0].0["cursor"].is_string(),
        "more follow, so a cursor: {}",
        pages[0].0
    );
    assert_pages_hold_the_limit(&pages, 100, "without a limit");
    let all: Vec<String> = pages.iter().flat_map(|(p, _)| listed_names(p).0).collect();
    let expected: Vec<String> = (0..105).map(|n| format!("f{n:03}.txt")).collect();
    assert_eq!(all, expected, "the pages together, in order");
    let ones = s.timed_pages(&json!({}), Some(1));
    assert_pages_hold_the_limit(&ones, 1, "`limit` 1");
    let ones: Vec<Value> = ones.into_iter().map(|(p, _)| p).collect();
    assert_eq!(every_name(&ones), expected, "pages of `limit` 1 together");
}

#[test]
fn a_cursor_answers_the_next_page_also_when_files_were_put_and_deleted_meanwhile() {
    let s = Storage::ready();
    for key in ["b", "d", "f", "h", "j", "l"] {
        s.put_ok(key, "x", None);
    }
    // Pages of at most 2 until `d` is answered (a page may be short).
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    while !seen.contains(&"d".to_owned()) {
        let mut args = json!({ "limit": 2 });
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let page = s.as_writer(&list(MARKETING, &args));
        let names = listed_names(&page["ok"]).0;
        assert!(names.len() <= 2, "at most `limit`: {page}");
        seen.extend(names);
        cursor = page["ok"]["cursor"].as_str().map(str::to_owned);
        let c = cursor
            .as_deref()
            .unwrap_or_else(|| panic!("more follow `d`, so a cursor: {page}"));
        assert!(
            c.len() <= 2048,
            "a cursor is at most 2,048 bytes, got {}",
            c.len()
        );
    }
    let before = seen.len();
    let last = seen.last().unwrap().clone();
    // Meanwhile: put behind the cursor and ahead of it, delete ahead.
    s.put_ok("a", "x", None);
    s.put_ok("c", "x", None);
    s.put_ok("e", "x", None);
    s.as_writer(&delete(MARKETING, "f"));
    s.as_writer(&delete(MARKETING, "d"));
    while let Some(c) = cursor {
        let page = s.as_writer(&list(MARKETING, &json!({ "limit": 2, "cursor": c })));
        assert!(page["ok"].is_object(), "expected a page, got {page}");
        seen.extend(listed_names(&page["ok"]).0);
        cursor = page["ok"]["cursor"].as_str().map(str::to_owned);
        assert!(seen.len() < 50, "the pages do not end: {seen:?}");
    }
    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a page never repeats a key: {seen:?}"
    );
    assert!(
        !seen.contains(&"a".to_owned()) && !seen.contains(&"c".to_owned()),
        "a key put meanwhile behind the cursor is not answered: {seen:?}"
    );
    // `e` and `f` are ahead of the cursor when the last key answered is
    // `d`; a short page may have stopped there or a page later.
    if last.as_str() < "e" {
        assert!(
            seen.contains(&"e".to_owned()),
            "a key put ahead is answered: {seen:?}"
        );
    }
    if last.as_str() < "f" {
        assert!(
            !seen.contains(&"f".to_owned()),
            "a key deleted ahead is not answered: {seen:?}"
        );
    }
    // The rest: what is there now beyond the last key answered, in byte
    // order.
    let rest: Vec<&str> = ["a", "b", "c", "e", "h", "j", "l"]
        .into_iter()
        .filter(|k| *k > last.as_str())
        .collect();
    assert_eq!(
        &seen[before..],
        rest,
        "expected the rest in byte order: {seen:?}"
    );
}

#[test]
fn a_folder_answered_on_one_page_is_not_answered_again_on_the_next() {
    let s = Storage::ready();
    for key in ["a.txt", "b/1", "b/2", "b/3", "c.txt", "d/1"] {
        s.put_ok(key, "x", None);
    }
    // Pages of at most 2 until `b/` is answered (a page may be short).
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let page_of = |cursor: &Option<String>| {
        let mut args = json!({ "delimiter": "/", "limit": 2 });
        if let Some(c) = cursor {
            args["cursor"] = json!(c);
        }
        let page = s.as_writer(&list(MARKETING, &args));
        let names = every_name(std::slice::from_ref(&page["ok"]));
        assert!(names.len() <= 2, "at most `limit`: {page}");
        (names, page["ok"]["cursor"].as_str().map(str::to_owned))
    };
    while !seen.contains(&"b/".to_owned()) {
        let (names, next) = page_of(&cursor);
        seen.extend(names);
        cursor = next;
        assert!(cursor.is_some(), "more follow `b/`, so a cursor: {seen:?}");
    }
    // A file put meanwhile in the folder already answered.
    s.put_ok("b/0", "x", None);
    s.put_ok("b/9", "x", None);
    while cursor.is_some() {
        let (names, next) = page_of(&cursor);
        seen.extend(names);
        cursor = next;
        assert!(seen.len() < 50, "the pages do not end: {seen:?}");
    }
    assert_eq!(
        seen,
        ["a.txt", "b/", "c.txt", "d/"],
        "expected the pages after the folder `b/` not to answer `b/` again"
    );
}

#[test]
fn the_platforms_own_names_are_never_listed() {
    let s = Storage::ready();
    s.put_ok("a.txt", "x", None);
    for args in [
        json!({}),
        json!({ "delimiter": "/" }),
        json!({ "prefix": "." }),
    ] {
        let listed = s.as_writer(&list(MARKETING, &args));
        let (files, folders) = listed_names(&listed["ok"]);
        assert!(
            files.iter().chain(&folders).all(|n| !n.contains(".vibe-")),
            "expected `.vibe-resource` and `.vibe-staging/` never listed, got {listed}"
        );
    }
}
