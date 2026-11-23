//! The stand-ins of the pulled Resources (blackbox/CONTRACT.md, "The local
//! loop: What a run does", steps 4 and 5), and the local Storage of each
//! pulled Storage Resource ("Storage: The local loop").
//!
//! Each `resources/<name>/standin.sql` the tool wrote becomes a database of
//! the private Postgres, `standin_<16 hex digits of the SHA-256 of the
//! name>`, with three roles of its own: `<database>_owner` owns it and runs
//! the file's statements one by one (it may log in only while it does, is
//! no superuser, and owns nothing else), `<database>_read` may `SELECT` from
//! every table, and `<database>_write` may also `INSERT`, `UPDATE`,
//! `DELETE` and use the sequences. Neither login owns anything; both get a
//! new password at every run (never written but in the local platform
//! database, which is the private Postgres's own).
//!
//! The statements are split by Postgres's own scanner (`pg_query`), never by
//! cutting at `;`. The database's comment holds the fingerprint and the
//! SHA-256 of the file it was made from: when either differs, the stand-in
//! is dropped and made again from nothing. The superuser's session does the
//! rest (roles, the database, the privileges, the comment), and nothing
//! else.
//!
//! A pulled Storage Resource is the folder `.vibe/local/storage/<name>/` of
//! the project, made by the first run that needs it, marked and registered
//! through the installation's own registration ([`register_storages`]):
//! the same backend, rules and bounds. One no longer pulled has its folder
//! removed.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use super::super::Ending;
use super::PgFailed;
use super::postgres::{CONNECT_WAIT, HOST, Secret};
use crate::platform_db::{Cause, Connection, DatabaseAddress, quote_identifier};
use crate::resources::{NewReach, NewResource, Resources};
use crate::storage::directory::{MARKER, STAGING};
use crate::storage::{Directory, Location, Root, Storage};

use super::super::pull::STANDIN_FIRST_LINE;
use super::super::test_users::BUILDER;

/// What is read of a `standin.sql`, and of a `storage.json`.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// How long one statement of the set-up or of a stand-in may take.
const STATEMENT_WAIT: Duration = Duration::from_secs(30);

/// A pulled Resource's `standin.sql`, read.
pub(super) struct StandinFile {
    pub resource: String,
    pub contact: String,
    /// The fingerprint and the SHA-256 of the file: what the database is
    /// marked with.
    pub mark: String,
    pub text: String,
}

/// What the pulled Resources are.
pub(super) struct Pulled {
    /// The pulled databases, each with its `standin.sql`.
    pub files: Vec<StandinFile>,
    /// The pulled Storage Resources, each with its `storage.json`.
    pub storages: Vec<StorageFile>,
    /// The Storage Resources whose `storage.json` is there but is not the
    /// tool's (unreadable, not generated, a link): their folders are kept
    /// and they are not registered for the run.
    pub unusable: Vec<String>,
    /// `stand-in-not-pulled` and the like.
    pub warnings: Vec<Value>,
}

/// A pulled Storage Resource's `storage.json`, read.
pub(super) struct StorageFile {
    pub resource: String,
    pub contact: String,
}

