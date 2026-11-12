//! The Own-database provisioner and migrator (ADR 0004, ADR 0013, ADR 0016;
//! blackbox/CONTRACT.md, "The Application's own database").
//!
//! Interface: [`OwnDatabases::load`] once at start; [`OwnDatabases::prepare`]
//! when a Version is sent: makes sure the Application's own database exists
//! and applies the Version's pending [`Migration`]s, or says which failed
//! and why ([`Problem`]); [`OwnDatabases::pool`], for the Operation gateway:
//! the bounded pool of the runtime role's connections to it.
//!
//! For the Application the platform numbers `n` (its id in the platform
//! database) and the platform database `<p>`, the own database is `<p>_app_<n>`
//! with two roles beside it, each with a password the platform generates,
//! stores in the platform database (table `own_databases`) and never
//! prints; Postgres is only ever sent its SCRAM verifier:
//!
//! - `<p>_app_<n>_owner` owns the database and every object in it, and is
//!   the login migrations are applied with;
//! - `<p>_app_<n>_runtime` is the login Application code runs as: `CONNECT`
//!   to this database alone, `USAGE` on its schemas, rows only (`SELECT`,
//!   `INSERT`, `UPDATE`, `DELETE`, and its sequences' `USAGE` and `SELECT`)
//!   in whatever the owner makes, through default privileges; no `CREATE`,
//!   no `TEMPORARY`, no `TRUNCATE`, member of no role.
//!
//! The platform's own role needs only `CREATEDB` and `CREATEROLE`: it
//! creates the two roles, makes itself able to `SET ROLE` to the owner role
//! (what Postgres asks of a non-superuser creating a database for another
//! role), and creates the database with the owner role as its owner.
//! Everything inside the database is done over a connection logged in as
//! the owner role: the comment, taking `PUBLIC`'s rights on the database
//! and its `public` schema away, the runtime role's rights, and the
//! migrations.
//!
//! Provisioning is repeatable: each step is skipped or done again harmlessly
//! if a crash left it half done, and the own database is marked ready in the
//! platform database only once every step is done. Concurrent provisioning
//! of one Application takes turns on a transaction-level advisory lock.
//!
//! Migrations: all pending ones in one transaction on the owner's
//! connection, each split at `drizzle-kit`'s `--> statement-breakpoint`,
//! with the record of which are applied (table `vm.migrations`, made by the
//! first transaction that applies one, which the runtime role cannot read)
//! written in that same transaction, so the record and the structure cannot
//! disagree. The transaction takes an advisory lock first, so two Versions
//! sent at once never interleave their migrations; the second then finds
//! the first's in the record. A list that does not begin with what the
//! record holds is [`Problem::Diverged`].

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::time::Instant;
use tokio_postgres::{CancelToken, Client, Config, NoTls};

use crate::executor::{self, Levels, Password, Source, Target};
use crate::log;
use crate::platform_db::{
    Connection, DatabaseAddress, DbError, PlatformDatabase, quote_identifier,
};

/// The line `drizzle-kit` puts between the statements of a migration.
pub const BREAKPOINT: &str = "--> statement-breakpoint";

/// The longest opening a connection as the owner role may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest deriving the two roles' SCRAM verifiers may take.
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest setting up a new own database's privileges may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest applying a Version's pending migrations may take, waiting
/// for another Version's migrations included. Below the server's limit on
/// one connection (60 s), so the sender is answered. Past it the statement
/// running is cancelled and the transaction rolled back.
pub const MIGRATION_TIMEOUT: Duration = Duration::from_secs(45);

/// After a cancel, how long the statement has to come back with
/// Postgres's answer before the connection is dropped (which rolls back).
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// Runtime pools held at once. Past it the least recently used is let go
/// (its idle connections close; operations running on it finish), and
/// made again when next needed. With [`RUNTIME_POOL_SIZE`], at most 256
/// connections to own databases at once.
pub const OWN_POOLS_HELD: usize = 64;

/// Connections of one own database's runtime pool, open or being opened.
pub const RUNTIME_POOL_SIZE: usize = 4;

/// The longest a role or database name may be: Postgres cuts longer ones.
const MAX_IDENTIFIER: usize = 63;

/// The advisory lock every migration transaction takes in its own
/// database: "vmmigrat".
const MIGRATIONS_LOCK: i64 = 0x766d_6d69_6772_6174;

