//! The program's parts, put together: what `vibe serve` serves with, and what
//! the local run (`vibe request`, crate::tool's `local`) runs one request
//! with. One assembly for both callers, so that the local run is the
//! installation's own machinery pointed at another Postgres.
//!
//! In order: the platform database (connected, made if it is not there, its
//! structure brought up to date), the own databases, the Application
//! registry, the Resources and their Grants, all within `start` (a time
//! limit); then every own database brought to form and its outbox shipped;
//! Logs' writer, the usage's writer, the Operation log, the Operation
//! gateway and the Request runner's engine threads.

use std::sync::Arc;
use std::time::Duration;

use crate::app_logs::{self, Logs, Writer};
use crate::executor;
use crate::gateway::{Gateway, PlatformGateway};
use crate::oplog::OperationLog;
use crate::outbound::{Outbound, PrivateRanges};
use crate::own_db::OwnDatabases;
use crate::platform_db::{self, Cause, DatabaseAddress, OpenError, PlatformDatabase, PoolLimits};
use crate::registry::Registry;
use crate::resources::Resources;
use crate::runner::{self, RequestRunner};
use crate::storage::{self, Files, Root, Storage};
use crate::usage::{self, Usage};

/// The bounds the parts are made with.
pub struct Limits {
    /// The platform database's pool.
    pub pool: PoolLimits,
    /// Each Resource's pools.
    pub executor: executor::Limits,
    /// The Request runner.
    pub runner: runner::Limits,
    /// The longest the platform database may take to be connected, made,
    /// brought up to date and read.
    pub start: Duration,
}

/// The parts, assembled.
pub struct Platform {
    pub database: Arc<PlatformDatabase>,
    pub own: Arc<OwnDatabases>,
    pub registry: Arc<Registry>,
    pub resources: Arc<Resources>,
    /// The one list of hosts Applications may call, in memory.
    pub outbound: Arc<Outbound>,
    pub logs: Arc<Logs>,
    /// The writer of Logs: owned here; dropping it ends its task, and
    /// [`Writer::finish`] waits for what is queued.
    pub writer: Writer,
    pub log: Arc<OperationLog>,
    pub gateway: Arc<PlatformGateway>,
    pub runner: RequestRunner,
    /// What each request an Application's code was given is counted in.
    pub usage: Arc<Usage>,
    /// The writer of the counts: owned here; dropping it ends its task.
    pub usage_writer: usage::Writer,
    /// The file threads, the budget of bodies, the cursors' key and the
    /// storage root. Its threads are ended by [`Files::stop`], which the
    /// caller calls as it stops.
    pub storage: Arc<Storage>,
}

impl Platform {
    /// Assembles the parts on the platform database at `address`. The
    /// error is a message for whoever started the program; it never holds
    /// the password. `outbound_private` is the ranges the operator of the
    /// machine opened for outbound calls.
    pub async fn open(
        address: &DatabaseAddress,
        limits: Limits,
        storage_root: Option<Root>,
        outbound_private: PrivateRanges,
        itself: crate::outbound::Itself,
        vault: Option<Arc<crate::credentials::Vault>>,
    ) -> Result<Self, OpenError> {
        let files = Files::start()
            .map_err(|e| OpenError::new(None, format!("cannot start the file threads: {e}")))?;
        let opened = Self::assemble(
            address,
            limits,
            storage_root,
            outbound_private,
            itself,
            Arc::clone(&files),
            vault,
        )
        .await;
        if opened.is_err() {
            files.stop(storage::files::STOP_WAIT).await;
        }
        opened
    }

