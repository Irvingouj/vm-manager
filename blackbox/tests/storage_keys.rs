//! Keys (CONTRACT.md, "Storage: Keys"): each rule a key must keep, named in
//! `invalid-key` after the operation was allowed, with nothing read or
//! written; the first rule of the table when several are broken; the
//! bounds taken exactly; letter case; what else is taken; a file and a
//! folder of the same name; and what a prefix of `list` is.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::scenario::Expected;
use blackbox::storage::{
    FILES, MARKETING, Storage, assert_got, assert_invalid_key, assert_ok_metadata, assert_ok_null,
    assert_storage_entry, delete, get, head, list, listed_names, put, statement_of,
    storage_expected, tree, wanda,
};
use serde_json::{Value, json};

/// Keys that break one rule each, with the rule.
fn broken() -> Vec<(String, &'static str)> {
    let mut keys = vec![
        (String::new(), "empty"),
        ("k".repeat(1025), "too-long"),
        (["d"; 65].join("/"), "too-deep"),
        ("/a".to_owned(), "empty-segment"),
        ("a/".to_owned(), "empty-segment"),
        ("a//b".to_owned(), "empty-segment"),
        ("s".repeat(256), "segment-too-long"),
        (format!("a/{}", "s".repeat(256)), "segment-too-long"),
        (".".to_owned(), "dot-segment"),
        ("..".to_owned(), "dot-segment"),
        ("a/./b".to_owned(), "dot-segment"),
        ("a/../b".to_owned(), "dot-segment"),
        ("a\\b".to_owned(), "character"),
        ("cafe\u{301}".to_owned(), "not-nfc"),
        ("\u{212b}".to_owned(), "not-nfc"),
        (".vibe-resource".to_owned(), "reserved"),
        (".vibe-staging/x".to_owned(), "reserved"),
        ("a/.vibe-anything".to_owned(), "reserved"),
    ];
    for c in [
        '\u{0}', '\u{1}', '\u{1f}', '\u{7f}', '\u{80}', '\u{9f}', '\u{61c}', '\u{200e}',
        '\u{200f}', '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}',
    ] {
        keys.push((format!("a{c}b"), "character"));
    }
    keys
}

#[test]
fn a_key_that_breaks_a_rule_fails_invalid_key_naming_it_and_has_its_failed_entry() {
    let s = Storage::ready();
    let before = tree(&s.place.base);
    for (key, rule) in broken() {
        for (operation, word, call) in [
            ("storage.head", "head", head(MARKETING, &key)),
            ("storage.get", "get", get(MARKETING, &key)),
            ("storage.put", "put", put(MARKETING, &key, b"x", None)),
            ("storage.delete", "delete", delete(MARKETING, &key)),
        ] {
            let (response, answer) = s.one(&wanda(), FILES, &call);
            let what = format!("{operation} of {key:?}");
            assert_invalid_key(&answer, rule, &what);
            let entry = s.entry_of(&response);
            if key.len() <= 1024 {
                let id = response.request_id();
                let statement = statement_of(word, &key);
                assert_storage_entry(
                    &entry,
                    &storage_expected(
                        Expected {
                            resource: Some(MARKETING),
                            ran: Some(false),
                            ..Expected::failed(&id, "wanda", FILES, "invalid-key")
                        },
                        operation,
                        &statement,
                    ),
                    &what,
                );
            } else {
                assert_eq!(entry["outcome"], "failed", "{what}: {entry}");
                assert_eq!(entry["error"], "invalid-key", "{what}: {entry}");
            }
        }
    }
    assert_eq!(
        tree(&s.place.base),
        before,
        "expected nothing read into being or written anywhere by a key that breaks a rule"
    );
}

