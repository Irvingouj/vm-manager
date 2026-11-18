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
//! on it. Every change of what serves (a Version sent, a go-back) adds one
//! to the Application's `serving_generation` in the same statement, under
//! the row's lock; memory only ever moves to a higher generation, so two
//! changes at once leave memory as the database's last one whatever order
//! they report back in.
//!
//! Who may change an Application and who may open it (blackbox/CONTRACT.md,
//! "Contributors and sharing") is one table, `application_entries`: an
//! entry of the Contributors or of the sharing names a user id, a Group or
//! (for the sharing) everyone. A change of a list ([`Registry::change`]) is
//! one transaction under the Application's row lock: the emails resolved,
//! the removals and additions made, then the last-Contributor rule and the
//! list's bound checked, and the whole rolled back if either fails. A
//! list holds at most [`MAX_CONTRIBUTORS`] or [`MAX_SHARED_WITH`] entries;
//! a change past that is refused and changes nothing. The API asks the
//! database whether a User is a Contributor (`is_contributor!`).
//!
//! Who may open an Application is decided at each request from memory,
//! like Grants (crate::resources) and the serving Version: [`Lists`] holds
//! each Application's two lists, loaded at start and changed only after
//! the platform database has committed and before the API answers. Every
//! change that changes a list adds one to the Application's
//! `lists_generation` under the row's lock, and memory keeps the higher,
//! so changes at once leave memory as the database whatever order they
//! report back in. A creation or a change whose answer is lost is read
//! back in one statement (the generation and the entries one snapshot);
//! if that fails too, memory forgets the Application's lists
//! ([`Held::Unknown`]) and Ingress reads them back at its next request,
//! answering `503` while it cannot: nobody is let in on a guess. Forgetting
//! only ever replaces what memory holds of an Application, so memory holds
//! at most one entry per Application.
//!
//! Going back ([`Registry::go_back`]) is one `UPDATE` that changes the
//! serving Version only where it is still the one the caller saw: two
//! go-backs from the same Version at once are serialised by the row's lock,
//! and the second finds it changed and changes nothing.
//!
//! Sending a Version (blackbox/CONTRACT.md, "What happens when a Version is
//! sent"): its migrations must continue the previous Version's (else
//! diverged, before anything is changed); then the Application's own
//! database is made ready and the pending migrations applied
//! (crate::own_db); only then is the Version recorded, in a transaction
//! that checks again, under the Application's row lock, that it continues
//! the Version recorded last.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use authz::AccessLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::FromSql;
use tokio_postgres::{Client, GenericClient, Row};

use crate::log;
use crate::own_db::{Migration, NotToTheEnd, OwnDatabases, Problem, Recorded, continues};
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::user::User;

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
    /// The Application's `serving_generation` when this Version was made
    /// the serving one: memory keeps the highest.
    pub generation: i64,
}

/// The most Contributors an Application has.
pub const MAX_CONTRIBUTORS: i64 = 100;

/// The most entries of whom an Application is shared with.
pub const MAX_SHARED_WITH: i64 = 1000;

/// SQL: whether the entry `e` (of `application_entries`) names the person
/// whose id is `$user` and whose Groups are `$groups` (a `text[]`): them,
/// one of their Groups, or everyone. Compared exactly, byte for byte.
macro_rules! names_person {
    ($user:literal, $groups:literal) => {
        concat!(
            "(e.kind = 'everyone' OR (e.kind = 'user' AND e.name = ",
            $user,
            ") OR (e.kind = 'group' AND e.name = ANY (",
            $groups,
            ")))"
        )
    };
}

/// SQL: whether that person can open the Application whose id is `$app`:
/// an entry of either list names them (an entry of the Contributors never
/// is `everyone`). One probe of the index `(application_id, kind, name,
/// list)` for each kind.
macro_rules! can_open {
    ($app:literal, $user:literal, $groups:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM application_entries e WHERE e.application_id = ",
            $app,
            " AND ",
            crate::registry::names_person!($user, $groups),
            ")"
        )
    };
}

/// SQL: whether that person is a Contributor of the Application `$app`.
macro_rules! is_contributor {
    ($app:literal, $user:literal, $groups:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM application_entries e WHERE e.application_id = ",
            $app,
            " AND e.list = 'contributors' AND ",
            crate::registry::names_person!($user, $groups),
            ")"
        )
    };
}

pub(crate) use names_person;

/// An Application's two lists as memory holds them, for deciding who may
/// open it: whom they name, both lists together, since either opens. At
/// most 1,100 names (100 Contributors and 1,000 shared with) of at most
/// 256 bytes each, so a few hundred kilobytes per Application at worst.
/// Deciding costs a lookup for the User's id and one for each of their
/// Groups, whatever the length of the lists.
#[derive(Debug, Default)]
pub struct Lists {
    /// The Application's `lists_generation` this was read at.
    generation: i64,
    users: HashSet<String>,
    groups: HashSet<String>,
    everyone: bool,
}

impl Lists {
    /// The lists of `record`, as of `generation`.
    fn of(record: &ApplicationRecord) -> Self {
        let mut lists = Self {
            generation: record.generation,
            ..Self::default()
        };
        for entry in record.contributors.iter().chain(&record.shared_with) {
            lists.add(&entry.named);
        }
        lists
    }

