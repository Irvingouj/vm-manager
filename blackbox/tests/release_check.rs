//! `scripts/release-check.sh [<tag>]` (CONTRACT.md, "The first release:
//! The scripts that make it" and "The licence"): it checks what must hold
//! before a release is built and says each thing that does not; exit code
//! 0 only when everything asked holds. Without a tag: the versions of "One
//! version" agree and the notes' file exists (a rehearsal goes on without
//! `LICENSE`, saying it is missing). With a tag: also the tag is
//! `v<version>`, its commit is on `main` (`origin/main` or an ancestor of
//! it), and `LICENSE` exists.
//!
//! The cases with a tag, a `LICENSE` or a file changed are made in a
//! throwaway clone of the repository (its `main` as committed), never in
//! the repository itself, and nothing is pushed anywhere.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::{Clone, IMAGE, Ran, package_version_in, repository_root, run_script};

/// Fails unless the check passed.
fn assert_passes(ran: &Ran, context: &str) {
    assert!(
        ran.ok(),
        "expected `release-check.sh` to pass {context}\n{}",
        ran.describe()
    );
}

/// Fails unless the check failed (an exit code other than 0) saying
/// something holding each of `words`.
fn assert_fails_naming(ran: &Ran, context: &str, words: &[&str]) {
    assert!(
        ran.code.is_some_and(|c| c != 0),
        "expected `release-check.sh` to fail {context}\n{}",
        ran.describe()
    );
    for word in words {
        assert!(
            ran.said().contains(word),
            "expected `release-check.sh` failing {context} to name {word:?}\n{}",
            ran.describe()
        );
    }
}

#[test]
fn in_the_repository_as_it_is_the_check_without_a_tag_passes_and_says_when_licence_is_missing() {
    let ran = run_script("release-check.sh", &[]);
    assert_passes(&ran, "in the repository as it is, without a tag");
    if !repository_root().join("LICENSE").exists() {
        assert!(
            ran.said().contains("LICENSE"),
            "expected the check without a tag to go on, saying that LICENSE is missing\n{}",
            ran.describe()
        );
    }
}

#[test]
fn the_tag_of_the_version_on_main_with_a_licence_passes() {
    let clone = Clone::of_repository();
    let version = package_version_in(clone.root());
    clone.with_licence();
    let tag = format!("v{version}");
    clone.tag(&tag, &clone.head());
    let ran = clone.run_script("release-check.sh", &[&tag]);
    assert_passes(
        &ran,
        &format!("with the tag {tag} on origin/main and a LICENSE"),
    );

    // A commit of main that is an ancestor of origin/main passes too.
    let tagged = clone.head();
    clone.commit("the release tests: a later commit of main");
    clone.origin_main_at_head();
    clone.git(&["tag", "-d", &tag]);
    clone.tag(&tag, &tagged);
    let ran = clone.run_script("release-check.sh", &[&tag]);
    assert_passes(
        &ran,
        &format!("with the tag {tag} on an ancestor of origin/main and a LICENSE"),
    );
}

#[test]
fn the_tag_of_the_version_without_a_licence_fails_naming_licence() {
    let clone = Clone::of_repository();
    let version = package_version_in(clone.root());
    clone.without_licence();
    let tag = format!("v{version}");
    clone.tag(&tag, &clone.head());
    let ran = clone.run_script("release-check.sh", &[&tag]);
    assert_fails_naming(
        &ran,
        &format!("with the tag {tag} and no LICENSE"),
        &["LICENSE"],
    );
}

#[test]
fn a_tag_of_another_version_fails_naming_it() {
    let clone = Clone::of_repository();
    clone.with_licence();
    for tag in ["v9.9.9", "v0.1", "0.1.0-not-a-version-tag"] {
        clone.tag(tag, &clone.head());
        let ran = clone.run_script("release-check.sh", &[tag]);
        assert_fails_naming(
            &ran,
            &format!("with the tag {tag}, which is not v<the package's version>"),
            &[tag],
        );
    }
}