/// The names in `resources/` that can be Resources, in name order: the
/// whole listing, as the local run read it before Storage (a cap here would
/// leave a pulled Resource silently without its stand-in), and the one
/// listing both [`pulled`] and [`has_storage_pull`] use, so that they agree.
fn resource_folders(project: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(project.join("resources"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| crate::resources::is_resource_name(name))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether any of those names has a `storage.json`: the local Storage's
/// root is built only then, so that a project with no pulled Storage
/// Resource is never refused for its path.
pub(super) fn has_storage_pull(project: &Path) -> bool {
    resource_folders(project)
        .iter()
        .any(|name| has_storage_file(project, name))
}

/// The pulled Resources of `project`, in name order: each database with its
/// `standin.sql` when the tool wrote one, each Storage Resource with its
/// `storage.json`. A folder holding both kinds' files is taken as the kind
/// the Manifest declares for it (`kinds`), as the contract says.
pub(super) fn pulled(project: &Path, kinds: &BTreeMap<String, String>, command: &str) -> Pulled {
    let names = resource_folders(project);
    let mut pulled = Pulled {
        files: Vec::new(),
        storages: Vec::new(),
        unusable: Vec::new(),
        warnings: Vec::new(),
    };
    for name in names {
        // The kind the Manifest declares decides a folder holding both
        // kinds' files; a folder it does not declare is a Storage Resource
        // when its `storage.json` is there, and a database otherwise.
        let storage = match kinds.get(&name).map(String::as_str) {
            Some("storage") => true,
            Some(_) => false,
            None => has_storage_file(project, &name),
        };
        if storage {
            match storage_state(project, &name) {
                // Not pulled: its folder is removed by this run.
                StorageState::Missing => {}
                StorageState::Unusable => {
                    pulled.unusable.push(name.clone());
                    pulled.warnings.push(json!({
                        "reason": "storage-file-unusable",
                        "resource": name,
                        "message": format!(
                            "resources/{name}/storage.json is there and is not a file the tool wrote (it cannot be read, does not hold the tool's `generated`, or is a link), so the local run has no Storage Resource {name} this time: its folder is left as it is. Run vibe pull {name} again once the file is right, then {command} again"
                        ),
                    }));
                }
                StorageState::Ready(file) => pulled.storages.push(file),
            }
            continue;
        }
        if !super::super::check::is_pulled(project, &name) {
            continue;
        }
        match read(project, &name) {
            Some(file) => pulled.files.push(file),
            None => pulled.warnings.push(json!({
                "reason": "stand-in-not-pulled",
                "resource": name,
                "message": format!(
                    "{name} was pulled before the local run could make a stand-in of it (it has no resources/{name}/standin.sql the tool wrote): it has no stand-in, so its operations are refused not-registered. Run vibe pull {name} again, then {command} again"
                ),
            })),
        }
    }
    pulled
}

/// The names of the pulled Resources of `project`, by their files (a
/// folder is one name), for the page that shows each pulled Resource's
/// access; the Manifest is not read there.
pub(super) fn pulled_names(
    project: &Path,
    kinds: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let pulled = pulled(project, kinds, "");
    let mut names: Vec<(String, String)> = pulled
        .files
        .into_iter()
        .map(|file| (file.resource, "postgres".to_owned()))
        .collect();
    names.extend(
        pulled
            .storages
            .into_iter()
            .map(|file| (file.resource, "storage".to_owned())),
    );
    names.sort();
    names
}

/// Whether `resources/<name>/storage.json` is there at all, a link
/// included: what tells a Storage pull in a folder the Manifest does not
/// declare.
fn has_storage_file(project: &Path, name: &str) -> bool {
    fs::symlink_metadata(project.join("resources").join(name).join("storage.json")).is_ok()
}

/// How a Storage Resource's `storage.json` stands.
enum StorageState {
    /// Not there: the Resource is no longer pulled.
    Missing,
    /// There, but not the file the tool wrote: a warning, the folder kept.
    Unusable,
    Ready(StorageFile),
}

fn storage_state(project: &Path, name: &str) -> StorageState {
    let path = project.join("resources").join(name).join("storage.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return StorageState::Missing,
        // A link is not a file the tool wrote, and is never followed.
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return StorageState::Unusable;
        }
        Ok(_) => {}
        Err(_) => return StorageState::Unusable,
    }
    match read_storage(project, name) {
        Some(file) => StorageState::Ready(file),
        None => StorageState::Unusable,
    }
}

/// `resources/<name>/storage.json`, when the tool wrote it: the tool's
/// `generated`, this Resource's name and a contact, from a regular file no
/// link leads to.
fn read_storage(project: &Path, name: &str) -> Option<StorageFile> {
    let path = project.join("resources").join(name).join("storage.json");
    // Bounded whatever the file says it holds; no link is followed (one
    // swapped in after the check only reads bytes, never a folder).
    let file = fs::File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > super::super::pull::STORAGE_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(super::super::pull::STORAGE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n > super::super::pull::STORAGE_FILE_BYTES) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let body: Value = serde_json::from_str(&text).ok()?;
    if body.get("generated").and_then(Value::as_str) != Some(super::super::pull::STORAGE_GENERATED)
    {
        return None;
    }
    if body.get("resource").and_then(Value::as_str) != Some(name) {
        return None;
    }
    Some(StorageFile {
        resource: name.to_owned(),
        contact: body.get("contact").and_then(Value::as_str)?.to_owned(),
    })
}

/// `resources/<name>/standin.sql`, when the tool wrote it.
fn read(project: &Path, name: &str) -> Option<StandinFile> {
    let path = project.join("resources").join(name).join("standin.sql");
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != STANDIN_FIRST_LINE {
        return None;
    }
    let fingerprint = lines
        .next()?
        .split_once("Fingerprint: ")
        .map(|(_, f)| f.trim().to_owned())?;
    let contact = lines.next()?.strip_prefix("-- Contact: ")?.to_owned();
    let digest: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(StandinFile {
        resource: name.to_owned(),
        contact,
        mark: format!("{fingerprint} {digest}"),
        text,
    })
}

/// A statement of a `standin.sql`: its text and its first line as
/// written (with its `;` when the statement is one line).
pub(super) struct Statement<'a> {
    pub text: &'a str,
    pub first_line: &'a str,
}

