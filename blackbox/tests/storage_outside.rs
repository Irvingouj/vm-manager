//! Nothing reaches outside a Storage Resource (CONTRACT.md, "Storage:
//! Nothing reaches outside"; ticket: no path reaches outside the Resource:
//! `..`, absolute paths, symbolic links out of it, and hostile names are
//! refused, and a test with a hostile fixture Application shows it; two
//! Resources never see each other's files).
//!
//! The hostile fixture `fixtures/storage-hostile.js` holds its own keys
//! and sends each through the five operations as a User holding `write`;
//! the test plants links, a FIFO and a folder in the directory itself; and
//! two Resources on sibling directories are written through each other.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

use blackbox::assert_api_error;
use blackbox::scenario::lines;
use blackbox::storage::{
    FILE_WRITERS, MARKETING, Storage, assert_invalid_key, assert_not_reachable, assert_ok_null,
    delete, get, head, list, listed_names, mkfifo, place, put, tree, wanda,
};
use serde_json::{Value, json};

/// The rule each hostile key of the fixture breaks, and the rule of the
/// key `<it>x` (what a list with it as its prefix is checked by), `None`
/// when that is a key that is taken.
const RULES: [(&str, &str, Option<&str>); 22] = [
    ("dotdot-secret", "dot-segment", Some("dot-segment")),
    ("climb", "dot-segment", Some("dot-segment")),
    ("absolute", "empty-segment", Some("empty-segment")),
    ("dot", "dot-segment", None),
    ("dotdot", "dot-segment", None),
    ("inner-dot", "dot-segment", Some("dot-segment")),
    ("double-slash", "empty-segment", Some("empty-segment")),
    ("trailing-slash", "empty-segment", None),
    ("empty", "empty", None),
    ("nul", "character", Some("character")),
    ("bell", "character", Some("character")),
    ("bidi", "character", Some("character")),
    ("backslash", "character", Some("character")),
    ("marker", "reserved", Some("reserved")),
    ("staging", "reserved", Some("reserved")),
    // `reserved` in any letter case.
    ("marker-upper", "reserved", Some("reserved")),
    ("staging-mixed", "reserved", Some("reserved")),
    ("inner-mixed", "reserved", Some("reserved")),
    ("long", "too-long", Some("too-long")),
    ("long-segment", "segment-too-long", Some("segment-too-long")),
    ("deep", "too-deep", Some("too-deep")),
    ("not-nfc", "not-nfc", Some("not-nfc")),
];

#[test]
fn every_hostile_key_fails_invalid_key_through_each_operation_and_nothing_is_made_or_read() {
    let s = Storage::ready();
    s.install(
        "hostile",
        "storage-hostile.js",
        &[(MARKETING, "storage", "write")],
    );
    let secret_beside = std::env::temp_dir().join("secret");
    let existed = secret_beside.exists();
    let before = tree(&s.place.base);
    let (response, body) = s.post(
        &wanda(),
        "hostile",
        "/keys",
        &json!({ "resource": MARKETING }),
    );
    let answers = &body["answers"];
    assert_eq!(
        answers.as_object().map(serde_json::Map::len),
        Some(RULES.len()),
        "expected an answer for each of the fixture's keys, got {body}"
    );
    let mut failed = 0;
    for (label, rule, prefix_rule) in RULES {
        let these = &answers[label];
        for operation in ["head", "get", "put", "delete"] {
            assert_invalid_key(
                &these[operation],
                rule,
                &format!("{operation} of the key {label}"),
            );
            failed += 1;
        }
        match prefix_rule {
            Some(rule) => {
                assert_invalid_key(
                    &these["list"],
                    rule,
                    &format!("list with the prefix {label}"),
                );
                failed += 1;
            }
            None => {
                let (files, folders) = listed_names(&these["list"]["ok"]);
                assert!(
                    these["list"]["ok"].is_object() && files.is_empty() && folders.is_empty(),
                    "list with the prefix {label}: expected an empty page (nothing was made), got {}",
                    these["list"]
                );
            }
        }
    }
    let entries = s.entries_eventually(&response, RULES.len() * 5);
    assert_eq!(
        entries.len(),
        RULES.len() * 5,
        "expected one entry for each operation, got\n{}",
        lines(&entries)
    );
    let failed_entries = entries
        .iter()
        .filter(|e| e["outcome"] == "failed" && e["error"] == "invalid-key")
        .count();
    assert_eq!(
        failed_entries,
        failed,
        "expected a `failed` entry with `invalid-key` for each refused key, got\n{}",
        lines(&entries)
    );
    assert_eq!(
        tree(&s.place.base),
        before,
        "expected nothing made anywhere, inside the directory or outside it"
    );
    assert_eq!(
        secret_beside.exists(),
        existed,
        "expected nothing made beside the test's folder"
    );
    assert!(!std::path::Path::new("/etc/x").exists());
    assert!(
        !body.to_string().contains("root:x:0:0"),
        "expected nothing of /etc/passwd in any answer"
    );
}

