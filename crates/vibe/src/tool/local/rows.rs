//! Test rows (blackbox/CONTRACT.md, "The local loop, kept running: Test
//! rows"): `test-rows/<resource>.sql` loaded into the stand-in of a pulled
//! Resource, `test-rows/own-database.sql` into the local own database.
//!
//! **What was loaded last** is kept in the private Postgres itself, in the
//! table [`TABLE`] of its maintenance database `postgres`, which only its
//! superuser reaches: for each target, the database it was loaded into (its
//! `oid`, so that a database made anew is told from the one before) and the
//! SHA-256 of the file loaded, or null when the target holds no file's rows
//! (emptied, or a loading that failed). Nothing of it is in the project.
//! One connection to that database serves a whole loading. A target is
//! remembered as holding no file's rows before it is emptied, so that the
//! record never says rows are there that are not.
//!
//! **Only files.** A `test-rows/<name>.sql` that is not a file (a link, a
//! folder) is `test-rows-failed`: no link is followed.
//!
//! **A stand-in.** Its tables are emptied and their sequences and
//! identities started again by the private Postgres's superuser, the role
//! that can (`TRUNCATE ... RESTART IDENTITY`): the write login gains no
//! right. Then the file's statements are run in order, in one transaction,
//! by the stand-in's write login, so that they can do what it can (insert,
//! update, delete, use the sequences) and nothing else.
//!
//! **The own database.** The file's statements are run in order in one
//! transaction of the runtime role (the role the Application's own
//! statements run as), through the executor's own pool, which attaches the
//! signed identity by the host's own means (`user_id` `test-rows`, `actor`
//! `application`, a `request` `test-rows-<uuid>`), so that the capture
//! records each row in the Change history; nothing goes through the
//! Operation gateway, so nothing is in the Operation log. When a file other
//! than the one loaded last is to be loaded into an own database that has
//! been loaded before, the own database is made anew first ([`remake`]).
//!
//! **Bounds.** A file is at most [`MAX_FILE_BYTES`]; each statement runs
//! at most [`STATEMENT_WAIT`] (a stand-in's: `statement_timeout` too; the
//! own database's: its pool's statement limit); one file's loading at most
//! [`LOAD_WAIT`]. A file is split by Postgres's own scanner, off the async
//! workers. No statement that begins, commits or ends a transaction is
//! run: the file is one transaction.

use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use tokio_postgres::SimpleQueryMessage;

use super::super::Ending;
use super::super::deploy;
use super::super::project_tools::Who;
use super::super::test_users::BUILDER;
use super::postgres::{CONNECT_WAIT, HOST};
use super::run::{
    Live, Pg, Said, migration_failed, migration_refused, off_thread, own_database_unavailable,
    read_failed,
};
use super::standins::{self, Standin};
use super::{PgFailed, database_failed};
use crate::executor::{Failure, Label, Level, Values};
use crate::own_db::{Migration, Problem};
use crate::platform_db::{Cause, Connection, DatabaseAddress, DbError, quote_identifier};

/// The folder of the project's test rows.
const FOLDER: &str = "test-rows";

/// The own database's file, in [`FOLDER`].
const OWN_FILE: &str = "own-database.sql";

/// The table of what was loaded last, in the maintenance database.
const TABLE: &str = "vm_local_test_rows";

/// The largest file loaded.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The longest one statement may take.
const STATEMENT_WAIT: Duration = Duration::from_secs(30);

/// The longest the loading of one file may take, its emptying included.
const LOAD_WAIT: Duration = Duration::from_secs(120);

/// The longest the shipping of the own database's changes is waited for
/// before it is made anew.
const SHIP_WAIT: Duration = Duration::from_secs(10);

/// The ending of a statement of step `step` (`what`, in words) that did
/// not end within `limit`, or that failed with `error`.
fn timed_out(step: &'static str, what: &str, limit: Duration, name: &str) -> Ending {
    PgFailed::timed_out(step, what, limit).ending(name)
}

fn refused(step: &'static str, what: &str, error: &tokio_postgres::Error, name: &str) -> Ending {
    PgFailed::postgres(step, what, error).ending(name)
}

