//! The relay: ships what an own database's outbox (`vm.outbox`) holds to
//! the platform database's `change_history`, then deletes it from the
//! outbox (ADR 0017, second slice; blackbox/CONTRACT.md, "Shipping";
//! research part C).
//!
//! At least once, and idempotent: a batch is inserted with `ON CONFLICT
//! (application, id) DO NOTHING` and committed before its rows are deleted
//! from the outbox by their ids, so a shipment stopped anywhere (the program
//! killed, a step timed out) leaves each change either still in the outbox,
//! to be shipped again and kept once, or shipped and deleted. A change
//! rolled back was never in the outbox for anyone else to see. Rows are
//! deleted by their ids, never by a range: a change committed later with a
//! lesser id (its transaction began first) is shipped by the next batch.
//!
//! Bounds, and who waits for what (blackbox/CONTRACT.md, "Nobody waits for
//! what is not theirs"):
//!
//! - **one shipment task per own database at a time** ([`Shipping`]): a
//!   request while one runs is counted (`asked`), and the running task goes
//!   on; so the relay's tasks are at most the own databases;
//! - **what a request waits for**: a write's request is shipped once the
//!   task sees the outbox empty at a look made after it. A read's (waited
//!   for) is given a mark, the greatest id in the outbox at the task's next
//!   look, and is served once a batch has shipped an id at least the mark,
//!   or the outbox is seen empty after it asked: everything committed
//!   before it asked is visible at that look, so a read never waits for
//!   writes made after it asked. At most [`MARKS`] reads hold a mark at
//!   once per own database; a read that finds every place taken is given
//!   one when a place comes free, at a later look ([`run`]);
//! - **turns of one batch**: [`AT_ONCE`] batches at once in the program,
//!   across own databases. A shipment takes a turn for each batch and gives
//!   it back after it, and the turns are handed out first come, first
//!   served (Tokio's semaphore is fair); so another Application's batch is
//!   at most the batches already waiting, one per own database, behind. A
//!   turn is waited for at most [`TURN_WAIT`]: with each turn at most three
//!   steps of [`STEP_TIMEOUT`] and the opening of one connection, that is
//!   reached only when the databases stop answering, and a shipment asked
//!   at start does not fail for want of a turn otherwise;
//! - **connections**: at most [`LINES`] to own databases in the program, in
//!   use, kept and being closed together. A task takes one only for a
//!   batch, in its turn, and puts it back in the [`Courier`]'s cache after
//!   it: its own database's if it is there; else one opened in a free
//!   place; else in the place of the one put back longest ago, which is
//!   closed first. So at most [`AT_ONCE`] are in use, and a task never
//!   waits for another to give one up (only, at most [`TURN_WAIT`], for a
//!   session being closed). A task with nothing more asked waits [`IDLE`],
//!   then closes its own database's connection if it is still in the
//!   cache, before it ends: no session outlives its task;
//! - **a batch** is at most [`BATCH_ROWS`] changes and, past its first,
//!   [`BATCH_BYTES`] of `old` and `new` as text, read by the own database
//!   as one JSON text and sent on as one parameter: what the program holds
//!   of row data at once per shipment is one batch (a row's size is bounded
//!   by the host's write limits). Each batch's write takes one connection
//!   of the platform database's pool (16) for its length, so the
//!   Operation log keeps at least 12;
//! - **each step** (reading the outbox, writing `change_history`, deleting
//!   from the outbox) is ended after [`STEP_TIMEOUT`], by the server's
//!   `statement_timeout` and `lock_timeout` and by the host.
//!
//! A shipment that fails is recorded (`history-not-shipped`, with the
//! Application and a fixed description of the step, nothing of any row);
//! what it did not ship stays in the outbox for the next shipment, which
//! any later write, Version, history read or start asks for.
//!
//! The relay connects to the own database as its platform role, with the
//! search path pinned to the catalog in the startup packet (as every
//! session of that role, crate::own_db::OwnDatabases::connect), and names
//! everything it reads by its schema: what it reads from the outbox is
//! data, passed on as a parameter, never run.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, Config, NoTls};

use crate::log;
use crate::platform_db::{DbError, PlatformDatabase};

