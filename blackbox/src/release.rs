//! What the tests of the first release share (CONTRACT.md, "The first
//! release"): the repository's root and the files of it that are the
//! release's own, the package's version as cargo reports it, the scripts
//! of `scripts/` run by their documented interface with a time limit, a
//! throwaway clone of the repository for the cases that need a tag or a
//! `LICENSE` (never the real repository, never pushed anywhere), the
//! notices file read into its two parts, and a tool archive read entry by
//! entry.
//!
//! Nothing here builds anything: the scripts' `cargo` calls are pointed at
//! the repository's own `target/` (`CARGO_TARGET_DIR`), so a clone makes no
//! second one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::program::run_to_exit;
use crate::shared_database::token;

pub use crate::sdk::repository_root;

/// The version CONTRACT.md gives the release ("One version").
pub const VERSION: &str = "0.1.1";

/// The two targets a release's tool is built for.
pub const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"];

/// The image's name without its tag.
pub const IMAGE: &str = "ghcr.io/irvingouj/vibe";

/// How long one script may take: `third-party-licenses.sh` reads the
/// licence files of every crate, `package-tool.sh` packs a debug program of
/// some 160 MB. One that takes longer is taken to hang and fails the test.
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long one `cargo metadata`, `cargo tree`, `git` or `tar` may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// What a command run to its end left: its exit code (`None` when it was
/// killed at its time limit), what it printed, and the command as text.
#[derive(Debug)]
pub struct Ran {
    pub command: String,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// Both streams, for a test that looks for what the command said
    /// wherever it said it.
    pub fn said(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    pub fn describe(&self) -> String {
        let ended = match self.code {
            Some(code) => format!("exit code {code}"),
            None => "no exit code: killed at its time limit, or ended by a signal".to_owned(),
        };
        format!(
            "$ {}\n({ended})\nstandard output:\n{}\nstandard error:\n{}",
            self.command,
            shorten(&self.stdout),
            shorten(&self.stderr)
        )
    }

    /// Fails the test unless the command ended with `0`.
    pub fn expect_ok(&self) -> &Self {
        assert!(
            self.ok(),
            "expected this to end with 0\n{}",
            self.describe()
        );
        self
    }
}

/// At most the first and last 4000 characters of a long output.
fn shorten(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 8000 {
        return text.to_owned();
    }
    let head: String = chars[..4000].iter().collect();
    let tail: String = chars[chars.len() - 4000..].iter().collect();
    format!(
        "{head}\n[... {} characters ...]\n{tail}",
        chars.len() - 8000
    )
}

/// Runs `command` to its end, at most `timeout`.
pub fn run(mut command: Command, timeout: Duration) -> Ran {
    let shown = format!("{command:?}");
    // Every cargo a command starts uses the repository's own `target/`.
    command.env("CARGO_TARGET_DIR", repository_root().join("target"));
    match run_to_exit(command, timeout) {
        Ok(finished) => Ran {
            command: shown,
            code: finished.status.and_then(|s| s.code()),
            stdout: finished.stdout,
            stderr: finished.stderr,
        },
        Err(problem) => Ran {
            command: shown,
            code: None,
            stdout: String::new(),
            stderr: problem,
        },
    }
}

/// `scripts/<name>` of the checkout at `root`, failing the test at once,
/// saying the script is not there, when it is not.
pub fn script(root: &Path, name: &str) -> PathBuf {
    let path = root.join("scripts").join(name);
    assert!(
        path.is_file(),
        "scripts/{name} is not there ({}): CONTRACT.md, \"The first release: The scripts that make it\"",
        path.display()
    );
    path
}

/// Runs `scripts/<name> <args>` of the checkout at `root`, from `root`,
/// with these variables added, at most [`SCRIPT_TIMEOUT`].
pub fn run_script_with(root: &Path, name: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let path = script(root, name);
    let mut command = Command::new(&path);
    command.args(args).current_dir(root);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut ran = run(command, SCRIPT_TIMEOUT);
    ran.command = format!("scripts/{name} {}", args.join(" "));
    ran
}

/// Runs `scripts/<name> <args>` of the real repository.
pub fn run_script(name: &str, args: &[&str]) -> Ran {
    run_script_with(&repository_root(), name, args, &[])
}

/// `cargo <args>` in `dir`, which must end with `0`; its standard output.
pub fn cargo_in(dir: &Path, args: &[&str]) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(dir).args(args);
    let ran = run(command, COMMAND_TIMEOUT);
    ran.expect_ok();
    ran.stdout
}

