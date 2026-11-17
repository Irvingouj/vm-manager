//! The local loop (blackbox/CONTRACT.md, "The local loop: one request as a
//! test User" and "The local loop, kept running"): `vm request` runs the
//! project on the Builder's machine, beside a private Postgres with a
//! stand-in of each pulled Resource, and hands it one request as a test
//! User; `vm dev` ([`dev`]) keeps it running for the Builder's browser; `vm
//! local operation-log` and `vm local change-history` read what the local
//! runs recorded; `vm local reset` starts the local databases over.
//!
//! **The same code.** Every command does its work by the functions of
//! [`run`]: the private Postgres started, the installation's own parts on
//! it, the project brought up, a request served through the installation's
//! own Ingress and what it recorded collected. A command that finds a `vm
//! dev` running in its project ([`control`]) does none of it: it hands
//! itself to that `vm dev`, which does the same work and sends back what
//! the command prints.
//!
//! **Endings.** A command is held to `--timeout` and ended by `SIGINT`,
//! `SIGTERM` and `SIGHUP` ([`Signals::within`]). The private Postgres and
//! the project's lock are kept outside the command's future ([`Held`]), so
//! that however it ends, its connections are dropped first (its future is
//! dropped, which ends their tasks and closes their sockets), then Postgres
//! is stopped and waited for, then the lock is let go. A dropped connection
//! closes its socket without reading the answer to what it sent last (the
//! close of a prepared statement), so postgres.log may say "could not
//! receive data from client: Connection reset by peer": nothing is lost by
//! it, since every statement was answered before.
//!
//! **Bounds.** A request body is at most [`MAX_BODY_BYTES`]; of the
//! response's body [`MAX_BODY_BYTES`] are printed; the drain of what the
//! request left (operations, its log entry, the changes it made) waits at
//! most [`DRAIN_WAIT`] for each part.

/// Appends a formatted line to a `String`.
macro_rules! say {
    ($text:expr, $($arg:tt)*) => {{
        $text.push_str(&format!($($arg)*));
        $text.push('\n');
    }};
}

/// Appends formatted text to a `String`.
macro_rules! put {
    ($text:expr, $($arg:tt)*) => {
        $text.push_str(&format!($($arg)*))
    };
}

mod control;
mod dev;
mod download;
mod page;
mod postgres;
mod rows;
mod run;
mod standins;
mod walk;

pub(super) use dev::dev;

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Uri};
use serde_json::{Map, Value, json};

use super::project_tools::{Bounded, DEFAULT_TIMEOUT, Scratch, Signals, Who, own_folder_failed};
use super::test_users::TestUsersFile;
use super::theirs::Theirs;
use super::{Done, Ending, Output, project_of};
use crate::oplog::OperationLog;
use crate::platform_db::{DbError, PlatformDatabase};
use crate::runner;
use crate::sign_in::RESERVED_HEADER_PREFIX;
use postgres::Local;
use run::{Base, Brought, Held, Live, Served, Size};

/// The largest request body sent, and the most of a response's printed.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The longest each part of the drain may take: the operations still
/// running, the engine thread, the shipping of the changes, the writing of
/// the log entry.
const DRAIN_WAIT: Duration = Duration::from_secs(10);

/// The longest the platform database may take to be opened and brought up
/// to date.
const PLATFORM_WAIT: Duration = Duration::from_secs(60);

/// The local Versions kept: the newest, the earlier ones forgotten.
const KEPT_VERSIONS: u32 = 50;

/// The commit a local Version is recorded with: none.
const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

/// The address a local Application answers at is `<name>.<DOMAIN>`.
const DOMAIN: &str = "localhost";

const WHO: Who = Who {
    command: "vm request",
    the: "the local run",
    folder: "vm-request",
};

const WHO_LOCAL: Who = Who {
    command: "vm local",
    the: "the local reading",
    folder: "vm-local",
};

const WHO_RESET: Who = Who {
    command: "vm local reset",
    the: "the reset",
    folder: "vm-reset",
};

const WHO_DEV: Who = Who {
    command: "vm dev",
    the: "the local run",
    folder: "vm-dev",
};