/// Makes the table of what was loaded last, unless it is there, on
/// `memory` (the maintenance database, as the superuser). Step
/// `test-rows-table`.
async fn make_table(memory: &Connection, name: &str) -> Result<(), Ending> {
    const WHAT: &str = "making the table of the test rows loaded";
    let statement = format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (target text PRIMARY KEY, database oid NOT NULL, sha256 text)"
    );
    tokio::time::timeout(STATEMENT_WAIT, memory.client().batch_execute(&statement))
        .await
        .map_err(|_| timed_out("test-rows-table", WHAT, STATEMENT_WAIT, name))?
        .map_err(|e| refused("test-rows-table", WHAT, &e, name))
}

/// Where test rows are loaded.
pub(super) struct Target<'a> {
    pub project: &'a Path,
    pub pg: &'a Pg,
    pub live: &'a Live,
    pub application: &'a str,
    /// The project's migrations: an own database made anew gets them all.
    pub migrations: &'a [Migration],
    pub who: &'a Who,
}

/// What a loading did.
pub(super) struct Loaded {
    /// The files loaded, in name order, with the rows each changed.
    pub files: Vec<(String, u64)>,
    /// When the own database was made anew: every migration, applied
    /// again.
    pub remade: Option<Vec<String>>,
}

/// A file of test rows, read.
struct RowsFile {
    /// `test-rows/<name>.sql`, as it is named in the output.
    name: String,
    text: String,
    sha256: String,
}

/// What was loaded last into a target.
struct Last {
    database: u32,
    sha256: Option<String>,
}

/// Loads the test rows that changed: each stand-in's (of `standins`), then
/// the own database's. A file for a Resource that is not pulled, or for an
/// own database the Application does not have, is a note. One connection
/// to the maintenance database serves the whole loading.
pub(super) async fn load(
    target: &Target<'_>,
    standins: &[Standin],
    said: &mut Said,
) -> Result<Loaded, Ending> {
    let name = target.who.command;
    let (pulled, stems) = {
        // Copied: the reads own their path on their own thread; once a
        // loading.
        let project = target.project.to_path_buf();
        off_thread(move || (standins::pulled(&project, name), sql_files(&project)))
            .await
            .ok_or_else(|| read_failed(name))?
    };
    let stems = stems.map_err(|file| {
        failed(
            &file,
            "",
            "it is not a file (a link, or a folder): test rows are files of the project, and a link is not followed",
            name,
        )
    })?;
    let mut known: Vec<String> = pulled.files.iter().map(|f| f.resource.clone()).collect();
    known.extend(
        pulled
            .warnings
            .iter()
            .filter_map(|w| w["resource"].as_str().map(str::to_owned)),
    );
    let own_stem = OWN_FILE.trim_end_matches(".sql");
    for stem in &stems {
        if stem == own_stem {
            if !has_own_tables(target) {
                said.notes.push(json!({
                    "reason": "test-rows-without-own-database",
                    "file": format!("{FOLDER}/{OWN_FILE}"),
                    "message": format!(
                        "{FOLDER}/{OWN_FILE} holds rows for the own database, and {} has no tables of its own there (the project has no migration): it is not loaded. Describe the tables in src/db/schema.ts and run vibe generate to have them, or remove the file",
                        target.application
                    ),
                }));
            }
        } else if !known.contains(stem) {
            said.notes.push(json!({
                "reason": "test-rows-without-resource",
                "file": format!("{FOLDER}/{stem}.sql"),
                "resource": stem,
                "message": format!(
                    "{FOLDER}/{stem}.sql is for a Resource named {stem}, which is not pulled: it is not loaded. Run vibe pull {stem} to have its stand-in, or remove the file"
                ),
            }));
        }
    }
    let memory = open(
        &target.pg.superuser("postgres"),
        "test-rows-connect",
        "connecting as its superuser to load the test rows",
        name,
    )
    .await?;
    make_table(&memory, name).await?;
    let mut loaded = Loaded {
        files: Vec::new(),
        remade: None,
    };
    for standin in standins {
        let file = read_named(target.project, &format!("{}.sql", standin.resource), name).await?;
        let oid = database_oid(&memory, &standin.database, name).await?;
        let key = format!("resource:{}", standin.resource);
        let last = last(&memory, &key, name).await?;
        let fresh = last.as_ref().map(|l| l.database) != Some(oid);
        let last_sha = last.and_then(|l| l.sha256);
        match file {
            Some(file) if fresh || last_sha.as_deref() != Some(file.sha256.as_str()) => {
                // Remembered as empty first, then emptied: whatever fails
                // after, the record never says rows are there that are not,
                // and the next run loads the file again.
                remember(&memory, &key, oid, None, name).await?;
                empty(target.pg, &standin.database, name).await?;
                let rows = load_standin(target.pg, standin, &file, name).await?;
                remember(&memory, &key, oid, Some(&file.sha256), name).await?;
                loaded.files.push((file.name, rows));
            }
            Some(_) => {}
            None => {
                if fresh || last_sha.is_some() {
                    remember(&memory, &key, oid, None, name).await?;
                }
                if !fresh && last_sha.is_some() {
                    empty(target.pg, &standin.database, name).await?;
                }
            }
        }
    }
    if let Some(remade) = own_rows(target, &memory, &mut loaded).await? {
        loaded.remade = Some(remade);
    }
    drop(memory);
    loaded.files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(loaded)
}

