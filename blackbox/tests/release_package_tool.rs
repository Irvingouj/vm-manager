//! `scripts/package-tool.sh --binary <file> --target <triple> --out
//! <folder>` (CONTRACT.md, "The first release: What a release holds" and
//! "The scripts that make it"): it makes `<folder>/vibe-<v>-<target>.tar.gz`,
//! a gzip'd tar with one folder `vibe-<v>-<target>/` holding exactly `vibe`
//! (executable), `THIRD-PARTY-LICENSES` (the notices of
//! `third-party-licenses.sh` for that target) and `LICENSE` when the
//! repository has one, `<v>` being what the binary's `--version` says; it
//! prints the archive's path; and it refuses, leaving nothing in the
//! folder, a file that is not there, one that does not say `vibe
//! <version>` first, and a version other than the package's.
//!
//! Against the debug program the gate has, and programs of the test's own
//! standing in for it (shell scripts that print a first line).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use blackbox::program::vm_binary;
use blackbox::release::{
    COMMAND_TIMEOUT, EntryKind, Folder, Ran, notices_text, package_version, read_archive,
    repository_root, run, run_script, stand_in_program, unpack,
};

/// `scripts/package-tool.sh --binary <binary> --target <target> --out <out>`.
fn package(binary: &Path, target: &str, out: &Path) -> Ran {
    run_script(
        "package-tool.sh",
        &[
            "--binary",
            binary.to_str().unwrap(),
            "--target",
            target,
            "--out",
            out.to_str().unwrap(),
        ],
    )
}

/// The version `vibe --version` of `binary` says on its first line.
fn version_of(binary: &Path) -> String {
    let mut command = Command::new(binary);
    command.arg("--version");
    let ran = run(command, COMMAND_TIMEOUT);
    ran.expect_ok();
    let first = ran.stdout.lines().next().unwrap_or_default();
    first
        .strip_prefix("vibe ")
        .unwrap_or_else(|| panic!("expected `vibe <version>` first\n{}", ran.describe()))
        .to_owned()
}

/// Fails unless the run made `<out>/vibe-<version>-<target>.tar.gz`, the
/// folder holds that and nothing else, and the path is what it printed.
fn made_archive(ran: &Ran, out: &Path, version: &str, target: &str) -> PathBuf {
    ran.expect_ok();
    let name = format!("vibe-{version}-{target}.tar.gz");
    let names = blackbox::release::names_in(out);
    assert_eq!(
        names,
        vec![name.clone()],
        "expected the folder to hold the archive {name} and nothing else\n{}",
        ran.describe()
    );
    let archive = out.join(&name);
    let printed = ran
        .stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let printed_path = Path::new(printed);
    assert!(
        printed_path.exists()
            && printed_path.canonicalize().unwrap() == archive.canonicalize().unwrap(),
        "expected the script to print the archive's path {}, it printed {printed:?}\n{}",
        archive.display(),
        ran.describe()
    );
    archive
}

/// Fails unless `archive` is a gzip'd tar of one folder `vibe-<version>-
/// <target>/` holding exactly `vibe` (executable), `THIRD-PARTY-LICENSES`
/// equal to `notices`, and the repository's `LICENSE` when it has one,
/// unchanged.
fn assert_archive_holds(archive: &Path, version: &str, target: &str, licence: Option<&[u8]>) {
    let folder = format!("vibe-{version}-{target}");
    let entries = read_archive(archive).unwrap_or_else(|problem| panic!("{problem}"));
    let mut files = Vec::new();
    for entry in &entries {
        let path = entry.path.trim_start_matches("./").trim_end_matches('/');
        match entry.kind {
            EntryKind::Folder => assert_eq!(
                path, folder,
                "expected the archive's one folder to be {folder}/, it holds the folder {:?}",
                entry.path
            ),
            EntryKind::File => {
                let inside = path.strip_prefix(&format!("{folder}/")).unwrap_or_else(|| {
                    panic!(
                        "expected every file of the archive in its one folder {folder}/, found {:?}",
                        entry.path
                    )
                });
                files.push(inside.to_owned());
            }
            EntryKind::Other => panic!(
                "expected only the folder and regular files in the archive, found {:?} ({:?})",
                entry.path, entry.kind
            ),
        }
    }
    files.sort();
    let mut expected = vec!["THIRD-PARTY-LICENSES".to_owned(), "vibe".to_owned()];
    if licence.is_some() {
        expected.insert(0, "LICENSE".to_owned());
    }
    assert_eq!(
        files, expected,
        "expected the folder {folder}/ to hold exactly these files (LICENSE exactly when the repository has one)"
    );
    let file = |name: &str| {
        entries
            .iter()
            .find(|e| e.path.trim_start_matches("./") == format!("{folder}/{name}"))
            .unwrap()
    };
    let program = file("vibe");
    assert!(
        program.mode & 0o100 != 0,
        "expected vibe executable in the archive, its mode is {:o}",
        program.mode
    );
    assert!(program.size > 0, "expected vibe not to be empty");
    let notices = String::from_utf8(file("THIRD-PARTY-LICENSES").bytes.clone().unwrap())
        .expect("THIRD-PARTY-LICENSES is plain text");
    assert!(
        notices == notices_text(&["--target", target]),
        "expected the archive's THIRD-PARTY-LICENSES to be what `scripts/third-party-licenses.sh --target {target}` writes"
    );
    if let Some(licence) = licence {
        assert_eq!(
            file("LICENSE").bytes.as_deref(),
            Some(licence),
            "expected the archive's LICENSE to be the repository's, unchanged"
        );
    }
}

