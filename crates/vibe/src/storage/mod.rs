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
//! - [`Body`], a put's body as the raw interface carries it (base64 text),
//!   looked at without being decoded; [`Bodies`], the program's one budget
//!   of bodies held at once; [`content_type_of`] and [`is_content_type`].
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

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub mod cursor;
pub mod directory;
pub mod files;
pub mod key;

pub use directory::{Directory, Root};
pub use files::Files;
pub use key::Key;

/// The most bytes a put's body or a get's file has: 8 MiB, exactly that
/// taken.
pub const BODY_BYTES: usize = 8 * 1024 * 1024;

/// The length of the base64 text (with padding) of [`BODY_BYTES`]: a text
/// longer than this is past the bound whatever it holds, and is not looked
/// at.
pub const BODY_TEXT: usize = BODY_BYTES.div_ceil(3) * 4;

/// The most bytes of bodies storage operations hold at once in the
/// program.
pub const BODIES_AT_ONCE: usize = 256 * 1024 * 1024;

/// How long an operation waits for room among [`BODIES_AT_ONCE`].
pub const ROOM_WAIT: Duration = Duration::from_secs(5);

/// The time an operation has from the moment it is decided.
pub const OPERATION_TIME: Duration = Duration::from_secs(10);

/// The most files and folders one list answers, and how many without
/// `limit`.
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

/// A file read whole.
#[derive(Debug)]
pub struct Got {
    pub metadata: Metadata,
    /// Its bytes in standard base64 with padding: the only form the raw
    /// interface carries a body in, made where the bytes were read so that
    /// no async worker encodes 8 MiB.
    pub body: String,
    /// The room its body holds among [`BODIES_AT_ONCE`], until the answer is
    /// handed on.
    pub room: Room,
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
    /// Where the next page starts; `None` when nothing follows.
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
    /// A body, or a file to get, is larger than [`BODY_BYTES`].
    Bytes,
    /// The operation did not end within its time.
    Time,
    /// A put where a file is a folder of the key, or a folder is the key.
    KeyConflict,
    NotReachable(Unreachable),
    /// The disk or the quota is full.
    Full,
    /// The place of files cannot be used now, any other error of it, or no
    /// room for the operation now.
    Unavailable,
    /// Given up by the host (a disable) before it landed: nothing was done.
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
pub trait Backend: Send + Sync {
    /// The file's metadata, or `None` when no file has the key.
    fn head(&self, key: Key, until: Until) -> Op<'_, Option<Metadata>>;

    /// The file read whole, one version of it, or `None`. A file larger
    /// than [`BODY_BYTES`] is not read ([`Failure::Bytes`]); room for its
    /// body is taken from `bodies` before it is read.
    fn get(&self, key: Key, bodies: Arc<Bodies>, until: Until) -> Op<'_, Option<Got>>;

    /// Creates the file or replaces it whole, atomically; `content_type`
    /// kept when given. Answers the metadata of the file as written.
    fn put(
        &self,
        key: Key,
        body: Body,
        content_type: Option<String>,
        until: Until,
    ) -> Op<'_, Metadata>;

    /// Removes the file if there is one, and every folder it leaves empty.
    fn delete(&self, key: Key, until: Until) -> Op<'_, ()>;