/// The most changes one batch holds.
pub const BATCH_ROWS: i64 = 1_000;

/// The most bytes of `old` and `new` as text (`octet_length` of each's
/// `jsonb` text) one batch holds past its first change, which always goes,
/// so that one large change never stalls the outbox.
pub const BATCH_BYTES: i64 = 4 * 1024 * 1024;

/// The longest each step of a batch may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// Batches shipped at once across all own databases.
pub const AT_ONCE: usize = 4;

/// The longest a shipment waits for a turn, or for a connection.
pub const TURN_WAIT: Duration = Duration::from_secs(600);

/// Connections the relay keeps to own databases at most.
pub const LINES: usize = 32;

/// How long a shipment task keeps its connection once nothing more is
/// asked of it.
pub const IDLE: Duration = Duration::from_secs(10);

/// Reads one own database gives a mark at most at once: its places to
/// wait ([`Marks`]).
const MARKS: usize = 64;

/// How long a connection given up is waited for to end before its place
/// is given back.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// How long a task that failed waits before it tries again for a request
/// made meanwhile.
const RETRY_AFTER: Duration = Duration::from_secs(1);

/// The longest opening the connection to the own database may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The greatest id in the outbox now: the mark a read is given, and
/// served by once a batch has shipped an id at least as great.
const MARK: &str = "SELECT pg_catalog.max(b.id) FROM vm.outbox b";

/// The next batch of the outbox, oldest first: the ids, and the rows as
/// one JSON array (of `json_build_object` of each), cut at [`BATCH_ROWS`]
/// and, past the first, [`BATCH_BYTES`]. Both null when it is empty. Built
/// as `json`, which is text, not `jsonb`: a batch of 1,000 rows is read in
/// about half the time (research part H).
const READ_BATCH: &str = "
SELECT pg_catalog.array_agg(x.id ORDER BY x.id),
       pg_catalog.json_agg(pg_catalog.json_build_object(
           'id', x.id, 'at', x.at, 'schema_name', x.schema_name, 'table_name', x.table_name,
           'kind', x.kind, 'key', x.key, 'old', x.old, 'new', x.new, 'changed', x.changed,
           'user_id', x.user_id, 'actor', x.actor, 'request', x.request,
           'migration', x.migration, 'structure', x.structure, 'json_nulls', x.json_nulls)
           ORDER BY x.id)::text
  FROM (SELECT o.*,
               pg_catalog.sum(o.size) OVER (ORDER BY o.id)
                 - pg_catalog.first_value(o.size) OVER (ORDER BY o.id) AS past_first
          FROM (SELECT b.*,
                       coalesce(pg_catalog.octet_length(b.old::pg_catalog.text), 0)
                         + coalesce(pg_catalog.octet_length(b.new::pg_catalog.text), 0) AS size
                  FROM vm.outbox b
                 ORDER BY b.id
                 LIMIT $1) o) x
 WHERE x.past_first <= $2";

/// One batch into `change_history`, for the Application `$1`, from the
/// JSON text `$2` the outbox gave. A change already there is left as it is.
const WRITE_BATCH: &str = "
INSERT INTO change_history (application, id, at, schema_name, table_name, kind, key, old, new,
                            changed, user_id, actor, request, migration, structure, json_nulls)
SELECT $1, r.id, r.at, r.schema_name, r.table_name, r.kind, r.key, r.old, r.new,
       r.changed, r.user_id, r.actor, r.request, r.migration, r.structure, r.json_nulls
  FROM pg_catalog.jsonb_populate_recordset(NULL::change_history, $2::pg_catalog.text::pg_catalog.jsonb) r
ON CONFLICT (application, id) DO NOTHING";

/// What has been asked of one own database's relay, and done.
#[derive(Debug)]
pub struct Shipping {
    /// Every request for a shipment, a write's or a read's, takes the next
    /// number: the task ships until it has seen the last.
    asked: AtomicU64,
    /// Every request that is waited for (a read of the history) takes the
    /// next number of these too: only these are given a mark.
    reads: AtomicU64,
    /// A shipment task runs for this own database.
    running: AtomicBool,
    /// The greatest read number served, and failed.
    passes: watch::Sender<Passes>,
    /// Told of a request while the task keeps its connection, idle.
    wake: Notify,
}