/// One migration of a Version, as `drizzle-kit` generated it.
#[derive(Debug)]
pub struct Migration {
    pub name: String,
    pub sql: String,
    /// Of `sql`'s UTF-8 bytes, in lower-case hex.
    pub sha256: String,
}

impl Migration {
    pub fn new(name: String, sql: String) -> Self {
        let sha256 = crate::registry::hex(&Sha256::digest(sql.as_bytes()));
        Self { name, sql, sha256 }
    }
}

/// A migration as a Version's record or the own database's record holds
/// it.
#[derive(Debug)]
pub struct Recorded {
    pub name: String,
    pub sha256: String,
}

/// Whether `list` begins with exactly `earlier`: the same names and texts,
/// in the same order.
pub fn continues(earlier: &[Recorded], list: &[Migration]) -> bool {
    earlier.len() <= list.len()
        && earlier
            .iter()
            .zip(list)
            .all(|(e, m)| e.name == m.name && e.sha256 == m.sha256)
}

/// Why a Version's own database could not be prepared.
#[derive(Debug)]
pub enum Problem {
    /// The list does not begin with the migrations the own database has
    /// had.
    Diverged,
    /// A migration failed in the database; nothing was applied.
    Failed {
        migration: String,
        /// Postgres's SQLSTATE.
        code: String,
        /// Postgres's message.
        message: String,
    },
    /// The own database could not be made ready or reached. `step` is a
    /// fixed description, `code` Postgres's SQLSTATE if it gave one: never
    /// Postgres's message, which could name a role beside the word
    /// password.
    Unavailable {
        step: &'static str,
        code: Option<String>,
    },
}

impl Problem {
    fn unavailable(step: &'static str, error: &DbError) -> Self {
        Self::Unavailable {
            step,
            code: error.code().map(|c| c.code().to_owned()),
        }
    }

    fn postgres(step: &'static str, error: &tokio_postgres::Error) -> Self {
        Self::Unavailable {
            step,
            code: error.code().map(|c| c.code().to_owned()),
        }
    }
}

/// A password of an own database's role. No `Display`; its `Debug` shows
/// nothing of it.
struct Secret(String);

