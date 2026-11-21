//! The directory backend (ADR 0019, "The directory backend: nothing
//! escapes"): a Storage Resource is a directory beneath the installation's
//! storage root, held open from when the Resource is loaded, and every key
//! is reached from that descriptor one segment at a time, never through a
//! path string.
//!
//! - **Reaching a key.** Each folder of the key is opened with
//!   `openat(parent, segment, O_DIRECTORY | O_NOFOLLOW)` from the
//!   descriptor before it, and must be on the Resource's file system
//!   (`st_dev`); the last segment is looked at with `statat(...,
//!   AT_SYMLINK_NOFOLLOW)` and opened only when it is a regular file, with
//!   `O_NOFOLLOW | O_NONBLOCK`, then `fstat`ed again. A key has no `.`,
//!   `..` or empty segment (crate::storage::key), so nothing leads out; a
//!   link anywhere on the way is refused (`not-reachable` `link`), and one
//!   swapped in between two steps is refused at the next step. Everything
//!   is `rustix::fs`, safe Rust.
//! - **A put** writes a new file in `.vibe-staging/` (`O_CREAT | O_EXCL`,
//!   mode `0640`, a random name), sets the content type on it, `fsync`s
//!   it, makes the folders of the key that are missing (mode `0750`),
//!   renames it onto the key and `fsync`s the folders it changed. A reader
//!   sees the file before or after, never in part; a put that fails
//!   removes its staging file. A put racing a delete that removes one of
//!   its folders walks again (at most [`PUT_TRIES`] times).
//! - **A delete** unlinks the file, then removes each folder of the key
//!   that is left empty, deepest first.
//! - **A list** reads one folder at a time, keeping only the names that can
//!   be a key's segment and the [`PAGE`]+1 smallest after where the page
//!   starts (a folder's name taken as `name/`, which gives the byte order of
//!   whole keys), and walks depth-first. Links, FIFOs, devices and folders
//!   on another file system are passed over; with the delimiter, a folder
//!   is answered only when it holds a file that can be reached.
//! - **Unavailable.** When the directory cannot be opened, or its
//!   `.vibe-resource` is not there or names another Resource, every
//!   operation fails `unavailable`, nothing is written, the program's log
//!   says `storage-unavailable` at most once a minute, and the directory is
//!   tried again at most every [`RETRY`], at an operation.
//!
//! All file work runs on the program's file threads (crate::storage::files).
//!
//! [`Root`] is the storage root and the registration of a directory
//! beneath it (blackbox/CONTRACT.md, "Registering a Storage Resource").

use std::collections::BinaryHeap;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, Stat, XattrFlags};
use rustix::io::Errno;
use serde_json::json;

use super::files::Files;
use super::key::{self, KEY_BYTES, Key, SEGMENTS};
use super::{
    BODY_BYTES, Backend, Bodies, Body, CONTENT_TYPE_ATTRIBUTE, CONTENT_TYPE_BYTES, Failure, Got,
    ListQuery, Metadata, Op, PAGE, Page, Position, Unreachable, Until, content_type_of, encode,
    is_content_type,
};
use crate::log;

/// The marker a Resource's directory holds, naming the Resource.
pub const MARKER: &str = ".vibe-resource";

/// Where a put writes its file before it lands.
pub const STAGING: &str = ".vibe-staging";

/// How often a directory that could not be used is tried again, at most.
pub const RETRY: Duration = Duration::from_secs(10);

/// How often `storage-unavailable` is said, at most, while it lasts.
pub const SAY_AGAIN: Duration = Duration::from_secs(60);

/// How many times a put walks to its key when a delete removed a folder of
/// it meanwhile.
pub const PUT_TRIES: usize = 4;

/// The most bytes of a marker read.
const MARKER_BYTES: u64 = 4096;

/// The mode of a file a put writes, and of a folder it makes.
const FILE_MODE: u32 = 0o640;
const FOLDER_MODE: u32 = 0o750;

/// The directory of a Resource, opened.
#[derive(Debug)]
pub struct Opened {
    fd: OwnedFd,
    dev: u64,
    ino: u64,
}

impl Opened {
    fn of(fd: OwnedFd) -> Result<Self, Errno> {
        let stat = rustix::fs::fstat(&fd)?;
        Ok(Self {
            fd,
            dev: stat.st_dev,
            ino: stat.st_ino,
        })
    }
}

/// Why a Resource's directory cannot be used: the `problem` of
/// `storage-unavailable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    Missing,
    NotADirectory,
    NotMarked,
    MarkedForAnother,
}

impl Problem {
    pub fn name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::NotADirectory => "not-a-directory",
            Self::NotMarked => "not-marked",
            Self::MarkedForAnother => "marked-for-another",
        }
    }
}

enum State {
    Open(Arc<Opened>),
    Closed {
        problem: Problem,
        tried: Instant,
        said: Instant,
    },
}

struct Inner {
    /// The Resource's name, which its marker must hold.
    name: String,
    /// The directory, as registered. Never printed.
    path: PathBuf,
    /// Locked only for a look or a change, never during file work or
    /// across an await.
    state: Mutex<State>,
}

/// A Storage Resource on a directory.
pub struct Directory {
    inner: Arc<Inner>,
    files: Arc<Files>,
}

impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Directory")
            .field("resource", &self.inner.name)
            .finish_non_exhaustive()
    }
}

impl Directory {
    /// The Resource `name` on `path`, opened on a file thread, its staging
    /// emptied of what a crash left: at start, and for the local run. When
    /// it cannot be used, it is held closed (and said so), and tried again
    /// at an operation.
    pub async fn load(name: &str, path: &Path, files: &Arc<Files>) -> Self {
        let inner = Arc::new(Inner {
            name: name.to_owned(),
            path: path.to_owned(),
            state: Mutex::new(State::Closed {
                problem: Problem::Missing,
                tried: Instant::now(),
                said: Instant::now(),
            }),
        });
        let opening = Arc::clone(&inner);
        let opened = match files.run(move || open(&opening.path, &opening.name)).await {
            Some(answer) => tokio::time::timeout(super::OPERATION_TIME, answer)
                .await
                .ok()
                .and_then(Result::ok),
            None => None,
        };
        let state = match opened {
            Some(Ok(opened)) => State::Open(Arc::new(opened)),
            Some(Err(problem)) => {
                say_unavailable(name, problem);
                closed(problem)
            }
            None => {
                say_unavailable(name, Problem::Missing);
                closed(Problem::Missing)
            }
        };
        *inner.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
        Self {
            inner,
            files: Arc::clone(files),
        }
    }

