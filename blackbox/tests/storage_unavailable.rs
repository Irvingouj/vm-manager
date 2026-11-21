//! When a Storage Resource's directory cannot be used (CONTRACT.md,
//! "Storage: When the directory cannot be used"): the program loads the
//! Resource at start; when its directory is not there, is not a directory,
//! has no `.vibe-resource` (a mount forgotten) or one naming another
//! Resource (another disk), the program's log says
//! `storage-unavailable` with the problem at most once a minute, every
//! allowed operation fails `unavailable` with its entry `failed`
//! `unavailable`, nothing is written anywhere; once the directory can be
//! used again, operations work within about 10 seconds, with no restart.
//! The directory is reached from the storage root the program was started
//! with, following no link (`outside-storage-root`, `link`); one no
//! longer at its place while the program runs is let go and opened again.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::Duration;

use blackbox::Vm;
use blackbox::records::event;
use blackbox::scenario::Expected;
use blackbox::storage::{
    FILE_READERS, FILE_WRITERS, FILES, MARKETING, Storage, assert_got, assert_ok_metadata,
    assert_storage_entry, delete, eventually, get, head, list, put, storage_expected, terminate,
    tree, wanda,
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
    unavailable_then_mended_on(s, &dir, problem, break_it, mend);
}

/// [`unavailable_then_mended`] on a Resource [`MARKETING`] already set up
/// as [`Storage::ready`] sets it up, on `dir`.
fn unavailable_then_mended_on(
    s: Storage,
    dir: &Path,
    problem: &str,
    break_it: &dyn Fn(&Path),
    mend: &dyn Fn(&Path),
) {
    s.put_ok("kept.txt", b"kept", None);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    break_it(dir);
    let before = tree(&place.base);
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    assert_every_operation_unavailable(&s, problem);
    assert_eq!(
        tree(&s.place.base),
        before,
        "{problem}: expected nothing written anywhere while the directory cannot be used"
    );
    assert_one_record(&s, problem, dir);

    mend(dir);
    assert_works_again(&s, dir, problem);
}

/// The calls of the tests: one of each operation, with its statement.
fn calls() -> [(&'static str, Value, &'static str); 5] {
    [
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
    ]
}

