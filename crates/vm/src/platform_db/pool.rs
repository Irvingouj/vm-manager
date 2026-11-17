//! A small bounded pool of Postgres connections.
//!
//! Written here rather than taken from a crate because the program needs
//! three things a general pool hides: each connection's background task
//! owned by the connection (told to close the session when it is dropped,
//! and ended within a stated time, never left running); a
//! timeout on every wait (checkout, connect, and the whole operation); and,
//! for later tickets, a pool per database and Access level with the same
//! small interface. It is one type and one method, [`Pool::run`].
//!
//! A connection is handed back only when the operation finished within its
//! time and the connection is still open. One that timed out is dropped
//! (its task closes the session within a stated time), since what it was
//! in the middle of is unknown.

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

/// The connections to Postgres of this process still open: each one's task
/// counts itself while it lives (driving its socket), here and in the own
/// databases' relay. What lets go of its connections and must know they are
/// closed (the local run, before it stops its Postgres) waits for none
/// ([`wait_for_no_connection`]).
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One live connection's count in [`LIVE`], given back when dropped (the
/// connection's task ended or was aborted and dropped: its socket closed).
pub(crate) struct Live(());

impl Live {
    pub(crate) fn new() -> Self {
        LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Waits, up to `limit`, until no connection of this process is open (an
/// aborted connection's task is dropped, its socket closed, when the
/// runtime next runs it). Answers whether none is.
pub async fn wait_for_no_connection(limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if LIVE.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The longest a dropped connection's task goes on, closing it: reading the
/// answers to what was sent last (a prepared statement's close, say) and
/// sending Postgres the message that ends a session. Closing the socket
/// before those answers are read would make the server's side see the
/// connection reset rather than ended.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// One open connection and the task that drives it.
pub(crate) struct Connection {
    client: Client,
    /// Dropped with the connection: tells its task to stop waiting on
    /// requests and to close within [`CLOSE_WAIT`].
    _closing: tokio::sync::oneshot::Sender<()>,
    /// Owned while the connection lives. When it is dropped the task is not
    /// aborted: it ends by itself once the session is closed, at most
    /// [`CLOSE_WAIT`] later, and counts itself in [`LIVE`] until then.
    _task: JoinHandle<()>,
}

impl Connection {
    /// Opens a connection within `limit`.
    pub(crate) async fn open(config: &Config, limit: Duration) -> Result<Self, DbError> {
        let (client, connection) = tokio::time::timeout(limit, config.connect(NoTls))
            .await
            .map_err(|_| DbError::timed_out("opening a connection", limit))?
            .map_err(DbError::from)?;
        let live = Live::new();
        let (closing, dropped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _live = live;
            let mut connection = connection;
            let ended = tokio::select! {
                ended = &mut connection => Some(ended),
                _ = dropped => None,
            };
            // The client is dropped with the connection, so from here the
            // session reads what is still owed to it, says it ends, and
            // closes; a server that does not answer is given up on.
            let ended = match ended {
                Some(ended) => ended,
                None => match tokio::time::timeout(CLOSE_WAIT, connection).await {
                    Ok(ended) => ended,
                    Err(_) => return,
                },
            };
            if let Err(error) = ended {
                log::record(
                    "platform-database-connection-ended",
                    serde_json::json!({ "error": DbError::from(error).to_string() }),
                );
            }
        });
        Ok(Self {
            client,
            _closing: closing,
            _task: task,
        })
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn client_mut(&mut self) -> &mut Client {
        &mut self.client
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