    /// The Resource `name`, just registered on `path`, which registration
    /// opened and marked.
    pub fn registered(name: &str, path: &Path, opened: Arc<Opened>, files: &Arc<Files>) -> Self {
        Self {
            inner: Arc::new(Inner {
                name: name.to_owned(),
                path: path.to_owned(),
                state: Mutex::new(State::Open(opened)),
            }),
            files: Arc::clone(files),
        }
    }

    /// The directory as registered, for the check that a new one is not
    /// inside it or holding it. Never printed.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The directory's identity while it is open.
    fn identity(&self) -> Option<Arc<Opened>> {
        match &*self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            State::Open(opened) => Some(Arc::clone(opened)),
            State::Closed { .. } => None,
        }
    }

    /// Runs `job` on a file thread with the directory opened, within
    /// `until`.
    async fn on_files<T: Send + 'static>(
        &self,
        until: Until,
        job: impl FnOnce(&Opened) -> Result<T, Failure> + Send + 'static,
    ) -> Result<T, Failure> {
        let inner = Arc::clone(&self.inner);
        let job_until = until.clone();
        let answer = self
            .files
            .run(move || {
                if job_until.over() {
                    return Err(Failure::Time);
                }
                let opened = inner.opened()?;
                job(&opened)
            })
            .await
            .ok_or(Failure::Unavailable)?;
        let deadline = tokio::time::Instant::from_std(until.deadline());
        match tokio::time::timeout_at(deadline, answer).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(Failure::Unavailable),
            Err(_) => Err(Failure::Time),
        }
    }
}

fn closed(problem: Problem) -> State {
    let now = Instant::now();
    State::Closed {
        problem,
        tried: now,
        said: now,
    }
}

fn say_unavailable(name: &str, problem: Problem) {
    log::record(
        "storage-unavailable",
        json!({ "resource": name, "problem": problem.name() }),
    );
}

impl Inner {
    /// The directory, opened: held since it was loaded, or opened again
    /// now when it could not be used and [`RETRY`] has passed. On a file
    /// thread.
    fn opened(&self) -> Result<Arc<Opened>, Failure> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match &mut *state {
                State::Open(opened) => return Ok(Arc::clone(opened)),
                State::Closed { tried, .. } if tried.elapsed() < RETRY => {
                    return Err(Failure::Unavailable);
                }
                // Taken by this thread: the others answer `unavailable`
                // meanwhile.
                State::Closed { tried, .. } => *tried = Instant::now(),
            }
        }
        let opened = open(&self.path, &self.name);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match opened {
            Ok(opened) => {
                let opened = Arc::new(opened);
                *state = State::Open(Arc::clone(&opened));
                log::record("storage-available", json!({ "resource": &self.name }));
                Ok(opened)
            }
            Err(now) => {
                if let State::Closed { problem, said, .. } = &mut *state
                    && (said.elapsed() >= SAY_AGAIN || *problem != now)
                {
                    *problem = now;
                    *said = Instant::now();
                    say_unavailable(&self.name, now);
                }
                Err(Failure::Unavailable)
            }
        }
    }
}

/// Opens the directory of the Resource `name` at `path`, checks its marker
/// and empties its staging of what a crash left (making it if it is not
/// there).
fn open(path: &Path, name: &str) -> Result<Opened, Problem> {
    let fd = rustix::fs::openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| match e {
        Errno::NOTDIR | Errno::LOOP => Problem::NotADirectory,
        _ => Problem::Missing,
    })?;
    let opened = Opened::of(fd).map_err(|_| Problem::Missing)?;
    match read_marker(opened.fd.as_fd()) {
        Marker::None => return Err(Problem::NotMarked),
        Marker::Names(marked) if marked != name => return Err(Problem::MarkedForAnother),
        Marker::Names(_) => {}
    }
    empty_staging(&opened);
    Ok(opened)
}

/// What a directory's `.vibe-resource` says.
enum Marker {
    /// Not there, or not one the program wrote.
    None,
    Names(String),
}

fn read_marker(dir: BorrowedFd<'_>) -> Marker {
    let Ok(fd) = rustix::fs::openat(
        dir,
        MARKER,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    ) else {
        return Marker::None;
    };
    match rustix::fs::fstat(&fd) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {}
        _ => return Marker::None,
    }
    let mut text = String::new();
    if std::fs::File::from(fd)
        .take(MARKER_BYTES)
        .read_to_string(&mut text)
        .is_err()
    {
        return Marker::None;
    }
    match serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .as_ref()
        .and_then(|v| v.get("resource"))
        .and_then(serde_json::Value::as_str)
    {
        Some(named) => Marker::Names(named.to_owned()),
        None => Marker::None,
    }
}

/// The marker's contents for the Resource `name`.
fn marker_of(name: &str) -> String {
    format!(
        "{}\n",
        json!({ "resource": name, "written_by": "vibe: the platform's own file, do not edit" })
    )
}

/// Removes what is in `.vibe-staging/` (what a crash left), making it when
/// it is not there. Done when the Resource is loaded, before any put.
fn empty_staging(opened: &Opened) {
    let Ok(staging) = staging(opened) else {
        return;
    };
    let Ok(dir) = Dir::read_from(&staging) else {
        return;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if entry.file_type() != FileType::Directory {
            rustix::fs::unlinkat(&staging, entry.file_name(), AtFlags::empty()).ok();
        }
    }
    rustix::fs::fsync(&staging).ok();
}

/// `.vibe-staging/`, opened (made when it is not there).
fn staging(opened: &Opened) -> Result<OwnedFd, Failure> {
    for _ in 0..2 {
        match open_folder(opened.fd.as_fd(), STAGING, opened.dev)? {
            Step::Folder(fd) => return Ok(fd),
            Step::Missing => {
                match rustix::fs::mkdirat(&opened.fd, STAGING, Mode::from_raw_mode(FOLDER_MODE)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(failure(e)),
                }
            }
            Step::File | Step::Other => return Err(Failure::Unavailable),
        }
    }
    Err(Failure::Unavailable)
}

/// What is at a folder's place on the way to a key.
enum Step {
    Folder(OwnedFd),
    Missing,
    /// A regular file.
    File,
    /// A FIFO, a socket or a device.
    Other,
}