/// Fails unless each of [`calls`] fails `unavailable` with its `failed`
/// entry.
fn assert_every_operation_unavailable(s: &Storage, problem: &str) {
    for (operation, call, statement) in &calls() {
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
}

/// Fails unless the log has exactly one `storage-unavailable` record for
/// [`MARKETING`], with `problem`, not holding the directory's path.
fn assert_one_record(s: &Storage, problem: &str, dir: &Path) {
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
}

/// Fails unless, within about 10 seconds and with no restart, operations
/// work again: `kept.txt` is read and a put lands in `dir`.
fn assert_works_again(s: &Storage, dir: &Path, problem: &str) {
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

/// Puts `kept.txt`, stops the program, starts it with `root` as its
/// storage root (`None`: without `--storage-root`), and checks that every
/// operation is `unavailable` with `outside-storage-root` and nothing is
/// written; then stops it and starts it with the right root, and
/// operations work.
fn outside_the_root_then_back(other_root: Option<&str>) {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    let root = s.root();
    s.put_ok("kept.txt", b"kept", None);
    let other = other_root.map(|name| s.place.base.join(name));
    if let Some(other) = &other {
        std::fs::create_dir(other).unwrap();
    }
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    let before = tree(&place.base);
    let s = Storage {
        vm: stopped.start_with_root(other.as_deref()),
        place,
    };
    let problem = "outside-storage-root";
    assert_every_operation_unavailable(&s, problem);
    assert_eq!(
        tree(&s.place.base),
        before,
        "{problem}: expected nothing written anywhere"
    );
    assert_one_record(&s, problem, &dir);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    let s = Storage {
        vm: stopped.start_with_root(Some(&root)),
        place,
    };
    assert_works_again(&s, &dir, "started again with the right storage root");
}

#[test]
fn started_with_another_storage_root_the_resource_is_unavailable_outside_the_storage_root() {
    outside_the_root_then_back(Some("other-root"));
}

#[test]
fn started_without_a_storage_root_the_resource_is_unavailable_outside_the_storage_root() {
    outside_the_root_then_back(None);
}

#[test]
fn a_folder_on_the_way_now_a_symbolic_link_makes_the_resource_unavailable_until_it_is_mended() {
    let s = Storage::start();
    let dir = s.root().join("teams").join(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    let teams = |dir: &Path| dir.parent().unwrap().to_path_buf();
    let teams_aside = |dir: &Path| dir.parent().unwrap().with_file_name("teams-aside");
    unavailable_then_mended_on(
        s,
        &dir,
        "link",
        &|dir| {
            std::fs::rename(teams(dir), teams_aside(dir)).unwrap();
            symlink(teams_aside(dir), teams(dir)).unwrap();
        },
        &|dir| {
            std::fs::remove_file(teams(dir)).unwrap();
            std::fs::rename(teams_aside(dir), teams(dir)).unwrap();
        },
    );
}

#[test]
fn a_directory_now_a_symbolic_link_to_where_it_was_moved_is_unavailable_until_it_is_mended() {
    unavailable_then_mended(
        "link",
        &|dir| {
            std::fs::rename(dir, aside(dir)).unwrap();
            symlink(aside(dir), dir).unwrap();
        },
        &|dir| {
            std::fs::remove_file(dir).unwrap();
            std::fs::rename(aside(dir), dir).unwrap();
        },
    );
}

/// Waits (25 seconds at most) until a head of `kept.txt` fails
/// `unavailable` and the log has a `storage-unavailable` record with
/// `problem`, with the program running all along.
fn becomes_unavailable(s: &Storage, dir: &Path, problem: &str) {
    let failed = eventually(Duration::from_secs(25), || {
        s.as_writer(&head(MARKETING, "kept.txt"))["error"]["kind"] == "unavailable"
    });
    assert!(
        failed,
        "{problem}: expected operations to fail `unavailable` within about 10 seconds of the directory leaving its place, with the program running"
    );
    let logged = eventually(Duration::from_secs(5), || {
        unavailable_records(&s.vm, MARKETING)
            .iter()
            .any(|r| r["problem"] == problem)
    });
    assert!(
        logged,
        "{problem}: expected a `storage-unavailable` record with the problem {problem:?}, got {:?}",
        unavailable_records(&s.vm, MARKETING)
    );
    let dir_text = dir.to_string_lossy().into_owned();
    for record in unavailable_records(&s.vm, MARKETING) {
        assert!(
            !record.to_string().contains(&dir_text),
            "expected the record not to hold the directory's path: {record}"
        );
    }
}

/// Fails unless the log has a `storage-available` record for
/// [`MARKETING`] within 5 seconds.
fn assert_available_record(s: &Storage) {
    let logged = eventually(Duration::from_secs(5), || {
        s.vm.records()
            .iter()
            .any(|r| event(r) == "storage-available" && r["resource"] == MARKETING)
    });
    assert!(
        logged,
        "expected a `storage-available` record for {MARKETING} once the directory is back"
    );
}

#[test]
fn a_directory_moved_away_while_the_program_runs_and_an_empty_one_in_its_place_is_let_go() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("kept.txt", b"kept", None);
    let aside = aside(&dir);
    std::fs::rename(&dir, &aside).unwrap();
    std::fs::create_dir(&dir).unwrap();
    let aside_before = tree(&aside);
    becomes_unavailable(&s, &dir, "not-marked");
    assert_every_operation_unavailable(&s, "not-marked");
    assert!(
        tree(&dir).is_empty(),
        "expected nothing written into the empty directory at the path (no `.vibe-staging`, no file), got {:?}",
        tree(&dir)
    );
    assert_eq!(
        tree(&aside),
        aside_before,
        "expected nothing more written into the directory moved away"
    );
    std::fs::remove_dir(&dir).unwrap();
    std::fs::rename(&aside, &dir).unwrap();
    let back = eventually(Duration::from_secs(25), || {
        s.as_writer(&get(MARKETING, "kept.txt"))["ok"].is_object()
    });
    assert!(
        back,
        "expected the directory in use again within about 10 seconds of it being back, with no restart"
    );
    assert_got(
        &s.as_writer(&get(MARKETING, "kept.txt")),
        "kept.txt",
        b"kept",
        "text/plain; charset=utf-8",
        "the first file, once the directory is back",
    );
    assert_works_again(&s, &dir, "moved back");
    assert_available_record(&s);
}

#[test]
fn a_directory_removed_and_made_again_empty_while_the_program_runs_stays_unavailable() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("kept.txt", b"kept", None);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir(&dir).unwrap();
    becomes_unavailable(&s, &dir, "not-marked");
    assert_every_operation_unavailable(&s, "not-marked");
    // Past the next try, 10 seconds on: still unavailable, still nothing.
    std::thread::sleep(Duration::from_secs(11));
    assert_every_operation_unavailable(&s, "not-marked");
    assert!(
        tree(&dir).is_empty(),
        "expected nothing written into the directory made again, got {:?}",
        tree(&dir)
    );
}

#[test]
fn a_directory_moved_away_while_the_program_runs_is_missing_until_it_is_back() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("kept.txt", b"kept", None);
    let aside = aside(&dir);
    std::fs::rename(&dir, &aside).unwrap();
    let aside_before = tree(&aside);
    becomes_unavailable(&s, &dir, "missing");
    assert_every_operation_unavailable(&s, "missing");
    assert!(
        !dir.exists(),
        "expected nothing made at the directory's path"
    );
    assert_eq!(
        tree(&aside),
        aside_before,
        "expected nothing more written into the directory moved away"
    );
    std::fs::rename(&aside, &dir).unwrap();
    assert_works_again(&s, &dir, "moved back");
    assert_available_record(&s);
}
