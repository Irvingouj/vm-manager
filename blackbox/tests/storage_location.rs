//! Changing a Storage Resource's location (CONTRACT.md, "Resources and
//! Grants in full: A Storage Resource's location"): `PUT
//! /v1/resources/<name>/location` answers `200` with `{ resource, location,
//! changed, files_moved }` and a message; from the `200` on every operation
//! uses the new directory and the files of the old one stay where they are;
//! the new directory gets `.vibe-resource` and an empty `.vibe-staging/`,
//! the old one loses them (never a file); the same directory again is
//! `"changed": false` and writes nothing; the Resource's own directory,
//! whatever is inside it, is not "another Resource's"; the log record
//! `resource-location-changed` never holds the path, and the path is in no
//! other answer or record. The refusals are in `storage_location_refused`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::{Path, PathBuf};
use std::sync::Barrier;

use blackbox::resources::{admin_resource, location_body, put_location, records_waited};
use blackbox::scenario::administrator;
use blackbox::shared_database::token;
use blackbox::storage::{
    FILES, MARKETING, Storage, VIEWER, assert_nowhere, assert_ok_null, get, mode, place, put, rita,
    set_mode, tree, wanda,
};
use blackbox::{Response, assert_api_error};
use serde_json::{Value, json};

fn change(s: &Storage, name: &str, directory: &Path) -> Response {
    put_location(
        &s.admin(),
        name,
        &location_body(&directory.to_string_lossy()),
    )
}