    fn add(&mut self, named: &Named) {
        match named {
            // Copied: memory keeps its own lists; once per entry each time
            // a list changes (at most 1,100), never at a request.
            Named::User(id) => {
                self.users.insert(id.clone());
            }
            Named::Group(group) => {
                self.groups.insert(group.clone());
            }
            Named::Everyone => self.everyone = true,
        }
    }

    /// Whether they name `user`: their id, one of their Groups, or
    /// everyone.
    fn open_to(&self, user: &User) -> bool {
        self.everyone
            || self.users.contains(&user.id)
            || user.groups.iter().any(|g| self.groups.contains(g))
    }
}

/// What memory holds of an Application's lists.
#[derive(Debug)]
pub enum Held {
    Known(Arc<Lists>),
    /// Forgotten: a change's answer was lost and could not be read back.
    /// `seen` is the highest generation reported since; `epoch` tells a
    /// read-back begun before this forgetting from one begun after.
    Unknown {
        seen: i64,
        epoch: u64,
    },
}

/// Whether a User may open an Application, as memory has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opens {
    Yes,
    /// It is not shared with them, nor are they a Contributor.
    No,
    /// Memory does not know its lists now: they must be read back.
    NotKnown,
    /// Memory holds no Application of that name.
    NoSuchApplication,
}

/// What memory held when a read-back began.
#[derive(Clone, Copy)]
enum Began {
    Nothing,
    Known,
    Unknown(u64),
}

/// Which of an Application's two lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    /// Who may change and deploy it.
    Contributors,
    /// Who may open it.
    SharedWith,
}

impl List {
    /// The list's name in the platform database.
    fn stored(self) -> &'static str {
        match self {
            Self::Contributors => "contributors",
            Self::SharedWith => "shared_with",
        }
    }

    /// The most entries it holds.
    fn bound(self) -> i64 {
        match self {
            Self::Contributors => MAX_CONTRIBUTORS,
            Self::SharedWith => MAX_SHARED_WITH,
        }
    }
}

/// What an entry of a list names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    /// A person, by their user id.
    User(String),
    /// A Group.
    Group(String),
    /// Every User who can sign in (shared with only).
    Everyone,
}

impl Named {
    /// The kind and the name as stored.
    fn stored(&self) -> (&'static str, &str) {
        match self {
            Self::User(id) => ("user", id),
            Self::Group(group) => ("group", group),
            Self::Everyone => ("everyone", ""),
        }
    }

    fn of_stored(kind: &str, name: String) -> Result<Self, DbError> {
        match kind {
            "user" => Ok(Self::User(name)),
            "group" => Ok(Self::Group(name)),
            "everyone" => Ok(Self::Everyone),
            _ => Err(DbError::unexpected("an entry of a list of an unknown kind")),
        }
    }

    /// Whether it names `user` (them, one of their Groups, or everyone).
    pub fn names(&self, user: &User) -> bool {
        match self {
            Self::User(id) => *id == user.id,
            Self::Group(group) => user.groups.iter().any(|g| g == group),
            Self::Everyone => true,
        }
    }
}

/// An entry of one of an Application's lists, as kept.
#[derive(Debug)]
pub struct Entry {
    pub named: Named,
    /// A person's name and email as last read at their sign-in; `None` for
    /// someone who never signed in here, and for a Group or everyone.
    pub name: Option<String>,
    pub email: Option<String>,
    pub added_by: String,
    /// RFC 3339, UTC.
    pub added_at: String,
}

/// An Application as the registry reports it.
#[derive(Debug)]
pub struct ApplicationRecord {
    /// The generation of its lists (`lists_generation`).
    pub generation: i64,
    pub name: String,
    pub description: Option<String>,
    /// In the order they were added.
    pub contributors: Vec<Entry>,
    /// In the order they were added.
    pub shared_with: Vec<Entry>,
    pub serving_version: Option<u64>,
    /// The highest-numbered Version; `None` when there is none.
    pub latest_version: Option<u64>,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

impl ApplicationRecord {
    /// Whether `user` is one of its Contributors (by their id or a Group).
    pub fn is_contributor(&self, user: &User) -> bool {
        self.contributors.iter().any(|e| e.named.names(user))
    }

    /// Whether `user` can open it: a Contributor, or shared with them.
    pub fn can_open(&self, user: &User) -> bool {
        self.is_contributor(user) || self.shared_with.iter().any(|e| e.named.names(user))
    }
}

/// An Application as a list of them shows it.
#[derive(Debug)]
pub struct ListedApplication {
    pub name: String,
    pub description: Option<String>,
    pub serving_version: Option<u64>,
    /// Whether the User the list is for is a Contributor of it.
    pub contributor: bool,
}

/// Where a User stands with an Application, by its two lists.
#[derive(Clone, Copy, Debug)]
pub struct Standing {
    pub contributor: bool,
    pub can_open: bool,
}

/// What is asked to be added or removed: an entry, or a person by the
/// email the platform last read of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Entry(Named),
    Email(String),
}

/// A change of one list: made whole or not at all.
#[derive(Debug)]
pub struct Change {
    pub list: List,
    pub add: Vec<Asked>,
    pub remove: Vec<Asked>,
}