/// `cargo metadata --no-deps` of the workspace at `dir`.
fn metadata(dir: &Path) -> serde_json::Value {
    let text = cargo_in(
        dir,
        &["metadata", "--no-deps", "--format-version", "1", "--locked"],
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON from `cargo metadata`: {e}"))
}

/// The version of the package `vibe` in the workspace at `dir`, as cargo
/// reports it.
pub fn package_version_in(dir: &Path) -> String {
    let metadata = metadata(dir);
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == "vibe")
        .and_then(|p| p["version"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("cargo metadata names no package `vibe`"))
}

/// The version of the package `vibe` in the repository.
pub fn package_version() -> String {
    package_version_in(&repository_root())
}

/// The names of the workspace's own crates.
pub fn own_crates() -> BTreeSet<String> {
    let metadata = metadata(&repository_root());
    let own: BTreeSet<String> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect();
    assert!(
        own.contains("vibe"),
        "cargo metadata named no `vibe`: {own:?}"
    );
    own
}

/// Every crate (name, version) `cargo tree -p vibe --edges normal,build
/// --prefix none --target <target>` lists, but the workspace's own.
pub fn built_crates(target: &str) -> BTreeSet<(String, String)> {
    let own = own_crates();
    let tree = cargo_in(
        &repository_root(),
        &[
            "tree",
            "-p",
            "vibe",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--locked",
            "--target",
            target,
        ],
    );
    let built: BTreeSet<(String, String)> = tree
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('['))
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let name = parts.next()?.to_owned();
            let version = parts.next()?.trim_start_matches('v').to_owned();
            Some((name, version))
        })
        .filter(|(name, _)| !own.contains(name))
        .collect();
    assert!(
        built.len() > 50,
        "cargo tree listed only {built:?} for {target}"
    );
    built
}

/// The machine's own Rust target (`rustc -vV`'s `host:`), with the
/// repository's toolchain.
pub fn host_target() -> String {
    let mut command = Command::new("rustc");
    command.arg("-vV").current_dir(repository_root());
    let ran = run(command, COMMAND_TIMEOUT);
    ran.expect_ok();
    ran.stdout
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_owned())
        .unwrap_or_else(|| panic!("rustc -vV named no host\n{}", ran.describe()))
}

/// The text of a file of the repository, by its path from the root;
/// failing the test, naming it, when it is not there.
pub fn read_repository_file(relative: &str) -> String {
    let path = repository_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "expected {relative} in the repository ({}): {e}",
            path.display()
        )
    })
}

/// A folder of the test's own under the system's temporary folder,
/// removed when dropped.
pub struct Folder(PathBuf);

impl Folder {
    pub fn new(what: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vm-blackbox-release-{what}-{}", token()));
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The names of what the folder holds, sorted.
    pub fn names(&self) -> Vec<String> {
        names_in(&self.0)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.0));
    }
}

/// The names of what `dir` holds, sorted.
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A program of the test's own standing in for `vibe`: a shell script
/// that prints `first_line` and ends with `0`, whatever it is asked.
pub fn stand_in_program(dir: &Path, name: &str, first_line: &str) -> PathBuf {
    let path = dir.join(name);
    let quoted = first_line.replace('\'', "'\\''");
    let text = format!("#!/bin/sh\nprintf '%s\\n' '{quoted}'\nprintf 'a line after it\\n'\n");
    crate::program::write_program(&path, text.as_bytes())
        .unwrap_or_else(|problem| panic!("cannot write {}: {problem}", path.display()));
    path
}