/// `vm request`.
#[derive(clap::Args)]
pub struct RequestArgs {
    /// The path to ask for: it begins with /, and may have a query.
    #[arg(value_parser = request_path)]
    pub path: String,
    /// The test User of vm.dev.json to send the request as (builder, outsider, ...).
    #[arg(long = "as", value_name = "TEST USER")]
    pub user: String,
    /// The method: GET without a body, POST with one; any token is taken.
    #[arg(long, value_parser = method)]
    pub method: Option<Method>,
    /// A header, "<name>: <value>"; may be given several times. Host,
    /// Content-Length and x-vm-* are the tool's.
    #[arg(long, value_parser = header)]
    pub header: Vec<(HeaderName, HeaderValue)>,
    /// The body, as text (at most 1 MiB).
    #[arg(long, value_parser = data_text, conflicts_with = "data_file")]
    pub data: Option<String>,
    /// The body, the bytes of this file (at most 1 MiB).
    #[arg(long, value_parser = data_file)]
    pub data_file: Option<PathBuf>,
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The longest the whole run may take, in seconds (1 to 3600; 300
    /// without it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// `vm dev`.
#[derive(clap::Args)]
pub struct DevArgs {
    /// The port to listen on, on 127.0.0.1 and ::1 only (1 to 65535).
    #[arg(long, default_value_t = 7480, allow_negative_numbers = true,
          value_parser = clap::value_parser!(u16).range(1..=65535))]
    pub port: u16,
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    #[command(flatten)]
    pub output: Output,
}

/// `vm local`.
#[derive(clap::Args)]
pub struct LocalArgs {
    #[command(subcommand)]
    pub command: LocalCommand,
}

/// What `vm local` does.
#[derive(clap::Subcommand)]
pub enum LocalCommand {
    /// The local Operation log: the newest entries of the project's local
    /// runs, oldest first.
    OperationLog(OperationLogArgs),
    /// The local Change history of the own database: the newest changes
    /// first, of one table or of all.
    ChangeHistory(ChangeHistoryArgs),
    /// Start the local databases over: the own database, the stand-ins,
    /// the local Versions and records are made again from nothing, with
    /// every migration and the test rows. The project is not touched.
    Reset(ResetArgs),
}

impl LocalArgs {
    pub(super) fn json(&self) -> bool {
        match &self.command {
            LocalCommand::OperationLog(args) => args.output.json,
            LocalCommand::ChangeHistory(args) => args.output.json,
            LocalCommand::Reset(args) => args.output.json,
        }
    }
}

/// `vm local operation-log`.
#[derive(clap::Args)]
pub struct OperationLogArgs {
    /// How many entries, the newest (1 to 1000; 100 without it).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The longest the whole command may take, in seconds (1 to 3600; 300
    /// without it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// `vm local change-history`.
#[derive(clap::Args)]
pub struct ChangeHistoryArgs {
    /// Only the changes of this table: [schema.]table (public without a
    /// schema).
    #[arg(long, value_parser = table)]
    pub table: Option<(String, String)>,
    /// How many changes, the newest (1 to 1000; 100 without it).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The longest the whole command may take, in seconds (1 to 3600; 300
    /// without it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// `vm local reset`.
#[derive(clap::Args)]
pub struct ResetArgs {
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The longest the whole reset may take, in seconds (1 to 3600; 300
    /// without it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

// The command line's own checks: each failure is exit code 2, by clap.

fn request_path(text: &str) -> Result<String, String> {
    if !text.starts_with('/') {
        return Err("the path begins with /, such as /incidents or /incidents?open=true".into());
    }
    text.parse::<Uri>()
        .map_err(|_| "not a path and query a request can have".to_owned())?;
    Ok(text.to_owned())
}

fn method(text: &str) -> Result<Method, String> {
    Method::from_bytes(text.as_bytes())
        .map_err(|_| "not a method: a token such as GET, POST or PATCH".to_owned())
}

fn header(text: &str) -> Result<(HeaderName, HeaderValue), String> {
    let Some((name, value)) = text.split_once(':') else {
        return Err("a header is \"<name>: <value>\"".into());
    };
    let name = HeaderName::from_bytes(name.trim().as_bytes())
        .map_err(|_| "not a header's name".to_owned())?;
    let lower = name.as_str();
    if lower == "host" || lower == "content-length" || lower.starts_with(RESERVED_HEADER_PREFIX) {
        return Err(format!(
            "{lower} is the tool's own: Host, Content-Length and every x-vm- header are set by vm request"
        ));
    }
    let value =
        HeaderValue::from_str(value.trim()).map_err(|_| "not a header's value".to_owned())?;
    Ok((name, value))
}

fn data_text(text: &str) -> Result<String, String> {
    if text.len() > MAX_BODY_BYTES {
        return Err("a body is at most 1 MiB".into());
    }
    Ok(text.to_owned())
}

fn data_file(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    if let Ok(meta) = fs::metadata(&path)
        && meta.len() > u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX)
    {
        return Err("a body is at most 1 MiB, and this file is larger".into());
    }
    Ok(path)
}

fn table(text: &str) -> Result<(String, String), String> {
    let (schema, name) = match text.split_once('.') {
        Some((schema, name)) => (schema, name),
        None => ("public", text),
    };
    if schema.is_empty() || name.is_empty() || name.contains('.') {
        return Err("a table is <table> or <schema>.<table>".into());
    }
    Ok((schema.to_owned(), name.to_owned()))
}

/// Where a command was, for the message of one that ran out of time or was
/// interrupted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Node,
    Packages,
    Programs,
    Waiting,
    Starting,
    Standins,
    Building,
    Version,
    Rows,
    Request,
    Recording,
    Reading,
}

impl Stage {
    const ALL: [Self; 12] = [
        Self::Node,
        Self::Packages,
        Self::Programs,
        Self::Waiting,
        Self::Starting,
        Self::Standins,
        Self::Building,
        Self::Version,
        Self::Rows,
        Self::Request,
        Self::Recording,
        Self::Reading,
    ];