/// What a change of a list came to.
#[derive(Debug)]
pub enum Changed {
    /// Made: the Application as it is after, and what was added and
    /// removed (each as kept), in order. Both empty when nothing changed.
    Changed {
        application: Box<ApplicationRecord>,
        added: Vec<Entry>,
        removed: Vec<Entry>,
    },
    NoSuchApplication,
    NotAContributor,
    /// Nobody who signed in here has this email.
    UnknownEmail(String),
    /// More than one person has it.
    AmbiguousEmail(String),
    /// The same person in `add` and in `remove`, once the emails are
    /// resolved: by their id on one side and by email on the other.
    SameInBoth(String),
    /// The change would leave no Contributor: the entries it removed.
    LastContributor(Vec<Entry>),
    /// The list would pass its bound: the entries it added.
    TooMany(Vec<Entry>),
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
    /// `{ "<resource>": "sha256:<hex>" }`: the fingerprint of each shared
    /// structure it was built against; `{}` for a Version sent without.
    pub structures: Value,
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
    /// The fingerprint of each shared structure it was built against, by
    /// Resource (each one it declares, as the API checked).
    pub structures: BTreeMap<String, String>,
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

/// What going back came to.
#[derive(Debug)]
pub enum WentBack {
    /// Version `to` serves now, in place of `from`.
    WentBack {
        /// The migrations the latest Version has and `to` has not, in order.
        migrations_not_undone: Vec<String>,
    },
    /// The serving Version is not `from`: this one serves (or none).
    ServingChanged(Option<u64>),
    /// `from` serves and there is no Version before it.
    NoPreviousVersion,
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
        /// The Version that was serving when it was sent, if one was.
        serving: Option<u64>,
    },
    /// A migration broke a rule of the platform's; nothing was applied or
    /// recorded.
    MigrationRefused {
        migration: String,
        rule: &'static str,
        message: String,
        /// The Version that was serving when it was sent, if one was.
        serving: Option<u64>,
    },
    /// Too many Versions are being sent at once in the program: nothing of
    /// this one was begun.
    Busy,
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
    /// Every Application's lists, by name: one entry per Application.
    /// Held only for a lookup or an insert, never across an await.
    lists: RwLock<HashMap<Arc<str>, Held>>,
    /// The number of forgettings so far, each one's `epoch`.
    forgotten: AtomicU64,
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
                        "SELECT a.name, v.number, v.bundle, v.declared_access, a.serving_generation
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
        // Every Application's lists, in one statement: one snapshot.
        let rows = database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.name, a.lists_generation, e.kind, e.name
                         FROM applications a
                         LEFT JOIN application_entries e ON e.application_id = a.id",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut all: HashMap<Arc<str>, Lists> = HashMap::new();
        for row in &rows {
            let name: &str = get(row, 0)?;
            let lists = match all.get_mut(name) {
                Some(lists) => lists,
                None => all.entry(Arc::from(name)).or_default(),
            };
            lists.generation = get(row, 1)?;
            if let Some(kind) = get::<Option<&str>>(row, 2)? {
                lists.add(&Named::of_stored(kind, get(row, 3)?)?);
            }
        }
        let lists = all
            .into_iter()
            .map(|(name, lists)| (name, Held::Known(Arc::new(lists))))
            .collect();
        Ok(Self {
            database,
            own,
            serving: RwLock::new(serving),
            lists: RwLock::new(lists),
            forgotten: AtomicU64::new(0),
        })
    }

    /// Whether `user` may open `name`, from memory: no round trip, and the
    /// lock is held for the lookups alone.
    pub fn opens(&self, name: &str, user: &User) -> Opens {
        match self
            .lists
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            None => Opens::NoSuchApplication,
            Some(Held::Unknown { .. }) => Opens::NotKnown,
            Some(Held::Known(lists)) if lists.open_to(user) => Opens::Yes,
            Some(Held::Known(_)) => Opens::No,
        }
    }

    /// Reads `name`'s lists back from the platform database into memory,
    /// for a request that found them not known (or not there, for an
    /// Application that serves). `false` when there is no such
    /// Application.
    pub async fn read_back(&self, name: &str) -> Result<bool, DbError> {
        let began = match self
            .lists
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            None => Began::Nothing,
            Some(Held::Known(_)) => Began::Known,
            Some(Held::Unknown { epoch, .. }) => Began::Unknown(*epoch),
        };
        let Some(lists) = self.lists_of(name).await? else {
            return Ok(false);
        };
        self.install_read(name, lists, began);
        Ok(true)
    }

    /// `name`'s lists from the platform database, in one statement (the
    /// generation and the entries one snapshot); `None` for no such
    /// Application.
    async fn lists_of(&self, name: &str) -> Result<Option<Lists>, DbError> {
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.lists_generation, e.kind, e.name
                         FROM applications a
                         LEFT JOIN application_entries e ON e.application_id = a.id
                         WHERE a.name = $1",
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut lists: Option<Lists> = None;
        for row in &rows {
            let held = lists.get_or_insert_with(Lists::default);
            held.generation = get(row, 0)?;
            if let Some(kind) = get::<Option<&str>>(row, 1)? {
                held.add(&Named::of_stored(kind, get(row, 2)?)?);
            }
        }
        Ok(lists)
    }

    /// Puts what a creation or a change committed into memory: over lists
    /// of a lower generation; over forgotten lists it only raises `seen`,
    /// since a lost change may have gone further (a request reads them
    /// back).
    fn install_changed(&self, name: &str, lists: Lists) {
        let mut all = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            None => {
                all.insert(Arc::from(name), Held::Known(Arc::new(lists)));
            }
            Some(Held::Known(held)) => {
                if held.generation < lists.generation {
                    *held = Arc::new(lists);
                }
            }
            Some(Held::Unknown { seen, .. }) => {
                *seen = (*seen).max(lists.generation);
            }
        }
    }

    /// Puts what a request's read-back read into memory: over lists of a
    /// lower generation; over forgotten lists only when they were
    /// forgotten before the read began, and it is at least the highest
    /// generation reported since.
    fn install_read(&self, name: &str, lists: Lists, began: Began) {
        let mut all = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            None => {
                if matches!(began, Began::Nothing) {
                    all.insert(Arc::from(name), Held::Known(Arc::new(lists)));
                }
            }
            Some(Held::Known(held)) => {
                if held.generation < lists.generation {
                    *held = Arc::new(lists);
                }
            }
            Some(held @ Held::Unknown { .. }) => {
                if let (Held::Unknown { seen, epoch }, Began::Unknown(at)) = (&*held, began)
                    && *epoch == at
                    && *seen <= lists.generation
                {
                    *held = Held::Known(Arc::new(lists));
                }
            }
        }
    }

    /// After a creation or a change whose answer was lost: its lists read
    /// back; when that fails too, forgotten, if memory holds them (a name
    /// memory does not hold is left so: no marker is added).
    async fn after_lost(&self, name: &str) {
        match self.lists_of(name).await {
            Ok(Some(lists)) => self.install_changed(name, lists),
            Ok(None) => {}
            Err(error) => {
                log::record(
                    "lists-unknown",
                    serde_json::json!({ "application": name, "error": error.to_string() }),
                );
                let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
                let mut all = self.lists.write().unwrap_or_else(PoisonError::into_inner);
                if let Some(held) = all.get_mut(name) {
                    let seen = match held {
                        Held::Known(lists) => lists.generation,
                        Held::Unknown { seen, .. } => *seen,
                    };
                    *held = Held::Unknown { seen, epoch };
                }
            }
        }
    }

    /// The name the platform last read of the person `id` at a sign-in.
    pub async fn person_name(&self, id: &str) -> Result<Option<String>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt("SELECT name FROM users WHERE id = $1", &[&id])
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        match row {
            Some(row) => get(&row, 0),
            None => Ok(None),
        }
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
    /// holds a later change of what serves (a higher generation).
    fn install(&self, version: Serving) {
        let mut serving = self.serving.write().unwrap_or_else(PoisonError::into_inner);
        let newer = serving
            .get(&*version.application)
            .is_none_or(|current| current.generation < version.generation);
        if newer {
            serving.insert(Arc::clone(&version.application), Arc::new(version));
        }
    }

    /// Creates the Application `name`, with `creator` as its first
    /// Contributor (added by them, when it was created), shared with
    /// nobody. The name must have passed [`check_name`].
    pub async fn create_application(
        &self,
        name: &str,
        description: Option<&str>,
        creator: &str,
    ) -> Result<Created, DbError> {
        let created = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let made = client
                    .query_opt(
                        "WITH created AS (
                            INSERT INTO applications (name, description, created_by)
                            VALUES ($1, $2, $3)
                            ON CONFLICT (name) DO NOTHING
                            RETURNING id, created_at
                        )
                        INSERT INTO application_entries
                            (application_id, list, kind, name, added_by, added_at)
                        SELECT id, 'contributors', 'user', $3, $3, created_at FROM created
                        RETURNING application_id",
                        &[&name, &description, &creator],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(made) = made else { return Ok(None) };
                record_of(client, Key::Id(get(&made, 0)?)).await
            })
            .await;
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                // It may have been made without the answer arriving.
                self.after_lost(name).await;
                return Err(error);
            }
        };
        Ok(match created {
            Some(application) => {
                self.install_changed(name, Lists::of(&application));
                Created::Created(application)
            }
            None => Created::NameTaken,
        })
    }

    /// Forgets the Versions of `name` older than its newest `keep`, in one
    /// statement, never the serving one; answers how many were forgotten.
    /// For the local run (blackbox/CONTRACT.md, "The local loop: What a run
    /// does", step 6), whose Application would otherwise keep every bundle
    /// it was ever built with.
    ///
    /// Safe for what refers to a Version: only `applications.serving_version`
    /// does by a key, and the serving Version is kept; the newest is kept, so
    /// the next number (the greatest plus one) is never one used before; a
    /// log entry and an Operation log entry hold a Version's number as a
    /// number only, and read the same without its row; the own database
    /// records the migrations it applied by name and text, and a new Version
    /// is compared with the newest, which is kept. Going back to a forgotten
    /// Version is not possible, and the local run never goes back.
    pub async fn forget_versions(&self, name: &str, keep: u32) -> Result<u64, DbError> {
        let keep = i32::try_from(keep).unwrap_or(i32::MAX);
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "DELETE FROM versions v
                          USING applications a
                          WHERE a.name = $1 AND v.application_id = a.id
                            AND v.number IS DISTINCT FROM a.serving_version
                            AND v.number <= (SELECT max(number) FROM versions
                                              WHERE application_id = a.id) - $2",
                        &[&name, &keep],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await
    }

    /// The Application `name`, if there is one, with both its lists.
    pub async fn application(&self, name: &str) -> Result<Option<ApplicationRecord>, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| record_of(client, Key::Name(name)).await)
            .await
    }

    /// The Applications `user` is a Contributor of or can open, each once,
    /// by name (compared byte by byte, whatever the database's collation).
    pub async fn listed_for(&self, user: &User) -> Result<Vec<ListedApplication>, DbError> {
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        concat!(
                            "SELECT a.name, a.description, a.serving_version, ",
                            is_contributor!("a.id", "$1", "$2"),
                            " FROM applications a
                              WHERE a.id IN (SELECT e.application_id FROM application_entries e
                                             WHERE ",
                            names_person!("$1", "$2"),
                            ") ORDER BY a.name COLLATE \"C\""
                        ),
                        &[&user.id, &user.groups],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        rows.iter()
            .map(|row| {
                Ok(ListedApplication {
                    name: get(row, 0)?,
                    description: get(row, 1)?,
                    serving_version: get::<Option<i32>>(row, 2)?.map(number_of).transpose()?,
                    contributor: get(row, 3)?,
                })
            })
            .collect()
    }

    /// What `user` may do with `name`; `None` when there is no such
    /// Application. One query.
    pub async fn standing(&self, name: &str, user: &User) -> Result<Option<Standing>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "SELECT ",
                            is_contributor!("a.id", "$2", "$3"),
                            ", ",
                            can_open!("a.id", "$2", "$3"),
                            " FROM applications a WHERE a.name = $1"
                        ),
                        &[&name, &user.id, &user.groups],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        row.map(|row| {
            Ok(Standing {
                contributor: get(&row, 0)?,
                can_open: get(&row, 1)?,
            })
        })
        .transpose()
    }

    /// Changes one list of `name` as `by` asks, who must be a Contributor:
    /// whole or not at all, in one transaction that holds the
    /// Application's row lock from the start, so that two changes at once
    /// take turns and the bounds and the last Contributor are checked on
    /// the list as the change leaves it. Every entry asked by email is
    /// resolved first; then what is removed, then what is added; then the
    /// list is counted. Any refusal rolls everything back.
    pub async fn change(&self, name: &str, by: &User, change: Change) -> Result<Changed, DbError> {
        let list = change.list.stored();
        let bound = change.list.bound();
        let contributors = change.list == List::Contributors;
        let changed = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        concat!(
                            "SELECT a.id, ",
                            is_contributor!("a.id", "$2", "$3"),
                            " FROM applications a WHERE a.name = $1 FOR UPDATE OF a"
                        ),
                        &[&name, &by.id, &by.groups],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(found) = found else {
                    return Ok(Changed::NoSuchApplication);
                };
                let id: i64 = get(&found, 0)?;
                if !get::<bool>(&found, 1)? {
                    return Ok(Changed::NotAContributor);
                }
                let mut resolved: [Vec<Named>; 2] = [Vec::new(), Vec::new()];
                for (asked, into) in [change.add, change.remove].into_iter().zip(&mut resolved) {
                    into.reserve(asked.len());
                    for one in asked {
                        match one {
                            Asked::Entry(named) => into.push(named),
                            Asked::Email(email) => match person_of(&transaction, &email).await? {
                                Person::One(id) => into.push(Named::User(id)),
                                Person::Nobody => return Ok(Changed::UnknownEmail(email)),
                                Person::Several => return Ok(Changed::AmbiguousEmail(email)),
                            },
                        }
                    }
                }
                let [add, remove] = resolved;
                // The same entry on both sides can only come of an email
                // (the API refuses it otherwise): a person by their id on
                // one side and by email on the other. At most 100 by 100.
                let mut add = add;
                if let Some(at) = add.iter().position(|a| remove.contains(a)) {
                    return Ok(Changed::SameInBoth(match add.swap_remove(at) {
                        Named::User(name) | Named::Group(name) => name,
                        Named::Everyone => "everyone".to_owned(),
                    }));
                }
                let removed = removed_from(&transaction, id, list, &remove).await?;
                let added = added_to(&transaction, id, list, &add, &by.id).await?;
                let counted = transaction
                    .query_one(
                        "SELECT count(*) FROM application_entries
                         WHERE application_id = $1 AND list = $2",
                        &[&id, &list],
                    )
                    .await
                    .map_err(DbError::from)?;
                let count: i64 = get(&counted, 0)?;
                if contributors && count == 0 {
                    return Ok(Changed::LastContributor(removed));
                }
                if count > bound {
                    return Ok(Changed::TooMany(added));
                }
                if !added.is_empty() || !removed.is_empty() {
                    transaction
                        .execute(
                            "UPDATE applications SET lists_generation = lists_generation + 1
                             WHERE id = $1",
                            &[&id],
                        )
                        .await
                        .map_err(DbError::from)?;
                }
                let application = record_of(&transaction, Key::Id(id)).await?.ok_or_else(|| {
                    DbError::unexpected("an Application gone while it was locked")
                })?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Changed::Changed {
                    application: Box::new(application),
                    added,
                    removed,
                })
            })
            .await;
        match changed {
            Ok(Changed::Changed {
                application,
                added,
                removed,
            }) => {
                self.install_changed(name, Lists::of(&application));
                Ok(Changed::Changed {
                    application,
                    added,
                    removed,
                })
            }
            Ok(refused) => Ok(refused),
            Err(error) => {
                // The commit may have happened without its answer arriving.
                self.after_lost(name).await;
                Err(error)
            }
        }
    }

    /// Sends a Version of `name` as `sender`, who must be a Contributor. Its
    /// migrations must continue the previous Version's; the own database is
    /// made ready and the pending ones applied; then it gets the next number
    /// and becomes the serving Version, in the database and then in memory,
    /// before this returns.
    ///
    /// All of it runs as a task of its own ([`OwnDatabases::to_the_end`]):
    /// a sender that goes away before the answer leaves the migrations
    /// committed and the Version recorded, or neither.
    pub async fn send_version(
        self: &Arc<Self>,
        name: &str,
        version: NewVersion,
        sender: &User,
    ) -> Result<Sent, DbError> {
        let registry = Arc::clone(self);
        // Copied: the task owns what it works on; once per Version sent.
        let name = name.to_owned();
        let sender = sender.clone();
        self.own
            .to_the_end(async move { registry.send_version_here(&name, version, &sender).await })
            .await
            .unwrap_or_else(|why| match why {
                NotToTheEnd::Busy => Ok(Sent::Busy),
                NotToTheEnd::Failed(why) => Err(DbError::unexpected(why)),
            })
    }

    /// [`Registry::send_version`]'s work, in its task.
    async fn send_version_here(
        &self,
        name: &str,
        version: NewVersion,
        sender: &User,
    ) -> Result<Sent, DbError> {
        let found = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "SELECT a.id, ",
                            is_contributor!("a.id", "$2", "$3"),
                            ", (SELECT v.migrations FROM versions v WHERE v.application_id = a.id
                                ORDER BY v.number DESC LIMIT 1),
                               a.serving_version
                             FROM applications a WHERE a.name = $1"
                        ),
                        &[&name, &sender.id, &sender.groups],
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
        // For the words of a refusal: the Version serving when it was sent.
        let serving = get::<Option<i32>>(&found, 3)?.map(number_of).transpose()?;
        if !continues(&previous, &version.migrations) {
            return Ok(Sent::Diverged);
        }
        match self
            .own
            .prepare(id, name, &sender.id, &version.migrations)
            .await
        {
            // What the migrations changed is shipped to the Change history:
            // asked for now, not waited for (blackbox/CONTRACT.md,
            // "Shipping: When").
            Ok(()) => self.own.ship(name),
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
                    serving,
                });
            }
            Err(Problem::Refused {
                migration,
                rule,
                message,
            }) => {
                return Ok(Sent::MigrationRefused {
                    migration,
                    rule,
                    message,
                    serving,
                });
            }
            Err(Problem::Unavailable { step, code }) => {
                return Ok(Sent::OwnDatabaseUnavailable { step, code });
            }
        }
        self.record(name, id, version, &sender.id).await
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
        let structures = serde_json::to_value(&version.structures)
            .map_err(|_| DbError::unexpected("the structures cannot be written as JSON"))?;
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
                    return Ok(None);
                }
                let row = transaction
                    .query_one(
                        concat!(
                            "WITH sent AS (
                                INSERT INTO versions (application_id, number, bundle, bundle_sha256,
                                                      declared_access, git_commit, created_by,
                                                      migrations, structures)
                                VALUES ($1,
                                        (SELECT coalesce(max(number), 0) + 1 FROM versions
                                         WHERE application_id = $1),
                                        $2, $3, $4, $5, $6, $7, $8)
                                RETURNING number, declared_access, git_commit, bundle_sha256,
                                          created_by, created_at, migrations, structures
                            ), serving AS (
                                UPDATE applications SET serving_version = (SELECT number FROM sent),
                                    serving_generation = serving_generation + 1
                                WHERE id = $1
                                RETURNING serving_generation
                            )
                            SELECT number, declared_access, git_commit, bundle_sha256, migrations,
                                   created_by, ",
                            rfc3339!("created_at"),
                            ", structures, (SELECT serving_generation FROM serving) FROM sent"
                        ),
                        &[
                            &id,
                            &&*bundle,
                            &digest,
                            &declared_access,
                            &git_commit,
                            &sender,
                            &migrations,
                            &structures,
                        ],
                    )
                    .await
                    .map_err(DbError::from)?;
                let generation: i64 = get(&row, 8)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some((
                    generation,
                    VersionRecord {
                        application: name.to_owned(),
                        number: number_of(get(&row, 0)?)?,
                        declared_access: get(&row, 1)?,
                        git_commit: get(&row, 2)?,
                        bundle_sha256: get(&row, 3)?,
                        migrations: get(&row, 4)?,
                        created_by: get(&row, 5)?,
                        created_at: get(&row, 6)?,
                        structures: get(&row, 7)?,
                    },
                )))
            })
            .await;
        match sent {
            Ok(Some((generation, record))) => {
                self.install(Serving {
                    application: Arc::from(name),
                    version: record.number,
                    bundle,
                    declared,
                    generation,
                });
                Ok(Sent::Sent(record))
            }
            Ok(None) => Ok(Sent::Diverged),
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
                        "SELECT a.name, v.number, v.bundle, v.declared_access, a.serving_generation
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

    /// Makes Version `to` of `name` the serving one in place of `from`, if
    /// `from` still serves and `to` is a Version of it (`to` 0 is none). In
    /// the database and then in memory, before this returns: once it has
    /// returned every new request finds `to`.
    ///
    /// The switch is one statement: it changes the row only where
    /// `serving_version` is still `from`. Two at once with the same `from`
    /// take the row's lock in turn, and the second, re-reading the row,
    /// finds `from` no longer serving and changes nothing.
    pub async fn go_back(&self, name: &str, from: u64, to: u64) -> Result<WentBack, DbError> {
        // A number no Version can have (Version numbers are `integer`): no
        // row matches, and the answer is what serves.
        let from_stored = i32::try_from(from).unwrap_or(0);
        let to_stored = i32::try_from(to).unwrap_or(0);
        let switched = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "WITH switched AS (
                            UPDATE applications a
                            SET serving_version = $3,
                                serving_generation = a.serving_generation + 1
                            WHERE a.name = $1 AND a.serving_version = $2
                              AND EXISTS (SELECT 1 FROM versions v
                                          WHERE v.application_id = a.id AND v.number = $3)
                            RETURNING a.id, a.serving_generation
                        )
                        SELECT s.serving_generation, v.bundle, v.declared_access, v.migrations,
                               (SELECT l.migrations FROM versions l WHERE l.application_id = s.id
                                ORDER BY l.number DESC LIMIT 1)
                        FROM switched s
                        JOIN versions v ON v.application_id = s.id AND v.number = $3",
                        &[&name, &from_stored, &to_stored],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await;
        let row = match switched {
            Ok(row) => row,
            Err(error) => {
                // The statement may have committed without its answer
                // arriving: read again what serves, so memory follows.
                self.refresh(name).await;
                return Err(error);
            }
        };
        let Some(row) = row else {
            return self.not_gone_back(name, from).await;
        };
        let to_list = recorded_of(get::<Option<Value>>(&row, 3)?.as_ref());
        let latest = recorded_of(get::<Option<Value>>(&row, 4)?.as_ref());
        let migrations_not_undone = latest
            .into_iter()
            .filter(|m| !to_list.iter().any(|t| t.name == m.name))
            .map(|m| m.name)
            .collect();
        self.install(Serving {
            application: Arc::from(name),
            version: to,
            bundle: Arc::from(get::<&str>(&row, 1)?),
            declared: Arc::new(declared_of(&get::<Value>(&row, 2)?)),
            generation: get(&row, 0)?,
        });
        Ok(WentBack::WentBack {
            migrations_not_undone,
        })
    }

    /// Why a go-back from `from` changed nothing: `from` serves (so there
    /// was no Version to go back to), or another does.
    async fn not_gone_back(&self, name: &str, from: u64) -> Result<WentBack, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT serving_version FROM applications WHERE name = $1",
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let serving = match row {
            Some(row) => get::<Option<i32>>(&row, 0)?.map(number_of).transpose()?,
            None => None,
        };
        Ok(if serving == Some(from) {
            WentBack::NoPreviousVersion
        } else {
            WentBack::ServingChanged(serving)
        })
    }

    /// The newest `limit` Versions of `name`, oldest first; `None` when
    /// there is no such Application.
    pub async fn versions(
        &self,
        name: &str,
        limit: u16,
    ) -> Result<Option<Vec<VersionRecord>>, DbError> {
        let limit = i64::from(limit);
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
                            ", v.migrations, v.structures FROM applications a
                              LEFT JOIN LATERAL (
                                  SELECT * FROM versions n WHERE n.application_id = a.id
                                  ORDER BY n.number DESC LIMIT $2) v ON true
                              WHERE a.name = $1
                              ORDER BY v.number"
                        ),
                        &[&name, &limit],
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
                            ", v.migrations, v.structures FROM applications a
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