impl Secret {
    /// 64 hex digits from the operating system's random source (two
    /// version 4 UUIDs: 244 random bits).
    fn generate() -> Self {
        Self(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// The SCRAM-SHA-256 verifier Postgres stores: what is sent instead of
    /// the password.
    fn verifier(&self) -> String {
        postgres_protocol::password::scram_sha_256(self.0.as_bytes())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// An own database that is ready.
#[derive(Debug)]
struct Known {
    /// `n`: the Application's id.
    number: i64,
    owner_password: Secret,
    runtime_password: Secret,
}

/// The names of `n`'s database and roles.
struct Names {
    database: String,
    owner: String,
    runtime: String,
}

/// The runtime pools held, with when each was last used.
struct Pools {
    /// At most [`OWN_POOLS_HELD`].
    held: HashMap<Arc<str>, Held>,
    clock: u64,
}

struct Held {
    pool: Arc<executor::Pool>,
    used: u64,
}

pub struct OwnDatabases {
    platform: Arc<PlatformDatabase>,
    /// Where Postgres is: the platform database's server.
    host: String,
    port: u16,
    /// `<p>`, which begins every own database's and role's name.
    prefix: String,
    limits: executor::Limits,
    /// Every ready own database, by Application: one entry per Application
    /// that has had a Version, as the registry holds one serving Version
    /// per Application. Held only for a lookup or an insert, never across
    /// an await.
    known: RwLock<HashMap<Arc<str>, Arc<Known>>>,
    /// Held only to find, make or let go of a pool, never across an await.
    pools: Mutex<Pools>,
}

impl OwnDatabases {
    /// Reads every ready own database from the platform database. Connects
    /// to none of them.
    pub async fn load(
        platform: Arc<PlatformDatabase>,
        address: &DatabaseAddress,
    ) -> Result<Self, DbError> {
        let rows = platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.name, o.application_id, o.owner_password, o.runtime_password
                         FROM own_databases o JOIN applications a ON a.id = o.application_id
                         WHERE o.ready",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut known = HashMap::with_capacity(rows.len());
        for row in &rows {
            let name: &str = row.try_get(0).map_err(DbError::from)?;
            known.insert(
                Arc::from(name),
                Arc::new(Known {
                    number: row.try_get(1).map_err(DbError::from)?,
                    owner_password: Secret(row.try_get(2).map_err(DbError::from)?),
                    runtime_password: Secret(row.try_get(3).map_err(DbError::from)?),
                }),
            );
        }
        Ok(Self {
            platform,
            host: address.host().to_owned(),
            port: address.port(),
            prefix: address.database().to_owned(),
            limits: executor::Limits {
                size: RUNTIME_POOL_SIZE,
                ..executor::Limits::default()
            },
            known: RwLock::new(known),
            pools: Mutex::new(Pools {
                held: HashMap::with_capacity(OWN_POOLS_HELD),
                clock: 0,
            }),
        })
    }

    /// Makes sure the own database of the Application `name`, numbered
    /// `number`, exists, and applies the migrations of `list` it has not
    /// had, all or none.
    pub async fn prepare(
        &self,
        number: i64,
        name: &str,
        list: &[Migration],
    ) -> Result<(), Problem> {
        let known = match self.known(name) {
            Some((_, known)) => known,
            None => self.provision(number, name).await?,
        };
        self.migrate(&known, list).await
    }

    /// The pool of the runtime role's connections to `application`'s own
    /// database, if it has one.
    pub fn pool(&self, application: &str) -> Option<Arc<executor::Pool>> {
        let (key, known) = self.known(application)?;
        let mut pools = self.pools.lock().unwrap_or_else(PoisonError::into_inner);
        pools.clock = pools.clock.saturating_add(1);
        let now = pools.clock;
        if let Some(held) = pools.held.get_mut(application) {
            held.used = now;
            return Some(Arc::clone(&held.pool));
        }
        let names = self.names(known.number).ok()?;
        // Copied: the pool owns its login, made once per pool, not per
        // operation.
        let target = Target::new(
            self.host.clone(),
            self.port,
            names.database,
            names.runtime,
            Password::new(known.runtime_password.0.clone()),
        );
        let pool = Arc::new(executor::Pool::new(
            Source::OwnDatabase(Arc::clone(&key)),
            Levels::Both,
            target,
            self.limits.clone(),
        ));
        if pools.held.len() >= OWN_POOLS_HELD {
            let oldest = pools
                .held
                .iter()
                .min_by_key(|(_, held)| held.used)
                .map(|(name, _)| Arc::clone(name));
            if let Some(oldest) = oldest {
                pools.held.remove(&oldest);
            }
        }
        pools.held.insert(
            key,
            Held {
                pool: Arc::clone(&pool),
                used: now,
            },
        );
        Some(pool)
    }

    fn known(&self, application: &str) -> Option<(Arc<str>, Arc<Known>)> {
        self.known
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get_key_value(application)
            .map(|(name, known)| (Arc::clone(name), Arc::clone(known)))
    }

    fn names(&self, number: i64) -> Result<Names, Problem> {
        let database = format!("{}_app_{number}", self.prefix);
        let names = Names {
            owner: format!("{database}_owner"),
            runtime: format!("{database}_runtime"),
            database,
        };
        if names.runtime.len() > MAX_IDENTIFIER {
            return Err(Problem::Unavailable {
                step: "naming the own database: the platform database's name is too long for its own databases' names (at most 63 bytes with _app_<n>_runtime)",
                code: None,
            });
        }
        Ok(names)
    }

    /// Creates the own database of `name` and its roles, or finishes what
    /// an earlier attempt left half done.
    async fn provision(&self, number: i64, name: &str) -> Result<Arc<Known>, Problem> {
        let names = self.names(number)?;
        let (known, ready) = self.roles(number, &names).await?;
        if !ready {
            self.create_database(&names).await?;
            tokio::time::timeout(SETUP_TIMEOUT, self.set_up(&names, name, &known))
                .await
                .map_err(|_| Problem::Unavailable {
                    step: "setting up the own database's privileges: it took too long",
                    code: None,
                })??;
            self.platform
                .pool()
                .run(async |client: &mut Client| {
                    client
                        .execute(
                            "UPDATE own_databases SET ready = true WHERE application_id = $1",
                            &[&number],
                        )
                        .await
                        .map_err(DbError::from)
                })
                .await
                .map_err(|e| Problem::unavailable("recording the own database as ready", &e))?;
            log::record(
                "own-database-created",
                serde_json::json!({ "application": name, "database": names.database }),
            );
        }
        let known = Arc::new(known);
        let mut all = self.known.write().unwrap_or_else(PoisonError::into_inner);
        let known = all.entry(Arc::from(name)).or_insert(known);
        Ok(Arc::clone(known))
    }

    /// The Application's passwords, chosen now unless an earlier attempt
    /// chose them, and its two roles with them, unless it was ready
    /// already; whether it was.
    async fn roles(&self, number: i64, names: &Names) -> Result<(Known, bool), Problem> {
        let (known, ready) = self.passwords(number).await?;
        if ready {
            return Ok((known, true));
        }
        // Deriving a SCRAM verifier is CPU work (4096 rounds): off the
        // async workers. The task is awaited here, so it has an owner.
        let derived = tokio::task::spawn_blocking(move || {
            let verifiers = (
                known.owner_password.verifier(),
                known.runtime_password.verifier(),
            );
            (known, verifiers)
        });
        let (known, (owner, runtime)) = tokio::time::timeout(VERIFIER_TIMEOUT, derived)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(Problem::Unavailable {
                step: "deriving the own database's role verifiers",
                code: None,
            })?;
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // Two Versions sent at once take turns here.
                transaction
                    .execute(
                        "SELECT pg_advisory_xact_lock(hashtextextended('vm own database ' || $1::bigint::text, 0))",
                        &[&number],
                    )
                    .await
                    .map_err(DbError::from)?;
                for (role, verifier) in [(&names.owner, &owner), (&names.runtime, &runtime)] {
                    let exists: bool = transaction
                        .query_one(
                            "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)",
                            &[role],
                        )
                        .await
                        .and_then(|row| row.try_get(0))
                        .map_err(DbError::from)?;
                    // What an earlier attempt made is given the stored
                    // password again: a role of this name is this
                    // platform's, by its name.
                    let verb = if exists { "ALTER" } else { "CREATE" };
                    transaction
                        .batch_execute(&format!(
                            "{verb} ROLE {} LOGIN PASSWORD {}",
                            quote_identifier(role),
                            literal(verifier)
                        ))
                        .await
                        .map_err(DbError::from)?;
                }
                // A non-superuser may create a database owned by a role it
                // can SET ROLE to; nothing else of the owner's is inherited.
                transaction
                    .batch_execute(&format!(
                        "GRANT {} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE",
                        quote_identifier(&names.owner)
                    ))
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("creating the own database's roles", &e))?;
        Ok((known, false))
    }

    /// The Application's passwords as stored, storing new ones first if it
    /// has none; and whether its own database is ready.
    async fn passwords(&self, number: i64) -> Result<(Known, bool), Problem> {
        let fresh = (Secret::generate(), Secret::generate());
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "INSERT INTO own_databases (application_id, owner_password, runtime_password)
                         VALUES ($1, $2, $3) ON CONFLICT (application_id) DO NOTHING",
                        &[&number, &fresh.0.0, &fresh.1.0],
                    )
                    .await
                    .map_err(DbError::from)?;
                // A statement of its own, so that it sees the row whichever
                // of two attempts at once wrote it.
                let row = client
                    .query_one(
                        "SELECT owner_password, runtime_password, ready
                         FROM own_databases WHERE application_id = $1",
                        &[&number],
                    )
                    .await
                    .map_err(DbError::from)?;
                let known = Known {
                    number,
                    owner_password: Secret(row.try_get(0).map_err(DbError::from)?),
                    runtime_password: Secret(row.try_get(1).map_err(DbError::from)?),
                };
                Ok((known, row.try_get(2).map_err(DbError::from)?))
            })
            .await
            .map_err(|e| Problem::unavailable("choosing the own database's passwords", &e))
    }