    fn words(self) -> &'static str {
        match self {
            Self::Node => "asking node its version",
            Self::Packages => "finding the project's packages",
            Self::Programs => "finding (or downloading) the programs of the private Postgres",
            Self::Waiting => {
                "waiting for another local command of this project (vm request, vm dev or vm local) to end"
            }
            Self::Starting => "starting the private Postgres",
            Self::Standins => "making the stand-ins of the pulled Resources",
            Self::Building => "building the project",
            Self::Version => "making the local Version and applying its migrations",
            Self::Rows => "loading the test rows",
            Self::Request => "waiting for the Application's response",
            Self::Recording => "waiting for what the request recorded",
            Self::Reading => "reading what the local runs recorded",
        }
    }
}

/// Where a command is now: set as it goes, read when it ran out of time.
/// Shared between threads (`vm dev` serves on several), so an atomic.
pub(super) struct Progress(AtomicUsize);

impl Progress {
    fn new(stage: Stage) -> Self {
        let progress = Self(AtomicUsize::new(0));
        progress.set(stage);
        progress
    }

    fn set(&self, stage: Stage) {
        let at = Stage::ALL.iter().position(|s| *s == stage).unwrap_or(0);
        self.0.store(at, Ordering::Relaxed);
    }

    fn get(&self) -> Stage {
        Stage::ALL
            .get(self.0.load(Ordering::Relaxed))
            .copied()
            .unwrap_or(Stage::Node)
    }
}

/// What every local command's messages need of it.
struct Command<'a> {
    who: &'a Who,
    json: bool,
    limit: Duration,
}

impl Command<'_> {
    fn timed_out(&self, stage: Stage) -> Ending {
        let command = self.who.command;
        Ending::new(
            1,
            "request-timed-out",
            format!(
                "{command} did not end within {} s and was stopped, with everything it started; it was {}. Give more time with --timeout <seconds>, then run {command} again",
                self.limit.as_secs(),
                stage.words()
            ),
        )
    }

    fn interrupted(&self, signal: &str, stage: Stage) -> Ending {
        let command = self.who.command;
        Ending::new(
            1,
            "interrupted",
            format!(
                "{command} was interrupted ({signal}) while {}, and stopped everything it had started: run {command} again",
                stage.words()
            ),
        )
    }

    fn base<'a>(&'a self, project: &'a Path, own: &'a Path) -> Base<'a> {
        Base {
            project,
            own,
            who: self.who,
            json: self.json,
        }
    }

    /// How a run held to its time and its signals ended, as its result.
    fn ended<T>(
        &self,
        bounded: Bounded<Result<T, Ending>>,
        progress: &Progress,
    ) -> Result<T, Ending> {
        match bounded {
            Bounded::Ended(ended) => ended,
            Bounded::TimedOut => Err(self.timed_out(progress.get())),
            Bounded::Interrupted(signal) => Err(self.interrupted(signal, progress.get())),
        }
    }
}

/// The project, given or the current directory, as its real path.
fn project_path(given: Option<&Path>, command: &str) -> Result<PathBuf, Ending> {
    let project = project_of(given, command)?;
    fs::canonicalize(&project).map_err(|error| {
        Ending::new(
            1,
            "project-file",
            format!(
                "the project's folder cannot be read ({error}): make it readable by you, then run {command} again"
            ),
        )
    })
}

