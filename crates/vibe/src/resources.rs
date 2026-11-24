//! The Resource registry and Grants (ADR 0003): shared databases registered
//! by Platform administrators, and Grants as plain rows of Resource, Group
//! and Access level. No policy engine.
//!
//! Interface: [`Resources::load`] once at start; [`Resources::register`]
//! and [`Resources::grant`] for the API; [`Resources::get`] and
//! [`Resources::list`], which answer from memory, for the Operation gateway
//! and the API.
//!
//! As with Applications and Versions (crate::registry), memory holds every
//! Resource and Grant, and is changed only after the platform database has
//! committed, so operations are decided from memory, also while the
//! platform database cannot be reached. A write of a Grant is one
//! transaction whose first statement takes the Resource's row lock in
//! `FOR NO KEY UPDATE` (the weakest mode that conflicts with the `FOR SHARE`
//! of a read-back; the `FOR KEY SHARE` a foreign key takes does not), held
//! until its commit is done: so the writes of one Resource's Grants take
//! turns, and each takes its revision (from one sequence) under that lock,
//! so a Resource's revisions grow in the order its writes commit. Memory
//! keeps the higher revision of each Group's Grant, so two writes leave
//! memory as the database whatever order they report back in.
//!
//! A write of a Grant whose answer is lost may still be made after that
//! answer (its commit on its way): its COMMIT is sent only once the row's
//! lock is held, so if it lands at all it holds the lock from before the
//! answer was lost until it has landed. Memory forgets the Resource's
//! Grants at once ([`Grants::Unknown`]), reading nothing first; an
//! operation that would come to the check of a Grant is then refused
//! `grants-unknown` (crate::gateway), and the refused operation's own task,
//! once it has answered, reads the Grants back ([`Resources::read_back`]):
//! in one transaction, the row's share lock taken without waiting (so
//! while the write is on its way this fails at once), then the Grants read
//! in a second statement, whose snapshot is after the lock. The API's paths
//! that show Grants try the same, and are `503` while it fails. A read-back
//! begun before a forgetting (`epoch`) or that misses a revision reported
//! since (`seen`) is not taken. At a start, Resources whose row a write
//! holds are skipped and their Grants held as not known.
//!
//! A Resource registered whose answer is lost may be registered after
//! that answer: nothing is kept of it. An API request that names a Resource
//! memory does not hold reads it ([`Resources::find`]), so it is known from
//! the next such request (registering it again, which is `name-taken`,
//! included).
//!
//! A Resource's connection and logins are held as a [`Connection`], which
//! never prints, and stored as the [`sealed`] module writes them, the one
//! place that knows the stored form: sealed under the installation's key
//! (crate::credentials) and bound to the Resource's id and name, so that a
//! value moved to another row does not open. A database Resource whose
//! stored value opens holds a pool per login (crate::executor): its read
//! pool, its write pool when it was registered with a write login, and its
//! structure pool when it was registered with a structure login (the
//! Structure puller's, crate::structure). A pool connects on first use.
//!
//! A database Resource whose value does not open, a Resource still in the
//! clear form of before (which the sealing at start did not reach), or a
//! platform database from which the key is missing, is **not served**
//! (blackbox/CONTRACT.md, "Resources and Grants in full: At start"): it is
//! in memory with its name, description, owner and contact, and every
//! operation on it is answered `unavailable` without anything sent to any
//! database. The program's log says why for each (`does-not-open`,
//! `not-sealed`, `no-secret-key`), once at start and then at most once a
//! minute for each while it lasts ([`Resource::mention_unserved`]).
//!
//! A Resource is of a kind (blackbox/CONTRACT.md, "Storage"): `postgres`, a
//! shared database as above, or `storage`, a named place of files, whose
//! location (crate::storage::Location) is stored where a database's
//! connection is and which holds its backend (crate::storage::Directory),
//! opened as it is loaded or registered, in place of pools.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use authz::AccessLevel;
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::FromSql;
use tokio_postgres::{Client, Row};

use crate::credentials::Vault;
use crate::executor::{self, Level, Levels, Password, Source, Target};
use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::registry::ResourceKind;
use crate::storage::directory::HeldPath;
use crate::storage::{Directory, Files, Location};

/// The name of an Access level, as the API and the Operation log write it.
pub fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Structure => "structure",
        AccessLevel::Read => "read",
        AccessLevel::Write => "write",
    }
}

/// The Access level of a name, exactly as written.
pub fn level_of(name: &str) -> Option<AccessLevel> {
    match name {
        "structure" => Some(AccessLevel::Structure),
        "read" => Some(AccessLevel::Read),
        "write" => Some(AccessLevel::Write),
        _ => None,
    }
}

/// Whether `name` can be a Resource's: 1 to 63 lower-case letters, digits
/// and underscores, starting with a letter.
pub fn is_resource_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && name.len() <= 63
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Where a Resource's database is and its logins, as the Platform
/// administrator gave them: the read login, and optionally the write login
/// and the structure login. Never printed: its `Debug` shows nothing.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    host: String,
    port: u16,
    database: String,
    read: Login,
    /// Absent in what was stored before write logins existed.
    #[serde(default)]
    write: Option<Login>,
    /// The login the Structure puller prefers (blackbox/CONTRACT.md, "The
    /// structure login"). Absent in what was stored before it existed.
    #[serde(default)]
    structure: Option<Login>,
}

/// A login to a shared database. Never printed: it has no `Debug` of its
/// own, and the [`Connection`] that holds it shows nothing.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Login {
    role: String,
    password: String,
}

impl Login {
    pub fn new(role: String, password: String) -> Self {
        Self { role, password }
    }

    /// The role's name, where a check needs it.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The password, where a check connects with it. Never printed.
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Connection(<hidden>)")
    }
}

mod check;
pub use check::{Checked, Failure, Warning, Which};

impl Connection {
    /// Checks the logins this connection gives, or only `only` of them.
    pub async fn check_logins(&self, only: &[check::Which]) -> Checked {
        check::check(self, only).await
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn read_login(&self) -> &Login {
        &self.read
    }

    pub fn write_login(&self) -> Option<&Login> {
        self.write.as_ref()
    }

    pub fn structure_login(&self) -> Option<&Login> {
        self.structure.as_ref()
    }

    /// Which logins it gives, in the order read, write, structure.
    pub fn logins(&self) -> Vec<check::Which> {
        let mut logins = vec![check::Which::Read];
        if self.write.is_some() {
            logins.push(check::Which::Write);
        }
        if self.structure.is_some() {
            logins.push(check::Which::Structure);
        }
        logins
    }

    pub fn new(
        host: String,
        port: u16,
        database: String,
        read: Login,
        write: Option<Login>,
        structure: Option<Login>,
    ) -> Self {
        Self {
            host,
            port,
            database,
            read,
            write,
            structure,
        }
    }

    /// The pools of `resource`: the read login's, and the write login's and
    /// the structure login's if there are, each with `limits`.
    fn into_pools(self, resource: &str, limits: &executor::Limits) -> Pools {
        let target = |login: Login, host: String, database: String| {
            Target::new(
                host,
                self.port,
                database,
                login.role,
                Password::new(login.password),
            )
        };
        let write = self.write.map(|login| {
            // Copied: each pool owns its target, and this is done once, as
            // the Resource is registered or loaded.
            let target = target(login, self.host.clone(), self.database.clone());
            let source = Source::Resource(resource.to_owned());
            Arc::new(executor::Pool::new(
                source,
                Levels::Only(Level::Write),
                target,
                limits.clone(),
            ))
        });
        let structure = self.structure.map(|login| {
            // Copied, as the write login's target is.
            let target = target(login, self.host.clone(), self.database.clone());
            let source = Source::Resource(resource.to_owned());
            Arc::new(executor::Pool::new(
                source,
                Levels::Only(Level::Read),
                target,
                limits.clone(),
            ))
        });
        let read = target(self.read, self.host, self.database);
        Pools {
            read: Arc::new(executor::Pool::new(
                Source::Resource(resource.to_owned()),
                Levels::Only(Level::Read),
                read,
                limits.clone(),
            )),
            write,
            structure,
        }
    }
}

/// A Resource's pools, one per login. Their `Debug` names the Resource
/// and the level only. Each is shared: an operation holds the pool it was
/// sent with to its end, so that the logins can be replaced under it and
/// the old pool closes when its last operation lets it go.
#[derive(Debug)]
struct Pools {
    read: Arc<executor::Pool>,
    write: Option<Arc<executor::Pool>>,
    /// The structure login's, which reads in read-only transactions only.
    structure: Option<Arc<executor::Pool>>,
}

/// The pools of a served Resource, shared with the operations that take
/// them.
struct HeldPools {
    read: Arc<executor::Pool>,
    write: Option<Arc<executor::Pool>>,
    structure: Option<Arc<executor::Pool>>,
}

/// The stored form of a [`Connection`] (and of a Storage Resource's
/// location): the one place that writes and reads it.
///
/// A connection is the sealed form of crate::oidc::seal (`1 || nonce ||
/// ciphertext || tag`), sealed under the key of its use with the Resource's
/// id and name as the associated data (crate::credentials). The form of
/// before this ticket is the connection as JSON, which begins with `{`; it
/// is told from the sealed form without a key and is never used, only
/// sealed at the first start with the key.
///
/// A Storage Resource's location is not a credential: it is stored as
/// before, as JSON.
mod sealed {
    use super::Connection;
    use crate::credentials::{Vault, resource_connection_bound};