    async fn assemble(
        address: &DatabaseAddress,
        limits: Limits,
        storage_root: Option<Root>,
        outbound_private: PrivateRanges,
        itself: crate::outbound::Itself,
        files: Arc<Files>,
        vault: Option<Arc<crate::credentials::Vault>>,
    ) -> Result<Self, OpenError> {
        let Limits {
            pool,
            executor,
            runner,
            start,
        } = limits;
        let root = storage_root.map(Arc::new);
        let not_ready = || {
            OpenError::new(
                Some(Cause::TimedOut(start)),
                format!(
                    "the platform database \"{}\" on Postgres at {} was not ready within {} s",
                    address.database(),
                    address.server(),
                    platform_db::seconds_text(start)
                ),
            )
        };
        let database =
            match tokio::time::timeout(start, PlatformDatabase::open(address, &pool)).await {
                Ok(opened) => Arc::new(opened?),
                Err(_) => return Err(not_ready()),
            };
        // Before the parts that serve are read: what a platform database
        // from before this ticket holds in clear is sealed in place, one row
        // at a time, within its own 15 seconds (blackbox/CONTRACT.md,
        // "Resources and Grants in full: At start"). It is not part of the
        // start-up time above, and it never stops the program from serving.
        if let Some(vault) = &vault {
            vault.seal_old(&database).await;
        }
        let unreadable = |e: platform_db::DbError| {
            OpenError::of(
                &e,
                format!(
                    "cannot read the Applications and Resources from the platform database \"{}\": {e}",
                    address.database()
                ),
            )
        };
        let started = tokio::time::timeout(start, async {
            let own = Arc::new(
                OwnDatabases::load(
                    Arc::clone(&database),
                    address,
                    vault.as_ref().map(Arc::clone),
                )
                .await
                .map_err(unreadable)?,
            );
            // One bound on readings of what memory does not know, for both.
            let read_backs = crate::resources::read_backs();
            let registry = Registry::load(
                Arc::clone(&database),
                Arc::clone(&own),
                Arc::clone(&read_backs),
            )
            .await
            .map_err(unreadable)?;
            let resources = Resources::load(
                Arc::clone(&database),
                vault.as_ref().map(Arc::clone),
                executor,
                read_backs,
                Arc::clone(&files),
                root.as_ref().map(Arc::clone),
            )
            .await
            .map_err(unreadable)?;
            let outbound = Outbound::load(Arc::clone(&database))
                .await
                .map_err(unreadable)?;
            let cursors = cursor_sealer(&database).await.map_err(unreadable)?;
            Ok::<_, OpenError>((own, registry, resources, outbound, cursors))
        })
        .await;
        let (own, registry, resources, outbound, cursors) = match started {
            Ok(started) => started?,
            Err(_) => return Err(not_ready()),
        };
        // Before serving: every own database in this program's form, its
        // migration role closed (each within its own time limits).
        own.bring_to_form().await;
        // Then what every outbox holds is shipped, without delaying serving
        // (blackbox/CONTRACT.md, "Shipping: When").
        own.ship_all();
        let registry = Arc::new(registry);
        let resources = Arc::new(resources);
        let outbound = Arc::new(outbound);
        let (logs, writer) = app_logs::start(Arc::clone(&database));
        let logs = Arc::new(logs);
        let (usage, usage_writer) = usage::start(Arc::clone(&database));
        let log = Arc::new(OperationLog::new(Arc::clone(&database)));
        let storage = Arc::new(Storage {
            files,
            open: Arc::new(storage::Open::default()),
            cursors,
            root,
        });
        let gateway = Arc::new(PlatformGateway::new(
            Arc::clone(&resources),
            Arc::clone(&storage),
            Arc::clone(&own),
            Arc::clone(&log),
            Arc::clone(&outbound),
            outbound_private,
            itself,
            tokio::runtime::Handle::current(),
        ));
        let as_gateway: Arc<dyn Gateway> = Arc::clone(&gateway) as Arc<dyn Gateway>;
        let runner = RequestRunner::start(&runner, &as_gateway)
            .map_err(|e| OpenError::new(None, format!("cannot start the engine threads: {e}")))?;
        Ok(Self {
            database,
            own,
            registry,
            resources,
            outbound,
            logs,
            writer,
            log,
            gateway,
            runner,
            usage,
            usage_writer,
            storage,
        })
    }
}

/// What seals Storage lists' cursors, made from the key the platform
/// database keeps (step 17). The key's bytes are read into it and dropped:
/// they are held nowhere else and never printed.
async fn cursor_sealer(
    database: &PlatformDatabase,
) -> Result<storage::cursor::Sealer, platform_db::DbError> {
    database
        .pool()
        .run(async |client: &mut tokio_postgres::Client| {
            let row = client
                .query_one("SELECT key FROM storage_cursor_key", &[])
                .await
                .map_err(platform_db::DbError::from)?;
            row.try_get::<_, &[u8]>(0)
                .map(storage::cursor::Sealer::new)
                .map_err(platform_db::DbError::from)
        })
        .await
}