/// A throwaway copy of the repository's history at the commit that is
/// checked out (its `HEAD`), in a folder of the test's own, removed when
/// dropped (CONTRACT.md, "The first release": the gate's release tests hold
/// for the commit checked out, whether or not a branch `main` names it; the
/// workflow runs the gate on a tag's commit, a detached `HEAD` with no
/// local branch). The copy is made as a checkout on a runner is: `git
/// init`, a remote `origin` naming the repository, `git fetch` of `HEAD`
/// alone and no tag (a tag of the repository, `v<version>` in a tag's run,
/// is not carried over); then a branch `main` and `origin/main` are made in
/// the copy at that commit. Commits and tags are made in it only; nothing
/// is pushed. No change of the real working tree is in it, and no git
/// configuration of the user's or the system's is read.
pub struct Clone {
    folder: Folder,
    root: PathBuf,
}

impl Clone {
    pub fn of_repository() -> Self {
        let folder = Folder::new("clone");
        let root = folder.path().join("repository");
        fs::create_dir_all(&root).unwrap_or_else(|e| panic!("cannot make {}: {e}", root.display()));
        let clone = Self { folder, root };
        clone.git(&["init", "--quiet"]);
        clone.git(&["config", "user.name", "Release test"]);
        clone.git(&["config", "user.email", "release-test@example.invalid"]);
        clone.git(&["config", "commit.gpgsign", "false"]);
        clone.git(&["config", "tag.gpgsign", "false"]);
        let source = repository_root();
        clone.git(&["remote", "add", "origin", source.to_str().unwrap()]);
        clone.git(&["fetch", "--quiet", "--no-tags", "origin", "HEAD"]);
        clone.git(&["checkout", "--quiet", "-B", "main", "FETCH_HEAD"]);
        clone.origin_main_at_head();
        clone
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A folder beside the clone for the test's own files.
    pub fn beside(&self, name: &str) -> PathBuf {
        let dir = self.folder.path().join(name);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
        dir
    }

    /// `git <args>` in the clone, which must end with `0`; its output.
    pub fn git(&self, args: &[&str]) -> String {
        let mut command = Command::new("git");
        command.args(args).current_dir(&self.root);
        isolate_git(&mut command);
        let ran = run(command, COMMAND_TIMEOUT);
        ran.expect_ok();
        ran.stdout
    }

    pub fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    }

    pub fn read(&self, relative: &str) -> String {
        let path = self.root.join(relative);
        fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("expected {relative} in the clone: {e}"))
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.root.join(relative).exists()
    }

    pub fn remove(&self, relative: &str) {
        let path = self.root.join(relative);
        fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
    }

    /// Commits everything of the working tree (an empty commit when there
    /// is nothing); the new commit's id.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.head()
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Makes `origin/main` the clone's `HEAD`, as if it had been pushed and
    /// fetched back (nothing leaves the clone).
    pub fn origin_main_at_head(&self) {
        self.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }

    /// An annotated tag `name` on `commit`.
    pub fn tag(&self, name: &str, commit: &str) {
        self.git(&["tag", "-a", "-m", name, name, commit]);
    }

    /// Makes the clone hold a `LICENSE` of the test's own, committed;
    /// `origin/main` follows. Its bytes.
    pub fn with_licence(&self) -> Vec<u8> {
        let text = format!(
            "The licence of a throwaway clone of the release tests ({}).\nNot a real licence.\n",
            token()
        );
        self.write("LICENSE", text.as_bytes());
        self.commit("the release tests: a LICENSE of their own");
        self.origin_main_at_head();
        text.into_bytes()
    }

    /// Makes the clone hold no `LICENSE`, committed; `origin/main` follows.
    pub fn without_licence(&self) {
        if self.exists("LICENSE") {
            self.remove("LICENSE");
            self.commit("the release tests: no LICENSE");
            self.origin_main_at_head();
        }
    }

    /// Runs `scripts/<name> <args>` of the clone, from it.
    pub fn run_script(&self, name: &str, args: &[&str]) -> Ran {
        run_script_with(&self.root, name, args, &[])
    }
}