fn local_state_failed(why: &str, command: &str) -> Ending {
    Ending::new(
        1,
        "local-state",
        format!(
            "The project's local state (.vm/local/) cannot be used: {why}; then run {command} again"
        ),
    )
}

fn postgres_failed(why: &str, command: &str) -> Ending {
    Ending::new(
        1,
        "postgres-failed",
        format!(
            "The private Postgres of the local run failed: {why}. Its own words are in .vm/local/postgres.log. Run {command} again; vm local reset starts the local databases anew (their test rows are loaded again)"
        ),
    )
}

fn database_failed(error: &DbError, command: &str) -> Ending {
    Ending::new(
        1,
        "postgres-failed",
        format!(
            "The private Postgres of the local run did not answer as it should ({error}). Run {command} again; vm local reset starts the local databases anew (their test rows are loaded again)"
        ),
    )
}

/// The size of the parts of a command that serves one request: a few
/// connections, one engine thread, the request held to the command's time.
fn one_request(limit: Duration) -> Size {
    Size {
        pool: 4,
        runner: runner::Limits {
            threads: std::num::NonZeroUsize::MIN,
            requests_per_thread: 1,
            queue_per_thread: 1,
            deadline: limit,
            ..runner::Limits::default()
        },
    }
}

/// The request of `vm request`, as the command line gives it.
struct Wanted<'a> {
    path: &'a str,
    user: &'a str,
    method: Method,
    headers: &'a [(HeaderName, HeaderValue)],
    body: Bytes,
}

/// `vm request`: answered by the `vm dev` of the project when one runs,
/// else alone.
pub(super) async fn request(args: &RequestArgs) -> Result<Done, Ending> {
    let project = project_path(args.project.as_deref(), WHO.command)?;
    let file = run::test_users_of(&project, WHO.command)?;
    if file.user(&args.user).is_none() {
        return Err(run::no_such_test_user(&file, &args.user));
    }
    let body = match (&args.data, &args.data_file) {
        (Some(text), _) => Bytes::copy_from_slice(text.as_bytes()),
        (None, Some(path)) => read_data_file(path)?,
        (None, None) => Bytes::new(),
    };
    let method = match &args.method {
        Some(method) => method.clone(),
        None if args.data.is_some() || args.data_file.is_some() => Method::POST,
        None => Method::GET,
    };
    let wanted = Wanted {
        path: &args.path,
        user: &args.user,
        method,
        headers: &args.header,
        body,
    };
    // In place before anything starts: from here on these signals end the
    // command (below), not the program, so what it started is stopped.
    let mut signals = Signals::listen(&WHO)?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let command = Command {
        who: &WHO,
        json: args.output.json,
        limit,
    };
    let progress = Progress::new(Stage::Request);
    if let Some(found) = control::running(&Local::of(&project)) {
        let sent = control::Sent::Request(control::request_body(&wanted));
        let forwarded = signals
            .within(limit, control::forward(&project, found, sent, &WHO))
            .await;
        match forwarded {
            Bounded::Ended(Some(ended)) => return ended,
            // Not answered: run alone, which waits for the project's lock.
            Bounded::Ended(None) => {}
            Bounded::TimedOut => return Err(command.timed_out(progress.get())),
            Bounded::Interrupted(signal) => {
                return Err(command.interrupted(signal, progress.get()));
            }
        }
    }
    let own = Scratch::make(&WHO).map_err(|e| own_folder_failed(&e, &WHO))?;
    progress.set(Stage::Node);
    let held = Held::default();
    let base = command.base(&project, &own.path);
    let bounded = signals
        .within(
            limit,
            request_alone(&base, &file, &wanted, limit, &progress, &held),
        )
        .await;
    let ended = command.ended(bounded, &progress);
    held.release().await;
    drop(own);
    ended
}

