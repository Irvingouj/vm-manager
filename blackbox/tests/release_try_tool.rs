//! `scripts/try-tool.sh --archive <file>`'s refusals (CONTRACT.md, "The
//! first release: The scripts that make it"): an archive that is not there,
//! one that holds no `vibe-<v>-<target>/vibe`, and one whose `vibe` does not
//! say the package's version are refused (an exit code other than 0, saying
//! why) before anything is started, and nothing of the script's own is left
//! behind.
//!
//! "Before anything is started" is held, from outside, to: the run ends
//! within [`REFUSAL_TIMEOUT`]; an `npm` of the test's own, first on the
//! path, is never called; the archive's `vibe` (a stand-in of the test's
//! that records how it is called) is asked for nothing but its version, so
//! no `vibe new` and no `vibe request`; and `HOME`, `XDG_CONFIG_HOME` and
//! `XDG_DATA_HOME`, folders of the test's own, stay empty (no download, no
//! credential). "Nothing left behind" is held to: the test's `TMPDIR` is
//! empty afterwards, `/tmp` holds no new entry but the harness's own
//! (`vm-blackbox-...`), and the archive's folder holds the archive alone.
//!
//! The whole run (a download of Postgres, `npm install` from the registry)
//! is not in the gate: the release workflow runs it on each system.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use blackbox::program::write_program;
use blackbox::release::{Folder, Ran, names_in, package_version, repository_root, run, script};

/// How long a refusal may take: nothing is downloaded or installed first.
const REFUSAL_TIMEOUT: Duration = Duration::from_secs(30);

const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The folders one run is given, all of the test's own.
struct Machine {
    folder: Folder,
}

impl Machine {
    fn new() -> Self {
        let machine = Self {
            folder: Folder::new("try"),
        };
        for dir in [
            "tmp", "home", "config", "data", "bin", "archives", "made", "calls",
        ] {
            fs::create_dir_all(machine.dir(dir)).unwrap();
        }
        // An npm that records that it was called, and fails.
        let calls = machine.dir("calls").join("npm");
        write_program(
            &machine.dir("bin").join("npm"),
            format!(
                "#!/bin/sh\necho \"npm $*\" >> '{}'\nexit 1\n",
                calls.display()
            )
            .as_bytes(),
        )
        .unwrap();
        machine
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.folder.path().join(name)
    }

    /// A program standing for `vibe` that records each call's arguments
    /// and says `first_line` first.
    fn stand_in(&self, first_line: &str) -> PathBuf {
        let path = self.dir("made").join("vibe");
        let calls = self.dir("calls").join("vibe");
        write_program(
            &path,
            format!(
                "#!/bin/sh\necho \"vibe $*\" >> '{}'\nprintf '%s\\n' '{first_line}'\n",
                calls.display()
            )
            .as_bytes(),
        )
        .unwrap();
        path
    }

    /// Packs `files` (path inside the archive, file on this machine) by
    /// hand with the system's `tar` into `archives/<name>`.
    fn pack(&self, name: &str, files: &[(&str, &Path)]) -> PathBuf {
        let staging = self.dir("staging");
        for (inside, from) in files {
            let to = staging.join(inside);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(from, &to).unwrap();
        }
        let tops: BTreeSet<String> = files
            .iter()
            .map(|(inside, _)| inside.split('/').next().unwrap().to_owned())
            .collect();
        let archive = self.dir("archives").join(name);
        let mut command = Command::new("tar");
        command.arg("-czf").arg(&archive).arg("-C").arg(&staging);
        command.args(&tops);
        run(command, REFUSAL_TIMEOUT).expect_ok();
        fs::remove_dir_all(&staging).unwrap();
        archive
    }

