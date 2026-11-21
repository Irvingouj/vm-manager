//! When a Storage Resource's directory cannot be used (CONTRACT.md,
//! "Storage: When the directory cannot be used"): the program loads the
//! Resource at start; when its directory is not there, is not a directory,
//! has no `.vibe-resource` (a mount forgotten) or one naming another
//! Resource (another disk), the program's log says
//! `storage-unavailable` with the problem at most once a minute, every
//! allowed operation fails `unavailable` with its entry `failed`
//! `unavailable`, nothing is written anywhere; once the directory can be
//! used again, operations work within about 10 seconds, with no restart.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;
use std::time::Duration;

use blackbox::Vm;
use blackbox::records::event;
use blackbox::scenario::Expected;
use blackbox::storage::{
    FILES, MARKETING, Storage, assert_ok_metadata, assert_storage_entry, delete, eventually, get,
    head, list, put, storage_expected, terminate, tree, wanda,
};
use serde_json::{Value, json};

/// The `storage-unavailable` records of the program's log for `resource`.
fn unavailable_records(vm: &Vm, resource: &str) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| event(r) == "storage-unavailable" && r["resource"] == resource)
        .collect()
}

/// Puts a file while all is well, stops the program, does `break_it` to
/// the directory, starts the program again, and checks that every
/// operation fails `unavailable`, that the log says `problem`, and that
/// nothing was written; then `mend` and operations work again with no
/// restart.
fn unavailable_then_mended(problem: &str, break_it: &dyn Fn(&Path), mend: &dyn Fn(&Path)) {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("kept.txt", b"kept", None);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    break_it(&dir);
    let before = tree(&place.base);
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    let calls = [
        (
            "storage.head",
            head(MARKETING, "kept.txt"),
            "head \"kept.txt\"",
        ),
        (
            "storage.get",
            get(MARKETING, "kept.txt"),
            "get \"kept.txt\"",
        ),
        (
            "storage.put",
            put(MARKETING, "new/file.txt", b"new", None),
            "put \"new/file.txt\"",
        ),
        (
            "storage.delete",
            delete(MARKETING, "kept.txt"),
            "delete \"kept.txt\"",
        ),
        ("storage.list", list(MARKETING, &json!({})), "list \"\""),
    ];
    for (operation, call, statement) in &calls {
        let (response, answer) = s.one(&wanda(), FILES, call);
        assert_eq!(
            answer["error"]["kind"], "unavailable",
            "{problem}: expected {operation} to fail {{ error: {{ kind: \"unavailable\" }} }}, got {answer}"
        );
        let entry = s.entry_of(&response);
        let id = response.request_id();
        assert_storage_entry(
            &entry,
            &storage_expected(
                Expected {
                    resource: Some(MARKETING),
                    ran: None,
                    ..Expected::failed(&id, "wanda", FILES, "unavailable")
                },
                operation,
                statement,
            ),
            &format!("{problem}: the entry of {operation}"),
        );
    }
    assert_eq!(
        tree(&s.place.base),
        before,
        "{problem}: expected nothing written anywhere while the directory cannot be used"
    );
    let records = unavailable_records(&s.vm, MARKETING);
    assert!(
        !records.is_empty(),
        "{problem}: expected a `storage-unavailable` record for {MARKETING}"
    );
    assert_eq!(
        records.len(),
        1,
        "{problem}: expected at most one `storage-unavailable` record a minute, got {records:?}"
    );
    assert_eq!(
        records[0]["problem"], problem,
        "expected the problem {problem:?}, got {}",
        records[0]
    );
    let dir_text = dir.to_string_lossy().into_owned();
    assert!(
        !records[0].to_string().contains(&dir_text),
        "expected the record not to hold the directory's path: {}",
        records[0]
    );

    mend(&dir);
    let worked = eventually(Duration::from_secs(25), || {
        s.as_writer(&head(MARKETING, "kept.txt"))["ok"].is_object()
    });
    assert!(
        worked,
        "{problem}: expected operations to work again within about 10 seconds of the directory being usable, with no restart"
    );
    assert_ok_metadata(
        &s.as_writer(&put(MARKETING, "after.txt", b"after", None)),
        "after.txt",
        5,
        "text/plain; charset=utf-8",
        "a put once the directory is usable again",
    );
    assert_eq!(std::fs::read(dir.join("after.txt")).unwrap(), b"after");
}

