//! `scripts/release-files.sh --out <folder>` (CONTRACT.md, "The first
//! release: What a release holds" and "The scripts that make it"): it
//! copies the three Compose files and `env.example` (`deploy/.env.example`)
//! into the folder, which may already hold the archives, unchanged, and
//! writes `SHA256SUMS` over every file then in it: one line each, `<64 hex
//! digits>`, two spaces, the name, in the names' order, so that
//! `sha256sum --check SHA256SUMS` passes there, `--ignore-missing` checks
//! the files that are there, and a file changed afterwards fails it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;
use std::process::Command;

use blackbox::release::{
    COMMAND_TIMEOUT, Folder, Ran, TARGETS, VERSION, repository_root, run, run_script, sha256_hex,
};

/// The release's files copied from `deploy/`, by their name in the release
/// and in the repository.
const COPIED: [(&str, &str); 4] = [
    ("compose.yaml", "deploy/compose.yaml"),
    ("compose.own-proxy.yaml", "deploy/compose.own-proxy.yaml"),
    (
        "compose.own-postgres.yaml",
        "deploy/compose.own-postgres.yaml",
    ),
    ("env.example", "deploy/.env.example"),
];

/// A folder holding two archives of the test's own, as the workflow's
/// packing leaves it, and `release-files.sh --out` run on it.
fn release_folder() -> (Folder, Ran, Vec<String>) {
    let out = Folder::new("files");
    let mut archives = Vec::new();
    for target in TARGETS {
        let name = format!("vibe-{VERSION}-{target}.tar.gz");
        fs::write(
            out.path().join(&name),
            format!("the release tests' stand-in for the archive of {target}\n"),
        )
        .unwrap();
        archives.push(name);
    }
    let ran = run_script("release-files.sh", &["--out", out.path().to_str().unwrap()]);
    (out, ran, archives)
}

/// `<program> <args>` in `dir`.
fn check_in(dir: &Path, program: &str, args: &[&str]) -> Ran {
    let mut command = Command::new(program);
    command.args(args).current_dir(dir);
    run(command, COMMAND_TIMEOUT)
}

#[test]
fn the_compose_files_and_env_example_are_copied_unchanged_beside_the_archives() {
    let (out, ran, archives) = release_folder();
    ran.expect_ok();
    let mut expected: Vec<String> = COPIED.iter().map(|(n, _)| (*n).to_owned()).collect();
    expected.extend(archives.iter().cloned());
    expected.push("SHA256SUMS".to_owned());
    expected.sort();
    assert_eq!(
        out.names(),
        expected,
        "expected the folder to hold the archives, the three Compose files, env.example and SHA256SUMS, and nothing else\n{}",
        ran.describe()
    );
    for (name, source) in COPIED {
        assert_eq!(
            fs::read(out.path().join(name)).unwrap(),
            fs::read(repository_root().join(source)).unwrap(),
            "expected {name} to be {source}, unchanged"
        );
    }
    for archive in &archives {
        assert!(
            fs::read_to_string(out.path().join(archive))
                .unwrap()
                .starts_with("the release tests' stand-in"),
            "expected the archive {archive} that was there left unchanged"
        );
    }
}

#[test]
fn sha256sums_has_one_line_per_file_in_the_names_order_in_sha256sums_form() {
    let (out, ran, _) = release_folder();
    ran.expect_ok();
    let sums = fs::read_to_string(out.path().join("SHA256SUMS"))
        .unwrap_or_else(|e| panic!("expected SHA256SUMS in the folder: {e}\n{}", ran.describe()));
    assert!(
        sums.ends_with('\n'),
        "expected SHA256SUMS to end with a newline, as sha256sum writes it"
    );
    let mut names = Vec::new();
    for line in sums.lines() {
        let (hash, name) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("expected `<64 hex digits>  <name>`, got {line:?}"));
        assert!(
            hash.len() == 64
                && hash
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "expected 64 lowercase hex digits first, got {line:?}"
        );
        assert!(
            !name.starts_with('*') && !name.contains('/'),
            "expected the bare name of a file of the folder after the two spaces, got {line:?}"
        );
        let bytes = fs::read(out.path().join(name)).unwrap_or_else(|e| {
            panic!("SHA256SUMS names {name:?}, which is not in the folder: {e}")
        });
        assert_eq!(
            hash,
            sha256_hex(&bytes),
            "expected the SHA-256 of {name} in its line"
        );
        names.push(name.to_owned());
    }
    let mut expected: Vec<String> = out
        .names()
        .into_iter()
        .filter(|n| n != "SHA256SUMS")
        .collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "expected one line for each file of the folder but SHA256SUMS, in the names' order"
    );
}

#[test]
fn sha256sum_check_passes_in_the_folder_and_fails_once_a_file_is_changed() {
    let (out, ran, archives) = release_folder();
    ran.expect_ok();
    let checked = check_in(out.path(), "sha256sum", &["--check", "SHA256SUMS"]);
    assert!(
        checked.ok(),
        "expected `sha256sum --check SHA256SUMS` to pass in the folder\n{}",
        checked.describe()
    );

    // Only some downloaded: --ignore-missing checks those.
    fs::remove_file(out.path().join(&archives[1])).unwrap();
    let some = check_in(
        out.path(),
        "sha256sum",
        &["--check", "--ignore-missing", "SHA256SUMS"],
    );
    assert!(
        some.ok(),
        "expected `sha256sum --check --ignore-missing SHA256SUMS` to pass with one archive not downloaded\n{}",
        some.describe()
    );

    fs::write(
        out.path().join(&archives[0]),
        b"changed after the sums were written\n",
    )
    .unwrap();
    let changed = check_in(
        out.path(),
        "sha256sum",
        &["--check", "--ignore-missing", "SHA256SUMS"],
    );
    assert!(
        !changed.ok(),
        "expected `sha256sum --check` to fail once {} was changed\n{}",
        archives[0],
        changed.describe()
    );
    assert!(
        changed.said().contains(&archives[0]),
        "expected the failure to name {}\n{}",
        archives[0],
        changed.describe()
    );
}
