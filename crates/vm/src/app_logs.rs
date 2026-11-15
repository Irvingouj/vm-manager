//! Logs: what an Application's code printed and the errors it did not
//! catch, kept in the platform database request by request, for its
//! Contributors and Platform administrators (blackbox/CONTRACT.md,
//! "Versions, going back, and logs: Logs, on the platform's API").
//!
//! Interface: [`start`] once, which answers the [`Logs`] handle and the
//! [`Writer`] that owns the writing task; then, per request, its context
//! collects into [`Collected`] (held by the request in flight), and
//! [`Logs::request_ended`] makes the request's [`Entry`] when Ingress has its
//! answer; [`Logs::printed_outside`] makes an entry of its own for a line
//! printed with no request in flight. [`Logs::read`] answers the API.
//!
//! **Never in a request's way.** An entry is handed to a bounded queue with
//! `try_send`, which neither waits nor blocks: when [`QUEUE`] entries are
//! already waiting the entry is dropped and counted for its Application,
//! and the next entry of that Application that is kept carries the count in
//! `dropped_before`. Entries the writer could not write are counted the same
//! way. The program's own log (crate::log) is written as before, whatever
//! happens here.
//!
//! **The writer.** One task, owned by [`Writer`] (aborted when it is
//! dropped), takes what is waiting, up to [`BATCH`] entries at once and
//! without waiting for more, inserts them in one statement, and deletes all
//! but the newest [`KEPT`] entries of each Application it wrote for. Every
//! write is held to [`WRITE_TIMEOUT`] besides the pool's own limits. Since
//! one writer inserts in queue order, an entry's `id` grows with the order
//! requests ended and with when it became readable: it is the cursor.
//!
//! **Bounds**, each stated in the contract: a message is cut to
//! [`MESSAGE_BYTES`] and an uncaught error to [`UNCAUGHT_BYTES`] (at a
//! character boundary); a request keeps at most [`PRINTED_RECORDS`] printed
//! records and [`PRINTED_BYTES`] of messages (the record that would pass it
//! is not kept, nor any after it) and [`UNCAUGHT_RECORDS`] uncaught errors.
//! So one waiting entry holds at most about 64 KiB of messages and 160 KiB
//! of errors, and the queue at most [`QUEUE`] of them. The counts of dropped
//! entries are one number per Application.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hyper::{Method, StatusCode};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::JoinHandle;
use tokio_postgres::Client;
use tokio_postgres::types::Json;

use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::runner::InFlight;

/// Entries waiting for the writer. When full, an entry is dropped and
/// counted, never waited for.
pub const QUEUE: usize = 1024;

/// Entries the writer takes and inserts at once.
const BATCH: usize = 64;

/// The longest one write (an insert and its trimming) may take.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// The entries of an Application kept; older ones are deleted.
pub const KEPT: i64 = 5000;

/// The longest message of a printed record kept.
pub const MESSAGE_BYTES: usize = 8192;

/// The longest uncaught error kept.
pub const UNCAUGHT_BYTES: usize = 16_384;

/// The printed records one request keeps.
pub const PRINTED_RECORDS: usize = 200;

/// The bytes of messages one request keeps.
pub const PRINTED_BYTES: usize = 64 * 1024;

/// The uncaught errors one request keeps.
pub const UNCAUGHT_RECORDS: usize = 10;

/// The most entries one read answers.
pub const MAX_READ: u16 = 1000;

/// One printed record of an entry.
#[derive(Debug, Serialize)]
struct Printed {
    level: &'static str,
    message: String,
}

/// What a request's context collected for its entry. Held by the request
/// in flight; written by its context's host calls on the engine thread,
/// and taken once by Ingress when the request is answered. The lock is
/// never held across an await, and only for a push or the take.
#[derive(Debug, Default)]
pub struct Collected(Mutex<Kept>);

#[derive(Debug, Default)]
struct Kept {
    printed: Vec<Printed>,
    uncaught: Vec<String>,
    /// The bytes of the messages kept.
    bytes: usize,
    /// A record was not kept for the 64 KiB: none after it is either.
    full: bool,
    truncated: bool,
}