/// Keeps the user's and the system's git configuration (signing, hooks,
/// templates) out of the harness's own git commands.
fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
}

/// What one crate's block of the notices says.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Notice {
    pub license: String,
    /// The whole block, its header line included.
    pub block: String,
    /// The texts after the `License:` line.
    pub text: String,
}

/// One entry of the notices' second part (`## <name> <version> (built in
/// through <crate>)`, `License: ...`, `Taken from: ...`, the texts).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuiltIn {
    pub name: String,
    pub through: String,
    pub header: String,
    pub license: String,
    pub taken_from: String,
    pub text: String,
}

/// The notices file read into its two parts.
#[derive(Debug, Default)]
pub struct Notices {
    pub crates: BTreeMap<(String, String), Notice>,
    pub built_in: Vec<BuiltIn>,
}

impl Notices {
    /// Reads `text` in the form CONTRACT.md gives ("Installing: The
    /// image"): every crate first, `## <name> <version>` then `License:
    /// <statement>` then its texts; then the entries of code built in,
    /// `## <name> <version> (built in through <crate>)`, `License: <SPDX>`,
    /// `Taken from: <where>`, the texts. Lines before the first header are
    /// a preamble, left alone. Says what is wrong when the text is not of
    /// that form.
    ///
    /// A licence's text may hold a line beginning `## ` of its own (a
    /// Markdown heading: webpki-roots' CDLA text carries `## 1. Provision of the
    /// Data`), so a line beginning `## ` is a header only when the line
    /// after it begins `License: `; any other is text.
    pub fn read(text: &str) -> Result<Self, String> {
        let mut notices = Self::default();
        let mut current: Option<Result<(String, String), usize>> = None;
        let mut expect: Vec<&str> = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().copied().enumerate() {
            let header = line.strip_prefix("## ").filter(|_| {
                lines
                    .get(at + 1)
                    .is_some_and(|n| n.starts_with("License: "))
            });
            if let Some(rest) = header {
                if !expect.is_empty() {
                    return Err(format!(
                        "expected `{}...` before the next header {line:?}",
                        expect.join("` and `")
                    ));
                }
                if let Some(at) = rest.find(" (built in through ") {
                    let (head, tail) = rest.split_at(at);
                    let words: Vec<&str> = head.split_whitespace().collect();
                    let through = tail
                        .strip_prefix(" (built in through ")
                        .unwrap_or_default()
                        .trim_end();
                    let Some(through) = through.strip_suffix(')') else {
                        return Err(format!(
                            "expected `## <name> <version> (built in through <crate>)`, got {line:?}"
                        ));
                    };
                    if words.len() < 2 || through.trim().is_empty() {
                        return Err(format!(
                            "expected `## <name> <version> (built in through <crate>)`, got {line:?}"
                        ));
                    }
                    notices.built_in.push(BuiltIn {
                        name: words[..words.len() - 1].join(" "),
                        through: through.trim().to_owned(),
                        header: line.to_owned(),
                        ..BuiltIn::default()
                    });
                    current = Some(Err(notices.built_in.len() - 1));
                    expect = vec!["License: ", "Taken from: "];
                    continue;
                }
                if !notices.built_in.is_empty() {
                    return Err(format!(
                        "expected every crate before the part of code built in, got {line:?} after it"
                    ));
                }
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err(format!("expected `## <name> <version>`, got {line:?}"));
                }
                let key = (
                    parts[0].to_owned(),
                    parts[1].trim_start_matches('v').to_owned(),
                );
                if notices.crates.contains_key(&key) {
                    return Err(format!("{line:?} is in the notices twice"));
                }
                notices.crates.insert(
                    key.clone(),
                    Notice {
                        block: format!("{line}\n"),
                        ..Notice::default()
                    },
                );
                current = Some(Ok(key));
                expect = vec!["License: "];
                continue;
            }
            match &current {
                None => {}
                Some(Ok(key)) => {
                    let notice = notices.crates.get_mut(key).unwrap();
                    notice.block.push_str(line);
                    notice.block.push('\n');
                    if expect.is_empty() {
                        notice.text.push_str(line);
                        notice.text.push('\n');
                    } else {
                        let prefix = expect.remove(0);
                        let Some(value) = line.strip_prefix(prefix) else {
                            return Err(format!(
                                "expected `License: <statement>` on the line after `## {} {}`, got {line:?}",
                                key.0, key.1
                            ));
                        };
                        notice.license = value.trim().to_owned();
                    }
                }
                Some(Err(at)) => {
                    let entry = &mut notices.built_in[*at];
                    if expect.is_empty() {
                        entry.text.push_str(line);
                        entry.text.push('\n');
                    } else {
                        let prefix = expect.remove(0);
                        let Some(value) = line.strip_prefix(prefix) else {
                            return Err(format!(
                                "expected `{prefix}...` after {:?}, got {line:?}",
                                entry.header
                            ));
                        };
                        if prefix == "License: " {
                            entry.license = value.trim().to_owned();
                        } else {
                            entry.taken_from = value.trim().to_owned();
                        }
                    }
                }
            }
        }
        if !expect.is_empty() {
            return Err(format!(
                "expected `{}...` after the last header",
                expect.join("` and `")
            ));
        }
        Ok(notices)
    }

    /// The entry of the second part whose name is one of `names` (any
    /// case).
    pub fn built_in_named(&self, names: &[&str]) -> Option<&BuiltIn> {
        self.built_in
            .iter()
            .find(|b| names.iter().any(|n| b.name.eq_ignore_ascii_case(n)))
    }
}