/// The statements of `text`, as Postgres's scanner splits them; `None`
/// when it cannot.
pub(super) fn statements(text: &str) -> Option<Vec<Statement<'_>>> {
    let pieces = pg_query::split_with_scanner(text).ok()?;
    let mut found = Vec::with_capacity(pieces.len());
    let mut from = 0;
    for piece in pieces {
        let at = text
            .get(from..)
            .and_then(|rest| rest.find(piece))
            .map_or(from, |i| from.saturating_add(i));
        from = at.saturating_add(piece.len());
        // The first line that is not empty and not a comment, as it is
        // in the file: to the end of that line.
        let mut start = at;
        for line in piece.split_inclusive('\n') {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("--") {
                start = start.saturating_add(line.len());
            } else {
                break;
            }
        }
        let first_line = text
            .get(start..)
            .and_then(|rest| rest.lines().next())
            .unwrap_or_default()
            .trim_end();
        if first_line.is_empty() {
            continue;
        }
        found.push(Statement {
            text: piece,
            first_line,
        });
    }
    Some(found)
}

/// A stand-in, ready: what its Resource is registered with.
pub(super) struct Standin {
    pub resource: String,
    pub contact: String,
    pub database: String,
    pub read: (String, Secret),
    pub write: (String, Secret),
    pub made: bool,
}

