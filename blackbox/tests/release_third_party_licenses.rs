//! `scripts/third-party-licenses.sh [--target <triple>]` (CONTRACT.md, "The
//! first release: The scripts that make it"; the form is "Installing: The
//! image"): it writes the notices to standard output for the given target,
//! or the machine's without one: every crate `cargo tree` lists as built
//! into `vibe` for that target, with its version and its licence, but the
//! workspace's own; then the part for the code built in that is no crate's
//! own. Run in the repository as it is, with no build.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;

use blackbox::release::{Notices, built_crates, host_target, notices_text, own_crates};

/// The entries the second part must have, each by the ways its name may
/// be written, and words its texts hold where the contract's sources make
/// them certain.
const BUILT_IN: [(&[&str], Option<&str>); 6] = [
    (
        &["V8"],
        Some("Redistribution and use in source and binary forms"),
    ),
    (&["ICU"], None),
    (&["libc++"], None),
    (&["abseil", "abseil-cpp"], None),
    (
        &["libpg_query"],
        Some("Redistribution and use in source and binary forms"),
    ),
    (&["PostgreSQL"], Some("PostgreSQL Global Development Group")),
];

/// Reads the notices, failing the test with what is wrong with their form.
fn read(text: &str, what: &str) -> Notices {
    Notices::read(text).unwrap_or_else(|problem| panic!("{what}: {problem}"))
}

/// Fails unless the crates of `notices` are exactly those built into the
/// program for `target`, each with a licence stated.
fn assert_crates_of(notices: &Notices, target: &str) {
    let built = built_crates(target);
    let listed: BTreeSet<(String, String)> = notices.crates.keys().cloned().collect();
    let missing: Vec<String> = built
        .difference(&listed)
        .map(|(n, v)| format!("{n} {v}"))
        .collect();
    assert!(
        missing.is_empty(),
        "expected every crate `cargo tree -p vibe --edges normal,build --target {target}` lists in the notices for {target}; missing {} of {}: {missing:?}",
        missing.len(),
        built.len()
    );
    let own = own_crates();
    let ours: Vec<&(String, String)> = listed.iter().filter(|(n, _)| own.contains(n)).collect();
    assert!(
        ours.is_empty(),
        "expected the workspace's own crates not in the notices, found {ours:?}"
    );
    let extra: Vec<String> = listed
        .difference(&built)
        .map(|(n, v)| format!("{n} {v}"))
        .collect();
    assert!(
        extra.is_empty(),
        "expected only the crates built into the program for {target} in its notices; these are not built for it: {extra:?}"
    );
    let unstated: Vec<&(String, String)> = notices
        .crates
        .iter()
        .filter(|(_, n)| n.license.is_empty())
        .map(|(k, _)| k)
        .collect();
    assert!(
        unstated.is_empty(),
        "expected each crate's licence stated (`License: <its manifest's statement>`), not for {unstated:?}"
    );
    // A crate whose source carries its licence's text has it in its block:
    // serde carries the MIT text.
    let serde = notices
        .crates
        .iter()
        .find(|((n, _), _)| n == "serde")
        .unwrap_or_else(|| {
            panic!("expected serde, which the program is built with, in the notices")
        });
    assert!(
        serde.1.text.contains("Permission is hereby granted"),
        "expected the licence texts serde's source carries in its block ({:?})",
        serde.0
    );
}

/// Fails unless the second part has each entry of [`BUILT_IN`] and the
/// Rust standard library, every entry in its form.
fn assert_built_in(notices: &Notices, target: &str) {
    let headers: Vec<&str> = notices.built_in.iter().map(|b| b.header.as_str()).collect();
    for entry in &notices.built_in {
        assert!(
            !entry.license.is_empty(),
            "expected a licence (`License: <SPDX name>`) for {:?} ({target})",
            entry.header
        );
        assert!(
            entry.taken_from.contains("https://"),
            "expected `Taken from: <the address and the tag or commit>` for {:?} ({target}), got {:?}",
            entry.header,
            entry.taken_from
        );
        assert!(
            !entry.text.trim().is_empty(),
            "expected the texts of {:?} ({target}) not to be empty",
            entry.header
        );
    }
    for (names, words) in BUILT_IN {
        let name = names[0];
        let entry = notices.built_in_named(names).unwrap_or_else(|| {
            panic!(
                "expected an entry for {name} in the second part of the notices for {target} (`## {name} <version> (built in through <crate>)`); its headers are {headers:?}"
            )
        });
        if let Some(words) = words {
            assert!(
                entry.text.contains(words),
                "expected the texts of {:?} ({target}) to hold {words:?}",
                entry.header
            );
        }
    }
    assert!(
        notices.built_in.iter().any(|b| b.through == "rustc"),
        "expected the Rust standard library in the second part, built in through rustc (`## <name> <version> (built in through rustc)`), for {target}; its headers are {headers:?}"
    );
}