/// Whether the Application has an own database to load rows into: one it
/// made tables in. Every Application is given its own database with its
/// first Version, but one whose project has no migration has nothing of
/// its own in it, so `own-database.sql` is for an own database it does not
/// have.
fn has_own_tables(target: &Target<'_>) -> bool {
    !target.migrations.is_empty() && target.live.own.has_own_database(target.application)
}

/// The own database's test rows; the migrations applied again when it was
/// made anew.
async fn own_rows(
    target: &Target<'_>,
    memory: &Connection,
    loaded: &mut Loaded,
) -> Result<Option<Vec<String>>, Ending> {
    let name = target.who.command;
    let live = target.live;
    if !has_own_tables(target) {
        return Ok(None);
    }
    let Some(number) = application_number(live, target.application, name).await? else {
        return Ok(None);
    };
    let database = live.own.database_name(number);
    let key = format!("own:{}", target.application);
    let file = read_named(target.project, OWN_FILE, name).await?;
    let oid = database_oid(memory, &database, name).await?;
    let last = last(memory, &key, name).await?;
    let fresh = last.as_ref().map(|l| l.database) != Some(oid);
    let last_sha = last.and_then(|l| l.sha256);
    let wanted = file.as_ref().map(|f| f.sha256.as_str());
    if !fresh && last_sha.as_deref() == wanted {
        return Ok(None);
    }
    let mut remade = None;
    let mut oid = oid;
    if !fresh {
        remade = Some(remake(target, memory, number, &database).await?);
        oid = database_oid(memory, &database, name).await?;
    }
    remember(memory, &key, oid, None, name).await?;
    if let Some(file) = file {
        let rows = load_own(target, &file).await?;
        remember(memory, &key, oid, Some(&file.sha256), name).await?;
        loaded.files.push((file.name, rows));
    }
    Ok(remade)
}

/// The stems of `test-rows/*.sql`, in name order. `Err` names the first
/// that is not a file (a link, a folder; no link is followed), or the
/// folder itself when it is a link.
fn sql_files(project: &Path) -> Result<Vec<String>, String> {
    let folder = project.join(FOLDER);
    match fs::symlink_metadata(&folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(meta) if meta.file_type().is_symlink() => return Err(FOLDER.to_owned()),
        _ => return Ok(Vec::new()),
    }
    let mut stems = Vec::new();
    let mut not_files = Vec::new();
    for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".sql") else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        // The entry's own type: a link is not followed.
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            stems.push(stem.to_owned());
        } else {
            not_files.push(format!("{FOLDER}/{file_name}"));
        }
    }
    not_files.sort();
    if let Some(first) = not_files.into_iter().next() {
        return Err(first);
    }
    stems.sort();
    Ok(stems)
}