#[derive(Clone, Copy, Debug, Default)]
struct Passes {
    shipped: u64,
    failed: u64,
}

impl Default for Shipping {
    fn default() -> Self {
        Self {
            asked: AtomicU64::new(0),
            reads: AtomicU64::new(0),
            running: AtomicBool::new(false),
            passes: watch::Sender::new(Passes::default()),
            wake: Notify::new(),
        }
    }
}

impl Shipping {
    /// A request for a shipment: its read number when it is `waited` for,
    /// and whether the caller must start the shipment task (none runs).
    pub fn ask(&self, waited: bool) -> (Option<u64>, bool) {
        // A read's number before the request's: a task that sees the
        // request (`asked`) sees the read.
        let ticket = waited.then(|| self.reads.fetch_add(1, Ordering::SeqCst).saturating_add(1));
        self.asked.fetch_add(1, Ordering::SeqCst);
        let start = !self.running.swap(true, Ordering::SeqCst);
        if !start {
            // The task may be idle, keeping its connection: wake it. A
            // wake-up it is not waiting for is kept for it, and costs at
            // most one look at an empty outbox.
            self.wake.notify_one();
        }
        (ticket, start)
    }

    /// Waits until read `ticket` is served: `true` once everything
    /// committed before it was asked is in `change_history`, `false` if the
    /// shipment failed. The caller bounds the wait.
    pub async fn wait(&self, ticket: u64) -> bool {
        let mut passes = self.passes.subscribe();
        match passes
            .wait_for(|p| p.shipped >= ticket || p.failed >= ticket)
            .await
        {
            Ok(p) => p.shipped >= ticket,
            Err(_) => false,
        }
    }

    /// A request could not even start a task: every read asked so far is
    /// failed.
    pub fn give_up(&self) {
        let reads = self.reads.load(Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        self.passes.send_modify(|p| p.failed = p.failed.max(reads));
    }
}

/// What a shipment needs of the program: where Postgres is, the platform
/// database, the turns and the connections.
pub struct Courier {
    pub platform: Arc<PlatformDatabase>,
    pub host: String,
    pub port: u16,
    /// [`AT_ONCE`] batches at once.
    pub turns: Semaphore,
    /// [`LINES`] connections at once: in the cache, in use, and being
    /// closed, together.
    lines: Arc<Semaphore>,
    /// The connections not in use, the one put back longest ago first: at
    /// most [`LINES`], one per own database. Locked only to take or put
    /// back, never across an await.
    cache: Mutex<VecDeque<Kept>>,
}

/// A connection in the cache, and the Application whose own database it is
/// to.
struct Kept {
    application: Arc<str>,
    line: Line,
}

impl Courier {
    pub fn new(platform: Arc<PlatformDatabase>, host: String, port: u16) -> Self {
        Self {
            platform,
            host,
            port,
            turns: Semaphore::new(AT_ONCE),
            lines: Arc::new(Semaphore::new(LINES)),
            cache: Mutex::new(VecDeque::with_capacity(LINES)),
        }
    }

    fn cache(&self) -> MutexGuard<'_, VecDeque<Kept>> {
        // A panic while it was held left it whole: every change to it is
        // one call of a `VecDeque` method.
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The connection to `application`'s own database put back in the
    /// cache, if it is still there.
    fn take(&self, application: &str) -> Option<Line> {
        let mut cache = self.cache();
        let at = cache.iter().position(|k| &*k.application == application)?;
        cache.remove(at).map(|k| k.line)
    }

