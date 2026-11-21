//! The Application registry: Applications, their Contributors, their
//! Versions, and which Version serves, kept in the platform database.
//!
//! Interface: [`Registry::load`] once at start; then the API's calls
//! ([`Registry::create_application`], [`Registry::send_version`], and the
//! reads), and Ingress's [`Registry::serving`], which answers from memory.
//!
//! What memory holds of each Application ([`Kept`]): which Version serves
//! it and its two lists. One program serves an installation (ADR 0015), so
//! memory is the database's copy and is only ever changed after the
//! database: a write commits, then memory is changed, then the API answers.
//! So once the answer has been received every new request finds what it
//! wrote, without a round trip to Postgres per request. A request already
//! running holds an `Arc` of the Version it started on and finishes on it.
//!
//! Every write of what serves (a Version sent, a go-back) and of the lists
//! (a creation, a change of a list) is one transaction that holds the
//! Application's row lock from its first statement that writes or locks
//! until its commit is done, and adds one to the Application's
//! `serving_generation` or `lists_generation` under it; memory only ever
//! moves to a higher generation, so writes at once leave memory as the
//! database's last whatever order they report back in.
//!
//! A write whose answer is lost may still be made after that answer (its
//! commit on its way). Its COMMIT is sent only once the statement that
//! takes the row's lock has answered, so if it lands at all it held the
//! lock from before the answer was lost until it landed. So memory forgets
//! the Application at once ([`Held::Unknown`]), reading nothing first that
//! could be earlier than the write, and a request to it reads it back
//! ([`Registry::read_back`]): in one transaction, the row's share lock
//! taken without waiting (while the write is on its way this fails at once
//! and the request is `503`), then, in a second statement whose snapshot is
//! after the lock, the generations, the serving Version with its bundle,
//! and the lists. A read-back begun before a forgetting (`epoch`), or that
//! misses a generation a write reported since (`seen`), is not taken. At a
//! start, Applications whose row a write holds are skipped and held as not
//! known. Nothing is served or let in on a guess. Forgetting only ever
//! replaces what memory holds of an Application, or adds one for an
//! Application the database has, so memory holds at most one entry per
//! Application.
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
//! database whether a User is a Contributor (`is_contributor!`). Who may
//! open an Application is decided at each request from memory, like
//! Grants (crate::resources).
//!
//! Going back ([`Registry::go_back`]) is one `UPDATE`, in a transaction of
//! its own, that changes the serving Version only where it is still the one
//! the caller saw: two go-backs from the same Version at once are
//! serialised by the row's lock, and the second finds it changed and
//! changes nothing.
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
use tokio::sync::Semaphore;
use tokio::time::Instant;
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

/// What memory holds of an Application it knows: its two lists and the
/// Version that serves it, each as of the generation it was written or
/// read at. Replaced whole, never changed in place.
#[derive(Debug)]
pub struct Kept {
    lists: Arc<Lists>,
    serving: Option<Arc<Serving>>,
    /// The Application's `serving_generation` `serving` is of.
    serving_generation: i64,
    /// Whether a Platform administrator has disabled it.
    disabled: bool,
    /// The Application's `disabled_generation` `disabled` is of.
    disabled_generation: i64,
}

impl Kept {
    /// Whether `user` may open it: a Contributor, or shared with them.
    pub fn opens(&self, user: &User) -> bool {
        self.lists.open_to(user)
    }

    /// The Version that serves it, if one does.
    pub fn serving(&self) -> Option<&Arc<Serving>> {
        self.serving.as_ref()
    }

    /// Whether a Platform administrator has disabled it.
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    /// The same, with `lists` in place of its lists.
    fn with_lists(&self, lists: Lists) -> Self {
        Self {
            lists: Arc::new(lists),
            serving: self.serving.as_ref().map(Arc::clone),
            ..*self
        }
    }
}

/// The generations a forgotten Application must be read back at, at
/// least: the highest memory held or a write reported since.
#[derive(Clone, Copy, Debug, Default)]
pub struct Seen {
    lists: i64,
    serving: i64,
    disabled: i64,
}

/// What stops the requests in flight to one Application when it is
/// disabled (blackbox/CONTRACT.md, "What Platform administrators see and
/// stop: Requests in flight"). A request takes the Application's one with
/// what memory holds of it ([`Registry::find_for_request`]); memory fires
/// it when it puts a disabled state in, after the database committed it and
/// before the disable is answered. Once fired it stays fired: the
/// Application gets a new one when memory next holds it enabled. One value
/// (a `watch`), so nothing queues.
#[derive(Debug)]
pub struct Stop {
    fired: tokio::sync::watch::Sender<bool>,
}

impl Default for Stop {
    fn default() -> Self {
        Self {
            fired: tokio::sync::watch::Sender::new(false),
        }
    }
}

impl Stop {
    fn fire(&self) {
        self.fired.send_replace(true);
    }

    /// Whether it has been fired: the Application was disabled.
    pub fn is_stopped(&self) -> bool {
        *self.fired.borrow()
    }

