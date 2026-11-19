//! What the commands that run the project's own tools share (`vm check`,
//! `vm generate`, `vm deploy`; blackbox/CONTRACT.md, "Checking a project",
//! "Generating migrations", "Deploying"): the command's own folder in
//! `TMPDIR` ([`Scratch`]), its end and its signals ([`Signals`]), `node`
//! and the project's packages as Node resolves them, and where and how a
//! tool (`node`, `git`) is run and its words quoted ([`Cx`]).
//!
//! **Endings.** A command's whole run is held to its time, and `SIGINT`,
//! `SIGTERM` and `SIGHUP` end it: their handlers are in place (with
//! [`Signals::listen`]) before the first tool starts. Either way the run's
//! future is dropped inside [`Signals::within`], which stops every tool's
//! process group ([`child`]); the caller removes its own folder after.
//!
//! **Bounds.** Of each tool's pipe [`TEXT_BYTES`] is kept unless the caller
//! keeps less or more (the rest is read and dropped); a `package.json` is
//! read to [`PACKAGE_JSON_BYTES`]; a tool's words in a message are its first
//! [`WORDS_LINES`] lines and [`WORDS_CHARS`] characters, without its stack
//! and without the own folder's path.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::future::Future;
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind, signal};

use super::Ending;
use super::child::{self, Kept, Sink};
use super::theirs::Theirs;

/// A command's time without `--timeout`.
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The oldest Node the project's tools are run with.
const NODE_MAJOR: u32 = 22;

/// What is kept of a tool's standard output or error.
pub(super) const TEXT_BYTES: usize = 64 * 1024;

/// What is read of a package's `package.json`.
const PACKAGE_JSON_BYTES: u64 = 1024 * 1024;

/// The lines of a tool's own words put in a message.
const WORDS_LINES: usize = 12;

/// The characters of a tool's own words put in a message.
const WORDS_CHARS: usize = 2000;

/// The command that runs the project's tools, as its messages name it.
#[derive(Clone, Copy)]
pub(super) struct Who {
    /// As the Builder runs it: `vm check`.
    pub command: &'static str,
    /// As a message's subject: `the check`.
    pub the: &'static str,
    /// The beginning of the name of its own folder in `TMPDIR`.
    pub folder: &'static str,
}

/// How a run held to its time and its signals ended.
pub(super) enum Bounded<T> {
    Ended(T),
    TimedOut,
    /// By this signal.
    Interrupted(&'static str),
}

/// The handlers of `SIGINT`, `SIGTERM` and `SIGHUP`: from when they are
/// made, these end the run (in [`Signals::within`]) rather than the
/// program, so that what it started is stopped.
pub(super) struct Signals {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

impl Signals {
    pub(super) fn listen(who: &Who) -> Result<Self, Ending> {
        Ok(Self {
            interrupt: listen(SignalKind::interrupt(), who)?,
            terminate: listen(SignalKind::terminate(), who)?,
            hangup: listen(SignalKind::hangup(), who)?,
        })
    }