/// `vm request` alone: the private Postgres, the project brought up, the
/// request served and what it recorded collected.
async fn request_alone(
    base: &Base<'_>,
    file: &TestUsersFile,
    wanted: &Wanted<'_>,
    limit: Duration,
    progress: &Progress,
    held: &Held,
) -> Result<Done, Ending> {
    let packages = run::tools(base, progress).await?;
    let programs = run::programs(base, progress).await?;
    let local = Local::of(base.project);
    run::lock_project(base, &local, progress, held).await?;
    let pg = run::start_server(base, &programs, progress, held, false).await?;
    let live = Live::open(&pg, one_request(limit), base.who).await?;
    let done = async {
        let brought = run::bring_up(base, &pg, &live, file, &packages, progress).await?;
        let Some(user) = file.user(wanted.user) else {
            return Err(run::no_such_test_user(file, wanted.user));
        };
        let asked = run::Asked {
            path: wanted.path,
            user,
            method: wanted.method.clone(),
            headers: wanted.headers,
            // A cheap copy: `Bytes` is shared.
            body: wanted.body.clone(),
        };
        let served =
            run::serve_and_collect(&live, &brought.application, &asked, progress, base.who).await?;
        Ok(request_output(&brought, &served, &asked))
    }
    .await;
    live.close().await;
    done
}

/// What `vm request` prints: the response, what the request recorded, and
/// what the bring-up before it did.
fn request_output(brought: &Brought, served: &Served, asked: &run::Asked<'_>) -> Done {
    let mut headers = Map::new();
    for name in served.parts.headers.keys() {
        let values: Vec<Value> = served
            .parts
            .headers
            .get_all(name)
            .iter()
            .map(|v| json!(String::from_utf8_lossy(v.as_bytes())))
            .collect();
        headers.insert(name.as_str().to_owned(), Value::Array(values));
    }
    let shown = Shown::of(&served.body);
    let mut warnings = brought.said.warnings.clone();
    warnings.extend(served.warnings.iter().cloned());
    let json = json!({
        "as": asked.user.id,
        "method": asked.method.as_str(),
        "path": asked.path,
        "application": brought.application,
        "version": brought.version,
        "request_id": served.request_id,
        "status": served.parts.status.as_u16(),
        "headers": headers,
        "body": shown.text,
        "body_base64": shown.base64,
        "body_truncated": shown.truncated,
        "printed": served.printed,
        "uncaught": served.uncaught,
        "operations": served.operations,
        "migrations_applied": brought.migrations_applied,
        "standins": brought.standins_json(),
        "test_rows": brought.test_rows_json(),
        "warnings": warnings,
        "notes": brought.said.notes,
    });
    let text = in_words(&json, &shown);
    Done { json, text }
}

/// The body in `path`, at most [`MAX_BODY_BYTES`].
fn read_data_file(path: &Path) -> Result<Bytes, Ending> {
    let cannot = |error: std::io::Error| {
        Ending::theirs(
            1,
            "data-file",
            &format!(
                "{} cannot be read ({error}): give a file you can read with --data-file, then run vm request again",
                path.display()
            ),
        )
    };
    let file = File::open(path).map_err(cannot)?;
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(MAX_BODY_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(cannot)?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(Ending::new(
            2,
            "usage",
            "a body is at most 1 MiB, and the file given with --data-file is larger".to_owned(),
        ));
    }
    Ok(Bytes::from(bytes))
}

/// What `vm local operation-log` and `vm local change-history` read.
pub(super) enum Reading {
    OperationLog {
        limit: u16,
    },
    ChangeHistory {
        limit: u16,
        table: Option<(String, String)>,
    },
}

