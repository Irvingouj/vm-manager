//! The stand-ins of the pulled Resources (blackbox/CONTRACT.md, "The local
//! loop: What a run does", steps 4 and 5).
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

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use super::super::Ending;
use super::PgFailed;
use super::postgres::{CONNECT_WAIT, HOST, Secret};
use crate::platform_db::{Cause, Connection, DatabaseAddress, quote_identifier};

use super::super::pull::STANDIN_FIRST_LINE;

/// What is read of a `standin.sql`.
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
    pub files: Vec<StandinFile>,
    /// `stand-in-not-pulled` and the like.
    pub warnings: Vec<Value>,
}

/// The pulled Resources of `project`, in name order, each with its
/// `standin.sql` when the tool wrote one.
pub(super) fn pulled(project: &Path, command: &str) -> Pulled {
    let mut names: Vec<String> = fs::read_dir(project.join("resources"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| crate::resources::is_resource_name(name))
                .filter(|name| super::super::check::is_pulled(project, name))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut pulled = Pulled {
        files: Vec::new(),
        warnings: Vec::new(),
    };
    for name in names {
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

/// `text` as an SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
