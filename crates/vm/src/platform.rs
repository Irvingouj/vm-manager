//! The program's parts, put together: what `vm serve` serves with, and what
//! the local run (`vm request`, crate::tool's `local`) runs one request
//! with. One assembly for both callers, so that the local run is the
//! installation's own machinery pointed at another Postgres.
//!
//! In order: the platform database (connected, made if it is not there, its
//! structure brought up to date), the own databases, the Application
//! registry, the Resources and their Grants, all within `start` (a time
//! limit); then every own database brought to form and its outbox shipped;
//! Logs' writer, the Operation log, the Operation gateway and the Request
//! runner's engine threads.

use std::sync::Arc;
use std::time::Duration;

use crate::app_logs::{self, Logs, Writer};
use crate::executor;
use crate::gateway::{Gateway, PlatformGateway};
use crate::oplog::OperationLog;
use crate::own_db::OwnDatabases;
use crate::platform_db::{self, DatabaseAddress, PlatformDatabase, PoolLimits};
use crate::registry::Registry;
use crate::resources::Resources;
use crate::runner::{self, RequestRunner};

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
    pub logs: Arc<Logs>,
    /// The writer of Logs: owned here; dropping it ends its task, and
    /// [`Writer::finish`] waits for what is queued.
    pub writer: Writer,
    pub log: Arc<OperationLog>,
    pub gateway: Arc<PlatformGateway>,
    pub runner: RequestRunner,
}

impl Platform {
    /// Assembles the parts on the platform database at `address`. The
    /// error is a message for whoever started the program; it never holds
    /// the password.
    pub async fn open(address: &DatabaseAddress, limits: Limits) -> Result<Self, String> {
        let Limits {
            pool,
            executor,
            runner,
            start,
        } = limits;
        let started = tokio::time::timeout(start, async {
            let database = Arc::new(PlatformDatabase::open(address, &pool).await?);
            let unreadable = |e: platform_db::DbError| {
                format!(
                    "cannot read the Applications and Resources from the platform database \"{}\": {e}",
                    address.database()
                )
            };
            let own = Arc::new(
                OwnDatabases::load(Arc::clone(&database), address)
                    .await
                    .map_err(unreadable)?,
            );
            let registry = Registry::load(Arc::clone(&database), Arc::clone(&own))
                .await
                .map_err(unreadable)?;
            let resources = Resources::load(Arc::clone(&database), executor)
                .await
                .map_err(unreadable)?;
            Ok::<_, String>((database, own, registry, resources))
        })
        .await;
        let (database, own, registry, resources) = match started {
            Ok(started) => started?,
            Err(_) => {
                return Err(format!(
                    "the platform database \"{}\" on Postgres at {} was not ready within {} s",
                    address.database(),
                    address.server(),
                    start.as_secs()
                ));
            }
        };
        // Before serving: every own database in this program's form, its
        // migration role closed (each within its own time limits).
        own.bring_to_form().await;
        // Then what every outbox holds is shipped, without delaying serving
        // (blackbox/CONTRACT.md, "Shipping: When").
        own.ship_all();
        let registry = Arc::new(registry);
        let resources = Arc::new(resources);
        let (logs, writer) = app_logs::start(Arc::clone(&database));
        let logs = Arc::new(logs);
        let log = Arc::new(OperationLog::new(Arc::clone(&database)));
        let gateway = Arc::new(PlatformGateway::new(
            Arc::clone(&resources),
            Arc::clone(&own),
            Arc::clone(&log),
            tokio::runtime::Handle::current(),
        ));
        let as_gateway: Arc<dyn Gateway> = Arc::clone(&gateway) as Arc<dyn Gateway>;
        let runner = RequestRunner::start(&runner, &as_gateway)
            .map_err(|e| format!("cannot start the engine threads: {e}"))?;
        Ok(Self {
            database,
            own,
            registry,
            resources,
            logs,
            writer,
            log,
            gateway,
            runner,
        })
    }
}