#[test]
fn a_tag_on_a_commit_that_is_not_on_origin_main_fails() {
    let clone = Clone::of_repository();
    let version = package_version_in(clone.root());
    clone.with_licence();
    // A commit after origin/main, on main, not pushed.
    let ahead = clone.commit("the release tests: a commit not on origin/main");
    let tag = format!("v{version}");
    clone.tag(&tag, &ahead);
    let ran = clone.run_script("release-check.sh", &[&tag]);
    assert_fails_naming(
        &ran,
        &format!("with the tag {tag} on a commit after origin/main"),
        &["main"],
    );

    // A commit of another branch, beside origin/main.
    clone.git(&["checkout", "--quiet", "-b", "elsewhere", "origin/main~1"]);
    let beside = clone.commit("the release tests: a commit of another branch");
    clone.git(&["tag", "-d", &tag]);
    clone.tag(&tag, &beside);
    clone.git(&["checkout", "--quiet", "main"]);
    let ran = clone.run_script("release-check.sh", &[&tag]);
    assert_fails_naming(
        &ran,
        &format!("with the tag {tag} on a commit of another branch"),
        &["main"],
    );
}

/// Replaces every `from` of the clone's `relative` with `to`; fails when
/// there is none.
fn change(clone: &Clone, relative: &str, from: &str, to: &str) {
    let text = clone.read(relative);
    assert!(
        text.contains(from),
        "expected {relative} in the clone to hold {from:?}, which the test changes"
    );
    clone.write(relative, text.replace(from, to).as_bytes());
}

#[test]
fn a_version_that_disagrees_in_one_place_fails_naming_the_place() {
    let clone = Clone::of_repository();
    let version = package_version_in(clone.root());
    let other = "9.8.7";
    let places: [(&str, String, String, &str); 4] = [
        (
            "Dockerfile",
            format!("ARG VIBE_VERSION={version}"),
            format!("ARG VIBE_VERSION={other}"),
            "Dockerfile",
        ),
        (
            "deploy/compose.yaml",
            format!("{IMAGE}:{version}"),
            format!("{IMAGE}:{other}"),
            "compose.yaml",
        ),
        (
            "deploy/.env.example",
            format!("{IMAGE}:{version}"),
            format!("{IMAGE}:{other}"),
            ".env.example",
        ),
        (
            "docs/getting-started.md",
            format!("/releases/download/v{version}/"),
            format!("/releases/download/v{other}/"),
            "getting-started.md",
        ),
    ];
    for (file, from, to, named) in &places {
        change(&clone, file, from, to);
        let ran = clone.run_script("release-check.sh", &[]);
        assert_fails_naming(
            &ran,
            &format!("with {file} naming {other} where the version is {version}"),
            &[named],
        );
        clone.git(&["checkout", "--quiet", "--", file]);
    }

    // An archive's name in the page.
    change(
        &clone,
        "docs/getting-started.md",
        &format!("vibe-{version}-x86_64-unknown-linux-gnu.tar.gz"),
        &format!("vibe-{other}-x86_64-unknown-linux-gnu.tar.gz"),
    );
    let ran = clone.run_script("release-check.sh", &[]);
    assert_fails_naming(
        &ran,
        "with an archive's name in docs/getting-started.md of another version",
        &["getting-started.md"],
    );
    clone.git(&["checkout", "--quiet", "--", "docs/getting-started.md"]);

    // The notes' file missing.
    let notes = format!("docs/releases/v{version}.md");
    clone.remove(&notes);
    let ran = clone.run_script("release-check.sh", &[]);
    assert_fails_naming(
        &ran,
        &format!("without {notes}"),
        &[&format!("v{version}.md")],
    );
    clone.git(&["checkout", "--quiet", "--", &notes]);

    // And, put back, the clone passes again: it was the one place.
    let ran = clone.run_script("release-check.sh", &[]);
    assert_passes(&ran, "in the clone with every place put back");
}