/// `test-rows-failed` for `file`, beside `statement` (its first line) and
/// `error` (the database's message, or what is wrong).
fn failed(file: &str, statement: &str, error: &str, name: &str) -> Ending {
    let message = if statement.is_empty() {
        format!(
            "The test rows of {file} cannot be loaded: {error}. Nothing of the file was loaded. Correct it, then run {name} again"
        )
    } else {
        format!(
            "A statement of {file} failed, so nothing of the file was loaded: {error}. The statement: {statement}. Correct it (test rows can only insert, update and delete rows, as the Application could), then run {name} again"
        )
    };
    deploy::detailed_refusal(
        "test-rows-failed",
        &message,
        &[("file", file), ("statement", statement), ("error", error)],
    )
}

/// `test-rows/<file>`, when it is a file (a link is not followed), read
/// off the async workers; at most [`MAX_FILE_BYTES`].
async fn read_named(project: &Path, file: &str, command: &str) -> Result<Option<RowsFile>, Ending> {
    let path = project.join(FOLDER).join(file);
    let name = format!("{FOLDER}/{file}");
    let read = off_thread(move || -> Result<Option<Vec<u8>>, String> {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("it cannot be read ({error})")),
        };
        if !meta.is_file() {
            return Err(
                "it is not a file (a link, or a folder): test rows are files of the project, and a link is not followed"
                    .to_owned(),
            );
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "it is larger than {} MiB",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|f| {
                f.take(MAX_FILE_BYTES.saturating_add(1))
                    .read_to_end(&mut bytes)
            })
            .map_err(|error| format!("it cannot be read ({error})"))?;
        if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
            return Err(format!(
                "it is larger than {} MiB",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        Ok(Some(bytes))
    })
    .await
    .unwrap_or_else(|| Err("it could not be read".to_owned()));
    let bytes = match read {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(why) => return Err(failed(&name, "", &why, command)),
    };
    let text =
        String::from_utf8(bytes).map_err(|_| failed(&name, "", "it is not UTF-8 text", command))?;
    let sha256 = super::hex(&Sha256::digest(text.as_bytes()));
    Ok(Some(RowsFile { name, text, sha256 }))
}

/// A statement of a file, owned, as the scanner split it.
struct Owned {
    text: String,
    first_line: String,
    /// It changes rows: `INSERT`, `UPDATE`, `DELETE`, `MERGE`; its count is
    /// counted.
    counts: bool,
    /// It begins, commits or ends a transaction.
    transaction: bool,
}