fn aside(dir: &Path) -> std::path::PathBuf {
    dir.with_file_name("marketing-aside")
}

#[test]
fn a_directory_that_is_not_there_makes_every_operation_unavailable_until_it_is_back() {
    unavailable_then_mended(
        "missing",
        &|dir| std::fs::rename(dir, aside(dir)).unwrap(),
        &|dir| std::fs::rename(aside(dir), dir).unwrap(),
    );
}

#[test]
fn a_directory_that_is_a_file_makes_every_operation_unavailable_until_it_is_back() {
    unavailable_then_mended(
        "not-a-directory",
        &|dir| {
            std::fs::rename(dir, aside(dir)).unwrap();
            std::fs::write(dir, b"a file where the directory was").unwrap();
        },
        &|dir| {
            std::fs::remove_file(dir).unwrap();
            std::fs::rename(aside(dir), dir).unwrap();
        },
    );
}

#[test]
fn a_directory_without_its_marker_a_mount_forgotten_is_unavailable_until_it_is_back() {
    unavailable_then_mended(
        "not-marked",
        &|dir| {
            std::fs::rename(dir, aside(dir)).unwrap();
            std::fs::create_dir(dir).unwrap();
        },
        &|dir| {
            std::fs::remove_dir(dir).unwrap();
            std::fs::rename(aside(dir), dir).unwrap();
        },
    );
}

#[test]
fn a_directory_marked_for_another_resource_another_disk_is_unavailable_until_it_is_back() {
    // The other Resource's directory is put where `marketing`'s was.
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.register("other");
    let other = s.dir("other");
    s.put_ok("kept.txt", b"kept", None);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    std::fs::rename(&dir, aside(&dir)).unwrap();
    // A copy of the other Resource's directory: its marker names `other`.
    std::fs::create_dir(&dir).unwrap();
    std::fs::copy(other.join(".vibe-resource"), dir.join(".vibe-resource")).unwrap();
    std::fs::create_dir(dir.join(".vibe-staging")).unwrap();
    let before = tree(&place.base);
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    let answer = s.as_writer(&put(MARKETING, "x.txt", b"x", None));
    assert_eq!(
        answer["error"]["kind"], "unavailable",
        "expected a put into a directory marked for another Resource to fail `unavailable`, got {answer}"
    );
    assert_eq!(tree(&s.place.base), before, "expected nothing written");
    let records = unavailable_records(&s.vm, MARKETING);
    assert!(
        records.len() == 1 && records[0]["problem"] == "marked-for-another",
        "expected one `storage-unavailable` record with problem `marked-for-another`, got {records:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::rename(aside(&dir), &dir).unwrap();
    assert!(
        eventually(Duration::from_secs(25), || {
            s.as_writer(&head(MARKETING, "kept.txt"))["ok"].is_object()
        }),
        "expected operations to work again once the right directory is back"
    );
}

#[test]
fn a_refused_operation_on_an_unavailable_resource_keeps_its_refusal() {
    // Who is allowed comes first: a User with no Grant is refused, not told
    // that the directory is unavailable.
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    std::fs::rename(&dir, aside(&dir)).unwrap();
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    let answer = s
        .one(&blackbox::storage::nobody(), FILES, &head(MARKETING, "x"))
        .1;
    assert_eq!(
        answer["refused"]["refusal"]["reason"], "no-grant",
        "expected `no-grant` before the directory is looked at, got {answer}"
    );
}