    /// `connection` sealed for the Resource `id`, whose name is `name`.
    pub(super) fn seal(
        connection: &Connection,
        vault: &Vault,
        id: i64,
        name: &str,
    ) -> Option<Vec<u8>> {
        let plain = serde_json::to_vec(connection).ok()?;
        let bound = resource_connection_bound(id, name);
        vault.resource_sealer().seal_bytes(plain, &bound)
    }

    /// What `stored` holds, sealed for the Resource `id`, whose name is
    /// `name`; `None` if it does not open. The reason is not given, as it
    /// could quote what is stored.
    pub(super) fn open(stored: &[u8], vault: &Vault, id: i64, name: &str) -> Option<Connection> {
        let bound = resource_connection_bound(id, name);
        let opened = vault.resource_sealer().open_bytes(stored, &bound)?;
        serde_json::from_slice(&opened).ok()
    }

    /// A Storage Resource's location, kept as before.
    pub(super) fn seal_location(location: &crate::storage::Location) -> Option<Vec<u8>> {
        serde_json::to_vec(location).ok()
    }

    pub(super) fn open_location(stored: &[u8]) -> Option<crate::storage::Location> {
        serde_json::from_slice(stored).ok()
    }
}

/// A Resource being registered.
#[derive(Debug)]
pub struct NewResource {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub contact: String,
    pub reach: NewReach,
}

/// How a Resource being registered is reached.
#[derive(Debug)]
pub enum NewReach {
    /// A shared database.
    Database(Connection),
    /// A place of files: its location as stored, and its backend, opened.
    Storage {
        location: Location,
        backend: Directory,
    },
}

/// How a Resource is reached: by its pools, by its backend, or not at all.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per Resource, inside its Arc, never moved after it is made"
)]
enum Reach {
    /// Its pools, or why it is not served. Held only for a lookup or a
    /// change of its logins, never across an await.
    Database(RwLock<Database>),
    /// A place of files. Swapped whole by a change of location: an
    /// operation already under way keeps the one it took and finishes
    /// there.
    Storage(RwLock<Directory>),
}

/// A database Resource as memory holds it now.
#[derive(Debug)]
enum Database {
    Served {
        pools: Pools,
        /// The logins as memory holds them, for a replacement that leaves
        /// some out: copied once per registration, load or replacement.
        connection: Connection,
    },
    /// Its connection is not served now: no key, a value that does not
    /// open, or a clear value that could not be sealed. Its name and what
    /// everyone may see are as any Resource's.
    Unserved(Unserved),
}

/// A database Resource that is not served, for this problem.
fn unserved(problem: &'static str) -> Reach {
    Reach::Database(RwLock::new(Database::Unserved(Unserved {
        problem,
        said: std::sync::Mutex::new(None),
    })))
}

/// Why a database Resource is not served, and when its problem was last
/// written to the program's log.
#[derive(Debug)]
struct Unserved {
    /// `no-secret-key`, `does-not-open` or `not-sealed`
    /// (blackbox/CONTRACT.md, "Resources and Grants in full: At start").
    problem: &'static str,
    /// Held only for a check and a write, never across an await.
    said: std::sync::Mutex<Option<Instant>>,
}

/// How often, at most, a Resource that is not served is written to the log
/// again while it lasts.
const MENTION_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
struct Granted {
    level: AccessLevel,
    revision: i64,
}

/// What memory holds of a Resource's Grants.
#[derive(Debug)]
enum Grants {
    /// By Group.
    Known(BTreeMap<String, Granted>),
    /// Forgotten: a write of a Grant whose answer was lost may still land.
    /// `seen` is the highest revision memory held or a write reported
    /// since; `epoch` tells a read-back begun before this forgetting from
    /// one begun after.
    Unknown { seen: i64, epoch: u64 },
    /// Taken out of memory ([`Resources::unregister`]): no Grant, and
    /// final. Nothing forgets it, makes it known or installs into it, so a
    /// write or a read-back that still holds it cannot bring it back into
    /// [`Resources::unknown`].
    LetGo,
}

impl Grants {
    /// The highest revision of `grants`, 0 for none.
    fn newest(grants: &BTreeMap<String, Granted>) -> i64 {
        grants.values().map(|g| g.revision).max().unwrap_or(0)
    }
}

/// A registered Resource. Its public face is what every signed-in User may
/// see; its connection is only in its pool.
#[derive(Debug)]
pub struct Resource {
    id: i64,
    pub name: String,
    /// Its description, owner and contact: changed in place, so that the
    /// very next refusal names the new contact.
    texts: RwLock<Texts>,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub kind: ResourceKind,
    /// A database's pools, one per login (a connection of one never serves
    /// the other); or a place of files' backend.
    reach: Reach,
    /// Held only for a lookup or a change, never across an await.
    grants: RwLock<Grants>,
    /// One turn for the writes of what the Resource is reached by and its
    /// texts, from before the row's transaction until memory is what was
    /// written, so that memory never ends older than the row.
    writes: tokio::sync::Mutex<()>,
    /// The pulls of its structure running now: at most [`PULLS_AT_ONCE`].
    pulls: Semaphore,
    /// Who asked for its removal whose answer was lost, until a reading
    /// finds the row gone and the record `resource-removed` is written.
    removal: std::sync::Mutex<Option<String>>,
    /// Who asked for a change of its location whose answer was lost, and
    /// where it was to, until a reading finds what became of it.
    location_change: std::sync::Mutex<Option<PendingLocation>>,
}

/// What of a Resource a Platform administrator may change without touching
/// its logins.
#[derive(Debug)]
struct Texts {
    description: Arc<str>,
    owner: Arc<str>,
    contact: Arc<str>,
}

/// Which logins a Resource that is served has, and nothing else of them.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Logins {
    pub read: bool,
    pub write: bool,
    pub structure: bool,
}

/// At most this many pulls of one Resource's structure run at once
/// (blackbox/CONTRACT.md, "What a pull reads"), so that pulls, which may
/// share the read login's pool with Applications, never hold more of its
/// connections than this.
pub const PULLS_AT_ONCE: usize = 2;

impl Resource {
    /// Its row's id, which every write of it locks and changes by.
    pub fn id(&self) -> i64 {
        self.id
    }

    /// Its description, as memory holds it now.
    pub fn description(&self) -> Arc<str> {
        Arc::clone(
            &self
                .texts
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .description,
        )
    }

    /// Its owner, as memory holds it now.
    pub fn owner(&self) -> Arc<str> {
        Arc::clone(
            &self
                .texts
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .owner,
        )
    }

    /// Whom to ask for access, as memory holds it now.
    pub fn contact(&self) -> Arc<str> {
        Arc::clone(
            &self
                .texts
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .contact,
        )
    }

    /// Takes new texts, as a change of them committed.
    pub fn set_texts(&self, description: Arc<str>, owner: Arc<str>, contact: Arc<str>) {
        *self.texts.write().unwrap_or_else(PoisonError::into_inner) = Texts {
            description,
            owner,
            contact,
        };
    }