/// `name` in `dir` opened as a folder on the file system `dev`, without
/// following a link. A link is `not-reachable` `link`, a folder on another
/// file system `other-filesystem`.
fn open_folder(dir: BorrowedFd<'_>, name: &str, dev: u64) -> Result<Step, Failure> {
    match rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            let stat = rustix::fs::fstat(&fd).map_err(failure)?;
            if stat.st_dev != dev {
                return Err(Failure::NotReachable(Unreachable::OtherFilesystem));
            }
            Ok(Step::Folder(fd))
        }
        Err(Errno::NOENT) => Ok(Step::Missing),
        Err(Errno::NOTDIR | Errno::LOOP) => {
            match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                    FileType::Symlink => Err(Failure::NotReachable(Unreachable::Link)),
                    FileType::RegularFile => Ok(Step::File),
                    // Swapped meanwhile: tried again by the caller's next
                    // operation.
                    FileType::Directory => Err(Failure::Unavailable),
                    _ => Ok(Step::Other),
                },
                Err(Errno::NOENT) => Ok(Step::Missing),
                Err(e) => Err(failure(e)),
            }
        }
        Err(e) => Err(failure(e)),
    }
}

/// A failure of the file system: `full` for a full disk or quota, else
/// `unavailable`.
fn failure(e: Errno) -> Failure {
    match e {
        Errno::NOSPC | Errno::DQUOT => Failure::Full,
        _ => Failure::Unavailable,
    }
}

fn io_failure(e: &std::io::Error) -> Failure {
    Errno::from_io_error(e).map_or(Failure::Unavailable, failure)
}

fn is_regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
}

/// The folders of `key` opened from the directory, not made: `None` when
/// one is not there or is not a folder (then no file has the key). Its
/// last segment is the caller's.
fn folders_of<'k>(
    opened: &Opened,
    key: &'k Key,
) -> Result<Option<(Vec<OwnedFd>, &'k str)>, Failure> {
    let mut segments = key.segments();
    let Some(last) = segments.next_back() else {
        return Ok(None);
    };
    let mut chain: Vec<OwnedFd> = Vec::new();
    for segment in segments {
        let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
        match open_folder(parent, segment, opened.dev)? {
            Step::Folder(fd) => chain.push(fd),
            Step::Missing | Step::File | Step::Other => return Ok(None),
        }
    }
    Ok(Some((chain, last)))
}

/// `name` in `dir`, when it is a regular file: opened for reading, and its
/// `fstat`. A link is `link`, anything else `not-a-file`; `None` when not
/// there. Nothing but a regular file is ever opened (so a FIFO is never
/// waited on).
fn open_file(
    dir: BorrowedFd<'_>,
    name: &str,
    dev: u64,
) -> Result<Option<(OwnedFd, Stat)>, Failure> {
    match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => {}
            FileType::Symlink => return Err(Failure::NotReachable(Unreachable::Link)),
            _ => return Err(Failure::NotReachable(Unreachable::NotAFile)),
        },
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(failure(e)),
    }
    let fd = match rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => return Err(Failure::NotReachable(Unreachable::Link)),
        Err(e) => return Err(failure(e)),
    };
    let stat = rustix::fs::fstat(&fd).map_err(failure)?;
    if !is_regular(&stat) {
        return Err(Failure::NotReachable(Unreachable::NotAFile));
    }
    if stat.st_dev != dev {
        return Err(Failure::NotReachable(Unreachable::OtherFilesystem));
    }
    Ok(Some((fd, stat)))
}

/// The content type kept on the file `fd`, if it holds one the program
/// could have written; else the one its key's extension gives.
fn content_type(fd: BorrowedFd<'_>, key: &str) -> String {
    let mut buffer = [0_u8; CONTENT_TYPE_BYTES];
    if let Ok(n) = rustix::fs::fgetxattr(fd, CONTENT_TYPE_ATTRIBUTE, &mut buffer[..])
        && let Some(kept) = buffer.get(..n)
        && let Ok(kept) = std::str::from_utf8(kept)
        && is_content_type(kept)
    {
        return kept.to_owned();
    }
    content_type_of(key).to_owned()
}

fn metadata(key: String, fd: BorrowedFd<'_>, stat: &Stat) -> Metadata {
    let content_type = content_type(fd, &key);
    Metadata {
        key,
        size: u64::try_from(stat.st_size).unwrap_or(0),
        content_type,
        modified: stat.st_mtime,
    }
}

impl Backend for Directory {
    fn head(&self, key: Key, until: Until) -> Op<'_, Option<Metadata>> {
        Box::pin(self.on_files(until, move |opened| {
            let Some((chain, last)) = folders_of(opened, &key)? else {
                return Ok(None);
            };
            let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
            let Some((fd, stat)) = open_file(parent, last, opened.dev)? else {
                return Ok(None);
            };
            Ok(Some(metadata(key.into_string(), fd.as_fd(), &stat)))
        }))
    }

    fn get(&self, key: Key, bodies: Arc<Bodies>, until: Until) -> Op<'_, Option<Got>> {
        Box::pin(async move {
            let found = self
                .on_files(until.clone(), move |opened| {
                    let Some((chain, last)) = folders_of(opened, &key)? else {
                        return Ok(None);
                    };
                    let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
                    let Some((fd, stat)) = open_file(parent, last, opened.dev)? else {
                        return Ok(None);
                    };
                    let metadata = metadata(key.into_string(), fd.as_fd(), &stat);
                    if metadata.size > u64::try_from(BODY_BYTES).unwrap_or(u64::MAX) {
                        return Err(Failure::Bytes);
                    }
                    Ok(Some((fd, metadata)))
                })
                .await?;
            let Some((fd, mut metadata)) = found else {
                return Ok(None);
            };
            let size = usize::try_from(metadata.size).unwrap_or(BODY_BYTES);
            let room = bodies.take(size).await.ok_or(Failure::Unavailable)?;
            self.on_files(until.clone(), move |_| {
                // At most the size `fstat` gave and one byte: a file that
                // grew past the bound meanwhile fails, it is not read on.
                let mut bytes = Vec::with_capacity(size);
                std::fs::File::from(fd)
                    .take(u64::try_from(size).unwrap_or(0).saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|e| io_failure(&e))?;
                if bytes.len() > BODY_BYTES {
                    return Err(Failure::Bytes);
                }
                metadata.size = u64::try_from(bytes.len()).unwrap_or(0);
                let body = encode(&bytes);
                Ok(Some(Got {
                    metadata,
                    body,
                    room,
                }))
            })
            .await
        })
    }

    fn put(
        &self,
        key: Key,
        body: Body,
        content_type: Option<String>,
        until: Until,
    ) -> Op<'_, Metadata> {
        let job_until = until.clone();
        Box::pin(self.on_files(until, move |opened| {
            put(opened, key, body, content_type, &job_until)
        }))
    }

    fn delete(&self, key: Key, until: Until) -> Op<'_, ()> {
        let job_until = until.clone();
        Box::pin(self.on_files(until, move |opened| delete(opened, &key, &job_until)))
    }

    fn list(&self, query: ListQuery, until: Until) -> Op<'_, Page> {
        let job_until = until.clone();
        Box::pin(self.on_files(until, move |opened| list(opened, &query, &job_until)))
    }
}