    /// Ends once it has been fired.
    pub async fn stopped(&self) {
        let mut fired = self.fired.subscribe();
        if fired.wait_for(|fired| *fired).await.is_err() {
            // The sender is `self`'s and lives as long as this borrow: not
            // reached. Never taken as a stop.
            std::future::pending::<()>().await;
        }
    }
}

/// What memory holds of an Application.
#[derive(Debug)]
pub enum Held {
    Known(Arc<Kept>),
    /// Forgotten: a write's answer was lost and it could not be read back.
    /// `epoch` tells a read-back begun before this forgetting from one
    /// begun after.
    Unknown {
        seen: Seen,
        epoch: u64,
    },
}

/// An Application as memory has it now.
#[derive(Debug)]
pub enum Found {
    Known(Arc<Kept>),
    /// Memory does not know it now: it must be read back.
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
    /// Since when, and by whom, it is disabled; `None` when it is not.
    pub disabled: Option<Disabled>,
}

/// A disable in force: when it was made (RFC 3339, UTC) and by which
/// Platform administrator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disabled {
    pub since: String,
    pub by: String,
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
    /// Since when it is disabled; `None` when it is not.
    pub disabled_since: Option<String>,
}

/// Which of the Applications a User contributes to or can open
/// [`Registry::listed_for`] reads.
#[derive(Clone, Copy, Debug)]
pub enum Listing {
    /// Every one, by name (`GET /v1/applications`).
    All,
    /// Those they are a Contributor of first, then the others, each group
    /// by name; `limit` of them from the `offset`-th on (the Dashboard).
    Paged { limit: i64, offset: i64 },
}

/// What the Dashboard shows of an Application besides its place in a list
/// ([`Registry::shown`]).
#[derive(Debug)]
pub struct ShownApplication {
    /// What its serving Version declares; empty when none serves.
    pub declared: Declared,
    /// How many Contributors it has.
    pub contributors_count: i64,
    /// Its first Contributors, in the order of the list.
    pub contributors: Vec<ShownPerson>,
}