    /// Why this database Resource is not served now; `None` for one that
    /// is, and for a Storage Resource.
    pub fn unserved(&self) -> Option<&'static str> {
        match &self.reach {
            Reach::Database(state) => {
                match &*state.read().unwrap_or_else(PoisonError::into_inner) {
                    Database::Unserved(unserved) => Some(unserved.problem),
                    Database::Served { .. } => None,
                }
            }
            Reach::Storage(_) => None,
        }
    }

    /// Writes the `resource-unusable` record of this Resource now, unless
    /// it was written less than a minute ago; and `force` writes it
    /// whatever was written before (the start's record, for each).
    pub fn mention_unserved(&self, force: bool) {
        let Reach::Database(state) = &self.reach else {
            return;
        };
        let state = state.read().unwrap_or_else(PoisonError::into_inner);
        let Database::Unserved(unserved) = &*state else {
            return;
        };
        let problem = unserved.problem;
        {
            let mut said = unserved.said.lock().unwrap_or_else(PoisonError::into_inner);
            if !force && said.is_some_and(|at| at.elapsed() < MENTION_EVERY) {
                return;
            }
            *said = Some(Instant::now());
        }
        drop(state);
        log::record(
            "resource-unusable",
            serde_json::json!({ "resource": &self.name, "problem": problem }),
        );
    }

    /// The pool of the read login; `None` for a Resource that is not
    /// served, and for one that is no database. Shared with the operation
    /// that takes it, so that replacing the logins does not take it away.
    pub fn read_pool(&self) -> Option<Arc<executor::Pool>> {
        self.served().map(|pools| pools.read)
    }

    /// The pool of the write login, if the Resource is a database and has
    /// one.
    pub fn write_pool(&self) -> Option<Arc<executor::Pool>> {
        self.served().and_then(|pools| pools.write)
    }

    /// The pool the Structure puller reads the catalogs with: the structure
    /// login's when the Resource has one, else the read login's. Never the
    /// write login's. `None` for a Resource that is no database.
    pub fn structure_pool(&self) -> Option<Arc<executor::Pool>> {
        self.served()
            .map(|pools| pools.structure.unwrap_or(pools.read))
    }

    /// The pools of a Resource that is served now (its read, write and
    /// structure login's); the guard is let go before this returns, and
    /// each pool is shared.
    fn served(&self) -> Option<HeldPools> {
        let Reach::Database(state) = &self.reach else {
            return None;
        };
        let state = state.read().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            Database::Served { pools, .. } => Some(HeldPools {
                read: Arc::clone(&pools.read),
                write: pools.write.as_ref().map(Arc::clone),
                structure: pools.structure.as_ref().map(Arc::clone),
            }),
            Database::Unserved(_) => None,
        }
    }

    /// The pool a pull uses (the structure login's, else the read login's),
    /// and the read and write role names the pull asks Postgres about, in
    /// one look under the Resource's lock; nothing of the connection but
    /// the two names is copied.
    pub fn structure_and_roles(&self) -> Option<(Arc<executor::Pool>, String, Option<String>)> {
        let Reach::Database(state) = &self.reach else {
            return None;
        };
        let state = state.read().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            Database::Served { pools, connection } => {
                let pool = pools.structure.as_ref().unwrap_or(&pools.read);
                Some((
                    Arc::clone(pool),
                    connection.read_login().role().to_owned(),
                    connection
                        .write_login()
                        .map(|login| login.role().to_owned()),
                ))
            }
            Database::Unserved(_) => None,
        }
    }

    /// The logins memory holds now, if it serves it.
    pub(crate) fn connection(&self) -> Option<Connection> {
        let Reach::Database(state) = &self.reach else {
            return None;
        };
        let state = state.read().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            Database::Served { connection, .. } => Some(connection.clone()),
            Database::Unserved(_) => None,
        }
    }

    /// Whether memory holds what a reading of the row found: the same
    /// connection served (`Ok`), or the same reason for not serving it
    /// (`Err`). Compared where it is held: nothing is copied.
    fn reached_as(&self, read: Result<&Connection, &'static str>) -> bool {
        let Reach::Database(state) = &self.reach else {
            return false;
        };
        let state = state.read().unwrap_or_else(PoisonError::into_inner);
        match (&*state, read) {
            (Database::Served { connection, .. }, Ok(read)) => connection == read,
            (Database::Unserved(unserved), Err(problem)) => unserved.problem == problem,
            _ => false,
        }
    }

    /// Which logins it has, for a Platform administrator: `None` for a
    /// Resource that is not served, and for a Storage Resource.
    pub fn logins(&self) -> Option<Logins> {
        self.served().map(|pools| Logins {
            read: true,
            write: pools.write.is_some(),
            structure: pools.structure.is_some(),
        })
    }

    /// Takes these pools as the Resource's from now on: every operation
    /// sent after this takes them, and the old ones close as their last
    /// operation lets go.
    fn replace_pools(&self, pools: Pools, connection: Connection) {
        let Reach::Database(state) = &self.reach else {
            return;
        };
        *state.write().unwrap_or_else(PoisonError::into_inner) =
            Database::Served { pools, connection };
    }

    /// The backend of a Storage Resource; `None` for a database.
    pub fn storage(&self) -> Option<Directory> {
        match &self.reach {
            Reach::Storage(backend) => Some(
                backend
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            Reach::Database(_) => None,
        }
    }

    /// Takes a new place of files, as a change of location committed.
    pub(crate) fn install_location(&self, backend: Directory) {
        if let Reach::Storage(state) = &self.reach {
            *state.write().unwrap_or_else(PoisonError::into_inner) = backend;
        }
    }

    /// A removal of this Resource was asked for and its answer lost: when
    /// a reading finds the row gone, `resource-removed` names `by`.
    pub(crate) fn expect_removal(&self, by: &str) {
        *self.removal.lock().unwrap_or_else(PoisonError::into_inner) = Some(by.to_owned());
    }

    /// A removal whose answer is certain: no record is owed by a reading.
    pub(crate) fn clear_removal(&self) {
        *self.removal.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn take_removal(&self) -> Option<String> {
        self.removal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Sets this change's pending only when none is pending for the same
    /// place: a request asking again what an earlier lost request asked
    /// keeps that request's `by` and what it made. Answers whether this
    /// request's own was set.
    pub(crate) fn expect_location_change_if_none_for(
        &self,
        by: &str,
        path: std::path::PathBuf,
        made_marker: bool,
        made_staging: bool,
    ) -> bool {
        let mut pending = self
            .location_change
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending
            .as_ref()
            .is_some_and(|existing| existing.path == path)
        {
            return false;
        }
        *pending = Some(PendingLocation {
            by: by.to_owned(),
            path,
            made_marker,
            made_staging,
        });
        true
    }

    /// A change whose answer is certain: no record is owed by a reading.
    pub(crate) fn clear_location_change(&self) {
        *self
            .location_change
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn take_location_change(&self) -> Option<PendingLocation> {
        self.location_change
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn pending_location(&self) -> Option<PendingLocation> {
        self.location_change
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// This Resource's turn for a write: held from before its row's
    /// transaction until memory is what was written, within `within`;
    /// `None` when another write holds it that long.
    pub(crate) async fn writing(
        &self,
        within: Duration,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        tokio::time::timeout(within, self.writes.lock()).await.ok()
    }

    /// A place among the pulls of this Resource running now, held until it
    /// is dropped (however the pull ends, its request's future dropped
    /// included); `None` when [`PULLS_AT_ONCE`] are running. Never waits.
    pub fn pulling(&self) -> Option<SemaphorePermit<'_>> {
        self.pulls.try_acquire().ok()
    }

    /// Runs `decide` with the Grants on this Resource, as the Authorization
    /// decision takes them; `None`, without running it, while memory does
    /// not know them.
    pub fn with_grants<T>(&self, decide: impl FnOnce(&[authz::Grant<'_>]) -> T) -> Option<T> {
        let grants = self.grants.read().unwrap_or_else(PoisonError::into_inner);
        let grants = match &*grants {
            Grants::Known(grants) => grants,
            Grants::LetGo => return Some(decide(&[])),
            Grants::Unknown { .. } => return None,
        };
        let list: Vec<authz::Grant<'_>> = grants
            .iter()
            .map(|(group, granted)| authz::Grant {
                group,
                level: granted.level,
            })
            .collect();
        Some(decide(&list))
    }

    /// Whether memory knows the Grants on this Resource now.
    pub fn grants_known(&self) -> bool {
        matches!(
            *self.grants.read().unwrap_or_else(PoisonError::into_inner),
            Grants::Known(_) | Grants::LetGo
        )
    }

    /// At most `limit` Grants, by Group in byte order, and how many more;
    /// only that many are copied. `None` while memory does not know them.
    pub fn grants_page(&self, limit: usize) -> Option<(Vec<(String, AccessLevel)>, usize)> {
        let grants = self.grants.read().unwrap_or_else(PoisonError::into_inner);
        match &*grants {
            Grants::Known(grants) => {
                let mut page: Vec<(String, AccessLevel)> =
                    Vec::with_capacity(limit.min(grants.len()));
                for (group, granted) in grants.iter().take(limit) {
                    page.push((group.clone(), granted.level));
                }
                let more = grants.len().saturating_sub(page.len());
                Some((page, more))
            }
            Grants::LetGo => Some((Vec::new(), 0)),
            Grants::Unknown { .. } => None,
        }
    }

    /// Each Group's Access level, by Group; `None` while memory does not
    /// know them.
    pub fn grants(&self) -> Option<Vec<(String, AccessLevel)>> {
        // Copied out so that the lock is not held while the answer is
        // written.
        match &*self.grants.read().unwrap_or_else(PoisonError::into_inner) {
            Grants::Known(grants) => Some(
                grants
                    .iter()
                    .map(|(group, granted)| (group.clone(), granted.level))
                    .collect(),
            ),
            Grants::LetGo => Some(Vec::new()),
            Grants::Unknown { .. } => None,
        }
    }

    /// Takes what a write committed: `level` for `group` unless memory
    /// already has a later write; over forgotten Grants it only raises
    /// `seen`, since a lost write may have gone further.
    fn install(&self, group: &str, granted: Granted) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        match &mut *grants {
            Grants::Known(grants) => match grants.get_mut(group) {
                Some(current) if current.revision >= granted.revision => {}
                Some(current) => *current = granted,
                None => {
                    grants.insert(group.to_owned(), granted);
                }
            },
            Grants::Unknown { seen, .. } => *seen = (*seen).max(granted.revision),
            Grants::LetGo => {}
        }
    }

    /// Forgets the Grants, as of `epoch`, keeping the highest revision
    /// memory held as `seen`. `unknown` counts the Resources in memory
    /// whose Grants are forgotten: one more when these were known, under
    /// the lock that changes them. It is exact because every change in or
    /// out of `Unknown` is made under that lock together with the count's
    /// change, and a Resource taken out of memory is `LetGo` first (also
    /// under it), which nothing forgets again.
    fn forget(&self, epoch: u64, unknown: &AtomicUsize) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        let seen = match &*grants {
            Grants::Known(known) => {
                unknown.fetch_add(1, Ordering::SeqCst);
                Grants::newest(known)
            }
            Grants::Unknown { seen, .. } => *seen,
            Grants::LetGo => return,
        };
        *grants = Grants::Unknown { seen, epoch };
    }

    /// For a Resource taken out of memory: `LetGo`, so that `unknown` no
    /// longer counts it and nothing counts it again.
    fn let_go(&self, unknown: &AtomicUsize) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        if let Grants::Unknown { .. } = &*grants {
            unknown.fetch_sub(1, Ordering::SeqCst);
        }
        *grants = Grants::LetGo;
    }

    /// The epoch of the forgetting memory is in; `None` when it knows the
    /// Grants.
    fn forgotten_at(&self) -> Option<u64> {
        match &*self.grants.read().unwrap_or_else(PoisonError::into_inner) {
            Grants::Known(_) | Grants::LetGo => None,
            Grants::Unknown { epoch, .. } => Some(*epoch),
        }
    }

    /// Takes what a read-back begun in the forgetting `epoch` read: only
    /// if memory is still in that forgetting, and it holds every revision
    /// reported since.
    /// `unknown` is one less when they become known, under the same lock.
    fn install_read(
        &self,
        read: BTreeMap<String, Granted>,
        texts: (String, String, String),
        reach: Option<ReachRead>,
        began: u64,
        unknown: &AtomicUsize,
    ) -> bool {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        if !reading_wins(&grants, began, &read) {
            return false;
        }
        *grants = Grants::Known(read);
        unknown.fetch_sub(1, Ordering::SeqCst);
        // The row whole, taken together with its Grants: its texts, and its
        // logins (the pools of its opened connection, or why it is not
        // served).
        let (description, owner, contact) = texts;
        *self.texts.write().unwrap_or_else(PoisonError::into_inner) = Texts {
            description: Arc::from(description),
            owner: Arc::from(owner),
            contact: Arc::from(contact),
        };
        let storage_changed = matches!(reach, Some(ReachRead::Storage(_)));
        match (&self.reach, reach) {
            (Reach::Database(state), Some(ReachRead::Database(database))) => {
                *state.write().unwrap_or_else(PoisonError::into_inner) = database;
            }
            (Reach::Storage(state), Some(ReachRead::Storage(directory))) => {
                *state.write().unwrap_or_else(PoisonError::into_inner) = directory;
            }
            _ => {}
        }
        if storage_changed && let Some(pending) = self.take_location_change() {
            log::record(
                "resource-location-changed",
                serde_json::json!({ "resource": &self.name, "by": pending.by }),
            );
        }
        true
    }
}

/// Whether a reading begun in the forgetting `began` is taken: only while
/// memory is still in that forgetting, and it holds every revision reported
/// since. The one place that decides which of two readings wins.
fn reading_wins(grants: &Grants, began: u64, read: &BTreeMap<String, Granted>) -> bool {
    match grants {
        Grants::Unknown { seen, epoch } => *epoch == began && *seen <= Grants::newest(read),
        Grants::Known(_) | Grants::LetGo => false,
    }
}

/// What a reading of a Resource's row found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadBack {
    /// Its Grants (and the rest of the row) are known now.
    Known,
    /// No row: the Resource was removed. It is let go from memory.
    Removed,
    /// It still cannot be told (a write on its way, or no time).
    Unknown,
}

/// A write of a Resource's row whose answer may be lost: armed before the
/// statement is sent, disarmed once memory is what the database has. A
/// future dropped while armed (the administrator's connection closed, the
/// request given up, a timeout) forgets what memory knows of the Resource,
/// so that nothing is sent through a login or a text it may no longer
/// have; the existing read-back then finds the row whole. It only
/// signals: it spawns nothing.
pub struct Unsettled<'a> {
    resources: &'a Resources,
    resource: &'a Resource,
    armed: bool,
}

impl<'a> Unsettled<'a> {
    pub fn new(resources: &'a Resources, resource: &'a Resource) -> Self {
        Self {
            resources,
            resource,
            armed: true,
        }
    }

    pub fn settled(&mut self) {
        self.armed = false;
    }
}

impl Drop for Unsettled<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.resources.write_failed(self.resource);
        }
    }
}