/// The given-up answer: `limit: time` when past the deadline, `Stopped`
/// when the host gave the operation up.
fn over(until: &Until) -> Failure {
    if std::time::Instant::now() >= until.deadline() {
        Failure::Time
    } else {
        Failure::Stopped
    }
}

/// A file in `.vibe-staging/`, removed when dropped unless it landed.
struct Staged {
    staging: OwnedFd,
    name: String,
    file: std::fs::File,
    landed: bool,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.landed {
            rustix::fs::unlinkat(&self.staging, self.name.as_str(), AtFlags::empty()).ok();
        }
    }
}

fn put(
    opened: &Opened,
    key: Key,
    body: Body,
    content_type: Option<String>,
    until: &Until,
) -> Result<Metadata, Failure> {
    let bytes = body.decode().ok_or(Failure::Unavailable)?;
    let staging = staging(opened)?;
    let name = format!("put-{}", uuid::Uuid::new_v4().simple());
    let fd = rustix::fs::openat(
        &staging,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(FILE_MODE),
    )
    .map_err(failure)?;
    let mut staged = Staged {
        staging,
        name,
        file: std::fs::File::from(fd),
        landed: false,
    };
    // The mode whatever the program's umask.
    rustix::fs::fchmod(&staged.file, Mode::from_raw_mode(FILE_MODE)).map_err(failure)?;
    staged.file.write_all(&bytes).map_err(|e| io_failure(&e))?;
    drop(bytes);
    if let Some(content_type) = &content_type {
        rustix::fs::fsetxattr(
            &staged.file,
            CONTENT_TYPE_ATTRIBUTE,
            content_type.as_bytes(),
            XattrFlags::empty(),
        )
        .map_err(failure)?;
    }
    rustix::fs::fsync(&staged.file).map_err(failure)?;
    let mut segments: Vec<&str> = key.segments().collect();
    let last = segments.pop().ok_or(Failure::Unavailable)?;
    for _ in 0..PUT_TRIES {
        let (chain, made) = make_folders(opened, &segments)?;
        let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
        match rustix::fs::statat(parent, last, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => {}
                FileType::Directory => return Err(Failure::KeyConflict),
                FileType::Symlink => return Err(Failure::NotReachable(Unreachable::Link)),
                _ => return Err(Failure::NotReachable(Unreachable::NotAFile)),
            },
            Err(Errno::NOENT) => {}
            Err(e) => return Err(failure(e)),
        }
        if until.over() {
            return Err(over(until));
        }
        match rustix::fs::renameat(&staged.staging, staged.name.as_str(), parent, last) {
            Ok(()) => {
                staged.landed = true;
                rustix::fs::fsync(parent).map_err(failure)?;
                // Each folder made, in the folder that holds it.
                for index in made {
                    let holder = match index.checked_sub(1) {
                        None => Some(opened.fd.as_fd()),
                        Some(before) => chain.get(before).map(AsFd::as_fd),
                    };
                    if let Some(holder) = holder {
                        rustix::fs::fsync(holder).map_err(failure)?;
                    }
                }
                let stat = rustix::fs::fstat(&staged.file).map_err(failure)?;
                return Ok(Metadata {
                    content_type: content_type
                        .unwrap_or_else(|| content_type_of(key.as_str()).to_owned()),
                    key: key.into_string(),
                    size: u64::try_from(stat.st_size).unwrap_or(0),
                    modified: stat.st_mtime,
                });
            }
            // A folder of it was removed meanwhile (a delete of its last
            // file): walked again.
            Err(Errno::NOENT) => {}
            Err(Errno::ISDIR | Errno::NOTEMPTY | Errno::EXIST) => return Err(Failure::KeyConflict),
            Err(e) => return Err(failure(e)),
        }
    }
    Err(Failure::Unavailable)
}

/// The folders `segments` opened from the directory, each one that is not
/// there made (mode `0750`); and the indexes of those made. A file in the
/// way is `key-conflict`.
fn make_folders(opened: &Opened, segments: &[&str]) -> Result<(Vec<OwnedFd>, Vec<usize>), Failure> {
    let mut chain: Vec<OwnedFd> = Vec::with_capacity(segments.len());
    let mut made = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let mut tries = 0;
        let fd = loop {
            let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
            match open_folder(parent, segment, opened.dev)? {
                Step::Folder(fd) => break fd,
                Step::File => return Err(Failure::KeyConflict),
                Step::Other => return Err(Failure::NotReachable(Unreachable::NotAFile)),
                Step::Missing if tries < 2 => {
                    tries += 1;
                    match rustix::fs::mkdirat(parent, *segment, Mode::from_raw_mode(FOLDER_MODE)) {
                        Ok(()) => {
                            made.push(index);
                            // The mode whatever the program's umask, on
                            // the folder just made, reached without a link.
                            if let Step::Folder(fd) = open_folder(parent, segment, opened.dev)? {
                                rustix::fs::fchmod(&fd, Mode::from_raw_mode(FOLDER_MODE))
                                    .map_err(failure)?;
                                break fd;
                            }
                        }
                        Err(Errno::EXIST) => {}
                        Err(e) => return Err(failure(e)),
                    }
                }
                Step::Missing => return Err(Failure::Unavailable),
            }
        };
        chain.push(fd);
    }
    Ok((chain, made))
}

fn delete(opened: &Opened, key: &Key, until: &Until) -> Result<(), Failure> {
    let mut segments: Vec<&str> = key.segments().collect();
    let last = segments.pop().ok_or(Failure::Unavailable)?;
    let mut chain: Vec<OwnedFd> = Vec::with_capacity(segments.len());
    for segment in &segments {
        let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
        match open_folder(parent, segment, opened.dev)? {
            Step::Folder(fd) => chain.push(fd),
            Step::Missing | Step::File | Step::Other => return Ok(()),
        }
    }
    let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
    match rustix::fs::statat(parent, last, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => {}
            FileType::Symlink => return Err(Failure::NotReachable(Unreachable::Link)),
            _ => return Err(Failure::NotReachable(Unreachable::NotAFile)),
        },
        Err(Errno::NOENT) => return Ok(()),
        Err(e) => return Err(failure(e)),
    }
    if until.over() {
        return Err(over(until));
    }
    match rustix::fs::unlinkat(parent, last, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(e) => return Err(failure(e)),
    }
    rustix::fs::fsync(parent).map_err(failure)?;
    // Each folder left empty, deepest first; the first that is not empty
    // ends it. Only an empty folder is removed, and a link in its place is
    // not (`ENOTDIR`).
    for index in (0..segments.len()).rev() {
        let holder = match index.checked_sub(1) {
            None => opened.fd.as_fd(),
            Some(before) => match chain.get(before) {
                Some(fd) => fd.as_fd(),
                None => break,
            },
        };
        let Some(segment) = segments.get(index) else {
            break;
        };
        if rustix::fs::unlinkat(holder, *segment, AtFlags::REMOVEDIR).is_err() {
            break;
        }
        rustix::fs::fsync(holder).ok();
    }
    Ok(())
}

