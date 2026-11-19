//! `scripts/gate.sh` looks for the browser first (CONTRACT.md, "A real
//! browser in the gate: The browser": "`scripts/gate.sh` looks for it
//! first, with its other checks before anything is built, and stops there
//! with the same words, so that a gate without a browser ends in
//! seconds").
//!
//! The gate is run as the release tests run it: the script of a throwaway
//! clone of the repository at its `HEAD` (`Clone::of_repository`), from the
//! clone's root, with `cargo` and `npm` stand-ins first on the `PATH` that
//! record every call (`GateStandIns`), so that no real gate runs inside the
//! gate. What the browser is, is the case's: no `firefox` on the `PATH`
//! (the `PATH` is the test's own, every program of the test's `PATH` but
//! one named `firefox...`), `VIBE_TEST_BROWSER` naming a path that is not
//! there, or a pretender of the test's own that answers `--version` as an
//! old Firefox, as Chromium, or as Firefox 128.
//!
//! The words are the harness's own (`firefox::NO_BROWSER`, "looked for",
//! `firefox::WHAT_TO_DO`), so that the gate's and the browser tests' cannot
//! drift apart.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use blackbox::firefox::{ENV, NO_BROWSER, WHAT_TO_DO};
use blackbox::release::{Clone, GateStandIns, Ran, run, script};

/// "A gate without a browser ends in seconds."
const ENDS_WITHIN: Duration = Duration::from_secs(30);

/// How long the gate may run at all here (its steps are stand-ins).
const GATE_TIMEOUT: Duration = Duration::from_secs(120);

/// What the browser is, in one case.
enum Browser {
    /// No `firefox` on the `PATH`, `VIBE_TEST_BROWSER` not set.
    NoneOnThePath,
    /// `VIBE_TEST_BROWSER` names a path that is not there.
    NamedButMissing,
    /// `VIBE_TEST_BROWSER` names a pretender that answers `--version` so.
    Named(&'static str),
    /// A pretender named `firefox` on the `PATH` answers `--version` so;
    /// `VIBE_TEST_BROWSER` not set.
    OnThePath(&'static str),
}

/// One run of the gate of a clone, and what its stand-ins recorded.
struct GateRun {
    ran: Ran,
    took: Duration,
    cargo: Vec<String>,
    npm: Vec<String>,
    /// What the output must say was looked for.
    looked_for: Vec<String>,
}

/// A folder of links to every program of `search` (a `PATH`) but those
/// named `firefox...`, the first of a name winning as on a `PATH`.
fn path_without_firefox(into: &Path, search: &str) {
    for dir in search.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(text) = name.to_str() else {
                continue;
            };
            if text.starts_with("firefox") {
                continue;
            }
            let link = into.join(&name);
            if link.symlink_metadata().is_ok() {
                continue;
            }
            drop(std::os::unix::fs::symlink(entry.path(), &link));
        }
    }
}

/// A program of the test's own that answers `--version` with `says` and
/// refuses anything else.
fn pretender(dir: &Path, name: &str, says: &str) -> PathBuf {
    let path = dir.join(name);
    let text = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' '{says}'; exit 0; fi\n\
         echo 'a pretender of the test: it answers --version only' >&2\nexit 1\n"
    );
    blackbox::program::write_program(&path, text.as_bytes())
        .unwrap_or_else(|problem| panic!("cannot write {}: {problem}", path.display()));
    path
}

fn gate_with(browser: &Browser) -> GateRun {
    let clone = Clone::of_repository();
    let stand_ins = GateStandIns::new(&[], &[]);
    let programs = clone.beside("programs");
    let pretenders = clone.beside("pretenders");
    let search = std::env::var("PATH").unwrap_or_default();
    path_without_firefox(&programs, &search);
    assert!(
        !programs.join("firefox").exists(),
        "the test's PATH must hold no firefox"
    );

    let mut path = vec![stand_ins.bin()];
    let mut named: Option<PathBuf> = None;
    let looked_for = match browser {
        Browser::NoneOnThePath => vec!["firefox".to_owned(), ENV.to_owned()],
        Browser::NamedButMissing => {
            let missing = pretenders.join("no-such-firefox");
            named = Some(missing.clone());
            vec![ENV.to_owned(), missing.display().to_string()]
        }
        Browser::Named(says) => {
            let program = pretender(&pretenders, "pretending-browser", says);
            named = Some(program.clone());
            vec![ENV.to_owned(), program.display().to_string()]
        }
        Browser::OnThePath(says) => {
            pretender(&pretenders, "firefox", says);
            path.push(pretenders);
            Vec::new()
        }
    };
    path.push(programs);
    let path = std::env::join_paths(&path).unwrap();

    let mut command = Command::new(script(clone.root(), "gate.sh"));
    command
        .current_dir(clone.root())
        .env("PATH", path)
        .env_remove("CARGO")
        .env_remove(ENV);
    if let Some(named) = &named {
        command.env(ENV, named);
    }
    let started = Instant::now();
    let mut ran = run(command, GATE_TIMEOUT);
    let took = started.elapsed();
    ran.command = format!(
        "scripts/gate.sh of a clone (cargo and npm stand-ins; {})",
        match &named {
            Some(named) => format!("{ENV}={}", named.display()),
            None => format!("{ENV} not set"),
        }
    );
    GateRun {
        ran,
        took,
        cargo: stand_ins.calls("cargo"),
        npm: stand_ins.calls("npm"),
        looked_for,
    }
}

