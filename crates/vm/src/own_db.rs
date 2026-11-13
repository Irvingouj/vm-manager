//! The Own-database provisioner and migrator (ADR 0004, ADR 0013, ADR 0016,
//! ADR 0017; blackbox/CONTRACT.md, "The Application's own database" and
//! "The platform's part of an own database is out of a migration's reach").
//!
//! Interface: [`OwnDatabases::load`] once at start, then
//! [`OwnDatabases::bring_to_form`] before the program serves;
//! [`OwnDatabases::prepare`] when a Version is sent: makes sure the
//! Application's own database exists and applies the Version's pending
//! [`Migration`]s, or says which failed or broke a rule and why
//! ([`Problem`]); [`OwnDatabases::pool`], for the Operation gateway: the
//! bounded pool of the runtime role's connections to it.
//!
//! For the Application the platform numbers `n` (its id in the platform
//! database) and the platform database `<p>`, the own database is `<p>_app_<n>`
//! with three roles beside it:
//!
//! - `<p>_app_<n>_platform`, the platform's own role for this database: it
//!   owns the database and the schema `vm` with everything in it (the keys,
//!   the Change history, the record of migrations, the capture and identity
//!   functions). It is an inheriting member of the migration role, which is
//!   what lets it put the capture triggers on the migration role's tables;
//!   it never reads an Application's table;
//! - `<p>_app_<n>_owner`, the migration role: owns what the migrations make
//!   and nothing else; `CONNECT` and `CREATE` on the database, `USAGE` and
//!   `CREATE` on `public`, in `vm` only what the host calls on its session
//!   (`own_db_schema.sql`). It cannot log in (`NOLOGIN`, no password)
//!   except while the platform applies migrations: then it is given a
//!   fresh random password, kept only in memory and valid for minutes, and
//!   is closed again (no login, no password, its sessions ended) whether
//!   the Version applied, failed or was refused, and at every start;
//! - `<p>_app_<n>_runtime`, the login Application code runs as: `CONNECT`
//!   to this database alone, `USAGE` on its schemas, rows only (`SELECT`,
//!   `INSERT`, `UPDATE`, `DELETE`, and its sequences' `USAGE` and `SELECT`)
//!   in whatever the migration role makes, through default privileges; no
//!   `CREATE`, no `TEMPORARY`, no `TRUNCATE`, member of no role.
//!
//! The platform's role and the runtime role have passwords the platform
//! generates, stores in the platform database (table `own_databases`) and
//! never prints; Postgres is only ever sent SCRAM verifiers.
//!
//! The platform's own role (that of `VM_DATABASE_URL`) needs only
//! `CREATEDB` and `CREATEROLE`: it creates the three roles, makes itself
//! able to `SET ROLE` to the platform's role of the database (what Postgres
//! asks of a non-superuser creating a database for another role), and
//! creates the database with that role as its owner. It never connects to
//! an own database. Everything inside one is done over a connection logged
//! in as the database's platform role (its comment, the privileges, the
//! schema `vm`, reading the keys and the record of migrations, ending the
//! migration role's sessions), except the migrations' transaction, which
//! runs on the migration role's own session: only that session sees what
//! its transaction has not committed. There the host's own work is calls
//! of functions of `vm` that pin their search path, whose results are kept
//! in the host's memory, never in the database (see [`OwnDatabases::migrate`]).
//!
//! Provisioning is repeatable: each step is skipped or done again harmlessly
//! if a crash left it half done, and the own database is marked ready in the
//! platform database only once every step is done. Concurrent provisioning
//! of one Application takes turns on a transaction-level advisory lock.
//!
//! The form of an own database (`own_databases.form`, [`FORM`]): which
//! form of the platform's part the program last brought it to. At start,
//! and before the first use of one that is not known to be in form, an own
//! database in an earlier form (no platform role, the database or `vm`
//! owned by the migration role, an older `own_db_schema.sql`) is brought
//! to this one, its history, keys and rows unchanged; and the migration
//! role is closed. One that cannot be is not served until it can.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_postgres::{CancelToken, Client, Config, GenericClient, NoTls, Row, Transaction};

use crate::executor::{self, IdentityKey, Levels, Password, Source, Target};
use crate::log;
use crate::migration_text;
use crate::oplog::WRITE_TIMEOUT;
use crate::platform_db::{
    Connection, DatabaseAddress, DbError, PlatformDatabase, quote_identifier,
};

/// The line `drizzle-kit` puts between the statements of a migration.
pub const BREAKPOINT: &str = "--> statement-breakpoint";

/// The form of the platform's part of an own database this program brings
/// every one to: 2 is three roles and `own_db_schema.sql` as it is. Raise
/// it whenever `own_db_schema.sql` or the roles change, so that every own
/// database is brought to the new form at the next start.
pub const FORM: i32 = 2;

/// The longest opening a connection to an own database may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest deriving the roles' SCRAM verifiers may take.
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest setting up or bringing to form one own database may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest applying a Version's pending migrations may take, waiting
/// for another Version's migrations and reading the migrations' text
/// included. Below the server's limit on one connection (60 s), so the
/// sender is answered. Past it the statement running is cancelled and the
/// transaction rolled back.
pub const MIGRATION_TIMEOUT: Duration = Duration::from_secs(45);

/// The longest closing the migration role again may take, after the
/// migrations, whatever became of them.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a password given to the migration role is valid (`VALID
/// UNTIL`): past it, should the program have been stopped before closing
/// the role again, the password no longer logs in.
const LOGIN_VALID: &str = "60 seconds";

/// How long ending one of the migration role's sessions is waited for, in
/// milliseconds (`pg_terminate_backend`'s second argument).
const TERMINATE_WAIT_MS: i64 = 2000;

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

/// How often, at most, the structure and the capture of an own database
/// are compared with what is kept, with a write (blackbox/CONTRACT.md, "What
/// something else changed").
const COMPARE_EVERY: Duration = Duration::from_secs(30);

/// Versions being sent at once in the program, across all Applications,
/// whether or not their senders still wait (blackbox/CONTRACT.md, "The
/// migration role's password": "At most 8"). Past it a Version is answered
/// unavailable at once, before anything of it is done.
const PREPARING: usize = 8;

/// The longest a Version's sender waits for it to be applied and
/// recorded: past [`MIGRATION_TIMEOUT`] and [`CLOSE_TIMEOUT`] together, and
/// below the server's limit on one connection (60 s).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(58);

/// The longest a role or database name may be: Postgres cuts longer ones.
const MAX_IDENTIFIER: usize = 63;

/// The advisory lock every migration transaction, and every setting up of
/// the platform's part, takes in its own database: "vmmigrat".
const MIGRATIONS_LOCK: i64 = 0x766d_6d69_6772_6174;

/// The platform's schema `vm`, made sure of (idempotent).
const PLATFORM_SCHEMA: &str = include_str!("own_db_schema.sql");

/// What the runtime role `runtime` and the migration role `owner` (both
/// quoted) may do in `vm`: use the schema, read `vm.history`, call
/// `vm.identity()` and `vm.transaction_key()`; the runtime role also the
/// check before a write's commit (`vm.before_commit()`); the migration role
/// also what the host calls on its session during migrations. Not the
/// capture's trigger functions: Postgres asks for `EXECUTE` on a trigger's
/// function of whoever makes the trigger, so a migration cannot make a
/// trigger of its own that calls one (`42501`). Nothing else.
fn vm_grants(owner: &str, runtime: &str) -> String {
    format!(
        "REVOKE ALL ON SCHEMA vm FROM {runtime}, {owner};
         GRANT USAGE ON SCHEMA vm TO {runtime}, {owner};
         REVOKE ALL ON ALL TABLES IN SCHEMA vm FROM {runtime}, {owner};
         REVOKE ALL ON ALL SEQUENCES IN SCHEMA vm FROM {runtime}, {owner};
         REVOKE ALL ON ALL FUNCTIONS IN SCHEMA vm FROM {runtime}, {owner};
         GRANT SELECT ON TABLE vm.history TO {runtime}, {owner};
         GRANT EXECUTE ON FUNCTION vm.identity(), vm.transaction_key() TO {runtime}, {owner};
         GRANT EXECUTE ON FUNCTION vm.capture_new_tables(), vm.capture_state(),
             vm.capture_problems(), vm.applied_migrations(),
             vm.structure_state(), vm.record_migration(integer, text, text),
             vm.application_owned(oid) TO {owner};
         GRANT EXECUTE ON FUNCTION vm.before_commit() TO {runtime};"
    )
}