/// `scripts/third-party-licenses.sh <args>` of the repository, which must
/// end with `0`; what it wrote to standard output.
pub fn notices_text(args: &[&str]) -> String {
    let ran = run_script("third-party-licenses.sh", args);
    ran.expect_ok();
    assert!(
        !ran.stdout.trim().is_empty(),
        "expected the notices on standard output\n{}",
        ran.describe()
    );
    ran.stdout
}

/// One entry of a tar archive.
#[derive(Debug)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub mode: u32,
    /// The bytes of a regular file of at most [`KEPT_ENTRY_BYTES`].
    pub bytes: Option<Vec<u8>>,
    pub size: u64,
}

/// What an entry of an archive is.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EntryKind {
    File,
    Folder,
    Other,
}

/// The largest file of an archive whose bytes are kept in memory (the
/// notices and a licence; not the program).
pub const KEPT_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Reads `path` as a gzip'd tar, every entry in its order. Says what is
/// wrong when it is not one.
pub fn read_archive(path: &Path) -> Result<Vec<Entry>, String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut entries = Vec::new();
    for entry in archive
        .entries()
        .map_err(|e| format!("{} is not a gzip'd tar: {e}", path.display()))?
    {
        let mut entry =
            entry.map_err(|e| format!("{} is not a gzip'd tar: {e}", path.display()))?;
        let header = entry.header();
        let kind = match header.entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::File,
            tar::EntryType::Directory => EntryKind::Folder,
            _ => EntryKind::Other,
        };
        let mode = header.mode().unwrap_or(0);
        let size = header.size().unwrap_or(0);
        let path_text = entry
            .path()
            .map_err(|e| format!("an entry of {} has no path: {e}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let bytes = if kind == EntryKind::File && size <= KEPT_ENTRY_BYTES {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("cannot read {path_text} of {}: {e}", path.display()))?;
            Some(bytes)
        } else {
            None
        };
        entries.push(Entry {
            path: path_text,
            kind,
            mode,
            bytes,
            size,
        });
    }
    Ok(entries)
}

