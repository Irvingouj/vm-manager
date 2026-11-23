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
//! - **A get opens the file and answers its metadata and a
//!   [`Reader`](super::Reader)**: the file as it was when it was opened —
//!   a rename never changes what an open descriptor reads — each chunk a
//!   `pread` job on the file threads, ending after exactly `size` bytes; a
//!   file found shorter while it is read fails the read. A file larger
//!   than [`FILE_BYTES`](super::FILE_BYTES) is not opened.
//! - **A put** writes a new file in `.vibe-staging/` (`O_CREAT | O_EXCL`,
//!   mode `0640`, a random name) as its body arrives — one `pwrite` job a
//!   chunk — then, when the body has ended cleanly, sets the content type
//!   on it, `fsync`s it, makes the folders of the key that are missing
//!   (mode `0750`), renames it onto the key and `fsync`s the folders it
//!   changed. A reader sees the file before or after, never in part; a
//!   put that fails — the body failed, the bound, a full disk, a request
//!   that ended first — removes its staging file and lands nothing. A put
//!   racing a delete that removes one of its folders walks again (at most
//!   [`PUT_TRIES`] times).
//! - **A delete** unlinks the file, then removes each folder of the key
//!   that is left empty, deepest first.
//! - **A list** reads one folder at a time, keeping only the names that can
//!   be a key's segment and the [`PAGE`]+1 smallest after where the page
//!   starts (a folder's name taken as `name/`, which gives the byte order of
//!   whole keys), and walks depth-first. Links, FIFOs, devices and folders
//!   on another file system are passed over; with the delimiter, a folder
//!   is answered only when it holds a file that can be reached.
//! - **Opening.** Every opening of the directory, at start too, reaches it
//!   as a registration does: from the storage root the program was started
//!   with, one component at a time, following no link ([`Root`]'s walk).
//!   At start the directories are opened together, waited for
//!   [`START_WAIT`] in all; the opening job sets the state itself, so one
//!   answered late is in use as soon as it is. At most one opening of a
//!   Resource is under way at once.
//! - **Unavailable.** When the directory cannot be opened so, or its
//!   `.vibe-resource` is not there or names another Resource, every
//!   operation fails `unavailable`, nothing is written, the program's log
//!   says `storage-unavailable` at most once a minute (at once when the
//!   problem is another), and the directory is tried again at most every
//!   [`RETRY`], at an operation; `storage-available` once it can be used.
//! - **Still at its place.** At an operation, at most every [`RETRY`], one
//!   `stat` of the path (no link followed) tells whether it still names the
//!   directory held; when not, the one held is let go and the path opened
//!   again as above. Operations never follow the path.
//!
//! All file work runs on the program's file threads (crate::storage::files).
//!
//! [`Root`] is the storage root and the registration of a directory
//! beneath it (blackbox/CONTRACT.md, "Registering a Storage Resource").

use std::collections::BinaryHeap;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, Stat, XattrFlags};
use rustix::io::Errno;
use serde_json::json;

use super::files::Files;
use super::key::{self, KEY_BYTES, Key, SEGMENTS};
use super::{
    Backend, CHUNK, CONTENT_TYPE_ATTRIBUTE, CONTENT_TYPE_BYTES, FILE_BYTES, Failure, GivenUp,
    ListQuery, Metadata, OPERATION_TIME, Op, PAGE, Page, Position, Source, Unreachable, Until,
    content_type_of, is_content_type,
};
use crate::log;
use bytes::{Bytes, BytesMut};
use openworkers_runtime_v8::runtime::stream_manager::StreamChunk;
use rustix::io::{pread, pwrite};
use tokio::sync::mpsc::error::TryRecvError;

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
    OutsideStorageRoot,
    Link,
    Missing,
    NotADirectory,
    NotMarked,
    MarkedForAnother,
    NoAnswer,
}

impl Problem {
    pub fn name(self) -> &'static str {
        match self {
            Self::OutsideStorageRoot => "outside-storage-root",
            Self::Link => "link",
            Self::Missing => "missing",
            Self::NotADirectory => "not-a-directory",
            Self::NotMarked => "not-marked",
            Self::MarkedForAnother => "marked-for-another",
            Self::NoAnswer => "no-answer",
        }
    }
}

/// How long the program waits at start, in all, for the directories of
/// the Storage Resources to be opened (they are opened together).
pub const START_WAIT: Duration = Duration::from_secs(3);

enum State {
    /// Held open; `checked` is when its path was last seen to name it.
    Open {
        opened: Arc<Opened>,
        checked: Instant,
    },
    /// Not usable: `tried` is when it was last opened, `said` when
    /// `storage-unavailable` was last written (`None`: not yet).
    Closed {
        problem: Problem,
        tried: Instant,
        said: Option<Instant>,
    },
}

struct Inner {
    /// The Resource's name, which its marker must hold.
    name: String,
    /// The directory, as registered. Never printed. Used for nothing but
    /// opening it and looking whether it still names the one held.
    path: PathBuf,
    /// The storage root the program was started with: every opening walks
    /// from it.
    root: Option<Arc<Root>>,
    /// Locked only for a look or a change, never during file work or
    /// across an await.
    state: Mutex<State>,
    /// An opening is under way: never more than one at once.
    opening: AtomicBool,
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

/// The one opening of a Resource's directory under way: the flag is let go
/// when this is dropped (the job ran, or it was dropped without running).
struct Opening(Arc<Inner>);

impl Opening {
    fn take(inner: &Arc<Inner>) -> Option<Self> {
        inner
            .opening
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(Arc::clone(inner)))
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.0.opening.store(false, Ordering::Release);
    }
}