/// `vm local ...`: answered by the `vm dev` of the project when one runs,
/// else alone.
pub(super) async fn local(args: &LocalArgs) -> Result<Done, Ending> {
    let (project, timeout, json, reading) = match &args.command {
        LocalCommand::Reset(a) => return reset(a).await,
        LocalCommand::OperationLog(a) => (
            &a.project,
            a.timeout,
            a.output.json,
            Reading::OperationLog { limit: a.limit },
        ),
        LocalCommand::ChangeHistory(a) => (
            &a.project,
            a.timeout,
            a.output.json,
            Reading::ChangeHistory {
                limit: a.limit,
                // Copied: the reading owns what it reads; once a command.
                table: a.table.clone(),
            },
        ),
    };
    let project = project_path(project.as_deref(), WHO_LOCAL.command)?;
    let local = Local::of(&project);
    let limit = timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let command = Command {
        who: &WHO_LOCAL,
        json,
        limit,
    };
    let progress = Progress::new(Stage::Reading);
    if let Some(found) = control::running(&local) {
        let mut signals = Signals::listen(&WHO_LOCAL)?;
        let sent = control::Sent::Reading(control::reading_body(&reading));
        match signals
            .within(limit, control::forward(&project, found, sent, &WHO_LOCAL))
            .await
        {
            Bounded::Ended(Some(ended)) => return ended,
            Bounded::Ended(None) => {}
            Bounded::TimedOut => return Err(command.timed_out(progress.get())),
            Bounded::Interrupted(signal) => {
                return Err(command.interrupted(signal, progress.get()));
            }
        }
    }
    if !local.has_run() {
        // Nothing made, nothing downloaded.
        return Ok(match reading {
            Reading::OperationLog { .. } => Done {
                json: json!({ "entries": [] }),
                text: "No local run was made in this project yet (vm request makes one): the local Operation log is empty.\n".to_owned(),
            },
            Reading::ChangeHistory { .. } => Done {
                json: json!({ "changes": [] }),
                text: "No local run was made in this project yet (vm request makes one): the local Change history is empty.\n".to_owned(),
            },
        });
    }
    let mut signals = Signals::listen(&WHO_LOCAL)?;
    let own = Scratch::make(&WHO_LOCAL).map_err(|e| own_folder_failed(&e, &WHO_LOCAL))?;
    progress.set(Stage::Programs);
    let held = Held::default();
    let base = command.base(&project, &own.path);
    let bounded = signals
        .within(limit, read_alone(&base, &reading, &progress, &held))
        .await;
    let ended = command.ended(bounded, &progress);
    held.release().await;
    drop(own);
    ended
}

async fn read_alone(
    base: &Base<'_>,
    reading: &Reading,
    progress: &Progress,
    held: &Held,
) -> Result<Done, Ending> {
    let name = base.who.command;
    let programs = run::programs(base, progress).await?;
    let local = Local::of(base.project);
    run::lock_project(base, &local, progress, held).await?;
    let pg = run::start_server(base, &programs, progress, held, false).await?;
    progress.set(Stage::Reading);
    let database = Arc::new(
        PlatformDatabase::open(
            &pg.platform_address(),
            &crate::platform_db::PoolLimits {
                size: 2,
                ..crate::platform_db::PoolLimits::default()
            },
        )
        .await
        .map_err(|why| postgres_failed(&why, name))?,
    );
    let read = read_records(&database, reading, name).await;
    drop(database);
    read
}

/// The local Operation log or Change history, as the command prints them.
async fn read_records(
    database: &Arc<PlatformDatabase>,
    reading: &Reading,
    name: &str,
) -> Result<Done, Ending> {
    let failed = |e: DbError| database_failed(&e, name);
    match reading {
        Reading::OperationLog { limit } => {
            let entries = OperationLog::new(Arc::clone(database))
                .newest(*limit)
                .await
                .map_err(failed)?;
            let mut text = String::new();
            if entries.is_empty() {
                say!(text, "The local Operation log is empty.");
            }
            for entry in &entries {
                say!(
                    text,
                    "{} request {} as {} ({} {})",
                    one_line(&entry["time"]),
                    one_line(&entry["request"]),
                    one_line(&entry["user"]),
                    one_line(&entry["application"]),
                    entry["version"]
                );
                say!(text, "  {}", operation_line(entry));
                if let Some(statement) = entry["statement"].as_str() {
                    say!(text, "  {}", Theirs::from(statement));
                }
                text.push('\n');
            }
            Ok(Done {
                json: json!({ "entries": entries }),
                text,
            })
        }
        Reading::ChangeHistory { limit, table } => {
            let table = table.as_ref().map(|(s, t)| (s.as_str(), t.as_str()));
            let changes = crate::own_db::recorded_changes(database, table, *limit)
                .await
                .map_err(failed)?;
            let mut text = String::new();
            if changes.is_empty() {
                say!(text, "The local Change history has no change of that.");
            }
            for change in &changes {
                say!(
                    text,
                    "{} {} of {}.{} by {} ({}), request {}",
                    one_line(&change["at"]),
                    one_line(&change["kind"]),
                    one_line(&change["schema"]),
                    one_line(&change["table"]),
                    one_line(&change["user_id"]),
                    one_line(&change["actor"]),
                    one_line(&change["request"])
                );
                for key in ["key", "old", "new", "changed"] {
                    if !change[key].is_null() {
                        say!(text, "  {key}: {}", one_line(&change[key]));
                    }
                }
                text.push('\n');
            }
            Ok(Done {
                json: json!({ "changes": changes }),
                text,
            })
        }
    }
}

