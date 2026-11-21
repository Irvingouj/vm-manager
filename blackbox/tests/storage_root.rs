//! The storage root (CONTRACT.md, "Storage: The storage root"; ticket
//! "Storage: a shared, named file Resource, on a mounted directory now and
//! S3 later": a directory that cannot be used is refused, saying why).
//!
//! - `--storage-root` that is not an absolute path is a start-up failure:
//!   non-zero within 15 seconds, a message naming `--storage-root`;
//! - without it nothing else changes, and every Storage registration is
//!   refused `storage-not-configured`, the message saying to start `vibe
//!   serve` with `--storage-root`;
//! - with it, when it is not there, is not a directory, or the program
//!   cannot create a file in it, `vibe serve` starts all the same and
//!   writes before `listening` one record `storage-root-unusable` with the
//!   problem; registrations are refused with the matching reason.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Instant;

use blackbox::assert_api_error;
use blackbox::postgres::Platform;
use blackbox::program::{EXIT_TIMEOUT, run_vm_to_exit};
use blackbox::records::event;
use blackbox::storage::{MARKETING, Place, Storage, postgres_body, set_mode};
use blackbox::{DOMAIN, VmOptions};
use serde_json::Value;

#[test]
fn a_storage_root_that_is_not_an_absolute_path_is_a_start_up_failure_naming_the_option() {
    let platform = Platform::fresh();
    for root in ["relative/storage", "./storage", "storage", ""] {
        let began = Instant::now();
        let finished = run_vm_to_exit(
            [
                "serve",
                "--domain",
                DOMAIN,
                "--listen",
                "127.0.0.1:0",
                "--stand-in-sign-in",
                "--storage-root",
                root,
            ],
            Some(platform.url()),
        );
        let took = began.elapsed();
        assert!(
            finished.status.is_some_and(|s| !s.success()),
            "expected `vibe serve --storage-root {root:?}` to exit non-zero, got {} after {took:?}\nstandard error:\n{}",
            finished.status_text(),
            finished.stderr
        );
        assert!(
            took < EXIT_TIMEOUT,
            "expected it to exit within {EXIT_TIMEOUT:?}, took {took:?}"
        );
        assert!(
            finished.stderr.contains("--storage-root"),
            "expected the message to name `--storage-root` for {root:?}, got\n{}",
            finished.stderr
        );
    }
    // The same option with an absolute path starts: what fails above is
    // the path, not the option.
    drop(Storage::start());
}

#[test]
fn without_a_storage_root_every_storage_registration_is_refused_and_a_database_still_registers() {
    let place = Place::new("no-root");
    let s = Storage::start_on(place, None, &VmOptions::new());
    let dir = s.root().join(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    for directory in [
        dir.to_string_lossy().into_owned(),
        "relative/marketing".to_owned(),
        "/".to_owned(),
    ] {
        let answer = s.try_register(MARKETING, &directory);
        assert_api_error(&answer, 422, "storage-not-configured");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains("--storage-root"),
            "expected the message to say to start `vibe serve` with `--storage-root`, got {message:?}"
        );
    }
    s.admin().get_resource(MARKETING).expect_status(404);
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "expected nothing written in the directory of a refused registration"
    );
    // Nothing else changes: a database still registers.
    s.admin()
        .create_resource(&postgres_body("devops"))
        .expect_status(201);
    assert!(
        !s.vm
            .records()
            .iter()
            .any(|r| event(r) == "storage-root-unusable"),
        "expected no `storage-root-unusable` record without `--storage-root`"
    );
}

/// Starts the program on `root`, which it must start on, and returns the
/// `storage-root-unusable` records written before `listening`.
fn unusable_records(s: &Storage) -> Vec<Value> {
    let records = s.vm.records();
    let listening = records
        .iter()
        .position(|r| event(r) == "listening")
        .unwrap_or(records.len());
    records[..listening]
        .iter()
        .filter(|r| event(r) == "storage-root-unusable")
        .cloned()
        .collect()
}

fn assert_one_unusable(s: &Storage, problem: &str) {
    let found = unusable_records(s);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one `storage-root-unusable` record before `listening`, got {found:?}"
    );
    assert_eq!(
        found[0]["problem"], problem,
        "expected the problem {problem:?}, got {}",
        found[0]
    );
    let after: Vec<Value> =
        s.vm.records_after_listening()
            .into_iter()
            .filter(|r| event(r) == "storage-root-unusable")
            .collect();
    assert!(
        after.is_empty(),
        "expected the record before `listening` only, got after it: {after:?}"
    );
}

#[test]
fn a_storage_root_that_is_not_there_starts_all_the_same_and_says_missing() {
    let place = Place::new("root-missing");
    let root = place.base.join("not-there");
    let s = Storage::start_on(place, Some(&root), &VmOptions::new());
    assert_one_unusable(&s, "missing");
    let answer = s.try_register(MARKETING, &root.join(MARKETING).to_string_lossy());
    assert_api_error(&answer, 422, "directory-missing");
    s.admin().get_resource(MARKETING).expect_status(404);
    assert!(
        !root.exists(),
        "expected the program not to make the storage root"
    );
}

#[test]
fn a_storage_root_that_is_a_file_starts_all_the_same_and_says_not_a_directory() {
    let place = Place::new("root-file");
    let root = place.base.join("a-file");
    std::fs::write(&root, b"not a directory").unwrap();
    let s = Storage::start_on(place, Some(&root), &VmOptions::new());
    assert_one_unusable(&s, "not-a-directory");
    let answer = s.try_register(MARKETING, &root.join(MARKETING).to_string_lossy());
    assert_api_error(&answer, 422, "not-a-directory");
    assert_eq!(
        std::fs::read(&root).unwrap(),
        b"not a directory",
        "expected the file left as it was"
    );
}

#[test]
fn a_storage_root_the_program_cannot_write_in_is_no_problem_when_the_resources_directory_can_be() {
    let place = Place::new("root-readonly");
    let root = place.root();
    std::fs::create_dir(root.join(MARKETING)).unwrap();
    set_mode(&root, 0o555);
    let s = Storage::start_on(place, Some(&root), &VmOptions::new());
    assert!(
        unusable_records(&s).is_empty()
            && !s
                .vm
                .records()
                .iter()
                .any(|r| event(r) == "storage-root-unusable"),
        "expected no `storage-root-unusable` record for a root the program cannot write in"
    );
    s.register_on(MARKETING, &root.join(MARKETING));
    let mut in_root: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    in_root.sort();
    assert_eq!(
        in_root,
        [MARKETING],
        "expected nothing written in the root itself"
    );
    set_mode(&root, 0o755);
}

#[test]
fn a_usable_storage_root_writes_no_unusable_record() {
    let s = Storage::start();
    assert!(
        unusable_records(&s).is_empty()
            && !s
                .vm
                .records()
                .iter()
                .any(|r| event(r) == "storage-root-unusable"),
        "expected no `storage-root-unusable` record on a usable storage root"
    );
}
