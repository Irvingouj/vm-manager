//! The tool (CONTRACT.md, "The tool"): the same `vibe` program run as a
//! command on a Builder's machine, here a temporary directory of the test's
//! own standing for that machine's home, so that nothing the tests do ever
//! touches the real home.
//!
//! - [`Machine`] runs `vibe <command>` with `HOME` and `XDG_CONFIG_HOME`
//!   pointed into its directory, an environment that holds nothing else of
//!   the test's (only `PATH`, and what the test adds on purpose), a time
//!   limit on every run, and keeps what the run printed and how it ended;
//! - [`Run`] checks that: an exit code, `--json`'s one object and a line
//!   end, an error's `reason` and `message`;
//! - the credential the tool keeps is found by reading the folder's files
//!   and asking the installation which run of text in them signs in
//!   ([`Machine::kept_credential`]): nothing about the file's name or form
//!   is promised, only that it is in that folder;
//! - [`windows_in`] and [`platform_tables_holding`] search what was printed
//!   and the platform database for a credential;
//! - [`NotAnInstallation`] is a listener that answers every request with
//!   something no installation would say.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::http::{Request, User};
use crate::postgres::Platform;
use crate::program::{run_to_exit, vm_binary};
use crate::shared_database::token;
use crate::sql::Sql;
use crate::vm::{DOMAIN, Vm};

/// The longest one run of the tool may take before it is killed. Above the
/// tool's own 30 s wait for an answer, so that a test sees the tool's own
/// ending rather than the harness's.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(50);

/// The shortest run of text in the credential file taken to be a
/// credential: 32 random bytes are at least 43 characters of base64.
const SHORTEST_CREDENTIAL: usize = 32;

/// The width of the windows of a credential searched for in what was
/// printed.
pub const WINDOW: usize = 16;

/// How a run's standard output or error is broken before the program
/// writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    /// Standard output (file descriptor 1) is closed.
    StdoutClosed,
    /// Standard output is a pipe whose reading end is already closed.
    StdoutReaderGone,
    /// Standard error (file descriptor 2) is closed; nothing of it is kept.
    StderrClosed,
    /// Both are closed.
    BothClosed,
}

impl Broken {
    /// The redirections `sh` makes before it runs the program.
    fn redirections(self) -> &'static str {
        match self {
            Self::StdoutClosed => " >&-",
            Self::StdoutReaderGone => "",
            Self::StderrClosed => " 2>&-",
            Self::BothClosed => " >&- 2>&-",
        }
    }

    fn keeps_stderr(self) -> bool {
        matches!(self, Self::StdoutClosed | Self::StdoutReaderGone)
    }
}

/// Where the Builder's configuration home is, as the tool is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigHome {
    /// `XDG_CONFIG_HOME` is an absolute path (`<machine>/config`).
    Absolute,
    /// `XDG_CONFIG_HOME` is not set.
    Unset,
    /// `XDG_CONFIG_HOME` holds this relative path.
    Relative(String),
}

/// A Builder's machine: a temporary directory with a home in it. Removed
/// when dropped.
pub struct Machine {
    root: PathBuf,
    home: PathBuf,
    config_home: ConfigHome,
    umask: Option<String>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// A machine whose `XDG_CONFIG_HOME` is an absolute path.
    pub fn new() -> Self {
        Self::with_config_home(ConfigHome::Absolute)
    }

    /// A machine with this configuration home. The home and (when absolute)
    /// `XDG_CONFIG_HOME` exist, as `$HOME/.config` does; the tool's own
    /// folder does not.
    pub fn with_config_home(config_home: ConfigHome) -> Self {
        let root = std::env::temp_dir().join(format!("vm-blackbox-tool-{}", token()));
        let home = root.join("home");
        for dir in [root.join("cwd"), home.join(".config"), root.join("config")] {
            fs::create_dir_all(&dir)
                .unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
        }
        Self {
            root,
            home,
            config_home,
            umask: None,
        }
    }

