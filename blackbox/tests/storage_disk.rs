//! What is on the host's directory (CONTRACT.md, "Storage: What is on the
//! directory"; ticket: the files are in the directory). The test looks at
//! the directory itself: the bytes put, a regular file with one link,
//! owned by the program's user (the test's own here), mode `0640`, folders
//! made `0750`, the extended attribute `user.vibe.content-type`, the time
//! of last change; folders removed when a delete leaves them empty; files
//! placed by hand; `.vibe-resource` and `.vibe-staging/`; names on the disk
//! that are not keys.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::MetadataExt;

use blackbox::storage::{
    CONTENT_TYPE_ATTRIBUTE, MARKETING, Storage, assert_invalid_key, assert_ok_metadata,
    assert_ok_null, assert_read, binary_with, delete, get, head, list, listed_names, mode, mtime,
    my_uid, place, set_xattr, terminate, tree, xattr,
};
use serde_json::json;

#[test]
fn a_put_leaves_the_bytes_in_a_regular_file_of_mode_0640_in_folders_of_mode_0750() {
    let s = Storage::ready_streams();
    let bytes = binary_with("on the disk");
    let written = s.put_hex_ok("reports/2025/q3.bin", &bytes, Some("application/x-report"));
    let dir = s.dir(MARKETING);
    let file = dir.join("reports/2025/q3.bin");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        bytes,
        "expected exactly the bytes put"
    );
    let meta = std::fs::symlink_metadata(&file).unwrap();
    assert!(meta.file_type().is_file(), "expected a regular file");
    assert_eq!(meta.nlink(), 1, "expected a link count of 1");
    assert_eq!(
        meta.uid(),
        my_uid(),
        "expected it owned by the program's user"
    );
    assert_eq!(
        mode(&file),
        0o640,
        "expected mode 0640, got {:o}",
        mode(&file)
    );
    for folder in ["reports", "reports/2025"] {
        let path = dir.join(folder);
        assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
        assert_eq!(
            mode(&path),
            0o750,
            "expected the folder {folder} made with mode 0750, got {:o}",
            mode(&path)
        );
    }
    assert_eq!(
        xattr(&file, CONTENT_TYPE_ATTRIBUTE).as_deref(),
        Some(b"application/x-report".as_slice()),
        "expected `{CONTENT_TYPE_ATTRIBUTE}` to hold the content type given"
    );
    let seconds = blackbox::api::parse_rfc3339(written["last_modified"].as_str().unwrap()).unwrap();
    assert_eq!(
        mtime(&file),
        seconds,
        "expected its time of modification, to the second"
    );

    s.put_ok("plain.txt", "none given", None);
    assert_eq!(
        xattr(&dir.join("plain.txt"), CONTENT_TYPE_ATTRIBUTE),
        None,
        "expected no attribute when no content type was given"
    );
    // Replacing with no content type removes the one given before.
    s.put_ok("reports/2025/q3.bin", "again", None);
    assert_eq!(xattr(&file, CONTENT_TYPE_ATTRIBUTE), None);
    assert_eq!(mode(&file), 0o640);
    assert_eq!(std::fs::symlink_metadata(&file).unwrap().nlink(), 1);
    assert!(
        tree(&dir.join(".vibe-staging")).is_empty(),
        "expected `.vibe-staging/` empty when no put is under way, got {:?}",
        tree(&dir.join(".vibe-staging"))
    );
}

#[test]
fn a_delete_removes_the_file_and_every_folder_it_leaves_empty() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("a/b/c/d.txt", "x", None);
    s.put_ok("a/keep.txt", "x", None);
    s.as_writer(&delete(MARKETING, "a/b/c/d.txt"));
    assert!(!dir.join("a/b/c/d.txt").exists());
    assert!(
        !dir.join("a/b").exists(),
        "expected the empty folders a/b/c and a/b removed"
    );
    assert!(
        dir.join("a/keep.txt").exists(),
        "expected a/ kept: it is not empty"
    );
    s.as_writer(&delete(MARKETING, "a/keep.txt"));
    assert!(!dir.join("a").exists(), "expected a/ removed once empty");
    assert!(
        dir.is_dir(),
        "expected the Resource's directory itself kept"
    );
    let mut left = tree(&dir);
    left.retain(|e| !e.starts_with(".vibe-"));
    assert!(left.is_empty(), "expected nothing left, got {left:?}");
}