    /// A page of the keys beginning with the query's prefix, in the byte
    /// order of their UTF-8.
    fn list(&self, query: ListQuery, until: Until) -> Op<'_, Page>;
}

/// The engine the raw interface's bodies are read with: standard base64,
/// with or without padding, no bits left over.
const ENGINE: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Standard base64 with padding, as a get's body is answered.
pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The length of the base64 text with padding of `bytes` bytes.
pub fn encoded_len(bytes: usize) -> usize {
    bytes.div_ceil(3).saturating_mul(4)
}

/// A put's body as the raw interface carries it: base64 text, looked at
/// but not decoded.
#[derive(Debug)]
pub struct Body {
    text: String,
    bytes: usize,
}

/// What a put's `body` text is.
#[derive(Debug)]
pub enum BodyText {
    /// Standard base64 of at most [`BODY_TEXT`] characters.
    Within(Body),
    /// Longer than [`BODY_TEXT`]: past the bound whatever it holds.
    Past,
}

impl Body {
    /// Looks at `text` without decoding it: [`BodyText::Past`] when it is
    /// longer than [`BODY_TEXT`]; otherwise standard base64, with or
    /// without padding, or `Err` (malformed).
    pub fn read(text: String) -> Result<BodyText, &'static str> {
        const NOT_BASE64: &str = "`body` must be the bytes in standard base64 (A-Z, a-z, 0-9, + and /, with or without = padding)";
        if text.len() > BODY_TEXT {
            return Ok(BodyText::Past);
        }
        let all = text.as_bytes();
        let pads = all.iter().rev().take_while(|b| **b == b'=').count();
        if pads > 2 {
            return Err(NOT_BASE64);
        }
        let data = all.get(..all.len() - pads).unwrap_or_default();
        if !data.iter().all(|b| sextet(*b).is_some()) {
            return Err(NOT_BASE64);
        }
        let last = data.last().and_then(|b| sextet(*b)).unwrap_or(0);
        let bytes = match (data.len() % 4, pads) {
            (0, 0) => data.len() / 4 * 3,
            // Two characters give one byte: the last one's low four bits
            // are left over and must be zero.
            (2, 0 | 2) if last & 0x0f == 0 => data.len() / 4 * 3 + 1,
            (3, 0 | 1) if last & 0x03 == 0 => data.len() / 4 * 3 + 2,
            _ => return Err(NOT_BASE64),
        };
        Ok(BodyText::Within(Self { text, bytes }))
    }

    /// The number of bytes it decodes to.
    pub fn len(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    /// The length of its text.
    pub fn text_len(&self) -> usize {
        self.text.len()
    }

    /// The bytes. `None` only if [`Body::read`] and the decoder disagree,
    /// which they do not.
    pub fn decode(self) -> Option<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.bytes);
        ENGINE.decode_vec(self.text.as_bytes(), &mut bytes).ok()?;
        (bytes.len() == self.bytes).then_some(bytes)
    }
}

/// The value of a base64 character, `None` for anything else.
fn sextet(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The program's one budget of bodies held at once ([`BODIES_AT_ONCE`]):
/// a put takes room for its bytes and its text before it decodes, a get
/// for its file and its text before it reads.
pub struct Bodies {
    room: Arc<Semaphore>,
}

/// Room taken among [`BODIES_AT_ONCE`], given back when dropped.
#[derive(Debug)]
pub struct Room(#[expect(dead_code, reason = "held for its drop")] Option<OwnedSemaphorePermit>);

impl Default for Bodies {
    fn default() -> Self {
        Self {
            room: Arc::new(Semaphore::new(BODIES_AT_ONCE)),
        }
    }
}

impl Bodies {
    /// Room for a body of `bytes` bytes and its base64 text, waiting up to
    /// [`ROOM_WAIT`]; `None` when there was none.
    pub async fn take(&self, bytes: usize) -> Option<Room> {
        let wanted = bytes.saturating_add(encoded_len(bytes));
        if wanted == 0 {
            return Some(Room(None));
        }
        let wanted = u32::try_from(wanted.min(BODIES_AT_ONCE)).ok()?;
        let room = Arc::clone(&self.room);
        match tokio::time::timeout(ROOM_WAIT, room.acquire_many_owned(wanted)).await {
            Ok(Ok(permit)) => Some(Room(Some(permit))),
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

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
/// the budget of bodies, the key of cursors, and the storage root (when
/// `--storage-root` was given). One for the program.
pub struct Storage {
    pub files: Arc<Files>,
    pub bodies: Arc<Bodies>,
    pub cursors: cursor::Sealer,
    pub root: Option<Arc<Root>>,
}
