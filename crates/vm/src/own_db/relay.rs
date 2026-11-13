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
//! Bounds:
//!
//! - **one shipment per own database at a time** ([`Shipping`]): a request
//!   while one runs is remembered (`asked`), and the running one goes
//!   round again; so there is at most one task per own database, and the
//!   relay's tasks are bounded by the number of own databases;
//! - **[`AT_ONCE`] shipments at once** in the program, across own
//!   databases: each holds one connection to its own database and, per
//!   step, one of the platform database's pool (16): the rest of the pool
//!   stays for the Operation log. A shipment waits for its turn at most
//!   [`TURN_WAIT`], then fails (recorded) and leaves its outbox for the
//!   next one;
//! - **a batch** is at most [`BATCH_ROWS`] changes and, past its first,
//!   [`BATCH_BYTES`] of `old` and `new` as text, read by the own database
//!   as one JSON text and sent on as one parameter: what the program holds
//!   of row data at once per shipment is one batch (a row's size is bounded
//!   by the host's write limits);
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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Semaphore, watch};
use tokio_postgres::{Client, Config};

use crate::log;
use crate::platform_db::{Connection, DbError, PlatformDatabase};

/// The most changes one batch holds.
pub const BATCH_ROWS: i64 = 1_000;

/// The most bytes of `old` and `new` as text (`octet_length` of each's
/// `jsonb` text) one batch holds past its first change, which always goes,
/// so that one large change never stalls the outbox.
pub const BATCH_BYTES: i64 = 4 * 1024 * 1024;

/// The longest each step of a batch may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// Shipments at once across all own databases.
pub const AT_ONCE: usize = 4;

/// The longest a shipment waits for one of the [`AT_ONCE`] turns.
pub const TURN_WAIT: Duration = Duration::from_secs(60);

/// The longest opening the connection to the own database may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The batch read from the outbox, oldest first: the ids, and the rows as
/// one JSON array (of `jsonb_build_object` of each), cut at [`BATCH_ROWS`]
/// and, past the first, [`BATCH_BYTES`]. Both null when it is empty.
const READ_BATCH: &str = "
SELECT pg_catalog.array_agg(x.id ORDER BY x.id),
       pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object(
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
    /// Every request for a shipment takes the next number.
    asked: AtomicU64,
    /// A shipment task runs for this own database.
    running: AtomicBool,
    /// The greatest request number a finished pass covered, shipped or
    /// failed.
    passes: watch::Sender<Passes>,
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
            running: AtomicBool::new(false),
            passes: watch::Sender::new(Passes::default()),
        }
    }
}

impl Shipping {
    /// A request for a shipment: its number, and whether the caller must
    /// start the shipment task (none runs).
    pub fn ask(&self) -> (u64, bool) {
        let ticket = self.asked.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        let start = !self.running.swap(true, Ordering::AcqRel);
        (ticket, start)
    }

    /// Waits until a pass that began after request `ticket` was made has
    /// ended: `true` if it shipped everything the outbox held, `false` if it
    /// failed. The caller bounds the wait.
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

    /// A request could not even start a task: the passes that would have
    /// answered it are failed.
    pub fn give_up(&self, ticket: u64) {
        self.running.store(false, Ordering::Release);
        self.passes.send_modify(|p| p.failed = p.failed.max(ticket));
    }
}

/// What a shipment needs of the program: where Postgres is, the platform
/// database, and the turns.
pub struct Courier {
    pub platform: Arc<PlatformDatabase>,
    pub host: String,
    pub port: u16,
    pub turns: Semaphore,
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

/// The shipment task of one own database: passes until nothing more was
/// asked while one ran. Each pass ships until the outbox is empty or a step
/// fails; its end is told to whoever waits.
pub async fn run(courier: Arc<Courier>, route: Route) {
    let shipping = &route.known.shipping;
    loop {
        let ticket = shipping.asked.load(Ordering::Acquire);
        let shipped = courier.pass(&route).await;
        match shipped {
            Ok(_) => shipping
                .passes
                .send_modify(|p| p.shipped = p.shipped.max(ticket)),
            Err(step) => {
                log::record(
                    "history-not-shipped",
                    serde_json::json!({ "application": &*route.application, "problem": step }),
                );
                shipping
                    .passes
                    .send_modify(|p| p.failed = p.failed.max(ticket));
            }
        }
        shipping.running.store(false, Ordering::Release);
        // Asked meanwhile, by a caller that saw this task running and so
        // started none: go round again, unless another task has started.
        if shipping.asked.load(Ordering::Acquire) == ticket
            || shipping.running.swap(true, Ordering::AcqRel)
        {
            return;
        }
    }
}

impl Courier {
    /// One pass: batches until the outbox is empty. Answers how many
    /// changes it shipped, or the step that failed.
    async fn pass(&self, route: &Route) -> Result<u64, &'static str> {
        if !route.known.in_form.load(Ordering::Acquire) {
            return Err("the own database is not in this program's form");
        }
        let _turn = tokio::time::timeout(TURN_WAIT, self.turns.acquire())
            .await
            .map_err(|_| "waiting for a turn to ship: too many shipments at once")?
            .map_err(|_| "waiting for a turn to ship: the program is stopping")?;
        let connection = self
            .connect(route)
            .await
            .map_err(|_| "connecting to the own database as its platform role")?;
        let mut shipped: u64 = 0;
        loop {
            let (ids, rows) = within("reading the outbox", read_batch(connection.client())).await?;
            let (Some(ids), Some(rows)) = (ids, rows) else {
                return Ok(shipped);
            };
            within(
                "writing change_history",
                self.write_batch(&route.application, &rows),
            )
            .await?;
            within(
                "deleting what was shipped from the outbox",
                delete_batch(connection.client(), &ids),
            )
            .await?;
            shipped = shipped.saturating_add(u64::try_from(ids.len()).unwrap_or(u64::MAX));
        }
    }

    /// A connection to the own database as its platform role: the search
    /// path pinned to the catalog, and the server ending any statement or
    /// lock wait at [`STEP_TIMEOUT`].
    async fn connect(&self, route: &Route) -> Result<Connection, DbError> {
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
        Connection::open(&config, CONNECT_TIMEOUT).await
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
