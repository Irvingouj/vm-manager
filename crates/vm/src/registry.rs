//! The Application registry: Applications, their Contributors, their
//! Versions, and which Version serves, kept in the platform database.
//!
//! Interface: [`Registry::load`] once at start; then the API's calls
//! ([`Registry::create_application`], [`Registry::send_version`], and the
//! reads), and Ingress's [`Registry::serving`], which answers from memory.
//!
//! Which Version serves is held in memory as well as in the database. One
//! program serves an installation (ADR 0015), so the memory is the
//! database's copy and is only ever changed after the database: a Version's
//! transaction commits, then memory is changed, then the API answers `201`.
//! So once the `201` has been received every new request finds the new
//! Version, without a round trip to Postgres per request. A request
//! already running holds an `Arc` of the Version it started on and finishes
//! on it. Memory only ever moves to a higher Version number, so two Versions
//! sent at once leave the newest serving whatever order their transactions
//! report back in.
//!
//! Sending a Version (blackbox/CONTRACT.md, "What happens when a Version is
//! sent"): its migrations must continue the previous Version's (else
//! diverged, before anything is changed); then the Application's own
//! database is made ready and the pending migrations applied
//! (crate::own_db); only then is the Version recorded, in a transaction
//! that checks again, under the Application's row lock, that it continues
//! the Version recorded last.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};

use authz::AccessLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::FromSql;
use tokio_postgres::{Client, Row};

use crate::log;
use crate::own_db::{Migration, OwnDatabases, Problem, Recorded, continues};
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};

/// Names that are the platform's own and can never be an Application's.
pub const RESERVED_NAMES: [&str; 6] = ["api", "auth", "dashboard", "git", "admin", "www"];

/// Why a name cannot be an Application's.
#[derive(Debug, PartialEq, Eq)]
pub enum NameProblem {
    /// Not a lower-case DNS label of 1 to 63 characters, or `xn--...`.
    Invalid,
    /// One of [`RESERVED_NAMES`].
    Reserved,
}

/// Whether `label` is a lower-case DNS label: 1 to 63 lower-case letters,
/// digits and hyphens, not starting or ending with a hyphen.
pub fn is_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An Application's name is its subdomain (ADR 0011): a lower-case DNS
/// label, not punycode (`xn--`, which could decode to a look-alike), and
/// not one of the platform's own.
pub fn check_name(name: &str) -> Result<(), NameProblem> {
    if !is_dns_label(name) || name.starts_with("xn--") {
        Err(NameProblem::Invalid)
    } else if RESERVED_NAMES.contains(&name) {
        Err(NameProblem::Reserved)
    } else {
        Ok(())
    }
}

/// The Resources a Version declares, with the Access level of each: what
/// the Authorization decision reads.
pub type Declared = BTreeMap<String, AccessLevel>;

/// The Version that serves an Application: what the Request runner runs.
/// Shared, never copied, between the registry and the requests running it.
#[derive(Debug)]
pub struct Serving {
    pub application: Arc<str>,
    pub version: u64,
    /// One classic script that registers
    /// `globalThis.default = { fetch(request, ctx) }`.
    pub bundle: Arc<str>,
    /// Its Declared access.
    pub declared: Arc<Declared>,
}