/// A change of a Storage Resource's location whose answer was lost: who
/// asked, and the place it was to.
#[derive(Clone, Debug)]
pub(crate) struct PendingLocation {
    pub by: String,
    pub path: std::path::PathBuf,
    /// What the change made in the new directory: its marker and its
    /// staging folder. A change found not to have landed removes exactly
    /// those, and nothing that was there before it.
    pub made_marker: bool,
    pub made_staging: bool,
}

/// What changing a Storage Resource's location came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationChanged {
    Changed,
    /// The row already holds this place: nothing was written.
    Same,
    /// The row holds another place than the one memory found: nothing was
    /// written.
    ChangedMeanwhile,
    NoSuchResource,
}

/// What removing a Resource came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    /// The row and its Grants are gone; `grants` went with it.
    Removed {
        grants: u64,
    },
    NoSuchResource,
}

/// What replacing a Resource's logins came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replaced {
    /// Stored: the Resource serves by the new logins from now on.
    Replaced,
    NoSuchResource,
    /// The logins found when the request began are not the ones the row
    /// holds: nothing was changed.
    ChangedMeanwhile,
}

/// What registering came to.
#[derive(Debug)]
pub enum Registered {
    Registered(Arc<Resource>),
    NameTaken,
    /// The installation holds the 1,000 Resources it may hold.
    TooMany,
}

/// At most this many Resources are registered.
pub const MAX_RESOURCES: usize = 1_000;

/// What one registration's transaction came to.
enum Inserted {
    /// The row, or none when the name was taken.
    One(Option<Row>),
    TooMany,
}

/// What writing a Grant came to.
#[derive(Debug, PartialEq, Eq)]
pub enum GrantWritten {
    Written,
    NoSuchResource,
}

pub struct Resources {
    database: Arc<PlatformDatabase>,
    /// The installation's key, as the keys a Resource's connection is
    /// sealed and opened with; `None` when the program was started without
    /// it, and then no database Resource is served.
    vault: Option<Arc<Vault>>,
    limits: executor::Limits,
    /// The file threads Storage Resources' directories are opened on.
    files: Arc<Files>,
    /// The storage root the program was started with, from which each
    /// Storage Resource's directory is reached.
    root: Option<Arc<crate::storage::Root>>,
    /// By name. Held only for a lookup or an insert, never across an await.
    all: RwLock<BTreeMap<String, Arc<Resource>>>,
    /// The number of forgettings of Grants so far, each one's `epoch`.
    forgotten: AtomicU64,
    /// How many Resources in memory have their Grants forgotten now: one
    /// atomic load tells a request whether it has any to read back. Kept
    /// exact: it changes only on a change between known and forgotten,
    /// made under that Resource's `grants` lock in the same critical
    /// section ([`Resource::forget`], [`Resource::install_read`],
    /// [`Resource::let_go`]); a Resource taken out of memory is made
    /// `LetGo` under that lock, and `LetGo` is never forgotten again, so a
    /// write that still holds it cannot count it.
    unknown: AtomicUsize,
    /// The readings of what memory does not know (Applications and
    /// Grants), shared with crate::registry: at most [`READ_BACKS`].
    reading_back: Arc<Semaphore>,
}

/// The most readings of what memory does not know (an Application, a
/// Resource's Grants) at once in the program; one more is not made (what
/// it was for stays not known: a request gets the "meanwhile" answers, and
/// the next one tries again).
pub const READ_BACKS: usize = 4;

/// The longest the readings of one request (or one operation's, or one
/// API request's) may take together.
pub const READ_BACK_WAIT: Duration = Duration::from_secs(1);

/// The program's one bound on readings at once ([`READ_BACKS`]), given to
/// the registry and the Resources.
pub fn read_backs() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(READ_BACKS))
}

const RESOURCE_COLUMNS: &str = concat!(
    "r.id, r.name, r.description, r.owner, r.contact, r.connection, r.created_by, ",
    rfc3339!("r.created_at"),
    ", r.kind"
);