/// A Contributor as the Dashboard's list shows them: what the entry names,
/// and a person's name and email as last read at their sign-in.
#[derive(Debug)]
pub struct ShownPerson {
    pub named: Named,
    pub name: Option<String>,
    pub email: Option<String>,
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
pub(crate) fn declared_of(stored: &Value) -> Declared {
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
    Created(Box<ApplicationRecord>),
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
    /// The own database could not be made ready or reached: the step, and
    /// why (with Postgres's SQLSTATE if it gave one).
    OwnDatabaseUnavailable {
        step: crate::own_db::Step,
        cause: crate::platform_db::Cause,
    },
}

pub struct Registry {
    database: Arc<PlatformDatabase>,
    own: Arc<OwnDatabases>,
    /// What memory holds of every Application, by name: one entry per
    /// Application. Held only for a lookup or an insert, never across an
    /// await.
    held: RwLock<HashMap<Arc<str>, Held>>,
    /// The number of forgettings so far, each one's `epoch`.
    forgotten: AtomicU64,
    /// The readings of what memory does not know under way, shared with
    /// crate::resources: at most [`crate::resources::READ_BACKS`] in the
    /// program. A request that finds them all taken is `503` at once.
    reading_back: Arc<Semaphore>,
    /// Each Application's [`Stop`], by name: one per Application a request
    /// was given (so at most one per Application). Taken only while
    /// [`Registry::held`] is held (read or written), after it, and never
    /// across an await.
    stops: std::sync::Mutex<HashMap<Arc<str>, Arc<Stop>>>,
}

impl Registry {
    /// Reads every Application, its serving Version and its lists from the
    /// platform database.
    ///
    /// In one transaction, every Application's row share lock is taken
    /// without waiting for the rows a write holds (`SKIP LOCKED`); then, in
    /// statements whose snapshot is after the locks, the generations, the
    /// serving Versions and the lists. An Application whose row was skipped
    /// is held as not known: a write of it was on its way; it is read back
    /// at a request once the write is settled. So one Application's waiting
    /// write keeps neither the start nor the others back.
    pub async fn load(
        database: Arc<PlatformDatabase>,
        own: Arc<OwnDatabases>,
        reading_back: Arc<Semaphore>,
    ) -> Result<Self, DbError> {
        let (applications, entries) = database
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
                    .query("SELECT id FROM applications FOR SHARE SKIP LOCKED", &[])
                    .await
                    .map_err(DbError::from)?;
                let locked: Vec<i64> = locked
                    .iter()
                    .map(|row| get::<i64>(row, 0))
                    .collect::<Result<_, _>>()?;
                // Statements after the locks: their snapshots are later.
                let applications = transaction
                    .query(
                        "SELECT a.id, a.name, a.lists_generation, a.serving_generation,
                                a.id = ANY ($1), v.number, v.bundle, v.declared_access,
                                a.disabled_at IS NOT NULL, a.disabled_generation
                         FROM applications a
                         LEFT JOIN versions v ON v.application_id = a.id
                                             AND v.number = a.serving_version
                                             AND a.id = ANY ($1)",
                        &[&locked],
                    )
                    .await
                    .map_err(DbError::from)?;
                let entries = transaction
                    .query(
                        "SELECT application_id, kind, name FROM application_entries
                         WHERE application_id = ANY ($1)",
                        &[&locked],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok((applications, entries))
            })
            .await?;
        let mut lists: HashMap<i64, Lists> = HashMap::new();
        for row in &entries {
            lists
                .entry(get(row, 0)?)
                .or_default()
                .add(&Named::of_stored(get(row, 1)?, get(row, 2)?)?);
        }
        let forgotten = AtomicU64::new(0);
        let mut held = HashMap::with_capacity(applications.len());
        let mut unknown = Vec::new();
        for row in &applications {
            let id: i64 = get(row, 0)?;
            let name: Arc<str> = Arc::from(get::<&str>(row, 1)?);
            if !get::<bool>(row, 4)? {
                unknown.push(Arc::clone(&name));
                let epoch = forgotten.fetch_add(1, Ordering::Relaxed);
                let seen = Seen::default();
                held.insert(name, Held::Unknown { seen, epoch });
                continue;
            }
            let mut read = lists.remove(&id).unwrap_or_default();
            read.generation = get(row, 2)?;
            let kept = Kept {
                lists: Arc::new(read),
                serving: serving_at(&name, row, 5)?.map(Arc::new),
                serving_generation: get(row, 3)?,
                disabled: get(row, 8)?,
                disabled_generation: get(row, 9)?,
            };
            held.insert(name, Held::Known(Arc::new(kept)));
        }
        if !unknown.is_empty() {
            log::record(
                "application-unknown",
                serde_json::json!({ "applications": unknown.iter().map(|n| &**n).collect::<Vec<_>>() }),
            );
        }
        Ok(Self {
            database,
            own,
            held: RwLock::new(held),
            forgotten,
            reading_back,
            stops: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// [`Registry::find`], with the Application's [`Stop`] when memory
    /// knows it: both read under one lock, so that a disable put into
    /// memory after this fires the one answered.
    pub fn find_for_request(&self, name: &str) -> (Found, Option<Arc<Stop>>) {
        let all = self.held.read().unwrap_or_else(PoisonError::into_inner);
        match all.get(name) {
            None => (Found::NoSuchApplication, None),
            Some(Held::Unknown { .. }) => (Found::NotKnown, None),
            Some(Held::Known(kept)) => {
                let stop = {
                    let mut stops = self.stops.lock().unwrap_or_else(PoisonError::into_inner);
                    match stops.get(name) {
                        Some(stop) => Arc::clone(stop),
                        None => {
                            let stop = Arc::new(Stop::default());
                            stops.insert(Arc::from(name), Arc::clone(&stop));
                            stop
                        }
                    }
                };
                (Found::Known(Arc::clone(kept)), Some(stop))
            }
        }
    }

    /// After memory's disabled state of `name` changed (`disabled`), with
    /// [`Registry::held`] written: a disable fires the Application's
    /// [`Stop`]; an enable leaves a fired one behind, so that the next
    /// request gets a new one.
    fn settle_stop(&self, name: &str, disabled: bool) {
        let mut stops = self.stops.lock().unwrap_or_else(PoisonError::into_inner);
        if disabled {
            if let Some(stop) = stops.get(name) {
                stop.fire();
            }
        } else if stops.get(name).is_some_and(|stop| stop.is_stopped()) {
            stops.remove(name);
        }
    }

    /// The Application `name` as memory has it: no round trip, and the
    /// lock is held for the lookup alone.
    pub fn find(&self, name: &str) -> Found {
        match self
            .held
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            None => Found::NoSuchApplication,
            Some(Held::Unknown { .. }) => Found::NotKnown,
            Some(Held::Known(kept)) => Found::Known(Arc::clone(kept)),
        }
    }

    /// Whether `name` may be served: memory knows it to have a serving
    /// Version, or does not know it now (it is read back at its request).
    pub fn may_serve(&self, name: &str) -> bool {
        match self.find(name) {
            Found::Known(kept) => kept.serving.is_some(),
            Found::NotKnown => true,
            Found::NoSuchApplication => false,
        }
    }

    /// Reads `name` back from the platform database into memory, for a
    /// request that found it not known. `false` when there is no such
    /// Application. Never waits for the platform database: at most
    /// [`crate::resources::READ_BACKS`] readings at once in the program
    /// (one more is refused at once), done by `deadline` (the request's,
    /// shared by all its readings), and refused at once while a write of it
    /// is on its way (the row is locked).
    pub async fn read_back(&self, name: &str, deadline: Instant) -> Result<bool, DbError> {
        let Ok(_permit) = self.reading_back.try_acquire() else {
            return Err(DbError::unexpected("too many read-backs at once"));
        };
        let began = match self
            .held
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            None => Began::Nothing,
            Some(Held::Known(_)) => Began::Known,
            Some(Held::Unknown { epoch, .. }) => Began::Unknown(*epoch),
        };
        let read = tokio::time::timeout_at(deadline, self.kept_of(name))
            .await
            .map_err(|_| {
                DbError::timed_out(
                    "reading an Application back",
                    crate::resources::READ_BACK_WAIT,
                )
            })?;
        let Some(kept) = read? else {
            return Ok(false);
        };
        self.install_read(name, kept, began);
        Ok(true)
    }