/// An Application as the registry reports it.
#[derive(Debug)]
pub struct ApplicationRecord {
    pub name: String,
    pub description: Option<String>,
    /// User ids, in the order they became Contributors.
    pub contributors: Vec<String>,
    pub serving_version: Option<u64>,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// A Version as the registry reports it; the bundle itself is not in it.
#[derive(Debug)]
pub struct VersionRecord {
    pub application: String,
    pub number: u64,
    pub declared_access: Value,
    pub git_commit: String,
    pub bundle_sha256: String,
    /// `[ { name, sha256 } ]`, in order.
    pub migrations: Value,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// What a Version is sent with.
#[derive(Debug)]
pub struct NewVersion {
    pub bundle: String,
    pub declared_access: DeclaredAccess,
    pub git_commit: String,
    /// The Application's whole history of migrations, in order.
    pub migrations: Vec<Migration>,
}

/// The Resources a Version declares it uses, and how. Read by the API,
/// which refuses a Resource named twice.
#[derive(Debug, Default, Serialize)]
pub struct DeclaredAccess {
    pub resources: BTreeMap<String, Resource>,
}

#[derive(Debug, Serialize)]
pub struct Resource {
    pub kind: ResourceKind,
    pub access: Access,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Postgres,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Structure,
    Read,
    Write,
}

impl From<Access> for AccessLevel {
    fn from(access: Access) -> Self {
        match access {
            Access::Structure => Self::Structure,
            Access::Read => Self::Read,
            Access::Write => Self::Write,
        }
    }
}

impl DeclaredAccess {
    /// Each declared Resource's Access level.
    fn levels(&self) -> Declared {
        self.resources
            .iter()
            .map(|(name, resource)| (name.clone(), resource.access.into()))
            .collect()
    }
}

/// The Declared access as stored (`{ "resources": { name: { kind, access }
/// } }`), read back. A Resource whose entry cannot be read is left out: it is
/// then not declared, and so refused.
fn declared_of(stored: &Value) -> Declared {
    let mut declared = Declared::new();
    if let Some(resources) = stored.get("resources").and_then(Value::as_object) {
        for (name, resource) in resources {
            let access = resource.get("access").and_then(Value::as_str);
            if let Some(access) = access.and_then(crate::resources::level_of) {
                declared.insert(name.clone(), access);
            }
        }
    }
    declared
}

/// What creating an Application came to.
#[derive(Debug)]
pub enum Created {
    Created(ApplicationRecord),
    NameTaken,
}

/// What sending a Version came to.
#[derive(Debug)]
pub enum Sent {
    Sent(VersionRecord),
    NoSuchApplication,
    NotAContributor,
    /// Its migrations do not continue the previous Version's.
    Diverged,
    /// A migration failed in the database; nothing was applied or recorded.
    MigrationFailed {
        migration: String,
        code: String,
        message: String,
    },
    /// The own database could not be made ready or reached: a fixed
    /// description of the step, and Postgres's SQLSTATE if it gave one.
    OwnDatabaseUnavailable {
        step: &'static str,
        code: Option<String>,
    },
}

pub struct Registry {
    database: Arc<PlatformDatabase>,
    own: Arc<OwnDatabases>,
    /// The serving Version of every Application that has one, by name.
    /// Held only for a lookup or an insert, never across an await.
    serving: RwLock<HashMap<Arc<str>, Arc<Serving>>>,
}

impl Registry {
    /// Reads every serving Version from the platform database.
    pub async fn load(
        database: Arc<PlatformDatabase>,
        own: Arc<OwnDatabases>,
    ) -> Result<Self, DbError> {
        let rows = database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.name, v.number, v.bundle, v.declared_access
                         FROM applications a
                         JOIN versions v ON v.application_id = a.id AND v.number = a.serving_version",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut serving = HashMap::with_capacity(rows.len());
        for row in &rows {
            let version = serving_of(row)?;
            serving.insert(Arc::clone(&version.application), Arc::new(version));
        }
        Ok(Self {
            database,
            own,
            serving: RwLock::new(serving),
        })
    }

    /// The Version serving `name` now, if it has one. From memory.
    pub fn serving(&self, name: &str) -> Option<Arc<Serving>> {
        self.serving
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(Arc::clone)
    }

    /// The names of the Applications that have a serving Version, sorted.
    pub fn serving_names(&self) -> Vec<Arc<str>> {
        let mut names: Vec<Arc<str>> = self
            .serving
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .map(Arc::clone)
            .collect();
        names.sort_unstable();
        names
    }

    /// Makes `version` the serving one in memory, unless memory already
    /// has a newer one.
    fn install(&self, version: Serving) {
        let mut serving = self.serving.write().unwrap_or_else(PoisonError::into_inner);
        let newer = serving
            .get(&*version.application)
            .is_none_or(|current| current.version < version.version);
        if newer {
            serving.insert(Arc::clone(&version.application), Arc::new(version));
        }
    }