impl Collected {
    fn lock(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A printed record of the request, kept within the bounds. `message`
    /// is moved in, not copied.
    pub fn printed(&self, level: &'static str, mut message: String) {
        let mut kept = self.lock();
        if kept.full || kept.printed.len() >= PRINTED_RECORDS {
            kept.truncated = true;
            return;
        }
        if cut(&mut message, MESSAGE_BYTES) {
            kept.truncated = true;
        }
        let bytes = kept.bytes.saturating_add(message.len());
        if bytes > PRINTED_BYTES {
            kept.full = true;
            kept.truncated = true;
            return;
        }
        kept.bytes = bytes;
        kept.printed.push(Printed { level, message });
    }

    /// An uncaught error of the request, kept within the bounds.
    pub fn uncaught(&self, error: &str) {
        let mut kept = self.lock();
        if kept.uncaught.len() >= UNCAUGHT_RECORDS {
            kept.truncated = true;
            return;
        }
        let end = error.floor_char_boundary(UNCAUGHT_BYTES);
        if end < error.len() {
            kept.truncated = true;
        }
        // Copied: the error is the caller's, and the entry outlives it. At
        // most 16 KiB, at most 10 times a request.
        let error = error.get(..end).unwrap_or_default().to_owned();
        kept.uncaught.push(error);
    }

    fn take(&self) -> Kept {
        std::mem::take(&mut *self.lock())
    }
}

/// Cuts `text` to at most `bytes`, at a character boundary; whether it cut.
fn cut(text: &mut String, bytes: usize) -> bool {
    if text.len() <= bytes {
        return false;
    }
    let end = text.floor_char_boundary(bytes);
    text.truncate(end);
    true
}

/// The facts of the request an entry is for.
#[derive(Debug)]
struct RequestFacts {
    id: Arc<str>,
    user: String,
    method: Method,
    path: String,
    status: u16,
}

/// One log entry, waiting to be written.
#[derive(Debug)]
pub struct Entry {
    application: Arc<str>,
    version: u64,
    time: SystemTime,
    /// `None` for what was printed outside a request.
    request: Option<RequestFacts>,
    printed: Vec<Printed>,
    uncaught: Vec<String>,
    truncated: bool,
    dropped_before: u64,
}

/// The entries dropped and not yet told, by Application. One number per
/// Application that has some; `pending` lets the common case (none) pass
/// without the lock.
#[derive(Default)]
struct Dropped {
    pending: AtomicBool,
    by_application: Mutex<HashMap<Arc<str>, u64>>,
}

impl Dropped {
    fn add(&self, application: &Arc<str>, count: u64) {
        let mut by = self
            .by_application
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let counted = by.entry(Arc::clone(application)).or_insert(0);
        *counted = counted.saturating_add(count);
        self.pending.store(true, Ordering::SeqCst);
    }

    /// The count for `application`, which is then told.
    fn take(&self, application: &str) -> u64 {
        if !self.pending.load(Ordering::SeqCst) {
            return 0;
        }
        let mut by = self
            .by_application
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = by.remove(application).unwrap_or(0);
        if by.is_empty() {
            self.pending.store(false, Ordering::SeqCst);
        }
        count
    }
}

/// The handle every request uses: hands entries to the writer, and reads.
pub struct Logs {
    sender: mpsc::Sender<Entry>,
    dropped: Arc<Dropped>,
    database: Arc<PlatformDatabase>,
}

/// Owns the writing task: it is aborted when this is dropped.
pub struct Writer {
    task: JoinHandle<()>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the writer on the current runtime.
pub fn start(database: Arc<PlatformDatabase>) -> (Logs, Writer) {
    // Bound: QUEUE entries waiting. When full, `keep` drops the entry and
    // counts it.
    let (sender, receiver) = mpsc::channel(QUEUE);
    let dropped = Arc::new(Dropped::default());
    let task = tokio::spawn(write_all(
        receiver,
        Arc::clone(&database),
        Arc::clone(&dropped),
    ));
    (
        Logs {
            sender,
            dropped,
            database,
        },
        Writer { task },
    )
}

impl Logs {
    /// Makes the entry of a request the Application's code was given, from
    /// what its context collected, once the request is answered `status`.
    pub fn request_ended(
        &self,
        in_flight: &InFlight,
        method: &Method,
        path: &str,
        status: StatusCode,
    ) {
        let kept = in_flight.collected.take();
        self.keep(Entry {
            application: Arc::clone(&in_flight.application),
            version: in_flight.version,
            time: SystemTime::now(),
            request: Some(RequestFacts {
                id: Arc::clone(&in_flight.request),
                // Copied: the entry outlives the request. The User's id is
                // at most 256 bytes; the path is as long as the request's
                // head allows; a method is a constant but for one that is
                // not standard.
                user: in_flight.user.id.clone(),
                method: method.clone(),
                path: path.to_owned(),
                status: status.as_u16(),
            }),
            printed: kept.printed,
            uncaught: kept.uncaught,
            truncated: kept.truncated,
            dropped_before: 0,
        });
    }

