//! Storage (ADR 0019; blackbox/CONTRACT.md, "Storage"): a Resource that is a
//! named place of files, reached through one interface whatever holds the
//! files.
//!
//! Interface:
//!
//! - [`Backend`], the five operations every backend keeps (`head`, `get`,
//!   `put`, `delete`, `list`), each on a [`Key`] the host has checked,
//!   within an [`Until`]. The first backend is a directory
//!   ([`directory::Directory`]); an S3 backend is a second implementation of
//!   this trait, not a second feature.
//! - [`key`]: the rules of a key, the same for every backend, and how a key
//!   is written in the Operation log.
//! - [`Source`] and [`Reader`]: a body as the interface carries it — a
//!   put's body is chunks as they arrive (a stream in is what S3 offers:
//!   a multipart upload), and a get answers the file's bytes as chunks
//!   (a response body), so no backend is asked to hold a file whole.
//!   [`bodies`]: the bounds on bodies open at once, 16 a request, 128 an
//!   Application and 512 the program. [`content_type_of`] and
//!   [`is_content_type`].
//! - [`cursor`]: a list's cursor, sealed so that only one this program
//!   answered for the same Resource, prefix and delimiter is taken.
//! - [`Root`] (crate::storage::directory): the installation's storage root
//!   and the registration of a directory beneath it.
//!
//! What is the host's (crate::gateway::storage) and not a backend's: the
//! arguments, who is allowed, the key's rules, the bounds of a body, the
//! Operation log, the 10 seconds of an operation.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use openworkers_runtime_v8::runtime::stream_manager::StreamChunk;
use tokio::sync::{mpsc, watch};

pub mod bodies;
pub mod cursor;
pub mod directory;
pub mod files;
pub mod key;

pub use bodies::{Bodies, Guard, Open};
pub use directory::{Directory, Root};
pub use files::Files;
pub use key::Key;

/// The most bytes a put's body or a get's file has: 5 GiB (5,368,709,120
/// bytes), exactly that taken. Past it, `limit` `bytes`.
pub const FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// A chunk of a body, fed to the code or written to the staging file: at
/// most 64 KiB (blackbox/CONTRACT.md, "Storage: Bounds"); exactly that is
/// taken.
pub const CHUNK: usize = 64 * 1024;

/// The time an operation has from the moment it is decided (head, get,
/// delete, list; a put ends with its request instead, its one writes and
/// its landing each having their own [`OPERATION_TIME`]).
pub const OPERATION_TIME: Duration = Duration::from_secs(10);

/// What the code's read of a body sees when the file it is reading
/// failed: the stream fails with these words — a file found shorter than
/// it was when it was opened is never ended as if whole.
pub const BODY_FAILED: &str =
    "the file's body failed: it is shorter than it was when it was opened, or could not be read";

/// How long a list looks before it answers what it has, with the cursor
/// of where it got to: a page may hold fewer than `limit`, even nothing.
pub const LOOK_TIME: Duration = Duration::from_secs(5);

/// The most files and folders one list answers, and how many without
/// `limit`; also how many names one pass over a folder keeps.
pub const PAGE: usize = 1000;
pub const DEFAULT_PAGE: usize = 100;

/// The most bytes of a content type given.
pub const CONTENT_TYPE_BYTES: usize = 255;

/// The content type of a file of no known kind.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// A file's metadata, as every answer gives it.
#[derive(Debug)]
pub struct Metadata {
    pub key: String,
    pub size: u64,
    pub content_type: String,
    /// Seconds since the Unix epoch: the time of the file's last change.
    pub modified: i64,
}

/// A file opened by a get, as the host reads its bytes: its metadata,
/// and a [`Reader`] the host feeds the code's stream from. The reader is
/// the file as it was when it was opened.
pub struct Opened {
    pub metadata: Metadata,
    pub reader: Arc<dyn Reader>,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

/// One step of reading a get's body: the next chunk, its whole end, or
/// its failure.
#[derive(Debug)]
pub enum Read {
    /// The next bytes: at most [`CHUNK`].
    Chunk(Bytes),
    /// The body's end, after exactly the size the file was opened with.
    End,
    /// The body failed — the file was found shorter while it was read,
    /// or could not be read: the stream the code reads fails with
    /// [`BODY_FAILED`], never ended as if whole.
    Failed,
}

/// A file opened by a get, as the host reads its body: one chunk at a
/// time, each read a job on the file threads (a response body for S3
/// later, fetched as it is read). See [`Read`] for what a read answers.
pub trait Reader: Send + Sync {
    fn read(&self) -> Op<'_, Read>;
}

/// Where a list goes on from: after `last`, and when `folder`, after
/// every key that begins with `last` (a folder answered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub last: String,
    pub folder: bool,
}