    /// Creates the Application `name`, with `creator` as its first
    /// Contributor. The name must have passed [`check_name`].
    pub async fn create_application(
        &self,
        name: &str,
        description: Option<&str>,
        creator: &str,
    ) -> Result<Created, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "WITH created AS (
                                INSERT INTO applications (name, description, created_by)
                                VALUES ($1, $2, $3)
                                ON CONFLICT (name) DO NOTHING
                                RETURNING id, name, description, created_by, created_at
                            ), contributor AS (
                                INSERT INTO contributors (application_id, user_id)
                                SELECT id, $3 FROM created
                            )
                            SELECT name, description, created_by, ",
                            rfc3339!("created_at"),
                            " FROM created"
                        ),
                        &[&name, &description, &creator],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else {
            return Ok(Created::NameTaken);
        };
        Ok(Created::Created(ApplicationRecord {
            name: get(&row, 0)?,
            description: get(&row, 1)?,
            contributors: vec![get(&row, 2)?],
            serving_version: None,
            created_by: get(&row, 2)?,
            created_at: get(&row, 3)?,
        }))
    }

    /// The Application `name`, if there is one.
    pub async fn application(&self, name: &str) -> Result<Option<ApplicationRecord>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "SELECT a.name, a.description,
                                coalesce((SELECT array_agg(c.user_id ORDER BY c.id)
                                          FROM contributors c WHERE c.application_id = a.id),
                                         '{}'::text[]),
                                a.serving_version, a.created_by, ",
                            rfc3339!("a.created_at"),
                            " FROM applications a WHERE a.name = $1"
                        ),
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(ApplicationRecord {
            name: get(&row, 0)?,
            description: get(&row, 1)?,
            contributors: get(&row, 2)?,
            serving_version: get::<Option<i32>>(&row, 3)?.map(number_of).transpose()?,
            created_by: get(&row, 4)?,
            created_at: get(&row, 5)?,
        }))
    }

    /// Whether `user` is a Contributor of `name`; `None` when there is no
    /// such Application.
    pub async fn is_contributor(&self, name: &str, user: &str) -> Result<Option<bool>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT EXISTS (SELECT 1 FROM contributors c
                                        WHERE c.application_id = a.id AND c.user_id = $2)
                         FROM applications a WHERE a.name = $1",
                        &[&name, &user],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        row.map(|row| get(&row, 0)).transpose()
    }

    /// Sends a Version of `name` as `sender`, who must be a Contributor. Its
    /// migrations must continue the previous Version's; the own database is
    /// made ready and the pending ones applied; then it gets the next number
    /// and becomes the serving Version, in the database and then in memory,
    /// before this returns.
    pub async fn send_version(
        &self,
        name: &str,
        version: NewVersion,
        sender: &str,
    ) -> Result<Sent, DbError> {
        let found = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT a.id, EXISTS (SELECT 1 FROM contributors c
                                              WHERE c.application_id = a.id AND c.user_id = $2),
                                (SELECT v.migrations FROM versions v WHERE v.application_id = a.id
                                 ORDER BY v.number DESC LIMIT 1)
                         FROM applications a WHERE a.name = $1",
                        &[&name, &sender],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(found) = found else {
            return Ok(Sent::NoSuchApplication);
        };
        let id: i64 = get(&found, 0)?;
        if !get::<bool>(&found, 1)? {
            return Ok(Sent::NotAContributor);
        }
        let previous = recorded_of(get::<Option<Value>>(&found, 2)?.as_ref());
        if !continues(&previous, &version.migrations) {
            return Ok(Sent::Diverged);
        }
        match self
            .own
            .prepare(id, name, sender, &version.migrations)
            .await
        {
            Ok(()) => {}
            Err(Problem::Diverged) => return Ok(Sent::Diverged),
            Err(Problem::Failed {
                migration,
                code,
                message,
            }) => {
                return Ok(Sent::MigrationFailed {
                    migration,
                    code,
                    message,
                });
            }
            Err(Problem::Unavailable { step, code }) => {
                return Ok(Sent::OwnDatabaseUnavailable { step, code });
            }
        }
        self.record(name, id, version, sender).await
    }

    /// Records `version` as the next of the Application `id`, unless a
    /// Version recorded meanwhile has migrations it does not continue.
    async fn record(
        &self,
        name: &str,
        id: i64,
        version: NewVersion,
        sender: &str,
    ) -> Result<Sent, DbError> {
        let digest = hex(&Sha256::digest(version.bundle.as_bytes()));
        let declared = Arc::new(version.declared_access.levels());
        let declared_access = serde_json::to_value(&version.declared_access)
            .map_err(|_| DbError::unexpected("the Declared access cannot be written as JSON"))?;
        let migrations = Value::Array(
            version
                .migrations
                .iter()
                .map(|m| serde_json::json!({ "name": m.name, "sha256": m.sha256 }))
                .collect(),
        );
        // The one copy of the bundle: into the shared form every request
        // running this Version will use.
        let bundle: Arc<str> = Arc::from(version.bundle);
        let git_commit = version.git_commit;
        let list = version.migrations;
        let sent = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_one(
                        "SELECT (SELECT v.migrations FROM versions v WHERE v.application_id = a.id
                                 ORDER BY v.number DESC LIMIT 1)
                         FROM applications a WHERE a.id = $1
                         FOR UPDATE",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                let previous = recorded_of(get::<Option<Value>>(&found, 0)?.as_ref());
                if !continues(&previous, &list) {
                    return Ok(Sent::Diverged);
                }
                let row = transaction
                    .query_one(
                        concat!(
                            "WITH sent AS (
                                INSERT INTO versions (application_id, number, bundle, bundle_sha256,
                                                      declared_access, git_commit, created_by,
                                                      migrations)
                                VALUES ($1,
                                        (SELECT coalesce(max(number), 0) + 1 FROM versions
                                         WHERE application_id = $1),
                                        $2, $3, $4, $5, $6, $7)
                                RETURNING number, declared_access, git_commit, bundle_sha256,
                                          created_by, created_at, migrations
                            ), serving AS (
                                UPDATE applications SET serving_version = (SELECT number FROM sent)
                                WHERE id = $1
                            )
                            SELECT number, declared_access, git_commit, bundle_sha256, migrations,
                                   created_by, ",
                            rfc3339!("created_at"),
                            " FROM sent"
                        ),
                        &[
                            &id,
                            &&*bundle,
                            &digest,
                            &declared_access,
                            &git_commit,
                            &sender,
                            &migrations,
                        ],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Sent::Sent(VersionRecord {
                    application: name.to_owned(),
                    number: number_of(get(&row, 0)?)?,
                    declared_access: get(&row, 1)?,
                    git_commit: get(&row, 2)?,
                    bundle_sha256: get(&row, 3)?,
                    migrations: get(&row, 4)?,
                    created_by: get(&row, 5)?,
                    created_at: get(&row, 6)?,
                }))
            })
            .await;
        match sent {
            Ok(Sent::Sent(record)) => {
                self.install(Serving {
                    application: Arc::from(name),
                    version: record.number,
                    bundle,
                    declared,
                });
                Ok(Sent::Sent(record))
            }
            Ok(other) => Ok(other),
            Err(error) => {
                // The commit may have happened without its answer arriving:
                // read again what serves, so memory follows the database.
                self.refresh(name).await;
                Err(error)
            }
        }
    }

    /// Reads `name`'s serving Version from the database into memory.
    async fn refresh(&self, name: &str) {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT a.name, v.number, v.bundle, v.declared_access
                         FROM applications a
                         JOIN versions v ON v.application_id = a.id AND v.number = a.serving_version
                         WHERE a.name = $1",
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        match row.and_then(|row| row.as_ref().map(serving_of).transpose()) {
            Ok(Some(version)) => self.install(version),
            Ok(None) => {}
            Err(error) => log::record(
                "serving-version-unknown",
                serde_json::json!({
                    "application": name,
                    "error": error.to_string(),
                }),
            ),
        }
    }

    /// The Versions of `name`, oldest first; `None` when there is no such
    /// Application.
    pub async fn versions(&self, name: &str) -> Result<Option<Vec<VersionRecord>>, DbError> {
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        concat!(
                            "SELECT a.name, v.number, v.declared_access, v.git_commit,
                                    v.bundle_sha256, v.created_by, ",
                            rfc3339!("v.created_at"),
                            ", v.migrations FROM applications a
                              LEFT JOIN versions v ON v.application_id = a.id
                              WHERE a.name = $1
                              ORDER BY v.number"
                        ),
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut versions = Vec::with_capacity(rows.len());
        for row in &rows {
            if get::<Option<i32>>(row, 1)?.is_some() {
                versions.push(version_of(row)?);
            }
        }
        Ok(Some(versions))
    }

    /// The Version `number` of `name`, if there is one.
    pub async fn version(&self, name: &str, number: u64) -> Result<Option<VersionRecord>, DbError> {
        let Ok(number) = i32::try_from(number) else {
            return Ok(None);
        };
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "SELECT a.name, v.number, v.declared_access, v.git_commit,
                                    v.bundle_sha256, v.created_by, ",
                            rfc3339!("v.created_at"),
                            ", v.migrations FROM applications a
                              JOIN versions v ON v.application_id = a.id
                              WHERE a.name = $1 AND v.number = $2"
                        ),
                        &[&name, &number],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        row.as_ref().map(version_of).transpose()
    }
}