/// One name of a folder being listed: its name, and whether it is a
/// folder; ordered by its key in a list (a folder's name with `/`).
#[derive(PartialEq, Eq)]
struct Entry {
    order: Vec<u8>,
    name: String,
    folder: bool,
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}

/// Where a folder's listing starts: entries ordered after `order`, or at
/// it when `inclusive`.
struct From {
    order: Vec<u8>,
    inclusive: bool,
}

impl From {
    fn admits(&self, order: &[u8]) -> bool {
        if self.inclusive {
            order >= self.order.as_slice()
        } else {
            order > self.order.as_slice()
        }
    }
}

/// The page being made.
struct Lister<'q> {
    query: &'q ListQuery,
    until: &'q Until,
    dev: u64,
    page: Page,
    /// The last item answered.
    last: Option<Position>,
    /// Something follows the page.
    more: bool,
}

/// The kind of a folder's entry: a regular file, a folder, or something
/// passed over (a link, a FIFO, a device).
fn kind_of(dir: BorrowedFd<'_>, name: &std::ffi::CStr, given: FileType) -> Option<bool> {
    let kind = if given == FileType::Unknown {
        let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        FileType::from_raw_mode(stat.st_mode)
    } else {
        given
    };
    match kind {
        FileType::RegularFile => Some(false),
        FileType::Directory => Some(true),
        _ => None,
    }
}

/// The `want` smallest entries of `dir` that can be a key's segment, begin
/// with `begins`, and come at or after `from`; and whether more were left.
/// Reads the folder whole once, keeping at most `want` names.
fn smallest(
    dir: BorrowedFd<'_>,
    begins: &str,
    from: Option<&From>,
    want: usize,
    until: &Until,
) -> Result<(Vec<Entry>, bool), Failure> {
    let reader = Dir::read_from(dir).map_err(failure)?;
    let mut kept: BinaryHeap<Entry> = BinaryHeap::with_capacity(want.saturating_add(1));
    let mut left = false;
    for (count, entry) in reader.enumerate() {
        if count % 1024 == 0 && until.over() {
            return Err(over(until));
        }
        let entry = entry.map_err(failure)?;
        let bytes = entry.file_name().to_bytes();
        if !bytes.starts_with(begins.as_bytes()) || !key::is_segment(bytes) {
            continue;
        }
        let Some(folder) = kind_of(dir, entry.file_name(), entry.file_type()) else {
            continue;
        };
        let mut order = bytes.to_vec();
        if folder {
            order.push(b'/');
        }
        if from.is_some_and(|f| !f.admits(&order)) {
            continue;
        }
        if kept.len() >= want && kept.peek().is_some_and(|top| order >= top.order) {
            left = true;
            continue;
        }
        let Ok(name) = String::from_utf8(bytes.to_vec()) else {
            continue;
        };
        kept.push(Entry {
            order,
            name,
            folder,
        });
        if kept.len() > want {
            kept.pop();
            left = true;
        }
    }
    Ok((kept.into_sorted_vec(), left))
}

/// Whether every key that begins with `folder` is at or before `position`.
fn passed(position: &Position, folder: &str) -> bool {
    (position.folder && folder.starts_with(&position.last))
        || (position.last.as_bytes() > folder.as_bytes() && !position.last.starts_with(folder))
}

/// Where the listing of the folder whose keys begin with `base` starts,
/// from the position a page starts after.
fn from_of(position: Option<&Position>, base: &str) -> Option<From> {
    let rest = position?.last.strip_prefix(base)?;
    let folder = position.is_some_and(|p| p.folder);
    Some(match rest.split_once('/') {
        // The folder `seg/` answered whole.
        Some((segment, "")) if folder => From {
            order: [segment.as_bytes(), b"/"].concat(),
            inclusive: false,
        },
        // Inside the folder `seg/`: it is listed from within.
        Some((segment, _)) => From {
            order: [segment.as_bytes(), b"/"].concat(),
            inclusive: true,
        },
        // The file `seg` answered.
        None => From {
            order: rest.as_bytes().to_vec(),
            inclusive: false,
        },
    })
}

fn list(opened: &Opened, query: &ListQuery, until: &Until) -> Result<Page, Failure> {
    let mut lister = Lister {
        query,
        until,
        dev: opened.dev,
        page: Page::default(),
        last: None,
        more: false,
    };
    let (folders, begins) = query.prefix.rsplit_once('/').unwrap_or(("", &query.prefix));
    let mut chain: Vec<OwnedFd> = Vec::new();
    let mut base = String::new();
    if !folders.is_empty() {
        for segment in folders.split('/') {
            let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
            match open_folder(parent, segment, opened.dev) {
                Ok(Step::Folder(fd)) => chain.push(fd),
                Ok(_) | Err(Failure::NotReachable(_)) => return Ok(Page::default()),
                Err(failure) => return Err(failure),
            }
        }
        base.push_str(folders);
        base.push('/');
    }
    let dir = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
    lister.visit(dir, &base, begins)?;
    let mut page = lister.page;
    page.next = if lister.more { lister.last } else { None };
    Ok(page)
}