    /// Runs every command under this umask (`000`, say), through `sh`.
    #[must_use]
    pub fn umask(mut self, umask: &str) -> Self {
        self.umask = Some(umask.to_owned());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The directory the tool runs in.
    pub fn cwd(&self) -> PathBuf {
        self.root.join("cwd")
    }

    /// The absolute `XDG_CONFIG_HOME` of [`ConfigHome::Absolute`].
    pub fn absolute_config_home(&self) -> PathBuf {
        self.root.join("config")
    }

    /// The folder CONTRACT.md says the credential is kept in:
    /// `$XDG_CONFIG_HOME/vibe/` when that is absolute, else
    /// `$HOME/.config/vibe/`.
    pub fn credential_dir(&self) -> PathBuf {
        match self.config_home {
            ConfigHome::Absolute => self.absolute_config_home().join("vibe"),
            ConfigHome::Unset | ConfigHome::Relative(_) => self.home.join(".config").join("vibe"),
        }
    }

    /// Every regular file under [`Machine::credential_dir`], at any depth.
    pub fn credential_files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        collect_files(&self.credential_dir(), &mut found);
        found.sort();
        found
    }

    /// The one file under the credential folder (failing the test unless
    /// there is exactly one): where the tool keeps the credential.
    pub fn credential_file(&self) -> PathBuf {
        match self.credential_files().as_slice() {
            [one] => one.clone(),
            other => panic!(
                "expected exactly one file in the credential folder {} after signing in, found {other:?}",
                self.credential_dir().display()
            ),
        }
    }