    /// `CREATE DATABASE`, from `template0` so that nothing is in it; one
    /// an earlier attempt made is taken as it is.
    async fn create_database(&self, names: &Names) -> Result<(), Problem> {
        let statement = format!(
            "CREATE DATABASE {} OWNER {} TEMPLATE template0",
            quote_identifier(&names.database),
            quote_identifier(&names.owner)
        );
        let created = self
            .platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .batch_execute(&statement)
                    .await
                    .map_err(DbError::from)
            })
            .await;
        match created {
            Ok(()) => Ok(()),
            Err(error)
                if error
                    .code()
                    .is_some_and(|c| matches!(c.code(), "42P04" | "23505")) =>
            {
                Ok(())
            }
            Err(error) => Err(Problem::unavailable("creating the own database", &error)),
        }
    }

    /// As the owner role, in the own database: its comment, `PUBLIC`'s
    /// rights taken away, and the runtime role's rights, now and on what
    /// the owner makes later.
    async fn set_up(&self, names: &Names, application: &str, known: &Known) -> Result<(), Problem> {
        let mut connection = self.connect_owner(names, known).await?;
        let database = quote_identifier(&names.database);
        let owner = quote_identifier(&names.owner);
        let runtime = quote_identifier(&names.runtime);
        let setup = format!(
            "SELECT pg_advisory_xact_lock({MIGRATIONS_LOCK});
             COMMENT ON DATABASE {database} IS {comment};
             REVOKE ALL ON DATABASE {database} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {database} TO {runtime};
             REVOKE ALL ON SCHEMA public FROM PUBLIC;
             GRANT USAGE ON SCHEMA public TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                 GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                 GRANT USAGE, SELECT ON SEQUENCES TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                 GRANT USAGE ON SCHEMAS TO {runtime};",
            comment = literal(&format!("vm application {application}")),
        );
        let client = connection.client_mut();
        let transaction = client
            .transaction()
            .await
            .map_err(|e| Problem::postgres("setting up the own database's privileges", &e))?;
        transaction
            .batch_execute(&setup)
            .await
            .map_err(|e| Problem::postgres("setting up the own database's privileges", &e))?;
        transaction
            .commit()
            .await
            .map_err(|e| Problem::postgres("setting up the own database's privileges", &e))
    }

    /// A connection to the own database, logged in as its owner role.
    async fn connect_owner(&self, names: &Names, known: &Known) -> Result<Connection, Problem> {
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .user(&names.owner)
            .password(&known.owner_password.0)
            .dbname(&names.database)
            .application_name("vm")
            .connect_timeout(CONNECT_TIMEOUT);
        Connection::open(&config, CONNECT_TIMEOUT)
            .await
            .map_err(|e| {
                Problem::unavailable("connecting to the own database as its owner role", &e)
            })
    }

    /// Applies the migrations of `list` the own database has not had, in
    /// one transaction as the owner role, within [`MIGRATION_TIMEOUT`].
    async fn migrate(&self, known: &Known, list: &[Migration]) -> Result<(), Problem> {
        let names = self.names(known.number)?;
        let deadline = Instant::now() + MIGRATION_TIMEOUT;
        let mut connection = self.connect_owner(&names, known).await?;
        let cancel = connection.client().cancel_token();
        let client = connection.client_mut();
        let unavailable = |e: &tokio_postgres::Error| Problem::postgres("applying migrations", e);
        let timed_out = || Problem::Unavailable {
            step: "applying migrations: they did not finish in time",
            code: None,
        };
        let transaction = within(deadline, &cancel, client.transaction())
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        let lock = format!("SELECT pg_advisory_xact_lock({MIGRATIONS_LOCK})");
        within(deadline, &cancel, transaction.batch_execute(&lock))
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        // Before the first migration there is no record.
        let recorded: bool = within(
            deadline,
            &cancel,
            transaction.query_one("SELECT to_regclass('vm.migrations') IS NOT NULL", &[]),
        )
        .await
        .ok_or_else(timed_out)?
        .and_then(|row| row.try_get(0))
        .map_err(|e| unavailable(&e))?;
        let mut applied = Vec::new();
        if recorded {
            let rows = within(
                deadline,
                &cancel,
                transaction.query(
                    "SELECT name, sha256 FROM vm.migrations ORDER BY position",
                    &[],
                ),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
            for row in &rows {
                applied.push(Recorded {
                    name: row.try_get(0).map_err(|e| unavailable(&e))?,
                    sha256: row.try_get(1).map_err(|e| unavailable(&e))?,
                });
            }
        }
        if !continues(&applied, list) {
            return Err(Problem::Diverged);
        }
        let pending = list.get(applied.len()..).unwrap_or_default();
        if pending.is_empty() {
            // Nothing is changed; the transaction ends with the connection.
            return Ok(());
        }
        if !recorded {
            let record = format!(
                "CREATE SCHEMA IF NOT EXISTS vm;
                 CREATE TABLE IF NOT EXISTS vm.migrations (
                     position integer PRIMARY KEY,
                     name text NOT NULL UNIQUE,
                     sha256 text NOT NULL,
                     sql text NOT NULL,
                     applied_at timestamptz NOT NULL DEFAULT now()
                 );
                 REVOKE ALL ON SCHEMA vm FROM {runtime};
                 REVOKE ALL ON TABLE vm.migrations FROM {runtime};",
                runtime = quote_identifier(&names.runtime)
            );
            within(deadline, &cancel, transaction.batch_execute(&record))
                .await
                .ok_or_else(timed_out)?
                .map_err(|e| unavailable(&e))?;
        }
        let failed = |migration: &Migration, error: tokio_postgres::Error| match error.as_db_error()
        {
            Some(db) => Problem::Failed {
                migration: migration.name.clone(),
                code: db.code().code().to_owned(),
                message: db.message().to_owned(),
            },
            None => unavailable(&error),
        };
        for (index, migration) in pending.iter().enumerate() {
            for statement in migration.sql.split(BREAKPOINT) {
                if statement.trim().is_empty() {
                    continue;
                }
                within(deadline, &cancel, transaction.batch_execute(statement))
                    .await
                    .ok_or_else(timed_out)?
                    .map_err(|e| failed(migration, e))?;
            }
            let position =
                i32::try_from(applied.len() + index).map_err(|_| Problem::Unavailable {
                    step: "applying migrations: too many",
                    code: None,
                })?;
            // Right after the migration, so that one that broke the record
            // is the one named.
            within(
                deadline,
                &cancel,
                transaction.execute(
                    "INSERT INTO vm.migrations (position, name, sha256, sql) VALUES ($1, $2, $3, $4)",
                    &[&position, &migration.name, &migration.sha256, &migration.sql],
                ),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| failed(migration, e))?;
        }
        let last = pending.last();
        within(deadline, &cancel, transaction.commit())
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| match last {
                // A check deferred to the end fails at commit: the last
                // migration is named, as the one that left it so.
                Some(last) => failed(last, e),
                None => unavailable(&e),
            })
    }
}