fn version_of(row: &Row) -> Result<VersionRecord, DbError> {
    Ok(VersionRecord {
        application: get(row, 0)?,
        number: number_of(get(row, 1)?)?,
        declared_access: get(row, 2)?,
        git_commit: get(row, 3)?,
        bundle_sha256: get(row, 4)?,
        created_by: get(row, 5)?,
        created_at: get(row, 6)?,
        migrations: get(row, 7)?,
    })
}

/// A Version's migrations as stored (`[ { name, sha256 } ]`), read back;
/// none for no Version. An entry that cannot be read reads as an empty name
/// and digest, which no migration has: every list diverges from it rather
/// than skip it.
fn recorded_of(stored: Option<&Value>) -> Vec<Recorded> {
    let Some(entries) = stored.and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| Recorded {
            name: text_of(entry, "name"),
            sha256: text_of(entry, "sha256"),
        })
        .collect()
}

fn serving_of(row: &Row) -> Result<Serving, DbError> {
    Ok(Serving {
        application: Arc::from(get::<&str>(row, 0)?),
        version: number_of(get(row, 1)?)?,
        bundle: Arc::from(get::<&str>(row, 2)?),
        declared: Arc::new(declared_of(&get::<Value>(row, 3)?)),
    })
}

fn get<'a, T: FromSql<'a>>(row: &'a Row, index: usize) -> Result<T, DbError> {
    row.try_get(index).map_err(DbError::from)
}

/// A Version number as stored (`integer`, above 0) to what the API shows.
fn number_of(stored: i32) -> Result<u64, DbError> {
    u64::try_from(stored).map_err(|_| DbError::unexpected("a Version number below 1"))
}

fn text_of(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `bytes` in lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail.
        write!(text, "{byte:02x}").ok();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_lower_case_dns_labels_not_punycode_not_the_platforms_own() {
        for name in ["a", "0", "my-app", "x--y", "xn-a", "apis", "my-api", "www1"] {
            assert_eq!(check_name(name), Ok(()), "{name}");
        }
        let long = "a".repeat(64);
        for name in [
            "",
            "A",
            "-a",
            "a-",
            "a.b",
            "a_b",
            "zoë",
            "xn--a",
            "xn--",
            long.as_str(),
        ] {
            assert_eq!(check_name(name), Err(NameProblem::Invalid), "{name}");
        }
        for name in RESERVED_NAMES {
            assert_eq!(check_name(name), Err(NameProblem::Reserved), "{name}");
        }
    }

    #[test]
    fn hex_is_lower_case() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