fn made(s: &Storage, relative: &str) -> PathBuf {
    let dir = s.root().join(relative);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fails unless `dir` holds `.vibe-resource` (a file) and an empty
/// `.vibe-staging/`, and the other entries `others` (as `tree` prints
/// them), nothing else.
fn assert_marked_with(dir: &Path, others: &[&str], context: &str) {
    let found = tree(dir);
    let marker = found.iter().any(|e| e.starts_with(".vibe-resource ("));
    let mut rest: Vec<&str> = found
        .iter()
        .map(String::as_str)
        .filter(|e| !e.starts_with(".vibe-resource (") && *e != ".vibe-staging/")
        .collect();
    rest.sort_unstable();
    let mut want: Vec<&str> = others.to_vec();
    want.sort_unstable();
    assert!(
        marker && found.contains(&".vibe-staging/".to_owned()) && rest == want,
        "{context}: expected `.vibe-resource`, an empty `.vibe-staging/` and {want:?}; it holds {found:?}"
    );
}

/// Fails unless `dir` holds none of the program's own files.
fn assert_unmarked(dir: &Path, context: &str) {
    let found = tree(dir);
    assert!(
        !found
            .iter()
            .any(|e| e.starts_with(".vibe-resource") || e.starts_with(".vibe-staging")),
        "{context}: expected neither `.vibe-resource` nor `.vibe-staging/`; the directory holds {found:?}"
    );
}

/// Fails unless `answer` is `{ ok: { key, size, ... } }` of this file.
fn assert_file(answer: &Value, key: &str, size: u64, context: &str) {
    assert_eq!(answer["ok"]["key"], json!(key), "{context}: {answer}");
    assert_eq!(
        answer["ok"]["size"].as_u64(),
        Some(size),
        "{context}: {answer}"
    );
}

fn location_of(s: &Storage, name: &str) -> Value {
    let view = admin_resource(&s.admin(), name);
    view.expect_status(200);
    view.json()["location"].clone()
}

#[test]
fn the_answer_has_its_shape_and_operations_use_the_new_directory_from_the_200_on() {
    let s = Storage::ready();
    let old = s.dir(MARKETING);
    s.put_ok("a.txt", "in the old directory", None);
    let new = made(&s, "moved");
    place(&new.join("b.txt"), b"already in the new one");

    let answer = change(&s, MARKETING, &new);
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(body["resource"], json!(MARKETING), "{body}");
    assert_eq!(
        body["location"],
        json!({ "backend": "directory", "directory": new.to_string_lossy() }),
        "{body}"
    );
    assert_eq!(body["changed"], json!(true), "{body}");
    assert_eq!(body["files_moved"], json!(false), "{body}");
    let message = body["message"].as_str().unwrap_or_default().to_lowercase();
    assert!(
        message.contains("old") && message.contains("new") && message.contains("files"),
        "expected the message to say that the files of the old directory stay where they are and that the new directory must hold the files the Resource is to have, got {body}"
    );
    assert_eq!(
        location_of(&s, MARKETING),
        body["location"],
        "the administrators' view shows the new location"
    );

    // From the 200 on, every operation uses the new directory.
    let seen = s.as_writer(&get(MARKETING, "b.txt"));
    assert_file(&seen, "b.txt", 22, "a file of the new directory");
    assert_ok_null(
        &s.as_writer(&get(MARKETING, "a.txt")),
        "a file of the old directory, which the Resource no longer has",
    );
    s.put_ok("c.txt", "written after the change", None);
    assert!(new.join("c.txt").is_file(), "{:?}", tree(&new));
    assert!(!old.join("c.txt").exists(), "{:?}", tree(&old));
    // The files of the old directory stay where they are.
    assert_eq!(
        std::fs::read(old.join("a.txt")).unwrap(),
        b"in the old directory"
    );
    // Every Application, and a read-only one.
    let viewer = s.one(&rita(), VIEWER, &get(MARKETING, "b.txt")).1;
    assert_file(&viewer, "b.txt", 22, "the read-only Application");
    // Listing sees the new directory's files.
    let (_, listed) = s.one(
        &wanda(),
        FILES,
        &blackbox::storage::list(MARKETING, &json!({})),
    );
    let (files, _) = blackbox::storage::listed_names(&listed["ok"]);
    assert_eq!(files, vec!["b.txt".to_owned(), "c.txt".to_owned()]);
}

#[test]
fn the_new_directory_gets_the_programs_files_and_the_old_one_loses_them_never_a_file() {
    let s = Storage::ready();
    let old = s.dir(MARKETING);
    s.put_ok("a.txt", "kept", None);
    place(&old.join("folder/inner.txt"), b"inner");
    let new = made(&s, "moved");
    place(&new.join("theirs.txt"), b"theirs");
    assert_marked_with(
        &old,
        &["a.txt (4 bytes)", "folder/", "folder/inner.txt (5 bytes)"],
        "before",
    );

    change(&s, MARKETING, &new).expect_status(200);
    assert_marked_with(&new, &["theirs.txt (6 bytes)"], "the new directory");
    assert_unmarked(&old, "the old directory");
    assert_eq!(
        tree(&old),
        vec![
            "a.txt (4 bytes)".to_owned(),
            "folder/".to_owned(),
            "folder/inner.txt (5 bytes)".to_owned()
        ],
        "expected every file of the old directory still there"
    );
}

#[test]
fn a_staging_folder_that_is_not_empty_stays_in_the_old_directory() {
    let s = Storage::ready();
    let old = s.dir(MARKETING);
    place(&old.join(".vibe-staging/left-over"), b"half a file");
    let new = made(&s, "moved");
    change(&s, MARKETING, &new).expect_status(200);
    assert!(
        !old.join(".vibe-resource").exists(),
        "expected `.vibe-resource` removed from the old directory"
    );
    assert!(
        old.join(".vibe-staging/left-over").is_file(),
        "expected a file never removed, even in `.vibe-staging/`: {:?}",
        tree(&old)
    );
    assert_marked_with(&new, &[], "the new directory");
}

#[test]
fn the_same_directory_again_is_200_changed_false_and_writes_nothing() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("a.txt", "kept", None);
    let before = tree(&dir);
    let marker = std::fs::read(dir.join(".vibe-resource")).unwrap();
    let modified = blackbox::storage::mtime(&dir.join(".vibe-resource"));
    let view = location_of(&s, MARKETING);
    for directory in [
        dir.to_string_lossy().into_owned(),
        format!("{}/", dir.display()),
    ] {
        let answer = put_location(&s.admin(), MARKETING, &location_body(&directory));
        answer.expect_status(200);
        let body = answer.json();
        assert_eq!(body["changed"], json!(false), "{body}");
        assert_eq!(body["resource"], json!(MARKETING), "{body}");
        assert_eq!(body["location"], view, "{body}");
        assert_eq!(
            tree(&dir),
            before,
            "expected nothing written for the same directory"
        );
        assert_eq!(std::fs::read(dir.join(".vibe-resource")).unwrap(), marker);
        assert_eq!(
            blackbox::storage::mtime(&dir.join(".vibe-resource")),
            modified
        );
    }
    // And it still serves.
    s.put_ok("b.txt", "still works", None);
}

#[test]
fn the_resources_own_directory_whatever_is_inside_it_is_not_another_resources() {
    let s = Storage::start();
    let outer = made(&s, "teams/marketing");
    s.register_on(MARKETING, &outer);
    // A directory inside its own, holding a file and a `.vibe-resource`
    // that names this same Resource.
    let inner = outer.join("inner");
    place(&inner.join("x.txt"), b"x");
    std::fs::copy(outer.join(".vibe-resource"), inner.join(".vibe-resource")).unwrap();
    let answer = change(&s, MARKETING, &inner);
    answer.expect_status(200);
    assert_eq!(answer.json()["changed"], json!(true));
    assert_unmarked(&outer, "the old directory, which holds the new one");
    assert_marked_with(
        &inner,
        &["x.txt (1 bytes)"],
        "the directory inside the old one",
    );
    // And back out to a directory that holds the Resource's directory.
    let parent = outer.parent().unwrap().to_path_buf();
    let answer = change(&s, MARKETING, &parent);
    answer.expect_status(200);
    assert_marked_with(
        &parent,
        &[
            "marketing/",
            "marketing/inner/",
            "marketing/inner/x.txt (1 bytes)",
        ],
        "the directory that holds the Resource's old one",
    );
    // A `.vibe-resource` naming this same Resource, in a directory that is
    // not its own, is taken.
    let copied = made(&s, "copied");
    std::fs::copy(parent.join(".vibe-resource"), copied.join(".vibe-resource")).unwrap();
    change(&s, MARKETING, &copied).expect_status(200);
    assert_marked_with(
        &copied,
        &[],
        "the directory whose marker named the Resource",
    );
}

