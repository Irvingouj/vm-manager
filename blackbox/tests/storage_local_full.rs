//! The local state seen by a tool that looks at no more than it must
//! (CONTRACT.md, "Storage: The local loop": the bullets "A local Storage
//! for each pulled Storage Resource" (a Resource pulled while `vibe dev`
//! runs) and "No link is followed in the local state" (more than 1,000
//! entries; a refused reset leaves `vibe dev` as it was; `postmaster.pid`
//! read only when it is a regular file, at most its first 4 KiB)).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::path::Path;
use std::time::Duration;

use blackbox::dev::Dev;
use blackbox::local::{BUILDER, Board, Local, OUTSIDER};
use blackbox::project::Tree;
use blackbox::storage::{MARKETING, mkfifo};
use blackbox::storage_tool::{Files, assert_storages};
use blackbox::tool::Run;
use serde_json::json;

/// More than the 1,000 entries a tool may look at.
const TOO_MANY: usize = 1_200;

fn reset_run(local: &Local, dir: &Path) -> Run {
    local.watch(dir);
    local.run(
        &[
            "local",
            "reset",
            "--project",
            dir.to_str().unwrap(),
            "--json",
        ],
        &[],
    )
}

/// CONTRACT.md: the message "names the folder and says that it holds more
/// than the tool looks through, and does not speak of a link when it met
/// none".
fn assert_names_the_full_folder(message: &str, folder: &str, context: &str) {
    assert!(
        message.contains(folder),
        "{context}, no link: expected the message to name the folder {folder:?}, got {message:?}"
    );
    assert!(
        !message.to_lowercase().contains("link"),
        "{context}, no link: expected the message not to speak of a link when it met none, got {message:?}"
    );
}

fn fill(folder: &Path, count: usize) {
    for i in 0..count {
        // Every other name is a hidden one: "every name directly in the
        // folder counts, hidden ones included".
        let dot = if i % 2 == 0 { "." } else { "" };
        fs::write(folder.join(format!("{dot}filler-{i:05}")), "x").unwrap();
    }
}

fn json_keys(body: &serde_json::Value) -> Vec<String> {
    let text = body["body"].as_str().unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    parsed["keys"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|k| k.as_str().map(str::to_owned))
        .collect()
}

/// A folder outside the project with a sentinel file (and a `PG_VERSION`
/// when `looks_like_data`, so that it looks like a Postgres data folder).
fn sentinel_folder(root: &Path, name: &str, looks_like_data: bool) -> std::path::PathBuf {
    let folder = root.join(name);
    fs::create_dir_all(folder.join("sentinel-folder")).unwrap();
    fs::write(folder.join("sentinel.txt"), "a sentinel, kept\n").unwrap();
    fs::write(folder.join("sentinel-folder/inner.txt"), "inner, kept\n").unwrap();
    if looks_like_data {
        fs::write(folder.join("PG_VERSION"), "18\n").unwrap();
    }
    folder
}

#[test]
fn a_local_state_of_more_than_1000_entries_is_refused_with_or_without_a_link() {
    let board = Board::start("read", "read");
    let warm = board.request_ok("/", OUTSIDER, &[]);
    assert_eq!(warm.status(), 200, "{}", warm.describe());
    let local = board.dir().join(".vibe/local");
    fill(&local, TOO_MANY);
    // No link at all: more than 1,000 entries is refused all the same.
    for (command, run) in [
        (
            "vibe request",
            board.local.request(board.dir(), "/", OUTSIDER, &[]),
        ),
        ("vibe local reset", reset_run(&board.local, board.dir())),
    ] {
        let message = run.json_error(1, "local-state");
        assert_names_the_full_folder(&message, ".vibe/local", command);
        assert!(
            fs::read_dir(&local).unwrap().count() >= TOO_MANY,
            "{command}: expected nothing removed from a state that was too full"
        );
    }
    // A link to a folder outside the project, with or without the marks of
    // a data folder.
    let stash = board.local.machine.root().join("stash");
    fs::create_dir_all(&stash).unwrap();
    for looks_like_data in [false, true] {
        let target = sentinel_folder(
            board.local.machine.root(),
            if looks_like_data { "as-data" } else { "plain" },
            looks_like_data,
        );
        let link = local.join("postgres");
        let aside = stash.join(format!("postgres-{looks_like_data}"));
        fs::rename(&link, &aside).unwrap();
        symlink(&target, &link).unwrap();
        let before = Tree::of(&target);
        for (command, run) in [
            (
                "vibe request",
                board.local.request(board.dir(), "/", OUTSIDER, &[]),
            ),
            ("vibe local reset", reset_run(&board.local, board.dir())),
        ] {
            run.json_error(1, "local-state");
            assert_eq!(
                before.differences(&Tree::of(&target)),
                Vec::<String>::new(),
                "{command}, link to a folder (data folder marks: {looks_like_data}), {TOO_MANY} entries: expected the target as it was, nothing created in it"
            );
        }
        fs::remove_file(&link).unwrap();
        fs::rename(&aside, &link).unwrap();
    }
}