    /// Puts a connection back, as the one used last.
    fn put_back(&self, application: &Arc<str>, line: Line) {
        self.cache().push_back(Kept {
            application: Arc::clone(application),
            line,
        });
    }
}

/// What one shipment ships from: the Application, its own database's name
/// and platform role, and what the program knows of it (that role's
/// password, its [`Shipping`], whether it is in form).
pub struct Route {
    pub application: Arc<str>,
    pub database: String,
    pub role: String,
    pub known: Arc<super::Known>,
}

/// A connection to the own database as its platform role, and its place
/// among the [`LINES`]. The task that drives the connection is owned here
/// and ended when this is dropped; the place goes with it.
struct Line {
    client: Option<Client>,
    task: JoinHandle<()>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Line {
    fn client(&self) -> Option<&Client> {
        self.client.as_ref().filter(|c| !c.is_closed())
    }

    /// Ends the session and answers its place: the client goes, the
    /// connection says goodbye to the server and its task ends (waited for
    /// at most [`CLOSE_WAIT`]), so that the place is free again only when
    /// the session is gone, and no more than [`LINES`] are open at once.
    async fn close(mut self) -> Option<OwnedSemaphorePermit> {
        self.client = None;
        tokio::time::timeout(CLOSE_WAIT, &mut self.task).await.ok();
        self.permit.take()
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The shipment task of one own database. It ships while anything is
/// asked of it, a batch at a time, each in a turn of its own and with its
/// connection taken from the cache for it; then it waits [`IDLE`] for
/// another request, and before it ends closes its connection if it is
/// still in the cache.
///
/// A write's request is given no mark: the task ships until the outbox is
/// empty at a look made after the request (a batch with nothing in it), so
/// what it committed is shipped. Only a read, which is waited for, is given
/// a mark: the greatest id in the outbox when the task first looks after
/// the read asked (between two batches, so at most one batch later), at
/// most [`MARKS`] at once. Everything committed before the read asked is
/// visible at that look, so has an id at most the mark; batches go in the
/// order of ids; so once a batch has shipped an id at least the mark, or
/// the outbox is seen empty after the read asked, all of it is shipped and
/// the read is served. A read that finds every place taken is given a mark
/// only when one comes free, at a look later still: what it waits for then
/// includes everything committed before it asked. No read is served
/// otherwise; a failure fails it.
pub async fn run(courier: Arc<Courier>, route: Route) {
    let shipping = &route.known.shipping;
    let mut marks = Marks::default();
    loop {
        // A step that fails fails every read asked before it began, also
        // those not yet given a mark.
        let asked_before = shipping.asked.load(Ordering::SeqCst);
        let reads_before = shipping.reads.load(Ordering::SeqCst);
        match courier.step(&route, &mut marks).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(step) => {
                marks.waiting.clear();
                marks.seen_reads = marks.seen_reads.max(reads_before);
                marks.seen_asked = marks.seen_asked.max(asked_before);
                log::record(
                    "history-not-shipped",
                    serde_json::json!({ "application": &*route.application, "problem": step }),
                );
                let failed = marks.seen_reads;
                shipping
                    .passes
                    .send_modify(|p| p.failed = p.failed.max(failed));
                if shipping.asked.load(Ordering::SeqCst) != marks.seen_asked {
                    // Asked again meanwhile: try again, not at once.
                    tokio::time::sleep(RETRY_AFTER).await;
                    continue;
                }
            }
        }
        if shipping.asked.load(Ordering::SeqCst) != marks.seen_asked {
            continue;
        }
        // Idle, the connection in the cache for the next request. A request
        // made since the look above has stored its wake-up (`notify_one`).
        let asked = tokio::select! {
            () = shipping.wake.notified() => true,
            () = tokio::time::sleep(IDLE) => false,
        };
        if asked {
            continue;
        }
        if let Some(open) = courier.take(&route.application) {
            open.close().await;
        }
        shipping.running.store(false, Ordering::SeqCst);
        // Asked meanwhile, by a caller that saw this task running and so
        // started none: go on, unless another task has started.
        if shipping.asked.load(Ordering::SeqCst) == marks.seen_asked
            || shipping.running.swap(true, Ordering::SeqCst)
        {
            return;
        }
    }
}

/// What a task has seen asked, and the reads it has given a mark and not
/// yet served.
#[derive(Default)]
struct Marks {
    /// The greatest request number whose writes are known shipped.
    seen_asked: u64,
    /// The greatest read number given a mark (or served or failed).
    seen_reads: u64,
    /// `(read number, mark)`, both increasing; at most [`MARKS`].
    waiting: VecDeque<(u64, i64)>,
}

impl Courier {
    /// One step of a shipment task, in a turn of its own and with its
    /// connection: a mark for the reads asked since the last if a place is
    /// free, then one batch. `true` when there may be more to do, `false`
    /// when everything asked is shipped; or the step that failed. The
    /// connection is put back in the cache after a step that did not fail,
    /// and closed after one that did.
    async fn step(&self, route: &Route, marks: &mut Marks) -> Result<bool, &'static str> {
        let shipping = &route.known.shipping;
        if !route.known.in_form.load(Ordering::Acquire) {
            return Err("the own database is not in this program's form");
        }
        if shipping.asked.load(Ordering::SeqCst) == marks.seen_asked && marks.waiting.is_empty() {
            return Ok(false);
        }
        let turn = tokio::time::timeout(TURN_WAIT, self.turns.acquire())
            .await
            .map_err(|_| "waiting for a turn to ship: none came")?
            .map_err(|_| "waiting for a turn to ship: the program is stopping")?;
        let line = self.line(route).await?;
        let done = self.batch(route, &line, marks).await;
        drop(turn);
        if done.is_ok() {
            self.put_back(&route.application, line);
        } else {
            line.close().await;
        }
        done
    }

    /// A connection to `route`'s own database for one batch: its own from
    /// the cache; else one opened in a free place; else in the place of
    /// the connection put back longest ago, closed first. Only a task with
    /// a turn takes one, and gives it back before the turn, so at most
    /// [`AT_ONCE`] are in use, and with [`LINES`] places there is one to
    /// take unless sessions are being closed by tasks that end: then it is
    /// waited for, at most [`TURN_WAIT`].
    async fn line(&self, route: &Route) -> Result<Line, &'static str> {
        let open = "connecting to the own database as its platform role";
        let mut permit = None;
        if let Some(line) = self.take(&route.application) {
            if line.client().is_some() {
                return Ok(line);
            }
            permit = line.close().await;
        }
        if permit.is_none() {
            permit = Arc::clone(&self.lines).try_acquire_owned().ok();
        }
        if permit.is_none() {
            let oldest = self.cache().pop_front();
            if let Some(oldest) = oldest {
                permit = oldest.line.close().await;
            }
        }
        let permit = match permit {
            Some(permit) => permit,
            None => tokio::time::timeout(TURN_WAIT, Arc::clone(&self.lines).acquire_owned())
                .await
                .map_err(|_| "waiting for a connection to ship: none came")?
                .map_err(|_| "waiting for a connection to ship: the program is stopping")?,
        };
        self.connect(route, permit).await.map_err(|_| open)
    }

