//! No link is followed anywhere in the local state (CONTRACT.md, "Storage:
//! The local loop": "No link is followed in the local state", and the
//! bullet after it; ticket "Storage: a shared, named file Resource, on a
//! mounted directory now and S3 later"): when `.vibe` or `.vibe/local` is a
//! link, or anything directly inside `.vibe/local/` or `.vibe/local/storage/`
//! is one, to a folder, to a file or to nothing, every local run (`vibe
//! request`, `vibe dev`, `vibe local ...`) and `vibe local reset` end
//! `local-state` (exit code `1`) having removed, emptied, truncated,
//! created and written nothing anywhere; the message names the link and
//! says to remove it, and not that the local databases are started anew. A
//! project with no pulled Storage Resource has no `.vibe/local/storage/`.
//!
//! The cases of `.vibe/local/postgres` and of the logs, `state.json` and
//! the lock file need a run before: the private Postgres has run once, so
//! that the real entry is there to be put aside and a link put in its
//! place (the data folder is what `vibe local reset` would empty).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blackbox::dev::Dev;
use blackbox::local::{APP, BUILDER, Board, Local, OUTSIDER};
use blackbox::project::Tree;
use blackbox::storage_tool::Files;
use blackbox::tool::Run;

/// What a link points to.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// A folder holding a sentinel folder and a sentinel file.
    Folder,
    /// A file with content.
    File,
    /// A path that does not exist.
    Nothing,
}

/// Makes the target of `kind` under `root/link-targets/`; returns the
/// path of the target and the folder that holds every target.
fn target_of(root: &Path, kind: Target) -> (PathBuf, PathBuf) {
    let all = root.join("link-targets");
    fs::create_dir_all(&all).unwrap();
    let target = match kind {
        Target::Folder => {
            let folder = all.join("a-folder");
            fs::create_dir_all(folder.join("sentinel-folder/deep")).unwrap();
            fs::write(
                folder.join("sentinel-folder/deep/inner.txt"),
                "inner, kept\n",
            )
            .unwrap();
            fs::write(folder.join("sentinel.txt"), "a sentinel file, kept\n").unwrap();
            folder
        }
        Target::File => {
            let file = all.join("a-file.txt");
            fs::write(&file, "content that must stay as it is\n").unwrap();
            file
        }
        Target::Nothing => all.join("does-not-exist"),
    };
    (target, all)
}

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

fn dev_run(local: &Local, dir: &Path, name: &str) -> Run {
    Dev::spawn_on_a_free_port(local, dir, name, true).wait_end_trying_ports(Duration::from_secs(90))
}

/// Fails unless `run` ended `local-state`, exit code `1`, with a message
/// that names `link` and says to remove it, and not that the local
/// databases are started anew.
fn assert_local_state_names(run: &Run, link: &str, context: &str) {
    let message = run.json_error(1, "local-state");
    let lower = message.to_lowercase();
    assert!(
        message.contains(link) && lower.contains("remov"),
        "{context}: expected the message to name the link {link:?} and say to remove it, got {message:?}"
    );
    assert!(
        !lower.contains("anew") && !lower.contains("start the local databases"),
        "{context}: expected the message not to say the local databases are started anew, got {message:?}"
    );
}

