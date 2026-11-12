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
//! platform database cannot be reached. Each Grant carries a revision the
//! database takes under the row's lock, and memory keeps the higher, so two
//! writes of one Group's Grant leave memory as the database whatever order
//! they report back in. A write whose answer is lost is read back.
//!
//! A Resource's connection and logins are held as a [`Connection`], which
//! never prints, and stored as the [`sealed`] module writes them, the one
//! place that knows the stored form (the ticket "Resources and Grants in
//! full" encrypts it there). Each Resource holds a pool per login (crate::
//! executor): its read pool, and its write pool when it was registered
//! with a write login. A pool connects on first use.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use authz::AccessLevel;
use serde::{Deserialize, Serialize};
use tokio_postgres::types::FromSql;
use tokio_postgres::{Client, Row};

use crate::executor::{self, Level, Password, Target};
use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};

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
/// administrator gave them: the read login, and optionally the write login.
/// Never printed: its `Debug` shows nothing.
#[derive(Serialize, Deserialize)]
pub struct Connection {
    host: String,
    port: u16,
    database: String,
    read: Login,
    /// Absent in what was stored before write logins existed.
    #[serde(default)]
    write: Option<Login>,
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
    ) -> Self {
        Self {
            host,
            port,
            database,
            read,
            write,
        }
    }

    /// The pools of `resource`: the read login's, and the write login's if
    /// there is one, each with `limits`.
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
            executor::Pool::new(resource, Level::Write, target, limits.clone())
        });
        let read = target(self.read, self.host, self.database);
        Pools {
            read: executor::Pool::new(resource, Level::Read, read, limits.clone()),
            write,
        }
    }
}

/// A Resource's pools, one per login. Their `Debug` names the Resource
/// and the level only.
#[derive(Debug)]
struct Pools {
    read: executor::Pool,
    write: Option<executor::Pool>,
}

/// The stored form of a [`Connection`]: the one place that writes and reads
/// it. For now the Connection as JSON; the ticket "Resources and Grants in
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
}

/// A Resource being registered.
#[derive(Debug)]
pub struct NewResource {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub contact: String,
    pub connection: Connection,
}

#[derive(Clone, Copy, Debug)]
struct Granted {
    level: AccessLevel,
    revision: i64,
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
    /// One pool per login: a connection of one never serves the other.
    pools: Pools,
    /// By Group. Held only for a lookup or a change, never across an await.
    grants: RwLock<BTreeMap<String, Granted>>,
}

impl Resource {
    /// The pool of the read login.
    pub fn read_pool(&self) -> &executor::Pool {
        &self.pools.read
    }

    /// The pool of the write login, if the Resource has one.
    pub fn write_pool(&self) -> Option<&executor::Pool> {
        self.pools.write.as_ref()
    }

