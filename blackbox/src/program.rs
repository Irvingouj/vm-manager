//! Getting the `vm` binary (CONTRACT.md, "How the tests get the program"),
//! and running it to its exit with a timeout.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long building the program may take. The first build of a fresh
/// checkout compiles the execution core; later ones are no-ops.
const BUILD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// How long a run that is expected to end by itself may take.
pub const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

/// The most bytes kept from one stream of a program run to its exit.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// The `vm` binary: `VM_BIN` when set, otherwise the one `cargo build -p vm
/// --bin vm` produces, built once per test binary (cargo makes the builds of
/// several test binaries wait for each other, and all but the first are
/// no-ops).
pub fn vm_binary() -> &'static Path {
    static BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match BINARY.get_or_init(find_or_build) {
        Ok(path) => path,
        Err(problem) => panic!("cannot get the vm program: {problem}"),
    }
}

fn find_or_build() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("VM_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "VM_BIN names {}, which is not a file",
                path.display()
            ))
        };
    }
    let manifest = manifest_dir();
    let workspace = manifest
        .parent()
        .ok_or("the blackbox crate has no parent directory")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(workspace).args([
        "build",
        "-p",
        "vm",
        "--bin",
        "vm",
        "--message-format=json-render-diagnostics",
    ]);
    let finished = run_to_exit(command, BUILD_TIMEOUT)?;
    if !finished.status.is_some_and(|s| s.success()) {
        return Err(format!(
            "`cargo build -p vm --bin vm` failed ({}):\n{}",
            finished.status_text(),
            finished.stderr
        ));
    }
    finished
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "vm")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "cargo built no `vm` executable".to_owned())
}

/// The `blackbox` crate's directory in the checkout being tested: the value
/// cargo gives the test process at run time, else the one this crate was
/// compiled in. Run time first, so that two checkouts sharing one `target/`
/// each test their own program and fixtures.
pub fn manifest_dir() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from)
}

/// What a program run to its exit left behind.
#[derive(Debug)]
pub struct Finished {
    /// `None` when it had to be killed at the timeout.
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
}

impl Finished {
    pub fn status_text(&self) -> String {
        match self.status {
            Some(status) => status.to_string(),
            None => "still running at the timeout, killed".to_owned(),
        }
    }
}

/// Runs `vm` with these arguments until it exits by itself, killing it at
/// [`EXIT_TIMEOUT`]. `VM_DATABASE_URL` is set to `database_url`, or unset
/// when it is `None`, whatever the test's own environment holds.
pub fn run_vm_to_exit<I, S>(args: I, database_url: Option<&str>) -> Finished
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(vm_binary());
    command.args(args).env_remove("VM_DATABASE_URL");
    if let Some(url) = database_url {
        command.env("VM_DATABASE_URL", url);
    }
    run_to_exit(command, EXIT_TIMEOUT)
        .unwrap_or_else(|problem| panic!("cannot run {}: {problem}", vm_binary().display()))
}

/// Runs `command` with no input until it exits, at most `timeout`, keeping
/// a bounded amount of its output.
pub fn run_to_exit(mut command: Command, timeout: Duration) -> Result<Finished, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {command:?}: {e}"))?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let status = wait_with_timeout(&mut child, timeout);
    let collect = |reader: Option<thread::JoinHandle<String>>| {
        reader
            .map(|r| r.join().unwrap_or_default())
            .unwrap_or_default()
    };
    Ok(Finished {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Waits for `child` to exit, at most `timeout`; kills it after that.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Reads a stream to its end on a thread, keeping at most
/// [`MAX_OUTPUT_BYTES`] and discarding the rest so the writer never blocks.
fn drain(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}