    /// Every file anywhere on this machine (its home, its configuration
    /// home, the directory the tool runs in), at any depth.
    pub fn all_files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        collect_files(&self.root, &mut found);
        found.sort();
        found
    }

    /// Whether any file on this machine holds `text`.
    pub fn files_holding(&self, text: &str) -> Vec<PathBuf> {
        self.all_files()
            .into_iter()
            .filter(|f| {
                fs::read(f).is_ok_and(|bytes| {
                    bytes
                        .windows(text.len().max(1))
                        .any(|w| w == text.as_bytes())
                })
            })
            .collect()
    }

    /// The credential the tool keeps: of the runs of credential-like text
    /// in the files of the credential folder, the one with which the
    /// installation answers `GET /v1/me`. Fails the test if none does.
    pub fn kept_credential(&self, vm: &Vm) -> String {
        let mut candidates = Vec::new();
        for file in self.credential_files() {
            let bytes =
                fs::read(&file).unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
            candidates.extend(credential_like_runs(&bytes));
        }
        let signing_in: Vec<String> = candidates
            .into_iter()
            .filter(|c| bearer(vm, "/v1/me", c).send().status == 200)
            .collect();
        match signing_in.as_slice() {
            [one] => one.clone(),
            [] => panic!(
                "expected a credential with which the installation answers GET /v1/me among the runs of text in the files of {} ({} files); found none",
                self.credential_dir().display(),
                self.credential_files().len()
            ),
            many => panic!(
                "expected one credential in {}, found {} that sign in",
                self.credential_dir().display(),
                many.len()
            ),
        }
    }

    /// Runs `vibe <args>` on this machine.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with_env(args, &[])
    }

    /// Runs `vibe <args>` on this machine with these environment variables
    /// added.
    pub fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let command = self.command(args, env, None);
        let began = Instant::now();
        let finished = run_to_exit(command, TOOL_TIMEOUT)
            .unwrap_or_else(|problem| panic!("cannot run the tool: {problem}"));
        Run {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: finished.status.and_then(|s| s.code()),
            stdout: finished.stdout,
            stderr: finished.stderr,
            elapsed: began.elapsed(),
        }
    }

    /// Runs `vibe <args>` on this machine with these environment variables
    /// added, its standard output and error written to files rather than
    /// pipes, killed at `timeout`. For a run that may leave a process of
    /// its own behind: a pipe held open by such a process would keep a
    /// reader of it waiting, a file does not.
    pub fn run_into_files(&self, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> Run {
        self.run_into_files_in(&self.cwd(), args, env, timeout)
    }

    /// As [`Machine::run_into_files`], run in the directory `cwd`.
    pub fn run_into_files_in(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> Run {
        self.spawn_into_files(cwd, args, env).wait(timeout)
    }

    /// Starts `vibe <args>` on this machine in `cwd`, with these environment
    /// variables added and its output written to files, and returns at
    /// once: the test may signal it ([`Running::signal`]) and then waits
    /// for it ([`Running::wait`]). Killed if dropped while running.
    pub fn spawn_into_files(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Running {
        let outputs = std::env::temp_dir().join(format!("vm-blackbox-run-{}", token()));
        fs::create_dir_all(&outputs)
            .unwrap_or_else(|e| panic!("cannot make {}: {e}", outputs.display()));
        let open = |name: &str| {
            fs::File::create(outputs.join(name))
                .unwrap_or_else(|e| panic!("cannot make {name} in {}: {e}", outputs.display()))
        };
        let mut command = self.command(args, env, None);
        command
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(open("stdout"))
            .stderr(open("stderr"));
        let began = Instant::now();
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
        Running {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            child,
            outputs,
            began,
        }
    }

    /// Starts `vibe <args>` on this machine with its standard output a pipe
    /// whose reading end the test holds (and may drop while it runs), its
    /// standard error written to a file; returns at once. The [`Run`] that
    /// [`Running::wait`] gives has an empty standard output.
    pub fn spawn_piped(&self, args: &[&str]) -> (Running, std::io::PipeReader) {
        self.spawn_piped_with_env(args, &[])
    }

    /// As [`Machine::spawn_piped`], with these environment variables added.
    pub fn spawn_piped_with_env(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (Running, std::io::PipeReader) {
        let outputs = std::env::temp_dir().join(format!("vm-blackbox-run-{}", token()));
        fs::create_dir_all(&outputs)
            .unwrap_or_else(|e| panic!("cannot make {}: {e}", outputs.display()));
        let stderr = fs::File::create(outputs.join("stderr"))
            .unwrap_or_else(|e| panic!("cannot make stderr in {}: {e}", outputs.display()));
        let (reader, writer) =
            std::io::pipe().unwrap_or_else(|e| panic!("cannot make a pipe: {e}"));
        let mut command = self.command(args, env, None);
        command.stdin(Stdio::null()).stdout(writer).stderr(stderr);
        let began = Instant::now();
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
        // The parent's copy of the writing end goes, so that only `vibe`
        // holds it.
        drop(command);
        (
            Running {
                args: args.iter().map(|a| (*a).to_owned()).collect(),
                child,
                outputs,
                began,
            },
            reader,
        )
    }

    /// Runs `vibe <args>` on this machine with its standard output or error
    /// broken before it can write anything; `stdout` of the [`Run`] is
    /// empty, and so is `stderr` when standard error is closed.
    pub fn run_broken(&self, args: &[&str], broken: Broken) -> Run {
        let mut command = self.command(args, &[], Some(broken));
        let began = Instant::now();
        let (reader, writer) =
            std::io::pipe().unwrap_or_else(|e| panic!("cannot make a pipe: {e}"));
        if broken == Broken::StdoutReaderGone {
            command.stdout(writer);
        } else {
            // `sh` closes it before `exec`, or it is not what is broken.
            command.stdout(Stdio::null());
            drop(writer);
        }
        // The reading end goes before the program starts: every write it
        // makes finds no reader.
        drop(reader);
        command
            .stdin(Stdio::null())
            .stderr(if broken.keeps_stderr() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
        // The parent's copy of the writing end (inside `command`) goes too.
        drop(command);
        let stderr = child.stderr.take().map(|mut stream| {
            thread::spawn(move || {
                let mut kept = Vec::new();
                drop(stream.by_ref().take(1024 * 1024).read_to_end(&mut kept));
                String::from_utf8_lossy(&kept).into_owned()
            })
        });
        let deadline = began + TOOL_TIMEOUT;
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                drop(child.kill());
                drop(child.wait());
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let signal = status.and_then(|s| s.signal());
        let mut stderr = stderr
            .map(|t| t.join().unwrap_or_default())
            .unwrap_or_default();
        if let Some(signal) = signal {
            stderr.push_str(&format!("\n[the harness: ended by signal {signal}]"));
        }
        Run {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: status.and_then(|s| s.code()),
            stdout: String::new(),
            stderr,
            elapsed: began.elapsed(),
        }
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)], broken: Option<Broken>) -> Command {
        let close = broken.map_or("", Broken::redirections);
        let mut command = if self.umask.is_none() && close.is_empty() {
            let mut command = Command::new(vm_binary());
            command.args(args);
            command
        } else {
            let umask = self
                .umask
                .as_ref()
                .map_or_else(String::new, |u| format!("umask {u} && "));
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(format!("{umask}exec \"$0\" \"$@\"{close}"))
                .arg(vm_binary())
                .args(args);
            command
        };
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .current_dir(self.cwd());
        match &self.config_home {
            ConfigHome::Absolute => {
                command.env("XDG_CONFIG_HOME", self.absolute_config_home());
            }
            ConfigHome::Unset => {}
            ConfigHome::Relative(path) => {
                command.env("XDG_CONFIG_HOME", OsString::from(path));
            }
        }
        for (name, value) in env {
            command.env(name, value);
        }
        command
    }

    /// `vibe sign-in` to `vibe` as this User, with `--json`, which must end
    /// with `0`; returns what it printed.
    pub fn sign_in(&self, vm: &Vm, user: &User) -> Value {
        let args = sign_in_args(vm.address(), user);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut with_json = refs.clone();
        with_json.push("--json");
        self.run(&with_json).ok_json()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        // A test may have taken permissions away; give them back so that
        // everything can be removed.
        restore_permissions(&self.root);
        drop(fs::remove_dir_all(&self.root));
    }
}

fn restore_permissions(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        drop(fs::set_permissions(path, fs::Permissions::from_mode(0o700)));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                restore_permissions(&entry.path());
            }
        }
    } else if meta.is_file() {
        drop(fs::set_permissions(path, fs::Permissions::from_mode(0o600)));
    }
}

