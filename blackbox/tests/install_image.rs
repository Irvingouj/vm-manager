//! The image (CONTRACT.md, "Installing: one image and a Compose file: The
//! image"; ticket "Install: one image and a Compose file, hardened"), as
//! an outsider with Docker sees it, without Compose: what `docker run`
//! prints, what `docker image inspect` says, and what is in its
//! filesystem (`docker create`, then `docker cp` and `docker export`).
//!
//! Against the image `VIBE_INSTALL_IMAGE` names; run by
//! `scripts/install-gate.sh`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use blackbox::install::{DOCKER_WAIT, EXPORT_WAIT, Ran, docker, docker_ok, image, inspect_image};
use blackbox::program::{run_to_exit, vm_binary};
use blackbox::sdk::repository_root;
use blackbox::shared_database::token;

/// Where the image holds the notices of what it is built from.
const LICENSES: &str = "/usr/share/doc/vibe/THIRD-PARTY-LICENSES";

/// The harness's own `vibe` with these arguments, nothing of the
/// installation in its environment: its exit code and what it printed.
fn harness_vibe(args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = Command::new(vm_binary());
    command.args(args);
    for name in ["VIBE_DATABASE_URL", "VIBE_SECRET_KEY", "VIBE_CA_FILE"] {
        command.env_remove(name);
    }
    let finished = run_to_exit(command, Duration::from_secs(30)).unwrap();
    (
        finished.status.and_then(|s| s.code()),
        finished.stdout,
        finished.stderr,
    )
}

/// A container made from the image and never started (`docker create`),
/// removed when dropped.
struct Created {
    name: String,
}

impl Created {
    fn of(image: &str) -> Self {
        let name = format!("vibe-install-{}-created", token());
        let created = Self { name };
        docker_ok(&["create", "--name", &created.name, image], DOCKER_WAIT);
        created
    }

    /// `docker cp <container>:<path> <to>`.
    fn copy_out(&self, path: &str, to: &Path) -> Ran {
        docker(
            &["cp", &format!("{}:{path}", self.name), to.to_str().unwrap()],
            DOCKER_WAIT,
        )
    }
}

impl Drop for Created {
    fn drop(&mut self) {
        drop(docker(
            &["rm", "--force", "--volumes", &self.name],
            DOCKER_WAIT,
        ));
    }
}