/// A program with `marketing` and `other` (each on its own directory,
/// `other` holding `theirs.txt`), `real.txt` put in `marketing`, a file
/// outside the storage root holding known bytes, and in `marketing`'s
/// directory: `etc` -> /etc, `secret` -> that file, `other` -> the other
/// Resource's directory, `inner` -> `<dir>/real.txt`, `deep/link` -> `..`.
struct Planted {
    s: Storage,
    known: Vec<u8>,
    outside_file: std::path::PathBuf,
}

fn planted() -> Planted {
    let s = Storage::ready();
    s.register("other");
    let other = s.dir("other");
    place(&other.join("theirs.txt"), b"their bytes, never to be read");
    s.put_ok("real.txt", b"real", None);
    let known = format!("known-{}", blackbox::shared_database::token()).into_bytes();
    let outside_file = s.place.outside().join("secret.txt");
    std::fs::write(&outside_file, &known).unwrap();
    let dir = s.dir(MARKETING);
    symlink("/etc", dir.join("etc")).unwrap();
    symlink(&outside_file, dir.join("secret")).unwrap();
    symlink(other, dir.join("other")).unwrap();
    symlink(dir.join("real.txt"), dir.join("inner")).unwrap();
    std::fs::create_dir(dir.join("deep")).unwrap();
    symlink("..", dir.join("deep/link")).unwrap();
    Planted {
        s,
        known,
        outside_file,
    }
}

#[test]
fn links_planted_on_the_host_are_never_followed() {
    let Planted {
        s,
        known,
        outside_file,
    } = planted();
    let dir = s.dir(MARKETING);
    let other = s.dir("other");
    let mut answers: Vec<Value> = Vec::new();
    for key in [
        "etc/passwd",
        "secret",
        "other/theirs.txt",
        "inner",
        "deep/link/real.txt",
    ] {
        for call in [get(MARKETING, key), head(MARKETING, key)] {
            let answer = s.as_writer(&call);
            assert_not_reachable(&answer, "link", &format!("{call}"));
            answers.push(answer);
        }
    }
    for key in ["etc/x", "secret", "other/x", "deep/link/x"] {
        let answer = s.as_writer(&put(MARKETING, key, b"planted through a link", None));
        assert_not_reachable(&answer, "link", &format!("put of {key}"));
        answers.push(answer);
    }
    assert!(
        !std::path::Path::new("/etc/x").exists(),
        "expected /etc/x not made"
    );
    assert!(!other.join("x").exists(), "expected <other>/x not made");
    assert!(
        !dir.join("x").exists(),
        "expected <dir>/x not made through deep/link"
    );
    assert_eq!(
        std::fs::read(&outside_file).unwrap(),
        known,
        "the outside file unchanged"
    );
    assert_eq!(
        std::fs::read_link(dir.join("secret")).unwrap(),
        outside_file,
        "expected the link `secret` still a link to the outside file"
    );
    for key in ["secret", "inner", "etc/passwd"] {
        let answer = s.as_writer(&delete(MARKETING, key));
        assert_not_reachable(&answer, "link", &format!("delete of {key}"));
        answers.push(answer);
    }
    assert!(
        std::fs::symlink_metadata(dir.join("secret"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "expected the link `secret` still there"
    );
    assert!(std::fs::symlink_metadata(dir.join("inner")).is_ok());
    assert_eq!(std::fs::read(dir.join("real.txt")).unwrap(), b"real");
    for args in [
        json!({}),
        json!({ "delimiter": "/" }),
        json!({ "prefix": "deep/" }),
    ] {
        let listed = s.as_writer(&list(MARKETING, &args));
        let (files, folders) = listed_names(&listed["ok"]);
        let expected: Vec<String> = if args.get("prefix").is_some() {
            vec![]
        } else {
            vec!["real.txt".to_owned()]
        };
        assert_eq!(
            files, expected,
            "expected no link listed ({args}): {listed}"
        );
        assert!(
            folders.is_empty(),
            "expected no folder for a link, and `deep/` only if it held a file that is reachable ({args}): {listed}"
        );
        answers.push(listed);
    }
    let all = serde_json::to_string(&answers).unwrap();
    let known_text = String::from_utf8(known.clone()).unwrap();
    for (what, needle) in [
        ("the outside file's bytes", known_text.as_str()),
        (
            "the outside file's bytes in base64",
            &blackbox::storage::b64(&known),
        ),
        ("the other Resource's bytes", "their bytes"),
        ("/etc/passwd", "root:x:0:0"),
    ] {
        assert!(!all.contains(needle), "expected {what} in no answer: {all}");
    }
}

#[test]
fn a_fifo_is_not_a_file_at_once_and_a_planted_folder_is_not_a_file_and_conflicts() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    mkfifo(&dir.join("pipe"));
    std::fs::create_dir(dir.join("f")).unwrap();
    for call in [get(MARKETING, "pipe"), head(MARKETING, "pipe")] {
        let began = Instant::now();
        let answer = s.as_writer(&call);
        assert_not_reachable(&answer, "not-a-file", &format!("{call}"));
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "expected the FIFO answered at once, without waiting on it; took {:?}",
            began.elapsed()
        );
    }
    assert_not_reachable(
        &s.as_writer(&get(MARKETING, "f")),
        "not-a-file",
        "get of a folder",
    );
    let answer = s.as_writer(&put(MARKETING, "f", b"x", None));
    assert_eq!(
        answer["error"]["kind"], "key-conflict",
        "expected a put onto a planted folder to be `key-conflict`, got {answer}"
    );
    assert!(std::fs::symlink_metadata(dir.join("f")).unwrap().is_dir());
    let listed = s.as_writer(&list(MARKETING, &json!({})));
    assert!(
        listed_names(&listed["ok"]).0.is_empty(),
        "expected neither the FIFO nor the folder listed as a file: {listed}"
    );
}