    /// Makes the entry of one line printed outside a request by Version
    /// `version` of `application`. `message` is moved in, not copied.
    pub fn printed_outside(
        &self,
        application: &Arc<str>,
        version: u64,
        level: &'static str,
        mut message: String,
    ) {
        let truncated = cut(&mut message, MESSAGE_BYTES);
        self.keep(Entry {
            application: Arc::clone(application),
            version,
            time: SystemTime::now(),
            request: None,
            printed: vec![Printed { level, message }],
            uncaught: Vec::new(),
            truncated,
            dropped_before: 0,
        });
    }

    /// Hands `entry` to the writer without waiting: dropped and counted
    /// when the queue is full.
    fn keep(&self, mut entry: Entry) {
        entry.dropped_before = self.dropped.take(&entry.application);
        match self.sender.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(entry) | TrySendError::Closed(entry)) => {
                self.dropped
                    .add(&entry.application, entry.dropped_before.saturating_add(1));
            }
        }
    }

    /// The entries of `application` that `query` asks for, oldest first,
    /// and the cursor `next`.
    pub async fn read(&self, application: &str, query: &Query) -> Result<Page, DbError> {
        let limit = i64::from(query.limit);
        let request = query.request.as_deref();
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let rows = match &query.after {
                    Some((after, _)) => {
                        client
                            .query(
                                concat!(
                                    "SELECT id, ",
                                    rfc3339!("made_at"),
                                    ", request, version, user_id, method, path, status,
                                       printed, uncaught, truncated, dropped_before
                                     FROM log_entries
                                     WHERE application = $1 AND id > $2
                                       AND ($3::text IS NULL OR request = $3)
                                     ORDER BY id LIMIT $4"
                                ),
                                &[&application, &after, &request, &limit],
                            )
                            .await
                    }
                    None => {
                        client
                            .query(
                                concat!(
                                    "SELECT * FROM (SELECT id, ",
                                    rfc3339!("made_at"),
                                    ", request, version, user_id, method, path, status,
                                       printed, uncaught, truncated, dropped_before
                                     FROM log_entries
                                     WHERE application = $1
                                       AND ($2::text IS NULL OR request = $2)
                                     ORDER BY id DESC LIMIT $3) newest
                                     ORDER BY id"
                                ),
                                &[&application, &request, &limit],
                            )
                            .await
                    }
                }
                .map_err(DbError::from)?;
                // With no entry and no `after`: a cursor after which only
                // entries made later come.
                let last = if rows.is_empty() && query.after.is_none() {
                    let row = client
                        .query_one("SELECT coalesce(max(id), 0) FROM log_entries", &[])
                        .await
                        .map_err(DbError::from)?;
                    Some(row.try_get::<_, i64>(0).map_err(DbError::from)?)
                } else {
                    None
                };
                Ok((rows, last))
            })
            .await?;
        let (rows, last) = rows;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            entries.push(EntryView {
                id: row.try_get::<_, i64>(0).map_err(DbError::from)?.to_string(),
                time: row.try_get(1).map_err(DbError::from)?,
                request: row.try_get(2).map_err(DbError::from)?,
                version: number(row.try_get::<_, i32>(3).map_err(DbError::from)?)?,
                user: row.try_get(4).map_err(DbError::from)?,
                method: row.try_get(5).map_err(DbError::from)?,
                path: row.try_get(6).map_err(DbError::from)?,
                status: row
                    .try_get::<_, Option<i32>>(7)
                    .map_err(DbError::from)?
                    .map(number)
                    .transpose()?,
                printed: row.try_get(8).map_err(DbError::from)?,
                uncaught: row.try_get(9).map_err(DbError::from)?,
                truncated: row.try_get(10).map_err(DbError::from)?,
                dropped_before: number(row.try_get::<_, i64>(11).map_err(DbError::from)?)?,
            });
        }
        let next = match (entries.last(), &query.after, last) {
            (Some(entry), _, _) => entry.id.clone(),
            (None, Some((_, given)), _) => given.clone(),
            (None, None, Some(last)) => last.to_string(),
            (None, None, None) => "0".to_owned(),
        };
        Ok(Page { entries, next })
    }
}

