//! The local run keeps a key of its own (CONTRACT.md, "Resources and Grants in
//! full: At start", last bullet): 32 random bytes in base64 in
//! `.vibe/local/secret-key`, made at its first run, mode `0600`, never
//! printed and never read from the environment; `vibe local reset` leaves the
//! file in place; a link in its place is `local-state`, as for any other name
//! in `.vibe/local/`. (`vibe serve --help` says what the key seals: at the
//! end.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use blackbox::dev::Dev;
use blackbox::local::{Local, OUTSIDER, files_holding, local_project};
use blackbox::sdk::TempProject;
use blackbox::tool::Run;

const KEY_FILE: &str = ".vibe/local/secret-key";

/// A made-up value for `VIBE_SECRET_KEY` in the tool's environment: a good
/// key, and not the file's.
const ENVIRONMENT_KEY: &str = "ZW52aXJvbm1lbnQta2V5LXRoZS10b29sLWlnbm9yZXM=";

/// A machine and a project on it, which no run has touched. The project is
/// removed when it is dropped, so it is returned with its folder.
fn first_run(label: &str) -> (Local, TempProject, PathBuf) {
    let local = Local::new();
    let project = local_project(&local.machine, label, &[]);
    let dir = project.dir.clone();
    (local, project, dir)
}

fn key_text(dir: &Path) -> String {
    fs::read_to_string(dir.join(KEY_FILE))
        .unwrap_or_else(|e| panic!("expected {KEY_FILE} to be there, but: {e}"))
}

fn assert_a_key(text: &str) {
    let key = STANDARD
        .decode(text.trim())
        .unwrap_or_else(|e| panic!("expected {KEY_FILE} to hold base64 ({e})"));
    assert_eq!(key.len(), 32, "expected 32 bytes in {KEY_FILE}");
}

fn reset(local: &Local, dir: &Path) -> Run {
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

#[test]
fn a_first_run_makes_the_key_of_32_random_bytes_with_mode_0600_and_never_prints_it() {
    let (local, _project, dir) = first_run("sealed-local-first");
    assert!(
        !dir.join(KEY_FILE).exists(),
        "the harness's own check: no key before the first run"
    );
    let run = local.request(&dir, "/", OUTSIDER, &[]);
    run.expect_code(0);
    let text = key_text(&dir);
    assert_a_key(&text);
    let mode = fs::metadata(dir.join(KEY_FILE))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o600, "the mode of {KEY_FILE}: {mode:o}");
    let key = text.trim();
    assert!(
        !run.stdout.contains(key) && !run.stderr.contains(key),
        "expected the key to be printed nowhere by the run"
    );
    let holding = files_holding(&dir, key.as_bytes());
    assert_eq!(
        holding,
        vec![dir.join(KEY_FILE)],
        "expected the key in its file and in no other file under the project, local databases included"
    );
    // Another run keeps it.
    local.request(&dir, "/", OUTSIDER, &[]).expect_code(0);
    assert_eq!(key_text(&dir), text, "the key of a second run");
}

#[test]
fn a_key_in_the_tool_s_environment_is_not_read_and_changes_nothing() {
    let (local, _project, dir) = first_run("sealed-local-env");
    let run = local.request_with_env(
        &dir,
        "/",
        OUTSIDER,
        &[],
        &[("VIBE_SECRET_KEY", ENVIRONMENT_KEY)],
    );
    run.expect_code(0);
    let text = key_text(&dir);
    assert_a_key(&text);
    assert_ne!(
        text.trim(),
        ENVIRONMENT_KEY,
        "expected the local run's key not to be the environment's"
    );
    // A value that is no key at all in the environment changes nothing.
    let run = local.request_with_env(
        &dir,
        "/",
        OUTSIDER,
        &[],
        &[("VIBE_SECRET_KEY", "not a key at all")],
    );
    run.expect_code(0);
    assert_eq!(
        key_text(&dir),
        text,
        "the key after a run with another value in the environment"
    );
}

#[test]
fn vibe_local_reset_leaves_the_key_in_place() {
    let (local, _project, dir) = first_run("sealed-local-reset");
    local.request(&dir, "/", OUTSIDER, &[]).expect_code(0);
    let text = key_text(&dir);
    reset(&local, &dir).expect_code(0);
    assert_eq!(key_text(&dir), text, "the key after `vibe local reset`");
    local.request(&dir, "/", OUTSIDER, &[]).expect_code(0);
    assert_eq!(
        key_text(&dir),
        text,
        "the key after a run following the reset"
    );
}

#[test]
fn a_link_in_place_of_the_key_is_local_state_and_nothing_follows_it() {
    let (local, _project, dir) = first_run("sealed-local-link");
    local.request(&dir, "/", OUTSIDER, &[]).expect_code(0);
    let outside = local.machine.root().join("link-target");
    fs::write(&outside, "content that must stay as it is\n").unwrap();
    let key = dir.join(KEY_FILE);
    assert!(
        key.exists(),
        "expected {KEY_FILE} to be there after a first run, before a link is put in its place"
    );
    fs::remove_file(&key).unwrap();
    symlink(&outside, &key).unwrap();
    for (what, run) in [
        ("a request", local.request(&dir, "/", OUTSIDER, &[])),
        ("a reset", reset(&local, &dir)),
    ] {
        let message = run.json_error(1, "local-state");
        let lower = message.to_lowercase();
        assert!(
            message.contains(KEY_FILE) && lower.contains("remov"),
            "{what}: expected the message to name {KEY_FILE} and say to remove it, got {message:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "content that must stay as it is\n",
        "expected the link's target to be left alone"
    );
}

#[test]
fn vibe_dev_makes_the_key_too() {
    let (local, _project, dir) = first_run("sealed-local-dev");
    let dev = Dev::start(&local, &dir, "sealed-local-dev");
    let text = key_text(&dir);
    assert_a_key(&text);
    dev.stop("INT").expect_code(0);
}

#[test]
fn two_first_runs_at_once_make_one_valid_key_and_end_as_a_single_run_would() {
    for round in 0..3 {
        let (local, _project, dir) = first_run(&format!("sealed-local-race-{round}"));
        let first = local.spawn_request(&dir, "/", OUTSIDER, &[]);
        let second = local.spawn_request(&dir, "/", OUTSIDER, &[]);
        let timeout = std::time::Duration::from_secs(180);
        let (first, second) = (first.wait(timeout), second.wait(timeout));
        for (which, run) in [("first", &first), ("second", &second)] {
            assert_eq!(
                run.code,
                Some(0),
                "round {round}: expected the {which} of two runs started together to end as a single run does\n{}",
                run.describe()
            );
        }
        let text = key_text(&dir);
        assert_a_key(&text);
        let mode = fs::metadata(dir.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o600,
            "round {round}: the mode of {KEY_FILE}: {mode:o}"
        );
        local.request(&dir, "/", OUTSIDER, &[]).expect_code(0);
        assert_eq!(
            key_text(&dir),
            text,
            "round {round}: the key after a third run"
        );
    }
}
