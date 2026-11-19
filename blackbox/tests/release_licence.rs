//! The licence (CONTRACT.md, "The first release: The licence"): when the
//! repository has a `LICENSE` at its root, each archive of the tool holds
//! it, unchanged; when it has not, no archive holds one. (The image's side
//! is an install test, `install_image.rs`; `release-check.sh`'s side is in
//! `release_check.rs`.)
//!
//! Both ways are made in throwaway clones of the repository, one given a
//! `LICENSE` of the test's own and one without, whatever the repository
//! itself holds; nothing is committed in the repository.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::{Clone, EntryKind, package_version_in, read_archive, stand_in_program};

/// The files of the archive the clone's `package-tool.sh` makes of a
/// program saying the clone's version, by their path inside its one
/// folder, with their bytes.
fn archive_files(clone: &Clone) -> Vec<(String, Option<Vec<u8>>)> {
    let version = package_version_in(clone.root());
    let stand_ins = clone.beside("stand-in");
    let binary = stand_in_program(&stand_ins, "vibe", &format!("vibe {version}"));
    let out = clone.beside("out");
    let target = "x86_64-unknown-linux-gnu";
    let ran = clone.run_script(
        "package-tool.sh",
        &[
            "--binary",
            binary.to_str().unwrap(),
            "--target",
            target,
            "--out",
            out.to_str().unwrap(),
        ],
    );
    ran.expect_ok();
    let archive = out.join(format!("vibe-{version}-{target}.tar.gz"));
    let folder = format!("vibe-{version}-{target}/");
    let mut files: Vec<(String, Option<Vec<u8>>)> = read_archive(&archive)
        .unwrap_or_else(|problem| panic!("{problem}\n{}", ran.describe()))
        .into_iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| {
            let path = e.path.trim_start_matches("./");
            let inside = path.strip_prefix(&folder).unwrap_or(path).to_owned();
            (inside, e.bytes)
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
fn with_a_licence_in_the_repository_each_archive_holds_it_unchanged() {
    let clone = Clone::of_repository();
    let licence = clone.with_licence();
    let files = archive_files(&clone);
    let held = files.iter().find(|(name, _)| name == "LICENSE");
    assert!(
        held.is_some(),
        "expected the archive to hold LICENSE when the repository has one; it holds {:?}",
        files.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    assert_eq!(
        held.unwrap().1.as_deref(),
        Some(licence.as_slice()),
        "expected the archive's LICENSE to be the repository's, unchanged"
    );
}

#[test]
fn without_a_licence_in_the_repository_no_archive_holds_one() {
    let clone = Clone::of_repository();
    clone.without_licence();
    let files = archive_files(&clone);
    let names: Vec<&String> = files.iter().map(|(n, _)| n).collect();
    assert!(
        !names
            .iter()
            .any(|n| n.to_ascii_uppercase().starts_with("LICEN")),
        "expected no LICENSE in the archive when the repository has none; it holds {names:?}"
    );
    assert!(
        names.contains(&&"vibe".to_owned()) && names.contains(&&"THIRD-PARTY-LICENSES".to_owned()),
        "expected vibe and THIRD-PARTY-LICENSES in the archive; it holds {names:?}"
    );
}