/// The statements of `file`, split by Postgres's scanner off the async
/// workers.
async fn split(file: &RowsFile, name: &str) -> Result<Vec<Owned>, Ending> {
    // Copied: the work moves to a blocking thread; at most 16 MiB, once a
    // loading.
    let text = file.text.clone();
    let split = off_thread(move || {
        standins::statements(&text).map(|list| {
            list.into_iter()
                .map(|statement| {
                    let types = pg_query::parse(statement.text, 0)
                        .map(|parsed| {
                            parsed
                                .statement_types()
                                .iter()
                                .map(|t| (*t).to_owned())
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default();
                    Owned {
                        text: statement.text.to_owned(),
                        first_line: statement.first_line.to_owned(),
                        counts: types.iter().any(|t| {
                            matches!(
                                t.as_str(),
                                "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt"
                            )
                        }),
                        transaction: types.iter().any(|t| t == "TransactionStmt"),
                    }
                })
                .collect::<Vec<Owned>>()
        })
    })
    .await
    .flatten();
    let list = split.ok_or_else(|| {
        failed(
            &file.name,
            "",
            "Postgres's scanner cannot split it into statements",
            name,
        )
    })?;
    if let Some(statement) = list.iter().find(|s| s.transaction) {
        return Err(failed(
            &file.name,
            &statement.first_line,
            "a file of test rows is loaded in one transaction of its own: it may not begin, commit or end one",
            name,
        ));
    }
    Ok(list)
}

/// The `oid` of `database`. Step `test-rows-database`.
async fn database_oid(memory: &Connection, database: &str, name: &str) -> Result<u32, Ending> {
    const STEP: &str = "test-rows-database";
    const WHAT: &str = "finding the database the test rows are loaded into";
    let row = tokio::time::timeout(
        STATEMENT_WAIT,
        memory.client().query_opt(
            "SELECT oid FROM pg_catalog.pg_database WHERE datname = $1",
            &[&database],
        ),
    )
    .await
    .map_err(|_| timed_out(STEP, WHAT, STATEMENT_WAIT, name))?
    .map_err(|e| refused(STEP, WHAT, &e, name))?;
    row.and_then(|row| row.try_get::<_, u32>(0).ok())
        .ok_or_else(|| {
            PgFailed::new(STEP, WHAT, Cause::Server(None))
                .said(format!("the database {database} is not there"))
                .ending(name)
        })
}

/// A connection to `address`, at step `step` (`what`, in words).
async fn open(
    address: &DatabaseAddress,
    step: &'static str,
    what: &str,
    name: &str,
) -> Result<Connection, Ending> {
    Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT)
        .await
        .map_err(|e| database_failed(step, what, &e, name))
}

/// What was loaded last into `key`.
async fn last(memory: &Connection, key: &str, name: &str) -> Result<Option<Last>, Ending> {
    let row = tokio::time::timeout(
        STATEMENT_WAIT,
        memory.client().query_opt(
            &format!("SELECT database, sha256 FROM {TABLE} WHERE target = $1"),
            &[&key],
        ),
    )
    .await
    .map_err(|_| {
        timed_out(
            "test-rows-record",
            "reading the record of the test rows loaded",
            STATEMENT_WAIT,
            name,
        )
    })?
    .map_err(|e| {
        refused(
            "test-rows-record",
            "reading the record of the test rows loaded",
            &e,
            name,
        )
    })?;
    Ok(row.and_then(|row| {
        Some(Last {
            database: row.try_get(0).ok()?,
            sha256: row.try_get(1).ok()?,
        })
    }))
}

/// Remembers that `key`'s database `oid` holds the rows of the file of
/// `sha256` (none: no file's rows).
async fn remember(
    memory: &Connection,
    key: &str,
    oid: u32,
    sha256: Option<&str>,
    name: &str,
) -> Result<(), Ending> {
    tokio::time::timeout(
        STATEMENT_WAIT,
        memory.client().execute(
            &format!(
                "INSERT INTO {TABLE} (target, database, sha256) VALUES ($1, $2, $3)
                 ON CONFLICT (target) DO UPDATE SET database = EXCLUDED.database, sha256 = EXCLUDED.sha256"
            ),
            &[&key, &oid, &sha256],
        ),
    )
    .await
    .map_err(|_| timed_out("test-rows-record", "writing the record of the test rows loaded", STATEMENT_WAIT, name))?
    .map_err(|e| refused("test-rows-record", "writing the record of the test rows loaded", &e, name))?;
    Ok(())
}

/// Empties every table of the stand-in database `database` and starts its
/// sequences and identities again, as the superuser.
async fn empty(pg: &Pg, database: &str, name: &str) -> Result<(), Ending> {
    const STEP: &str = "test-rows-empty";
    const WHAT: &str = "emptying a stand-in";
    let connection = open(&pg.superuser(database), STEP, WHAT, name).await?;
    let statement = "DO $$ DECLARE t text; BEGIN
        SELECT pg_catalog.string_agg(pg_catalog.format('%I.%I', n.nspname, c.relname), ', ')
          INTO t
          FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r', 'p') AND n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema';
        IF t IS NOT NULL THEN
          EXECUTE 'TRUNCATE ' || t || ' RESTART IDENTITY CASCADE';
        END IF;
      END $$";
    tokio::time::timeout(LOAD_WAIT, connection.client().batch_execute(statement))
        .await
        .map_err(|_| timed_out(STEP, WHAT, LOAD_WAIT, name))?
        .map_err(|e| refused(STEP, WHAT, &e, name))
}

/// The database's own message of `error`.
fn database_message(error: &tokio_postgres::Error) -> String {
    error
        .as_db_error()
        .map_or_else(|| error.to_string(), |db| db.message().to_owned())
}

/// Runs `file`'s statements in one transaction as the stand-in's write
/// login; the rows they changed.
async fn load_standin(
    pg: &Pg,
    standin: &Standin,
    file: &RowsFile,
    name: &str,
) -> Result<u64, Ending> {
    let list = split(file, name).await?;
    let address = DatabaseAddress::new(
        HOST,
        pg.port,
        &standin.write.0,
        standin.write.1.copy(),
        &standin.database,
    );
    let connection = open(
        &address,
        "test-rows-connect",
        "connecting as the stand-in's login that writes",
        name,
    )
    .await?;
    let client = connection.client();
    let deadline = Instant::now() + LOAD_WAIT;
    let begin = format!(
        "BEGIN; SET LOCAL statement_timeout = {}",
        STATEMENT_WAIT.as_millis()
    );
    tokio::time::timeout_at(deadline, client.batch_execute(&begin))
        .await
        .map_err(|_| failed(&file.name, "", "the loading did not begin in time", name))?
        .map_err(|e| failed(&file.name, "", &database_message(&e), name))?;
    let mut rows: u64 = 0;
    for statement in &list {
        let ran = tokio::time::timeout_at(deadline, client.simple_query(&statement.text)).await;
        let messages = match ran {
            Ok(Ok(messages)) => messages,
            Ok(Err(error)) => {
                return Err(failed(
                    &file.name,
                    &statement.first_line,
                    &database_message(&error),
                    name,
                ));
            }
            Err(_) => {
                return Err(failed(
                    &file.name,
                    &statement.first_line,
                    &format!(
                        "the loading of the file did not end within {} s",
                        LOAD_WAIT.as_secs()
                    ),
                    name,
                ));
            }
        };
        if statement.counts {
            for message in messages {
                if let SimpleQueryMessage::CommandComplete(count) = message {
                    rows = rows.saturating_add(count);
                }
            }
        }
    }
    tokio::time::timeout_at(deadline, client.batch_execute("COMMIT"))
        .await
        .map_err(|_| failed(&file.name, "", "the loading did not commit in time", name))?
        .map_err(|e| failed(&file.name, "", &database_message(&e), name))?;
    // The connection is dropped: a transaction left open by a failure
    // above is rolled back with it.
    drop(connection);
    Ok(rows)
}

/// The Application's number in the local platform database.
async fn application_number(
    live: &Live,
    application: &str,
    name: &str,
) -> Result<Option<i64>, Ending> {
    live.database
        .pool()
        .run(async |client: &mut tokio_postgres::Client| {
            client
                .query_opt(
                    "SELECT id FROM applications WHERE name = $1",
                    &[&application],
                )
                .await
                .map_err(DbError::from)
                .map(|row| row.and_then(|row| row.try_get::<_, i64>(0).ok()))
        })
        .await
        .map_err(|e| {
            database_failed(
                "test-rows-own-database",
                "reading the Application's number",
                &e,
                name,
            )
        })
}

/// Runs `file`'s statements in one transaction of the own database's
/// runtime role, with the identity of test rows attached; the rows they
/// changed. Their changes are shipped to the Change history after.
async fn load_own(target: &Target<'_>, file: &RowsFile) -> Result<u64, Ending> {
    let name = target.who.command;
    let live = target.live;
    let list = split(file, name).await?;
    const STEP: &str = "test-rows-own-database";
    const WHAT: &str = "beginning to load the own database's test rows";
    let pool = match live.own.pool_or_problem(target.application, true).await {
        Ok(Some(pool)) => pool,
        Ok(None) => {
            return Err(PgFailed::new(STEP, WHAT, Cause::Server(None))
                .said("the local own database is not there")
                .ending(name));
        }
        Err(Problem::Unavailable { step, cause }) => {
            return Err(own_database_unavailable(step, cause, name));
        }
        Err(_) => {
            return Err(PgFailed::new(STEP, WHAT, Cause::Server(None)).ending(name));
        }
    };
    let request = format!("test-rows-{}", uuid::Uuid::new_v4());
    let label = Label {
        user: "test-rows",
        application: target.application,
        request: &request,
        actor: Some("application"),
    };
    let deadline = Instant::now() + LOAD_WAIT;
    let mut held = pool
        .checkout_or_cause(Level::Write)
        .await
        .map_err(|cause| PgFailed::new(STEP, WHAT, cause).ending(name))?;
    held.begin_or_cause(None, label)
        .await
        .map_err(|cause| PgFailed::new(STEP, WHAT, cause).ending(name))?;
    let mut rows: u64 = 0;
    for statement in &list {
        let outcome = held
            .run(&statement.text, &[], Values::Typed, deadline)
            .await;
        match outcome.result {
            Ok(result) => {
                if statement.counts {
                    rows = rows.saturating_add(result.affected.unwrap_or(0));
                }
            }
            Err(failure) => {
                let error = match failure {
                    Failure::Database { message, .. } => message,
                    Failure::Limit(limit) => format!("it passed the limit of {}", limit.name()),
                    Failure::Unavailable => "the own database could not be reached".to_owned(),
                };
                if held.is_open() {
                    held.end(false).await.ok();
                }
                return Err(failed(&file.name, &statement.first_line, &error, name));
            }
        }
    }
    match held.end(true).await {
        Ok(()) => {}
        Err(Failure::Database { message, .. }) => {
            return Err(failed(&file.name, "", &message, name));
        }
        Err(_) => {
            return Err(failed(
                &file.name,
                "",
                "the own database did not commit the test rows",
                name,
            ));
        }
    }
    drop(pool);
    // Shipped now, so that the Change history has them when the command
    // (or the request) that follows reads it.
    tokio::time::timeout(SHIP_WAIT, live.own.ship_and_wait(target.application))
        .await
        .ok();
    Ok(rows)
}

/// Makes the own database anew: what it recorded shipped first, its
/// database dropped (by the superuser) and forgotten, made again from
/// nothing, its outbox's ids taken on after those already in the Change
/// history, and every migration applied again by the same code as at a
/// deploy. Answers their names.
async fn remake(
    target: &Target<'_>,
    memory: &Connection,
    number: i64,
    database: &str,
) -> Result<Vec<String>, Ending> {
    let name = target.who.command;
    let live = target.live;
    let own = &live.own;
    tokio::time::timeout(SHIP_WAIT, own.ship_and_wait(target.application))
        .await
        .ok();
    const STEP: &str = "test-rows-remake";
    own.forget(target.application).await.map_err(|e| {
        database_failed(
            STEP,
            "forgetting the own database to make it anew",
            &e,
            name,
        )
    })?;
    tokio::time::timeout(
        LOAD_WAIT,
        memory.client().batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            quote_identifier(database)
        )),
    )
    .await
    .map_err(|_| {
        timed_out(
            STEP,
            "dropping the own database to make it anew",
            LOAD_WAIT,
            name,
        )
    })?
    .map_err(|e| refused(STEP, "dropping the own database to make it anew", &e, name))?;
    let problem = |problem: Problem| match problem {
        Problem::Failed {
            migration,
            code,
            message,
        } => migration_failed(&migration, &code, &message, name),
        Problem::Refused {
            migration,
            rule,
            message,
        } => migration_refused(&migration, rule, &message, name),
        Problem::Unavailable { step, cause } => own_database_unavailable(step, cause, name),
        Problem::Diverged => {
            PgFailed::new(STEP, "making the own database anew", Cause::Server(None))
                .said("the own database made anew has migrations already")
                .ending(name)
        }
    };
    own.prepare(number, target.application, BUILDER, &[])
        .await
        .map_err(problem)?;
    let next: i64 = live
        .database
        .pool()
        .run(async |client: &mut tokio_postgres::Client| {
            client
                .query_one(
                    "SELECT coalesce(max(id), 0) + 1 FROM change_history WHERE application = $1",
                    &[&target.application],
                )
                .await
                .and_then(|row| row.try_get(0))
                .map_err(DbError::from)
        })
        .await
        .map_err(|e| database_failed(STEP, "reading where the Change history is", &e, name))?;
    let inside = open(
        &target.pg.superuser(database),
        STEP,
        "connecting to the own database made anew",
        name,
    )
    .await?;
    tokio::time::timeout(
        STATEMENT_WAIT,
        inside.client().batch_execute(&format!(
            "ALTER TABLE vm.outbox ALTER COLUMN id RESTART WITH {next}"
        )),
    )
    .await
    .map_err(|_| {
        timed_out(
            STEP,
            "readying the own database made anew",
            STATEMENT_WAIT,
            name,
        )
    })?
    .map_err(|e| refused(STEP, "readying the own database made anew", &e, name))?;
    drop(inside);
    own.prepare(number, target.application, BUILDER, target.migrations)
        .await
        .map_err(problem)?;
    own.ship(target.application);
    Ok(target.migrations.iter().map(|m| m.name.clone()).collect())
}