impl Directory {
    /// The Resource `name` on `path`, not opened yet: unavailable until
    /// [`Directory::opening`] has opened it. At start, and for a Resource
    /// read from the platform database.
    pub fn new(name: &str, path: &Path, root: Option<Arc<Root>>, files: &Arc<Files>) -> Self {
        Self {
            inner: Arc::new(Inner {
                name: name.to_owned(),
                path: path.to_owned(),
                root,
                state: Mutex::new(State::Closed {
                    problem: Problem::NoAnswer,
                    tried: Instant::now(),
                    said: None,
                }),
                opening: AtomicBool::new(false),
            }),
            files: Arc::clone(files),
        }
    }

    /// Opens the directory on a file thread, its staging emptied of what a
    /// crash left, and waits for it until `deadline`. The job sets the
    /// state itself, so an answer that comes later is in use as soon as it
    /// comes; one that has not come by `deadline` is `no-answer`. Owns
    /// nothing borrowed, so the openings at start run together.
    pub fn opening(
        &self,
        deadline: tokio::time::Instant,
    ) -> impl Future<Output = ()> + Send + 'static {
        let inner = Arc::clone(&self.inner);
        let files = Arc::clone(&self.files);
        async move {
            let Some(opening) = Opening::take(&inner) else {
                return;
            };
            let placed = tokio::time::timeout_at(
                deadline,
                files.run(move || {
                    let Opening(inner) = &opening;
                    let opened = open(inner.root.as_deref(), &inner.path, &inner.name);
                    inner.settle(opened);
                    drop(opening);
                }),
            )
            .await;
            let answered = match placed {
                Ok(Some(answer)) => {
                    matches!(tokio::time::timeout_at(deadline, answer).await, Ok(Ok(())))
                }
                Ok(None) | Err(_) => false,
            };
            if !answered {
                inner.no_answer();
            }
        }
    }

    /// The Resource `name`, just registered on `path`, which registration
    /// opened and marked.
    pub fn registered(
        name: &str,
        path: &Path,
        opened: Arc<Opened>,
        root: Option<Arc<Root>>,
        files: &Arc<Files>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                name: name.to_owned(),
                path: path.to_owned(),
                root,
                state: Mutex::new(State::Open {
                    opened,
                    checked: Instant::now(),
                }),
                opening: AtomicBool::new(false),
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
            State::Open { opened, .. } => Some(Arc::clone(opened)),
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

fn say_unavailable(name: &str, problem: Problem) {
    log::record(
        "storage-unavailable",
        json!({ "resource": name, "problem": problem.name() }),
    );
}

/// What [`Inner::opened`] does once it has looked at the state.
enum Next {
    Use(Arc<Opened>),
    /// Look whether the path still names the directory held.
    Check(Arc<Opened>),
    Unavailable,
    Reopen,
}

impl Inner {
    /// The directory, opened: held since it was loaded, or opened again
    /// now when it could not be used and [`RETRY`] has passed. At most
    /// every [`RETRY`] it looks (one `stat`, following no link) whether the
    /// path still names the directory held; when it does not, the one held
    /// is let go and the path opened again. On a file thread.
    fn opened(self: &Arc<Self>) -> Result<Arc<Opened>, Failure> {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match &mut *state {
                State::Open { opened, checked } if checked.elapsed() < RETRY => {
                    Next::Use(Arc::clone(opened))
                }
                // Taken by this thread: the others use the one held
                // meanwhile.
                State::Open { opened, checked } => {
                    *checked = Instant::now();
                    Next::Check(Arc::clone(opened))
                }
                State::Closed { tried, .. } if tried.elapsed() < RETRY => Next::Unavailable,
                State::Closed { .. } => Next::Reopen,
            }
        };
        match next {
            Next::Use(opened) => Ok(opened),
            Next::Check(opened) if self.still_named(&opened) => Ok(opened),
            Next::Unavailable => Err(Failure::Unavailable),
            Next::Check(_) | Next::Reopen => {
                let Some(opening) = Opening::take(self) else {
                    return Err(Failure::Unavailable);
                };
                let opened = open(self.root.as_deref(), &self.path, &self.name);
                let settled = self.settle(opened);
                drop(opening);
                settled.ok_or(Failure::Unavailable)
            }
        }
    }

    /// Whether the path still names the directory `opened`: one `stat`,
    /// not following a link in its last component.
    fn still_named(&self, opened: &Opened) -> bool {
        rustix::fs::statat(CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| (stat.st_dev, stat.st_ino) == (opened.dev, opened.ino))
    }

    /// Holds what an opening found, and says it: `storage-available` when
    /// it can be used again after `storage-unavailable` was said;
    /// `storage-unavailable` at once when it could be used before or the
    /// problem is another, else at most every [`SAY_AGAIN`].
    fn settle(&self, opened: Result<Opened, Problem>) -> Option<Arc<Opened>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        match opened {
            Ok(opened) => {
                let opened = Arc::new(opened);
                if let State::Closed { said: Some(_), .. } = &*state {
                    log::record("storage-available", json!({ "resource": &self.name }));
                }
                *state = State::Open {
                    opened: Arc::clone(&opened),
                    checked: now,
                };
                Some(opened)
            }
            Err(found) => {
                let said = match &*state {
                    State::Closed {
                        problem,
                        said: Some(said),
                        ..
                    } if *problem == found && said.elapsed() < SAY_AGAIN => *said,
                    _ => {
                        say_unavailable(&self.name, found);
                        now
                    }
                };
                *state = State::Closed {
                    problem: found,
                    tried: now,
                    said: Some(said),
                };
                None
            }
        }
    }

    /// The opening at start was not answered in time: `no-answer`, said
    /// once, unless the opening has said something meanwhile.
    fn no_answer(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let State::Closed {
            problem,
            said: said @ None,
            ..
        } = &mut *state
        {
            *problem = Problem::NoAnswer;
            *said = Some(Instant::now());
            say_unavailable(&self.name, Problem::NoAnswer);
        }
    }
}