/// The statements, made by Postgres with its own quoting, that give the
/// platform's role (the session's user) whatever in `vm` the migration
/// role (`$1`) owns: an own database in the form before ADR 0017, where
/// the migration role owned everything.
const TAKE_OVER_VM: &str = "
SELECT x.s FROM (
  SELECT 1 AS o, pg_catalog.format('ALTER SCHEMA vm OWNER TO %I', CURRENT_USER) AS s
    FROM pg_catalog.pg_namespace n
   WHERE n.nspname = 'vm' AND pg_catalog.pg_get_userbyid(n.nspowner) = $1
  UNION ALL
  SELECT 2, pg_catalog.format(CASE c.relkind WHEN 'm' THEN 'ALTER MATERIALIZED VIEW %s OWNER TO %I'
                                              ELSE 'ALTER TABLE %s OWNER TO %I' END,
                              c.oid::pg_catalog.regclass, CURRENT_USER)
    FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'vm' AND pg_catalog.pg_get_userbyid(c.relowner) = $1
     AND (c.relkind IN ('r', 'p', 'v', 'm')
          OR (c.relkind = 'S' AND NOT EXISTS (
                SELECT FROM pg_catalog.pg_depend d
                 WHERE d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                   AND d.objid = c.oid AND d.deptype IN ('a', 'i'))))
  UNION ALL
  SELECT 3, pg_catalog.format('ALTER ROUTINE %s OWNER TO %I', p.oid::pg_catalog.regprocedure, CURRENT_USER)
    FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
   WHERE n.nspname = 'vm' AND pg_catalog.pg_get_userbyid(p.proowner) = $1
  UNION ALL
  SELECT 4, pg_catalog.format(CASE t.typtype WHEN 'd' THEN 'ALTER DOMAIN %s OWNER TO %I'
                                             ELSE 'ALTER TYPE %s OWNER TO %I' END,
                              t.oid::pg_catalog.regtype, CURRENT_USER)
    FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
   WHERE n.nspname = 'vm' AND pg_catalog.pg_get_userbyid(t.typowner) = $1
     AND (t.typtype IN ('e', 'd', 'r')
          OR (t.typtype = 'c' AND EXISTS (SELECT FROM pg_catalog.pg_class c
                                           WHERE c.oid = t.typrelid AND c.relkind = 'c')))
) x ORDER BY x.o";

/// The own database's keys, read as the platform's role.
async fn read_key(client: &impl GenericClient) -> Result<IdentityKey, Problem> {
    let row: Row = client
        .query_one("SELECT k1, k2 FROM vm.keys", &[])
        .await
        .map_err(|e| Problem::postgres("reading the own database's keys", &e))?;
    key_of(&row)
}

fn key_of(row: &Row) -> Result<IdentityKey, Problem> {
    let unreadable = || Problem::Unavailable {
        step: "reading the own database's keys",
        code: None,
    };
    let k1: &[u8] = row.try_get(0).map_err(|_| unreadable())?;
    let k2: &[u8] = row.try_get(1).map_err(|_| unreadable())?;
    IdentityKey::from_parts(k1, k2).ok_or_else(unreadable)
}

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
    /// A migration broke a rule of the platform's; nothing was applied.
    Refused {
        /// The migration whose text broke the rule, or after which the
        /// check failed.
        migration: String,
        /// The rule, in kebab-case.
        rule: &'static str,
        /// What was found, where, and what to do instead.
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
    /// A fixed description, for the program's log.
    fn step(&self) -> &'static str {
        match self {
            Self::Diverged => "the migrations diverged",
            Self::Failed { .. } => "a migration failed",
            Self::Refused { .. } => "a migration was refused",
            Self::Unavailable { step, .. } => step,
        }
    }

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

    fn timed_out(step: &'static str) -> Self {
        Self::Unavailable { step, code: None }
    }

    /// Something that should not happen happened: `step` says what. Not a
    /// timeout, not Postgres's refusal.
    fn internal(step: &'static str) -> Self {
        Self::Unavailable { step, code: None }
    }

    fn refused(migration: &Migration, rule: &'static str, message: String) -> Self {
        Self::Refused {
            migration: migration.name.clone(),
            rule,
            message,
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

/// Derives the verifiers of `secrets` off the async workers (4096 rounds
/// each), within [`VERIFIER_TIMEOUT`]. The task is awaited here, so it has
/// an owner.
async fn verifiers<const N: usize>(secrets: [&Secret; N]) -> Result<[String; N], Problem> {
    // Copied: the blocking thread needs passwords of its own; once per
    // role change, not per operation.
    let owned: [Secret; N] = secrets.map(|s| Secret(s.0.clone()));
    let derived = tokio::task::spawn_blocking(move || owned.map(|s| s.verifier()));
    tokio::time::timeout(VERIFIER_TIMEOUT, derived)
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(Problem::Unavailable {
            step: "deriving the own database's role verifiers",
            code: None,
        })
}

/// An own database that is ready.
#[derive(Debug)]
struct Known {
    /// `n`: the Application's id.
    number: i64,
    platform_password: Secret,
    runtime_password: Secret,
    /// Its keys, once read.
    key: OnceLock<Arc<IdentityKey>>,
    /// Brought to [`FORM`], its migration role closed, in this program's
    /// run.
    in_form: AtomicBool,
    /// Held while the migration role may log in, and while the own
    /// database is brought to form: one at a time per own database, so
    /// that closing the migration role never ends another Version's
    /// migrations.
    turn: tokio::sync::Mutex<()>,
    /// Held while the structure and the capture are compared with what is
    /// kept (`own_database_states`), and while a Version's migrations are
    /// committed and what they leave is kept: one at a time, so that a
    /// comparison never reads the own database and what is kept on either
    /// side of a commit.
    comparing: tokio::sync::Mutex<()>,
    /// When this program last compared, or last tried to and could not, if
    /// it has: a comparison that could not be made is not tried again by
    /// every operation that follows, only at the next occasion.
    compared: Mutex<Option<Instant>>,
}

impl Known {
    fn new(number: i64, platform_password: Secret, runtime_password: Secret) -> Self {
        Self {
            number,
            platform_password,
            runtime_password,
            key: OnceLock::new(),
            in_form: AtomicBool::new(false),
            turn: tokio::sync::Mutex::new(()),
            comparing: tokio::sync::Mutex::new(()),
            compared: Mutex::new(None),
        }
    }

    /// Whether a comparison is due: never made in this run, or `every` ago.
    fn due(&self, every: Duration) -> bool {
        self.compared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none_or(|at| at.elapsed() >= every)
    }

    fn never_compared(&self) -> bool {
        self.compared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    }

    fn mark_compared(&self) {
        *self.compared.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }
}

/// The names of `n`'s database and roles.
struct Names {
    database: String,
    platform: String,
    owner: String,
    runtime: String,
}

/// What the platform database's server says of an own database's form.
struct Facts {
    form: i32,
    platform_role: bool,
    owned_by_platform_role: bool,
    owner_can_log_in: bool,
    owner_sessions: i64,
}

impl Facts {
    fn of(row: &Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            form: row.try_get("form")?,
            platform_role: row.try_get("platform_role")?,
            owned_by_platform_role: row.try_get("owned")?,
            owner_can_log_in: row.try_get("owner_login")?,
            owner_sessions: row.try_get("owner_sessions")?,
        })
    }

    /// Whether the own database must be brought to [`FORM`].
    fn earlier(&self) -> bool {
        self.form < FORM || !self.platform_role || !self.owned_by_platform_role
    }

    /// Whether the migration role must be closed.
    fn open(&self) -> bool {
        self.owner_can_log_in || self.owner_sessions > 0
    }
}

/// The facts of every ready own database (`$2` null) or of one (`$2` its
/// number), `$1` being `<p>`.
const FACTS: &str = "
SELECT a.name, o.form,
       pr.oid IS NOT NULL AS platform_role,
       coalesce(pg_catalog.pg_get_userbyid(d.datdba) = $1 || '_app_' || o.application_id::text || '_platform', false) AS owned,
       coalesce(ow.rolcanlogin, false) AS owner_login,
       (SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity s
         WHERE s.usename = $1 || '_app_' || o.application_id::text || '_owner') AS owner_sessions
  FROM own_databases o
  JOIN applications a ON a.id = o.application_id
  LEFT JOIN pg_catalog.pg_roles pr ON pr.rolname = $1 || '_app_' || o.application_id::text || '_platform'
  LEFT JOIN pg_catalog.pg_roles ow ON ow.rolname = $1 || '_app_' || o.application_id::text || '_owner'
  LEFT JOIN pg_catalog.pg_database d ON d.datname = $1 || '_app_' || o.application_id::text
 WHERE o.ready AND ($2::bigint IS NULL OR o.application_id = $2)";

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

/// What the capture triggers of each table were, by table: its name and
/// the identity of its triggers' catalog rows (`vm.capture_state()`).
type CaptureState = Vec<(u32, String, String)>;

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
    /// The Versions being sent, each a task of its own so that it runs to
    /// its end whether or not its sender waits for it
    /// ([`OwnDatabases::to_the_end`]); at most [`PREPARING`]. Each ends by
    /// itself within its time limits; any still running are ended when
    /// this is dropped. Held only to spawn or reap, never across an await.
    preparing: Mutex<JoinSet<()>>,
}