/// The gate stopped at its look for the browser: non-zero, within
/// [`ENDS_WITHIN`], in the harness's words, and nothing built.
fn assert_stopped_for_no_browser(case: &str, gate: &GateRun) {
    let said = gate.ran.said();
    assert!(
        gate.ran.code.is_some_and(|c| c != 0),
        "{case}: expected the gate to end non-zero, with no browser\n{}",
        gate.ran.describe()
    );
    assert!(
        gate.took < ENDS_WITHIN,
        "{case}: expected a gate without a browser to end within {ENDS_WITHIN:?}, it took {:?}",
        gate.took
    );
    for words in [NO_BROWSER, "looked for", WHAT_TO_DO]
        .into_iter()
        .chain(gate.looked_for.iter().map(String::as_str))
    {
        assert!(
            said.contains(words),
            "{case}: expected the gate's output to hold {words:?} (the words of the browser tests' own failure)\n{}",
            gate.ran.describe()
        );
    }
    assert!(
        gate.cargo.is_empty() && gate.npm.is_empty(),
        "{case}: expected nothing built or run before the look for the browser: cargo was called {:?}, npm {:?}\n{}",
        gate.cargo,
        gate.npm,
        gate.ran.describe()
    );
}

#[test]
fn the_gate_with_no_firefox_on_the_path_and_no_vibe_test_browser_stops_before_anything_is_built() {
    let gate = gate_with(&Browser::NoneOnThePath);
    assert_stopped_for_no_browser("no firefox on the PATH", &gate);
}

#[test]
fn the_gate_with_vibe_test_browser_naming_a_missing_path_stops_before_anything_is_built() {
    let gate = gate_with(&Browser::NamedButMissing);
    assert_stopped_for_no_browser("VIBE_TEST_BROWSER names no program", &gate);
}

#[test]
fn the_gate_with_a_firefox_older_than_128_stops_before_anything_is_built() {
    let gate = gate_with(&Browser::Named("Mozilla Firefox 127.0.2"));
    assert_stopped_for_no_browser("Mozilla Firefox 127.0.2", &gate);
}

#[test]
fn the_gate_with_a_browser_that_is_not_firefox_stops_before_anything_is_built() {
    let gate = gate_with(&Browser::Named("Chromium 140.0"));
    assert_stopped_for_no_browser("Chromium 140.0", &gate);
}

#[test]
fn the_gate_with_firefox_128_goes_on_to_its_steps_in_their_order() {
    let gate = gate_with(&Browser::OnThePath("Mozilla Firefox 128.0"));
    let said = gate.ran.said();
    assert!(
        !said.contains(NO_BROWSER),
        "expected the gate to take Firefox 128\n{}",
        gate.ran.describe()
    );
    assert!(
        gate.ran.ok(),
        "expected the gate to pass with every step a stand-in that passes\n{}",
        gate.ran.describe()
    );
    // The order the gate promises (AGENTS.md, "The gate"): that blackbox
    // depends on no product crate, fmt, clippy, npm ci and npm test, cargo
    // test.
    let cargo_steps = [
        "tree --workspace",
        "tree -p blackbox",
        "fmt --all --check",
        "clippy --workspace",
        "test --workspace",
    ];
    assert_eq!(
        gate.cargo.len(),
        cargo_steps.len(),
        "expected cargo called once for each of its steps, {cargo_steps:?}; it was called {:?}",
        gate.cargo
    );
    for (call, step) in gate.cargo.iter().zip(cargo_steps) {
        assert!(
            call.starts_with(step),
            "expected cargo's calls in the gate's order {cargo_steps:?}; they were {:?}",
            gate.cargo
        );
    }
    let npm_steps = ["ci", "test"];
    assert_eq!(
        gate.npm.len(),
        npm_steps.len(),
        "expected npm called for `npm ci` then `npm test`; it was called {:?}",
        gate.npm
    );
    for (call, step) in gate.npm.iter().zip(npm_steps) {
        assert!(
            call.starts_with(step),
            "expected npm's calls `npm ci` then `npm test`; they were {:?}",
            gate.npm
        );
    }
}