/// Opens the directory of the Resource `name` at `path`, reached from the
/// storage root `root` one component at a time following no link, as a
/// registration reaches it; checks its marker and empties its staging of
/// what a crash left (making it if it is not there).
fn open(root: Option<&Root>, path: &Path, name: &str) -> Result<Opened, Problem> {
    let root = root.ok_or(Problem::OutsideStorageRoot)?;
    let segments = path
        .to_str()
        .and_then(segments_of)
        .ok_or(Problem::OutsideStorageRoot)?;
    let beneath = root.beneath(&segments).ok_or(Problem::OutsideStorageRoot)?;
    let walked = root.walk(beneath).map_err(|e| match e {
        Walk::Link => Problem::Link,
        Walk::NotADirectory => Problem::NotADirectory,
        Walk::Missing => Problem::Missing,
    })?;
    let opened = walked.opened;
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

/// The beginning of the name of every staging file this run of the
/// program writes: `put-<a mark made once, at the first>-`. What an
/// earlier run left has another.
fn this_run() -> &'static str {
    static RUN: OnceLock<String> = OnceLock::new();
    RUN.get_or_init(|| format!("put-{}-", uuid::Uuid::new_v4().simple()))
}

/// Removes what is in `.vibe-staging/` that an earlier run left (a crash),
/// making it when it is not there. Done each time the directory is opened;
/// the staging files of this run's puts, which may be under way, are
/// spared (their names begin with [`this_run`]).
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
        if name.starts_with(this_run().as_bytes()) {
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

    fn get(&self, key: Key, until: Until) -> Op<'_, Option<super::Opened>> {
        let files = Arc::clone(&self.files);
        Box::pin(async move {
            let found = self
                .on_files(until, move |opened| {
                    let Some((chain, last)) = folders_of(opened, &key)? else {
                        return Ok(None);
                    };
                    let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
                    let Some((fd, stat)) = open_file(parent, last, opened.dev)? else {
                        return Ok(None);
                    };
                    let metadata = metadata(key.into_string(), fd.as_fd(), &stat);
                    if metadata.size > FILE_BYTES {
                        return Err(Failure::Bytes);
                    }
                    Ok(Some((fd, metadata)))
                })
                .await?;
            let Some((fd, metadata)) = found else {
                return Ok(None);
            };
            // The file is opened; the get has ended. The reader gives the
            // file as it was when it was opened — a rename never changes
            // what an open descriptor reads — a chunk at a time.
            let size = metadata.size;
            Ok(Some(super::Opened {
                metadata,
                reader: Arc::new(FileReader {
                    file: Arc::new(fd),
                    size,
                    at: Mutex::new(0),
                    files,
                }),
            }))
        })
    }

    fn put(
        &self,
        key: Key,
        body: Source,
        content_type: Option<String>,
        mut given_up: GivenUp,
    ) -> Op<'_, Metadata> {
        Box::pin(async move {
            // The staging file, made on a file thread.
            let staged = self.open_staging().await?;
            let answer = async {
                let mut written = 0_u64;
                match body {
                    Source::Whole(bytes) => {
                        // A whole body's length is known before it is read.
                        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        if length > FILE_BYTES {
                            return Err(Failure::Bytes);
                        }
                        if *given_up.borrow_and_update() {
                            return Err(Failure::Stopped);
                        }
                        write(&staged.file, bytes, 0, &self.files).await?;
                    }
                    Source::Stream { chunks, length } => {
                        // A known length is checked before a byte is read;
                        // an unknown one as the body is written.
                        if length.is_some_and(|length| length > FILE_BYTES) {
                            return Err(Failure::Bytes);
                        }
                        let mut chunks = chunks;
                        loop {
                            // No time of its own: a source that stalls is
                            // bounded by the request's end (`given_up`), and
                            // by the request body's own rules when it is the
                            // request's.
                            let chunk = tokio::select! {
                                biased;
                                _ = given_up.wait_for(|over| *over) => {
                                    // The put is given up: nothing lands,
                                    // and the staging file is removed by
                                    // this future's own end below.
                                    return Err(Failure::Stopped);
                                }
                                chunk = chunks.recv() => chunk,
                            };
                            match chunk {
                                Some(StreamChunk::Data(first)) => {
                                    // What has already arrived is gathered,
                                    // up to one chunk's worth, before the
                                    // write: a body arriving in very small
                                    // pieces (a client's tiny frames, a
                                    // stream of a hundred bytes) costs one
                                    // write job a [`CHUNK`] rather than one
                                    // a piece. It never waits for a piece
                                    // that has not arrived: what is there
                                    // when the source has nothing ready is
                                    // written.
                                    let mut batch = first;
                                    let mut ended = false;
                                    while batch.len() < CHUNK {
                                        match chunks.try_recv() {
                                            Ok(StreamChunk::Data(more)) => {
                                                // Copied into the batch for
                                                // the one write job — at most
                                                // [`CHUNK`] bytes, once, in
                                                // place of a job a piece on
                                                // the file threads (rule 2).
                                                let mut joined = BytesMut::with_capacity(
                                                    batch.len().saturating_add(more.len()),
                                                );
                                                joined.extend_from_slice(&batch);
                                                joined.extend_from_slice(&more);
                                                batch = joined.freeze();
                                            }
                                            Ok(StreamChunk::Done) => {
                                                ended = true;
                                                break;
                                            }
                                            // A stream of the code's own
                                            // errored, or the request's body
                                            // failed under the put: nothing
                                            // lands.
                                            Ok(StreamChunk::Error(_))
                                            | Err(TryRecvError::Disconnected) => {
                                                return Err(Failure::BodyFailed);
                                            }
                                            Err(TryRecvError::Empty) => break,
                                        }
                                    }
                                    let length = u64::try_from(batch.len()).unwrap_or(u64::MAX);
                                    let at = written;
                                    written = written.checked_add(length).ok_or(Failure::Bytes)?;
                                    if written > FILE_BYTES {
                                        return Err(Failure::Bytes);
                                    }
                                    write(&staged.file, batch, at, &self.files).await?;
                                    // The body ended cleanly: the landing,
                                    // its own [`OPERATION_TIME`].
                                    if ended {
                                        break;
                                    }
                                }
                                Some(StreamChunk::Done) => break,
                                // A stream of the code's own errored, or the
                                // request's body failed under the put; the
                                // source ended without its end. Nothing
                                // lands.
                                Some(StreamChunk::Error(_)) | None => {
                                    return Err(Failure::BodyFailed);
                                }
                            }
                        }
                    }
                }
                // The landing is not taken once the put is given up; one
                // already on a file thread is told the same, and does not
                // rename (its own job removes its staging file).
                if *given_up.borrow_and_update() {
                    return Err(Failure::Stopped);
                }
                land(
                    &self.inner,
                    staged.clone(),
                    key,
                    content_type,
                    given_up.clone(),
                    &self.files,
                )
                .await
            }
            .await;
            if answer.is_err() {
                // A put that fails lands nothing and leaves nothing in
                // staging (a write in flight on the file ends harmlessly
                // into the unlinked file). A removal that fails itself is
                // what the next load of the Resource clears
                // (blackbox/CONTRACT.md, "What is on the directory"); the
                // put's own failure is the answer either way.
                let _removed = remove(&staged, &self.files).await;
            }
            answer
        })
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