impl Lister<'_> {
    fn full(&self) -> bool {
        self.page.files.len() + self.page.folders.len() >= self.query.limit
    }

    /// Lists the folder `dir`, whose keys begin with `base`, its names
    /// beginning with `begins`. Answers whether the page is done.
    fn visit(&mut self, dir: BorrowedFd<'_>, base: &str, begins: &str) -> Result<bool, Failure> {
        let mut from = from_of(self.query.after.as_ref(), base);
        loop {
            let want = self.query.limit.saturating_add(1).min(PAGE + 1);
            let (entries, left) = smallest(dir, begins, from.as_ref(), want, self.until)?;
            for entry in &entries {
                if self.until.over() {
                    return Err(over(self.until));
                }
                if self.item(dir, base, entry)? {
                    return Ok(true);
                }
            }
            match (left, entries.last()) {
                (true, Some(last)) => {
                    from = Some(From {
                        order: last.order.clone(),
                        inclusive: false,
                    });
                }
                _ => return Ok(false),
            }
        }
    }

    /// One entry of a folder. Answers whether the page is done.
    fn item(&mut self, dir: BorrowedFd<'_>, base: &str, entry: &Entry) -> Result<bool, Failure> {
        if entry.folder {
            let folder = format!("{base}{}/", entry.name);
            // No key can be inside: past the bounds of a key.
            if folder.len() >= KEY_BYTES || folder.matches('/').count() >= SEGMENTS {
                return Ok(false);
            }
            if self
                .query
                .after
                .as_ref()
                .is_some_and(|p| passed(p, &folder))
            {
                return Ok(false);
            }
            let fd = match open_folder(dir, &entry.name, self.dev) {
                Ok(Step::Folder(fd)) => fd,
                Ok(_) | Err(Failure::NotReachable(_) | Failure::Unavailable) => return Ok(false),
                Err(failure) => return Err(failure),
            };
            if self.query.delimiter {
                if !holds_a_file(fd.as_fd(), &folder, self.dev, self.until, 0)? {
                    return Ok(false);
                }
                if self.full() {
                    self.more = true;
                    return Ok(true);
                }
                self.page.folders.push(folder.clone());
                self.last = Some(Position {
                    last: folder,
                    folder: true,
                });
                return Ok(false);
            }
            return self.visit(fd.as_fd(), &folder, "");
        }
        let key = format!("{base}{}", entry.name);
        if key::check(&key).is_err() {
            return Ok(false);
        }
        if self.query.after.as_ref().is_some_and(|p| !p.before(&key)) {
            return Ok(false);
        }
        if self.full() {
            self.more = true;
            return Ok(true);
        }
        match open_file(dir, &entry.name, self.dev) {
            Ok(Some((fd, stat))) => {
                self.page
                    .files
                    .push(metadata(key.clone(), fd.as_fd(), &stat));
                self.last = Some(Position {
                    last: key,
                    folder: false,
                });
            }
            // Gone, or no longer a file: passed over.
            Ok(None) | Err(Failure::NotReachable(_) | Failure::Unavailable) => {}
            Err(failure) => return Err(failure),
        }
        Ok(false)
    }
}

/// Whether the folder `dir`, whose keys begin with `base`, holds a file
/// that can be reached and whose key is taken, at any depth.
fn holds_a_file(
    dir: BorrowedFd<'_>,
    base: &str,
    dev: u64,
    until: &Until,
    depth: usize,
) -> Result<bool, Failure> {
    if depth >= SEGMENTS {
        return Ok(false);
    }
    let reader = Dir::read_from(dir).map_err(failure)?;
    for entry in reader {
        if until.over() {
            return Err(over(until));
        }
        let entry = entry.map_err(failure)?;
        let bytes = entry.file_name().to_bytes();
        if !key::is_segment(bytes) {
            continue;
        }
        let Ok(name) = std::str::from_utf8(bytes) else {
            continue;
        };
        match kind_of(dir, entry.file_name(), entry.file_type()) {
            Some(false) => {
                if key::check(&format!("{base}{name}")).is_ok()
                    && let Ok(Some(_)) = open_file(dir, name, dev)
                {
                    return Ok(true);
                }
            }
            Some(true) => {
                let inner = format!("{base}{name}/");
                if inner.len() >= KEY_BYTES {
                    continue;
                }
                if let Ok(Step::Folder(fd)) = open_folder(dir, name, dev)
                    && holds_a_file(fd.as_fd(), &inner, dev, until, depth + 1)?
                {
                    return Ok(true);
                }
            }
            None => {}
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------- the root

/// Why a directory cannot be registered (blackbox/CONTRACT.md,
/// "Registering a Storage Resource"), in the table's order. No message
/// holds the directory's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NotConfigured,
    Outside,
    Link,
    Missing,
    NotADirectory,
    NotWritable,
    Unusable,
    InUse,
    /// The file threads had no room, or another registration held the
    /// check too long: try again.
    Busy,
}

impl Refused {
    /// The HTTP status.
    pub fn status(self) -> u16 {
        match self {
            Self::InUse => 409,
            Self::Busy => 503,
            _ => 422,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::NotConfigured => "storage-not-configured",
            Self::Outside => "outside-storage-root",
            Self::Link => "directory-link",
            Self::Missing => "directory-missing",
            Self::NotADirectory => "not-a-directory",
            Self::NotWritable => "directory-not-writable",
            Self::Unusable => "directory-unusable",
            Self::InUse => "directory-in-use",
            Self::Busy => "unavailable",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::NotConfigured => {
                "this installation has no storage root: start vibe serve with --storage-root <directory> (in the Compose file, the mount of VIBE_STORAGE_DIR), then register the Resource on a directory beneath it"
            }
            Self::Outside => {
                "the directory must be an absolute path beneath the storage root (not the root itself), written without `.`, `..` or an empty segment: choose a directory of its own beneath the storage root"
            }
            Self::Link => {
                "the directory, or a folder on its way beneath the storage root, is a symbolic link: register the real directory, beneath the storage root, without a link on its way"
            }
            Self::Missing => {
                "the directory is not there: make it on the host, beneath the storage root, and give it to the program's user (mkdir, then chown 65532:65532 in the image)"
            }
            Self::NotADirectory => {
                "the path names something that is not a directory: make a directory of its own for the Resource beneath the storage root"
            }
            Self::NotWritable => {
                "the program cannot create a file in the directory: give it to the program's user on the host (chown 65532:65532 in the image) or let that user write in it"
            }
            Self::Unusable => {
                "the directory's file system does not keep extended attributes in the user. namespace, which hold each file's content type: put the directory on a file system that does (ext4, xfs, btrfs)"
            }
            Self::InUse => {
                "the directory is another Resource's, is inside one, or holds one: choose a directory of its own for this Resource"
            }
            Self::Busy => "the platform could not check the directory now: try again shortly",
        }
    }
}

/// A registration under way: its directory, until it is registered or
/// given up.
struct Claim {
    id: u64,
    segments: Vec<String>,
    dev: u64,
    ino: u64,
}

/// The installation's storage root (`vibe serve --storage-root`): the one
/// directory beneath which every Storage Resource's directory is.
pub struct Root {
    path: PathBuf,
    segments: Vec<String>,
    /// The registrations under way. Locked on a file thread only, for the
    /// checks of one registration and its marker, never across an await.
    claims: Mutex<Vec<Claim>>,
    next_claim: std::sync::atomic::AtomicU64,
}

/// How long a registration waits for the one before it to finish its
/// checks.
const CLAIM_WAIT: Duration = Duration::from_secs(5);

/// The longest the checks of a registration may take.
const CHECK_TIME: Duration = Duration::from_secs(10);

/// What a registration found and made, until it is registered or undone.
pub struct Prepared {
    /// The directory as it is kept (`<root>/<segments>`), never printed.
    pub directory: PathBuf,
    pub opened: Arc<Opened>,
    made_marker: bool,
    made_staging: bool,
    claim: u64,
}

impl Prepared {
    /// The claim to give back ([`Root::release`]) once the Resource is in
    /// memory.
    pub fn claim(&self) -> u64 {
        self.claim
    }
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Prepared(<hidden>)")
    }
}

/// A directory of a Resource registered before, for the check that a new
/// one is not inside it or holding it.
pub struct Taken {
    pub path: PathBuf,
    pub identity: Option<(u64, u64)>,
}

impl Directory {
    /// What [`Root::prepare`] compares a new directory with.
    pub fn taken(&self) -> Taken {
        Taken {
            path: self.inner.path.clone(),
            identity: self.identity().map(|o| (o.dev, o.ino)),
        }
    }
}

/// The segments of an absolute path written without `.`, `..` or an empty
/// segment, one `/` at its end dropped; `None` otherwise.
fn segments_of(text: &str) -> Option<Vec<String>> {
    let text = text.strip_suffix('/').unwrap_or(text);
    let rest = text.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let segments: Vec<String> = rest.split('/').map(str::to_owned).collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || s == "." || s == ".." || s.contains('\0'))
    {
        return None;
    }
    Some(segments)
}