impl Resources {
    /// Reads every Resource and Grant from the platform database. Each
    /// Resource's pool uses `limits`; each Storage Resource's directory is
    /// opened on `files`.
    ///
    /// In one transaction, every Resource's row share lock is taken
    /// without waiting for the rows a write of a Grant holds (`SKIP
    /// LOCKED`); then, in statements whose snapshot is after the locks, the
    /// Resources and the Grants. The Grants of a Resource whose row was
    /// skipped are held as not known: a write of them was on its way. So a
    /// waiting write keeps neither the start nor the other Resources back.
    pub async fn load(
        database: Arc<PlatformDatabase>,
        vault: Option<Arc<Vault>>,
        limits: executor::Limits,
        reading_back: Arc<Semaphore>,
        files: Arc<Files>,
        root: Option<Arc<crate::storage::Root>>,
    ) -> Result<Self, DbError> {
        let (locked, resources, grants) = database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // Row locks only: nothing of this transaction must last, so
                // its commit need not wait to be flushed or replicated.
                transaction
                    .batch_execute("SET LOCAL synchronous_commit = off")
                    .await
                    .map_err(DbError::from)?;
                let locked = transaction
                    .query("SELECT id FROM resources FOR SHARE SKIP LOCKED", &[])
                    .await
                    .map_err(DbError::from)?;
                let resources = transaction
                    .query(
                        &format!("SELECT {RESOURCE_COLUMNS} FROM resources r ORDER BY r.name"),
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
                let grants = transaction
                    .query(
                        "SELECT g.resource_id, g.group_name, g.access, g.revision FROM grants g",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok((locked, resources, grants))
            })
            .await?;
        let locked: HashSet<i64> = locked
            .iter()
            .map(|row| get::<i64>(row, 0))
            .collect::<Result<_, _>>()?;
        let this = Self {
            database,
            vault,
            limits,
            files,
            root,
            all: RwLock::new(BTreeMap::new()),
            forgotten: AtomicU64::new(0),
            unknown: AtomicUsize::new(0),
            reading_back,
        };
        let mut read: BTreeMap<i64, BTreeMap<String, Granted>> = BTreeMap::new();
        for row in &grants {
            let id: i64 = get(row, 0)?;
            let (group, granted) = granted_of(row, 1)?;
            read.entry(id)
                .or_default()
                .insert(group.to_owned(), granted);
        }
        let mut unknown = Vec::new();
        let mut all = BTreeMap::new();
        for row in &resources {
            let resource = this.resource_of(row)?;
            // One record for each database Resource not served, at start.
            resource.mention_unserved(true);
            let grants = read.remove(&resource.id).unwrap_or_default();
            // A Grant is not a credential and needs no key: a Resource that
            // is not served remembers its Grants as any other does, and a
            // row another session holds leaves them unknown as for any other
            // (blackbox/CONTRACT.md, "A write whose answer is lost"): a User
            // with a Grant is answered `unavailable`, one without `no-grant`,
            // and `grants-unknown` only while the Grants are not known.
            if locked.contains(&resource.id) {
                *resource
                    .grants
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = Grants::Known(grants);
            } else {
                let epoch = this.forgotten.fetch_add(1, Ordering::Relaxed);
                resource.forget(epoch, &this.unknown);
                unknown.push(resource.name.clone());
            }
            all.insert(resource.name.clone(), Arc::new(resource));
        }
        if !unknown.is_empty() {
            log::record(
                "grants-unknown",
                serde_json::json!({ "resources": unknown }),
            );
        }
        // The directories, opened together, waited for START_WAIT in all.
        let deadline = tokio::time::Instant::now() + crate::storage::directory::START_WAIT;
        let mut openings = tokio::task::JoinSet::new();
        for resource in all.values() {
            if let Some(directory) = resource.storage() {
                openings.spawn(directory.opening(deadline));
            }
        }
        // Each opening ends by the deadline; the set is drained here, and
        // what is left (none) ended with it.
        while openings.join_next().await.is_some() {}
        *this.all.write().unwrap_or_else(PoisonError::into_inner) = all;
        Ok(this)
    }

    /// The Resource of `row`; a Storage Resource's directory not opened yet
    /// ([`Directory::opening`]). A database Resource whose connection does
    /// not open, or with no key to open it, is [`Reach::Unserved`] and not
    /// an error: the Resource exists, and is not served.
    fn resource_of(&self, row: &Row) -> Result<Resource, DbError> {
        let id: i64 = get(row, 0)?;
        let name: String = get(row, 1)?;
        let kind: &str = get(row, 8)?;
        let (kind, reach) = match kind {
            "storage" => {
                let location = sealed::open_location(get(row, 5)?).ok_or_else(|| {
                    DbError::unexpected("a Storage Resource's stored location cannot be read")
                })?;
                let backend = match location {
                    Location::Directory { directory } => Directory::new(
                        &name,
                        std::path::Path::new(&directory),
                        self.root.as_ref().map(Arc::clone),
                        &self.files,
                    ),
                };
                (ResourceKind::Storage, Reach::Storage(RwLock::new(backend)))
            }
            _ => {
                let stored: Vec<u8> = get(row, 5)?;
                let reach = match &self.vault {
                    Some(vault) if !crate::oidc::seal::is_sealed(&stored) => {
                        // The clear form of before, which the sealing at start
                        // did not reach in time: not served until a later
                        // start seals it (blackbox/CONTRACT.md, "Resources
                        // and Grants in full: At start").
                        unserved("not-sealed")
                    }
                    Some(vault) => match sealed::open(&stored, vault, id, &name) {
                        Some(connection) => {
                            // Copied once, as the Resource is loaded: the
                            // logins memory holds beside the pools.
                            let kept = connection.clone();
                            let pools = connection.into_pools(&name, &self.limits);
                            Reach::Database(RwLock::new(Database::Served {
                                pools,
                                connection: kept,
                            }))
                        }
                        None => unserved("does-not-open"),
                    },
                    None => unserved("no-secret-key"),
                };
                (ResourceKind::Postgres, reach)
            }
        };
        Ok(Resource {
            id,
            kind,
            reach,
            name,
            texts: RwLock::new(Texts {
                description: Arc::from(get::<String>(row, 2)?),
                owner: Arc::from(get::<String>(row, 3)?),
                contact: Arc::from(get::<String>(row, 4)?),
            }),
            created_by: get(row, 6)?,
            created_at: get(row, 7)?,
            grants: RwLock::new(Grants::Known(BTreeMap::new())),
            writes: tokio::sync::Mutex::new(()),
            pulls: Semaphore::new(PULLS_AT_ONCE),
            removal: std::sync::Mutex::new(None),
            location_change: std::sync::Mutex::new(None),
        })
    }

    /// The Resource `name`, if one is registered. From memory.
    pub fn get(&self, name: &str) -> Option<Arc<Resource>> {
        self.all
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(Arc::clone)
    }

    /// The directories of the Storage Resources registered, for the check
    /// that a new one is not one of them, inside one or holding one. From
    /// memory.
    pub fn storage_taken(&self) -> Vec<crate::storage::directory::Taken> {
        self.all
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter_map(|resource| resource.storage().map(|directory| directory.taken()))
            .collect()
    }