/// A file in `.vibe-staging/` that a put writes into: the staging folder
/// and the file's name there (removed when the put fails), and the file
/// itself, written one `pwrite` job a chunk. Cloned cheaply for the jobs
/// that touch it.
#[derive(Clone)]
struct Staged {
    staging: Arc<OwnedFd>,
    name: String,
    file: Arc<OwnedFd>,
}

impl Directory {
    /// The staging file, opened on a file thread: `.vibe-staging/` (made
    /// when it is not there), a fresh name, `O_CREAT | O_EXCL`, mode
    /// `0640` whatever the program's umask.
    async fn open_staging(&self) -> Result<Staged, Failure> {
        let inner = Arc::clone(&self.inner);
        in_time(&self.files, move || {
            let opened = inner.opened()?;
            let staging = staging(&opened)?;
            let name = format!("{}{}", this_run(), uuid::Uuid::new_v4().simple());
            let fd = rustix::fs::openat(
                &staging,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(FILE_MODE),
            )
            .map_err(failure)?;
            // The mode whatever the program's umask.
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(FILE_MODE)).map_err(failure)?;
            Ok(Staged {
                staging: Arc::new(staging),
                name,
                file: Arc::new(fd),
            })
        })
        .await
    }
}

/// Runs `job` on a file thread, its answer waited for within
/// [`OPERATION_TIME`] in all — the place among the waiting jobs included
/// (the pool's own wait is under it).
async fn in_time<T: Send + 'static>(
    files: &Files,
    job: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    let placed = tokio::time::timeout(OPERATION_TIME, files.run(job)).await;
    let answer = match placed {
        Err(_) => return Err(Failure::Time),
        Ok(None) => return Err(Failure::Unavailable),
        Ok(Some(answer)) => answer,
    };
    match tokio::time::timeout(OPERATION_TIME, answer).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => Err(Failure::Unavailable),
        Err(_) => Err(Failure::Time),
    }
}

/// [`in_time`] for a job that must know when it is answered `limit`
/// `time`: `timed_out` is set as the answer is `Time`, so that a job still
/// running (a landing) does not do what that answer says did not happen.
async fn in_time_until<T: Send + 'static>(
    files: &Files,
    timed_out: &Arc<AtomicBool>,
    job: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    let placed = tokio::time::timeout(OPERATION_TIME, files.run(job)).await;
    let answer = match placed {
        Err(_) => return Err(Failure::Time),
        Ok(None) => return Err(Failure::Unavailable),
        Ok(Some(answer)) => answer,
    };
    match tokio::time::timeout(OPERATION_TIME, answer).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => Err(Failure::Unavailable),
        Err(_) => {
            timed_out.store(true, Ordering::SeqCst);
            Err(Failure::Time)
        }
    }
}

/// Writes `bytes` at `at` in the staging `file`: one `pwrite` job, its
/// own [`OPERATION_TIME`]. A disk that is full fails `full`.
async fn write(file: &Arc<OwnedFd>, bytes: Bytes, at: u64, files: &Files) -> Result<(), Failure> {
    let file = Arc::clone(file);
    in_time(files, move || {
        let mut written = 0_usize;
        while let Some(rest) = bytes.get(written..)
            && !rest.is_empty()
        {
            let place = at + u64::try_from(written).unwrap_or(u64::MAX);
            match pwrite(&file, rest, place) {
                // A write of nothing on a regular file is not a write.
                Ok(0) => return Err(Failure::Full),
                Ok(n) => written += n,
                Err(Errno::INTR) => {}
                Err(e) => return Err(failure(e)),
            }
        }
        Ok(())
    })
    .await
}

/// Removes the staging file `staged` names: a put that fails lands
/// nothing and leaves nothing in staging. A write still in flight on the
/// file ends harmlessly into the unlinked file.
async fn remove(staged: &Staged, files: &Files) -> Result<(), Failure> {
    let staged = staged.clone();
    in_time(files, move || {
        rustix::fs::unlinkat(&staged.staging, staged.name.as_str(), AtFlags::empty())
            .map_err(failure)
    })
    .await
}