#[test]
fn when_several_rules_are_broken_the_first_of_the_table_is_named() {
    let s = Storage::ready();
    let deep_and_long = format!("{}/{}", ["d"; 64].join("/"), "x".repeat(1000));
    for (key, rule) in [
        (deep_and_long, "too-long"),
        (format!("{}/", ["d"; 65].join("/")), "too-deep"),
        (format!("/{}", "s".repeat(256)), "empty-segment"),
        (format!("{}/..", "s".repeat(256)), "segment-too-long"),
        ("../a\u{0}".to_owned(), "dot-segment"),
        ("a\u{0}/cafe\u{301}".to_owned(), "character"),
        ("cafe\u{301}/.vibe-x".to_owned(), "not-nfc"),
        ("//..".to_owned(), "empty-segment"),
    ] {
        let answer = s.as_writer(&put(MARKETING, &key, b"x", None));
        assert_invalid_key(&answer, rule, &format!("{key:?}"));
    }
}

#[test]
fn the_bounds_of_a_key_are_taken_exactly() {
    let s = Storage::ready();
    let segment = "s".repeat(255);
    let longest = [
        "a".repeat(255),
        "b".repeat(255),
        "c".repeat(255),
        "d".repeat(250),
        "e".repeat(5),
    ]
    .join("/");
    assert_eq!(longest.len(), 1024, "the harness's own check");
    let deepest = ["d"; 64].join("/");
    // 1,024 bytes whose last character is two bytes long.
    let wide = format!(
        "{}/{}/é",
        ["a".repeat(255), "b".repeat(255), "c".repeat(255)].join("/"),
        "d".repeat(253)
    );
    assert_eq!(wide.len(), 1024, "the harness's own check");
    for key in [segment.as_str(), &longest, &deepest, &wide] {
        let answer = s.as_writer(&put(MARKETING, key, b"edge", None));
        assert_ok_metadata(
            &answer,
            key,
            4,
            "application/octet-stream",
            &format!("a key at the bound ({} bytes)", key.len()),
        );
        assert_got(
            &s.as_writer(&get(MARKETING, key)),
            key,
            b"edge",
            "application/octet-stream",
            "read back",
        );
        assert_eq!(
            std::fs::read(s.dir(MARKETING).join(key)).unwrap(),
            b"edge",
            "expected the file at <dir>/<key>"
        );
        s.as_writer(&delete(MARKETING, key));
    }
}

#[test]
fn letter_case_matters_and_everything_else_is_taken() {
    let s = Storage::ready();
    s.put_ok("Report.pdf", b"upper", None);
    s.put_ok("report.pdf", b"lower", None);
    assert_got(
        &s.as_writer(&get(MARKETING, "Report.pdf")),
        "Report.pdf",
        b"upper",
        "application/pdf",
        "Report.pdf",
    );
    assert_got(
        &s.as_writer(&get(MARKETING, "report.pdf")),
        "report.pdf",
        b"lower",
        "application/pdf",
        "report.pdf",
    );
    for key in [
        "with space.txt",
        "hash#1.txt",
        "per%20cent.txt",
        "and&or.txt",
        "[bracket].txt",
        "it's.txt",
        ".hidden",
        "a/.hidden/b.txt",
        "报告/第三季度.pdf",
        "café.txt",
        "emoji 📄.txt",
        "...",
        "a/.../b",
        ".vibe",
        "x.vibe-y",
        "tab-less ~!@$^*()_+={}|;:,<>?",
    ] {
        let answer = s.as_writer(&put(MARKETING, key, key.as_bytes(), None));
        assert!(
            answer["ok"].is_object(),
            "expected the key {key:?} taken, got {answer}"
        );
        assert_eq!(answer["ok"]["key"], json!(key));
        assert_eq!(
            std::fs::read(s.dir(MARKETING).join(key)).unwrap(),
            key.as_bytes(),
            "expected {key:?} on the disk under its own name"
        );
    }
}