    /// Every Resource, by name. From memory.
    pub fn list(&self) -> Vec<Arc<Resource>> {
        self.all
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Arc::clone)
            .collect()
    }

    /// Whether this program was started with the installation's key: a
    /// database Resource cannot be registered without it.
    pub fn has_vault(&self) -> bool {
        self.vault.is_some()
    }

    /// Registers `new`, by `by`. The name must have passed
    /// [`is_resource_name`]. Does not connect to the database.
    ///
    /// A database Resource's connection is sealed in the program before the
    /// `INSERT` that carries it is built (crate::credentials): in one
    /// transaction, its row's id is taken from the sequence first, the
    /// associated data is bound to that id and the name, and the `INSERT`
    /// sends sealed bytes and never a clear credential. One transaction, so
    /// that a registration whose answer is lost is still one write that may
    /// land.
    pub async fn register(&self, new: NewResource, by: &str) -> Result<Registered, DbError> {
        let kind = match &new.reach {
            NewReach::Database(_) => ResourceKind::Postgres,
            NewReach::Storage { .. } => ResourceKind::Storage,
        };
        let kind_name = kind.name();
        let inserted = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // The cap is decided under the table's own lock, so two
                // registrations at the 999th cannot both pass; ACCESS SHARE
                // readers (the start's load) are not blocked.
                transaction
                    .batch_execute("LOCK TABLE resources IN SHARE ROW EXCLUSIVE MODE")
                    .await
                    .map_err(DbError::from)?;
                let count: i64 = transaction
                    .query_one("SELECT count(*) FROM resources", &[])
                    .await
                    .and_then(|row| row.try_get(0))
                    .map_err(DbError::from)?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_RESOURCES {
                    return Ok(Inserted::TooMany);
                }
                let row = match &new.reach {
                    NewReach::Database(connection) => {
                        let vault = self.vault.as_ref().ok_or_else(|| {
                            DbError::unexpected(
                                "a database Resource cannot be registered without the key",
                            )
                        })?;
                        let id: i64 = transaction
                            .query_one(
                                "SELECT nextval(pg_get_serial_sequence('resources', 'id'))",
                                &[],
                            )
                            .await
                            .and_then(|row| row.try_get(0))
                            .map_err(DbError::from)?;
                        let stored =
                            sealed::seal(connection, vault, id, &new.name).ok_or_else(|| {
                                DbError::unexpected("a Resource's connection cannot be sealed")
                            })?;
                        transaction
                            .query_opt(
                                concat!(
                                    "INSERT INTO resources (id, name, kind, description, owner,
                                                            contact, connection, created_by)
                                     OVERRIDING SYSTEM VALUE
                                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                     ON CONFLICT (name) DO NOTHING
                                     RETURNING id, ",
                                    rfc3339!("created_at")
                                ),
                                &[
                                    &id,
                                    &new.name,
                                    &kind_name,
                                    &new.description,
                                    &new.owner,
                                    &new.contact,
                                    &stored,
                                    &by,
                                ],
                            )
                            .await
                    }
                    NewReach::Storage { location, .. } => {
                        let stored = sealed::seal_location(location).ok_or_else(|| {
                            DbError::unexpected("a Resource's location cannot be stored")
                        })?;
                        transaction
                            .query_opt(
                                concat!(
                                    "INSERT INTO resources (name, kind, description, owner,
                                                            contact, connection, created_by)
                                     VALUES ($1, $2, $3, $4, $5, $6, $7)
                                     ON CONFLICT (name) DO NOTHING
                                     RETURNING id, ",
                                    rfc3339!("created_at")
                                ),
                                &[
                                    &new.name,
                                    &kind_name,
                                    &new.description,
                                    &new.owner,
                                    &new.contact,
                                    &stored,
                                    &by,
                                ],
                            )
                            .await
                    }
                }
                .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Inserted::One(row))
            })
            .await;
        let row = match inserted {
            Ok(Inserted::TooMany) => return Ok(Registered::TooMany),
            Ok(Inserted::One(Some(row))) => row,
            Ok(Inserted::One(None)) => {
                // Registered before, perhaps by a registration whose answer
                // was lost: known from this answer on.
                if let Err(error) = self.find(&new.name).await {
                    unknown("resource-unknown", &new.name, &error);
                }
                return Ok(Registered::NameTaken);
            }
            // The insert may still commit after this answer: nothing is
            // kept of it, and the next request of the API that names it
            // reads it (`find`).
            Err(error) => return Err(error),
        };
        let resource = Arc::new(Resource {
            id: get(&row, 0)?,
            created_at: get(&row, 1)?,
            kind,
            reach: match new.reach {
                NewReach::Database(connection) => {
                    // Copied once, as the Resource is registered.
                    let kept = connection.clone();
                    let pools = connection.into_pools(&new.name, &self.limits);
                    Reach::Database(RwLock::new(Database::Served {
                        pools,
                        connection: kept,
                    }))
                }
                NewReach::Storage { backend, .. } => Reach::Storage(RwLock::new(backend)),
            },
            name: new.name,
            texts: RwLock::new(Texts {
                description: Arc::from(new.description),
                owner: Arc::from(new.owner),
                contact: Arc::from(new.contact),
            }),
            created_by: by.to_owned(),
            grants: RwLock::new(Grants::Known(BTreeMap::new())),
            writes: tokio::sync::Mutex::new(()),
            pulls: Semaphore::new(PULLS_AT_ONCE),
            removal: std::sync::Mutex::new(None),
            location_change: std::sync::Mutex::new(None),
        });
        self.all
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(resource.name.clone())
            .or_insert_with(|| Arc::clone(&resource));
        Ok(Registered::Registered(resource))
    }

    /// Takes new description, owner and contact for `name`, in one
    /// transaction that takes the row's lock first; `None` when the row is
    /// not there. Answers the row's whole texts afterwards.
    pub async fn change_texts(
        &self,
        id: i64,
        resource_name: &str,
        description: Option<&str>,
        owner: Option<&str>,
        contact: Option<&str>,
    ) -> Result<Option<(String, String, String)>, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        "SELECT description, owner, contact FROM resources
                          WHERE id = $1 AND name = $2 FOR NO KEY UPDATE",
                        &[&id, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(None);
                };
                let read = |at: usize| row.try_get::<_, String>(at).map_err(DbError::from);
                let description = match description {
                    Some(text) => text.to_owned(),
                    None => read(0)?,
                };
                let owner = match owner {
                    Some(text) => text.to_owned(),
                    None => read(1)?,
                };
                let contact = match contact {
                    Some(text) => text.to_owned(),
                    None => read(2)?,
                };
                transaction
                    .execute(
                        "UPDATE resources
                            SET description = $2, owner = $3, contact = $4
                          WHERE id = $1 AND name = $5",
                        &[&id, &description, &owner, &contact, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some((description, owner, contact)))
            })
            .await
    }

    /// A write of a Resource's row whose answer was lost: its Grants, and
    /// the row whole, are not known until they are read back.
    pub fn write_failed(&self, resource: &Resource) {
        let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
        resource.forget(epoch, &self.unknown);
        log::record(
            "grants-unknown",
            serde_json::json!({ "resource": &resource.name }),
        );
    }

    /// What replacing a Resource's logins came to.
    pub async fn replace_connection(
        &self,
        id: i64,
        resource_name: &str,
        expected: Option<&Connection>,
        new: &Connection,
    ) -> Result<Replaced, DbError> {
        let vault = self.vault.as_ref().ok_or_else(|| {
            DbError::unexpected("a Resource's logins cannot be sealed without the key")
        })?;
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        "SELECT connection FROM resources
                          WHERE id = $1 AND name = $2 FOR NO KEY UPDATE",
                        &[&id, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(Replaced::NoSuchResource);
                };
                let stored: Vec<u8> = row.try_get(0).map_err(DbError::from)?;
                if let Some(expected) = expected {
                    // The logins as written: another replacement between the
                    // finding and this write is `changed-meanwhile`. The row
                    // is the very Resource held, by its id, so the seal is
                    // bound to that id and name.
                    let current = sealed::open(&stored, vault, id, resource_name);
                    if current.as_ref() != Some(expected) {
                        transaction.commit().await.map_err(DbError::from)?;
                        return Ok(Replaced::ChangedMeanwhile);
                    }
                }
                let sealed = sealed::seal(new, vault, id, resource_name).ok_or_else(|| {
                    DbError::unexpected("a Resource's connection cannot be sealed")
                })?;
                transaction
                    .execute(
                        "UPDATE resources SET connection = $2 WHERE id = $1",
                        &[&id, &sealed],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Replaced::Replaced)
            })
            .await
    }

    /// Serves `resource` with `connection` from now on: every operation
    /// sent after this takes the new pools; the old pools close when their
    /// last operation lets go.
    pub(crate) fn install_connection(&self, resource: &Resource, connection: Connection) {
        let pools = connection.clone().into_pools(&resource.name, &self.limits);
        resource.replace_pools(pools, connection);
    }

    /// Changes `name`'s stored place to `new`, in one transaction that
    /// takes the row's lock first, when the row still holds `expected`'s
    /// place; `Same` when it already holds `new`. The caller installs the
    /// opened directory in memory once the row is certainly written.
    pub async fn change_location(
        &self,
        id: i64,
        resource_name: &str,
        expected: &std::path::Path,
        new: &Location,
    ) -> Result<LocationChanged, DbError> {
        let stored_new = sealed::seal_location(new)
            .ok_or_else(|| DbError::unexpected("a Resource's location cannot be stored"))?;
        let new_path = match new {
            Location::Directory { directory } => std::path::PathBuf::from(directory),
        };
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        "SELECT connection FROM resources WHERE id = $1 AND name = $2 FOR NO KEY UPDATE",
                        &[&id, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(LocationChanged::NoSuchResource);
                };
                let stored: Vec<u8> = row.try_get(0).map_err(DbError::from)?;
                let current = sealed::open_location(&stored).ok_or_else(|| {
                    DbError::unexpected("a Resource's stored location cannot be read")
                })?;
                let current_path = match current {
                    Location::Directory { directory } => std::path::PathBuf::from(directory),
                };
                if current_path == new_path {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(LocationChanged::Same);
                }
                if current_path != expected {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(LocationChanged::ChangedMeanwhile);
                }
                transaction
                    .execute(
                        "UPDATE resources SET connection = $2 WHERE id = $1 AND name = $3",
                        &[&id, &stored_new, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(LocationChanged::Changed)
            })
            .await
    }

    /// Removes `name`'s row and its Grants, in one transaction that takes
    /// the row's lock first; the caller lets the Resource go from memory
    /// once this says the row is certainly gone (`forget_removed`). Its
    /// pools close once the last request holding it lets go.
    pub async fn remove(&self, id: i64, resource_name: &str) -> Result<Removed, DbError> {
        let removed = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        "SELECT 1 FROM resources WHERE id = $1 AND name = $2 FOR NO KEY UPDATE",
                        &[&id, &resource_name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(_row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(None);
                };
                let grants: i64 = transaction
                    .query_one("SELECT count(*) FROM grants WHERE resource_id = $1", &[&id])
                    .await
                    .and_then(|row| row.try_get(0))
                    .map_err(DbError::from)?;
                transaction
                    .execute("DELETE FROM grants WHERE resource_id = $1", &[&id])
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute("DELETE FROM resources WHERE id = $1", &[&id])
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(grants))
            })
            .await?;
        Ok(match removed {
            Some(grants) => Removed::Removed {
                grants: u64::try_from(grants).unwrap_or(0),
            },
            None => Removed::NoSuchResource,
        })
    }

    /// Removes the Resource `name` and its Grants, in the database and then
    /// in memory, before this returns; `false` when there was none. Its
    /// pools close once the last request holding it lets go. For the local
    /// run, whose Resources are made again from the project at each run.
    pub async fn unregister(&self, name: &str) -> Result<bool, DbError> {
        let removed = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM grants WHERE resource_id IN
                           (SELECT id FROM resources WHERE name = $1)",
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let removed = transaction
                    .execute("DELETE FROM resources WHERE name = $1", &[&name])
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(removed > 0)
            })
            .await?;
        let gone = self
            .all
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if let Some(gone) = gone {
            gone.let_go(&self.unknown);
        }
        Ok(removed)
    }

    /// Lets `resource` go from memory because its row is certainly gone:
    /// removed from the map (only the very Resource, never one registered
    /// under its name meanwhile), its Grants `LetGo`, and the count of
    /// unknown Resources down by one exactly. Its pools close when the
    /// last request or operation holding it lets go. `false` when memory
    /// did not hold this one.
    pub fn forget_removed(&self, resource: &Resource) -> Option<HeldPath> {
        // The directory is held before the Resource leaves the map, and
        // until the caller's cleaning ends: at no moment is it neither a
        // Resource's nor held, so a registration of that name and directory
        // meanwhile never takes a marker that is about to be removed.
        let held = resource
            .storage()
            .and_then(|directory| self.root.as_ref()?.hold(directory.path()));
        let removed = {
            let mut all = self.all.write().unwrap_or_else(PoisonError::into_inner);
            match all.get(&resource.name) {
                Some(held) if held.id == resource.id => {
                    all.remove(&resource.name);
                    true
                }
                _ => false,
            }
        };
        if removed {
            resource.let_go(&self.unknown);
            if let Some(by) = resource.take_removal() {
                log::record(
                    "resource-removed",
                    serde_json::json!({ "resource": &resource.name, "by": by }),
                );
            }
            held
        } else {
            None
        }
    }

    /// Whether memory holds this very Resource now (the same row, not one
    /// registered under its name since).
    pub fn is_registered(&self, resource: &Resource) -> bool {
        self.all
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&resource.name)
            .is_some_and(|held| held.id == resource.id)
    }

    /// Sets `group`'s Grant on `resource` to `level`, in the database and
    /// then in memory, before this returns. One transaction: the
    /// Resource's row lock first (`FOR NO KEY UPDATE`, held until the
    /// commit is done), then the Grant and its revision. When its answer
    /// is lost the Resource's Grants are forgotten at once.
    pub async fn grant(
        &self,
        resource: &str,
        group: &str,
        level: AccessLevel,
    ) -> Result<GrantWritten, DbError> {
        let Some(found) = self.get(resource) else {
            return Ok(GrantWritten::NoSuchResource);
        };
        let access = level_name(level);
        // Armed before the statement is sent: a future given up, or an
        // answer lost, forgets what memory knows of the Resource.
        let mut unsettled = Unsettled::new(self, &found);
        let written = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let locked = transaction
                    .query_opt(
                        "SELECT 1 FROM resources WHERE id = $1 AND name = $2 FOR NO KEY UPDATE",
                        &[&found.id, &found.name],
                    )
                    .await
                    .map_err(DbError::from)?;
                if locked.is_none() {
                    return Ok(None);
                }
                let row = transaction
                    .query_one(
                        "INSERT INTO grants (resource_id, group_name, access, revision)
                         VALUES ($1, $2, $3, nextval('grant_revisions'))
                         ON CONFLICT (resource_id, group_name) DO UPDATE
                         SET access = EXCLUDED.access, revision = nextval('grant_revisions')
                         RETURNING revision",
                        &[&found.id, &group, &access],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(row))
            })
            .await;
        match written {
            Ok(Some(row)) => {
                let revision = get(&row, 0)?;
                found.install(group, Granted { level, revision });
                unsettled.settled();
                Ok(GrantWritten::Written)
            }
            Ok(None) => {
                // Certain: the row is not there. Memory lets it go, and the
                // guard is disarmed (nothing is unknown).
                self.forget_removed(&found);
                unsettled.settled();
                Ok(GrantWritten::NoSuchResource)
            }
            // Its guard forgets what memory knows: nothing is read now,
            // which could be earlier than a commit on its way.
            Err(error) => Err(error),
        }
    }

    /// The Resource `name`: from memory, or else from the platform
    /// database (a registration whose answer was lost, landed since), with
    /// its Grants, read under its row's share lock taken without waiting,
    /// and then held in memory. For the API's requests that name a
    /// Resource; operations look in memory only.
    pub async fn find(&self, name: &str) -> Result<Option<Arc<Resource>>, DbError> {
        if let Some(found) = self.get(name) {
            return Ok(Some(found));
        }
        if !is_resource_name(name) {
            return Ok(None);
        }
        let read = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // Row locks only: nothing of this transaction must last.
                transaction
                    .batch_execute("SET LOCAL synchronous_commit = off")
                    .await
                    .map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        &format!(
                            "SELECT {RESOURCE_COLUMNS} FROM resources r WHERE r.name = $1 FOR SHARE NOWAIT"
                        ),
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(found) = found else {
                    return Ok(None);
                };
                let id: i64 = get(&found, 0)?;
                let grants = transaction
                    .query(
                        "SELECT group_name, access, revision FROM grants WHERE resource_id = $1",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some((found, grants)))
            })
            .await?;
        let Some((row, grants)) = read else {
            return Ok(None);
        };
        let resource = self.resource_of(&row)?;
        // At most once a minute, as any operation that meets it.
        resource.mention_unserved(false);
        if let Some(directory) = resource.storage() {
            // Its own bound: one Resource, on an API request.
            directory
                .opening(tokio::time::Instant::now() + crate::storage::OPERATION_TIME)
                .await;
        }
        *resource
            .grants
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Grants::Known(grants_of(&grants)?);
        let resource = Arc::new(resource);
        let held = Arc::clone(
            self.all
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(resource.name.clone())
                .or_insert(resource),
        );
        Ok(Some(held))
    }

    /// Whether memory has forgotten the Grants of any Resource now: one
    /// atomic load, no lock.
    pub fn any_grants_unknown(&self) -> bool {
        self.unknown.load(Ordering::SeqCst) > 0
    }

    /// Reads `resource`'s Grants back when memory does not know them, and
    /// answers whether it knows them after. Never waits for the platform
    /// database: at most [`READ_BACKS`] readings at once in the program
    /// (one more is not made), done by `deadline` (the caller's, shared by
    /// all its readings), and not made while a write of a Grant is on its
    /// way (the row is locked).
    pub async fn read_back(&self, resource: &Resource, deadline: Instant) -> ReadBack {
        if resource.forgotten_at().is_none() {
            return ReadBack::Known;
        }
        if Instant::now() >= deadline {
            return ReadBack::Unknown;
        }
        // The Resource's write turn: a reading never installs while a write
        // of its row is between the database and memory, and never before a
        // reading that began earlier. Taken before the program's place for a
        // reading, so that readings waiting for one Resource's write do not
        // hold the places the other Resources' readings need.
        let Ok(_turn) = tokio::time::timeout_at(deadline, resource.writes.lock()).await else {
            return ReadBack::Unknown;
        };
        let Ok(_permit) = self.reading_back.try_acquire() else {
            return ReadBack::Unknown;
        };
        self.read_back_locked(resource, deadline).await
    }

    /// The reading and the install, for a caller that already holds the
    /// Resource's write turn: a write's own answer, read after it wrote and
    /// before the turn is given back, so memory is never older than the row.
    pub async fn read_back_locked(&self, resource: &Resource, deadline: Instant) -> ReadBack {
        let Some(began) = resource.forgotten_at() else {
            return ReadBack::Known;
        };
        if Instant::now() >= deadline {
            return ReadBack::Unknown;
        }
        let read = tokio::time::timeout_at(deadline, self.row_of(resource.id))
            .await
            .unwrap_or_else(|_| {
                Err(DbError::timed_out(
                    "reading a Resource's row back",
                    READ_BACK_WAIT,
                ))
            });
        match read {
            Ok(Some(row)) => {
                // The row whole, prepared before the one lock that decides
                // whether the reading is taken: its texts, and for a
                // database its stored connection opened or not.
                let texts = (
                    row.description.clone(),
                    row.owner.clone(),
                    row.contact.clone(),
                );
                let storage_before = resource.storage();
                let pending = resource.pending_location();
                let row_location: Option<std::path::PathBuf> =
                    if row.kind == ResourceKind::Storage.name() {
                        sealed::open_location(&row.connection).map(|location| {
                            let Location::Directory { directory } = location;
                            std::path::PathBuf::from(directory)
                        })
                    } else {
                        None
                    };
                // A location change whose answer was lost: when the row
                // holds another place than the one it was to, the change
                // did not land, and exactly what it made there is undone
                // (never a marker or staging that was there before).
                let stale = pending.as_ref().and_then(|pending| {
                    row_location
                        .as_ref()
                        .filter(|path| **path != pending.path)
                        .map(|_| {
                            (
                                pending.path.clone(),
                                pending.made_marker,
                                pending.made_staging,
                            )
                        })
                });
                // A reading that finds the row forgets who asked for a
                // removal: it did not land.
                resource.clear_removal();
                let reach = self.reach_of(resource, &row);
                let storage_changed = matches!(reach, Some(ReachRead::Storage(_)));
                let installed =
                    resource.install_read(row.grants, texts, reach, began, &self.unknown);
                if installed {
                    if storage_changed && let Some(directory) = resource.storage() {
                        // The new place is opened now, as `find` opens a
                        // Storage Resource it reads, in the reading's time;
                        // past it, the first operation opens it.
                        directory.opening(deadline).await;
                    }
                    let left = deadline.saturating_duration_since(Instant::now());
                    if storage_changed {
                        // The change landed: its record is written (by the
                        // install, before this), and the old directory's
                        // marker and empty staging go, in the time the
                        // reading has left.
                        if let (Some(old), Some(path)) = (&storage_before, &row_location)
                            && old.path() != path.as_path()
                        {
                            old.clean(left).await;
                        }
                    } else if let (Some(pending), Some(path)) = (&pending, &row_location)
                        && pending.path == *path
                        && let Some(pending) = resource.take_location_change()
                    {
                        // Memory already held the place the row has: the
                        // change is in force and its record is owed.
                        log::record(
                            "resource-location-changed",
                            serde_json::json!({ "resource": &resource.name, "by": pending.by }),
                        );
                    }
                }
                if let Some((path, marker, staging)) = stale {
                    resource.clear_location_change();
                    if let Some(root) = &self.root {
                        let backend = Directory::new(
                            &resource.name,
                            &path,
                            Some(Arc::clone(root)),
                            &self.files,
                        );
                        let left = deadline.saturating_duration_since(Instant::now());
                        backend.undo_made(marker, staging, left).await;
                    }
                }
            }
            // A reading that finds no row means the Resource was removed:
            // it is let go from memory, at once and for good.
            Ok(None) => {
                // A removal found out: its record is written (by the let
                // go, before this), and its directory is cleaned as an
                // answered removal would, in the time left.
                let storage = resource.storage();
                let held = self.forget_removed(resource);
                // Cleaned only while the directory is held: without the
                // hold (the claims are full) it is left, as one that could
                // not be cleaned in time.
                if let (Some(directory), Some(_held)) = (storage, &held) {
                    let left = deadline.saturating_duration_since(Instant::now());
                    directory.clean(left).await;
                }
                drop(held);
                return ReadBack::Removed;
            }
            Err(error) => {
                // A write on its way is no failure: it is waited out by
                // trying again later.
                if error.code() != Some(&SqlState::LOCK_NOT_AVAILABLE) {
                    unknown("grants-unknown", &resource.name, &error);
                }
            }
        }
        if resource.grants_known() {
            ReadBack::Known
        } else {
            ReadBack::Unknown
        }
    }

    /// What a reading of the row says a database Resource is reached by
    /// now, when that is not what memory holds: its opened connection with
    /// new pools, or why it is not served. `None` for a row sealed as memory
    /// has it (taken as it is: its pools and their connections stay, and a
    /// problem already said is not said again), and for a Storage Resource
    /// (its location is pass B's). The caller holds the Resource's write
    /// turn, so what memory holds does not change between this and the
    /// install.
    fn reach_of(&self, resource: &Resource, row: &StoredRow) -> Option<ReachRead> {
        if row.kind == ResourceKind::Storage.name() {
            let location = sealed::open_location(&row.connection)?;
            let Location::Directory { directory } = location;
            let path = std::path::PathBuf::from(directory);
            if resource
                .storage()
                .is_some_and(|held| held.path() == path.as_path())
            {
                return None;
            }
            let backend = Directory::new(
                &resource.name,
                &path,
                self.root.as_ref().map(Arc::clone),
                &self.files,
            );
            return Some(ReachRead::Storage(backend));
        }
        if row.kind != ResourceKind::Postgres.name() {
            return None;
        }
        let unserved = |problem: &'static str| {
            (!resource.reached_as(Err(problem))).then(|| {
                Database::Unserved(Unserved {
                    problem,
                    said: std::sync::Mutex::new(None),
                })
            })
        };
        let Some(vault) = &self.vault else {
            return Some(ReachRead::Database(unserved("no-secret-key")?));
        };
        if !crate::oidc::seal::is_sealed(&row.connection) {
            return Some(ReachRead::Database(unserved("not-sealed")?));
        }
        match sealed::open(&row.connection, vault, resource.id, &resource.name) {
            Some(connection) => {
                if resource.reached_as(Ok(&connection)) {
                    return None;
                }
                // Copied once, for a Resource whose logins changed.
                let pools = connection.clone().into_pools(&resource.name, &self.limits);
                Some(ReachRead::Database(Database::Served { pools, connection }))
            }
            None => Some(ReachRead::Database(unserved("does-not-open")?)),
        }
    }

    /// The Resource's row whole (its stored connection, texts and kind) and
    /// its Grants, never earlier than a write of them: in one transaction,
    /// its row's share lock taken without waiting (`NOWAIT`: a write holds
    /// the row's lock until its commit is done, so while one is on its way
    /// this fails at once with `lock_not_available`), then, in statements
    /// whose snapshot is after the lock was got, the Grants. `None` for no
    /// such Resource.
    async fn row_of(&self, id: i64) -> Result<Option<StoredRow>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                // Row locks only: nothing of this transaction must last, so
                // its commit need not wait to be flushed or replicated.
                transaction
                    .batch_execute("SET LOCAL synchronous_commit = off")
                    .await
                    .map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        "SELECT connection, description, owner, contact, kind
                           FROM resources WHERE id = $1 FOR SHARE NOWAIT",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(found) = found else {
                    return Ok(None);
                };
                let rows = transaction
                    .query(
                        "SELECT group_name, access, revision FROM grants WHERE resource_id = $1",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(StoredRow {
                    connection: found.try_get(0).map_err(DbError::from)?,
                    description: found.try_get(1).map_err(DbError::from)?,
                    owner: found.try_get(2).map_err(DbError::from)?,
                    contact: found.try_get(3).map_err(DbError::from)?,
                    kind: found.try_get(4).map_err(DbError::from)?,
                    grants: grants_of(&rows)?,
                }))
            })
            .await?;
        Ok(row)
    }
}