#[test]
fn a_file_placed_by_hand_is_a_file_of_the_resource_like_any_other() {
    let s = Storage::ready_streams();
    let dir = s.dir(MARKETING);
    place(&dir.join("hand/report.PDF"), b"by hand");
    place(&dir.join("hand/typed.bin"), b"typed by hand");
    set_xattr(
        &dir.join("hand/typed.bin"),
        CONTENT_TYPE_ATTRIBUTE,
        b"text/x-hand",
    );
    let listed = s.as_writer(&list(MARKETING, &json!({ "prefix": "hand/" })));
    assert_eq!(
        listed_names(&listed["ok"]).0,
        ["hand/report.PDF", "hand/typed.bin"],
        "{listed}"
    );
    assert_read(
        &s.read("hand/report.PDF"),
        "hand/report.PDF",
        b"by hand",
        "application/pdf",
        "a file placed by hand, typed by the table",
    );
    assert_read(
        &s.read("hand/typed.bin"),
        "hand/typed.bin",
        b"typed by hand",
        "text/x-hand",
        "a file placed by hand with the attribute",
    );
    s.put_ok("hand/report.PDF", "replaced", None);
    assert_eq!(
        std::fs::read(dir.join("hand/report.PDF")).unwrap(),
        b"replaced"
    );
    s.as_writer(&delete(MARKETING, "hand/typed.bin"));
    assert!(!dir.join("hand/typed.bin").exists());
}

#[test]
fn names_on_the_disk_that_are_not_keys_are_never_listed_and_cannot_be_reached() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    s.put_ok("ok.txt", "ok", None);
    let odd = [
        "bell\u{7}.txt",
        "back\\slash.txt",
        ".vibe-mine",
        "cafe\u{301}.txt",
        "bidi\u{202e}.txt",
        // `.vibe-` in another letter case.
        ".VIBE-notes.txt",
    ];
    for name in odd {
        place(&dir.join(name), b"not a key");
    }
    place(&dir.join("folder\u{1}/inner.txt"), b"not a key");
    place(&dir.join(".vibe-staging-not/x.txt"), b"not a key");
    place(&dir.join(".Vibe-old/kept.txt"), b"not a key");
    for args in [json!({}), json!({ "delimiter": "/" })] {
        let listed = s.as_writer(&list(MARKETING, &args));
        let (files, folders) = listed_names(&listed["ok"]);
        assert_eq!(
            files,
            ["ok.txt"],
            "expected only the keys that are taken: {listed}"
        );
        assert!(folders.is_empty(), "{listed}");
    }
    for name in odd {
        let answer = s.as_writer(&get(MARKETING, name));
        assert!(
            answer["error"]["kind"] == "invalid-key",
            "expected {name:?} unreachable, refused as a key, got {answer}"
        );
    }
    assert_invalid_key(
        &s.as_writer(&get(MARKETING, ".vibe-resource")),
        "reserved",
        "the marker",
    );
    assert_invalid_key(
        &s.as_writer(&head(MARKETING, ".vibe-staging")),
        "reserved",
        "the staging folder",
    );
    for (call, what) in [
        (
            get(MARKETING, ".VIBE-notes.txt"),
            "a file `.VIBE-notes.txt` placed by hand",
        ),
        (
            get(MARKETING, ".Vibe-old/kept.txt"),
            "a file in a folder `.Vibe-old/` placed by hand",
        ),
        (
            head(MARKETING, ".Vibe-old"),
            "a folder `.Vibe-old/` placed by hand",
        ),
    ] {
        assert_invalid_key(&s.as_writer(&call), "reserved", what);
    }
    let listed = s.as_writer(&list(MARKETING, &json!({ "prefix": ".Vibe-old/" })));
    assert_invalid_key(&listed, "reserved", "a list on the prefix `.Vibe-old/`");
}

#[test]
fn the_directory_holds_its_marker_and_staging_and_what_a_crash_left_there_is_removed_at_the_next_load()
 {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    assert!(
        std::fs::symlink_metadata(dir.join(".vibe-resource"))
            .unwrap()
            .is_file()
    );
    assert!(
        std::fs::symlink_metadata(dir.join(".vibe-staging"))
            .unwrap()
            .is_dir()
    );
    s.put_ok("kept.txt", "kept", None);
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    // What a crash in the middle of a put leaves.
    std::fs::write(dir.join(".vibe-staging/left-by-a-crash"), b"half a file").unwrap();
    let s = Storage {
        vm: stopped.start(),
        place,
    };
    assert!(
        tree(&dir.join(".vibe-staging")).is_empty(),
        "expected what a crash left in `.vibe-staging/` removed when the Resource is loaded, got {:?}",
        tree(&dir.join(".vibe-staging"))
    );
    assert_ok_metadata(
        &s.as_writer(&head(MARKETING, "kept.txt")),
        "kept.txt",
        4,
        "text/plain; charset=utf-8",
        "the file after the restart",
    );
    assert_ok_null(
        &s.as_writer(&get(MARKETING, "left-by-a-crash")),
        "a staging file is not a file of the Resource",
    );
}