/// `vm local reset`: done by the `vm dev` of the project when one runs,
/// else alone.
async fn reset(args: &ResetArgs) -> Result<Done, Ending> {
    let project = project_path(args.project.as_deref(), WHO_RESET.command)?;
    let local = Local::of(&project);
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let command = Command {
        who: &WHO_RESET,
        json: args.output.json,
        limit,
    };
    let mut signals = Signals::listen(&WHO_RESET)?;
    let progress = Progress::new(Stage::Starting);
    if let Some(found) = control::running(&local) {
        match signals
            .within(
                limit,
                control::forward(&project, found, control::Sent::Reset, &WHO_RESET),
            )
            .await
        {
            Bounded::Ended(Some(ended)) => return ended,
            Bounded::Ended(None) => {}
            Bounded::TimedOut => return Err(command.timed_out(progress.get())),
            Bounded::Interrupted(signal) => {
                return Err(command.interrupted(signal, progress.get()));
            }
        }
    }
    let file = run::test_users_of(&project, WHO_RESET.command)?;
    let own = Scratch::make(&WHO_RESET).map_err(|e| own_folder_failed(&e, &WHO_RESET))?;
    progress.set(Stage::Node);
    let held = Held::default();
    let base = command.base(&project, &own.path);
    let bounded = signals
        .within(limit, reset_alone(&base, &file, limit, &progress, &held))
        .await;
    let ended = command.ended(bounded, &progress);
    held.release().await;
    drop(own);
    ended
}

async fn reset_alone(
    base: &Base<'_>,
    file: &TestUsersFile,
    limit: Duration,
    progress: &Progress,
    held: &Held,
) -> Result<Done, Ending> {
    let packages = run::tools(base, progress).await?;
    let programs = run::programs(base, progress).await?;
    let local = Local::of(base.project);
    run::lock_project(base, &local, progress, held).await?;
    let pg = run::start_server(base, &programs, progress, held, true).await?;
    let live = Live::open(&pg, one_request(limit), base.who).await?;
    let brought = run::bring_up(base, &pg, &live, file, &packages, progress).await;
    live.close().await;
    Ok(reset_output(&brought?))
}

/// What `vm local reset` prints.
fn reset_output(brought: &Brought) -> Done {
    let json = json!({
        "reset": true,
        "application": brought.application,
        "version": brought.version,
        "migrations_applied": brought.migrations_applied,
        "standins": brought.standins_json(),
        "test_rows": brought.test_rows_json(),
        "warnings": brought.said.warnings,
        "notes": brought.said.notes,
    });
    let mut text = String::new();
    say!(
        text,
        "The local databases of {} were started over: its local Version is {} again.",
        Theirs::from(brought.application.as_str()),
        brought.version
    );
    text.push_str(&brought_in_words(&json));
    Done { json, text }
}

/// What a bring-up did, in words: the migrations it applied, the stand-ins
/// it made, the test rows it loaded, its warnings and notes.
fn brought_in_words(json: &Value) -> String {
    let mut text = String::new();
    let applied = json["migrations_applied"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if !applied.is_empty() {
        let names: Vec<String> = applied.iter().map(one_line).collect();
        say!(text, "Migrations applied: {}", names.join(", "));
    }
    for standin in json["standins"].as_array().into_iter().flatten() {
        if standin["made"].as_bool() == Some(true) {
            say!(
                text,
                "The stand-in of {} was made from its standin.sql.",
                one_line(&standin["resource"])
            );
        }
    }
    for loaded in json["test_rows"].as_array().into_iter().flatten() {
        say!(
            text,
            "Test rows loaded from {}: {} rows.",
            one_line(&loaded["file"]),
            loaded["rows"]
        );
    }
    for (list, label) in [("warnings", "Warning"), ("notes", "Note")] {
        for item in json[list].as_array().into_iter().flatten() {
            say!(
                text,
                "{label} ({}): {}",
                one_line(&item["reason"]),
                one_line(&item["message"])
            );
        }
    }
    text
}

/// The response's body as it is printed.
struct Shown {
    text: Option<String>,
    base64: Option<String>,
    truncated: bool,
}

impl Shown {
    fn of(bytes: &[u8]) -> Self {
        let truncated = bytes.len() > MAX_BODY_BYTES;
        let kept = bytes.get(..MAX_BODY_BYTES).unwrap_or(bytes);
        let text = match std::str::from_utf8(kept) {
            Ok(text) => Some(text.to_owned()),
            // Cut inside a character: the text up to it.
            Err(error) if truncated && error.error_len().is_none() => kept
                .get(..error.valid_up_to())
                .and_then(|valid| std::str::from_utf8(valid).ok())
                .map(str::to_owned),
            Err(_) => None,
        };
        match text {
            Some(text) => Self {
                text: Some(text),
                base64: None,
                truncated,
            },
            None => Self {
                text: None,
                base64: Some(base64(kept)),
                truncated,
            },
        }
    }
}

/// Standard base64, with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |index: u32| {
        ALPHABET
            .get(usize::try_from(index & 63).unwrap_or(0))
            .map_or('=', |b| char::from(*b))
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(symbol(n >> 18));
        out.push(symbol(n >> 12));
        out.push(if chunk.len() > 1 { symbol(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n) } else { '=' });
    }
    out
}