fn collect_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => collect_files(&path, found),
            Ok(_) => found.push(path),
            Err(_) => {}
        }
    }
}

/// The arguments of `vibe sign-in` to the program at `address` as this User
/// (without `--json`).
pub fn sign_in_args(address: SocketAddr, user: &User) -> Vec<String> {
    let mut args = vec![
        "sign-in".to_owned(),
        "--domain".to_owned(),
        DOMAIN.to_owned(),
        "--connect".to_owned(),
        address.to_string(),
        "--stand-in-user".to_owned(),
        user.id.clone(),
    ];
    if !user.groups.is_empty() {
        args.push("--stand-in-groups".to_owned());
        args.push(user.groups.join(","));
    }
    args
}

/// The permission bits of a file or folder.
pub fn mode_of(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("cannot read the mode of {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

/// Sets the permission bits of a file or folder.
pub fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("cannot set the mode of {}: {e}", path.display()));
}

/// The maximal runs of credential-like text (letters, digits and `-_+/=~`)
/// of at least [`SHORTEST_CREDENTIAL`] characters in `bytes`.
pub fn credential_like_runs(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| !(b.is_ascii_alphanumeric() || b"-_+/=~".contains(b)))
        .filter(|run| run.len() >= SHORTEST_CREDENTIAL)
        .map(|run| String::from_utf8_lossy(run).into_owned())
        .collect()
}

/// A request to the platform's API of `vibe`, signed in with this tool
/// credential only.
pub fn bearer(vm: &Vm, target: &str, credential: &str) -> Request {
    vm.api_request(target).bearer(credential)
}

/// Where any [`WINDOW`]-byte window of `secret` appears in these texts, as
/// (whence, the window's offset) pairs. The window itself is not shown.
pub fn windows_in(texts: &[(String, String)], secret: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = secret.as_bytes();
    for (whence, text) in texts {
        let hit = bytes
            .windows(WINDOW.min(bytes.len()).max(1))
            .enumerate()
            .find(|(_, w)| std::str::from_utf8(w).is_ok_and(|w| !w.is_empty() && text.contains(w)));
        if let Some((offset, _)) = hit {
            found.push(format!(
                "{whence} holds the {WINDOW} bytes at offset {offset} of the credential"
            ));
        }
    }
    found
}

/// The tables of the platform database (outside Postgres's own schemas) in
/// which some row, as text, holds `needle` or the hex of its bytes. Read as
/// the harness's superuser.
pub fn platform_tables_holding(platform: &Platform, needle: &str) -> Vec<String> {
    let sql = Sql::connect(platform.url())
        .unwrap_or_else(|e| panic!("cannot reach the platform database {platform:?}: {e}"));
    let tables: Vec<String> = sql
        .batch(
            "SELECT quote_ident(schemaname) || '.' || quote_ident(tablename) FROM pg_tables \
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1",
        )
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    assert!(
        !tables.is_empty(),
        "the harness's own check: expected the platform database {platform:?} to have tables"
    );
    let quoted = needle.replace('\'', "''");
    let hex: String = needle.bytes().map(|b| format!("{b:02x}")).collect();
    tables
        .into_iter()
        .filter(|table| {
            let count = sql.value(&format!(
                "SELECT count(*) FROM {table} AS t WHERE strpos(t::text, '{quoted}') > 0 \
                 OR strpos(lower(t::text), '{hex}') > 0"
            ));
            count.as_deref() != Some("0")
        })
        .collect()
}

/// A run of the tool started by [`Machine::spawn_into_files`].
pub struct Running {
    args: Vec<String>,
    child: std::process::Child,
    outputs: PathBuf,
    began: Instant,
}