#[test]
fn exactly_1000_entries_in_the_local_state_and_no_link_run_as_usual() {
    let board = Board::start("read", "read");
    let warm = board.request_ok("/", OUTSIDER, &[]);
    assert_eq!(warm.status(), 200, "{}", warm.describe());
    let local = board.dir().join(".vibe/local");
    let there = fs::read_dir(&local).unwrap().count();
    fill(&local, 1_000 - there);
    assert_eq!(
        fs::read_dir(&local).unwrap().count(),
        1_000,
        "the harness's own check"
    );
    let run = board.local.request(board.dir(), "/", OUTSIDER, &[]);
    run.expect_code(0);
}

#[test]
fn a_local_storage_folder_of_more_than_1000_entries_is_refused_with_or_without_a_link() {
    let files = Files::start("full-storage-board", true);
    files.request("/put?key=kept.txt", BUILDER, &[]);
    let storage = files.dir().join(".vibe/local/storage");
    fill(&storage, TOO_MANY);
    for (command, run) in [
        (
            "vibe request",
            files.local.request(files.dir(), "/", BUILDER, &[]),
        ),
        ("vibe local reset", reset_run(&files.local, files.dir())),
    ] {
        let message = run.json_error(1, "local-state");
        assert_names_the_full_folder(&message, ".vibe/local/storage", command);
        assert_eq!(
            fs::read(files.folder().join("kept.txt")).unwrap(),
            b"stored by the local run".as_slice(),
            "{command}, no link: expected nothing emptied"
        );
    }
    let target = sentinel_folder(files.local.machine.root(), "storage-target", false);
    symlink(&target, storage.join("a-link")).unwrap();
    let before = Tree::of(&target);
    for (command, run) in [
        (
            "vibe request",
            files.local.request(files.dir(), "/", BUILDER, &[]),
        ),
        ("vibe local reset", reset_run(&files.local, files.dir())),
    ] {
        run.json_error(1, "local-state");
        assert_eq!(
            before.differences(&Tree::of(&target)),
            Vec::<String>::new(),
            "{command}, a link among {TOO_MANY} entries: expected the target as it was"
        );
    }
}