/// The name of the stand-in database of `resource`.
fn database_of(resource: &str) -> String {
    let digest = Sha256::digest(resource.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("standin_{hex}")
}

/// Why a statement of the superuser's or the owner's did not run: its
/// cause, and the database's own message when it gave one.
struct Refused {
    cause: Cause,
    said: Option<String>,
}

impl Refused {
    fn of(error: &tokio_postgres::Error) -> Self {
        Self {
            cause: Cause::of(error),
            said: Some(database_message(error)),
        }
    }

    fn timed_out() -> Self {
        Self {
            cause: Cause::TimedOut(STATEMENT_WAIT),
            said: None,
        }
    }

    /// In words, for a warning.
    fn words(&self) -> String {
        match (&self.said, &self.cause) {
            (Some(said), _) => said.clone(),
            (None, Cause::TimedOut(limit)) => format!(
                "it did not end within {} s",
                crate::platform_db::seconds_text(*limit)
            ),
            (None, cause) => cause.name().to_owned(),
        }
    }

    /// The local run's failure at `step` (`what`, in words).
    fn failed(self, step: &'static str, what: String) -> PgFailed {
        let failed = PgFailed::new(step, what, self.cause);
        match self.said {
            Some(said) => failed.said(said),
            None => failed,
        }
    }
}

async fn run(client: &Client, statement: &str) -> Result<(), Refused> {
    match tokio::time::timeout(STATEMENT_WAIT, client.batch_execute(statement)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(Refused::of(&error)),
        Err(_) => Err(Refused::timed_out()),
    }
}

/// Why a stand-in could not be made: the private Postgres's failure, or no
/// randomness for a password.
enum Stop {
    Pg(PgFailed),
    NoRandomness(String),
}

impl From<PgFailed> for Stop {
    fn from(failed: PgFailed) -> Self {
        Self::Pg(failed)
    }
}

impl Stop {
    fn ending(&self, resource: &str, command: &str) -> Ending {
        match self {
            Self::Pg(failed) => failed.ending(command),
            Self::NoRandomness(why) => Ending::new(
                1,
                "failed",
                format!("the stand-in of {resource} cannot be made: {why}. Run {command} again"),
            ),
        }
    }
}

/// The database's own message of `error`.
fn database_message(error: &tokio_postgres::Error) -> String {
    error
        .as_db_error()
        .map_or_else(|| error.to_string(), |db| db.message().to_owned())
}

/// Makes (when its mark differs) and readies the stand-in of `file`; its
/// warnings go to `warnings`. `superuser` is a session of the private
/// Postgres's superuser on its maintenance database; `address` that
/// session's address (another database is reached with it). The steps of
/// a stand-in (`stand-in-...`) are named where they are made.
pub(super) async fn ready(
    file: &StandinFile,
    superuser: &Client,
    address: &DatabaseAddress,
    warnings: &mut Vec<Value>,
    command: &str,
) -> Result<Standin, Ending> {
    ready_or_stop(file, superuser, address, warnings)
        .await
        .map_err(|stop| stop.ending(&file.resource, command))
}

async fn ready_or_stop(
    file: &StandinFile,
    superuser: &Client,
    address: &DatabaseAddress,
    warnings: &mut Vec<Value>,
) -> Result<Standin, Stop> {
    let database = database_of(&file.resource);
    let owner = format!("{database}_owner");
    let reader = format!("{database}_read");
    let writer = format!("{database}_write");
    let quoted = |name: &str| quote_identifier(name);
    let of = |what: &str| format!("{what} of the stand-in of {}", file.resource);
    let marked = tokio::time::timeout(
        STATEMENT_WAIT,
        superuser.query_opt(
            "SELECT shobj_description(oid, 'pg_database') FROM pg_catalog.pg_database WHERE datname = $1",
            &[&database],
        ),
    )
    .await
    .map_err(|_| Refused::timed_out().failed("stand-in-mark-read", of("reading the mark")))?
    .map_err(|e| Refused::of(&e).failed("stand-in-mark-read", of("reading the mark")))?;
    let mark: Option<String> = marked.and_then(|row| row.try_get(0).ok());
    let made = mark.as_deref() != Some(file.mark.as_str());
    for role in [&owner, &reader, &writer] {
        run(
            superuser,
            &format!(
                "DO $$ BEGIN
                   IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = {literal}) THEN
                     CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT;
                   END IF;
                 END $$",
                literal = literal(role),
                role = quoted(role),
            ),
        )
        .await
        .map_err(|refused| refused.failed("stand-in-roles", of("making the roles")))?;
    }
    if made {
        for statement in [
            format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", quoted(&database)),
            format!(
                "CREATE DATABASE {} OWNER {} TEMPLATE template0 ENCODING 'UTF8'",
                quoted(&database),
                quoted(&owner)
            ),
            format!("REVOKE ALL ON DATABASE {} FROM PUBLIC", quoted(&database)),
            format!(
                "GRANT CONNECT ON DATABASE {} TO {}, {}",
                quoted(&database),
                quoted(&reader),
                quoted(&writer)
            ),
        ] {
            run(superuser, &statement)
                .await
                .map_err(|refused| refused.failed("stand-in-create", of("making the database")))?;
        }
        make(file, &database, &owner, superuser, address, warnings).await?;
    }
    let read = Secret::generate().map_err(Stop::NoRandomness)?;
    let write = Secret::generate().map_err(Stop::NoRandomness)?;
    for (role, secret) in [(&reader, &read), (&writer, &write)] {
        run(
            superuser,
            &format!(
                "ALTER ROLE {} LOGIN PASSWORD {}",
                quoted(role),
                literal(&secret.verifier())
            ),
        )
        .await
        .map_err(|refused| {
            refused.failed(
                "stand-in-logins",
                of("giving their passwords to the logins"),
            )
        })?;
    }
    Ok(Standin {
        resource: file.resource.clone(),
        contact: file.contact.clone(),
        database,
        read: (reader, read),
        write: (writer, write),
        made,
    })
}