/// Text of the project's or the Application's, escaped, its line ends
/// kept.
fn escaped_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| Theirs::from(line).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `value` as one escaped line: a text as it is, anything else as JSON.
fn one_line(value: &Value) -> String {
    match value {
        Value::String(text) => Theirs::from(text.as_str()).to_string(),
        other => Theirs::from(other.to_string().as_str()).to_string(),
    }
}

/// What `vm request` prints in words.
fn in_words(json: &Value, shown: &Shown) -> String {
    let mut text = String::new();
    let status = json["status"].as_u64().unwrap_or_default();
    let reason = u16::try_from(status)
        .ok()
        .and_then(|s| hyper::StatusCode::from_u16(s).ok())
        .and_then(|s| s.canonical_reason())
        .unwrap_or("");
    say!(text, "{status} {reason}");
    if let Some(headers) = json["headers"].as_object() {
        for (name, values) in headers {
            for value in values.as_array().into_iter().flatten() {
                say!(text, "{name}: {}", one_line(value));
            }
        }
    }
    text.push('\n');
    match (&shown.text, &shown.base64) {
        (Some(body), _) => {
            text.push_str(&escaped_lines(body));
            if !body.is_empty() && !body.ends_with('\n') {
                text.push('\n');
            }
        }
        (None, Some(base64)) => say!(text, "(the body is not text; in base64:)\n{base64}"),
        (None, None) => {}
    }
    if shown.truncated {
        say!(text, "(the body was longer: its first MiB is shown)");
    }
    text.push('\n');
    say!(
        text,
        "As {}: {} {} of {}, Version {} (request {})",
        one_line(&json["as"]),
        one_line(&json["method"]),
        one_line(&json["path"]),
        one_line(&json["application"]),
        json["version"],
        one_line(&json["request_id"])
    );
    let printed = json["printed"].as_array().map_or(&[][..], Vec::as_slice);
    if !printed.is_empty() {
        say!(text, "Printed:");
        for line in printed {
            let loading = if line["loading"].as_bool() == Some(true) {
                " (while loading)"
            } else {
                ""
            };
            say!(
                text,
                "  {}{loading}: {}",
                one_line(&line["level"]),
                one_line(&line["message"])
            );
        }
    }
    let uncaught = json["uncaught"].as_array().map_or(&[][..], Vec::as_slice);
    if !uncaught.is_empty() {
        say!(text, "Uncaught:");
        for error in uncaught {
            say!(text, "  {}", one_line(error));
        }
    }
    let operations = json["operations"].as_array().map_or(&[][..], Vec::as_slice);
    if !operations.is_empty() {
        say!(text, "Operations:");
        for entry in operations {
            say!(text, "  {}", operation_line(entry));
        }
    }
    text.push_str(&brought_in_words(json));
    text
}

/// One operation of the Operation log, on one line: its Resource or the
/// own database, its outcome, the reason of a refusal.
fn operation_line(entry: &Value) -> String {
    let target = if entry["own_database"].as_bool() == Some(true) {
        "the own database".to_owned()
    } else {
        entry["resource"].as_str().map_or_else(
            || "(no Resource)".to_owned(),
            |r| Theirs::from(r).to_string(),
        )
    };
    let mut line = format!(
        "{target}: {} {}",
        one_line(&entry["operation"]),
        one_line(&entry["outcome"])
    );
    if let Some(reason) = entry["reason"].as_str() {
        put!(line, " ({})", Theirs::from(reason));
    }
    if let Some(rows) = entry["rows"].as_i64() {
        put!(line, ", {rows} rows");
    }
    if let Some(error) = entry["error"].as_str() {
        put!(line, ": {}", Theirs::from(error));
    }
    line
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