/// `work` until `deadline`; past it the statement running is cancelled and
/// Postgres's answer to that awaited for [`CANCEL_GRACE`]. `None` when there
/// was none, or the statement succeeded anyway: the time is up either way.
async fn within<T>(
    deadline: Instant,
    cancel: &CancelToken,
    work: impl Future<Output = Result<T, tokio_postgres::Error>>,
) -> Option<Result<T, tokio_postgres::Error>> {
    let mut work = std::pin::pin!(work);
    if let Ok(done) = tokio::time::timeout_at(deadline, &mut work).await {
        return Some(done);
    }
    tokio::time::timeout(CANCEL_GRACE, cancel.cancel_query(NoTls))
        .await
        .ok();
    match tokio::time::timeout(CANCEL_GRACE, work).await {
        Ok(Err(error)) if error.as_db_error().is_some() => Some(Err(error)),
        Ok(_) | Err(_) => None,
    }
}

/// `text` as a Postgres string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_and_is_sent_as_a_verifier() {
        let secret = Secret::generate();
        assert_eq!(secret.0.len(), 64);
        assert_eq!(format!("{secret:?}"), "Secret(<hidden>)");
        let verifier = secret.verifier();
        assert!(verifier.starts_with("SCRAM-SHA-256$"), "{verifier}");
        assert!(!verifier.contains(&secret.0));
        assert_ne!(Secret::generate().0, secret.0);
    }

    #[test]
    fn a_list_continues_one_it_begins_with_exactly() {
        let list = |items: &[(&str, &str)]| -> Vec<Migration> {
            items
                .iter()
                .map(|(n, s)| Migration::new((*n).into(), (*s).into()))
                .collect()
        };
        let recorded = |migrations: &[Migration]| -> Vec<Recorded> {
            migrations
                .iter()
                .map(|m| Recorded {
                    name: m.name.clone(),
                    sha256: m.sha256.clone(),
                })
                .collect()
        };
        let earlier = recorded(&list(&[("a", "1"), ("b", "2")]));
        assert!(continues(&earlier, &list(&[("a", "1"), ("b", "2")])));
        assert!(continues(
            &earlier,
            &list(&[("a", "1"), ("b", "2"), ("c", "3")])
        ));
        assert!(continues(&[], &list(&[])));
        for other in [
            list(&[("a", "1")]),
            list(&[("a", "1"), ("b", "2 ")]),
            list(&[("a", "1"), ("c", "2")]),
            list(&[("b", "2"), ("a", "1")]),
            list(&[]),
        ] {
            assert!(!continues(&earlier, &other), "{other:?}");
        }
    }

    #[test]
    fn literals_double_their_quotes() {
        assert_eq!(literal("it's"), "'it''s'");
    }
}
