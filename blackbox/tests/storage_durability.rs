//! Durability (CONTRACT.md, "Storage: Durability"; ticket: the files are in
//! the directory, and survive a restart): a put whose answer was received
//! is on the disk; after the program is stopped (with `SIGTERM`, and
//! killed with `SIGKILL`) and started again against the same platform
//! database and storage root, the file has its bytes, its content type and
//! its time of last change, and the Resource answers as before: its
//! Grants, its location and the Applications' Declared access are in the
//! platform database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::Vm;
use blackbox::scenario::assert_refusal;
use blackbox::storage::{
    FILES, FILES_CONTACT, MARKETING, Stopped, Storage, assert_read, head, kill, list, listed_names,
    nobody, put, rita, terminate, wanda,
};
use serde_json::{Value, json};

/// One file a surviving program keeps: its key, the text put (or `None`
/// when its bytes are not UTF-8), the bytes on the disk, the content type
/// given to the put and the one answered.
type File<'a> = (&'a str, Option<&'a str>, &'a [u8], Option<&'a str>, &'a str);

/// Puts three files, stops the program with `stop`, starts it again, and
/// checks that each file and the Resource answer as before.
fn survives(stop: fn(Vm) -> Stopped, what: &str) {
    let s = Storage::ready_streams();
    let files: [File<'_>; 3] = [
        (
            "reports/q3.pdf",
            Some("%PDF the report"),
            b"%PDF the report",
            None,
            "application/pdf",
        ),
        (
            "typed.bin",
            None,
            b"\x00\x01typed",
            Some("application/x-typed"),
            "application/x-typed",
        ),
        (
            "报告/第三季度.txt",
            Some("第三季度"),
            "第三季度".as_bytes(),
            None,
            "text/plain; charset=utf-8",
        ),
    ];
    let mut written: Vec<Value> = Vec::new();
    for (key, text, bytes, content_type, _) in files {
        written.push(match text {
            Some(text) => s.put_ok(key, text, content_type),
            None => s.put_hex_ok(key, bytes, content_type),
        });
    }
    let resource_before = s.admin().get_resource(MARKETING).json();
    let Storage { vm, place } = s;
    let stopped = stop(vm);
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    for ((key, _, bytes, _, content_type), before) in files.iter().zip(&written) {
        let got = s.read(key);
        assert_read(&got, key, bytes, content_type, &format!("{what}: {key}"));
        assert_eq!(
            got["ok"]["last_modified"], before["last_modified"],
            "{what}: expected {key}'s time of last change kept"
        );
        assert_eq!(
            s.as_writer(&head(MARKETING, key))["ok"],
            *before,
            "{what}: expected {key}'s head as it was answered by the put"
        );
    }
    assert_eq!(
        s.admin().get_resource(MARKETING).json(),
        resource_before,
        "{what}: expected the Resource as it was"
    );
    // The Grants, the Declared access and the location hold as before.
    assert_eq!(
        listed_names(&s.as_writer(&list(MARKETING, &json!({})))["ok"])
            .0
            .len(),
        3,
        "{what}: the list after the restart"
    );
    assert!(s.one(&rita(), FILES, &head(MARKETING, "typed.bin")).1["ok"].is_object());
    assert_refusal(
        &s.one(&rita(), FILES, &put(MARKETING, "x", "x", None)).1,
        "no-grant",
        &json!({ "resource": MARKETING, "user": "rita", "needed": "write", "held": "read", "contact": FILES_CONTACT }),
        what,
    );
    assert_refusal(
        &s.one(&nobody(), FILES, &head(MARKETING, "x")).1,
        "no-grant",
        &json!({ "resource": MARKETING, "user": "nobody", "needed": "read", "held": null, "contact": FILES_CONTACT }),
        what,
    );
    assert!(
        s.one(&wanda(), FILES, &put(MARKETING, "after.txt", "after", None))
            .1["ok"]
            .is_object(),
        "{what}: a put after the restart"
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("after.txt")).unwrap(),
        b"after"
    );
}

#[test]
fn a_file_put_survives_the_program_stopped_and_started_again() {
    survives(terminate, "stopped with SIGTERM");
}

#[test]
fn a_file_put_survives_the_program_killed_and_started_again() {
    survives(kill, "killed with SIGKILL");
}

#[test]
fn a_file_whose_answer_was_received_is_on_the_disk_at_once_before_any_stop() {
    let s = Storage::ready();
    s.put_ok("now.txt", "at once", None);
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("now.txt")).unwrap(),
        b"at once"
    );
    let Storage { vm, place } = s;
    let stopped = kill(vm);
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("now.txt")).unwrap(),
        b"at once"
    );
    assert!(
        s.dir(MARKETING).join(".vibe-staging").is_dir(),
        "expected `.vibe-staging/` there after the restart"
    );
}