impl Position {
    /// Whether `name` (a key, or a folder ending with `/`) comes after
    /// this position: in byte order after `last`, and not inside the
    /// folder `last` when it is one.
    pub fn before(&self, name: &str) -> bool {
        name.as_bytes() > self.last.as_bytes() && !(self.folder && name.starts_with(&self.last))
    }
}

/// What a list asks for: the keys that begin with `prefix`, with the
/// delimiter `/` or without, after `after`, at most `limit`.
#[derive(Debug)]
pub struct ListQuery {
    pub prefix: String,
    pub delimiter: bool,
    pub after: Option<Position>,
    pub limit: usize,
}

/// One page of a list.
#[derive(Debug, Default)]
pub struct Page {
    pub files: Vec<Metadata>,
    pub folders: Vec<String>,
    /// Where the next page starts (the last name passed, answered or
    /// not); `None` only when nothing follows.
    pub next: Option<Position>,
}

/// Why something on the way to a key cannot be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreachable {
    /// The key, or a folder on its way, is a symbolic link.
    Link,
    /// The key is something other than a regular file.
    NotAFile,
    /// A folder on its way is on another file system than the Resource's.
    OtherFilesystem,
}

impl Unreachable {
    pub fn name(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::NotAFile => "not-a-file",
            Self::OtherFilesystem => "other-filesystem",
        }
    }
}

/// How an operation failed after it was allowed and its key taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A body, or a file to get, is larger than [`FILE_BYTES`].
    Bytes,
    /// The operation did not end within its time.
    Time,
    /// A put's body failed before its end: a stream of the code's own
    /// errored, or the request's body failed while the put read it (past
    /// its bound, no byte for 10 seconds, the client gone).
    BodyFailed,
    /// A put where a file is a folder of the key, or a folder is the key.
    KeyConflict,
    NotReachable(Unreachable),
    /// The disk or the quota is full.
    Full,
    /// The place of files cannot be used now, any other error of it, or no
    /// room for the operation now.
    Unavailable,
    /// Given up: its request ended or its Application was disabled. Looked
    /// at before a put's landing step, and by the put's own body loop; a
    /// put given up while its last system calls run (the rename, the
    /// `fsync`s) may have landed, whole, as one past its time may.
    Stopped,
}

/// The bound of one operation: its deadline, and whether the host gave it
/// up. A backend looks at it before it changes anything, and a put once
/// more before it lands, so that an operation answered `limit: time` (or
/// stopped by a disable) changes nothing it had not changed then. A system
/// call under way is not stopped.
#[derive(Clone, Debug)]
pub struct Until {
    at: std::time::Instant,
    given_up: Arc<AtomicBool>,
}

impl Until {
    pub fn new(at: std::time::Instant) -> Self {
        Self {
            at,
            given_up: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the operation is past its time or given up.
    pub fn over(&self) -> bool {
        self.given_up.load(Ordering::SeqCst) || std::time::Instant::now() >= self.at
    }

    /// The host has stopped waiting for it.
    pub fn give_up(&self) {
        self.given_up.store(true, Ordering::SeqCst);
    }

    /// The deadline, for the host's wait.
    pub fn deadline(&self) -> std::time::Instant {
        self.at
    }
}

/// What a backend's operation answers.
pub type Op<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// A place of files: the five operations every backend keeps (ADR 0019,
/// "One interface, five operations, flat keys"). Keys are checked by the
/// host before they reach a backend; a backend answers only what the
/// table of failures says, and nothing of where it keeps the files.
///
/// A body crosses as chunks, never whole: a put's [`Source`] arrives as it
/// is read and the file lands only when it has ended cleanly; a get
/// answers an [`Opened`] whose [`Reader`] gives the file's bytes to the
/// host a chunk at a time. Nothing in the trait names a directory, so an
/// S3 backend keeps it (a put of unknown length a multipart upload, a
/// get a response body).
pub trait Backend: Send + Sync {
    /// The file's metadata, or `None` when no file has the key. `application`
    /// is whose operation it is: its file jobs count against its share.
    fn head<'a>(
        &'a self,
        key: Key,
        until: Until,
        application: &'a Arc<str>,
    ) -> Op<'a, Option<Metadata>>;

    /// The file opened — its metadata and a [`Reader`] of its bytes, the
    /// file as it was when it was opened, ending after exactly `size`
    /// bytes — or `None`. A file larger than [`FILE_BYTES`] is not opened
    /// ([`Failure::Bytes`]); its `head` still answers.
    fn get<'a>(
        &'a self,
        key: Key,
        until: Until,
        application: &'a Arc<str>,
    ) -> Op<'a, Option<Opened>>;