    /// `name` from the platform database, never earlier than a write of
    /// it: in one transaction, the Application's row share lock taken
    /// without waiting (`NOWAIT`: a write holds the row's lock until its
    /// commit is done, so while one is on its way this fails at once with
    /// `lock_not_available`), then, in a second statement whose snapshot
    /// is after the lock was got, the generations, the serving Version
    /// with its bundle, and the entries of both lists. `None` for no such
    /// Application.
    async fn kept_of(&self, name: &str) -> Result<Option<Kept>, DbError> {
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
                        "SELECT id FROM applications WHERE name = $1 FOR SHARE NOWAIT",
                        &[&name],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(found) = found else {
                    return Ok(None);
                };
                let id: i64 = get(&found, 0)?;
                let row = transaction
                    .query_one(
                        "SELECT a.lists_generation, a.serving_generation,
                                v.number, v.bundle, v.declared_access,
                                ARRAY(SELECT e.kind FROM application_entries e
                                      WHERE e.application_id = a.id ORDER BY e.id),
                                ARRAY(SELECT e.name FROM application_entries e
                                      WHERE e.application_id = a.id ORDER BY e.id),
                                a.disabled_at IS NOT NULL, a.disabled_generation
                         FROM applications a
                         LEFT JOIN versions v ON v.application_id = a.id
                                             AND v.number = a.serving_version
                         WHERE a.id = $1",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(row))
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let mut lists = Lists {
            generation: get(&row, 0)?,
            ..Lists::default()
        };
        let kinds: Vec<&str> = get(&row, 5)?;
        let names: Vec<String> = get(&row, 6)?;
        for (kind, named) in kinds.into_iter().zip(names) {
            lists.add(&Named::of_stored(kind, named)?);
        }
        Ok(Some(Kept {
            lists: Arc::new(lists),
            serving: serving_at(name, &row, 2)?.map(Arc::new),
            serving_generation: get(&row, 1)?,
            disabled: get(&row, 7)?,
            disabled_generation: get(&row, 8)?,
        }))
    }

    /// Puts into memory the lists a creation (`created`) or a change
    /// committed: over lists of a lower generation; over a forgotten
    /// Application it only raises `seen`, since a lost write may have gone
    /// further. An Application memory does not hold that was not just
    /// created (its creation's answer was lost) is held as not known: its
    /// serving Version is read back at a request.
    fn install_lists(&self, name: &str, lists: Lists, created: bool) {
        let mut all = self.held.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            None => {
                let held = if created {
                    Held::Known(Arc::new(Kept {
                        lists: Arc::new(lists),
                        serving: None,
                        serving_generation: 0,
                        disabled: false,
                        disabled_generation: 0,
                    }))
                } else {
                    Held::Unknown {
                        seen: Seen {
                            lists: lists.generation,
                            ..Seen::default()
                        },
                        epoch: self.forgotten.fetch_add(1, Ordering::Relaxed),
                    }
                };
                all.insert(Arc::from(name), held);
            }
            Some(Held::Known(kept)) => {
                if kept.lists.generation < lists.generation {
                    *kept = Arc::new(kept.with_lists(lists));
                }
            }
            Some(Held::Unknown { seen, .. }) => {
                seen.lists = seen.lists.max(lists.generation);
            }
        }
    }

    /// Puts into memory the Version a Version sent or a go-back made the
    /// serving one, as of `generation`: over a lower generation; over a
    /// forgotten Application it only raises `seen`. An Application memory
    /// does not hold (its creation's answer was lost) is held as not known:
    /// its lists are read back at a request.
    fn install_serving(&self, name: &str, serving: Serving, generation: i64) {
        let mut all = self.held.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            None => {
                let seen = Seen {
                    serving: generation,
                    ..Seen::default()
                };
                let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
                all.insert(Arc::from(name), Held::Unknown { seen, epoch });
            }
            Some(Held::Known(kept)) => {
                if kept.serving_generation < generation {
                    *kept = Arc::new(Kept {
                        lists: Arc::clone(&kept.lists),
                        serving: Some(Arc::new(serving)),
                        serving_generation: generation,
                        disabled: kept.disabled,
                        disabled_generation: kept.disabled_generation,
                    });
                }
            }
            Some(Held::Unknown { seen, .. }) => {
                seen.serving = seen.serving.max(generation);
            }
        }
    }