/// Lands the staged file, one job on a file thread, its own
/// [`OPERATION_TIME`]: the content type on it, `fsync`, the folders of
/// the key that are missing made, the rename onto the key, the folders
/// `fsync`ed. Answers the metadata of the file as written.
///
/// The job is told when the put is given up (`given_up`) and when the
/// landing has passed its own time (`timed_out` is set by
/// [`in_time_until`] as it answers `limit` `time`), and looks at both
/// immediately before the rename: a landing whose answer was already
/// written `failed` does not land, and removes its staging file itself —
/// the window left is the rename system call itself.
async fn land(
    inner: &Arc<Inner>,
    staged: Staged,
    key: Key,
    content_type: Option<String>,
    given_up: GivenUp,
    files: &Files,
) -> Result<Metadata, Failure> {
    let inner = Arc::clone(inner);
    let timed_out = Arc::new(AtomicBool::new(false));
    let job_timed_out = Arc::clone(&timed_out);
    in_time_until(files, &timed_out, move || {
        land_now(&inner, staged, key, content_type, &given_up, &job_timed_out)
    })
    .await
}

fn land_now(
    inner: &Arc<Inner>,
    staged: Staged,
    key: Key,
    content_type: Option<String>,
    given_up: &GivenUp,
    timed_out: &AtomicBool,
) -> Result<Metadata, Failure> {
    let opened = inner.opened()?;
    let Staged {
        staging,
        name,
        file,
    } = staged;
    if let Some(content_type) = &content_type {
        rustix::fs::fsetxattr(
            &file,
            CONTENT_TYPE_ATTRIBUTE,
            content_type.as_bytes(),
            XattrFlags::empty(),
        )
        .map_err(failure)?;
    }
    rustix::fs::fsync(&file).map_err(failure)?;
    let mut segments: Vec<&str> = key.segments().collect();
    let last = segments.pop().ok_or(Failure::Unavailable)?;
    for _ in 0..PUT_TRIES {
        let (chain, made) = make_folders(&opened, &segments)?;
        let parent = chain.last().map_or(opened.fd.as_fd(), AsFd::as_fd);
        match rename_onto(&staging, &name, parent, last, given_up, timed_out) {
            Ok(true) => {}
            // A folder of it was removed meanwhile (a delete of its last
            // file): walked again.
            Ok(false) => {
                unmake(&opened, &chain, &segments, &made);
                continue;
            }
            Err(failed) => {
                unmake(&opened, &chain, &segments, &made);
                return Err(failed);
            }
        }
        rustix::fs::fsync(parent).map_err(failure)?;
        // Each folder made, in the folder that holds it.
        for index in made {
            if let Some(holder) = holder_of(&opened, &chain, index) {
                rustix::fs::fsync(holder).map_err(failure)?;
            }
        }
        let stat = rustix::fs::fstat(&file).map_err(failure)?;
        return Ok(Metadata {
            content_type: content_type.unwrap_or_else(|| content_type_of(key.as_str()).to_owned()),
            key: key.into_string(),
            size: u64::try_from(stat.st_size).unwrap_or(0),
            modified: stat.st_mtime,
        });
    }
    Err(Failure::Unavailable)
}

/// Renames the staged file onto `last` in `parent`: `true` once it
/// landed, `false` when a folder of its key was removed meanwhile.
///
/// A put given up, or a landing past its time, is looked at once more here,
/// immediately before the rename, and then the staging file is removed by
/// this same job: the only window left is the rename system call itself.
fn rename_onto(
    staging: &OwnedFd,
    name: &str,
    parent: BorrowedFd<'_>,
    last: &str,
    given_up: &GivenUp,
    timed_out: &AtomicBool,
) -> Result<bool, Failure> {
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
    let timed_out = timed_out.load(Ordering::SeqCst);
    if timed_out || *given_up.borrow() {
        // The put does not land: nothing is left in staging, removed here,
        // in the landing's own job (the caller's answer — `failed`
        // `request-ended` or `limit` `time` — was already written).
        rustix::fs::unlinkat(staging, name, AtFlags::empty()).ok();
        return Err(if timed_out {
            Failure::Time
        } else {
            Failure::Stopped
        });
    }
    match rustix::fs::renameat(staging, name, parent, last) {
        Ok(()) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(Errno::ISDIR | Errno::NOTEMPTY | Errno::EXIST) => Err(Failure::KeyConflict),
        Err(e) => Err(failure(e)),
    }
}

/// A file opened by a get, as the host reads its body: each read one
/// `pread` job on the file threads, at the reader's own place (never the
/// descriptor's), a chunk of at most [`CHUNK`]. Ends after exactly the
/// size the file was opened with; a file found shorter while it is read
/// fails, never ended as if whole.
struct FileReader {
    file: Arc<OwnedFd>,
    size: u64,
    at: Mutex<u64>,
    files: Arc<Files>,
}

impl super::Reader for FileReader {
    fn read(&self) -> Op<'_, super::Read> {
        let file = Arc::clone(&self.file);
        let files = Arc::clone(&self.files);
        // Taken under the lock and given back after the job answers: the
        // body's pump is the only reader, so the place is this reader's
        // alone.
        let at = *self
            .at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let size = self.size;
        Box::pin(async move {
            let room = size.saturating_sub(at);
            if room == 0 {
                // The whole file was read: its exact end.
                return Ok(super::Read::End);
            }
            let room = room.min(u64::try_from(CHUNK).unwrap_or(u64::MAX));
            let room = usize::try_from(room).unwrap_or(CHUNK);
            // One chunk, one job, its answer waited for within
            // [`OPERATION_TIME`].
            let read = in_time(&files, move || {
                let mut buffer = vec![0_u8; room];
                let mut filled = 0_usize;
                while let Some(rest) = buffer.get_mut(filled..)
                    && !rest.is_empty()
                {
                    let place = at + u64::try_from(filled).unwrap_or(u64::MAX);
                    match pread(&file, rest, place) {
                        // The file was shortened while it was read: it
                        // fails, never ended as if whole.
                        Ok(0) => return Err(Failure::Unavailable),
                        Ok(n) => filled += n,
                        Err(Errno::INTR) => {}
                        Err(e) => return Err(failure(e)),
                    }
                }
                Ok(Bytes::from(buffer))
            })
            .await;
            let step = match read {
                Ok(bytes) => {
                    // The place the next read starts from. The lock is
                    // taken again after the job, never held across the
                    // await.
                    *self
                        .at
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        at + u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    super::Read::Chunk(bytes)
                }
                // The file was found shorter while it was read, or the
                // read did not end within its time: the body fails.
                Err(_) => super::Read::Failed,
            };
            Ok(step)
        })
    }
}