/// A number the database holds, as the API shows it.
fn number<T: TryInto<u64>>(stored: T) -> Result<u64, DbError> {
    stored
        .try_into()
        .map_err(|_| DbError::unexpected("a negative number in a log entry"))
}

/// What a read of the logs asks for.
#[derive(Debug)]
pub struct Query {
    /// 1 to [`MAX_READ`].
    pub limit: u16,
    /// The entries after this cursor, and the cursor as given.
    pub after: Option<(i64, String)>,
    /// Only the entry of this request.
    pub request: Option<String>,
}

/// A cursor as the API gives it: the decimal digits of an entry's `id`, or
/// `0`, with no sign and no leading zero.
pub fn cursor(text: &str) -> Option<i64> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    canonical.then(|| text.parse().ok()).flatten()
}

/// One answer of a read.
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<EntryView>,
    pub next: String,
}

/// An entry as the API answers it.
#[derive(Debug, Serialize)]
pub struct EntryView {
    id: String,
    time: String,
    request: Option<String>,
    version: u64,
    user: Option<String>,
    method: Option<String>,
    path: Option<String>,
    status: Option<u64>,
    printed: Value,
    uncaught: Value,
    truncated: bool,
    dropped_before: u64,
}

/// An entry as the writer sends it to the database, in one JSON array.
#[derive(Debug, Serialize)]
struct Row<'a> {
    application: &'a str,
    version: u64,
    time_us: i64,
    request: Option<&'a str>,
    user_id: Option<&'a str>,
    method: Option<&'a str>,
    path: Option<&'a str>,
    status: Option<u16>,
    printed: Vec<RowPrinted<'a>>,
    uncaught: Vec<Cow<'a, str>>,
    truncated: bool,
    dropped_before: u64,
}

#[derive(Debug, Serialize)]
struct RowPrinted<'a> {
    level: &'static str,
    message: Cow<'a, str>,
}

/// Postgres's `jsonb` cannot hold U+0000: it is written U+FFFD. Copied only
/// when there is one.
fn storable(text: &str) -> Cow<'_, str> {
    if text.contains('\0') {
        Cow::Owned(text.replace('\0', "\u{fffd}"))
    } else {
        Cow::Borrowed(text)
    }
}

impl<'a> Row<'a> {
    fn of(entry: &'a Entry) -> Self {
        let time_us = entry
            .time
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        let request = entry.request.as_ref();
        Self {
            application: &entry.application,
            version: entry.version,
            time_us,
            request: request.map(|r| &*r.id),
            user_id: request.map(|r| r.user.as_str()),
            method: request.map(|r| r.method.as_str()),
            path: request.map(|r| r.path.as_str()),
            status: request.map(|r| r.status),
            printed: entry
                .printed
                .iter()
                .map(|p| RowPrinted {
                    level: p.level,
                    message: storable(&p.message),
                })
                .collect(),
            uncaught: entry.uncaught.iter().map(|u| storable(u)).collect(),
            truncated: entry.truncated,
            dropped_before: entry.dropped_before,
        }
    }
}