    /// Puts into memory whether `name` is disabled, as a disable or an
    /// enable committed it at `generation`: over a lower generation; over a
    /// forgotten Application it only raises `seen`. A disable put in fires
    /// the Application's [`Stop`]. An Application memory does not hold is
    /// held as not known.
    fn install_disabled(&self, name: &str, disabled: bool, generation: i64) {
        let mut all = self.held.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            None => {
                let seen = Seen {
                    disabled: generation,
                    ..Seen::default()
                };
                let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
                all.insert(Arc::from(name), Held::Unknown { seen, epoch });
            }
            Some(Held::Known(kept)) => {
                if kept.disabled_generation < generation {
                    *kept = Arc::new(Kept {
                        lists: Arc::clone(&kept.lists),
                        serving: kept.serving.as_ref().map(Arc::clone),
                        serving_generation: kept.serving_generation,
                        disabled,
                        disabled_generation: generation,
                    });
                    self.settle_stop(name, disabled);
                }
            }
            Some(Held::Unknown { seen, .. }) => {
                seen.disabled = seen.disabled.max(generation);
            }
        }
    }

    /// Puts what a request's read-back read into memory: over a forgotten
    /// Application only when it was forgotten before the read began, and
    /// what was read is at least of the generations reported since; never
    /// over one memory knows (its writes keep it).
    fn install_read(&self, name: &str, kept: Kept, began: Began) {
        let mut all = self.held.write().unwrap_or_else(PoisonError::into_inner);
        let disabled = kept.disabled;
        match all.get_mut(name) {
            None => {
                if matches!(began, Began::Nothing) {
                    all.insert(Arc::from(name), Held::Known(Arc::new(kept)));
                    self.settle_stop(name, disabled);
                }
            }
            Some(Held::Known(_)) => {}
            Some(held @ Held::Unknown { .. }) => {
                if let (Held::Unknown { seen, epoch }, Began::Unknown(at)) = (&*held, began)
                    && *epoch == at
                    && seen.lists <= kept.lists.generation
                    && seen.serving <= kept.serving_generation
                    && seen.disabled <= kept.disabled_generation
                {
                    *held = Held::Known(Arc::new(kept));
                    self.settle_stop(name, disabled);
                }
            }
        }
    }

    /// After a write whose answer was lost: it may still be made (its
    /// commit on its way), so the Application is forgotten at once, with
    /// nothing read first that could be earlier than it; requests read it
    /// back once it is settled. A name memory does not hold is held as not
    /// known only when the database has the Application (`exists`: it was
    /// found there before the write); a lost creation adds nothing.
    fn after_lost(&self, name: &str, exists: bool) {
        log::record(
            "application-unknown",
            serde_json::json!({ "application": name }),
        );
        let epoch = self.forgotten.fetch_add(1, Ordering::Relaxed);
        let mut all = self.held.write().unwrap_or_else(PoisonError::into_inner);
        match all.get_mut(name) {
            Some(held) => {
                let seen = match held {
                    Held::Known(kept) => Seen {
                        lists: kept.lists.generation,
                        serving: kept.serving_generation,
                        disabled: kept.disabled_generation,
                    },
                    Held::Unknown { seen, .. } => *seen,
                };
                *held = Held::Unknown { seen, epoch };
            }
            None if exists => {
                let seen = Seen::default();
                all.insert(Arc::from(name), Held::Unknown { seen, epoch });
            }
            None => {}
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

    /// The names of the Applications memory knows to have a serving
    /// Version, sorted.
    pub fn serving_names(&self) -> Vec<Arc<str>> {
        let mut names: Vec<Arc<str>> = self
            .held
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, held)| matches!(held, Held::Known(kept) if kept.serving.is_some()))
            .map(|(name, _)| Arc::clone(name))
            .collect();
        names.sort_unstable();
        names
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
                self.after_lost(name, false);
                return Err(error);
            }
        };
        Ok(match created {
            Some(application) => {
                self.install_lists(name, Lists::of(&application), true);
                Created::Created(Box::new(application))
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
    /// by name (compared byte by byte, whatever the database's collation);
    /// with [`Listing::Paged`], those they are a Contributor of first, and
    /// one page of them. One query.
    pub async fn listed_for(
        &self,
        user: &User,
        listing: Listing,
    ) -> Result<Vec<ListedApplication>, DbError> {
        let (contributors_first, limit, offset) = match listing {
            Listing::All => (false, None, 0_i64),
            Listing::Paged { limit, offset } => (true, Some(limit), offset),
        };
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        concat!(
                            "SELECT name, description, serving_version, contributor, disabled_since FROM (
                               SELECT a.name, a.description, a.serving_version, ",
                            rfc3339!("a.disabled_at"),
                            " AS disabled_since, ",
                            is_contributor!("a.id", "$1", "$2"),
                            " AS contributor FROM applications a
                              WHERE a.id IN (SELECT e.application_id FROM application_entries e
                                             WHERE ",
                            names_person!("$1", "$2"),
                            ")) l ORDER BY $5 AND contributor DESC, name COLLATE \"C\" LIMIT $3 OFFSET $4"
                        ),
                        &[&user.id, &user.groups, &limit, &offset, &contributors_first],
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
                    disabled_since: get(row, 4)?,
                })
            })
            .collect()
    }

    /// What the Dashboard shows of each of the Applications `names` besides
    /// what [`Registry::listed_for`] gives: the Resources its serving
    /// Version declares, how many Contributors it has, and its first
    /// `first` Contributors in the order of the list, a person with the
    /// name and the email the platform last read of them. One query, whatever the number
    /// of names; at most `first` Contributors of each are read.
    pub async fn shown(
        &self,
        names: &[&str],
        first: i64,
    ) -> Result<HashMap<String, ShownApplication>, DbError> {
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT a.name, v.declared_access,
                                (SELECT count(*) FROM application_entries e
                                  WHERE e.application_id = a.id AND e.list = 'contributors'),
                                c.kinds, c.names, c.people, c.emails
                         FROM applications a
                         LEFT JOIN versions v ON v.application_id = a.id
                                             AND v.number = a.serving_version
                         CROSS JOIN LATERAL (
                            SELECT coalesce(array_agg(f.kind ORDER BY f.id), '{}') AS kinds,
                                   coalesce(array_agg(f.name ORDER BY f.id), '{}') AS names,
                                   coalesce(array_agg(u.name ORDER BY f.id), '{}') AS people,
                                   coalesce(array_agg(u.email ORDER BY f.id), '{}') AS emails
                            FROM (SELECT e.id, e.kind, e.name FROM application_entries e
                                  WHERE e.application_id = a.id AND e.list = 'contributors'
                                  ORDER BY e.id LIMIT $2) f
                            LEFT JOIN users u ON f.kind = 'user' AND u.id = f.name
                         ) c
                         WHERE a.name = ANY ($1)",
                        &[&names, &first],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let mut shown = HashMap::with_capacity(rows.len());
        for row in &rows {
            let kinds: Vec<&str> = get(row, 3)?;
            let entries: Vec<String> = get(row, 4)?;
            let people: Vec<Option<String>> = get(row, 5)?;
            let emails: Vec<Option<String>> = get(row, 6)?;
            let mut contributors = Vec::with_capacity(kinds.len());
            for (((kind, name), person), email) in
                kinds.into_iter().zip(entries).zip(people).zip(emails)
            {
                contributors.push(ShownPerson {
                    named: Named::of_stored(kind, name)?,
                    name: person,
                    email,
                });
            }
            shown.insert(
                get::<String>(row, 0)?,
                ShownApplication {
                    declared: get::<Option<Value>>(row, 1)?
                        .map(|stored| declared_of(&stored))
                        .unwrap_or_default(),
                    contributors_count: get(row, 2)?,
                    contributors,
                },
            );
        }
        Ok(shown)
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
                self.install_lists(name, Lists::of(&application), false);
                Ok(Changed::Changed {
                    application,
                    added,
                    removed,
                })
            }
            Ok(refused) => Ok(refused),
            Err(error) => {
                // The commit may have happened without its answer arriving
                // (the Application was found under its lock).
                self.after_lost(name, true);
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
            Err(Problem::Unavailable { step, cause }) => {
                return Ok(Sent::OwnDatabaseUnavailable { step, cause });
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
                let serving = Serving {
                    application: Arc::from(name),
                    version: record.number,
                    bundle,
                    declared,
                };
                self.install_serving(name, serving, generation);
                Ok(Sent::Sent(record))
            }
            Ok(None) => Ok(Sent::Diverged),
            Err(error) => {
                // The commit may land after this answer (its first
                // statement took the row's lock): forgotten at once.
                self.after_lost(name, true);
                Err(error)
            }
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
    /// finds `from` no longer serving and changes nothing. It runs in a
    /// transaction of its own so that its COMMIT is sent only once it holds
    /// the row's lock: a switch still waiting for the lock when its answer
    /// is lost never lands, and one that lands after held the lock first.
    pub async fn go_back(&self, name: &str, from: u64, to: u64) -> Result<WentBack, DbError> {
        // A number no Version can have (Version numbers are `integer`): no
        // row matches, and the answer is what serves.
        let from_stored = i32::try_from(from).unwrap_or(0);
        let to_stored = i32::try_from(to).unwrap_or(0);
        let switched = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let row = transaction
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
                    .map_err(DbError::from)?;
                // What the switch answered is read before the commit: a
                // failure to read it is then a switch not made.
                let Some(row) = row else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(None);
                };
                let to_list = recorded_of(get::<Option<Value>>(&row, 3)?.as_ref());
                let latest = recorded_of(get::<Option<Value>>(&row, 4)?.as_ref());
                let migrations_not_undone: Vec<String> = latest
                    .into_iter()
                    .filter(|m| !to_list.iter().any(|t| t.name == m.name))
                    .map(|m| m.name)
                    .collect();
                let serving = Serving {
                    application: Arc::from(name),
                    version: to,
                    bundle: Arc::from(get::<&str>(&row, 1)?),
                    declared: Arc::new(declared_of(&get::<Value>(&row, 2)?)),
                };
                let generation: i64 = get(&row, 0)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some((serving, generation, migrations_not_undone)))
            })
            .await;
        let switched = match switched {
            Ok(switched) => switched,
            Err(error) => {
                // The commit may land after this answer: forgotten at once
                // (the API found the Application before).
                self.after_lost(name, true);
                return Err(error);
            }
        };
        let Some((serving, generation, migrations_not_undone)) = switched else {
            return self.not_gone_back(name, from).await;
        };
        self.install_serving(name, serving, generation);
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

    /// Disables `name` (`disable`) as the Platform administrator `by`, or
    /// enables it, within `limit` (blackbox/CONTRACT.md, "What Platform
    /// administrators see and stop: Disabling and enabling"). One
    /// transaction: one `UPDATE` that changes the row only where it is not
    /// already so (two disables at once take the row's lock in turn, and
    /// the second, re-reading the row, changes nothing), then the
    /// Application read as it leaves it. Then memory, before this returns:
    /// a disable put in stops the requests in flight. `None` when there is
    /// no such Application; else the Application and whether it changed.
    ///
    /// Its COMMIT is sent only once the `UPDATE` (which takes the row's
    /// lock) has answered; a failure or the end of `limit` forgets the
    /// Application at once, as every write whose answer is lost does.
    pub async fn set_disabled(
        &self,
        name: &str,
        disable: bool,
        by: &str,
        limit: std::time::Duration,
    ) -> Result<Option<(ApplicationRecord, bool)>, DbError> {
        let write = self.database.pool().run(async |client: &mut Client| {
            let transaction = client.transaction().await.map_err(DbError::from)?;
            let row = if disable {
                transaction
                    .query_opt(
                        "UPDATE applications
                         SET disabled_at = now(), disabled_by = $2,
                             disabled_generation = disabled_generation + 1
                         WHERE name = $1 AND disabled_at IS NULL
                         RETURNING disabled_generation",
                        &[&name, &by],
                    )
                    .await
            } else {
                transaction
                    .query_opt(
                        "UPDATE applications
                         SET disabled_at = NULL, disabled_by = NULL,
                             disabled_generation = disabled_generation + 1
                         WHERE name = $1 AND disabled_at IS NOT NULL
                         RETURNING disabled_generation",
                        &[&name],
                    )
                    .await
            }
            .map_err(DbError::from)?;
            let generation: Option<i64> = row.as_ref().map(|row| get(row, 0)).transpose()?;
            let record = record_of(&transaction, Key::Name(name)).await?;
            transaction.commit().await.map_err(DbError::from)?;
            Ok((generation, record))
        });
        let written = match tokio::time::timeout(limit, write).await {
            Ok(written) => written,
            Err(_) => Err(DbError::timed_out("disabling or enabling", limit)),
        };
        let (generation, record) = match written {
            Ok(written) => written,
            Err(error) => {
                // The commit may land after this answer: forgotten at once.
                let exists = !matches!(self.find(name), Found::NoSuchApplication);
                self.after_lost(name, exists);
                return Err(error);
            }
        };
        let Some(record) = record else {
            return Ok(None);
        };
        let changed = generation.is_some();
        if let Some(generation) = generation {
            self.install_disabled(name, disable, generation);
            log::record(
                if disable {
                    "application-disabled"
                } else {
                    "application-enabled"
                },
                serde_json::json!({ "application": name, "by": by }),
            );
        }
        Ok(Some((record, changed)))
    }

    /// One page of every Application of the installation, for Platform
    /// administrators (blackbox/CONTRACT.md, "Every Application, on the
    /// platform's API"): those whose name comes after `after` (byte order),
    /// `limit` of them at most, by name, each with its serving Version's
    /// Declared access, its Contributors (at most [`MAX_CONTRIBUTORS`]), how
    /// many entries of each kind it is shared with, its disable, and its
    /// usage over the last 30 UTC days. One query, in a read-only
    /// transaction whose statements Postgres ends after `limit_time`.
    pub async fn every_application(
        &self,
        after: Option<&str>,
        limit: i64,
        limit_time: std::time::Duration,
    ) -> Result<Vec<AdminApplication>, DbError> {
        let timeout = statement_timeout(limit_time);
        let read = self.database.pool().run(async |client: &mut Client| {
            let transaction = client
                .build_transaction()
                .read_only(true)
                .start()
                .await
                .map_err(DbError::from)?;
            transaction
                .batch_execute(&timeout)
                .await
                .map_err(DbError::from)?;
            let rows = transaction
                .query(
                    concat!(
                        "WITH p AS (SELECT (now() AT TIME ZONE 'UTC')::date - 29 AS from_day)
                         SELECT a.name, a.description, a.serving_version, a.created_by, ",
                        rfc3339!("a.created_at"),
                        ", (SELECT max(n.number) FROM versions n WHERE n.application_id = a.id),
                           v.declared_access, ",
                        rfc3339!("a.disabled_at"),
                        ", a.disabled_by,
                           c.kinds, c.names, c.people, c.emails, c.added_by, c.added_at,
                           s.everyone, s.groups, s.people,
                           to_char(p.from_day, 'YYYY-MM-DD'), u.requests, u.users
                         FROM p, applications a
                         LEFT JOIN versions v ON v.application_id = a.id
                                             AND v.number = a.serving_version
                         CROSS JOIN LATERAL (
                            SELECT coalesce(array_agg(f.kind ORDER BY f.id), '{}') AS kinds,
                                   coalesce(array_agg(f.name ORDER BY f.id), '{}') AS names,
                                   coalesce(array_agg(w.name ORDER BY f.id), '{}') AS people,
                                   coalesce(array_agg(w.email ORDER BY f.id), '{}') AS emails,
                                   coalesce(array_agg(f.added_by ORDER BY f.id), '{}') AS added_by,
                                   coalesce(array_agg(",
                        rfc3339!("f.added_at"),
                        " ORDER BY f.id), '{}') AS added_at
                            FROM (SELECT e.id, e.kind, e.name, e.added_by, e.added_at
                                  FROM application_entries e
                                  WHERE e.application_id = a.id AND e.list = 'contributors'
                                  ORDER BY e.id LIMIT $3) f
                            LEFT JOIN users w ON f.kind = 'user' AND w.id = f.name
                         ) c
                         CROSS JOIN LATERAL (
                            SELECT coalesce(bool_or(e.kind = 'everyone'), false) AS everyone,
                                   count(*) FILTER (WHERE e.kind = 'group') AS groups,
                                   count(*) FILTER (WHERE e.kind = 'user') AS people
                            FROM application_entries e
                            WHERE e.application_id = a.id AND e.list = 'shared_with'
                         ) s
                         CROSS JOIN LATERAL (
                            SELECT coalesce(sum(g.requests), 0)::bigint AS requests,
                                   count(DISTINCT g.user_id) AS users
                            FROM application_usage g
                            WHERE g.application = a.name AND g.day >= p.from_day
                         ) u
                         WHERE $1::text IS NULL OR a.name COLLATE \"C\" > $1::text COLLATE \"C\"
                         ORDER BY a.name COLLATE \"C\"
                         LIMIT $2"
                    ),
                    &[&after, &limit, &MAX_CONTRIBUTORS],
                )
                .await
                .map_err(DbError::from)?;
            transaction.commit().await.map_err(DbError::from)?;
            Ok(rows)
        });
        let rows = match tokio::time::timeout(limit_time, read).await {
            Ok(rows) => rows?,
            Err(_) => return Err(DbError::timed_out("reading every Application", limit_time)),
        };
        rows.iter().map(admin_application_of).collect()
    }
}

/// `SET LOCAL` of a statement and a lock timeout of `limit`, for a
/// transaction whose statements Postgres must end then.
pub(crate) fn statement_timeout(limit: std::time::Duration) -> String {
    let ms = limit.as_millis();
    format!("SET LOCAL statement_timeout = {ms}; SET LOCAL lock_timeout = {ms}")
}

/// An Application as Platform administrators' list of every Application
/// has it ([`Registry::every_application`]).
#[derive(Debug)]
pub struct AdminApplication {
    pub name: String,
    pub description: Option<String>,
    pub created_by: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub serving_version: Option<u64>,
    pub latest_version: Option<u64>,
    /// The serving Version's Declared access as it was taken; `None` when
    /// no Version serves.
    pub declared_access: Option<Value>,
    /// In the order of the list.
    pub contributors: Vec<Entry>,
    pub everyone: bool,
    pub groups: i64,
    pub people: i64,
    pub disabled: Option<Disabled>,
    /// The first day usage is counted from (`YYYY-MM-DD`, UTC).
    pub usage_from: String,
    pub requests: i64,
    pub users: i64,
}

fn admin_application_of(row: &Row) -> Result<AdminApplication, DbError> {
    let kinds: Vec<&str> = get(row, 9)?;
    let names: Vec<String> = get(row, 10)?;
    let people: Vec<Option<String>> = get(row, 11)?;
    let emails: Vec<Option<String>> = get(row, 12)?;
    let added_by: Vec<String> = get(row, 13)?;
    let added_at: Vec<String> = get(row, 14)?;
    let mut contributors = Vec::with_capacity(kinds.len());
    for (((((kind, name), person), email), by), at) in kinds
        .into_iter()
        .zip(names)
        .zip(people)
        .zip(emails)
        .zip(added_by)
        .zip(added_at)
    {
        contributors.push(Entry {
            named: Named::of_stored(kind, name)?,
            name: person,
            email,
            added_by: by,
            added_at: at,
        });
    }
    Ok(AdminApplication {
        name: get(row, 0)?,
        description: get(row, 1)?,
        serving_version: get::<Option<i32>>(row, 2)?.map(number_of).transpose()?,
        created_by: get(row, 3)?,
        created_at: get(row, 4)?,
        latest_version: get::<Option<i32>>(row, 5)?.map(number_of).transpose()?,
        declared_access: get(row, 6)?,
        disabled: disabled_of(get(row, 7)?, get(row, 8)?),
        contributors,
        everyone: get(row, 15)?,
        groups: get(row, 16)?,
        people: get(row, 17)?,
        usage_from: get(row, 18)?,
        requests: get(row, 19)?,
        users: get(row, 20)?,
    })
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
                   a.lists_generation, ",
                rfc3339!("a.disabled_at"),
                ", a.disabled_by
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
        disabled: disabled_of(get(&row, 8)?, get(&row, 9)?),
    }))
}

/// A disable in force from its two columns, both set or both null.
fn disabled_of(since: Option<String>, by: Option<String>) -> Option<Disabled> {
    Some(Disabled {
        since: since?,
        by: by?,
    })
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

/// The serving Version of the Application `name` from the columns
/// `number, bundle, declared_access` from `at` on; `None` when `number` is
/// null (no Version serves, or it was not read).
fn serving_at(name: &str, row: &Row, at: usize) -> Result<Option<Serving>, DbError> {
    let Some(number) = get::<Option<i32>>(row, at)? else {
        return Ok(None);
    };
    let bundle: &str = get(row, at + 1)?;
    Ok(Some(Serving {
        application: Arc::from(name),
        version: number_of(number)?,
        bundle: Arc::from(bundle),
        declared: Arc::new(declared_of(&get::<Value>(row, at + 2)?)),
    }))
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