/// What a reading of a Resource's row says it is reached by now: a
/// database's opened connection or why not, or a Storage Resource's place.
#[expect(
    clippy::large_enum_variant,
    reason = "one per read-back, moved into memory at once, never held in a collection"
)]
enum ReachRead {
    Database(Database),
    Storage(Directory),
}

/// A Resource's row as the read-back finds it.
#[derive(Debug)]
struct StoredRow {
    connection: Vec<u8>,
    description: String,
    owner: String,
    contact: String,
    kind: String,
    grants: BTreeMap<String, Granted>,
}

/// A record of something that could not be read back, without the
/// database's own words (which could name a role).
fn unknown(event: &str, resource: &str, error: &DbError) {
    log::record(
        event,
        serde_json::json!({ "resource": resource, "error": error.to_string() }),
    );
}

/// The Grants of rows `group_name, access, revision`, by Group.
fn grants_of(rows: &[Row]) -> Result<BTreeMap<String, Granted>, DbError> {
    let mut grants = BTreeMap::new();
    for row in rows {
        let (group, granted) = granted_of(row, 0)?;
        // Copied: memory keeps its own Grants; once per Grant read back.
        grants.insert(group.to_owned(), granted);
    }
    Ok(grants)
}

/// A Grant read from a row: group, access and revision from column `at` on.
fn granted_of(row: &Row, at: usize) -> Result<(&str, Granted), DbError> {
    let group: &str = get(row, at)?;
    let level = level_of(get(row, at + 1)?)
        .ok_or_else(|| DbError::unexpected("a Grant of an unknown Access level"))?;
    let revision = get(row, at + 2)?;
    Ok((group, Granted { level, revision }))
}