    /// One batch, on `line`.
    async fn batch(
        &self,
        route: &Route,
        line: &Line,
        marks: &mut Marks,
    ) -> Result<bool, &'static str> {
        let shipping = &route.known.shipping;
        let client = line
            .client()
            .ok_or("connecting to the own database as its platform role")?;
        // `asked` before `reads`: every read counted in the one is counted
        // in the other (`Shipping::ask`). Both before the looks below.
        let asked = shipping.asked.load(Ordering::SeqCst);
        let reads = shipping.reads.load(Ordering::SeqCst);
        if reads > marks.seen_reads && marks.waiting.len() < MARKS {
            let mark = within("reading the outbox", read_mark(client)).await?;
            match mark {
                Some(mark) => {
                    marks.seen_reads = reads;
                    marks.waiting.push_back((reads, mark));
                }
                None => {
                    serve_all(shipping, marks, asked, reads);
                    return Ok(false);
                }
            }
        }
        let (ids, rows) = within("reading the outbox", read_batch(client)).await?;
        let (Some(ids), Some(rows)) = (ids, rows) else {
            // Empty at a look made after every request counted above: all
            // of them are served, also reads that hold no mark because
            // every place was taken.
            serve_all(shipping, marks, asked, reads);
            return Ok(true);
        };
        within(
            "writing change_history",
            self.write_batch(&route.application, &rows),
        )
        .await?;
        within(
            "deleting what was shipped from the outbox",
            delete_batch(client, &ids),
        )
        .await?;
        let top = ids.last().copied().unwrap_or(i64::MIN);
        while let Some(&(ticket, mark)) = marks.waiting.front() {
            if mark > top {
                break;
            }
            marks.waiting.pop_front();
            shipping
                .passes
                .send_modify(|p| p.shipped = p.shipped.max(ticket));
        }
        Ok(true)
    }