impl OwnDatabases {
    /// Reads every ready own database from the platform database, choosing
    /// a password for the platform's role of each that has none yet (one
    /// made before ADR 0017). Connects to none of them.
    pub async fn load(
        platform: Arc<PlatformDatabase>,
        address: &DatabaseAddress,
    ) -> Result<Self, DbError> {
        let rows = platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.name, o.application_id, o.platform_password, o.runtime_password
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
            let number: i64 = row.try_get(1).map_err(DbError::from)?;
            let platform_password =
                match row.try_get::<_, Option<String>>(2).map_err(DbError::from)? {
                    Some(password) => Secret(password),
                    None => Self::choose_platform_password(&platform, number).await?,
                };
            known.insert(
                Arc::from(name),
                Arc::new(Known::new(
                    number,
                    platform_password,
                    Secret(row.try_get(3).map_err(DbError::from)?),
                )),
            );
        }
        Ok(Self {
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
            preparing: Mutex::new(JoinSet::new()),
            platform,
        })
    }

    /// Stores a password for the platform's role of the own database `n`
    /// unless one is stored, and answers the one stored.
    async fn choose_platform_password(
        platform: &PlatformDatabase,
        number: i64,
    ) -> Result<Secret, DbError> {
        let fresh = Secret::generate();
        platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_one(
                        "UPDATE own_databases SET platform_password = coalesce(platform_password, $2)
                         WHERE application_id = $1 RETURNING platform_password",
                        &[&number, &fresh.0],
                    )
                    .await
                    .and_then(|row| row.try_get(0))
                    .map(Secret)
                    .map_err(DbError::from)
            })
            .await
    }

    /// Before the program serves: brings every ready own database to
    /// [`FORM`] and closes its migration role (the program may have been
    /// stopped while one was open). One that cannot be is logged and left
    /// out of form, so it is not served until it can be.
    pub async fn bring_to_form(&self) {
        let prefix = self.prefix.as_str();
        let rows = self
            .platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(FACTS, &[&prefix, &None::<i64>])
                    .await
                    .map_err(DbError::from)
            })
            .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                log::record(
                    "own-databases-not-checked",
                    serde_json::json!({ "error": error.to_string() }),
                );
                return;
            }
        };
        for row in &rows {
            let Ok(name) = row.try_get::<_, &str>("name") else {
                continue;
            };
            let Some((_, known)) = self.known(name) else {
                continue;
            };
            match Facts::of(row) {
                Ok(facts) if !facts.earlier() && !facts.open() => {
                    known.in_form.store(true, Ordering::Release);
                }
                _ => {
                    if let Err(problem) = self.ensure_form(name, &known).await {
                        log::record(
                            "own-database-not-in-form",
                            serde_json::json!({ "application": name, "problem": problem.step() }),
                        );
                    }
                }
            }
        }
    }

    /// Runs `work` to its end as a task of its own, held in
    /// [`OwnDatabases::preparing`], and answers what it came to, waited for
    /// up to [`ANSWER_TIMEOUT`]. For sending a Version: it runs to its end
    /// (the migrations committed and the Version recorded, or neither; the
    /// migration role closed again) also when its sender goes away before
    /// the answer (blackbox/CONTRACT.md, "The migration role's password").
    /// Each such task ends by itself within its own time limits. At most
    /// [`PREPARING`] at once; past it, refused at once.
    pub async fn to_the_end<T: Send + 'static>(
        &self,
        work: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, NotToTheEnd> {
        let (done, answer) = tokio::sync::oneshot::channel();
        {
            let mut preparing = self
                .preparing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while preparing.try_join_next().is_some() {}
            if preparing.len() >= PREPARING {
                return Err(NotToTheEnd::Busy);
            }
            preparing.spawn(async move {
                let outcome = work.await;
                // Nobody may be waiting any more: then the outcome is only
                // what the databases now hold.
                done.send(outcome).ok();
            });
        }
        match tokio::time::timeout(ANSWER_TIMEOUT, answer).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(NotToTheEnd::Failed(
                "sending the Version: its task ended without an answer",
            )),
            Err(_) => Err(NotToTheEnd::Failed(
                "sending the Version: it did not finish in time",
            )),
        }
    }

    /// Makes sure the own database of the Application `name`, numbered
    /// `number`, exists, and applies the migrations of `list` it has not
    /// had, all or none, as sent by the Contributor `sender`.
    pub async fn prepare(
        &self,
        number: i64,
        name: &str,
        sender: &str,
        list: &[Migration],
    ) -> Result<(), Problem> {
        let known = match self.known(name) {
            Some((_, known)) => known,
            None => self.provision(number, name).await?,
        };
        self.migrate(&known, name, sender, list).await
    }

    /// The pool of the runtime role's connections to `application`'s own
    /// database, if it has one in form; `write` when it is for a
    /// read-write transaction. Before it: the own database brought to form
    /// and its migration role closed if this program has not yet (or could
    /// not after a Version); its keys read if they are not known yet (the
    /// first operation after the program starts); and its structure and
    /// capture compared with what is kept (blackbox/CONTRACT.md, "What
    /// something else changed") at its first use in this run, and then
    /// with a write whenever the last comparison is [`COMPARE_EVERY`] old.
    pub async fn pool(&self, application: &str, write: bool) -> Option<Arc<executor::Pool>> {
        let (key, known) = self.known(application)?;
        let ready = match self.ensure_form(application, &known).await {
            Ok(()) => self.identity_key(&known).await,
            Err(problem) => Err(problem),
        };
        let identity = match ready {
            Ok(identity) => identity,
            Err(problem) => {
                log::record(
                    "own-database-unavailable",
                    serde_json::json!({ "application": application, "problem": problem.step() }),
                );
                return None;
            }
        };
        if known.never_compared() {
            self.compare_logged(application, &known, Occasion::FirstUse)
                .await;
        } else if write && known.due(COMPARE_EVERY) {
            self.compare_logged(application, &known, Occasion::Write)
                .await;
        }
        if let Some(pool) = self.held(application) {
            return Some(pool);
        }
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
        let pool = Arc::new(
            executor::Pool::new(
                Source::OwnDatabase(Arc::clone(&key)),
                Levels::Both,
                target,
                self.limits.clone(),
            )
            .with_identity_key(identity),
        );
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

    /// The pool held for `application`, marked as just used.
    fn held(&self, application: &str) -> Option<Arc<executor::Pool>> {
        let mut pools = self.pools.lock().unwrap_or_else(PoisonError::into_inner);
        pools.clock = pools.clock.saturating_add(1);
        let now = pools.clock;
        let held = pools.held.get_mut(application)?;
        held.used = now;
        Some(Arc::clone(&held.pool))
    }

    /// The own database's keys: as known, or read as its platform role.
    async fn identity_key(&self, known: &Known) -> Result<Arc<IdentityKey>, Problem> {
        if let Some(key) = known.key.get() {
            return Ok(Arc::clone(key));
        }
        let names = self.names(known.number)?;
        let read = tokio::time::timeout(SETUP_TIMEOUT, async {
            let connection = self.connect_platform(&names, known).await?;
            read_key(connection.client()).await
        })
        .await
        .map_err(|_| Problem::timed_out("reading the own database's keys: it took too long"))??;
        Ok(Arc::clone(known.key.get_or_init(|| Arc::new(read))))
    }

    /// [`OwnDatabases::compare`] at `occasion`, its failure logged
    /// (`own-database-not-compared`): the operation or Version that met it
    /// goes on as it would have, and it is made again at the next occasion
    /// (a start, a Version, or a write [`COMPARE_EVERY`] later;
    /// blackbox/CONTRACT.md, "What something else changed"). Holds its
    /// caller up for at most [`SETUP_TIMEOUT`], waiting for a comparison
    /// under way or a Version being committed included. A comparison that
    /// could not be made, or not begun for that wait, still counts as tried
    /// (`mark_compared`): the operations that follow within
    /// [`COMPARE_EVERY`] are not held up again by it.
    async fn compare_logged(&self, application: &str, known: &Known, occasion: Occasion) {
        let compared = tokio::time::timeout(SETUP_TIMEOUT, async {
            let _turn = match occasion {
                // A write does not wait for a comparison under way (or a
                // Version being committed).
                Occasion::Write => match known.comparing.try_lock() {
                    Ok(turn) => turn,
                    Err(_) => return Ok(()),
                },
                Occasion::FirstUse | Occasion::Version => known.comparing.lock().await,
            };
            // Another operation's comparison may have been made while this
            // one waited for its turn.
            let made = match occasion {
                Occasion::FirstUse => !known.never_compared(),
                Occasion::Write => !known.due(COMPARE_EVERY),
                Occasion::Version => false,
            };
            if made {
                return Ok(());
            }
            let compared = self.compare(application, known).await;
            known.mark_compared();
            compared
        })
        .await
        .unwrap_or_else(|_| {
            known.mark_compared();
            Err(Problem::timed_out(
                "comparing the own database's structure: it took too long",
            ))
        });
        if let Err(problem) = compared {
            log::record(
                "own-database-not-compared",
                serde_json::json!({ "application": application, "problem": problem.step() }),
            );
        }
    }

    /// Compares the structure and the capture of `application`'s own
    /// database with what is kept in the platform database
    /// (blackbox/CONTRACT.md, "What something else changed"), its
    /// `comparing` turn held by the caller. What is kept is the state after
    /// the last Version's migrations, or the pending one if the own
    /// database's record of migrations shows that their commit was made
    /// (the program may have been stopped between that commit and keeping
    /// it). Nothing kept yet: what is found is kept. When tables differ:
    /// the capture is put back (`vm.restore_capture()`), what is then found
    /// is kept, and it is said once, in the Operation log and the program's
    /// log. All of it as the platform's role of the database.
    async fn compare(&self, application: &str, known: &Known) -> Result<(), Problem> {
        let names = self.names(known.number)?;
        let mut connection = self.connect_platform(&names, known).await?;
        let (record, found) = read_structure(connection.client_mut()).await?;
        let expected = match self.kept(known.number).await? {
            None => {
                self.keep(known.number, &found).await?;
                return Ok(());
            }
            Some(Kept {
                pending: Some((pending, pending_record)),
                ..
            }) if pending_record == record => {
                self.keep(known.number, &pending).await?;
                pending
            }
            Some(kept) => kept.tables,
        };
        let tables = differing(&expected, &found);
        if tables.is_empty() {
            return Ok(());
        }
        let uncaptured: Vec<String> = {
            let transaction = connection
                .client_mut()
                .transaction()
                .await
                .map_err(|e| Problem::postgres("putting the capture back", &e))?;
            let rows = transaction
                .query("SELECT * FROM vm.restore_capture()", &[])
                .await
                .map_err(|e| Problem::postgres("putting the capture back", &e))?;
            transaction
                .commit()
                .await
                .map_err(|e| Problem::postgres("putting the capture back", &e))?;
            rows.iter()
                .filter_map(|row| row.try_get::<_, String>(0).ok())
                .collect()
        };
        let (_, now) = read_structure(connection.client_mut()).await?;
        // The entry and what is kept are written together: when the entry
        // cannot be written, nothing is kept, and the next comparison finds
        // the change again against what was kept before it (the capture
        // already put back has new triggers, so the same tables still
        // differ) and says it then, once.
        self.keep_said(known.number, application, &now).await?;
        log::record(
            "capture-changed-outside",
            serde_json::json!({
                "application": application,
                "tables": tables,
                "uncaptured": uncaptured,
            }),
        );
        Ok(())
    }

    /// Keeps `structure` as what `number`'s own database (of the
    /// Application `application`) is, nothing pending, in the same
    /// statement as the Operation log's `capture-check` entry that says it
    /// was changed outside: both or neither.
    async fn keep_said(
        &self,
        number: i64,
        application: &str,
        structure: &Structure,
    ) -> Result<(), Problem> {
        let tables = serde_json::json!(structure);
        let kept = tokio::time::timeout(
            WRITE_TIMEOUT,
            self.platform.pool().run(async |client: &mut Client| {
                client
                    .query_one(
                        "WITH entry AS (
                           INSERT INTO operation_log (application, version, actor, own_database,
                                                      operation, statement, outcome, error)
                           SELECT $2, coalesce(max(number), 0), 'platform', true,
                                  'capture-check', 'capture-check', 'failed', 'changed-outside'
                             FROM versions WHERE application_id = $1
                           RETURNING id)
                         , kept AS (
                           INSERT INTO own_database_states (application_id, tables)
                           SELECT $1, $3 FROM entry
                           ON CONFLICT (application_id) DO UPDATE
                           SET tables = excluded.tables, pending = NULL, pending_record = NULL
                           RETURNING application_id)
                         SELECT count(*) FROM kept",
                        &[&number, &application, &tables],
                    )
                    .await
                    .and_then(|row| row.try_get::<_, i64>(0))
                    .map_err(DbError::from)
            }),
        )
        .await
        .map_err(|_| Problem::timed_out("writing the capture-check entry: it took too long"))?
        .map_err(|e| Problem::unavailable("writing the capture-check entry", &e))?;
        if kept == 1 {
            Ok(())
        } else {
            Err(Problem::internal(
                "writing the capture-check entry: what is kept was not written with it",
            ))
        }
    }

    /// What is kept of `number`'s own database's structure, if anything.
    async fn kept(&self, number: i64) -> Result<Option<Kept>, Problem> {
        let row = self
            .platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT tables, pending, pending_record FROM own_database_states
                         WHERE application_id = $1",
                        &[&number],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("reading what is kept of the own database", &e))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let unreadable =
            |_| Problem::internal("reading what is kept of the own database: not as written");
        let tables = structure_of(&row.try_get::<_, serde_json::Value>(0).map_err(unreadable)?);
        let pending = row
            .try_get::<_, Option<serde_json::Value>>(1)
            .map_err(unreadable)?
            .map(|p| structure_of(&p));
        let pending_record: Option<String> = row.try_get(2).map_err(unreadable)?;
        Ok(Some(Kept {
            tables,
            pending: pending.zip(pending_record),
        }))
    }

    /// Keeps `structure` as what `number`'s own database is; nothing
    /// pending.
    async fn keep(&self, number: i64, structure: &Structure) -> Result<(), Problem> {
        let tables = serde_json::json!(structure);
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "INSERT INTO own_database_states (application_id, tables) VALUES ($1, $2)
                         ON CONFLICT (application_id) DO UPDATE
                         SET tables = excluded.tables, pending = NULL, pending_record = NULL",
                        &[&number, &tables],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await
            .map(|_| ())
            .map_err(|e| Problem::unavailable("keeping the own database's structure", &e))
    }

    /// Keeps `structure` as what `number`'s own database is once the
    /// migrations being applied are committed, with `record` the digest of
    /// its record of migrations then.
    async fn keep_pending(
        &self,
        number: i64,
        structure: &Structure,
        record: &str,
    ) -> Result<(), Problem> {
        let pending = serde_json::json!(structure);
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "INSERT INTO own_database_states (application_id, tables, pending, pending_record)
                         VALUES ($1, $2, $2, $3)
                         ON CONFLICT (application_id) DO UPDATE
                         SET pending = excluded.pending, pending_record = excluded.pending_record",
                        &[&number, &pending, &record],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await
            .map(|_| ())
            .map_err(|e| Problem::unavailable("keeping the own database's structure", &e))
    }

    /// The migrations whose record is `record` were committed: what was
    /// pending is what the own database is.
    async fn settle_pending(&self, number: i64, record: &str) -> Result<(), Problem> {
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "UPDATE own_database_states
                         SET tables = pending, pending = NULL, pending_record = NULL
                         WHERE application_id = $1 AND pending_record = $2",
                        &[&number, &record],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await
            .map(|_| ())
            .map_err(|e| Problem::unavailable("keeping the own database's structure", &e))
    }

    /// Keeps what `number`'s own database's structure is now, read as its
    /// platform role: after it was made, or brought to form (whose capture
    /// triggers the platform itself made).
    async fn keep_found(&self, names: &Names, known: &Known) -> Result<(), Problem> {
        let mut connection = self.connect_platform(names, known).await?;
        let (_, found) = read_structure(connection.client_mut()).await?;
        self.keep(known.number, &found).await
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
            platform: format!("{database}_platform"),
            owner: format!("{database}_owner"),
            runtime: format!("{database}_runtime"),
            database,
        };
        if names.platform.len() > MAX_IDENTIFIER {
            return Err(Problem::Unavailable {
                step: "naming the own database: the platform database's name is too long for its own databases' names (at most 63 bytes with _app_<n>_platform)",
                code: None,
            });
        }
        Ok(names)
    }

    /// Creates the own database of `name` and its roles, or finishes what
    /// an earlier attempt left half done.
    async fn provision(&self, number: i64, name: &str) -> Result<Arc<Known>, Problem> {
        let names = self.names(number)?;
        let (known, ready) = self.passwords(number).await?;
        if !ready {
            self.roles(&names, &known).await?;
            self.create_database(&names).await?;
            self.give_database(&names).await?;
            self.set_up(&names, name, &known).await?;
            self.keep_found(&names, &known).await?;
            self.platform
                .pool()
                .run(async |client: &mut Client| {
                    client
                        .execute(
                            "UPDATE own_databases SET ready = true, form = $2 WHERE application_id = $1",
                            &[&number, &FORM],
                        )
                        .await
                        .map_err(DbError::from)
                })
                .await
                .map_err(|e| Problem::unavailable("recording the own database as ready", &e))?;
            known.in_form.store(true, Ordering::Release);
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

    /// The Application's passwords as stored, storing new ones first if it
    /// has none; and whether its own database is ready.
    async fn passwords(&self, number: i64) -> Result<(Known, bool), Problem> {
        let fresh = (Secret::generate(), Secret::generate());
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "INSERT INTO own_databases (application_id, platform_password, runtime_password, form)
                         VALUES ($1, $2, $3, $4) ON CONFLICT (application_id) DO NOTHING",
                        &[&number, &fresh.0.0, &fresh.1.0, &FORM],
                    )
                    .await
                    .map_err(DbError::from)?;
                // A row from before ADR 0017 has no platform password yet.
                client
                    .execute(
                        "UPDATE own_databases SET platform_password = $2
                         WHERE application_id = $1 AND platform_password IS NULL",
                        &[&number, &fresh.0.0],
                    )
                    .await
                    .map_err(DbError::from)?;
                // A statement of its own, so that it sees the row whichever
                // of two attempts at once wrote it.
                let row = client
                    .query_one(
                        "SELECT platform_password, runtime_password, ready
                         FROM own_databases WHERE application_id = $1",
                        &[&number],
                    )
                    .await
                    .map_err(DbError::from)?;
                let known = Known::new(
                    number,
                    Secret(row.try_get(0).map_err(DbError::from)?),
                    Secret(row.try_get(1).map_err(DbError::from)?),
                );
                Ok((known, row.try_get(2).map_err(DbError::from)?))
            })
            .await
            .map_err(|e| Problem::unavailable("choosing the own database's passwords", &e))
    }

    /// The three roles, as the platform's role, with the stored passwords,
    /// whether or not an earlier attempt or an earlier form made them: the
    /// platform's role and the runtime role log in, the migration role does
    /// not and has no password. The platform's role may become the
    /// database's platform role (to create or take the database for it)
    /// and nothing else of it; the database's platform role inherits the
    /// migration role's rights (to put the capture on its tables) and may
    /// not become it.
    async fn roles(&self, names: &Names, known: &Known) -> Result<(), Problem> {
        let [platform_verifier, runtime_verifier] =
            verifiers([&known.platform_password, &known.runtime_password]).await?;
        let number = known.number;
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // Two at once take turns here.
                transaction
                    .execute(
                        "SELECT pg_advisory_xact_lock(hashtextextended('vm own database ' || $1::bigint::text, 0))",
                        &[&number],
                    )
                    .await
                    .map_err(DbError::from)?;
                for (role, rest) in [
                    (&names.platform, format!("LOGIN PASSWORD {}", literal(&platform_verifier))),
                    (&names.runtime, format!("LOGIN PASSWORD {}", literal(&runtime_verifier))),
                    (&names.owner, "NOLOGIN PASSWORD NULL".to_owned()),
                ] {
                    let exists: bool = transaction
                        .query_one(
                            "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)",
                            &[role],
                        )
                        .await
                        .and_then(|row| row.try_get(0))
                        .map_err(DbError::from)?;
                    // What an earlier attempt or form made is set again: a
                    // role of this name is this platform's, by its name.
                    let verb = if exists { "ALTER" } else { "CREATE" };
                    transaction
                        .batch_execute(&format!("{verb} ROLE {} {rest}", quote_identifier(role)))
                        .await
                        .map_err(DbError::from)?;
                }
                transaction
                    .batch_execute(&format!(
                        "GRANT {platform} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE;
                         GRANT {owner} TO {platform} WITH INHERIT TRUE, SET FALSE;",
                        platform = quote_identifier(&names.platform),
                        owner = quote_identifier(&names.owner),
                    ))
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("creating the own database's roles", &e))
    }

    /// `CREATE DATABASE`, from `template0` so that nothing is in it, owned
    /// by the platform's role of it; one an earlier attempt made is taken
    /// as it is.
    async fn create_database(&self, names: &Names) -> Result<(), Problem> {
        let statement = format!(
            "CREATE DATABASE {} OWNER {} TEMPLATE template0",
            quote_identifier(&names.database),
            quote_identifier(&names.platform)
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

    /// Gives the database to its platform role if the migration role owns
    /// it (the form before ADR 0017, or a database an earlier program
    /// began to make). The platform's role may give away only what it
    /// owns, so for this one transaction it inherits the migration role's
    /// rights, then gives that membership up again.
    async fn give_database(&self, names: &Names) -> Result<(), Problem> {
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let owner: Option<String> = transaction
                    .query_opt(
                        "SELECT pg_get_userbyid(datdba)::text FROM pg_database WHERE datname = $1",
                        &[&names.database],
                    )
                    .await
                    .map_err(DbError::from)?
                    .map(|row| row.try_get(0))
                    .transpose()
                    .map_err(DbError::from)?;
                match owner {
                    Some(owner) if owner == names.platform => {}
                    Some(owner) if owner == names.owner => {
                        transaction
                            .batch_execute(&format!(
                                "GRANT {owner} TO CURRENT_USER WITH INHERIT TRUE, SET FALSE;
                                 ALTER DATABASE {database} OWNER TO {platform};
                                 REVOKE {owner} FROM CURRENT_USER;",
                                owner = quote_identifier(&names.owner),
                                database = quote_identifier(&names.database),
                                platform = quote_identifier(&names.platform),
                            ))
                            .await
                            .map_err(DbError::from)?;
                    }
                    _ => {
                        return Err(DbError::unexpected(
                            "the own database is missing or owned by a role that is not the platform's",
                        ));
                    }
                }
                transaction.commit().await.map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("giving the own database to its platform role", &e))
    }

    /// As the platform's role of the database, in it, in one transaction
    /// within [`SETUP_TIMEOUT`]: its comment, `PUBLIC`'s rights taken away,
    /// the migration role's and the runtime role's rights, now and on what
    /// the migration role makes later; whatever in `vm` the migration role
    /// owns (the earlier form) taken over; the platform's schema `vm` made
    /// sure of, and the grants in it.
    async fn set_up(&self, names: &Names, application: &str, known: &Known) -> Result<(), Problem> {
        let step = "setting up the own database's privileges";
        tokio::time::timeout(SETUP_TIMEOUT, async {
            let mut connection = self.connect_platform(names, known).await?;
            let database = quote_identifier(&names.database);
            let owner = quote_identifier(&names.owner);
            let runtime = quote_identifier(&names.runtime);
            let privileges = format!(
                "COMMENT ON DATABASE {database} IS {comment};
                 ALTER DATABASE {database} RESET ALL;
                 REVOKE ALL ON DATABASE {database} FROM PUBLIC;
                 GRANT CONNECT, CREATE ON DATABASE {database} TO {owner};
                 GRANT CONNECT ON DATABASE {database} TO {runtime};
                 REVOKE ALL ON SCHEMA public FROM PUBLIC;
                 GRANT USAGE, CREATE ON SCHEMA public TO {owner};
                 GRANT USAGE ON SCHEMA public TO {runtime};
                 ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                     GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime};
                 ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                     GRANT USAGE, SELECT ON SEQUENCES TO {runtime};
                 ALTER DEFAULT PRIVILEGES FOR ROLE {owner}
                     GRANT USAGE ON SCHEMAS TO {runtime};",
                comment = literal(&format!("vm application {application}")),
            );
            let fail = |e: &tokio_postgres::Error| Problem::postgres(step, e);
            let transaction = connection
                .client_mut()
                .transaction()
                .await
                .map_err(|e| fail(&e))?;
            transaction
                .execute(
                    "SELECT pg_catalog.pg_advisory_xact_lock($1)",
                    &[&MIGRATIONS_LOCK],
                )
                .await
                .map_err(|e| fail(&e))?;
            transaction
                .batch_execute(&privileges)
                .await
                .map_err(|e| fail(&e))?;
            let take_over = transaction
                .query(TAKE_OVER_VM, &[&names.owner])
                .await
                .map_err(|e| fail(&e))?;
            for row in &take_over {
                let statement: &str = row.try_get(0).map_err(|e| fail(&e))?;
                transaction
                    .batch_execute(statement)
                    .await
                    .map_err(|e| fail(&e))?;
            }
            transaction
                .batch_execute(PLATFORM_SCHEMA)
                .await
                .map_err(|e| Problem::postgres("making sure of the platform's schema vm", &e))?;
            transaction
                .batch_execute(&vm_grants(&owner, &runtime))
                .await
                .map_err(|e| fail(&e))?;
            transaction.commit().await.map_err(|e| fail(&e))
        })
        .await
        .map_err(|_| Problem::timed_out("setting up the own database: it took too long"))?
    }

    /// Brings the own database of `application` to [`FORM`] and closes its
    /// migration role, unless this program has already; one at a time per
    /// own database.
    async fn ensure_form(&self, application: &str, known: &Known) -> Result<(), Problem> {
        if known.in_form.load(Ordering::Acquire) {
            return Ok(());
        }
        let _turn = tokio::time::timeout(SETUP_TIMEOUT, known.turn.lock())
            .await
            .map_err(|_| Problem::timed_out("waiting for the own database's migrations to end"))?;
        // Each of its steps has a time limit of its own; the whole of it
        // too, so that the turn is never held past one.
        tokio::time::timeout(
            MIGRATION_TIMEOUT,
            self.ensure_form_in_turn(application, known),
        )
        .await
        .map_err(|_| Problem::timed_out("bringing the own database to form: it took too long"))?
    }

    /// [`Self::ensure_form`], its turn already held.
    async fn ensure_form_in_turn(&self, application: &str, known: &Known) -> Result<(), Problem> {
        if known.in_form.load(Ordering::Acquire) {
            return Ok(());
        }
        let names = self.names(known.number)?;
        let prefix = self.prefix.as_str();
        let number = known.number;
        let facts = self
            .platform
            .pool()
            .run(async |client: &mut Client| {
                let row = client
                    .query_opt(FACTS, &[&prefix, &Some(number)])
                    .await
                    .map_err(DbError::from)?;
                row.map(|row| Facts::of(&row))
                    .transpose()
                    .map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("reading the own database's form", &e))?
            .ok_or(Problem::Unavailable {
                step: "reading the own database's form: it is not ready",
                code: None,
            })?;
        if facts.earlier() {
            self.roles(&names, known).await?;
            self.give_database(&names).await?;
            self.set_up(&names, application, known).await?;
            // What the earlier form's tables are now, the platform's own
            // capture made whole on them, is the baseline: nothing was kept
            // of them before (blackbox/CONTRACT.md, "What something else
            // changed").
            self.keep_found(&names, known).await?;
            self.platform
                .pool()
                .run(async |client: &mut Client| {
                    client
                        .execute(
                            "UPDATE own_databases SET form = $2, owner_password = NULL
                             WHERE application_id = $1",
                            &[&number, &FORM],
                        )
                        .await
                        .map_err(DbError::from)
                })
                .await
                .map_err(|e| Problem::unavailable("recording the own database's form", &e))?;
            log::record(
                "own-database-brought-to-form",
                serde_json::json!({ "application": application, "database": names.database, "form": FORM }),
            );
        }
        if facts.earlier() || facts.open() {
            tokio::time::timeout(CLOSE_TIMEOUT, self.close_owner(&names, known))
                .await
                .map_err(|_| {
                    Problem::timed_out("closing the migration role: it took too long")
                })??;
        }
        known.in_form.store(true, Ordering::Release);
        Ok(())
    }

    /// A connection to the own database, logged in as its platform role.
    async fn connect_platform(&self, names: &Names, known: &Known) -> Result<Connection, Problem> {
        self.connect(names, &names.platform, &known.platform_password, true)
            .await
            .map_err(|e| {
                Problem::unavailable("connecting to the own database as its platform role", &e)
            })
    }

    /// A connection to the own database, logged in as `role`.
    /// `pinned`: the session's search path is set in the startup packet to
    /// the catalog alone (`pg_catalog, pg_temp`), which no setting of the
    /// database's or the role's (`ALTER DATABASE ... SET`, `ALTER ROLE ...
    /// SET`) overrides, so that nothing of the Application's (a function,
    /// an operator or a type in `public`) is found by a statement of the
    /// platform's. Every session as the platform's role is; the migration
    /// role's is not, since migrations are written for the usual path.
    async fn connect(
        &self,
        names: &Names,
        role: &str,
        password: &Secret,
        pinned: bool,
    ) -> Result<Connection, DbError> {
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .user(role)
            .password(&password.0)
            .dbname(&names.database)
            .application_name("vm")
            .connect_timeout(CONNECT_TIMEOUT);
        if pinned {
            config.options("-c search_path=pg_catalog,pg_temp");
        }
        Connection::open(&config, CONNECT_TIMEOUT).await
    }

    /// Lets the migration role log in with a fresh password, valid for
    /// [`LOGIN_VALID`], its own settings (which a migration may have
    /// changed) reset; answers the password, which is kept nowhere else.
    async fn open_owner(&self, names: &Names) -> Result<Secret, Problem> {
        let fresh = Secret::generate();
        let [verifier] = verifiers([&fresh]).await?;
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let until: String = transaction
                    .query_one(
                        "SELECT (pg_catalog.clock_timestamp() + $1::text::pg_catalog.interval)::pg_catalog.text",
                        &[&LOGIN_VALID],
                    )
                    .await
                    .and_then(|row| row.try_get(0))
                    .map_err(DbError::from)?;
                transaction
                    .batch_execute(&format!(
                        "ALTER ROLE {owner} RESET ALL;
                         ALTER ROLE {owner} IN DATABASE {database} RESET ALL;
                         ALTER ROLE {owner} LOGIN PASSWORD {verifier} VALID UNTIL {until};",
                        owner = quote_identifier(&names.owner),
                        database = quote_identifier(&names.database),
                        verifier = literal(&verifier),
                        until = literal(&until),
                    ))
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("letting the migration role log in", &e))?;
        Ok(fresh)
    }

    /// Closes the migration role: it may no longer log in and has no
    /// password; then every session of it is ended, as the database's
    /// platform role (which may, inheriting its rights): one opened while
    /// it could log in, by whoever learnt a password a migration gave it,
    /// would otherwise outlive this.
    async fn close_owner(&self, names: &Names, known: &Known) -> Result<(), Problem> {
        self.platform
            .pool()
            .run(async |client: &mut Client| {
                client
                    .batch_execute(&format!(
                        "ALTER ROLE {} NOLOGIN PASSWORD NULL",
                        quote_identifier(&names.owner)
                    ))
                    .await
                    .map_err(DbError::from)
            })
            .await
            .map_err(|e| Problem::unavailable("closing the migration role", &e))?;
        let connection = self.connect_platform(names, known).await?;
        connection
            .client()
            .query(
                "SELECT pg_catalog.pg_terminate_backend(s.pid, $2)
                   FROM pg_catalog.pg_stat_activity s WHERE s.usename = $1",
                &[&names.owner, &TERMINATE_WAIT_MS],
            )
            .await
            .map_err(|e| Problem::postgres("ending the migration role's sessions", &e))?;
        Ok(())
    }

    /// Applies the migrations of `list` the own database has not had, as
    /// sent by the Contributor `sender` for the Application `application`,
    /// within [`MIGRATION_TIMEOUT`]:
    ///
    /// 1. as the platform's role: the own database brought to form, its
    ///    keys and its record of migrations read; the list must continue
    ///    the record;
    /// 2. every pending migration's text split into single statements, and
    ///    its rules checked ([`migration_text`]): nothing is sent unless
    ///    every one passes;
    /// 3. the migration role let in with a fresh password, and the pending
    ///    migrations applied on its session in one transaction
    ///    ([`Self::apply`]);
    /// 4. whatever became of them, the migration role closed again.
    async fn migrate(
        &self,
        known: &Known,
        application: &str,
        sender: &str,
        list: &[Migration],
    ) -> Result<(), Problem> {
        let deadline = Instant::now() + MIGRATION_TIMEOUT;
        let timed_out = || Problem::timed_out("applying migrations: they did not finish in time");
        let _turn = tokio::time::timeout_at(deadline, known.turn.lock())
            .await
            .map_err(|_| timed_out())?;
        tokio::time::timeout_at(deadline, self.ensure_form_in_turn(application, known))
            .await
            .map_err(|_| timed_out())??;
        // What something else changed is found, said and put right before
        // the Version's migrations (blackbox/CONTRACT.md, "What something
        // else changed"); one that cannot be compared now is at the next
        // occasion.
        tokio::time::timeout_at(
            deadline,
            self.compare_logged(application, known, Occasion::Version),
        )
        .await
        .ok();
        let names = self.names(known.number)?;
        let (key, applied) = tokio::time::timeout_at(deadline, async {
            let connection = self.connect_platform(&names, known).await?;
            let key = match known.key.get() {
                Some(key) => Arc::clone(key),
                None => Arc::new(read_key(connection.client()).await?),
            };
            let rows = connection
                .client()
                .query(
                    "SELECT name, sha256 FROM vm.migrations ORDER BY position",
                    &[],
                )
                .await
                .map_err(|e| Problem::postgres("reading the record of migrations", &e))?;
            let mut applied = Vec::with_capacity(rows.len());
            for row in &rows {
                applied.push(Recorded {
                    name: row
                        .try_get(0)
                        .map_err(|e| Problem::postgres("reading the record of migrations", &e))?,
                    sha256: row
                        .try_get(1)
                        .map_err(|e| Problem::postgres("reading the record of migrations", &e))?,
                });
            }
            Ok::<_, Problem>((key, applied))
        })
        .await
        .map_err(|_| timed_out())??;
        if !continues(&applied, list) {
            return Err(Problem::Diverged);
        }
        let pending = list.get(applied.len()..).unwrap_or_default();
        if pending.is_empty() {
            known.key.get_or_init(|| key);
            return Ok(());
        }
        let plans = tokio::time::timeout_at(deadline, plan(pending))
            .await
            .map_err(|_| timed_out())??;
        let outcome = match tokio::time::timeout_at(deadline, self.open_owner(&names))
            .await
            .map_err(|_| timed_out())
        {
            Ok(Ok(password)) => {
                let work = Work {
                    names: &names,
                    known,
                    password: &password,
                    key: &key,
                    application,
                    sender,
                    list,
                    checked_from: applied.len(),
                    plans: &plans,
                    deadline,
                };
                self.apply(&work).await
            }
            Ok(Err(problem)) | Err(problem) => Err(problem),
        };
        let closed = tokio::time::timeout(CLOSE_TIMEOUT, self.close_owner(&names, known))
            .await
            .unwrap_or_else(|_| {
                Err(Problem::timed_out(
                    "closing the migration role: it took too long",
                ))
            });
        if let Err(problem) = closed {
            // Tried again before the own database is next used.
            known.in_form.store(false, Ordering::Release);
            log::record(
                "own-database-migration-role-not-closed",
                serde_json::json!({ "application": application, "problem": problem.step() }),
            );
        }
        if outcome.is_ok() {
            known.key.get_or_init(|| key);
        }
        outcome
    }

    /// The migrations' transaction, on the migration role's own session:
    ///
    /// - the advisory lock, then the record of migrations read again (as
    ///   `vm.applied_migrations()`); the list must still continue it;
    /// - per pending migration: its identity attached (`actor`
    ///   `migration`); the capture triggers' catalog rows read
    ///   (`vm.capture_state()`) and kept here; each statement sent alone
    ///   over the extended protocol, the transaction's key
    ///   (`vm.transaction_key()`) asked after each and required unchanged;
    ///   `SET CONSTRAINTS ALL IMMEDIATE` and `CLOSE ALL`, so that nothing of
    ///   the migration's is left to run at the commit; the capture
    ///   triggers' rows read again and compared (`capture-changed`); the
    ///   capture put on the tables it made (`vm.capture_new_tables()`); the
    ///   rest of the checks (`vm.capture_problems()`);
    /// - the platform's own identity attached (`actor` `platform`), the
    ///   pending migrations recorded (`vm.record_migration`), the record
    ///   read back and compared with what was written; the commit.
    ///
    /// Every statement the host sends here calls a function by its schema,
    /// or is `SET CONSTRAINTS` or `CLOSE`, with values as parameters: no
    /// name, operator or type a migration could put first on the search
    /// path is looked up.
    async fn apply(&self, work: &Work<'_>) -> Result<(), Problem> {
        let deadline = work.deadline;
        let timed_out = || Problem::timed_out("applying migrations: they did not finish in time");
        let unavailable = |e: &tokio_postgres::Error| Problem::postgres("applying migrations", e);
        let mut connection = tokio::time::timeout_at(
            deadline,
            self.connect(work.names, &work.names.owner, work.password, false),
        )
        .await
        .map_err(|_| timed_out())?
        .map_err(|e| {
            Problem::unavailable("connecting to the own database as its migration role", &e)
        })?;
        let cancel = connection.client().cancel_token();
        let client = connection.client_mut();
        let transaction = within(deadline, &cancel, client.transaction())
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        within(
            deadline,
            &cancel,
            transaction.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock($1)",
                &[&MIGRATIONS_LOCK],
            ),
        )
        .await
        .ok_or_else(timed_out)?
        .map_err(|e| unavailable(&e))?;
        let applied = within(deadline, &cancel, read_record(&transaction))
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        let recorded: Vec<Recorded> = applied
            .iter()
            .map(|(_, name, sha256, _)| Recorded {
                name: name.clone(),
                sha256: sha256.clone(),
            })
            .collect();
        if !continues(&recorded, work.list) {
            return Err(Problem::Diverged);
        }
        // Only what was checked is sent: the record cannot have shrunk
        // since it was read (the turn is held), but if it had, what is now
        // pending was not all checked.
        let skip = applied
            .len()
            .checked_sub(work.checked_from)
            .ok_or(Problem::Unavailable {
                step: "applying migrations: the record of migrations changed meanwhile",
                code: None,
            })?;
        let pending = work.list.get(applied.len()..).unwrap_or_default();
        let plans = work.plans.get(skip..).unwrap_or_default();
        if pending.is_empty() {
            return Ok(());
        }
        let transaction_key: String = within(
            deadline,
            &cancel,
            transaction.query_one("SELECT vm.transaction_key()", &[]),
        )
        .await
        .ok_or_else(timed_out)?
        .and_then(|row| row.try_get(0))
        .map_err(|e| unavailable(&e))?;
        let failed = |migration: &Migration, error: tokio_postgres::Error| match error.as_db_error()
        {
            Some(db) => Problem::Failed {
                migration: migration.name.clone(),
                code: db.code().code().to_owned(),
                message: db.message().to_owned(),
            },
            None => unavailable(&error),
        };
        for (migration, statements) in pending.iter().zip(plans) {
            // The migration's own identity, valid in this transaction only.
            let who = executor::identity(
                work.sender,
                work.application,
                "migration",
                None,
                Some(&migration.name),
            );
            let ctx = work.key.attach(&transaction_key, &who);
            within(
                deadline,
                &cancel,
                transaction.execute("SELECT pg_catalog.set_config('vm.ctx', $1, true)", &[&ctx]),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
            let before = within(deadline, &cancel, capture_state(&transaction))
                .await
                .ok_or_else(timed_out)?
                .map_err(|e| failed(migration, e))?;
            for range in statements {
                let Some(statement) = migration.sql.get(range.clone()) else {
                    return Err(Problem::internal(
                        "applying migrations: a statement's place in its migration was lost",
                    ));
                };
                // The statement and the question of the transaction's key
                // after it travel together: each its own extended-protocol
                // exchange (one statement per Parse, which Postgres refuses
                // to hold two), sent at once, answered in order. If the
                // statement fails, the question fails after it.
                let (ran, key_now) = within(deadline, &cancel, async {
                    let (ran, key_now) = tokio::join!(
                        transaction.query_typed(statement, &[]),
                        transaction.query_typed_one("SELECT vm.transaction_key()", &[]),
                    );
                    Ok((ran, key_now))
                })
                .await
                .ok_or_else(timed_out)?
                .map_err(|e| unavailable(&e))?;
                ran.map_err(|e| failed(migration, e))?;
                let key_now: Option<String> = key_now.and_then(|row| row.try_get(0)).ok();
                if key_now.as_deref() != Some(transaction_key.as_str()) {
                    return Err(Problem::refused(
                        migration,
                        "transaction-control",
                        "a statement of the migration ended the platform's transaction: the platform applies all of a Version's migrations in one transaction that it begins and commits itself; remove whatever ends, commits or chains it".to_owned(),
                    ));
                }
            }
            // Nothing of the migration's may be left to run at the commit,
            // after the checks.
            for settle in ["SET CONSTRAINTS ALL IMMEDIATE", "CLOSE ALL"] {
                within(deadline, &cancel, transaction.execute(settle, &[]))
                    .await
                    .ok_or_else(timed_out)?
                    .map_err(|e| failed(migration, e))?;
            }
            let after = within(deadline, &cancel, capture_state(&transaction))
                .await
                .ok_or_else(timed_out)?
                .map_err(|e| failed(migration, e))?;
            if let Some(table) = changed(&before, &after) {
                return Err(Problem::refused(
                    migration,
                    "capture-changed",
                    format!(
                        "a Change history capture trigger of the table {table} was disabled, dropped, renamed or replaced during the migration (also when it is put back as it was, rows changed meanwhile have no history): change rows with the capture as it is, and leave the platform's triggers (named vm_capture_...) alone"
                    ),
                ));
            }
            // The tables it made are captured from its end on.
            within(
                deadline,
                &cancel,
                transaction.query_one("SELECT vm.capture_new_tables()", &[]),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| failed(migration, e))?;
            let problems = within(
                deadline,
                &cancel,
                transaction.query("SELECT * FROM vm.capture_problems()", &[]),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| failed(migration, e))?;
            if let Some(row) = problems.first() {
                let rule: &str = row.try_get(0).map_err(|e| unavailable(&e))?;
                let message: String = row.try_get(1).map_err(|e| unavailable(&e))?;
                let rule = match rule {
                    "capture-missing" => "capture-missing",
                    "deferrable" => "deferrable",
                    "json-cast" => "json-cast",
                    "in-vm" => "in-vm",
                    "security-definer" => "security-definer",
                    "index-code" => "index-code",
                    "type-code" => "type-code",
                    _ => {
                        return Err(Problem::internal(
                            "applying migrations: a check answered a rule the program does not know",
                        ));
                    }
                };
                return Err(Problem::refused(migration, rule, message));
            }
        }
        // The record, under the platform's own identity, which no migration
        // can attach; then read back.
        let platform = executor::identity(work.sender, work.application, "platform", None, None);
        let ctx = work.key.attach(&transaction_key, &platform);
        within(
            deadline,
            &cancel,
            transaction.execute("SELECT pg_catalog.set_config('vm.ctx', $1, true)", &[&ctx]),
        )
        .await
        .ok_or_else(timed_out)?
        .map_err(|e| unavailable(&e))?;
        let last = pending.last();
        for (index, migration) in pending.iter().enumerate() {
            let position = i32::try_from(applied.len().saturating_add(index))
                .map_err(|_| Problem::internal("applying migrations: more than 2^31 of them"))?;
            within(
                deadline,
                &cancel,
                transaction.execute(
                    "SELECT vm.record_migration($1, $2, $3)",
                    &[&position, &migration.name, &migration.sql],
                ),
            )
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| failed(migration, e))?;
        }
        let record = within(deadline, &cancel, read_record(&transaction))
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        let expected = applied
            .iter()
            .map(|(_, name, sha256, _)| (name.as_str(), sha256.as_str()))
            .chain(pending.iter().map(|m| (m.name.as_str(), m.sha256.as_str())));
        let as_written = record.len() == applied.len().saturating_add(pending.len())
            && record.iter().zip(expected).enumerate().all(
                |(index, ((place, name, sha256, sql_sha256), (want_name, want_sha256)))| {
                    usize::try_from(*place).is_ok_and(|p| p == index)
                        && name == want_name
                        && sha256 == want_sha256
                        && sql_sha256 == want_sha256
                },
            );
        if !as_written {
            return Err(Problem::Unavailable {
                step: "applying migrations: the record of migrations did not read back as written",
                code: None,
            });
        }
        // What the structure and the capture are once this commits, kept
        // in the platform database before the commit with the record it
        // goes with: a comparison after a crash between the two knows by
        // the record whether the commit was made.
        let structure = within(deadline, &cancel, structure_rows(&transaction))
            .await
            .ok_or_else(timed_out)?
            .map_err(|e| unavailable(&e))?;
        let record = record_digest(
            record
                .iter()
                .map(|(_, name, sha256, _)| (name.as_str(), sha256.as_str())),
        );
        // No comparison runs from here until what is kept is settled.
        let _comparing = tokio::time::timeout_at(deadline, work.known.comparing.lock())
            .await
            .map_err(|_| timed_out())?;
        tokio::time::timeout_at(
            deadline,
            self.keep_pending(work.known.number, &structure, &record),
        )
        .await
        .map_err(|_| timed_out())??;
        // The host's last statement before the commit, after everything of
        // the migrations' can no longer run: no password a migration gave
        // the role is good from the commit on (blackbox/CONTRACT.md, "The
        // migration role's password").
        let committed = match within(
            deadline,
            &cancel,
            transaction.batch_execute("ALTER ROLE CURRENT_USER PASSWORD NULL"),
        )
        .await
        {
            Some(Ok(())) => within(deadline, &cancel, transaction.commit())
                .await
                .ok_or_else(timed_out)
                .and_then(|ended| {
                    ended.map_err(|e| match last {
                        // Should something still fail at the commit, the
                        // last migration is named, as the one that left it
                        // so.
                        Some(last) => failed(last, e),
                        None => unavailable(&e),
                    })
                }),
            Some(Err(e)) => Err(unavailable(&e)),
            None => Err(timed_out()),
        };
        // Committed: what was pending is what the own database is. Should
        // that fail, or the commit not be known to have failed, what is
        // pending stays: the next comparison settles it by the record (a
        // pending state whose record never comes to be is never used, and
        // is replaced by the next Version's).
        if committed.is_ok() {
            let settled = tokio::time::timeout(
                CLOSE_TIMEOUT,
                self.settle_pending(work.known.number, &record),
            )
            .await
            .unwrap_or_else(|_| {
                Err(Problem::timed_out(
                    "keeping the own database's structure: it took too long",
                ))
            });
            match settled {
                Ok(()) => work.known.mark_compared(),
                Err(problem) => log::record(
                    "own-database-structure-not-kept",
                    serde_json::json!({ "application": work.application, "problem": problem.step() }),
                ),
            }
        }
        committed
    }
}