/// How an Application is looked up.
#[derive(Clone, Copy)]
enum Key<'a> {
    Name(&'a str),
    Id(i64),
}

/// The Application by `key`, with both its lists, read on `client` (a
/// transaction's, to see what it changed): two queries.
async fn record_of<C: GenericClient>(
    client: &C,
    key: Key<'_>,
) -> Result<Option<ApplicationRecord>, DbError> {
    macro_rules! application_where {
        ($condition:literal) => {
            concat!(
                "SELECT a.id, a.name, a.description, a.serving_version, a.created_by, ",
                rfc3339!("a.created_at"),
                ", (SELECT max(v.number) FROM versions v WHERE v.application_id = a.id),
                   a.lists_generation
                  FROM applications a WHERE ",
                $condition
            )
        };
    }
    let row = match key {
        Key::Name(name) => {
            client
                .query_opt(application_where!("a.name = $1"), &[&name])
                .await
        }
        Key::Id(id) => {
            client
                .query_opt(application_where!("a.id = $1"), &[&id])
                .await
        }
    }
    .map_err(DbError::from)?;
    let Some(row) = row else { return Ok(None) };
    let id: i64 = get(&row, 0)?;
    let rows = client
        .query(
            concat!(
                "SELECT e.list, e.kind, e.name, u.name, u.email, e.added_by, ",
                rfc3339!("e.added_at"),
                " FROM application_entries e
                  LEFT JOIN users u ON e.kind = 'user' AND u.id = e.name
                  WHERE e.application_id = $1 ORDER BY e.id"
            ),
            &[&id],
        )
        .await
        .map_err(DbError::from)?;
    let mut contributors = Vec::new();
    let mut shared_with = Vec::new();
    for row in &rows {
        let list: &str = get(row, 0)?;
        let entry = entry_of(row, 1)?;
        if list == List::Contributors.stored() {
            contributors.push(entry);
        } else {
            shared_with.push(entry);
        }
    }
    Ok(Some(ApplicationRecord {
        generation: get(&row, 7)?,
        name: get(&row, 1)?,
        description: get(&row, 2)?,
        contributors,
        shared_with,
        serving_version: get::<Option<i32>>(&row, 3)?.map(number_of).transpose()?,
        latest_version: get::<Option<i32>>(&row, 6)?.map(number_of).transpose()?,
        created_by: get(&row, 4)?,
        created_at: get(&row, 5)?,
    }))
}