#[test]
fn the_notices_for_linux_name_every_crate_built_into_the_program_with_its_licence() {
    let target = "x86_64-unknown-linux-gnu";
    let text = notices_text(&["--target", target]);
    let notices = read(&text, "the notices for x86_64-unknown-linux-gnu");
    assert_crates_of(&notices, target);
}

#[test]
fn the_notices_second_part_names_v8_its_libraries_libpg_query_postgresql_and_the_standard_library()
{
    let target = "x86_64-unknown-linux-gnu";
    let text = notices_text(&["--target", target]);
    let notices = read(&text, "the notices for x86_64-unknown-linux-gnu");
    assert_built_in(&notices, target);
}

#[test]
fn without_a_target_the_notices_are_those_of_the_machines_own() {
    let host = host_target();
    let without = notices_text(&[]);
    let with = notices_text(&["--target", &host]);
    assert!(
        without == with,
        "expected `third-party-licenses.sh` without --target to write what `--target {host}` (this machine's) writes"
    );
}

#[test]
fn the_notices_for_macos_on_linux_differ_from_linuxs_only_where_the_crates_differ() {
    let linux_target = "x86_64-unknown-linux-gnu";
    let mac_target = "aarch64-apple-darwin";
    let linux = read(
        &notices_text(&["--target", linux_target]),
        "the notices for x86_64-unknown-linux-gnu",
    );
    let mac = read(
        &notices_text(&["--target", mac_target]),
        "the notices for aarch64-apple-darwin (made on Linux)",
    );
    assert_crates_of(&mac, mac_target);
    assert_built_in(&mac, mac_target);
    let mut differing = Vec::new();
    for (key, notice) in &mac.crates {
        if let Some(other) = linux.crates.get(key)
            && other != notice
        {
            differing.push(format!("{} {}", key.0, key.1));
        }
    }
    assert!(
        differing.is_empty(),
        "expected a crate built for both targets to have the same block in both notices; these differ: {differing:?}"
    );
    let linux_only: BTreeSet<&(String, String)> = linux
        .crates
        .keys()
        .filter(|k| !mac.crates.contains_key(*k))
        .collect();
    let mac_only: BTreeSet<&(String, String)> = mac
        .crates
        .keys()
        .filter(|k| !linux.crates.contains_key(*k))
        .collect();
    let linux_built = built_crates(linux_target);
    let mac_built = built_crates(mac_target);
    assert_eq!(
        linux_only,
        linux_built.difference(&mac_built).collect::<BTreeSet<_>>(),
        "expected the crates only in Linux's notices to be those only built for Linux"
    );
    assert_eq!(
        mac_only,
        mac_built.difference(&linux_built).collect::<BTreeSet<_>>(),
        "expected the crates only in macOS's notices to be those only built for macOS"
    );
}

/// The harness's own check of [`Notices::read`].
#[test]
fn the_notices_reader_takes_the_contracts_form_and_refuses_another() {
    let good = "Third-party licences\n\n## serde 1.0.0\nLicense: MIT OR Apache-2.0\nPermission is hereby granted\n## 1. A heading of a licence's own\nmore text\n\n## V8 14.0 (built in through v8)\nLicense: BSD-3-Clause\nTaken from: https://chromium.googlesource.com/v8/v8 at 14.0\nRedistribution\n";
    let notices = Notices::read(good).unwrap();
    assert_eq!(notices.crates.len(), 1);
    let serde = &notices.crates[&("serde".to_owned(), "1.0.0".to_owned())];
    assert!(
        serde
            .text
            .contains("## 1. A heading of a licence's own\nmore text")
    );
    assert_eq!(notices.built_in.len(), 1);
    assert_eq!(notices.built_in[0].name, "V8");
    assert_eq!(notices.built_in[0].through, "v8");
    for bad in [
        "## serde\nLicense: MIT\n",
        "## serde 1.0 extra\nLicense: MIT\n",
        "## V8 14.0 (built in through v8)\nLicense: BSD\nfrom somewhere\n",
        "## V8 14.0 (built in through v8)\nLicense: BSD\nTaken from: https://x\n## serde 1.0.0\nLicense: MIT\n",
        "## V8 14.0 (built in through v8)\nLicense: BSD\n",
    ] {
        assert!(Notices::read(bad).is_err(), "expected {bad:?} refused");
    }
    // A `## ` line not followed by `License: ` is text, not a crate.
    assert!(
        Notices::read("## serde 1.0.0\nMIT\n")
            .unwrap()
            .crates
            .is_empty()
    );
}