/// Why [`OwnDatabases::to_the_end`] answered nothing of the work's.
#[derive(Clone, Copy, Debug)]
pub enum NotToTheEnd {
    /// [`PREPARING`] are already being sent: nothing of it was begun.
    Busy,
    /// Begun, but no answer came: a fixed description.
    Failed(&'static str),
}

/// When a comparison is made (blackbox/CONTRACT.md, "What something else
/// changed").
#[derive(Clone, Copy)]
enum Occasion {
    /// The first operation on the own database in this run.
    FirstUse,
    /// A write, [`COMPARE_EVERY`] after the last comparison.
    Write,
    /// A Version sent, before its migrations.
    Version,
}

/// The structure and the capture of an own database's tables, by
/// `"<schema>.<table>"`: a digest each (`vm.structure_state()`).
type Structure = BTreeMap<String, String>;

/// What the platform database keeps of an own database's structure.
struct Kept {
    tables: Structure,
    /// What it is once the migrations being applied are committed, and the
    /// digest of the record of migrations then.
    pending: Option<(Structure, String)>,
}

fn structure_of(value: &serde_json::Value) -> Structure {
    value
        .as_object()
        .map(|tables| {
            tables
                .iter()
                .filter_map(|(table, digest)| Some((table.clone(), digest.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// The tables whose digest is not the one expected, or that are on one
/// side only.
fn differing(expected: &Structure, found: &Structure) -> Vec<String> {
    let mut tables: Vec<String> = expected
        .iter()
        .filter(|(table, digest)| found.get(*table) != Some(*digest))
        .map(|(table, _)| table.clone())
        .chain(
            found
                .keys()
                .filter(|table| !expected.contains_key(*table))
                .cloned(),
        )
        .collect();
    tables.sort();
    tables
}

/// The digest of a record of migrations: of each one's name and sha256,
/// in order.
fn record_digest<'a>(record: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut digest = Sha256::new();
    for (name, sha256) in record {
        digest.update(name.as_bytes());
        digest.update(b"\t");
        digest.update(sha256.as_bytes());
        digest.update(b"\n");
    }
    crate::registry::hex(&digest.finalize())
}

/// The own database's record of migrations (its digest) and its structure,
/// read together in one read-only snapshot.
async fn read_structure(client: &mut Client) -> Result<(String, Structure), Problem> {
    let fail =
        |e: tokio_postgres::Error| Problem::postgres("reading the own database's structure", &e);
    let transaction = client
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(fail)?;
    let record = transaction
        .query(
            "SELECT name, sha256 FROM vm.migrations ORDER BY position",
            &[],
        )
        .await
        .map_err(fail)?;
    let mut migrations = Vec::with_capacity(record.len());
    for row in &record {
        migrations.push((
            row.try_get::<_, String>(0).map_err(fail)?,
            row.try_get::<_, String>(1).map_err(fail)?,
        ));
    }
    let structure = structure_rows(&transaction).await.map_err(fail)?;
    transaction.commit().await.map_err(fail)?;
    Ok((
        record_digest(migrations.iter().map(|(n, s)| (n.as_str(), s.as_str()))),
        structure,
    ))
}

/// `vm.structure_state()`, on whichever session.
async fn structure_rows(client: &impl GenericClient) -> Result<Structure, tokio_postgres::Error> {
    let rows = client
        .query("SELECT * FROM vm.structure_state()", &[])
        .await?;
    let mut structure = Structure::new();
    for row in &rows {
        structure.insert(row.try_get(0)?, row.try_get(1)?);
    }
    Ok(structure)
}

/// What [`OwnDatabases::apply`] works on.
struct Work<'a> {
    names: &'a Names,
    known: &'a Known,
    /// The migration role's password for this time.
    password: &'a Secret,
    key: &'a IdentityKey,
    application: &'a str,
    sender: &'a str,
    list: &'a [Migration],
    /// The length of the record of migrations when the text was checked:
    /// `plans` are of `list` from there on.
    checked_from: usize,
    /// Each pending migration's statements, as byte ranges of its text.
    plans: &'a [Vec<Range<usize>>],
    deadline: Instant,
}

/// Every pending migration's text split into single statements, as byte
/// ranges of it, off the async workers; or the first rule broken, by the
/// migration that broke it. The task is awaited by the caller, so it has
/// an owner.
async fn plan(pending: &[Migration]) -> Result<Vec<Vec<Range<usize>>>, Problem> {
    // Copied: the parser runs on a blocking thread, which needs the texts
    // as its own; once per Version sent.
    let texts: Vec<(String, String)> = pending
        .iter()
        .map(|m| (m.name.clone(), m.sql.clone()))
        .collect();
    let parsed = tokio::task::spawn_blocking(move || {
        let mut plans = Vec::with_capacity(texts.len());
        for (name, sql) in &texts {
            match migration_text::statements(sql) {
                Ok(statements) => plans.push(statements),
                Err(refusal) => {
                    return Err(Problem::Refused {
                        migration: name.clone(),
                        rule: refusal.rule,
                        message: refusal.message,
                    });
                }
            }
        }
        Ok(plans)
    });
    parsed.await.map_err(|_| Problem::Unavailable {
        step: "reading the migrations' text",
        code: None,
    })?
}

/// The record of migrations as `vm.applied_migrations()` reads it: place,
/// name, sha256 recorded, sha256 of the text kept; in order of place.
async fn read_record(
    transaction: &Transaction<'_>,
) -> Result<Vec<(i32, String, String, String)>, tokio_postgres::Error> {
    let rows = transaction
        .query("SELECT * FROM vm.applied_migrations()", &[])
        .await?;
    let mut record = Vec::with_capacity(rows.len());
    for row in &rows {
        record.push((
            row.try_get(0)?,
            row.try_get(1)?,
            row.try_get(2)?,
            row.try_get(3)?,
        ));
    }
    record.sort_by_key(|(place, ..)| *place);
    Ok(record)
}

/// The capture triggers' catalog rows of every table, as
/// `vm.capture_state()` reads them on the migration role's session.
async fn capture_state(
    transaction: &Transaction<'_>,
) -> Result<CaptureState, tokio_postgres::Error> {
    let rows = transaction
        .query("SELECT * FROM vm.capture_state()", &[])
        .await?;
    let mut state = Vec::with_capacity(rows.len());
    for row in &rows {
        state.push((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?));
    }
    state.sort_by_key(|(oid, ..)| *oid);
    Ok(state)
}

/// The first table, by name, whose capture triggers are not the very rows
/// they were `before`, among those that still exist `after` (a table
/// dropped takes its triggers with it; that is a change of structure).
fn changed<'a>(before: &'a CaptureState, after: &CaptureState) -> Option<&'a str> {
    let now: HashMap<u32, &str> = after
        .iter()
        .map(|(oid, _, triggers)| (*oid, triggers.as_str()))
        .collect();
    before
        .iter()
        .find(|(oid, _, triggers)| now.get(oid).is_some_and(|t| *t != triggers.as_str()))
        .map(|(_, table, _)| table.as_str())
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

    #[test]
    fn a_capture_trigger_changed_is_found_on_a_table_that_is_still_there() {
        let state = |rows: &[(u32, &str, &str)]| -> CaptureState {
            rows.iter()
                .map(|(o, t, g)| (*o, (*t).to_owned(), (*g).to_owned()))
                .collect()
        };
        let before = state(&[
            (1, "public.a", "x"),
            (2, "public.b", "y"),
            (3, "public.c", "z"),
        ]);
        assert_eq!(changed(&before, &before), None);
        // b dropped, d new: neither is a change of capture.
        let after = state(&[
            (1, "public.a", "x"),
            (3, "public.c", "z"),
            (4, "public.d", ""),
        ]);
        assert_eq!(changed(&before, &after), None);
        let after = state(&[
            (1, "public.a", "x"),
            (2, "public.b", "y2"),
            (3, "public.c", ""),
        ]);
        assert_eq!(changed(&before, &after), Some("public.b"));
    }
}