/// The folder that holds the folder `index` of `chain`: the directory for
/// the first.
fn holder_of<'a>(opened: &'a Opened, chain: &'a [OwnedFd], index: usize) -> Option<BorrowedFd<'a>> {
    match index.checked_sub(1) {
        None => Some(opened.fd.as_fd()),
        Some(before) => chain.get(before).map(AsFd::as_fd),
    }
}

/// Removes the folders a put made (`made`, indexes of `segments` and of
/// `chain`) when the put failed: deepest first, each only when empty. One
/// already gone (`ENOENT`) is passed; it stops at the first that is there
/// and does not go.
fn unmake(opened: &Opened, chain: &[OwnedFd], segments: &[&str], made: &[usize]) {
    for &index in made.iter().rev() {
        let (Some(holder), Some(segment)) = (holder_of(opened, chain, index), segments.get(index))
        else {
            return;
        };
        match rustix::fs::unlinkat(holder, *segment, AtFlags::REMOVEDIR) {
            Ok(()) => {
                rustix::fs::fsync(holder).ok();
            }
            Err(Errno::NOENT) => {}
            Err(_) => return,
        }
    }
}

/// The folders `segments` opened from the directory, each one that is not
/// there made (mode `0750`); and the indexes of those made. A file in the
/// way is `key-conflict`. When it fails, the folders it made are removed.
fn make_folders(opened: &Opened, segments: &[&str]) -> Result<(Vec<OwnedFd>, Vec<usize>), Failure> {
    let mut chain: Vec<OwnedFd> = Vec::with_capacity(segments.len());
    let mut made = Vec::new();
    match make_folders_into(opened, segments, &mut chain, &mut made) {
        Ok(()) => Ok((chain, made)),
        Err(failure) => {
            unmake(opened, &chain, segments, &made);
            Err(failure)
        }
    }
}

fn make_folders_into(
    opened: &Opened,
    segments: &[&str],
    chain: &mut Vec<OwnedFd>,
    made: &mut Vec<usize>,
) -> Result<(), Failure> {
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
                            // Once: made again after a delete removed it
                            // meanwhile is the same folder of this put.
                            if made.last() != Some(&index) {
                                made.push(index);
                            }
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
    Ok(())
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
    name: String,
    folder: bool,
}

/// The bytes a name is ordered by in a list: a folder's name with `/`.
fn order_of(name: &[u8], folder: bool) -> impl Iterator<Item = &u8> {
    name.iter().chain(folder.then_some(&b'/'))
}

impl Entry {
    fn order(&self) -> impl Iterator<Item = &u8> {
        order_of(self.name.as_bytes(), self.folder)
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order().cmp(other.order())
    }
}

/// Where a folder's listing starts: entries ordered after `order`, or at
/// it when `inclusive`.
struct From {
    order: Vec<u8>,
    inclusive: bool,
}

impl From {
    fn admits<'o>(&self, order: impl Iterator<Item = &'o u8>) -> bool {
        let at = order.copied().cmp(self.order.iter().copied());
        if self.inclusive {
            at.is_ge()
        } else {
            at.is_gt()
        }
    }
}

/// The page being made.
struct Lister<'q> {
    query: &'q ListQuery,
    until: &'q Until,
    dev: u64,
    page: Page,
    /// The last name passed, answered or not (a folder passed whole is
    /// `folder: true`): where the next page starts.
    passed: Option<Position>,
    /// When the list stops looking and answers what it has
    /// ([`super::LOOK_TIME`] from its start).
    looking: Instant,
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
        if from.is_some_and(|f| !f.admits(order_of(bytes, folder))) {
            continue;
        }
        if kept.len() >= want
            && kept
                .peek()
                .is_some_and(|top| order_of(bytes, folder).cmp(top.order()).is_ge())
        {
            left = true;
            continue;
        }
        let Ok(name) = String::from_utf8(bytes.to_vec()) else {
            continue;
        };
        kept.push(Entry { name, folder });
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
        passed: None,
        looking: Instant::now() + super::LOOK_TIME,
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
    page.next = if lister.more {
        lister.passed.or_else(|| query.after.clone())
    } else {
        None
    };
    Ok(page)
}