/// A folder of the test's own, removed when dropped.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("vm-blackbox-image-{}", token()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_image_runs_vibe_which_prints_what_the_programs_version_prints() {
    let image = image();
    let (code, want, _) = harness_vibe(&["--version"]);
    assert_eq!(code, Some(0));
    let ran = docker(
        &["run", "--rm", "--pull", "never", &image, "--version"],
        DOCKER_WAIT,
    );
    assert!(
        ran.ok(),
        "expected `docker run <image> --version` to end with 0\n{}",
        ran.describe(&[])
    );
    // The first line (the name and the version) is the working tree's;
    // the lines after it describe the build, and are only to be there.
    let first = |text: &str| text.lines().next().unwrap_or_default().to_owned();
    assert_eq!(
        first(&ran.stdout),
        first(&want),
        "expected the first line of `docker run <image> --version` to be that of `vibe --version` of the working tree\n{}",
        ran.describe(&[])
    );
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert!(
        lines.len() == want.lines().count() && lines.iter().all(|l| !l.trim().is_empty()),
        "expected `docker run <image> --version` to print as many lines as `vibe --version` ({}), none empty\n{}",
        want.lines().count(),
        ran.describe(&[])
    );
    // `vibe` is on the path.
    let on_path = docker(
        &[
            "run",
            "--rm",
            "--pull",
            "never",
            "--entrypoint",
            "vibe",
            &image,
            "--version",
        ],
        DOCKER_WAIT,
    );
    assert!(
        on_path.ok() && first(&on_path.stdout) == first(&want),
        "expected `vibe` on the image's path\n{}",
        on_path.describe(&[])
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_images_default_command_is_serve() {
    let image = image();
    // Started with nothing: `vibe serve` with no option and no environment,
    // which ends as the program's own `vibe serve` ends so.
    let (code, _, stderr) = harness_vibe(&["serve"]);
    assert_ne!(
        code,
        Some(0),
        "the harness's `vibe serve` with nothing ended with 0"
    );
    let ran = docker(&["run", "--rm", "--pull", "never", &image], DOCKER_WAIT);
    assert_eq!(
        (ran.code, ran.stderr.trim()),
        (code, stderr.trim()),
        "expected `docker run <image>` to be `vibe serve`: to end as `vibe serve` with nothing ends\n{}",
        ran.describe(&[])
    );
}

/// The user an image's configuration names: `uid` or `uid:gid`.
fn uid_of(user: &str) -> Option<u32> {
    user.split(':').next().and_then(|u| u.parse().ok())
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_image_runs_as_a_numbered_user_that_is_not_root_and_has_no_shell() {
    let image = image();
    let inspected = inspect_image(&image);
    let user = inspected["Config"]["User"].as_str().unwrap_or_default();
    let uid = uid_of(user).unwrap_or_else(|| {
        panic!("expected the image's user to be given as a number (uid or uid:gid), got {user:?}")
    });
    assert_ne!(uid, 0, "expected a user that is not root, got {user:?}");
    for shell in ["/bin/sh", "sh", "/bin/bash", "/busybox"] {
        let ran = docker(
            &[
                "run",
                "--rm",
                "--pull",
                "never",
                "--entrypoint",
                shell,
                &image,
                "-c",
                "echo started",
            ],
            DOCKER_WAIT,
        );
        assert!(
            !ran.ok() && !ran.stdout.contains("started"),
            "expected no shell in the image: `{shell}` started\n{}",
            ran.describe(&[])
        );
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_images_labels_say_where_it_comes_from() {
    let image = image();
    let inspected = inspect_image(&image);
    let labels = &inspected["Config"]["Labels"];
    let label = |name: &str| labels[name].as_str().unwrap_or_default().to_owned();
    let source = label("org.opencontainers.image.source");
    assert!(
        source.starts_with("https://"),
        "expected org.opencontainers.image.source to be the repository's address, got {source:?} (labels {labels})"
    );
    let (_, version, _) = harness_vibe(&["--version"]);
    // `vibe --version` says `vibe <version>` on its first line.
    let number = version
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    assert!(
        !number.is_empty(),
        "the harness's `vibe --version` printed {version:?}"
    );
    assert_eq!(
        label("org.opencontainers.image.version"),
        number,
        "expected org.opencontainers.image.version to be the version `vibe --version` says (labels {labels})"
    );
    assert!(
        !label("org.opencontainers.image.licenses").trim().is_empty(),
        "expected org.opencontainers.image.licenses (labels {labels})"
    );
}

/// The paths of the entries of a tar file (`docker export`), without a
/// leading `./` or `/`.
fn tar_paths(file: &Path) -> Vec<String> {
    let mut archive = tar::Archive::new(std::fs::File::open(file).unwrap());
    archive
        .entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            e.path()
                .unwrap()
                .to_string_lossy()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_owned()
        })
        .collect()
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_image_holds_no_compiler_no_package_manager_no_shell_and_no_source() {
    let image = image();
    let created = Created::of(&image);
    let folder = Folder::new();
    let exported = folder.0.join("image.tar");
    docker_ok(
        &[
            "export",
            "--output",
            exported.to_str().unwrap(),
            &created.name,
        ],
        EXPORT_WAIT,
    );
    let paths = tar_paths(&exported);
    assert!(
        paths.iter().any(|p| p == LICENSES.trim_start_matches('/')),
        "expected {LICENSES} in the image"
    );
    let base_copyrights = paths
        .iter()
        .filter(|p| {
            p.starts_with("usr/share/doc/")
                && p.ends_with("/copyright")
                && p.matches('/').count() == 4
        })
        .count();
    assert!(
        base_copyrights >= 1,
        "expected the base image's own usr/share/doc/*/copyright files left in the image"
    );
    let file_name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let forbidden: BTreeSet<&str> = [
        // Shells.
        "sh", "bash", "dash", "ash", "zsh", "busybox", // Package managers.
        "apt", "apt-get", "dpkg", "apk", "rpm", "yum", "dnf", "microdnf", "pip", "npm",
        // Compilers and the Rust toolchain.
        "cargo", "rustc", "rustup", "cc", "gcc", "g++", "clang", "ld", "c++",
    ]
    .into_iter()
    .collect();
    let in_bins = |p: &str| {
        [
            "bin/",
            "sbin/",
            "usr/bin/",
            "usr/sbin/",
            "usr/local/bin/",
            "usr/local/sbin/",
        ]
        .iter()
        .any(|dir| p.starts_with(dir))
    };
    let found: Vec<&String> = paths
        .iter()
        .filter(|p| in_bins(p) && forbidden.contains(file_name(p).as_str()))
        .collect();
    assert!(
        found.is_empty(),
        "expected no shell, package manager or compiler in the image, found {found:?}"
    );
    let source: Vec<&String> = paths
        .iter()
        .filter(|p| {
            let name = file_name(p);
            name.ends_with(".rs")
                || name == "Cargo.toml"
                || name == "Cargo.lock"
                || p.contains(".cargo/registry")
        })
        .collect();
    assert!(
        source.is_empty(),
        "expected no source in the image, found {:?}",
        source.iter().take(20).collect::<Vec<_>>()
    );
}

/// The Rust target the image is built for, from what `docker image
/// inspect` says of its platform (Linux on amd64 or arm64).
fn target_of(inspected: &serde_json::Value) -> String {
    let os = inspected["Os"].as_str().unwrap_or_default();
    let architecture = inspected["Architecture"].as_str().unwrap_or_default();
    assert_eq!(os, "linux", "expected an image for Linux");
    match architecture {
        "amd64" => "x86_64-unknown-linux-gnu".to_owned(),
        "arm64" => "aarch64-unknown-linux-gnu".to_owned(),
        other => panic!("the harness knows no Rust target for the architecture {other:?}"),
    }
}

/// `cargo <args>` in the repository; its standard output. Fails the test
/// unless it ends with `0`.
fn cargo(args: &[&str]) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(repository_root()).args(args);
    let finished = run_to_exit(command, Duration::from_secs(300)).unwrap();
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "expected `cargo {}` to end with 0\nstandard error:\n{}",
        args.join(" "),
        finished.stderr
    );
    finished.stdout
}

/// What one crate's block of the notices says: its licence line and the
/// text after it.
#[derive(Debug, Default)]
struct Notice {
    license: Option<String>,
    text: String,
}

/// One entry of the notices' second part: code built into the program
/// that is not a crate's own (CONTRACT.md: `## <name> <version> (built in
/// through <crate>)`, `License: ...`, `Taken from: ...`, the texts).
#[derive(Debug, Default)]
struct BuiltIn {
    name: String,
    header: String,
    license: Option<String>,
    taken_from: Option<String>,
    text: String,
}

/// The notices file read into its two parts. A header line is of the
/// second part when it ends with `(built in through <crate>)`, as the
/// contract's form has it; every other `## ` header is a crate's, `## <name>
/// <version>`. Fails on a crate's header of another form, a block whose
/// next line is not `License: ...`, or a built-in entry whose line after
/// that is not `Taken from: ...`.
fn notices(text: &str) -> (BTreeMap<(String, String), Notice>, Vec<BuiltIn>) {
    enum Current {
        None,
        Crate((String, String)),
        BuiltIn(usize),
    }
    let mut crates: BTreeMap<(String, String), Notice> = BTreeMap::new();
    let mut built_in: Vec<BuiltIn> = Vec::new();
    let mut current = Current::None;
    // The lines still expected after a header: `License: `, then for a
    // built-in entry `Taken from: `.
    let mut expect: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(at) = rest.find(" (built in through ") {
                let words: Vec<&str> = rest
                    .get(..at)
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect();
                assert!(
                    words.len() >= 2 && rest.trim_end().ends_with(')'),
                    "expected `## <name> <version> (built in through <crate>)`, got {line:?}"
                );
                built_in.push(BuiltIn {
                    name: words[..words.len() - 1].join(" "),
                    header: line.to_owned(),
                    ..BuiltIn::default()
                });
                current = Current::BuiltIn(built_in.len() - 1);
                expect = vec!["License: ", "Taken from: "];
                continue;
            }
            assert!(
                built_in.is_empty(),
                "expected every crate before the part of code built in through crates, got {line:?} after it"
            );
            let parts: Vec<&str> = rest.split_whitespace().collect();
            assert_eq!(
                parts.len(),
                2,
                "expected `## <name> <version>`, got {line:?}"
            );
            let key = (
                parts[0].to_owned(),
                parts[1].trim_start_matches('v').to_owned(),
            );
            assert!(!crates.contains_key(&key), "{line:?} is in the file twice");
            crates.insert(key.clone(), Notice::default());
            current = Current::Crate(key);
            expect = vec!["License: "];
            continue;
        }
        match &current {
            Current::None => {}
            Current::Crate(key) => {
                let notice = crates.get_mut(key).unwrap();
                if let Some(prefix) = (!expect.is_empty()).then(|| expect.remove(0)) {
                    let license = line.strip_prefix(prefix).unwrap_or_else(|| {
                        panic!(
                            "expected `License: <statement>` on the line after `## {} {}`, got {line:?}",
                            key.0, key.1
                        )
                    });
                    notice.license = Some(license.trim().to_owned());
                } else {
                    notice.text.push_str(line);
                    notice.text.push('\n');
                }
            }
            Current::BuiltIn(at) => {
                let entry = &mut built_in[*at];
                if let Some(prefix) = (!expect.is_empty()).then(|| expect.remove(0)) {
                    let value = line
                        .strip_prefix(prefix)
                        .unwrap_or_else(|| {
                            panic!(
                                "expected `{prefix}...` after {:?}, got {line:?}",
                                entry.header
                            )
                        })
                        .trim()
                        .to_owned();
                    if prefix == "License: " {
                        entry.license = Some(value);
                    } else {
                        entry.taken_from = Some(value);
                    }
                } else {
                    entry.text.push_str(line);
                    entry.text.push('\n');
                }
            }
        }
    }
    (crates, built_in)
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_images_third_party_licenses_name_every_crate_built_into_the_program_with_its_licence() {
    let image = image();
    let target = target_of(&inspect_image(&image));
    let created = Created::of(&image);
    let folder = Folder::new();
    let copied = folder.0.join("THIRD-PARTY-LICENSES");
    let ran = created.copy_out(LICENSES, &copied);
    assert!(
        ran.ok(),
        "expected {LICENSES} in the image\n{}",
        ran.describe(&[])
    );
    let text = std::fs::read_to_string(&copied)
        .unwrap_or_else(|e| panic!("expected {LICENSES} to be plain text: {e}"));
    let (listed, _) = notices(&text);

    let tree = cargo(&[
        "tree",
        "-p",
        "vibe",
        "--edges",
        "normal,build",
        "--prefix",
        "none",
        "--locked",
        "--target",
        &target,
    ]);
    let metadata: serde_json::Value = serde_json::from_str(&cargo(&[
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
        "--locked",
    ]))
    .unwrap();
    let own: BTreeSet<String> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        own.contains("vibe"),
        "cargo metadata named no `vibe`: {own:?}"
    );
    let built: BTreeSet<(String, String)> = tree
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('['))
        .map(|l| {
            let mut parts = l.split_whitespace();
            let name = parts.next().unwrap().to_owned();
            let version = parts
                .next()
                .unwrap_or_default()
                .trim_start_matches('v')
                .to_owned();
            (name, version)
        })
        .filter(|(name, _)| !own.contains(name))
        .collect();
    assert!(built.len() > 50, "cargo tree listed only {built:?}");

    let missing: Vec<String> = built
        .iter()
        .filter(|key| !listed.contains_key(*key))
        .map(|(n, v)| format!("{n} {v}"))
        .collect();
    assert!(
        missing.is_empty(),
        "expected every crate built into the program in {LICENSES} with its version; missing {} of {}: {missing:?}",
        missing.len(),
        built.len()
    );
    let ours: Vec<&(String, String)> = listed.keys().filter(|(n, _)| own.contains(n)).collect();
    assert!(
        ours.is_empty(),
        "expected the workspace's own crates not in it, found {ours:?}"
    );
    let unstated: Vec<&(String, String)> = listed
        .iter()
        .filter(|(_, n)| n.license.as_deref().is_none_or(str::is_empty))
        .map(|(k, _)| k)
        .collect();
    assert!(
        unstated.is_empty(),
        "expected each crate's licence stated, not for {unstated:?}"
    );
    // A crate whose source carries its licence's text has it in its block:
    // serde carries the MIT text.
    if let Some((key, notice)) = listed.iter().find(|((n, _), _)| n == "serde") {
        assert!(
            notice.text.contains("Permission is hereby granted"),
            "expected the licence texts serde's source carries in its block ({key:?})"
        );
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_images_third_party_licenses_have_a_part_for_the_code_built_in_v8_and_its_libraries_libpg_query_and_postgresql()
 {
    let image = image();
    let created = Created::of(&image);
    let folder = Folder::new();
    let copied = folder.0.join("THIRD-PARTY-LICENSES");
    let ran = created.copy_out(LICENSES, &copied);
    assert!(
        ran.ok(),
        "expected {LICENSES} in the image\n{}",
        ran.describe(&[])
    );
    let text = std::fs::read_to_string(&copied).unwrap();
    let (_, built_in) = notices(&text);
    let headers: Vec<&str> = built_in.iter().map(|b| b.header.as_str()).collect();
    // Every entry of the second part, whatever its name, has the form.
    for entry in &built_in {
        assert!(
            entry
                .license
                .as_deref()
                .is_some_and(|l| !l.trim().is_empty()),
            "expected a licence (`License: <SPDX>`) for {:?}",
            entry.header
        );
        assert!(
            entry
                .taken_from
                .as_deref()
                .is_some_and(|t| t.contains("https://")),
            "expected `Taken from:` with an https:// address for {:?}, got {:?}",
            entry.header,
            entry.taken_from
        );
        assert!(
            !entry.text.trim().is_empty(),
            "expected the texts of {:?} not to be empty",
            entry.header
        );
    }
    // The named ones: each of the alternatives is a way of writing the name;
    // the words, where given, are in its texts.
    for (names, words) in [
        (
            &["V8"][..],
            Some("Redistribution and use in source and binary forms"),
        ),
        (
            &["libpg_query"][..],
            Some("Redistribution and use in source and binary forms"),
        ),
        (
            &["PostgreSQL"][..],
            Some("PostgreSQL Global Development Group"),
        ),
        (&["ICU"][..], None),
        (&["libc++"][..], None),
        (&["abseil", "abseil-cpp"][..], None),
    ] {
        let name = names.first().unwrap();
        let entry = built_in
            .iter()
            .find(|b| names.iter().any(|n| b.name.eq_ignore_ascii_case(n)))
            .unwrap_or_else(|| {
                panic!(
                    "expected an entry for {name} in the part of code built in through crates (`## {name} <version> (built in through <crate>)`); its headers are {headers:?}"
                )
            });
        if let Some(words) = words {
            assert!(
                entry.text.contains(words),
                "expected the texts of {:?} to hold {words:?}",
                entry.header
            );
        }
    }
}

/// Where the image holds the program's own licence (CONTRACT.md, "The
/// first release: The licence").
const OWN_LICENSE: &str = "/usr/share/doc/vibe/LICENSE";

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_image_holds_the_programs_licence_exactly_when_the_repository_has_one() {
    let image = image();
    let created = Created::of(&image);
    let folder = Folder::new();
    let copied = folder.0.join("LICENSE");
    let ran = created.copy_out(OWN_LICENSE, &copied);
    match std::fs::read(repository_root().join("LICENSE")) {
        Ok(licence) => {
            assert!(
                ran.ok(),
                "expected {OWN_LICENSE} in the image, since the repository has a LICENSE\n{}",
                ran.describe(&[])
            );
            assert_eq!(
                std::fs::read(&copied).unwrap(),
                licence,
                "expected the image's {OWN_LICENSE} to be the repository's LICENSE, unchanged"
            );
        }
        Err(_) => assert!(
            !ran.ok(),
            "expected no {OWN_LICENSE} in the image, since the repository has no LICENSE"
        ),
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_images_notices_are_what_third_party_licenses_sh_writes_for_its_target() {
    let image = image();
    let target = target_of(&inspect_image(&image));
    let created = Created::of(&image);
    let folder = Folder::new();
    let copied = folder.0.join("THIRD-PARTY-LICENSES");
    let ran = created.copy_out(LICENSES, &copied);
    assert!(
        ran.ok(),
        "expected {LICENSES} in the image\n{}",
        ran.describe(&[])
    );
    let in_image = std::fs::read_to_string(&copied).unwrap();
    let written = blackbox::release::notices_text(&["--target", &target]);
    assert!(
        in_image == written,
        "expected the image's {LICENSES} to be what `scripts/third-party-licenses.sh --target {target}` writes (CONTRACT.md, \"The first release: The scripts that make it\")"
    );
}