#[test]
fn a_file_and_a_folder_of_the_same_name_cannot_both_exist() {
    let s = Storage::ready();
    s.put_ok("a", b"a file", None);
    s.put_ok("b/c", b"in a folder", None);
    let before = tree(&s.place.base);
    for (key, what) in [
        ("a/b", "a key beneath a file"),
        ("a/b/c", "a key two beneath a file"),
        ("b", "a key that is a folder"),
    ] {
        let (response, answer) = s.one(&wanda(), FILES, &put(MARKETING, key, b"x", None));
        assert_eq!(
            answer["error"]["kind"], "key-conflict",
            "{what}: expected {{ error: {{ kind: \"key-conflict\", message }} }}, got {answer}"
        );
        assert!(
            answer["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{what}: expected a message, got {answer}"
        );
        let entry = s.entry_of(&response);
        assert_eq!(
            (entry["outcome"].as_str(), entry["error"].as_str()),
            (Some("failed"), Some("key-conflict")),
            "{what}: {entry}"
        );
    }
    assert_eq!(
        tree(&s.place.base),
        before,
        "expected nothing written by a conflict"
    );
    // Gone, the folder makes room for a file, and the file for a folder.
    s.as_writer(&delete(MARKETING, "b/c"));
    s.put_ok("b", b"now a file", None);
    s.as_writer(&delete(MARKETING, "a"));
    s.put_ok("a/b", b"now in a folder", None);
}

/// A list on `prefix`, which must fail `invalid-key` with `rule` (or
/// answer a page when `rule` is `None`).
fn list_prefix(s: &Storage, prefix: &str, rule: Option<&str>) -> Value {
    let (response, answer) = s.one(
        &wanda(),
        FILES,
        &list(MARKETING, &json!({ "prefix": prefix })),
    );
    match rule {
        Some(rule) => {
            assert_invalid_key(&answer, rule, &format!("the prefix {prefix:?}"));
            let entry = s.entry_of(&response);
            assert_eq!(
                (entry["outcome"].as_str(), entry["error"].as_str()),
                (Some("failed"), Some("invalid-key")),
                "the entry of a list on the prefix {prefix:?}: {entry}"
            );
        }
        None => assert!(
            answer["ok"].is_object(),
            "expected the prefix {prefix:?} taken, got {answer}"
        ),
    }
    answer
}

#[test]
fn a_prefix_is_one_that_with_a_character_after_it_is_a_key_that_is_taken() {
    let s = Storage::ready();
    for key in ["reports/q3.pdf", "报告/a.txt", "x.txt"] {
        s.put_ok(key, b"x", None);
    }
    for prefix in ["", "reports/", "rep", "报告/", "a/", ".", ".."] {
        list_prefix(&s, prefix, None);
    }
    assert_eq!(
        listed_names(&list_prefix(&s, "报告/", None)["ok"]).0,
        ["报告/a.txt"]
    );
    // 1,023 bytes: with one more character, a key of 1,024 bytes.
    let longest_prefix = format!(
        "{}/{}/",
        ["a".repeat(255), "b".repeat(255), "c".repeat(255)].join("/"),
        "d".repeat(254)
    );
    assert_eq!(longest_prefix.len(), 1023, "the harness's own check");
    list_prefix(&s, &longest_prefix, None);
    // 1,024 bytes: past the bound of 1,023.
    list_prefix(&s, &format!("{longest_prefix}e"), Some("too-long"));
    for (prefix, rule) in [
        ("../", "dot-segment"),
        ("/x", "empty-segment"),
        ("a//", "empty-segment"),
        ("a/./", "dot-segment"),
        (".vibe-", "reserved"),
        (".vibe-staging/", "reserved"),
        ("a\u{0}", "character"),
        ("a\\", "character"),
        ("cafe\u{301}", "not-nfc"),
    ] {
        list_prefix(&s, prefix, Some(rule));
    }
    list_prefix(&s, &"p".repeat(1025), Some("too-long"));
    list_prefix(&s, &"s".repeat(255), Some("segment-too-long"));
    list_prefix(&s, &format!("{}/", ["d"; 64].join("/")), Some("too-deep"));
    assert_ok_null(
        &s.as_writer(&head(MARKETING, "rep")),
        "a prefix is not a file",
    );
}