#[test]
fn a_reset_refused_under_dev_leaves_that_dev_as_it_was() {
    let files = Files::start("dev-refused-board", true);
    let mut dev = Dev::start(&files.local, files.dir(), "dev-refused-board");
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let put = browser.get("/put?key=before.txt");
    assert_eq!(put.status, 200, "{}", put.describe());
    let target = sentinel_folder(files.local.machine.root(), "dev-target", false);
    let link = files.dir().join(".vibe/local/a-name-the-tool-does-not-use");
    symlink(&target, &link).unwrap();
    let before = Tree::of(&target);
    let run = reset_run(&files.local, files.dir());
    let message = run.json_error(1, "local-state");
    assert!(
        message.contains("a-name-the-tool-does-not-use"),
        "expected the message to name the link, got {message:?}"
    );
    assert_eq!(
        before.differences(&Tree::of(&target)),
        Vec::<String>::new(),
        "expected the sentinel untouched"
    );
    assert!(!dev.has_ended(), "expected `vibe dev` still running");
    // The same `vibe dev` goes on answering, with what it had.
    let got = browser.get("/get?key=before.txt");
    assert_eq!(got.status, 200, "{}", got.describe());
    assert_eq!(
        got.json()["file"]["text"],
        json!("stored by the local run"),
        "expected the file put before the refused reset still there\n{}",
        got.describe()
    );
    // Through it, as a command asks: once the link is gone (a command's own
    // check would refuse while it is there).
    fs::remove_file(&link).unwrap();
    let listed = files.request("/list", BUILDER, &[]);
    assert_eq!(listed["status"], json!(200), "{listed}");
    assert_eq!(
        json_keys(&listed),
        ["before.txt"],
        "expected the file put before the refused reset still listed\n{listed}"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_resource_pulled_while_dev_runs_is_in_the_next_rebuild_with_its_folder() {
    let files = Files::start("late-pull-board", false);
    let folder = files.folder();
    let dev = Dev::start(&files.local, files.dir(), "late-pull-board");
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let before = browser.get("/get?key=late.txt");
    assert_eq!(before.status, 403, "{}", before.describe());
    assert!(
        !files.dir().join(".vibe/local/storage").exists(),
        "the harness's own check: nothing registered, no local Storage folder"
    );
    // The real pull, while `vibe dev` runs; the next request sees the
    // project changed and rebuilds.
    files.local.pull_ok(files.dir(), MARKETING);
    // The pull added the Builder to `vibe.dev.json`.
    browser.choose(BUILDER);
    let put = browser.get("/put?key=late.txt");
    assert_eq!(put.status, 200, "{}", put.describe());
    let got = browser.get("/get?key=late.txt");
    assert_eq!(got.status, 200, "{}", got.describe());
    assert_eq!(
        got.json()["file"]["text"],
        json!("stored by the local run"),
        "{}",
        got.describe()
    );
    let rebuilt = dev.wait_for_line("the `rebuilt` line", Duration::from_secs(60), |l| {
        l["event"] == "rebuilt"
    });
    assert_storages(&rebuilt, &[(MARKETING, true)], "the rebuild after the pull");
    assert!(
        folder.join(".vibe-resource").is_file(),
        "expected the folder made"
    );
    assert!(
        dev.events("rebuild-failed").is_empty(),
        "expected no failure about reading the project's files\n{}",
        dev.stdout()
    );
    dev.stop("INT").expect_code(0);
}

/// A project that has run once and is not running now, with
/// `.vibe/local/postgres/postmaster.pid` replaced by a link to what `make`
/// makes under the machine's own folder (removed with it).
fn board_with_pid_link(
    make: impl FnOnce(&Path) -> std::path::PathBuf,
) -> (Board, std::path::PathBuf, std::path::PathBuf) {
    let board = Board::start("read", "read");
    let warm = board.request_ok("/", OUTSIDER, &[]);
    assert_eq!(warm.status(), 200, "{}", warm.describe());
    let target = make(board.local.machine.root());
    let pid = board.dir().join(".vibe/local/postgres/postmaster.pid");
    drop(fs::remove_file(&pid));
    symlink(&target, &pid).unwrap();
    (board, pid, target)
}

#[test]
fn a_postmaster_pid_that_is_a_link_to_a_pipe_is_removed_and_not_waited_on() {
    let (board, pid, pipe) = board_with_pid_link(|root| {
        let pipe = root.join("nobody-writes-here");
        mkfifo(&pipe);
        pipe
    });
    // A hard limit: a run that waits on the pipe is killed, and fails.
    let running = board.local.spawn_request(board.dir(), "/", OUTSIDER, &[]);
    let run = running.wait(Duration::from_secs(60));
    assert_eq!(
        run.code,
        Some(0),
        "expected a normal run within the margin, not a wait on the pipe\n{}",
        run.describe()
    );
    assert!(
        fs::symlink_metadata(&pid).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            || !fs::symlink_metadata(&pid).unwrap().file_type().is_symlink(),
        "expected the link removed"
    );
    assert!(
        fs::metadata(&pipe).unwrap().file_type().is_fifo(),
        "expected the pipe still there"
    );
}

#[test]
fn a_postmaster_pid_that_is_a_link_to_a_big_file_is_removed_and_the_file_is_untouched() {
    let digits = "1234567890".repeat(104_858);
    let (board, pid, big) = board_with_pid_link(|root| {
        let big = root.join("digits.txt");
        fs::write(&big, &digits).unwrap();
        big
    });
    let running = board.local.spawn_request(board.dir(), "/", OUTSIDER, &[]);
    let run = running.wait(Duration::from_secs(60));
    assert_eq!(
        run.code,
        Some(0),
        "expected the run to go on as with no such file\n{}",
        run.describe()
    );
    assert_eq!(
        fs::read_to_string(&big).unwrap(),
        digits,
        "expected the file the link pointed to untouched"
    );
    assert!(
        fs::symlink_metadata(&pid).is_err()
            || !fs::symlink_metadata(&pid).unwrap().file_type().is_symlink(),
        "expected the link removed"
    );
}
