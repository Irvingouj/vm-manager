//! A small bounded pool of Postgres connections.
//!
//! Written here rather than taken from a crate because the program needs
//! three things a general pool hides: each connection's background task
//! owned by the connection (ended when it is dropped, never detached); a
//! timeout on every wait (checkout, connect, and the whole operation); and,
//! for later tickets, a pool per database and Access level with the same
//! small interface. It is one type and one method, [`Pool::run`].
//!
//! A connection is handed back only when the operation finished within its
//! time and the connection is still open. One that timed out is dropped
//! (its task ends with it), since what it was in the middle of is unknown.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, Config, NoTls};

use super::DbError;
use crate::log;

/// The pool's bounds. Each is a bound on connections or on time.
#[derive(Clone, Debug)]
pub struct PoolLimits {
    /// Connections open at once, idle or in use.
    pub size: usize,
    /// The longest an operation waits for a free connection.
    pub checkout: Duration,
    /// The longest opening a connection may take, login included.
    pub connect: Duration,
    /// The longest one operation may take once it has its connection.
    pub operation: Duration,
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            size: 16,
            checkout: Duration::from_secs(5),
            connect: Duration::from_secs(5),
            operation: Duration::from_secs(20),
        }
    }
}

/// One open connection and the task that drives it.
pub(crate) struct Connection {
    client: Client,
    /// Owned: aborted when the connection is dropped. It ends by itself
    /// when the client is dropped or the server closes the connection.
    task: JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Connection {
    /// Opens a connection within `limit`.
    pub(crate) async fn open(config: &Config, limit: Duration) -> Result<Self, DbError> {
        let (client, connection) = tokio::time::timeout(limit, config.connect(NoTls))
            .await
            .map_err(|_| DbError::timed_out("opening a connection", limit))?
            .map_err(DbError::from)?;
        let task = tokio::spawn(async move {
            if let Err(error) = connection.await {
                log::record(
                    "platform-database-connection-ended",
                    serde_json::json!({ "error": DbError::from(error).to_string() }),
                );
            }
        });
        Ok(Self { client, task })
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }
}

/// A bounded pool of connections to one database as one role.
pub struct Pool {
    /// Holds the password; never printed (no `Debug` on this type).
    config: Config,
    limits: PoolLimits,
    /// Bound: `limits.size` permits, one per connection open or being
    /// opened. When all are taken an operation waits up to
    /// `limits.checkout`, then fails.
    permits: Semaphore,
    /// Connections not in use. At most `limits.size`, since each one
    /// stands for a permit given back.
    idle: Mutex<Vec<Connection>>,
}

impl Pool {
    pub(crate) fn new(config: Config, limits: PoolLimits, first: Option<Connection>) -> Self {
        let size = limits.size.max(1);
        let mut idle = Vec::with_capacity(size);
        idle.extend(first);
        Self {
            config,
            limits,
            permits: Semaphore::new(size),
            idle: Mutex::new(idle),
        }
    }

    /// Runs `operation` on a connection of the pool, within the pool's
    /// time limits, and hands the connection back if it is still good.
    pub async fn run<T>(
        &self,
        operation: impl AsyncFnOnce(&mut Client) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let checkout = self.limits.checkout;
        let _permit = tokio::time::timeout(checkout, self.permits.acquire())
            .await
            .map_err(|_| DbError::timed_out("waiting for a free connection", checkout))?
            .map_err(|_| DbError::closed())?;
        let mut connection = match self.take_idle() {
            Some(connection) => connection,
            None => Connection::open(&self.config, self.limits.connect).await?,
        };
        let limit = self.limits.operation;
        match tokio::time::timeout(limit, operation(&mut connection.client)).await {
            Ok(result) => {
                if !connection.client.is_closed() {
                    self.give_back(connection);
                }
                result
            }
            Err(_) => Err(DbError::timed_out("an operation", limit)),
        }
    }

    fn take_idle(&self) -> Option<Connection> {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(connection) = idle.pop() {
            if !connection.client.is_closed() {
                return Some(connection);
            }
        }
        None
    }

    fn give_back(&self, connection: Connection) {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(connection);
    }
}