/// An entry from the columns `kind, name, <person's name>, <person's
/// email>, added_by, added_at` from `at` on.
fn entry_of(row: &Row, at: usize) -> Result<Entry, DbError> {
    let kind: &str = get(row, at)?;
    Ok(Entry {
        named: Named::of_stored(kind, get(row, at + 1)?)?,
        name: get(row, at + 2)?,
        email: get(row, at + 3)?,
        added_by: get(row, at + 4)?,
        added_at: get(row, at + 5)?,
    })
}

/// Who the platform knows by an email.
enum Person {
    One(String),
    Nobody,
    Several,
}

/// The person whose email, as last read at their sign-in, is `email`,
/// letter case ignored (by the index on `lower(email)`); at most two read.
async fn person_of<C: GenericClient>(client: &C, email: &str) -> Result<Person, DbError> {
    let rows = client
        .query(
            "SELECT id FROM users WHERE lower(email) = lower($1) ORDER BY id LIMIT 2",
            &[&email],
        )
        .await
        .map_err(DbError::from)?;
    Ok(match rows.as_slice() {
        [] => Person::Nobody,
        [one] => Person::One(get(one, 0)?),
        _ => Person::Several,
    })
}

/// The kinds and the names of `entries`, as two arrays for `unnest`.
fn arrays(entries: &[Named]) -> (Vec<&str>, Vec<&str>) {
    entries.iter().map(Named::stored).unzip()
}