    /// A connection to the own database as its platform role, in the place
    /// `permit`: the search path pinned to the catalog, and the server
    /// ending any statement or lock wait at [`STEP_TIMEOUT`].
    async fn connect(&self, route: &Route, permit: OwnedSemaphorePermit) -> Result<Line, DbError> {
        let mut config = Config::new();
        let ms = STEP_TIMEOUT.as_millis();
        config
            .host(&self.host)
            .port(self.port)
            .user(&route.role)
            .password(&route.known.platform_password.0)
            .dbname(&route.database)
            .application_name("vm relay")
            .connect_timeout(CONNECT_TIMEOUT)
            .options(format!(
                "-c search_path=pg_catalog,pg_temp -c statement_timeout={ms} -c lock_timeout={ms}"
            ));
        let (client, connection) = tokio::time::timeout(CONNECT_TIMEOUT, config.connect(NoTls))
            .await
            .map_err(|_| DbError::timed_out("opening a connection", CONNECT_TIMEOUT))?
            .map_err(DbError::from)?;
        // Owned by the Line, ended with it.
        let live = crate::platform_db::Live::new();
        let task = tokio::spawn(async move {
            let _live = live;
            connection.await.ok();
        });
        Ok(Line {
            client: Some(client),
            task,
            permit: Some(permit),
        })
    }

    /// One batch in one transaction of the platform database, with no
    /// savepoint: its rows share one `xmin`.
    async fn write_batch(&self, application: &str, rows: &str) -> Result<(), DbError> {
        let ms = STEP_TIMEOUT.as_millis();
        let settings = format!("SET LOCAL statement_timeout = {ms}; SET LOCAL lock_timeout = {ms}");
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .batch_execute(&settings)
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(WRITE_BATCH, &[&application, &rows])
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)
            })
            .await
    }
}

/// Every request counted in `asked` and every read counted in `reads` is
/// served: the outbox was seen empty at a look made after them.
fn serve_all(shipping: &Shipping, marks: &mut Marks, asked: u64, reads: u64) {
    marks.seen_asked = asked;
    marks.seen_reads = marks.seen_reads.max(reads);
    marks.waiting.clear();
    shipping
        .passes
        .send_modify(|p| p.shipped = p.shipped.max(reads));
}

/// The mark: the greatest id in the outbox now; `None` when it is empty.
async fn read_mark(client: &Client) -> Result<Option<i64>, DbError> {
    client
        .query_one(MARK, &[])
        .await
        .and_then(|row| row.try_get(0))
        .map_err(DbError::from)
}

/// The next batch of the outbox, oldest first.
async fn read_batch(client: &Client) -> Result<(Option<Vec<i64>>, Option<String>), DbError> {
    let row = client
        .query_one(READ_BATCH, &[&BATCH_ROWS, &BATCH_BYTES])
        .await
        .map_err(DbError::from)?;
    Ok((
        row.try_get(0).map_err(DbError::from)?,
        row.try_get(1).map_err(DbError::from)?,
    ))
}

/// Deletes the changes shipped, by their ids.
async fn delete_batch(client: &Client, ids: &[i64]) -> Result<(), DbError> {
    client
        .execute(
            "DELETE FROM vm.outbox WHERE id OPERATOR(pg_catalog.=) ANY ($1::pg_catalog.int8[])",
            &[&ids],
        )
        .await
        .map(|_| ())
        .map_err(DbError::from)
}

/// `step` within [`STEP_TIMEOUT`] (and a little more, for the server's own
/// timeout to answer first); its failure as the step's description.
async fn within<T>(
    step: &'static str,
    work: impl Future<Output = Result<T, DbError>>,
) -> Result<T, &'static str> {
    match tokio::time::timeout(STEP_TIMEOUT + Duration::from_secs(1), work).await {
        Ok(Ok(done)) => Ok(done),
        Ok(Err(_)) | Err(_) => Err(step),
    }
}