/// Runs the file's statements in the new database as its owner, then gives
/// the logins their privileges and marks the database.
async fn make(
    file: &StandinFile,
    database: &str,
    owner: &str,
    superuser: &Client,
    address: &DatabaseAddress,
    warnings: &mut Vec<Value>,
) -> Result<(), Stop> {
    let of = |what: &str| format!("{what} of the stand-in of {}", file.resource);
    let quoted = |name: &str| quote_identifier(name);
    let secret = Secret::generate().map_err(Stop::NoRandomness)?;
    run(
        superuser,
        &format!(
            "ALTER ROLE {} LOGIN PASSWORD {}",
            quoted(owner),
            literal(&secret.verifier())
        ),
    )
    .await
    .map_err(|refused| refused.failed("stand-in-owner-login", of("letting in the owner")))?;
    let as_owner = DatabaseAddress::new(HOST, address.port(), owner, secret.copy(), database);
    let made = async {
        let connection = Connection::open(&as_owner.config(CONNECT_WAIT), CONNECT_WAIT)
            .await
            .map_err(|e| PgFailed::db("stand-in-owner-connect", of("connecting as the owner"), &e))?;
        let client = connection.client();
        let Some(list) = statements(&file.text) else {
            warnings.push(json!({
                "reason": "stand-in-incomplete",
                "resource": file.resource,
                "statement": "",
                "error": "Postgres's scanner cannot split the file into statements",
                "message": format!(
                    "resources/{}/standin.sql cannot be split into statements, so its stand-in is empty: run vibe pull {} again",
                    file.resource, file.resource
                ),
            }));
            return Ok(());
        };
        for statement in list {
            if let Err(refused) = run(client, statement.text).await {
                let error = refused.words();
                warnings.push(json!({
                    "reason": "stand-in-incomplete",
                    "resource": file.resource,
                    "statement": statement.first_line,
                    "error": error,
                    "message": format!(
                        "A statement of resources/{}/standin.sql cannot run in the local Postgres, so it was left out of the stand-in ({error}): {}",
                        file.resource, statement.first_line
                    ),
                }));
            }
        }
        // The logins' privileges, given by the owner of what they reach.
        let grants = format!(
            "DO $$ DECLARE s text; BEGIN
               FOR s IN SELECT nspname FROM pg_catalog.pg_namespace
                        WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' LOOP
                 EXECUTE format('GRANT USAGE ON SCHEMA %I TO {reader}, {writer}', s);
                 EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA %I TO {reader}, {writer}', s);
                 EXECUTE format('GRANT INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO {writer}', s);
                 EXECUTE format('GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA %I TO {writer}', s);
               END LOOP;
             END $$",
            reader = quoted(&format!("{database}_read")).replace('\'', "''"),
            writer = quoted(&format!("{database}_write")).replace('\'', "''"),
        );
        run(client, &grants).await.map_err(|refused| {
            refused.failed(
                "stand-in-privileges",
                of("giving their privileges to the logins"),
            )
        })?;
        drop(connection);
        Ok::<(), PgFailed>(())
    }
    .await;
    // Whatever happened: the owner may not log in any more.
    let closed = run(
        superuser,
        &format!("ALTER ROLE {} NOLOGIN PASSWORD NULL", quoted(owner)),
    )
    .await;
    made?;
    closed.map_err(|refused| {
        refused.failed("stand-in-owner-close", of("closing the owner's login"))
    })?;
    run(
        superuser,
        &format!(
            "COMMENT ON DATABASE {} IS {}",
            quoted(database),
            literal(&file.mark)
        ),
    )
    .await
    .map_err(|refused| Stop::Pg(refused.failed("stand-in-mark", of("marking the database"))))
}

/// Removes every stand-in database, and every stand-in role, that no pulled
/// Resource has now (its folder was removed). Only names a stand-in has
/// are touched: `standin_` and 16 lower-case hex digits, and that with
/// `_owner`, `_read` or `_write`; any other database or role is left as it
/// is. `kept` are the stand-in databases of the pulled Resources.
pub(super) async fn remove_others(superuser: &Client, kept: &[&str]) -> Result<(), PgFailed> {
    let failed = |refused: Refused| {
        refused.failed(
            "stand-in-remove",
            "removing the stand-ins no longer pulled".to_owned(),
        )
    };
    let listed = |query: &'static str| async move {
        tokio::time::timeout(STATEMENT_WAIT, superuser.query(query, &[]))
            .await
            .map_err(|_| failed(Refused::timed_out()))?
            .map_err(|e| failed(Refused::of(&e)))
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.try_get::<_, String>(0).ok())
                    .collect::<Vec<String>>()
            })
    };
    let databases = listed(
        "SELECT datname FROM pg_catalog.pg_database WHERE datname ~ '^standin_[0-9a-f]{16}$'",
    )
    .await?;
    for database in databases.iter().filter(|d| !kept.contains(&d.as_str())) {
        if is_standin_database(database) {
            run(
                superuser,
                &format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    quote_identifier(database)
                ),
            )
            .await
            .map_err(failed)?;
        }
    }
    let roles = listed(
        "SELECT rolname FROM pg_catalog.pg_roles WHERE rolname ~ '^standin_[0-9a-f]{16}_(owner|read|write)$'",
    )
    .await?;
    for role in &roles {
        let database = role.rsplit_once('_').map_or("", |(d, _)| d);
        if is_standin_database(database) && !kept.contains(&database) {
            run(
                superuser,
                &format!("DROP ROLE IF EXISTS {}", quote_identifier(role)),
            )
            .await
            .map_err(failed)?;
        }
    }
    Ok(())
}

