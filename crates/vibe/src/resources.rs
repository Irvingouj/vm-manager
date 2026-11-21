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
//! place that knows the stored form (the ticket "Resources and Grants in
//! full" encrypts it there). Each Resource holds a pool per login (crate::
//! executor): its read pool, its write pool when it was registered with a
//! write login, and its structure pool when it was registered with a
//! structure login (the Structure puller's, crate::structure). A pool
//! connects on first use.
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

use crate::executor::{self, Level, Levels, Password, Source, Target};
use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::registry::ResourceKind;
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
#[derive(Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize)]
pub struct Login {
    role: String,
    password: String,
}

impl Login {
    pub fn new(role: String, password: String) -> Self {
        Self { role, password }
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Connection(<hidden>)")
    }
}

impl Connection {
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
            executor::Pool::new(source, Levels::Only(Level::Write), target, limits.clone())
        });
        let structure = self.structure.map(|login| {
            // Copied, as the write login's target is.
            let target = target(login, self.host.clone(), self.database.clone());
            let source = Source::Resource(resource.to_owned());
            executor::Pool::new(source, Levels::Only(Level::Read), target, limits.clone())
        });
        let read = target(self.read, self.host, self.database);
        Pools {
            read: executor::Pool::new(
                Source::Resource(resource.to_owned()),
                Levels::Only(Level::Read),
                read,
                limits.clone(),
            ),
            write,
            structure,
        }
    }
}

/// A Resource's pools, one per login. Their `Debug` names the Resource
/// and the level only.
#[derive(Debug)]
struct Pools {
    read: executor::Pool,
    write: Option<executor::Pool>,
    /// The structure login's, which reads in read-only transactions only.
    structure: Option<executor::Pool>,
}

/// The stored form of a [`Connection`] (and of a Storage Resource's
/// location): the one place that writes and reads it. For now the Connection as JSON; the ticket "Resources and Grants in
/// full" encrypts it here, and nothing else changes.
mod sealed {
    use super::Connection;

    pub(super) fn seal(connection: &Connection) -> Option<Vec<u8>> {
        serde_json::to_vec(connection).ok()
    }

    /// `None` if it cannot be read; the reason is not given, as it could
    /// quote what is stored.
    pub(super) fn open(stored: &[u8]) -> Option<Connection> {
        serde_json::from_slice(stored).ok()
    }

    /// A Storage Resource's location, kept as a connection is.
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

/// How a Resource is reached: by its pools, or by its backend.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per Resource, inside its Arc, never moved after it is made"
)]
enum Reach {
    Database(Pools),
    Storage(Directory),
}

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
    pub description: String,
    pub owner: String,
    pub contact: String,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub kind: ResourceKind,
    /// A database's pools, one per login (a connection of one never serves
    /// the other); or a place of files' backend.
    reach: Reach,
    /// Held only for a lookup or a change, never across an await.
    grants: RwLock<Grants>,
    /// The pulls of its structure running now: at most [`PULLS_AT_ONCE`].
    pulls: Semaphore,
}

/// At most this many pulls of one Resource's structure run at once
/// (blackbox/CONTRACT.md, "What a pull reads"), so that pulls, which may
/// share the read login's pool with Applications, never hold more of its
/// connections than this.
pub const PULLS_AT_ONCE: usize = 2;

impl Resource {
    fn pools(&self) -> Option<&Pools> {
        match &self.reach {
            Reach::Database(pools) => Some(pools),
            Reach::Storage(_) => None,
        }
    }

    /// The pool of the read login; `None` for a Resource that is no
    /// database.
    pub fn read_pool(&self) -> Option<&executor::Pool> {
        self.pools().map(|pools| &pools.read)
    }

    /// The pool of the write login, if the Resource is a database and has
    /// one.
    pub fn write_pool(&self) -> Option<&executor::Pool> {
        self.pools().and_then(|pools| pools.write.as_ref())
    }

