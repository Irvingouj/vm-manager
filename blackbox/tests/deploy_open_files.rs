//! The open files of the installed program, as `deploy/` says them, in the
//! daily gate (no Docker needed: the running container's limit is looked at
//! by the install tests, `install_walk.rs` and `install_own_tls.rs`).
//!
//! CONTRACT.md, "Installing: one image and a Compose file: The open files":
//! the `vibe` service of `deploy/compose.yaml` has `ulimits: nofile` 65536
//! soft and hard, and no other Compose file of `deploy/` takes it away or
//! lowers it; `deploy/README.md` says so where it says how `vibe` is
//! hardened.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::install::VIBE_OPEN_FILES;
use blackbox::release::read_repository_file;
use yaml_rust2::{Yaml, YamlLoader};

fn compose(file: &str) -> Yaml {
    let text = read_repository_file(file);
    let mut docs = YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("expected {file} to be YAML: {e}"));
    assert_eq!(docs.len(), 1, "expected one YAML document in {file}");
    docs.remove(0)
}

fn as_number(value: &Yaml) -> Option<u64> {
    match value {
        Yaml::Integer(n) => u64::try_from(*n).ok(),
        Yaml::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[test]
fn the_vibe_service_may_open_65536_files_soft_and_hard() {
    let file = compose("deploy/compose.yaml");
    let nofile = &file["services"]["vibe"]["ulimits"]["nofile"];
    assert_eq!(
        (as_number(&nofile["soft"]), as_number(&nofile["hard"])),
        (Some(VIBE_OPEN_FILES), Some(VIBE_OPEN_FILES)),
        "expected `ulimits: nofile: {{soft: {VIBE_OPEN_FILES}, hard: {VIBE_OPEN_FILES}}}` on the vibe service of deploy/compose.yaml, found {nofile:?}"
    );
}

#[test]
fn no_other_compose_file_changes_the_vibe_services_open_files() {
    for file in [
        "deploy/compose.own-proxy.yaml",
        "deploy/compose.own-postgres.yaml",
        "deploy/compose.own-tls.yaml",
    ] {
        let yaml = compose(file);
        let ulimits = &yaml["services"]["vibe"]["ulimits"];
        assert!(
            ulimits.is_badvalue(),
            "expected {file} to leave the vibe service's `ulimits` as deploy/compose.yaml sets them, it has {ulimits:?}"
        );
    }
}

#[test]
fn the_install_document_says_how_many_files_vibe_may_open() {
    let readme = read_repository_file("deploy/README.md");
    assert!(
        readme.lines().any(|line| line.contains("`vibe`")
            && line.contains("65,536")
            && line.contains("nofile")),
        "expected deploy/README.md to say, on one line, that `vibe` may open 65,536 files (`ulimits: nofile`)"
    );
}