/// Whether `name` is a stand-in database's: `standin_` and 16 lower-case
/// hex digits.
fn is_standin_database(name: &str) -> bool {
    name.strip_prefix("standin_").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// How long the local state's folders may take to be looked at, made,
/// emptied or removed, off the async worker; past it the run ends
/// `local-state` (the blocking work goes on to its end on its own thread,
/// which holds nothing the run waits for).
const STATE_WAIT: Duration = Duration::from_secs(30);

/// The pulled Storage Resources' folders made (with `fresh`, emptied but
/// for their marks, as `vibe local reset` says), the folders of those no
/// longer pulled removed, then each usable one registered through the
/// installation's own registration. A Resource whose `storage.json` is
/// there but not the tool's ([`Pulled::unusable`]) keeps its folder and is
/// not registered. `made` says whether this run made the folder because it
/// was not there, by Resource name, for the Resources registered.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts one bring-up registers its local Storage from, each named, once a run"
)]
pub(super) async fn register_storages(
    project: &Path,
    root: Option<&Arc<Root>>,
    storage: &Storage,
    resources: &Arc<Resources>,
    pulled: &[StorageFile],
    unusable: &[String],
    fresh: bool,
    command: &str,
) -> Result<Vec<(String, bool)>, Ending> {
    // Copied: the plan owns its names on its own thread; once a bring-up.
    let make: Vec<String> = pulled.iter().map(|file| file.resource.clone()).collect();
    let keep: Vec<String> = unusable.to_vec();
    // Set when the wait ends, or the run is dropped: the work stops before
    // its next step (blackbox/CONTRACT.md, "Storage: The local loop": a
    // link ... and what a `local-state` timeout may say).
    let stopped = Arc::new(AtomicBool::new(false));
    let guard = StopWhenDropped(Arc::clone(&stopped));
    let planned = {
        let project = project.to_path_buf();
        // Copied: the work owns its words on its own thread; once a
        // bring-up.
        let words = command.to_owned();
        let worker = Arc::clone(&stopped);
        let timed = tokio::time::timeout(
            STATE_WAIT,
            super::run::off_thread(move || plan(&project, make, &keep, fresh, &words, &worker)),
        )
        .await;
        match timed {
            Ok(Some(Ok(Some(planned)))) => planned,
            // The work stopped at a step boundary: nothing more is made,
            // emptied or removed.
            Ok(Some(Ok(None))) => return Err(state_stopped(command)),
            Ok(Some(Err(ending))) => return Err(ending),
            Ok(None) => return Err(super::run::read_failed(command)),
            Err(_) => {
                stopped.store(true, Ordering::SeqCst);
                return Err(state_stopped(command));
            }
        }
    };
    drop(guard);
    let planned: BTreeMap<String, (PathBuf, bool)> = planned
        .into_iter()
        .map(|(name, folder, made)| (name, (folder, made)))
        .collect();
    let mut made = Vec::with_capacity(pulled.len());
    for file in pulled {
        let Some((folder, was_made)) = planned.get(&file.resource) else {
            // Not planned is not a folder: plan made every usable one.
            return Err(super::run::read_failed(command));
        };
        let Some(root) = root else {
            // The root is built when a `storage.json` is there; a pulled
            // one without it cannot happen.
            return Err(super::run::read_failed(command));
        };
        let directory = folder.to_string_lossy().into_owned();
        let taken = Arc::clone(resources);
        let prepared = match root
            .prepare(&storage.files, &file.resource, &directory, move || {
                taken.storage_taken()
            })
            .await
        {
            Ok(prepared) => prepared,
            Err(refused) => {
                return Err(Ending::new(
                    1,
                    "local-storage",
                    format!(
                        "the local Storage of {} (.vibe/local/storage/{}/) cannot be used ({}): {}. Remove that folder if it is in the way, then run {command} again",
                        file.resource,
                        file.resource,
                        refused.reason(),
                        refused.message()
                    ),
                ));
            }
        };
        let backend = Directory::registered(
            &file.resource,
            &prepared.directory,
            Arc::clone(&prepared.opened),
            Some(Arc::clone(root)),
            &storage.files,
        );
        let new = NewResource {
            name: file.resource.clone(),
            description: format!("The local Storage of {}", file.resource),
            owner: "the local run".to_owned(),
            contact: file.contact.clone(),
            reach: NewReach::Storage {
                location: Location::Directory {
                    directory: prepared.directory.to_string_lossy().into_owned(),
                },
                backend,
            },
        };
        match resources.register(new, BUILDER).await {
            Ok(_) => {
                root.release(prepared.claim());
                made.push((file.resource.clone(), *was_made));
            }
            Err(error) => {
                root.undo(&storage.files, prepared).await;
                return Err(super::database_failed(
                    "local-storage",
                    "registering the local Storage Resources",
                    &error,
                    command,
                ));
            }
        }
    }
    Ok(made)
}