/// The writer: takes what is waiting, writes it, until every handle is
/// gone.
async fn write_all(
    mut receiver: mpsc::Receiver<Entry>,
    database: Arc<PlatformDatabase>,
    dropped: Arc<Dropped>,
) {
    let mut batch = Vec::with_capacity(BATCH);
    loop {
        // Returns as soon as one entry waits, with up to BATCH of them: no
        // entry waits for others to come.
        if receiver.recv_many(&mut batch, BATCH).await == 0 {
            return;
        }
        let inserted = tokio::time::timeout(WRITE_TIMEOUT, insert(&database, &batch)).await;
        match failure(inserted) {
            None => {
                // Older entries are deleted after; when that fails, the next
                // write deletes them.
                let trimmed = tokio::time::timeout(WRITE_TIMEOUT, trim(&database, &batch)).await;
                if let Some(error) = failure(trimmed) {
                    log::record(
                        "log-entries-not-trimmed",
                        serde_json::json!({ "error": error }),
                    );
                }
            }
            Some(error) => {
                // Not written (or, past the time, not known to be): counted
                // as dropped, told by the next entry kept.
                for entry in &batch {
                    dropped.add(&entry.application, entry.dropped_before.saturating_add(1));
                }
                log::record(
                    "log-entries-unwritten",
                    serde_json::json!({ "entries": batch.len(), "error": error }),
                );
            }
        }
        batch.clear();
    }
}

/// What went wrong with a write held to [`WRITE_TIMEOUT`], if anything.
fn failure(written: Result<Result<(), DbError>, tokio::time::error::Elapsed>) -> Option<String> {
    match written {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some(format!(
            "writing took longer than {} ms",
            WRITE_TIMEOUT.as_millis()
        )),
    }
}

/// Inserts `batch`, in its order, in one statement.
async fn insert(database: &PlatformDatabase, batch: &[Entry]) -> Result<(), DbError> {
    let rows: Vec<Row<'_>> = batch.iter().map(Row::of).collect();
    database
        .pool()
        .run(async |client: &mut Client| {
            client
                .execute(
                    "INSERT INTO log_entries (application, version, made_at, request, user_id,
                                              method, path, status, printed, uncaught,
                                              truncated, dropped_before)
                     SELECT r.application, r.version,
                            to_timestamp(r.time_us::double precision / 1000000),
                            r.request, r.user_id, r.method, r.path, r.status, r.printed,
                            r.uncaught, r.truncated, r.dropped_before
                     FROM ROWS FROM (jsonb_to_recordset($1::jsonb) AS (
                              application text, version integer, time_us bigint,
                              request text, user_id text, method text, path text,
                              status integer, printed jsonb, uncaught jsonb,
                              truncated boolean, dropped_before bigint))
                          WITH ORDINALITY AS r(application, version, time_us, request,
                              user_id, method, path, status, printed, uncaught, truncated,
                              dropped_before, ord)
                     ORDER BY r.ord",
                    &[&Json(&rows)],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
}

/// Deletes all but the newest [`KEPT`] entries of each Application of
/// `batch`.
async fn trim(database: &PlatformDatabase, batch: &[Entry]) -> Result<(), DbError> {
    let applications: Vec<&str> = batch
        .iter()
        .map(|e| &*e.application)
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    let offset = KEPT - 1;
    database
        .pool()
        .run(async |client: &mut Client| {
            client
                .execute(
                    "DELETE FROM log_entries e
                     USING (SELECT a.name,
                                   (SELECT l.id FROM log_entries l WHERE l.application = a.name
                                    ORDER BY l.id DESC OFFSET $2::bigint LIMIT 1) AS oldest_kept
                            FROM unnest($1::text[]) AS a(name)) k
                     WHERE e.application = k.name AND e.id < k.oldest_kept",
                    &[&applications, &offset],
                )
                .await
                .map_err(DbError::from)?;
            Ok(())
        })
        .await
}