    /// `body`, held to `limit` and ended by a signal. When a signal comes or
    /// the time is up, `body` is dropped: every tool's process group is
    /// stopped then.
    pub(super) async fn within<T>(
        &mut self,
        limit: Duration,
        body: impl Future<Output = T>,
    ) -> Bounded<T> {
        tokio::select! {
            ended = tokio::time::timeout(limit, body) => match ended {
                Ok(ended) => Bounded::Ended(ended),
                Err(_) => Bounded::TimedOut,
            },
            _ = self.interrupt.recv() => Bounded::Interrupted("SIGINT"),
            _ = self.terminate.recv() => Bounded::Interrupted("SIGTERM"),
            _ = self.hangup.recv() => Bounded::Interrupted("SIGHUP"),
        }
    }
}

/// A handler of `kind`, which from now on ends the run rather than the
/// program.
fn listen(kind: SignalKind, who: &Who) -> Result<Signal, Ending> {
    signal(kind).map_err(|error| {
        Ending::new(
            1,
            "failed",
            format!(
                "{} cannot take its signals ({error}): run it again",
                who.command
            ),
        )
    })
}

pub(super) fn own_folder_failed(error: &io::Error, who: &Who) -> Ending {
    let command = who.command;
    Ending::new(
        1,
        "failed",
        format!(
            "{command} cannot make or use its own folder in the system's temporary folder ({error}): make room there, or set TMPDIR to a folder you can write, then run {command} again"
        ),
    )
}

/// A command's own folder: private (mode `0700`), in the system's
/// temporary folder (`TMPDIR`), removed when this is dropped.
pub(super) struct Scratch {
    pub path: PathBuf,
}

impl Scratch {
    pub(super) fn make(who: &Who) -> io::Result<Self> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("{}-{hex}", who.folder));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

/// Where a command runs the project's tools: the project, the command's
/// own folder (every tool's `TMPDIR`; the copy of `migrations/` is in it),
/// and how a tool's words are cleaned of it.
pub(super) struct Cx<'a> {
    pub project: &'a Path,
    pub own: &'a Path,
    /// What the own folder is written as in a tool's words, and what it is
    /// written as instead, longest first.
    masks: Vec<(String, String)>,
    /// The own folder's name: a line of a tool's words that still holds it
    /// is left out.
    own_name: String,
}

/// What the copy of `migrations/` is named as in a tool's words.
const MIGRATIONS: &str = "migrations";

impl<'a> Cx<'a> {
    pub(super) fn new(project: &'a Path, own: &'a Path, who: &Who) -> Self {
        let canonical = fs::canonicalize(own).unwrap_or_else(|_| own.to_path_buf());
        let relative = from_to(project, &canonical);
        let text = |path: &Path| path.to_string_lossy().into_owned();
        // What a tool's words say instead of a path of the own folder.
        let own_folder = format!("({}'s own folder)", who.the);
        let mut masks = Vec::new();
        for base in [text(&canonical), text(own), text(&relative)] {
            masks.push((format!("./{base}/migrations"), MIGRATIONS.to_owned()));
            masks.push((format!("{base}/migrations"), MIGRATIONS.to_owned()));
            masks.push((format!("./{base}"), own_folder.clone()));
            masks.push((base, own_folder.clone()));
        }
        masks.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        Self {
            project,
            own,
            masks,
            own_name: own
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    /// The copy of `migrations/`.
    pub(super) fn copy(&self) -> PathBuf {
        self.own.join("migrations")
    }

    /// The first lines of a tool's own words: standard error, then standard
    /// output; without the empty lines, the lines of a stack trace (`at
    /// ...`), and the own folder (a file of the copy of `migrations/` named
    /// as the project's).
    pub(super) fn words(&self, err: &str, out: &str) -> String {
        let mut lines = Vec::new();
        for line in err.lines().chain(out.lines()) {
            if lines.len() >= WORDS_LINES {
                break;
            }
            let line = line.trim();
            if line.is_empty() || line.starts_with("at ") {
                continue;
            }
            let line = self
                .masks
                .iter()
                .fold(line.to_owned(), |line, (from, to)| line.replace(from, to));
            if !self.own_name.is_empty() && line.contains(&self.own_name) {
                continue;
            }
            lines.push(line);
        }
        let joined = lines.join(" / ");
        if joined.chars().count() > WORDS_CHARS {
            let cut: String = joined.chars().take(WORDS_CHARS).collect();
            format!("{cut} ...")
        } else {
            joined
        }
    }

    /// Whether a tool's words name a file of the copy of `migrations/`.
    pub(super) fn names_the_copy(&self, text: &str) -> bool {
        self.masks
            .iter()
            .any(|(from, to)| to == MIGRATIONS && text.contains(from.as_str()))
    }

    /// Runs `node <args>` in the project with the own folder as `TMPDIR`.
    pub(super) async fn run<O: Sink, E: Sink>(
        &self,
        args: &[&OsStr],
        out: O,
        err: E,
    ) -> io::Result<child::Ran<O, E>> {
        child::run("node", self.project, self.own, args, out, err).await
    }

    /// Runs `git <args>` in the project with the own folder as `TMPDIR`, as
    /// a tool of the project's is run (its own process group, its pipes
    /// bounded by `out` and `err`, inside the command's time).
    pub(super) async fn git<O: Sink, E: Sink>(
        &self,
        args: &[&OsStr],
        out: O,
        err: E,
    ) -> io::Result<child::Ran<O, E>> {
        child::run("git", self.project, self.own, args, out, err).await
    }
}

/// The path of `to` relative to `from`, both absolute: up to the root,
/// then down.
pub(super) fn from_to(from: &Path, to: &Path) -> PathBuf {
    let mut path = PathBuf::new();
    for component in from.components() {
        if matches!(component, Component::Normal(_)) {
            path.push("..");
        }
    }
    for component in to.components() {
        if let Component::Normal(name) = component {
            path.push(name);
        }
    }
    path
}

pub(super) fn tool_failed(tool: &str, what: &str, who: &Who) -> Ending {
    Ending::new(
        1,
        "tool-failed",
        format!(
            "{tool} {what}: check that the project's packages are installed whole (run npm install), then run {} again",
            who.command
        ),
    )
}

/// How a tool ended, in words.
pub(super) fn ended(status: std::process::ExitStatus) -> String {
    status.code().map_or_else(
        || "was stopped by a signal".to_owned(),
        |code| format!("ended with {code}"),
    )
}

/// That `node` is on the `PATH` and is 22 or later.
pub(super) async fn node_is_there(cx: &Cx<'_>, who: &Who) -> Result<(), Ending> {
    let command = who.command;
    let missing = |found: &str| {
        Ending::new(
            1,
            "node-missing",
            format!(
                "{command} runs the project's tools with Node {NODE_MAJOR} or later, and {found}: install Node {NODE_MAJOR} or later and put its node on the PATH, then run {command} again"
            ),
        )
    };
    let ran = match cx
        .run(
            &[OsStr::new("--version")],
            Kept::up_to(TEXT_BYTES),
            Kept::up_to(TEXT_BYTES),
        )
        .await
    {
        Ok(ran) => ran,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(missing("there is no node on the PATH"));
        }
        Err(error) => return Err(missing(&format!("node cannot be started ({error})"))),
    };
    let printed = ran.out.text();
    let major = printed
        .trim()
        .strip_prefix('v')
        .and_then(|v| v.split('.').next())
        .and_then(|m| m.parse::<u32>().ok());
    match major {
        Some(major) if ran.status.success() && major >= NODE_MAJOR => Ok(()),
        Some(major) => Err(missing(&format!("the node on the PATH is Node {major}"))),
        None => Err(missing(&format!(
            "the node on the PATH does not give its version ({})",
            Theirs::from(printed.trim())
        ))),
    }
}

/// The folder of the package `name`, as Node resolves it from `project`:
/// `node_modules/<name>` of the project or of the nearest folder above it
/// that has it.
fn package_dir(project: &Path, name: &str) -> Option<PathBuf> {
    project
        .ancestors()
        .map(|dir| dir.join("node_modules").join(name))
        .find(|dir| dir.join("package.json").is_file())
}

/// The program `bin` names in the `package.json` of `dir`: its `bin` when
/// that is one path, else its entry `command`.
fn bin_of(dir: &Path, command: &str) -> Option<PathBuf> {
    let mut text = String::new();
    File::open(dir.join("package.json"))
        .ok()?
        .take(PACKAGE_JSON_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let package: Value = serde_json::from_str(&text).ok()?;
    let bin = match package.get("bin")? {
        Value::String(path) => path.as_str(),
        Value::Object(bins) => bins.get(command)?.as_str()?,
        _ => return None,
    };
    let path = dir.join(bin);
    path.is_file().then_some(path)
}

fn not_installed(what: &str, who: &Who) -> Ending {
    Ending::new(
        1,
        "not-installed",
        format!(
            "the project's packages are not installed ({what} does not resolve from the project as Node resolves it): run npm install in the project, then run {} again",
            who.command
        ),
    )
}

/// The SDK's tooling (`@vm/sdk`'s `tooling/main.js`).
pub(super) fn sdk_tooling(project: &Path, who: &Who) -> Result<PathBuf, Ending> {
    package_dir(project, "@vm/sdk")
        .map(|dir| dir.join("tooling").join("main.js"))
        .filter(|main| main.is_file())
        .ok_or_else(|| not_installed("@vm/sdk", who))
}

/// `typescript`'s `tsc`.
pub(super) fn tsc(project: &Path, who: &Who) -> Result<PathBuf, Ending> {
    package_dir(project, "typescript")
        .and_then(|dir| bin_of(&dir, "tsc"))
        .ok_or_else(|| not_installed("typescript", who))
}

/// `drizzle-kit`'s program.
pub(super) fn drizzle_kit(project: &Path, who: &Who) -> Result<PathBuf, Ending> {
    package_dir(project, "drizzle-kit")
        .and_then(|dir| bin_of(&dir, "drizzle-kit"))
        .ok_or_else(|| not_installed("drizzle-kit", who))
}