impl Lister<'_> {
    fn full(&self) -> bool {
        self.page.files.len() + self.page.folders.len() >= self.query.limit
    }

    /// `last` (a key, or a folder with `/` when `folder`) was passed.
    fn pass(&mut self, last: String, folder: bool) {
        self.passed = Some(Position { last, folder });
    }

    /// Lists the folder `dir`, whose keys begin with `base`, its names
    /// beginning with `begins`. Answers whether the page is done. Each
    /// pass over the folder keeps the [`PAGE`]+1 smallest names left,
    /// whatever the page's `limit`, so names passed over cost no more
    /// than names answered.
    fn visit(&mut self, dir: BorrowedFd<'_>, base: &str, begins: &str) -> Result<bool, Failure> {
        let mut from = from_of(self.query.after.as_ref(), base);
        loop {
            let (entries, left) = smallest(dir, begins, from.as_ref(), PAGE + 1, self.until)?;
            for entry in &entries {
                if self.until.over() {
                    return Err(over(self.until));
                }
                // Looked long enough: what it has, with where it got to
                // (once it got somewhere).
                if self.passed.is_some() && Instant::now() >= self.looking {
                    self.more = true;
                    return Ok(true);
                }
                if self.item(dir, base, entry)? {
                    return Ok(true);
                }
            }
            match (left, entries.last()) {
                (true, Some(last)) => {
                    from = Some(From {
                        order: last.order().copied().collect(),
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
            if self
                .query
                .after
                .as_ref()
                .is_some_and(|p| passed(p, &folder))
            {
                return Ok(false);
            }
            // No key can be inside: past the bounds of a key.
            if folder.len() >= KEY_BYTES || folder.matches('/').count() >= SEGMENTS {
                self.pass(folder, true);
                return Ok(false);
            }
            let fd = match open_folder(dir, &entry.name, self.dev) {
                Ok(Step::Folder(fd)) => fd,
                Ok(_) | Err(Failure::NotReachable(_) | Failure::Unavailable) => {
                    self.pass(folder, true);
                    return Ok(false);
                }
                Err(failure) => return Err(failure),
            };
            if self.query.delimiter {
                if !holds_a_file(fd.as_fd(), &folder, self.dev, self.until, 0)? {
                    self.pass(folder, true);
                    return Ok(false);
                }
                if self.full() {
                    self.more = true;
                    return Ok(true);
                }
                self.page.folders.push(folder.clone());
                self.pass(folder, true);
                return Ok(false);
            }
            if self.visit(fd.as_fd(), &folder, "")? {
                return Ok(true);
            }
            self.pass(folder, true);
            return Ok(false);
        }
        let key = format!("{base}{}", entry.name);
        if self.query.after.as_ref().is_some_and(|p| !p.before(&key)) {
            return Ok(false);
        }
        if key::check(&key).is_err() {
            self.pass(key, false);
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
            }
            // Gone, or no longer a file: passed over.
            Ok(None) | Err(Failure::NotReachable(_) | Failure::Unavailable) => {}
            Err(failure) => return Err(failure),
        }
        self.pass(key, false);
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

/// The most registrations claimed at once: those under way, and those
/// whose insert may still land and could not be read back (kept until the
/// program stops). When it is full a registration is refused `503`
/// `unavailable` ([`Refused::Busy`]), and nothing is made.
pub const CLAIMS: usize = 64;

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
    /// Its directory, while it is open.
    pub opened: Option<Arc<Opened>>,
}

impl Taken {
    fn identity(&self) -> Option<(u64, u64)> {
        self.opened.as_ref().map(|o| (o.dev, o.ino))
    }
}

impl Directory {
    /// What [`Root::prepare`] compares a new directory with.
    pub fn taken(&self) -> Taken {
        Taken {
            path: self.inner.path.clone(),
            opened: self.identity(),
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

/// Why a walk from the root did not reach a directory.
enum Walk {
    Link,
    Missing,
    NotADirectory,
}

/// A directory reached from the root: opened, the identities of the root
/// and of each folder on the way, and whether one of those folders holds a
/// marker.
struct Walked {
    opened: Opened,
    ancestors: Vec<(u64, u64)>,
    marked_above: bool,
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

    /// The storage root `path`, as a path (never through a string that
    /// could alter it): an absolute path written without `.`, `..` or an
    /// empty segment, whose segments are UTF-8 as a Resource's directory
    /// is named on the API, else a message. For the local run's own root.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("the storage root must be an absolute path".to_owned());
        }
        let mut segments = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(segment) => match segment.to_str() {
                    Some(segment) => segments.push(segment.to_owned()),
                    None => {
                        return Err(
                            "the storage root's path is not valid UTF-8, which a Resource's directory must be named in"
                                .to_owned(),
                        );
                    }
                },
                // `.`, `..` and a Windows prefix are not a storage root.
                _ => {
                    return Err(
                        "the storage root must be an absolute path written without `.` or `..`"
                            .to_owned(),
                    );
                }
            }
        }
        let mut root = PathBuf::from("/");
        root.extend(&segments);
        Ok(Self {
            path: root,
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
    /// thread. `taken` reads the directories of the Resources registered;
    /// it is called under the claims' lock, inside the checks, so that a
    /// registration checked second sees the first, registered (in memory)
    /// or still claimed. The directory is claimed until [`Root::release`]
    /// or [`Root::undo`]. A registration nobody waits for any more (its
    /// checks took more than [`CHECK_TIME`]) writes nothing, or undoes what
    /// it wrote and gives its claim back.
    pub async fn prepare(
        self: &Arc<Self>,
        files: &Files,
        name: &str,
        directory: &str,
        taken: impl Fn() -> Vec<Taken> + Send + 'static,
    ) -> Result<Prepared, Refused> {
        let root = Arc::clone(self);
        let undoing = Arc::clone(self);
        let name = name.to_owned();
        let directory = directory.to_owned();
        let deadline = Instant::now() + CHECK_TIME;
        let until = Until::new(deadline);
        let mut answer = files
            .run_or_undo(
                move || root.check_and_mark(&name, &directory, &taken, &until),
                move |answer| {
                    if let Ok(prepared) = answer {
                        undoing.undo_now(prepared);
                    }
                },
            )
            .await
            .ok_or(Refused::Busy)?;
        let waited =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut answer).await;
        match waited {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(_)) => Err(Refused::Busy),
            Err(_) => {
                // Not waited for any more: an answer sent meanwhile is
                // undone here; one sent after this is undone by the job.
                answer.close();
                if let Ok(Ok(prepared)) = answer.try_recv() {
                    self.undo(files, prepared).await;
                }
                Err(Refused::Busy)
            }
        }
    }

    /// Undoes what [`Root::prepare`] made (the registration was not made):
    /// the marker and `.vibe-staging/` it wrote are removed. Releases the
    /// claim.
    pub async fn undo(self: &Arc<Self>, files: &Files, prepared: Prepared) {
        let root = Arc::clone(self);
        let claim = prepared.claim;
        let undone = files.run(move || root.undo_now(prepared)).await;
        match undone {
            Some(answer) => {
                tokio::time::timeout(CHECK_TIME, answer).await.ok();
            }
            None => self.release(claim),
        }
    }

    /// [`Root::undo`], on a file thread.
    fn undo_now(&self, prepared: Prepared) {
        let Prepared {
            opened,
            made_marker,
            made_staging,
            claim,
            ..
        } = prepared;
        if made_marker {
            rustix::fs::unlinkat(&opened.fd, MARKER, AtFlags::empty()).ok();
        }
        if made_staging {
            rustix::fs::unlinkat(&opened.fd, STAGING, AtFlags::REMOVEDIR).ok();
        }
        rustix::fs::fsync(&opened.fd).ok();
        self.release(claim);
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
        taken: &dyn Fn() -> Vec<Taken>,
        until: &Until,
    ) -> Result<Prepared, Refused> {
        let segments = segments_of(directory).ok_or(Refused::Outside)?;
        let beneath = self.beneath(&segments).ok_or(Refused::Outside)?;
        let mut path = self.path.clone();
        path.extend(beneath);
        let Walked {
            opened,
            ancestors,
            marked_above,
        } = self.walk(beneath).map_err(|e| match e {
            Walk::Link => Refused::Link,
            Walk::NotADirectory => Refused::NotADirectory,
            Walk::Missing => Refused::Missing,
        })?;
        probe(&opened)?;
        let mut claims = self.claims_within(CLAIM_WAIT)?;
        if claims.len() >= CLAIMS {
            return Err(Refused::Busy);
        }
        // Read now, under the claims' lock: a registration whose claim was
        // given back is in memory by then.
        let taken = taken();
        // In use: another Resource's directory, inside one, or holding one;
        // by path, by identity, and by marker.
        let identity = (opened.dev, opened.ino);
        let in_use_by_path =
            |other: &[String]| other.starts_with(&segments) || segments.starts_with(other);
        for other in &taken {
            let other_segments = other
                .path
                .to_str()
                .and_then(segments_of)
                .unwrap_or_default();
            if in_use_by_path(&other_segments)
                || other
                    .identity()
                    .is_some_and(|id| id == identity || ancestors.contains(&id))
            {
                return Err(Refused::InUse);
            }
        }
        let root_identity = ancestors.first().copied().unwrap_or((0, 0));
        if taken
            .iter()
            .filter_map(|t| t.opened.as_deref())
            .any(|other| holds(&opened, other, root_identity))
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
        // Nobody waits for it any more: nothing is written.
        if until.over() {
            return Err(Refused::Busy);
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
        if until.over() {
            if made_marker {
                rustix::fs::unlinkat(&opened.fd, MARKER, AtFlags::empty()).ok();
            }
            if made_staging {
                rustix::fs::unlinkat(&opened.fd, STAGING, AtFlags::REMOVEDIR).ok();
            }
            return Err(Refused::Busy);
        }
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

    /// The segments of `segments` (an absolute path's) beneath the root,
    /// when it is beneath it and not the root itself.
    fn beneath<'s>(&self, segments: &'s [String]) -> Option<&'s [String]> {
        segments
            .strip_prefix(self.segments.as_slice())
            .filter(|rest| !rest.is_empty())
    }

    /// The directory `beneath` the root, reached from the root one
    /// component at a time, never through a link: as a registration checks
    /// it and as every opening of a Resource's directory reaches it.
    fn walk(&self, beneath: &[String]) -> Result<Walked, Walk> {
        let root = match rustix::fs::openat(
            CWD,
            &self.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Opened::of(fd).map_err(|_| Walk::Missing)?,
            Err(Errno::NOTDIR) => return Err(Walk::NotADirectory),
            Err(_) => return Err(Walk::Missing),
        };
        let mut current = root;
        let mut ancestors: Vec<(u64, u64)> = vec![(current.dev, current.ino)];
        let mut marked_above = false;
        for (index, segment) in beneath.iter().enumerate() {
            match rustix::fs::statat(&current.fd, segment.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                    FileType::Symlink => return Err(Walk::Link),
                    FileType::Directory => {}
                    _ => return Err(Walk::NotADirectory),
                },
                Err(Errno::NOTDIR) => return Err(Walk::NotADirectory),
                Err(_) => return Err(Walk::Missing),
            }
            let fd = match rustix::fs::openat(
                &current.fd,
                segment.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(Errno::LOOP) => return Err(Walk::Link),
                Err(Errno::NOTDIR) => return Err(Walk::NotADirectory),
                Err(_) => return Err(Walk::Missing),
            };
            current = Opened::of(fd).map_err(|_| Walk::Missing)?;
            if index + 1 < beneath.len() {
                ancestors.push((current.dev, current.ino));
                marked_above |=
                    rustix::fs::statat(&current.fd, MARKER, AtFlags::SYMLINK_NOFOLLOW).is_ok();
            }
        }
        Ok(Walked {
            opened: current,
            ancestors,
            marked_above,
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

/// Whether the directory `opened` holds the directory `other` (another
/// Resource's, held open): walking up from `other` through `..`, at most
/// [`SEGMENTS`] levels beneath the storage root, stopping at the root
/// (`root`) or at `/`.
fn holds(opened: &Opened, other: &Opened, root: (u64, u64)) -> bool {
    let target = (opened.dev, opened.ino);
    let Ok(mut current) = rustix::fs::openat(
        &other.fd,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    let mut here = (other.dev, other.ino);
    for _ in 0..=SEGMENTS {
        if here == target {
            return true;
        }
        if here == root {
            return false;
        }
        let Ok(up) = rustix::fs::openat(
            &current,
            "..",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return false;
        };
        let Ok(stat) = rustix::fs::fstat(&up) else {
            return false;
        };
        let above = (stat.st_dev, stat.st_ino);
        // `/`, whose `..` is itself.
        if above == here {
            return false;
        }
        here = above;
        current = up;
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
