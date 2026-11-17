//! A tool run by the commands that work on a project (`vm check`, `vm
//! generate`, `vm deploy`; blackbox/CONTRACT.md, "Checking a project",
//! "Deploying"): a program from the `PATH` (`node`, `git`) with the
//! arguments given, in the project's folder, with the command's own folder
//! as its `TMPDIR` and nothing on its standard input.
//!
//! - **Its own process group.** The tool is started as the leader of a new
//!   group, and the whole group is sent `SIGKILL` as soon as the tool has
//!   ended (before its leader is reaped: see `Group::stop`), and again (through [`Group`]'s `Drop`) if the run is given up
//!   before that, as when the check's time is up and its future is dropped.
//!   So nothing it started is left running, whichever way the check ends.
//!   (A process that leaves the group on purpose is not followed.)
//! - **Bounded reading.** Standard output and standard error are read to
//!   their end, at the same time as the tool is waited for, so that a full
//!   pipe never stalls it; what is kept of them is a [`Sink`]'s to bound.
//!   Reading goes on, and what is past the bound is dropped, until the
//!   pipes close (at the latest when the group is stopped).
//! - **No wait of its own.** The run has no end of its own: it is always
//!   inside the check's whole timeout, which drops it.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use std::time::Duration;

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use tokio::io::{AsyncRead, AsyncReadExt};

/// How much of a pipe is read at a time.
const CHUNK_BYTES: usize = 8 * 1024;

/// What is kept of what a tool writes on one of its pipes.
pub trait Sink {
    /// The next bytes the tool wrote.
    fn take(&mut self, bytes: &[u8]);
    /// The pipe has closed.
    fn end(&mut self) {}
}

/// The first `cap` bytes written; the rest is dropped and counted as over.
pub struct Kept {
    bytes: Vec<u8>,
    cap: usize,
    pub over: bool,
}

impl Kept {
    pub fn up_to(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
            over: false,
        }
    }

    /// What was kept, as text (what is not UTF-8 replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// What was kept, handed over.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Sink for Kept {
    fn take(&mut self, bytes: &[u8]) {
        let room = self.cap.saturating_sub(self.bytes.len());
        if bytes.len() > room {
            self.over = true;
        }
        self.bytes
            .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
    }
}

/// Bytes cut into lines at each line end (or into records at each NUL); a
/// line longer than `max` bytes is kept to its first `max` (the rest of it
/// dropped).
pub struct Lines {
    line: Vec<u8>,
    max: usize,
    /// What ends a line: `\n` (a `\r` before it is dropped too), or NUL.
    end: u8,
}

impl Lines {
    pub fn of_at_most(max: usize) -> Self {
        Self {
            line: Vec::new(),
            max,
            end: b'\n',
        }
    }

    /// Records ended by NUL (as `git ... -z` writes them), each kept as it
    /// is, to its first `max` bytes.
    pub fn records_of_at_most(max: usize) -> Self {
        Self {
            line: Vec::new(),
            max,
            end: 0,
        }
    }

    /// The line kept so far, as text.
    fn text(&self) -> std::borrow::Cow<'_, str> {
        let text = String::from_utf8_lossy(&self.line);
        if self.end == b'\n' && text.ends_with('\r') {
            std::borrow::Cow::Owned(text.trim_end_matches('\r').to_owned())
        } else {
            text
        }
    }

    /// Feeds `bytes`, handing each line that ends in them to `each`.
    pub fn feed(&mut self, bytes: &[u8], mut each: impl FnMut(&str)) {
        for &byte in bytes {
            if byte == self.end {
                each(&self.text());
                self.line.clear();
            } else if self.line.len() < self.max {
                self.line.push(byte);
            }
        }
    }

    /// Hands the last line, if it had no line end, to `each`.
    pub fn finish(&mut self, mut each: impl FnMut(&str)) {
        if !self.line.is_empty() {
            each(&self.text());
            self.line.clear();
        }
    }
}

