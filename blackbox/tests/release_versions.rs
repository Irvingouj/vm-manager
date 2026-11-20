//! One version (CONTRACT.md, "The first release: One version"; ticket "A
//! first, minimal release"): the program's version is the package `vibe`'s,
//! `vibe --version` says it on its first line, it is `0.1.1` now (it was
//! `0.1.0` for that ticket), and every file of the release that names a version names that
//! one: the `Dockerfile`'s default, the image the Compose file takes, the
//! image `.env.example` and `deploy/README.md` show, the addresses and
//! archives of `docs/getting-started.md`, and the notes' file.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::process::Command;

use blackbox::program::vm_binary;
use blackbox::release::{
    COMMAND_TIMEOUT, IMAGE, TARGETS, VERSION, package_version, read_repository_file,
    repository_root, run,
};

/// Every `<tag>` of `ghcr.io/irvingouj/vibe:<tag>` in `text` that is a
/// version (it begins with a digit), with its line; placeholders such as
/// `<the newer version>` are not versions.
fn image_versions_in(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let prefix = format!("{IMAGE}:");
    for line in text.lines() {
        for (at, _) in line.match_indices(&prefix) {
            let (_, tail) = line.split_at(at + prefix.len());
            let tag: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
                .collect();
            if tag.starts_with(|c: char| c.is_ascii_digit()) {
                found.push((tag, line.to_owned()));
            }
        }
    }
    found
}

#[test]
fn the_first_line_of_vibe_version_is_vibe_and_the_packages_version_and_that_is_0_1_0() {
    let mut command = Command::new(vm_binary());
    command.arg("--version");
    let ran = run(command, COMMAND_TIMEOUT);
    ran.expect_ok();
    let first = ran.stdout.lines().next().unwrap_or_default();
    let package = package_version();
    assert_eq!(
        first,
        format!("vibe {package}"),
        "expected the first line of `vibe --version` to be `vibe <the package vibe's version>`\n{}",
        ran.describe()
    );
    assert_eq!(
        package, VERSION,
        "expected the package vibe's version (cargo metadata) to be {VERSION} for this ticket"
    );
}

#[test]
fn the_dockerfiles_default_of_vibe_version_is_the_version() {
    let dockerfile = read_repository_file("Dockerfile");
    let defaults: Vec<&str> = dockerfile
        .lines()
        .filter_map(|l| l.trim().strip_prefix("ARG VIBE_VERSION="))
        .map(str::trim)
        .collect();
    assert!(
        !defaults.is_empty(),
        "expected a line `ARG VIBE_VERSION=<version>` in the Dockerfile"
    );
    for default in &defaults {
        assert_eq!(
            *default, VERSION,
            "expected the Dockerfile's default of VIBE_VERSION to be {VERSION}, got {defaults:?}"
        );
    }
}

#[test]
fn the_image_compose_takes_without_vibe_image_and_the_one_env_example_and_readme_show_are_of_the_version()
 {
    let compose = read_repository_file("deploy/compose.yaml");
    let default = format!("${{VIBE_IMAGE:-{IMAGE}:{VERSION}}}");
    assert!(
        compose.contains(&default),
        "expected deploy/compose.yaml to take {default} without VIBE_IMAGE; its lines naming the image:\n{}",
        compose
            .lines()
            .filter(|l| l.contains(IMAGE))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let example = read_repository_file("deploy/.env.example");
    assert!(
        !image_versions_in(&example).is_empty(),
        "expected deploy/.env.example to show the image {IMAGE}:<version>"
    );
    let mut wrong = Vec::new();
    for file in [
        "deploy/compose.yaml",
        "deploy/compose.own-proxy.yaml",
        "deploy/compose.own-postgres.yaml",
        "deploy/compose.own-tls.yaml",
        "deploy/.env.example",
        "deploy/README.md",
    ] {
        let text = read_repository_file(file);
        for (tag, line) in image_versions_in(&text) {
            if tag != VERSION {
                wrong.push(format!("{file}: {line:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "expected every {IMAGE}:<version> of deploy/ to name {VERSION}, found:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn every_release_address_and_archive_of_the_getting_started_page_names_the_version() {
    let page = read_repository_file("docs/getting-started.md");
    let mut addresses = 0;
    let mut wrong = Vec::new();
    for (at, _) in page.match_indices("/releases/download/") {
        let (_, tail) = page.split_at(at + "/releases/download/".len());
        let tag: String = tail.chars().take_while(|c| *c != '/').collect();
        addresses += 1;
        if tag != format!("v{VERSION}") {
            wrong.push(format!("an address of the release {tag:?}"));
        }
    }
    assert!(
        addresses > 0,
        "expected docs/getting-started.md to give the addresses of the release's files (.../releases/download/v{VERSION}/...)"
    );
    let mut archives = 0;
    for target in TARGETS {
        let suffix = format!("-{target}.tar.gz");
        for (at, _) in page.match_indices(&suffix) {
            let (head, _) = page.split_at(at);
            let Some(start) = head.rfind("vibe-") else {
                continue;
            };
            let (_, version) = head.split_at(start + "vibe-".len());
            archives += 1;
            if version != VERSION {
                wrong.push(format!("the archive vibe-{version}{suffix}"));
            }
        }
    }
    assert!(
        archives >= TARGETS.len(),
        "expected docs/getting-started.md to name the archives vibe-{VERSION}-<target>.tar.gz of both targets"
    );
    assert!(
        wrong.is_empty(),
        "expected every release address and archive of docs/getting-started.md to name {VERSION}, found: {wrong:?}"
    );
}

#[test]
fn the_notes_of_the_versions_release_are_a_file_of_docs_releases() {
    let notes = repository_root()
        .join("docs")
        .join("releases")
        .join(format!("v{VERSION}.md"));
    assert!(
        notes.is_file(),
        "expected the release's notes at docs/releases/v{VERSION}.md ({})",
        notes.display()
    );
    let package = package_version();
    let named = repository_root()
        .join("docs")
        .join("releases")
        .join(format!("v{package}.md"));
    assert!(
        named.is_file(),
        "expected the notes' file to be named by the package's version: docs/releases/v{package}.md"
    );
}

/// The harness's own check of [`image_versions_in`].
#[test]
fn the_images_versions_are_found_and_placeholders_are_not() {
    let text = format!(
        "image: ${{VIBE_IMAGE:-{IMAGE}:1.2.3}}\n#VIBE_IMAGE={IMAGE}:0.0.0\nVIBE_IMAGE={IMAGE}:<the newer version>\n{IMAGE}:latest"
    );
    let found: Vec<String> = image_versions_in(&text)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert_eq!(found, vec!["1.2.3".to_owned(), "0.0.0".to_owned()]);
}