    /// Runs `decide` with the Grants on this Resource, as the Authorization
    /// decision takes them.
    pub fn with_grants<T>(&self, decide: impl FnOnce(&[authz::Grant<'_>]) -> T) -> T {
        let grants = self.grants.read().unwrap_or_else(PoisonError::into_inner);
        let list: Vec<authz::Grant<'_>> = grants
            .iter()
            .map(|(group, granted)| authz::Grant {
                group,
                level: granted.level,
            })
            .collect();
        decide(&list)
    }

    /// Each Group's Access level, by Group.
    pub fn grants(&self) -> Vec<(String, AccessLevel)> {
        // Copied out so that the lock is not held while the answer is
        // written.
        self.grants
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(group, granted)| (group.clone(), granted.level))
            .collect()
    }

    /// Takes `level` for `group` unless memory already has a later write.
    fn install(&self, group: &str, granted: Granted) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        match grants.get_mut(group) {
            Some(current) if current.revision >= granted.revision => {}
            Some(current) => *current = granted,
            None => {
                grants.insert(group.to_owned(), granted);
            }
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
    /// By name. Held only for a lookup or an insert, never across an await.
    all: RwLock<BTreeMap<String, Arc<Resource>>>,
}

const RESOURCE_COLUMNS: &str = concat!(
    "r.id, r.name, r.description, r.owner, r.contact, r.connection, r.created_by, ",
    rfc3339!("r.created_at")
);

impl Resources {
    /// Reads every Resource and Grant from the platform database. Each
    /// Resource's pool uses `limits`.
    pub async fn load(
        database: Arc<PlatformDatabase>,
        limits: executor::Limits,
    ) -> Result<Self, DbError> {
        let (resources, grants) = database
            .pool()
            .run(async |client: &mut Client| {
                let resources = client
                    .query(
                        &format!("SELECT {RESOURCE_COLUMNS} FROM resources r ORDER BY r.name"),
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
                let grants = client
                    .query(
                        "SELECT r.name, g.group_name, g.access, g.revision
                         FROM grants g JOIN resources r ON r.id = g.resource_id",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
                Ok((resources, grants))
            })
            .await?;
        let this = Self {
            database,
            limits,
            all: RwLock::new(BTreeMap::new()),
        };
        for row in &resources {
            let resource = this.resource_of(row)?;
            this.all
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(resource.name.clone(), Arc::new(resource));
        }
        for row in &grants {
            let name: &str = get(row, 0)?;
            if let Some(resource) = this.get(name) {
                let (group, granted) = granted_of(row, 1)?;
                resource.install(group, granted);
            }
        }
        Ok(this)
    }

    fn resource_of(&self, row: &Row) -> Result<Resource, DbError> {
        let name: String = get(row, 1)?;
        let connection = sealed::open(get(row, 5)?)
            .ok_or_else(|| DbError::unexpected("a Resource's stored connection cannot be read"))?;
        Ok(Resource {
            id: get(row, 0)?,
            pools: connection.into_pools(&name, &self.limits),
            name,
            description: get(row, 2)?,
            owner: get(row, 3)?,
            contact: get(row, 4)?,
            created_by: get(row, 6)?,
            created_at: get(row, 7)?,
            grants: RwLock::new(BTreeMap::new()),
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
        let stored = sealed::seal(&new.connection)
            .ok_or_else(|| DbError::unexpected("a Resource's connection cannot be stored"))?;
        let inserted = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "INSERT INTO resources (name, kind, description, owner, contact,
                                                    connection, created_by)
                             VALUES ($1, 'postgres', $2, $3, $4, $5, $6)
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
                        ],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        let row = match inserted {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(Registered::NameTaken),
            Err(error) => {
                // The insert may have committed without its answer
                // arriving: read again, so memory follows the database.
                self.refresh(&new.name).await;
                return Err(error);
            }
        };
        let resource = Arc::new(Resource {
            id: get(&row, 0)?,
            created_at: get(&row, 1)?,
            pools: new.connection.into_pools(&new.name, &self.limits),
            name: new.name,
            description: new.description,
            owner: new.owner,
            contact: new.contact,
            created_by: by.to_owned(),
            grants: RwLock::new(BTreeMap::new()),
        });
        self.all
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(resource.name.clone())
            .or_insert_with(|| Arc::clone(&resource));
        Ok(Registered::Registered(resource))
    }

    /// Sets `group`'s Grant on `resource` to `level`, in the database and
    /// then in memory, before this returns.
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
                client
                    .query_one(
                        "INSERT INTO grants (resource_id, group_name, access, revision)
                         VALUES ($1, $2, $3, nextval('grant_revisions'))
                         ON CONFLICT (resource_id, group_name) DO UPDATE
                         SET access = EXCLUDED.access, revision = nextval('grant_revisions')
                         RETURNING revision",
                        &[&found.id, &group, &access],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        match written {
            Ok(row) => {
                let revision = get(&row, 0)?;
                found.install(group, Granted { level, revision });
                Ok(GrantWritten::Written)
            }
            Err(error) => {
                self.refresh_grants(&found).await;
                Err(error)
            }
        }
    }

    /// Reads the Resource `name` and its Grants into memory, if it is in
    /// the database and not in memory.
    async fn refresh(&self, name: &str) {
        let read = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        &format!("SELECT {RESOURCE_COLUMNS} FROM resources r WHERE r.name = $1"),
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        match read.and_then(|row| row.as_ref().map(|row| self.resource_of(row)).transpose()) {
            Ok(Some(resource)) => {
                let resource = Arc::new(resource);
                self.all
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(resource.name.clone())
                    .or_insert_with(|| Arc::clone(&resource));
            }
            Ok(None) => {}
            Err(error) => unknown("resource-unknown", name, &error),
        }
    }

    /// Reads `resource`'s Grants into memory.
    async fn refresh_grants(&self, resource: &Resource) {
        let read = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT group_name, access, revision FROM grants WHERE resource_id = $1",
                        &[&resource.id],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        let installed = read.and_then(|rows| {
            for row in &rows {
                let (group, granted) = granted_of(row, 0)?;
                resource.install(group, granted);
            }
            Ok(())
        });
        if let Err(error) = installed {
            unknown("grants-unknown", &resource.name, &error);
        }
    }
}

fn unknown(event: &str, resource: &str, error: &DbError) {
    log::record(
        event,
        serde_json::json!({ "resource": resource, "error": error.to_string() }),
    );
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
        assert!(sealed::open(b"not what was stored").is_none());
        // What was stored before write logins existed still opens.
        let older = br#"{"host":"h","port":1,"database":"d","read":{"role":"r","password":"p"}}"#;
        let opened = sealed::open(older).unwrap();
        assert!(opened.write.is_none());
    }
}