fn get<'a, T: FromSql<'a>>(row: &'a Row, index: usize) -> Result<T, DbError> {
    row.try_get(index).map_err(DbError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_names_are_lower_case_letters_digits_and_underscores_from_a_letter() {
        let longest = format!("a{}", "b".repeat(62));
        for name in ["a", "devops", "x9_", "a_1_b", longest.as_str()] {
            assert!(is_resource_name(name), "{name}");
        }
        let too_long = format!("a{}", "b".repeat(63));
        for name in [
            "",
            "Devops",
            "1a",
            "_a",
            "dev-ops",
            "dev ops",
            "dévops",
            too_long.as_str(),
        ] {
            assert!(!is_resource_name(name), "{name}");
        }
    }

    #[test]
    fn a_reading_is_taken_only_in_its_forgetting_and_with_every_revision_since() {
        let read = |revision| {
            let mut grants = BTreeMap::new();
            grants.insert(
                "sre".to_owned(),
                Granted {
                    level: AccessLevel::Read,
                    revision,
                },
            );
            grants
        };
        let unknown = Grants::Unknown { seen: 3, epoch: 7 };
        assert!(reading_wins(&unknown, 7, &read(3)));
        assert!(reading_wins(&unknown, 7, &read(4)));
        assert!(!reading_wins(&unknown, 7, &read(2)));
        assert!(!reading_wins(&unknown, 8, &read(4)));
        assert!(!reading_wins(&Grants::Known(BTreeMap::new()), 7, &read(4)));
        assert!(!reading_wins(&Grants::LetGo, 7, &read(4)));
    }

    #[test]
    fn a_connection_never_prints_and_survives_being_stored() {
        let connection = Connection::new(
            "db.example".into(),
            6543,
            "secret_db".into(),
            Login::new("secret_role".into(), "secret-password".into()),
            Some(Login::new(
                "secret_writer".into(),
                "secret-write-password".into(),
            )),
            Some(Login::new(
                "secret_structure".into(),
                "secret-structure-password".into(),
            )),
        );
        assert_eq!(format!("{connection:?}"), "Connection(<hidden>)");
        let key = crate::oidc::key::InstallationKey::random().unwrap();
        let vault = Vault::new(&key).unwrap();
        let stored = sealed::seal(&connection, &vault, 7, "devops").unwrap();
        let opened = sealed::open(&stored, &vault, 7, "devops").unwrap();
        assert_eq!(opened.port, 6543);
        assert_eq!(opened.read.role, "secret_role");
        assert_eq!(
            opened.write.map(|w| w.role).as_deref(),
            Some("secret_writer")
        );
        assert_eq!(
            opened.structure.map(|s| s.role).as_deref(),
            Some("secret_structure")
        );
        assert!(sealed::open(b"not what was stored", &vault, 7, "devops").is_none());
        // A value sealed for one row does not open in another.
        assert!(sealed::open(&stored, &vault, 8, "devops").is_none());
        assert!(sealed::open(&stored, &vault, 7, "ledger").is_none());
        // What was stored before this ticket, in clear, does not open.
        let older = br#"{"host":"h","port":1,"database":"d","read":{"role":"r","password":"p"}}"#;
        assert!(sealed::open(older, &vault, 7, "devops").is_none());
    }
}