/// Sets the work's flag when it is dropped: the work stops before its next
/// step, whether the wait ended or the run was dropped.
struct StopWhenDropped(Arc<AtomicBool>);

impl Drop for StopWhenDropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The ending of a wait for the local Storage that ended before the work
/// did: the work stops before its next step, and a step already under way
/// may still finish.
fn state_stopped(command: &str) -> Ending {
    Ending::new(
        1,
        "local-state",
        format!(
            "The project's local Storage (.vibe/local/storage/) did not answer within {} s, so the work was stopped: it stops before its next step, and a step already under way (a folder being made, emptied or removed) may still finish. Look at its disk (a network mount gone?), then run {command} again and check that nothing is left half-done",
            STATE_WAIT.as_secs()
        ),
    )
}

/// One Storage Resource's folder, planned: its name, its path, and whether
/// this run makes it because it was not there.
type Planned = (String, PathBuf, bool);

/// The local state's Storage folders, off the async worker: the `make`
/// ones made (and, with `fresh`, emptied but for their marks), the `keep`
/// ones left as they are (a `storage.json` that is not the tool's), and
/// every other folder under the root removed. No link is followed at
/// `.vibe`, `.vibe/local`, `.vibe/local/storage` or a Resource's folder:
/// one ends the run `local-state`, naming it, before anything is read,
/// made or removed. `.vibe/local/storage/` is not made when nothing is to
/// be made or kept and it is not there. `None` when the work stopped at a
/// step boundary, because the wait ended.
fn plan(
    project: &Path,
    make: Vec<String>,
    keep: &[String],
    fresh: bool,
    command: &str,
    stopped: &AtomicBool,
) -> Result<Option<Vec<Planned>>, Ending> {
    let stop = || stopped.load(Ordering::Relaxed);
    let state = project.join(".vibe");
    let local = state.join("local");
    let root = local.join("storage");
    // The state's own folders, each looked at itself.
    for at in [&state, &local, &root] {
        if super::link_at(at) {
            return Err(super::link_in_local_state(
                &super::shown_path(project, at),
                command,
            ));
        }
    }
    // Each Storage Resource's folder too, before anything of the state is
    // read, made or removed.
    for name in make.iter().chain(keep.iter()) {
        let folder = root.join(name);
        if super::link_at(&folder) {
            return Err(super::link_in_local_state(
                &super::shown_path(project, &folder),
                command,
            ));
        }
    }
    // Nothing pulled, nothing kept, and no root: nothing to do, and no
    // `.vibe/local/storage/` is made for a project without one.
    let root_is_dir = fs::symlink_metadata(&root).is_ok_and(|meta| meta.is_dir());
    if make.is_empty() && keep.is_empty() && !root_is_dir {
        return Ok(Some(Vec::new()));
    }
    let failed = |what: &str, error: std::io::Error| {
        Ending::new(
            1,
            "local-state",
            format!(
                "The project's local Storage (.vibe/local/storage/) cannot be used: {what} ({error}). Removing the folder .vibe/local/ starts the local databases and the local Storage over (their test rows and files are lost); then run {command} again"
            ),
        )
    };
    // Looked at before the first step that reads or makes anything.
    if stop() {
        return Ok(None);
    }
    fs::create_dir_all(&root).map_err(|e| failed("its folder cannot be made", e))?;
    let listing = fs::read_dir(&root).map_err(|e| failed("its folders cannot be read", e))?;
    for entry in listing.flatten() {
        if stop() {
            return Ok(None);
        }
        let name = entry.file_name().into_string().ok();
        let ours = name
            .as_ref()
            .is_some_and(|name| make.contains(name) || keep.contains(name));
        if ours {
            continue;
        }
        let path = entry.path();
        // Looked at itself: a link is not a folder of the platform's, and
        // is left alone (nothing is removed through it).
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        // The root re-checked before each removal: one swapped for a link
        // after the check above is not removed through. (The window that
        // is left: a link swapped in between this look and the removal.)
        if super::link_at(&root) {
            return Err(super::link_in_local_state(
                &super::shown_path(project, &root),
                command,
            ));
        }
        fs::remove_dir_all(&path).map_err(|e| {
            failed(
                "the folder of a Storage Resource no longer pulled cannot be removed",
                e,
            )
        })?;
    }
    let mut planned = Vec::with_capacity(make.len());
    for name in make {
        if stop() {
            return Ok(None);
        }
        let folder = root.join(&name);
        // Re-checked just before it is made or emptied, the root too (a
        // link swapped in is not made through).
        if super::link_at(&root) {
            return Err(super::link_in_local_state(
                &super::shown_path(project, &root),
                command,
            ));
        }
        if super::link_at(&folder) {
            return Err(super::link_in_local_state(
                &super::shown_path(project, &folder),
                command,
            ));
        }
        let existed = fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir());
        fs::create_dir_all(&folder)
            .map_err(|e| failed("a Storage Resource's folder cannot be made", e))?;
        if fresh && !empty(project, &folder, command, stopped)? {
            return Ok(None);
        }
        // `made` is true only when this run made the folder because it was
        // not there; a reset empties one that was there and keeps it.
        planned.push((name, folder, !existed));
    }
    Ok(Some(planned))
}