/// Removes `entries` (those there) from the list `list` of the Application
/// `id`, in one statement; answers the entries removed, as they were kept,
/// in the order they had been added.
async fn removed_from<C: GenericClient>(
    client: &C,
    id: i64,
    list: &str,
    entries: &[Named],
) -> Result<Vec<Entry>, DbError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let (kinds, names) = arrays(entries);
    let rows = client
        .query(
            concat!(
                "WITH asked AS (SELECT * FROM unnest($3::text[], $4::text[]) AS t (kind, name)),
                 gone AS (
                    DELETE FROM application_entries e USING asked
                    WHERE e.application_id = $1 AND e.list = $2
                      AND e.kind = asked.kind AND e.name = asked.name
                    RETURNING e.id, e.kind, e.name, e.added_by, e.added_at
                 )
                 SELECT g.kind, g.name, u.name, u.email, g.added_by, ",
                rfc3339!("g.added_at"),
                " FROM gone g LEFT JOIN users u ON g.kind = 'user' AND u.id = g.name
                  ORDER BY g.id"
            ),
            &[&id, &list, &kinds, &names],
        )
        .await
        .map_err(DbError::from)?;
    rows.iter().map(|row| entry_of(row, 0)).collect()
}

/// Adds `entries` (those not there) to the list `list` of the Application
/// `id`, as added by `by`, in one statement and in their order (an entry
/// given twice is added once; one there keeps its row); answers the
/// entries added, in order.
async fn added_to<C: GenericClient>(
    client: &C,
    id: i64,
    list: &str,
    entries: &[Named],
    by: &str,
) -> Result<Vec<Entry>, DbError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let (kinds, names) = arrays(entries);
    let rows = client
        .query(
            concat!(
                "WITH added AS (
                    INSERT INTO application_entries (application_id, list, kind, name, added_by)
                    SELECT $1, $2, t.kind, t.name, $5
                    FROM unnest($3::text[], $4::text[]) WITH ORDINALITY AS t (kind, name, n)
                    ORDER BY t.n
                    ON CONFLICT (application_id, kind, name, list) DO NOTHING
                    RETURNING id, kind, name, added_by, added_at
                 )
                 SELECT a.kind, a.name, u.name, u.email, a.added_by, ",
                rfc3339!("a.added_at"),
                " FROM added a LEFT JOIN users u ON a.kind = 'user' AND u.id = a.name
                  ORDER BY a.id"
            ),
            &[&id, &list, &kinds, &names, &by],
        )
        .await
        .map_err(DbError::from)?;
    rows.iter().map(|row| entry_of(row, 0)).collect()
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
        structures: get(row, 8)?,
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
        generation: get(row, 4)?,
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