/// Unpacks the gzip'd tar `archive` into `into` with the system's `tar`,
/// as a person would.
pub fn unpack(archive: &Path, into: &Path) {
    let mut command = Command::new("tar");
    command.arg("-xzf").arg(archive).arg("-C").arg(into);
    run(command, COMMAND_TIMEOUT).expect_ok();
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a stand-in for `cargo` or `npm` does when its arguments begin
/// with `prefix`: prints `stderr` to its standard error and ends with
/// `code`. The first rule whose prefix matches decides; with none, it
/// prints nothing and ends with `0`.
#[derive(Debug, Clone)]
pub struct Rule {
    pub prefix: String,
    pub code: i32,
    pub stderr: String,
}

impl Rule {
    pub fn new(prefix: &str, code: i32, stderr: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            code,
            stderr: stderr.to_owned(),
        }
    }
}

/// The seam of the tests of `scripts/gate.sh` and `scripts/gate-hosted.sh`:
/// the scripts of the repository run as they are, from its root, with a
/// `PATH` whose `cargo` and `npm` come first and are stand-ins of the
/// test's own (and no `CARGO` in the environment), so that no real gate
/// runs inside the gate. Each stand-in records every call (its arguments,
/// one line) and answers as the case's [`Rule`]s say.
pub struct GateStandIns {
    folder: Folder,
}

impl GateStandIns {
    pub fn new(cargo: &[Rule], npm: &[Rule]) -> Self {
        let stand_ins = Self {
            folder: Folder::new("gate"),
        };
        fs::create_dir_all(stand_ins.folder.path().join("bin")).unwrap();
        stand_ins.write_stand_in("cargo", cargo);
        stand_ins.write_stand_in("npm", npm);
        stand_ins
    }

    fn write_stand_in(&self, program: &str, rules: &[Rule]) {
        let calls = self.folder.path().join(format!("{program}.calls"));
        let mut text = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n",
            calls.display()
        );
        for (at, rule) in rules.iter().enumerate() {
            assert!(
                !rule.prefix.contains(['"', '\'', '\\', '*', '?', '[']),
                "the harness writes no quote or glob in a rule's prefix"
            );
            let said = self.folder.path().join(format!("{program}.{at}.stderr"));
            fs::write(&said, &rule.stderr).unwrap();
            text.push_str(&format!(
                "  \"{}\"*) cat '{}' >&2; exit {} ;;\n",
                rule.prefix,
                said.display(),
                rule.code
            ));
        }
        text.push_str("  *) exit 0 ;;\nesac\n");
        crate::program::write_program(
            &self.folder.path().join("bin").join(program),
            text.as_bytes(),
        )
        .unwrap_or_else(|problem| panic!("cannot write the stand-in {program}: {problem}"));
    }

    /// The folder holding the stand-ins, to put first on a `PATH`.
    pub fn bin(&self) -> PathBuf {
        self.folder.path().join("bin")
    }

    /// Every call of the stand-in `program` (`cargo` or `npm`), its
    /// arguments as one line, in order.
    pub fn calls(&self, program: &str) -> Vec<String> {
        fs::read_to_string(self.folder.path().join(format!("{program}.calls")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Runs `scripts/<name> <args>` of the repository with the stand-ins
    /// first on the path, at most `timeout`.
    pub fn run(&self, name: &str, args: &[&str], timeout: Duration) -> Ran {
        let root = repository_root();
        let path = script(&root, name);
        let mut command = Command::new(&path);
        command.args(args).current_dir(&root);
        let search = format!(
            "{}:{}",
            self.bin().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command.env("PATH", search).env_remove("CARGO");
        let mut ran = run(command, timeout);
        ran.command = format!(
            "scripts/{name} {} (cargo and npm stand-ins)",
            args.join(" ")
        );
        ran
    }
}