#[test]
fn a_link_directly_inside_the_local_state_is_followed_by_no_run_and_no_reset() {
    let board = Board::start("read", "read");
    // One run, so that the private Postgres has run once: its data folder,
    // `vibe.log`, `postgres.log`, `state.json` and the lock file are there.
    let warm = board.request_ok("/", OUTSIDER, &[]);
    assert_eq!(warm.status(), 200, "{}", warm.describe());
    let dir = board.dir().to_path_buf();
    let local_state = dir.join(".vibe/local");
    let stash = board.local.machine.root().join("stash");
    fs::create_dir_all(&stash).unwrap();
    let cases = [
        ("postgres", Target::Folder),
        ("vibe.log", Target::File),
        ("postgres.log", Target::File),
        ("state.json", Target::File),
        ("lock", Target::Nothing),
        ("anything", Target::Folder),
        ("storage/not-a-resource", Target::Folder),
    ];
    for (name, kind) in cases {
        let context = format!(".vibe/local/{name} a link to {kind:?}");
        let link = local_state.join(name);
        let put_aside = stash.join(name.replace('/', "-"));
        fs::create_dir_all(local_state.join("storage")).unwrap();
        if fs::symlink_metadata(&link).is_ok() {
            fs::rename(&link, &put_aside).unwrap();
        }
        let (target, all) = target_of(board.local.machine.root(), kind);
        symlink(&target, &link).unwrap();
        let before = Tree::of(&all);
        let link_text = format!(".vibe/local/{name}");
        for (command, run) in [
            (
                "vibe request",
                board.local.request(&dir, "/", OUTSIDER, &[]),
            ),
            ("vibe local reset", reset_run(&board.local, &dir)),
            (
                "vibe dev",
                dev_run(&board.local, &dir, blackbox::local::APP),
            ),
        ] {
            assert_local_state_names(&run, &link_text, &format!("{context}, {command}"));
            assert_eq!(
                before.differences(&Tree::of(&all)),
                Vec::<String>::new(),
                "{context}, {command}: expected what the link points to as it was, entry for entry"
            );
            assert!(
                fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()),
                "{context}, {command}: expected the link still there"
            );
            if matches!(kind, Target::Nothing) {
                assert!(
                    fs::symlink_metadata(&target).is_err(),
                    "{context}, {command}: expected a link to nothing still pointing at nothing"
                );
            }
        }
        // Put it as it was for the next case.
        fs::remove_file(&link).unwrap();
        if put_aside.exists() {
            fs::rename(&put_aside, &link).unwrap();
        }
        fs::remove_dir_all(&all).unwrap();
    }
}

#[test]
fn a_project_with_no_pulled_storage_resource_has_no_local_storage_folder() {
    let board = Board::start("read", "read");
    let folder = board.dir().join(".vibe/local/storage");
    let first = board.request_ok("/", BUILDER, &[]);
    assert_eq!(first.status(), 200, "{}", first.describe());
    assert!(
        !folder.exists(),
        "expected no .vibe/local/storage/ made by a local run of a project with no Storage Resource"
    );
    let reset = reset_run(&board.local, board.dir());
    assert_eq!(reset.code, Some(0), "{}", reset.describe());
    assert!(!folder.exists(), "expected none made by a reset either");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    assert_eq!(browser.get("/").status, 200);
    assert!(!folder.exists(), "expected none made by `vibe dev` either");
    dev.stop("INT").expect_code(0);
    assert!(!folder.exists(), "expected none after `vibe dev` ended");
}

#[test]
fn a_reset_asked_of_a_running_dev_checks_the_links_again_before_it_empties_anything() {
    // CONTRACT.md: checked "again before a reset started while `vibe dev`
    // runs empties anything". `vibe local reset` in a project where
    // `vibe dev` runs is answered by it.
    let files = Files::start("dev-links-board", true);
    let dev = Dev::start(&files.local, files.dir(), "dev-links-board");
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let put = browser.get("/put?key=kept.txt");
    assert_eq!(put.status, 200, "{}", put.describe());
    let stash = files.local.machine.root().join("stash");
    fs::create_dir_all(&stash).unwrap();
    for (name, kind) in [
        (blackbox::storage::MARKETING, Target::Folder),
        ("not-a-resource", Target::Folder),
    ] {
        let link = files.dir().join(".vibe/local/storage").join(name);
        let put_aside = stash.join(name);
        if fs::symlink_metadata(&link).is_ok() {
            fs::rename(&link, &put_aside).unwrap();
        }
        let (target, all) = target_of(files.local.machine.root(), kind);
        symlink(&target, &link).unwrap();
        let before = Tree::of(&all);
        let run = reset_run(&files.local, files.dir());
        assert_local_state_names(
            &run,
            &format!(".vibe/local/storage/{name}"),
            &format!("a reset asked of a running `vibe dev`, {name} a link"),
        );
        assert_eq!(
            before.differences(&Tree::of(&all)),
            Vec::<String>::new(),
            "{name}: expected nothing emptied through the link"
        );
        fs::remove_file(&link).unwrap();
        if put_aside.exists() {
            fs::rename(&put_aside, &link).unwrap();
        }
        fs::remove_dir_all(&all).unwrap();
    }
    assert_eq!(
        fs::read(files.folder().join("kept.txt")).unwrap(),
        b"stored by the local run".as_slice(),
        "expected what was put before still there: nothing emptied"
    );
    dev.stop("INT");
}