#[test]
fn a_directory_that_was_left_is_free_for_another_resource_and_the_new_one_is_in_use() {
    let s = Storage::start();
    let old = made(&s, "old");
    let new = made(&s, "new");
    s.register_on(MARKETING, &old);
    change(&s, MARKETING, &new).expect_status(200);
    // The old directory can be registered under another name; the new one,
    // as `directory-in-use`.
    let answer = s.try_register("beta", &new.to_string_lossy());
    assert_api_error(&answer, 409, "directory-in-use");
    s.register_on("beta", &old);
    assert_marked_with(&old, &[], "the old directory, registered again");
    // And the same one registered again under the name it had.
    let answer = change(&s, MARKETING, &old);
    assert_api_error(&answer, 409, "directory-in-use");
}

#[test]
fn a_cleaning_that_cannot_be_done_is_left_and_the_old_directory_is_in_use_until_by_hand() {
    if blackbox::storage::my_uid() == 0 {
        return;
    }
    let s = Storage::start();
    let old = made(&s, "old");
    let new = made(&s, "new");
    s.register_on(MARKETING, &old);
    // The program may not remove a file from it.
    set_mode(&old, 0o555);
    let answer = change(&s, MARKETING, &new);
    set_mode(&old, 0o755);
    answer.expect_status(200);
    assert_eq!(answer.json()["changed"], json!(true));
    assert!(
        old.join(".vibe-resource").exists(),
        "the marker is left where it could not be removed"
    );
    assert_eq!(mode(&old) & 0o777, 0o755);
    let answer = s.try_register("beta", &old.to_string_lossy());
    assert_api_error(&answer, 409, "directory-in-use");
    std::fs::remove_file(old.join(".vibe-resource")).unwrap();
    s.register_on("beta", &old);
}

#[test]
fn the_log_record_never_holds_the_path_and_the_path_is_in_no_other_answer_or_record() {
    let s = Storage::ready();
    let t = token();
    let new = made(&s, &format!("moved-{t}"));
    let old = s.dir(MARKETING);
    s.grant(MARKETING, "designers", "write");
    let answer = change(&s, MARKETING, &new);
    answer.expect_status(200);
    let records = records_waited(&s.vm, "resource-location-changed", 1);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["resource"], json!(MARKETING));
    assert_eq!(records[0]["by"], json!(administrator().id));
    assert!(
        !records[0].to_string().contains(&t) && !records[0].to_string().contains("marketing/"),
        "expected the record to hold no path: {}",
        records[0]
    );
    let mut texts: Vec<String> = Vec::new();
    for path in [
        "/v1/resources".to_owned(),
        format!("/v1/resources/{MARKETING}"),
        format!("/v1/resources/{MARKETING}/grants"),
        "/v1/operation-log".to_owned(),
    ] {
        texts.push(s.admin().get(&path).text());
    }
    // An operation, answered and logged.
    let (response, answer) = s.one(&wanda(), FILES, &put(MARKETING, "k.txt", "v", None));
    texts.push(answer.to_string());
    texts.push(response.text());
    // Only `location` of the administrators' own views holds the path.
    assert_nowhere(&s.vm, &texts, &t, "the directory's path", "after a change");
    let _ = old;
}

#[test]
fn a_change_asked_twice_at_once_for_one_directory_is_decided_one_after_the_other() {
    let s = Storage::start();
    for round in 0..4 {
        let a = made(&s, &format!("a{round}"));
        let b = made(&s, &format!("b{round}"));
        let target = made(&s, &format!("target{round}"));
        let (first, second) = (format!("first{round}"), format!("second{round}"));
        s.register_on(&first, &a);
        s.register_on(&second, &b);
        let barrier = Barrier::new(2);
        let statuses: Vec<u16> = std::thread::scope(|scope| {
            let asked: Vec<_> = [first.as_str(), second.as_str()]
                .into_iter()
                .map(|name| {
                    let (barrier, target, s) = (&barrier, &target, &s);
                    scope.spawn(move || {
                        barrier.wait();
                        change(s, name, target).status
                    })
                })
                .collect();
            asked.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut sorted = statuses.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![200, 409],
            "round {round}: expected exactly one of two changes to one directory to be made, the other `directory-in-use`, got {statuses:?}"
        );
    }
}