#[test]
fn two_resources_never_see_each_others_files() {
    let s = Storage::ready();
    let root = s.root();
    let a = root.join("team").join("a");
    let b = root.join("team").join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    s.register_on("alpha", &a);
    s.register_on("beta", &b);
    for name in ["alpha", "beta"] {
        s.grant(name, FILE_WRITERS, "write");
    }
    s.install(
        "both",
        "operations.js",
        &[("alpha", "storage", "write"), ("beta", "storage", "write")],
    );
    let call = |c: &Value| s.one(&wanda(), "both", c).1;
    assert!(call(&put("alpha", "x.txt", b"alpha's", None))["ok"].is_object());
    assert!(call(&put("alpha", "dir/y.txt", b"alpha's", None))["ok"].is_object());
    let listed = call(&list("beta", &json!({})));
    assert_eq!(
        listed,
        json!({ "ok": { "files": [], "folders": [], "cursor": null } }),
        "expected beta to list nothing of alpha's"
    );
    for key in ["x.txt", "dir/y.txt"] {
        assert_ok_null(&call(&get("beta", key)), &format!("beta's get of {key}"));
        assert_ok_null(&call(&head("beta", key)), &format!("beta's head of {key}"));
        assert_eq!(call(&delete("beta", key)), json!({ "ok": {} }));
    }
    for key in ["../a/x.txt", "../../team/a/x.txt"] {
        assert_invalid_key(&call(&get("beta", key)), "dot-segment", key);
        assert_invalid_key(&call(&delete("beta", key)), "dot-segment", key);
    }
    assert!(call(&put("beta", "x.txt", b"beta's", None))["ok"].is_object());
    assert_eq!(std::fs::read(a.join("x.txt")).unwrap(), b"alpha's");
    assert_eq!(std::fs::read(a.join("dir/y.txt")).unwrap(), b"alpha's");
    assert_eq!(std::fs::read(b.join("x.txt")).unwrap(), b"beta's");
    for (directory, what) in [
        (a.join("dir"), "inside alpha's"),
        (root.join("team"), "holding both"),
    ] {
        let answer = s.try_register("gamma", &directory.to_string_lossy());
        assert!(answer.status == 409, "{what}: {}", answer.describe());
        assert_api_error(&answer, 409, "directory-in-use");
    }
}