/// The repository's `LICENSE`, when it has one.
fn repository_licence() -> Option<Vec<u8>> {
    std::fs::read(repository_root().join("LICENSE")).ok()
}

#[test]
fn the_debug_program_packed_for_linux_is_one_folder_with_vibe_and_its_notices_and_says_the_version_unpacked()
 {
    let binary = vm_binary();
    let version = version_of(binary);
    let target = "x86_64-unknown-linux-gnu";
    let out = Folder::new("package");
    let ran = package(binary, target, out.path());
    let archive = made_archive(&ran, out.path(), &version, target);
    assert_archive_holds(&archive, &version, target, repository_licence().as_deref());

    let unpacked = Folder::new("unpacked");
    unpack(&archive, unpacked.path());
    let program = unpacked
        .path()
        .join(format!("vibe-{version}-{target}"))
        .join("vibe");
    assert_eq!(
        version_of(&program),
        package_version(),
        "expected the unpacked vibe to say `vibe <the package's version>`"
    );
}

#[test]
fn a_program_packed_for_macos_is_named_for_its_target_and_holds_that_targets_notices() {
    let version = package_version();
    let stand_ins = Folder::new("stand-in");
    let binary = stand_in_program(stand_ins.path(), "vibe", &format!("vibe {version}"));
    let target = "aarch64-apple-darwin";
    let out = Folder::new("package");
    let ran = package(&binary, target, out.path());
    let archive = made_archive(&ran, out.path(), &version, target);
    assert_archive_holds(&archive, &version, target, repository_licence().as_deref());
}

/// Fails unless the run was refused, said why (holding each of `words`),
/// and left nothing in `out`.
fn assert_refused(ran: &Ran, out: &Path, words: &[&str]) {
    assert!(
        ran.code.is_some_and(|c| c != 0),
        "expected the script to refuse (an exit code other than 0)\n{}",
        ran.describe()
    );
    assert!(
        !ran.said().trim().is_empty(),
        "expected the script to say why it refused\n{}",
        ran.describe()
    );
    for word in words {
        assert!(
            ran.said().contains(word),
            "expected the refusal to name {word:?}\n{}",
            ran.describe()
        );
    }
    let left = blackbox::release::names_in(out);
    assert!(
        left.is_empty(),
        "expected nothing left in the folder after a refusal, found {left:?}\n{}",
        ran.describe()
    );
}

#[test]
fn a_binary_that_is_not_there_is_refused_and_nothing_is_left_in_the_folder() {
    let stand_ins = Folder::new("stand-in");
    let missing = stand_ins.path().join("no-such-vibe");
    let out = Folder::new("package");
    let ran = package(&missing, "x86_64-unknown-linux-gnu", out.path());
    assert_refused(&ran, out.path(), &["no-such-vibe"]);
}

#[test]
fn a_binary_that_does_not_say_vibe_and_a_version_first_is_refused_and_nothing_is_left() {
    let version = package_version();
    let stand_ins = Folder::new("stand-in");
    for (name, first) in [
        ("other-name", format!("notvibe {version}")),
        ("capital", format!("Vibe {version}")),
        ("no-version", "vibe".to_owned()),
        ("empty-first-line", String::new()),
        ("something-else", "Usage: something".to_owned()),
    ] {
        let binary = stand_in_program(stand_ins.path(), name, &first);
        let out = Folder::new("package");
        let ran = package(&binary, "x86_64-unknown-linux-gnu", out.path());
        assert_refused(&ran, out.path(), &[]);
    }
    // A file that says nothing at all: not a program.
    let text = stand_ins.path().join("not-a-program");
    std::fs::write(&text, format!("vibe {version}\n")).unwrap();
    let out = Folder::new("package");
    let ran = package(&text, "x86_64-unknown-linux-gnu", out.path());
    assert_refused(&ran, out.path(), &[]);
}

#[test]
fn a_binary_of_another_version_than_the_packages_is_refused_and_nothing_is_left() {
    let stand_ins = Folder::new("stand-in");
    let binary = stand_in_program(stand_ins.path(), "vibe", "vibe 9.9.9");
    let out = Folder::new("package");
    let ran = package(&binary, "x86_64-unknown-linux-gnu", out.path());
    assert_refused(&ran, out.path(), &["9.9.9"]);
}