impl Root {
    /// The storage root `text`: an absolute path, else a message for the
    /// start-up failure naming `--storage-root`.
    pub fn new(text: &str) -> Result<Self, String> {
        let Some(segments) = segments_of(text) else {
            return Err(
                "--storage-root must be an absolute path written without `.`, `..` or an empty segment (such as /var/lib/vibe/storage)".to_owned(),
            );
        };
        let mut path = PathBuf::from("/");
        path.extend(&segments);
        Ok(Self {
            path,
            segments,
            claims: Mutex::new(Vec::new()),
            next_claim: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// What is wrong with the root now, if anything: `missing` or
    /// `not-a-directory` (`storage-root-unusable`). Blocking: called at
    /// start, before the program's runtime serves.
    pub fn problem(&self) -> Option<&'static str> {
        match std::fs::metadata(&self.path) {
            Ok(meta) if meta.is_dir() => None,
            Ok(_) => Some("not-a-directory"),
            Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => Some("not-a-directory"),
            Err(_) => Some("missing"),
        }
    }

    /// Checks `directory` for the Resource `name` in the table's order and,
    /// when it passes, writes its marker and `.vibe-staging/`: on a file
    /// thread. `taken` are the directories of the Resources registered. The
    /// directory is claimed until [`Root::release`].
    pub async fn prepare(
        self: &Arc<Self>,
        files: &Files,
        name: &str,
        directory: &str,
        taken: Vec<Taken>,
    ) -> Result<Prepared, Refused> {
        let root = Arc::clone(self);
        let name = name.to_owned();
        let directory = directory.to_owned();
        let answer = files
            .run(move || root.check_and_mark(&name, &directory, &taken))
            .await
            .ok_or(Refused::Busy)?;
        match tokio::time::timeout(CHECK_TIME, answer).await {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(_)) | Err(_) => Err(Refused::Busy),
        }
    }

    /// Undoes what [`Root::prepare`] made (the registration was not made):
    /// the marker and `.vibe-staging/` it wrote are removed. Releases the
    /// claim.
    pub async fn undo(self: &Arc<Self>, files: &Files, prepared: Prepared) {
        let root = Arc::clone(self);
        let claim = prepared.claim;
        let undone = files
            .run(move || {
                if prepared.made_marker {
                    rustix::fs::unlinkat(&prepared.opened.fd, MARKER, AtFlags::empty()).ok();
                }
                if prepared.made_staging {
                    rustix::fs::unlinkat(&prepared.opened.fd, STAGING, AtFlags::REMOVEDIR).ok();
                }
                rustix::fs::fsync(&prepared.opened.fd).ok();
                root.release(claim);
            })
            .await;
        match undone {
            Some(answer) => {
                tokio::time::timeout(CHECK_TIME, answer).await.ok();
            }
            None => self.release(claim),
        }
    }

    /// The registration `claim` is over (registered, or undone).
    pub fn release(&self, claim: u64) {
        self.claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|c| c.id != claim);
    }