impl Running {
    /// The process id of `vibe`.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Sends `signal` (`INT`, `TERM`, `HUP`, ...) to `vibe` alone, with the
    /// `kill` program.
    pub fn signal(&self, signal: &str) {
        let mut command = Command::new("kill");
        command
            .arg(format!("-{signal}"))
            .arg(self.pid().to_string());
        let sent = run_to_exit(command, Duration::from_secs(5))
            .unwrap_or_else(|problem| panic!("cannot run kill: {problem}"));
        assert!(
            sent.status.is_some_and(|s| s.success()),
            "kill -{signal} {} failed: {}",
            self.pid(),
            sent.stderr
        );
    }

    /// What `vibe` has written on its standard output so far, while it runs.
    pub fn stdout_so_far(&self) -> String {
        let bytes = fs::read(self.outputs.join("stdout")).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// What `vibe` has written on its standard error so far, while it runs.
    pub fn stderr_so_far(&self) -> String {
        let bytes = fs::read(self.outputs.join("stderr")).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Whether `vibe` has ended.
    pub fn has_ended(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Waits for `vibe` to end, at most `timeout` (then kills it), and
    /// returns what it printed and how it ended.
    pub fn wait(mut self, timeout: Duration) -> Run {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                drop(self.child.kill());
                drop(self.child.wait());
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let read = |name: &str| {
            let bytes = fs::read(self.outputs.join(name)).unwrap_or_default();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Run {
            args: std::mem::take(&mut self.args),
            code: status.and_then(|s| s.code()),
            stdout: read("stdout"),
            stderr: read("stderr"),
            elapsed: self.began.elapsed(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            drop(self.child.kill());
            drop(self.child.wait());
        }
        drop(fs::remove_dir_all(&self.outputs));
    }
}

/// What one run of the tool printed and how it ended.
#[derive(Clone, Debug)]
pub struct Run {
    pub args: Vec<String>,
    /// `None` when it ended by a signal or was killed at [`TOOL_TIMEOUT`].
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Run {
    /// The run as text, for failure messages.
    pub fn describe(&self) -> String {
        format!(
            "`vibe {}` ended with {} after {:?}\nstandard output:\n{}\nstandard error:\n{}",
            self.args.join(" "),
            self.code
                .map_or_else(|| "no exit code".to_owned(), |c| format!("exit code {c}")),
            self.elapsed,
            self.stdout,
            self.stderr
        )
    }

    /// Both outputs, labelled, for searches.
    pub fn texts(&self) -> Vec<(String, String)> {
        let command = format!("`vibe {}`", self.args.join(" "));
        vec![
            (
                format!("the standard output of {command}"),
                self.stdout.clone(),
            ),
            (
                format!("the standard error of {command}"),
                self.stderr.clone(),
            ),
        ]
    }

    /// Fails unless it ended with this exit code.
    pub fn expect_code(&self, code: i32) -> &Self {
        assert!(
            self.code == Some(code),
            "expected exit code {code}, got\n{}",
            self.describe()
        );
        self
    }

    /// Fails unless standard output is exactly one JSON object and a line
    /// end; returns the object.
    pub fn json(&self) -> Value {
        assert!(
            self.stdout.ends_with('\n') && !self.stdout.ends_with("\n\n"),
            "expected standard output to be one JSON object and a line end, got\n{}",
            self.describe()
        );
        let value: Value = serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "expected standard output to be exactly one JSON object ({e}), got\n{}",
                self.describe()
            )
        });
        assert!(
            value.is_object(),
            "expected a JSON object on standard output, got\n{}",
            self.describe()
        );
        value
    }

    /// Fails unless it ended with `0` and printed one JSON object.
    pub fn ok_json(&self) -> Value {
        self.expect_code(0);
        self.json()
    }

    /// Fails unless it ended with this code and printed (it was run with
    /// `--json`) `{ "error": { "reason": <reason>, "message": <a sentence> } }`;
    /// returns the message.
    pub fn json_error(&self, code: i32, reason: &str) -> String {
        self.expect_code(code);
        let body = self.json();
        assert_eq!(
            body["error"]["reason"].as_str(),
            Some(reason),
            "expected the error's reason {reason:?}, got\n{}",
            self.describe()
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            !message.trim().is_empty(),
            "expected the error to carry a non-empty `message`, got\n{}",
            self.describe()
        );
        message.to_owned()
    }

    /// Fails unless it ended with this code, without `--json`: nothing on
    /// standard output and a message on standard error; returns the
    /// message.
    pub fn text_error(&self, code: i32) -> String {
        self.expect_code(code);
        assert!(
            self.stdout.trim().is_empty(),
            "expected nothing on standard output for an error without --json, got\n{}",
            self.describe()
        );
        assert!(
            !self.stderr.trim().is_empty(),
            "expected the error's message on standard error, got\n{}",
            self.describe()
        );
        self.stderr.clone()
    }

    /// Fails unless it ended with `0` and wrote text (not JSON) on
    /// standard output; returns it.
    pub fn ok_text(&self) -> String {
        self.expect_code(0);
        assert!(
            !self.stdout.trim().is_empty(),
            "expected text on standard output, got\n{}",
            self.describe()
        );
        self.stdout.clone()
    }
}

/// What a [`NotAnInstallation`] does with each connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pretender {
    /// Answers `200` with a plain-text body: not JSON.
    Nonsense,
    /// Answers `200`, `application/json`, with a well-formed JSON object
    /// of [`HUGE_ANSWER_BYTES`].
    Huge,
    /// Accepts the connection, reads, and never answers.
    Silent,
}

/// The size of [`Pretender::Huge`]'s body: over the tool's 8 MiB.
pub const HUGE_ANSWER_BYTES: usize = 8 * 1024 * 1024 + 4096;

/// The most connections [`Pretender::Silent`] holds open at once; later
/// ones are closed at once.
const SILENT_CONNECTIONS: usize = 16;

/// A listener on 127.0.0.1 that is not an installation (see
/// [`Pretender`]). Stopped and joined when dropped.
pub struct NotAnInstallation {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl NotAnInstallation {
    /// One that answers [`Pretender::Nonsense`].
    pub fn start() -> Self {
        Self::pretending(Pretender::Nonsense)
    }

    pub fn pretending(how: Pretender) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("cannot bind 127.0.0.1:0");
        listener
            .set_nonblocking(true)
            .expect("cannot make the listener non-blocking");
        let address = listener.local_addr().expect("the listener has an address");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut held: Vec<TcpStream> = Vec::new();
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => match how {
                        Pretender::Nonsense => {
                            answer(stream, "text/plain", "this is not an installation")
                        }
                        Pretender::Huge => {
                            let body = format!("{{\"x\":\"{}\"}}", "a".repeat(HUGE_ANSWER_BYTES));
                            answer(stream, "application/json", &body);
                        }
                        Pretender::Silent => {
                            if held.len() < SILENT_CONNECTIONS {
                                held.push(stream);
                            }
                        }
                    },
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

fn answer(mut stream: TcpStream, content_type: &str, body: &str) {
    drop(stream.set_nonblocking(false));
    drop(stream.set_read_timeout(Some(Duration::from_secs(2))));
    drop(stream.set_write_timeout(Some(Duration::from_secs(10))));
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 64 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
    }
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    drop(stream.write_all(head.as_bytes()));
    drop(stream.write_all(body.as_bytes()));
}

impl Drop for NotAnInstallation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

/// An address on 127.0.0.1 where nothing listens.
pub fn closed_address() -> SocketAddr {
    let port = crate::postgres::free_port().expect("a free port");
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_like_runs_are_found_in_any_form_of_file() {
        let credential = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcdef";
        let file = format!(
            "{{\"domain\":\"vm.test\",\"credential\":\"{credential}\",\"expires_at\":\"2026-12-14T10:00:00Z\"}}"
        );
        assert_eq!(credential_like_runs(file.as_bytes()), vec![credential]);
        assert_eq!(
            credential_like_runs(format!("{credential}\n").as_bytes()),
            vec![credential]
        );
        assert!(credential_like_runs(b"short=value\nvm.test").is_empty());
    }

    #[test]
    fn any_window_of_a_credential_is_found() {
        let credential = "0123456789abcdefghijklmnopqrstuvwxyz";
        let texts = |text: &str| vec![("a text".to_owned(), text.to_owned())];
        assert_eq!(windows_in(&texts("nothing here"), credential).len(), 0);
        assert_eq!(
            windows_in(&texts("…ghijklmnopqrstuv…"), credential).len(),
            1
        );
        assert_eq!(windows_in(&texts(credential), credential).len(), 1);
        assert_eq!(windows_in(&texts("ghijklmnopqrstu"), credential).len(), 0);
    }
}