    /// `scripts/try-tool.sh --archive <archive>` with this machine's
    /// folders, ended at [`REFUSAL_TIMEOUT`].
    fn try_tool(&self, archive: &Path) -> Ran {
        let path = script(&repository_root(), "try-tool.sh");
        let mut command = Command::new(&path);
        command
            .arg("--archive")
            .arg(archive)
            .current_dir(repository_root());
        let search = format!(
            "{}:{}",
            self.dir("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .env("PATH", search)
            .env("TMPDIR", self.dir("tmp"))
            .env("HOME", self.dir("home"))
            .env("XDG_CONFIG_HOME", self.dir("config"))
            .env("XDG_DATA_HOME", self.dir("data"));
        let mut ran = run(command, REFUSAL_TIMEOUT);
        ran.command = format!("scripts/try-tool.sh --archive {}", archive.display());
        ran
    }

    fn calls(&self, program: &str) -> Vec<String> {
        fs::read_to_string(self.dir("calls").join(program))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The entries of `/tmp` but the harness's own.
fn tmp_entries() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for dir in [PathBuf::from("/tmp"), std::env::temp_dir()] {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with("vm-blackbox-") {
                    found.insert(format!("{}/{name}", dir.display()));
                }
            }
        }
    }
    found
}

/// Runs the script on `archive` and fails unless it refused, saying why
/// (each of `words` in what it said), before anything was started, leaving
/// nothing behind.
fn assert_refused(machine: &Machine, archive: &Path, words: &[&str]) {
    // One run at a time in this test binary: a run of another test, in
    // the middle of its own, would show in /tmp as this one's.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = tmp_entries();
    let ran = machine.try_tool(archive);
    let after = tmp_entries();
    assert!(
        ran.code.is_some_and(|c| c != 0),
        "expected try-tool.sh to refuse (an exit code other than 0) within {REFUSAL_TIMEOUT:?}\n{}",
        ran.describe()
    );
    assert!(
        !ran.said().trim().is_empty(),
        "expected try-tool.sh to say why it refused\n{}",
        ran.describe()
    );
    for word in words {
        assert!(
            ran.said().contains(word),
            "expected the refusal to name {word:?}\n{}",
            ran.describe()
        );
    }
    assert_eq!(
        machine.calls("npm"),
        Vec::<String>::new(),
        "expected nothing started before the refusal: npm was called\n{}",
        ran.describe()
    );
    let vibe_calls: Vec<String> = machine
        .calls("vibe")
        .into_iter()
        .filter(|c| c.trim() != "vibe --version")
        .collect();
    assert!(
        vibe_calls.is_empty(),
        "expected the archive's vibe asked for its version only before the refusal, it was called as {vibe_calls:?}\n{}",
        ran.describe()
    );
    for dir in ["home", "config", "data", "tmp"] {
        let left = names_in(&machine.dir(dir));
        assert!(
            left.is_empty(),
            "expected nothing made in the test's {dir} folder by a refused run, found {left:?}\n{}",
            ran.describe()
        );
    }
    let new: Vec<&String> = after.difference(&before).collect();
    assert!(
        new.is_empty(),
        "expected a refused run to leave nothing of its own in /tmp, found {new:?}\n{}",
        ran.describe()
    );
    if let Some(parent) = archive.parent().filter(|p| p.is_dir()) {
        let beside: Vec<String> = names_in(parent);
        let expected: Vec<String> = archive
            .file_name()
            .filter(|_| archive.exists())
            .map(|n| n.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        assert_eq!(
            beside,
            expected,
            "expected nothing unpacked beside the archive\n{}",
            ran.describe()
        );
    }
}

#[test]
fn an_archive_that_is_not_there_is_refused_before_anything_is_started() {
    let machine = Machine::new();
    let version = package_version();
    let missing = machine
        .dir("archives")
        .join(format!("vibe-{version}-{TARGET}.tar.gz"));
    assert_refused(
        &machine,
        &missing,
        &[&format!("vibe-{version}-{TARGET}.tar.gz")],
    );
}

/// The archive's name and its one folder's, for the package's version.
fn names() -> (String, String) {
    let folder = format!("vibe-{}-{TARGET}", package_version());
    let name = format!("{folder}.tar.gz");
    (folder, name)
}

#[test]
fn an_archive_whose_folder_holds_no_vibe_is_refused_before_anything_is_started() {
    let (folder, name) = names();
    let machine = Machine::new();
    let notices = machine.dir("made").join("THIRD-PARTY-LICENSES");
    fs::write(&notices, "## serde 1.0.0\nLicense: MIT\n").unwrap();
    let archive = machine.pack(
        &name,
        &[(&format!("{folder}/THIRD-PARTY-LICENSES"), &notices)],
    );
    assert_refused(&machine, &archive, &[]);
}

#[test]
fn an_archive_holding_vibe_outside_its_folder_is_refused_before_anything_is_started() {
    let (_, name) = names();
    let machine = Machine::new();
    let program = machine.stand_in(&format!("vibe {}", package_version()));
    let archive = machine.pack(&name, &[("vibe", &program)]);
    assert_refused(&machine, &archive, &[]);
}

#[test]
fn an_archive_holding_vibe_in_a_folder_of_another_name_is_refused_before_anything_is_started() {
    let (_, name) = names();
    let machine = Machine::new();
    let program = machine.stand_in(&format!("vibe {}", package_version()));
    let archive = machine.pack(&name, &[("vibe-other/vibe", &program)]);
    assert_refused(&machine, &archive, &[]);
}

#[test]
fn a_file_that_is_not_an_archive_is_refused_before_anything_is_started() {
    let (_, name) = names();
    let machine = Machine::new();
    let archive = machine.dir("archives").join(&name);
    fs::write(&archive, "not a gzip'd tar\n").unwrap();
    assert_refused(&machine, &archive, &[]);
}

#[test]
fn an_archive_whose_vibe_says_another_version_is_refused_before_anything_is_started() {
    let version = package_version();
    for said in ["vibe 9.9.9", "something else"] {
        let machine = Machine::new();
        let program = machine.stand_in(said);
        let folder = format!("vibe-{version}-{TARGET}");
        let archive = machine.pack(
            &format!("{folder}.tar.gz"),
            &[(&format!("{folder}/vibe"), &program)],
        );
        let words: &[&str] = if said == "vibe 9.9.9" {
            &["9.9.9"]
        } else {
            &[]
        };
        assert_refused(&machine, &archive, words);
    }
}