/// Empties `folder`, its own marks ([`MARKER`] and [`STAGING`]) kept and no
/// link followed: what `vibe local reset` does to a local Storage. `false`
/// when the work stopped at a step boundary, because the wait ended.
fn empty(
    project: &Path,
    folder: &Path,
    command: &str,
    stopped: &AtomicBool,
) -> Result<bool, Ending> {
    let stop = || stopped.load(Ordering::Relaxed);
    let shown = super::shown_path(project, folder);
    let failed = |error: std::io::Error| {
        Ending::new(
            1,
            "local-state",
            format!(
                "{shown} cannot be emptied ({error}): give it back to yourself on the host, or remove it, then run {command} again"
            ),
        )
    };
    // A link at the folder, at the root above it, or swapped in since they
    // were checked, is not followed.
    let root = project.join(".vibe").join("local").join("storage");
    let root_shown = super::shown_path(project, &root);
    if super::link_at(&root) {
        return Err(super::link_in_local_state(&root_shown, command));
    }
    if super::link_at(folder) {
        return Err(super::link_in_local_state(&shown, command));
    }
    let listing = fs::read_dir(folder).map_err(failed)?;
    for entry in listing.flatten() {
        if stop() {
            return Ok(false);
        }
        let name = entry.file_name();
        if name == MARKER || name == STAGING {
            continue;
        }
        // Each removal re-checks the folder and the root (the window left:
        // one swapped in between this look and the removal).
        if super::link_at(&root) {
            return Err(super::link_in_local_state(&root_shown, command));
        }
        if super::link_at(folder) {
            return Err(super::link_in_local_state(&shown, command));
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&path).map_err(failed)?,
            Ok(_) => fs::remove_file(&path).map_err(failed)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(error)),
        }
    }
    Ok(true)
}

/// `text` as an SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