    fn check_and_mark(
        &self,
        name: &str,
        directory: &str,
        taken: &[Taken],
    ) -> Result<Prepared, Refused> {
        let segments = segments_of(directory).ok_or(Refused::Outside)?;
        let beneath = segments
            .strip_prefix(self.segments.as_slice())
            .filter(|rest| !rest.is_empty())
            .ok_or(Refused::Outside)?
            .to_vec();
        let mut path = self.path.clone();
        path.extend(&beneath);
        let root = match rustix::fs::openat(
            CWD,
            &self.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Opened::of(fd).map_err(|_| Refused::Missing)?,
            Err(Errno::NOTDIR) => return Err(Refused::NotADirectory),
            Err(_) => return Err(Refused::Missing),
        };
        // Each component beneath the root, one at a time, never through a
        // link; the identity of each, and whether one holds a marker.
        let mut current = root;
        let mut ancestors: Vec<(u64, u64)> = vec![(current.dev, current.ino)];
        let mut marked_above = false;
        for (index, segment) in beneath.iter().enumerate() {
            match rustix::fs::statat(&current.fd, segment.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                    FileType::Symlink => return Err(Refused::Link),
                    FileType::Directory => {}
                    _ => return Err(Refused::NotADirectory),
                },
                Err(Errno::NOENT) => return Err(Refused::Missing),
                Err(Errno::NOTDIR) => return Err(Refused::NotADirectory),
                Err(_) => return Err(Refused::Missing),
            }
            let fd = match rustix::fs::openat(
                &current.fd,
                segment.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(Errno::LOOP) => return Err(Refused::Link),
                Err(Errno::NOTDIR) => return Err(Refused::NotADirectory),
                Err(_) => return Err(Refused::Missing),
            };
            current = Opened::of(fd).map_err(|_| Refused::Missing)?;
            if index + 1 < beneath.len() {
                ancestors.push((current.dev, current.ino));
                marked_above |=
                    rustix::fs::statat(&current.fd, MARKER, AtFlags::SYMLINK_NOFOLLOW).is_ok();
            }
        }
        let opened = current;
        probe(&opened)?;
        let mut claims = self.claims_within(CLAIM_WAIT)?;
        // In use: another Resource's directory, inside one, or holding one;
        // by path, by identity, and by marker.
        let identity = (opened.dev, opened.ino);
        let in_use_by_path =
            |other: &[String]| other.starts_with(&segments) || segments.starts_with(other);
        for other in taken {
            let other_segments = other
                .path
                .to_str()
                .and_then(segments_of)
                .unwrap_or_default();
            if in_use_by_path(&other_segments)
                || other
                    .identity
                    .is_some_and(|id| id == identity || ancestors.contains(&id))
            {
                return Err(Refused::InUse);
            }
        }
        if taken
            .iter()
            .filter_map(|t| t.identity)
            .any(|other| holds(&opened, other, &ancestors))
        {
            return Err(Refused::InUse);
        }
        for claim in claims.iter() {
            if in_use_by_path(&claim.segments)
                || (claim.dev, claim.ino) == identity
                || ancestors.contains(&(claim.dev, claim.ino))
            {
                return Err(Refused::InUse);
            }
        }
        if marked_above {
            return Err(Refused::InUse);
        }
        let made_marker = match read_marker(opened.fd.as_fd()) {
            Marker::Names(marked) if marked == name => false,
            Marker::Names(_) => return Err(Refused::InUse),
            Marker::None => {
                if rustix::fs::statat(&opened.fd, MARKER, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
                    // Something named so that is not the program's marker.
                    return Err(Refused::InUse);
                }
                write_marker(&opened, name)?;
                true
            }
        };
        let made_staging = match make_staging(&opened) {
            Ok(made) => made,
            Err(refused) => {
                if made_marker {
                    rustix::fs::unlinkat(&opened.fd, MARKER, AtFlags::empty()).ok();
                }
                return Err(refused);
            }
        };
        rustix::fs::fsync(&opened.fd).ok();
        let claim = self
            .next_claim
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        claims.push(Claim {
            id: claim,
            segments,
            dev: opened.dev,
            ino: opened.ino,
        });
        Ok(Prepared {
            directory: path,
            opened: Arc::new(opened),
            made_marker,
            made_staging,
            claim,
        })
    }

    /// The claims, locked: waiting at most `wait` for a registration whose
    /// checks hold them.
    fn claims_within(
        &self,
        wait: Duration,
    ) -> Result<std::sync::MutexGuard<'_, Vec<Claim>>, Refused> {
        let deadline = Instant::now() + wait;
        loop {
            match self.claims.try_lock() {
                Ok(claims) => return Ok(claims),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    return Ok(poisoned.into_inner());
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::TryLockError::WouldBlock) => return Err(Refused::Busy),
            }
        }
    }
}

/// Whether the directory `opened` holds the directory `other` (by
/// identity): walking up from `other` through `..`, at most [`SEGMENTS`]
/// levels, stopping at the root (the first of `ancestors`).
fn holds(opened: &Opened, other: (u64, u64), ancestors: &[(u64, u64)]) -> bool {
    let target = (opened.dev, opened.ino);
    let root = ancestors.first().copied();
    // The other directory is reached through its own Resource's
    // descriptor, which this function does not hold: walked from the
    // root's side instead, by searching `opened` for it.
    let _ = root;
    contains_identity(opened.fd.as_fd(), target, other, 0)
}

/// Whether the folder `dir` holds a folder of identity `wanted` (not
/// following links), at most [`SEGMENTS`] deep.
fn contains_identity(
    dir: BorrowedFd<'_>,
    me: (u64, u64),
    wanted: (u64, u64),
    depth: usize,
) -> bool {
    if me == wanted {
        return true;
    }
    if depth >= SEGMENTS {
        return false;
    }
    let Ok(reader) = Dir::read_from(dir) else {
        return false;
    };
    for entry in reader.flatten() {
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if entry.file_type() != FileType::Directory && entry.file_type() != FileType::Unknown {
            continue;
        }
        let Ok(fd) = rustix::fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            continue;
        };
        let Ok(stat) = rustix::fs::fstat(&fd) else {
            continue;
        };
        if contains_identity(fd.as_fd(), (stat.st_dev, stat.st_ino), wanted, depth + 1) {
            return true;
        }
    }
    false
}

/// Whether the program can create a file in the directory, and whether its
/// file system keeps `user.` extended attributes: shown by making a file,
/// setting one on it, and removing it.
fn probe(opened: &Opened) -> Result<(), Refused> {
    let name = format!(".vibe-probe-{}", uuid::Uuid::new_v4().simple());
    let fd = rustix::fs::openat(
        &opened.fd,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| Refused::NotWritable)?;
    let kept = rustix::fs::fsetxattr(
        &fd,
        CONTENT_TYPE_ATTRIBUTE,
        b"application/octet-stream",
        XattrFlags::empty(),
    );
    drop(fd);
    rustix::fs::unlinkat(&opened.fd, name.as_str(), AtFlags::empty()).ok();
    match kept {
        Ok(()) => Ok(()),
        Err(_) => Err(Refused::Unusable),
    }
}

fn write_marker(opened: &Opened, name: &str) -> Result<(), Refused> {
    let fd = match rustix::fs::openat(
        &opened.fd,
        MARKER,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(FILE_MODE),
    ) {
        Ok(fd) => fd,
        Err(Errno::EXIST) => return Err(Refused::InUse),
        Err(_) => return Err(Refused::NotWritable),
    };
    let mut file = std::fs::File::from(fd);
    let written = file
        .write_all(marker_of(name).as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        rustix::fs::unlinkat(&opened.fd, MARKER, AtFlags::empty()).ok();
        return Err(Refused::NotWritable);
    }
    Ok(())
}

/// Makes `.vibe-staging/` in the directory; `false` when it was there
/// already as a folder. Something else of that name (the platform's own)
/// is removed first.
fn make_staging(opened: &Opened) -> Result<bool, Refused> {
    match rustix::fs::statat(&opened.fd, STAGING, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => {
            return Ok(false);
        }
        Ok(_) => {
            rustix::fs::unlinkat(&opened.fd, STAGING, AtFlags::empty())
                .map_err(|_| Refused::NotWritable)?;
        }
        Err(_) => {}
    }
    match rustix::fs::mkdirat(&opened.fd, STAGING, Mode::from_raw_mode(FOLDER_MODE)) {
        Ok(()) => Ok(true),
        Err(Errno::EXIST) => Ok(false),
        Err(_) => Err(Refused::NotWritable),
    }
}