    /// Creates the file or replaces it whole, atomically; `content_type`
    /// kept when given. The body is written to the staging file as it
    /// arrives; the file lands — fsync, rename, folders — only when the
    /// body has ended cleanly, and answers the metadata of the file as
    /// written.
    ///
    /// A put is part of its request and ends with it: `given_up` is the
    /// request's end, and a put that sees it fired ends its body itself —
    /// staging removed, nothing landed — so the caller never has to drop
    /// a put mid-flight (a write already on a file thread ends, the
    /// landing in flight may land whole, a landing not started is not
    /// taken). One write and the landing each have
    /// [`OPERATION_TIME`] of their own, and a source that stalls is
    /// bounded by `given_up`, not by a time of its own.
    fn put<'a>(
        &'a self,
        key: Key,
        body: Source,
        content_type: Option<String>,
        given_up: GivenUp,
        application: &'a Arc<str>,
    ) -> Op<'a, Metadata>;

    /// Removes the file if there is one, and every folder it leaves empty.
    fn delete<'a>(&'a self, key: Key, until: Until, application: &'a Arc<str>) -> Op<'a, ()>;

    /// A page of the keys beginning with the query's prefix, in the byte
    /// order of their UTF-8.
    fn list<'a>(
        &'a self,
        query: ListQuery,
        until: Until,
        application: &'a Arc<str>,
    ) -> Op<'a, Page>;
}

/// A put's body, as a backend takes it: the bytes in hand (a string as
/// UTF-8), or chunks as they arrive — the request's own body, another
/// file's, or a stream the code pumps — with the body's length when it
/// is known before it is read.
#[derive(Debug)]
pub enum Source {
    Whole(Bytes),
    Stream {
        chunks: mpsc::Receiver<StreamChunk>,
        length: Option<u64>,
    },
}

/// What ends a put when its request does: fires once, stays fired (the
/// request's end, from the backend's point of view; a disable ends the
/// request too). See [`Backend::put`].
pub type GivenUp = watch::Receiver<bool>;

/// Whether `text` can be a content type given to a put: 1 to 255 bytes of
/// printable ASCII (U+0020 to U+007E) holding a `/`.
pub fn is_content_type(text: &str) -> bool {
    (1..=CONTENT_TYPE_BYTES).contains(&text.len())
        && text.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && text.contains('/')
}

/// The content type of a file without one kept: by its key's last
/// extension, in any letter case; [`OCTET_STREAM`] for any other.
pub fn content_type_of(key: &str) -> &'static str {
    let name = key.rsplit('/').next().unwrap_or(key);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return OCTET_STREAM;
    };
    match extension.to_ascii_lowercase().as_str() {
        "txt" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => OCTET_STREAM,
    }
}

/// The extended attribute a file's content type is kept in.
pub const CONTENT_TYPE_ATTRIBUTE: &str = "user.vibe.content-type";

/// A Storage Resource's location as the platform database keeps it, in the
/// column of a database's connection. Never printed: its `Debug` shows
/// nothing, and no answer, log record or error holds it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum Location {
    Directory { directory: String },
}

impl std::fmt::Debug for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Location(<hidden>)")
    }
}

/// What the program needs to reach Storage Resources: the file threads,
/// the program's places for bodies open at once, the key of cursors, and
/// the storage root (when `--storage-root` was given). One for the program.
pub struct Storage {
    pub files: Arc<Files>,
    pub open: Arc<Open>,
    pub cursors: cursor::Sealer,
    pub root: Option<Arc<Root>>,
}