    /// The pool the Structure puller reads the catalogs with: the structure
    /// login's when the Resource has one, else the read login's. Never the
    /// write login's. `None` for a Resource that is no database.
    pub fn structure_pool(&self) -> Option<&executor::Pool> {
        self.pools()
            .map(|pools| pools.structure.as_ref().unwrap_or(&pools.read))
    }

    /// The backend of a Storage Resource; `None` for a database.
    pub fn storage(&self) -> Option<&Directory> {
        match &self.reach {
            Reach::Storage(backend) => Some(backend),
            Reach::Database(_) => None,
        }
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
    fn install_read(&self, read: BTreeMap<String, Granted>, began: u64, unknown: &AtomicUsize) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        if let Grants::Unknown { seen, epoch } = &*grants
            && *epoch == began
            && *seen <= Grants::newest(&read)
        {
            *grants = Grants::Known(read);
            unknown.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// What registering came to.
#[derive(Debug)]
pub enum Registered {
    Registered(Arc<Resource>),
    NameTaken,
}

/// What writing a Grant came to.
#[derive(Debug, PartialEq, Eq)]
pub enum GrantWritten {
    Written,
    NoSuchResource,
}

pub struct Resources {
    database: Arc<PlatformDatabase>,
    limits: executor::Limits,
    /// The file threads Storage Resources' directories are opened on.
    files: Arc<Files>,
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
        limits: executor::Limits,
        reading_back: Arc<Semaphore>,
        files: Arc<Files>,
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
            limits,
            files,
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
            let resource = this.resource_of(row).await?;
            let grants = read.remove(&resource.id).unwrap_or_default();
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
        *this.all.write().unwrap_or_else(PoisonError::into_inner) = all;
        Ok(this)
    }

    async fn resource_of(&self, row: &Row) -> Result<Resource, DbError> {
        let name: String = get(row, 1)?;
        let kind: &str = get(row, 8)?;
        let (kind, reach) = match kind {
            "storage" => {
                let location = sealed::open_location(get(row, 5)?).ok_or_else(|| {
                    DbError::unexpected("a Storage Resource's stored location cannot be read")
                })?;
                let backend = match location {
                    Location::Directory { directory } => {
                        Directory::load(&name, std::path::Path::new(&directory), &self.files).await
                    }
                };
                (ResourceKind::Storage, Reach::Storage(backend))
            }
            _ => {
                let connection = sealed::open(get(row, 5)?).ok_or_else(|| {
                    DbError::unexpected("a Resource's stored connection cannot be read")
                })?;
                (
                    ResourceKind::Postgres,
                    Reach::Database(connection.into_pools(&name, &self.limits)),
                )
            }
        };
        Ok(Resource {
            id: get(row, 0)?,
            kind,
            reach,
            name,
            description: get(row, 2)?,
            owner: get(row, 3)?,
            contact: get(row, 4)?,
            created_by: get(row, 6)?,
            created_at: get(row, 7)?,
            grants: RwLock::new(Grants::Known(BTreeMap::new())),
            pulls: Semaphore::new(PULLS_AT_ONCE),
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
            .filter_map(|resource| resource.storage().map(Directory::taken))
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

    /// Registers `new`, by `by`. The name must have passed
    /// [`is_resource_name`]. Does not connect to the database.
    pub async fn register(&self, new: NewResource, by: &str) -> Result<Registered, DbError> {
        let (kind, stored) = match &new.reach {
            NewReach::Database(connection) => (ResourceKind::Postgres, sealed::seal(connection)),
            NewReach::Storage { location, .. } => {
                (ResourceKind::Storage, sealed::seal_location(location))
            }
        };
        let stored = stored
            .ok_or_else(|| DbError::unexpected("a Resource's connection cannot be stored"))?;
        let kind_name = kind.name();
        let inserted = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "INSERT INTO resources (name, kind, description, owner, contact,
                                                    connection, created_by)
                             VALUES ($1, $7, $2, $3, $4, $5, $6)
                             ON CONFLICT (name) DO NOTHING
                             RETURNING id, ",
                            rfc3339!("created_at")
                        ),
                        &[
                            &new.name,
                            &new.description,
                            &new.owner,
                            &new.contact,
                            &stored,
                            &by,
                            &kind_name,
                        ],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        let row = match inserted {
            Ok(Some(row)) => row,
            Ok(None) => {
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
                    Reach::Database(connection.into_pools(&new.name, &self.limits))
                }
                NewReach::Storage { backend, .. } => Reach::Storage(backend),
            },
            name: new.name,
            description: new.description,
            owner: new.owner,
            contact: new.contact,
            created_by: by.to_owned(),
            grants: RwLock::new(Grants::Known(BTreeMap::new())),
            pulls: Semaphore::new(PULLS_AT_ONCE),
        });
        self.all
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(resource.name.clone())
            .or_insert_with(|| Arc::clone(&resource));
        Ok(Registered::Registered(resource))
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
        let written = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let locked = transaction
                    .query_opt(
                        "SELECT 1 FROM resources WHERE id = $1 FOR NO KEY UPDATE",
                        &[&found.id],
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
                Ok(GrantWritten::Written)
            }
            Ok(None) => Ok(GrantWritten::NoSuchResource),
            Err(error) => {
                // The commit may land after this answer: nothing is read
                // now, which could be earlier than it.
                let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
                found.forget(epoch, &self.unknown);
                log::record(
                    "grants-unknown",
                    serde_json::json!({ "resource": &found.name }),
                );
                Err(error)
            }
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
        let resource = self.resource_of(&row).await?;
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
    pub async fn read_back(&self, resource: &Resource, deadline: Instant) -> bool {
        let Some(began) = resource.forgotten_at() else {
            return true;
        };
        if Instant::now() >= deadline {
            return false;
        }
        let Ok(_permit) = self.reading_back.try_acquire() else {
            return false;
        };
        let read = tokio::time::timeout_at(deadline, self.grants_of(resource.id))
            .await
            .unwrap_or_else(|_| {
                Err(DbError::timed_out(
                    "reading a Resource's Grants back",
                    READ_BACK_WAIT,
                ))
            });
        match read {
            Ok(Some(grants)) => resource.install_read(grants, began, &self.unknown),
            Ok(None) => {}
            Err(error) => {
                // A write on its way is no failure: it is waited out by
                // trying again later.
                if error.code() != Some(&SqlState::LOCK_NOT_AVAILABLE) {
                    unknown("grants-unknown", &resource.name, &error);
                }
            }
        }
        resource.grants_known()
    }

    /// The Grants of the Resource `id`, never earlier than a write of
    /// them: in one transaction, its row's share lock taken without
    /// waiting (`NOWAIT`: a write holds the row's lock until its commit is
    /// done, so while one is on its way this fails at once with
    /// `lock_not_available`), then, in a second statement whose snapshot is
    /// after the lock was got, the Grants. `None` for no such Resource.
    async fn grants_of(&self, id: i64) -> Result<Option<BTreeMap<String, Granted>>, DbError> {
        let rows = self
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
                        "SELECT 1 FROM resources WHERE id = $1 FOR SHARE NOWAIT",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                if found.is_none() {
                    return Ok(None);
                }
                let rows = transaction
                    .query(
                        "SELECT group_name, access, revision FROM grants WHERE resource_id = $1",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(rows))
            })
            .await?;
        rows.as_deref().map(grants_of).transpose()
    }
}

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
        let stored = sealed::seal(&connection).unwrap();
        let opened = sealed::open(&stored).unwrap();
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
        assert!(sealed::open(b"not what was stored").is_none());
        // What was stored before write logins existed still opens.
        let older = br#"{"host":"h","port":1,"database":"d","read":{"role":"r","password":"p"}}"#;
        let opened = sealed::open(older).unwrap();
        assert!(opened.write.is_none());
        assert!(opened.structure.is_none());
    }
}