/// How a tool ended, and what was kept of what it wrote.
pub struct Ran<O, E> {
    pub status: ExitStatus,
    pub out: O,
    pub err: E,
}

/// A process group to stop: the one a tool leads.
struct Group {
    leader: Option<Pid>,
    stopped: bool,
}

impl Group {
    fn led_by(id: Option<u32>) -> Self {
        Self {
            leader: id
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw),
            stopped: false,
        }
    }

    /// Sends `SIGKILL` to every process of the group, once. A group with
    /// no process left is no error.
    ///
    /// Why the id is still the group's: a group's id is its leader's process
    /// id, and the kernel gives that number to no other process while any
    /// process of the group lives, the leader's zombie included. So `stop`
    /// is safe while the leader runs or has exited and is not yet reaped:
    /// that is when [`run`] calls it (it learns of the exit with a `waitid`
    /// that does not reap, then signals, then reaps), and when a run given
    /// up is dropped (the leader is reaped only after, by tokio, as the
    /// child is dropped). Only when `waitid` itself fails is the leader
    /// reaped first; then, if no process of the group is left either, the
    /// number is free and could in principle be given to a new process in
    /// the moment before the signal, which would reach that process's group.
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            if let Some(leader) = self.leader {
                kill_process_group(leader, Signal::KILL).ok();
            }
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.stop();
    }
}

/// How often a running tool is asked whether it has exited.
const EXIT_POLL: Duration = Duration::from_millis(10);

/// Waits until the process `leader` has exited, without reaping it: asked
/// every [`EXIT_POLL`] with `waitid(WEXITED | WNOWAIT | WNOHANG)`, which
/// leaves it waitable for tokio's `wait`, and never blocks the thread.
/// `false` when that cannot be asked; the caller then waits otherwise.
async fn exited(leader: Option<Pid>) -> bool {
    let Some(leader) = leader else {
        return false;
    };
    loop {
        match waitid(
            WaitId::Pid(leader),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
        ) {
            Ok(Some(_)) => return true,
            Ok(None) => tokio::time::sleep(EXIT_POLL).await,
            Err(_) => return false,
        }
    }
}

/// Runs `<program> <args>` (`program` a path, or a name found on the
/// `PATH`) in `cwd`, with
/// `tmpdir` as its `TMPDIR`, and reads its pipes into `out` and `err`. The
/// error is the one of starting it (`NotFound`: no such program on the
/// `PATH`).
pub async fn run<P: AsRef<OsStr>, O: Sink, E: Sink>(
    program: P,
    cwd: &Path,
    tmpdir: &Path,
    args: &[&OsStr],
    out: O,
    err: E,
) -> io::Result<Ran<O, E>> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("TMPDIR", tmpdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    // Declared after the child, so dropped before it: the whole group is
    // stopped first, then the leader is killed and left to be reaped.
    let mut group = Group::led_by(child.id());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let leader = group.leader;
    let waited = async {
        // The tool has ended: what it started ends too, and the pipes
        // that those held close. Before the leader is reaped where that
        // can be known (see `Group::stop`), else just after.
        if exited(leader).await {
            group.stop();
            child.wait().await
        } else {
            let status = child.wait().await;
            group.stop();
            status
        }
    };
    let (status, out, err) = tokio::join!(waited, drain(stdout, out), drain(stderr, err));
    Ok(Ran {
        status: status?,
        out,
        err,
    })
}

/// Reads `pipe` to its end into `sink`. A pipe that fails to read is taken
/// as ended.
async fn drain<S: Sink>(pipe: Option<impl AsyncRead + Unpin>, mut sink: S) -> S {
    if let Some(mut pipe) = pipe {
        let mut chunk = vec![0u8; CHUNK_BYTES];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.take(chunk.get(..n).unwrap_or_default()),
            }
        }
    }
    sink.end();
    sink
}
