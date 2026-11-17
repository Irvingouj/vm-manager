//! The local loop (blackbox/CONTRACT.md, "The local loop: one request as a
//! test User"): `vm request` runs the project on the Builder's machine,
//! beside a private Postgres with a stand-in of each pulled Resource, and
//! hands it one request as a test User; `vm local operation-log` and `vm
//! local change-history` read what such runs recorded.
//!
//! **The same code.** A run is the installation's own parts
//! ([`crate::platform::Platform`]: the registry, the own databases, the
//! Resources and their Grants, the Operation gateway, the Request runner,
//! Logs, the Operation log) on the private Postgres, and the request goes
//! through the installation's own [`Ingress`], handed over in the process:
//! nothing listens but the private Postgres, on `127.0.0.1`.
//!
//! **Endings.** The whole run is held to `--timeout` and ended by `SIGINT`,
//! `SIGTERM` and `SIGHUP` ([`Signals::within`]). The private Postgres and
//! the project's lock are kept outside the run's future ([`Held`]), so that
//! however it ends, the run's connections are closed first (its future is
//! dropped), then Postgres is stopped and waited for, then the lock is let
//! go.
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

mod download;
mod postgres;
mod standins;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{HOST, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::check::{self, Found};
use super::deploy;
use super::project_tools::{
    Bounded, Cx, DEFAULT_TIMEOUT, Scratch, Signals, Who, node_is_there, own_folder_failed,
    tool_failed,
};
use super::test_users::{self, BUILDER, TestUser, TestUsersFile};
use super::theirs::Theirs;
use super::{Done, Ending, Output, project_of};
use crate::api::{Api, read_version};
use crate::app_logs::{self, Query};
use crate::executor;
use crate::ingress::Ingress;
use crate::oplog::OperationLog;
use crate::platform::{Limits, Platform};
use crate::platform_db::{Connection, DatabaseAddress, DbError, PlatformDatabase, PoolLimits};
use crate::registry::Sent;
use crate::resources::{self, Login, NewResource};
use crate::runner;
use crate::sign_in::{FixedSignIn, RESERVED_HEADER_PREFIX};
use crate::tool_credentials::{DEFAULT_LIFETIME, ToolCredentials};
use postgres::{CONNECT_WAIT, HOST as LOOPBACK, Local, PLATFORM_DATABASE, PLATFORM_ROLE, Server};

/// The largest request body sent, and the most of a response's printed.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The longest each part of the drain may take: the operations still
/// running, the engine thread, the shipping of the changes, the writing of
/// the log entry.
const DRAIN_WAIT: Duration = Duration::from_secs(10);

/// The longest the platform database may take to be opened and brought up
/// to date.
const PLATFORM_WAIT: Duration = Duration::from_secs(60);

/// The longest the run's connections may take to close before its Postgres
/// is stopped all the same.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

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

/// `vm local`.
#[derive(clap::Args)]
pub struct LocalArgs {
    #[command(subcommand)]
    pub command: LocalCommand,
}

/// What `vm local` reads.
#[derive(clap::Subcommand)]
pub enum LocalCommand {
    /// The local Operation log: the newest entries of the project's local
    /// runs, oldest first.
    OperationLog(OperationLogArgs),
    /// The local Change history of the own database: the newest changes
    /// first, of one table or of all.
    ChangeHistory(ChangeHistoryArgs),
}

impl LocalArgs {
    pub(super) fn json(&self) -> bool {
        match &self.command {
            LocalCommand::OperationLog(args) => args.output.json,
            LocalCommand::ChangeHistory(args) => args.output.json,
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

/// Where a run was, for the message of one that ran out of time or was
/// interrupted.
#[derive(Clone, Copy)]
enum Stage {
    Node,
    Packages,
    Programs,
    Waiting,
    Starting,
    Standins,
    Building,
    Version,
    Request,
    Recording,
    Reading,
}

impl Stage {
    fn words(self) -> &'static str {
        match self {
            Self::Node => "asking node its version",
            Self::Packages => "finding the project's packages",
            Self::Programs => "finding (or downloading) the programs of the private Postgres",
            Self::Waiting => {
                "waiting for another local command of this project (vm request or vm local) to end"
            }
            Self::Starting => "starting the private Postgres",
            Self::Standins => "making the stand-ins of the pulled Resources",
            Self::Building => "building the project",
            Self::Version => "making the local Version and applying its migrations",
            Self::Request => "waiting for the Application's response",
            Self::Recording => "waiting for what the request recorded",
            Self::Reading => "reading what the local runs recorded",
        }
    }
}

/// What a run holds outside its own future, to let go of after it however
/// it ended: the private Postgres (stopped and waited for), then the lock.
#[derive(Default)]
struct Held {
    server: RefCell<Option<Server>>,
    lock: RefCell<Option<postgres::Lock>>,
}

impl Held {
    async fn release(&self) {
        // The run's connections were let go of with its future; each
        // closes its socket once the runtime drops its task. Closed before
        // Postgres is told to stop, so that it sees each client leave.
        crate::platform_db::wait_for_no_connection(CLOSE_WAIT).await;
        let server = self.server.borrow_mut().take();
        if let Some(server) = server {
            server.stop().await;
        }
        drop(self.lock.borrow_mut().take());
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
            "The private Postgres of the local run failed: {why}. Its own words are in .vm/local/postgres.log. Run {command} again; removing the folder .vm/local/ starts the local databases anew (their test rows are lost)"
        ),
    )
}

fn database_failed(error: &DbError, command: &str) -> Ending {
    Ending::new(
        1,
        "postgres-failed",
        format!(
            "The private Postgres of the local run did not answer as it should ({error}). Run {command} again; removing the folder .vm/local/ starts the local databases anew (their test rows are lost)"
        ),
    )
}

/// Starts the private Postgres of the project in `project`: the programs,
/// `.vm/local/`, the lock, the state, a server a killed run left, the data
/// folder, the server (in `held`), the first run's set-up. Its port, and
/// the state.
async fn start(
    project: &Path,
    own: &Path,
    command: &Command<'_>,
    stage: &Cell<Stage>,
    held: &Held,
) -> Result<(Local, postgres::State, u16), Ending> {
    let name = command.who.command;
    stage.set(Stage::Programs);
    let json = command.json;
    let programs = postgres::programs(own, name, |line| {
        if !json {
            let mut err = std::io::stderr().lock();
            writeln!(err, "{line}").and_then(|()| err.flush()).ok();
        }
    })
    .await?;
    let local = Local::of(project);
    local.make().map_err(|why| local_state_failed(&why, name))?;
    stage.set(Stage::Waiting);
    let lock = local
        .lock(|| {
            if !json {
                let mut err = std::io::stderr().lock();
                writeln!(
                    err,
                    "Another local command of this project (vm request or vm local) is running: waiting for it to end."
                )
                .and_then(|()| err.flush())
                .ok();
            }
        })
        .await
        .map_err(|why| local_state_failed(&why, name))?;
    *held.lock.borrow_mut() = Some(lock);
    // The parts' own records go to .vm/local/vm.log, not the terminal;
    // started anew by every run, so that it does not grow from run to run.
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(local.dir.join("vm.log"))
    {
        crate::log::to_file(file);
    }
    stage.set(Stage::Starting);
    let mut state = local
        .state()
        .map_err(|why| local_state_failed(&why, name))?;
    local
        .stop_leftover(&state)
        .await
        .map_err(|why| local_state_failed(&why, name))?;
    local
        .initdb(&programs, &state, own)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    let server = Server::start(&local, &programs)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    let port = server.port;
    *held.server.borrow_mut() = Some(server);
    local
        .set_up(&mut state, port)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    Ok((local, state, port))
}

/// The platform's address in the private Postgres.
fn platform_address(state: &postgres::State, port: u16) -> DatabaseAddress {
    DatabaseAddress::new(
        LOOPBACK,
        port,
        PLATFORM_ROLE,
        state.platform.copy(),
        PLATFORM_DATABASE,
    )
}

/// A pool of the platform database for a run: a few connections; the
/// request's own work is one at a time.
fn pool_limits() -> PoolLimits {
    PoolLimits {
        size: 4,
        ..PoolLimits::default()
    }
}

/// `vm request`.
pub(super) async fn request(args: &RequestArgs) -> Result<Done, Ending> {
    let project = project_path(args.project.as_deref(), WHO.command)?;
    let Some(file) = test_users::read(&project, WHO.command)? else {
        return Err(Ending::new(
            1,
            "test-users-file",
            format!(
                "The project has no {}: it holds the test Users a request is sent as. Make it as a new project has it (vm new makes one), or run vm pull <resource>, which adds your own; then run {} again",
                test_users::FILE,
                WHO.command
            ),
        ));
    };
    let Some(user) = file.user(&args.user) else {
        let there: Vec<&str> = file.users.iter().map(|u| u.id.as_str()).collect();
        return Err(Ending::theirs(
            1,
            "no-such-test-user",
            &format!(
                "{} is not a test User of {}; those there are: {}. Send the request as one of them (--as <id>), or add the User to {}",
                args.user,
                test_users::FILE,
                there.join(", "),
                test_users::FILE
            ),
        ));
    };
    let body = match (&args.data, &args.data_file) {
        (Some(text), _) => Some(Bytes::copy_from_slice(text.as_bytes())),
        (None, Some(path)) => Some(read_data_file(path)?),
        (None, None) => None,
    };
    let method = match (&args.method, &body) {
        (Some(method), _) => method.clone(),
        (None, Some(_)) => Method::POST,
        (None, None) => Method::GET,
    };
    // In place before the first tool starts: from here on these signals end
    // the run (below), not the program, so what it started is stopped.
    let mut signals = Signals::listen(&WHO)?;
    let own = Scratch::make(&WHO).map_err(|e| own_folder_failed(&e, &WHO))?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let command = Command {
        who: &WHO,
        json: args.output.json,
        limit,
    };
    let stage = Cell::new(Stage::Node);
    let held = Held::default();
    let asked = Asked {
        args,
        file: &file,
        user,
        method,
        body: body.unwrap_or_default(),
    };
    let ended = match signals
        .within(
            limit,
            run(&project, &own.path, &asked, &command, &stage, &held),
        )
        .await
    {
        Bounded::Ended(ended) => ended,
        Bounded::TimedOut => Err(command.timed_out(stage.get())),
        Bounded::Interrupted(signal) => Err(command.interrupted(signal, stage.get())),
    };
    held.release().await;
    drop(own);
    ended
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

/// What the request is.
struct Asked<'a> {
    args: &'a RequestArgs,
    /// The test Users file, as read once for the run: its User and its
    /// Grants are of one reading.
    file: &'a TestUsersFile,
    user: &'a TestUser,
    method: Method,
    body: Bytes,
}

/// What the run says beside the response.
#[derive(Default)]
struct Said {
    warnings: Vec<Value>,
    notes: Vec<Value>,
}

/// The whole run, held to its time by the caller.
async fn run(
    project: &Path,
    own: &Path,
    asked: &Asked<'_>,
    command: &Command<'_>,
    stage: &Cell<Stage>,
    held: &Held,
) -> Result<Done, Ending> {
    let name = WHO.command;
    let cx = Cx::new(project, own, &WHO);
    stage.set(Stage::Node);
    node_is_there(&cx, &WHO).await?;
    stage.set(Stage::Packages);
    let packages = check::packages(project, &WHO)?;

    let (local, state, port) = start(project, own, command, stage, held).await?;
    let mut said = Said::default();
    let platform = Platform::open(
        &platform_address(&state, port),
        Limits {
            pool: pool_limits(),
            executor: executor::Limits {
                size: 2,
                ..executor::Limits::default()
            },
            runner: runner::Limits {
                threads: std::num::NonZeroUsize::MIN,
                requests_per_thread: 1,
                queue_per_thread: 1,
                // The command's own time bounds the request.
                deadline: command.limit,
                ..runner::Limits::default()
            },
            start: PLATFORM_WAIT,
        },
    )
    .await
    .map_err(|why| postgres_failed(&why, name))?;

    stage.set(Stage::Standins);
    let standins = standins_of(
        project, asked.file, &local, &state, port, &platform, &mut said,
    )
    .await?;

    stage.set(Stage::Building);
    let mut found = Found::default();
    let manifest = check::manifest(&cx, &packages, &mut found, &WHO).await?;
    let manifest = match manifest {
        Some(manifest) if found.problems.is_empty() => manifest,
        _ => {
            return Err(match check::report(found, "") {
                Err(ending) => ending,
                Ok(_) => tool_failed("the SDK's tooling (manifest)", "gave no Manifest", &WHO),
            });
        }
    };
    said.warnings.append(&mut found.warnings);
    said.notes.append(&mut found.notes);
    let bundle = deploy::bundle(&cx, &packages, &WHO).await?;
    let migrations = deploy::project_migrations(project, &WHO)?;
    let body = deploy::body(
        &bundle,
        &manifest,
        NO_COMMIT,
        &migrations,
        &BTreeMap::new(),
        &WHO,
    )?;
    drop(bundle);
    drop(migrations);
    let version = read_version(&body).map_err(|why| {
        tool_failed(
            "the SDK's tooling",
            &format!("gave what is not a Version ({why})"),
            &WHO,
        )
    })?;
    drop(body);

    stage.set(Stage::Version);
    let (number, applied) = local_version(&platform, &manifest, version).await?;

    stage.set(Stage::Request);
    let Platform {
        database,
        own: own_databases,
        registry,
        resources,
        logs,
        writer,
        log,
        gateway,
        runner,
    } = platform;
    let ingress = Ingress {
        domain: Arc::from(DOMAIN),
        registry: Arc::clone(&registry),
        api: Api::new(
            Arc::clone(&registry),
            resources,
            Arc::clone(&log),
            Arc::clone(&logs),
            Arc::new(ToolCredentials::new(
                Arc::clone(&database),
                DEFAULT_LIFETIME,
            )),
            Arc::from(DOMAIN),
            None,
        ),
        sign_in: Box::new(FixedSignIn::new(
            // Copied: the sign-in owns the User it signs in; once a run.
            asked.user.id.clone(),
            asked.user.groups.clone(),
        )),
        credentials: Arc::new(ToolCredentials::new(
            Arc::clone(&database),
            DEFAULT_LIFETIME,
        )),
        runner,
        logs,
        max_request_body_bytes: MAX_BODY_BYTES,
    };
    let mut request = Request::builder()
        .method(asked.method.clone())
        .uri(asked.args.path.as_str())
        .header(HOST, format!("{}.{DOMAIN}", manifest.name));
    for (header, value) in &asked.args.header {
        request = request.header(header, value);
    }
    let request = request
        // A cheap copy: `Bytes` is shared.
        .body(Full::new(asked.body.clone()))
        .map_err(|e| {
            tool_failed(
                "vm request",
                &format!("cannot make the request ({e})"),
                &WHO,
            )
        })?;
    let response = ingress.handle(request).await;
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
    };
    // A response to HEAD has no body, as on the wire (where hyper leaves it
    // out for a deployed Application).
    let bytes = if asked.method == Method::HEAD {
        Bytes::new()
    } else {
        bytes
    };
    let request_id = parts
        .headers
        .get("x-vm-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    // Nothing ends before what the request recorded is there: its
    // operations, its changes, its log entry.
    stage.set(Stage::Recording);
    let Ingress {
        runner, logs, api, ..
    } = ingress;
    drop(api);
    // Operations: the engine thread ends (the request's work, waitUntil
    // included, with it), then what the gateway still runs; one time for
    // both.
    let operations_by = tokio::time::Instant::now() + DRAIN_WAIT;
    let threads_ended = runner.stop(DRAIN_WAIT).await;
    let left = operations_by.saturating_duration_since(tokio::time::Instant::now());
    let operations_ended = gateway.drain(left).await;
    drop(gateway);
    if !(threads_ended && operations_ended) {
        said.warnings.push(incomplete(
            "operations",
            "the request's operations did not all end within 10 seconds of its response: the operations below may lack some, and their entries in the local Operation log may come later or not at all",
        ));
    }
    // Changes: only an Application with an own database has an outbox to
    // ship into the Change history.
    if own_databases.has_own_database(&manifest.name) {
        let shipped =
            tokio::time::timeout(DRAIN_WAIT, own_databases.ship_and_wait(&manifest.name)).await;
        if shipped != Ok(true) {
            said.warnings.push(incomplete(
                "changes",
                "the changes the request made in the own database did not reach the local Change history within 10 seconds: vm local change-history may lack them until a later run ships them",
            ));
        }
    }
    // Its log entry: written once every other holder of Logs has let go.
    let logged = match Arc::into_inner(logs) {
        Some(logs) => writer.finish(logs, DRAIN_WAIT).await,
        None => false,
    };
    if !logged {
        said.warnings.push(incomplete(
            "log",
            "the request's log entry was not written within 10 seconds: printed and uncaught may lack what the code printed",
        ));
    }

    let operations = if request_id.is_empty() {
        Vec::new()
    } else {
        log.of_request(&request_id)
            .await
            .map_err(|e| database_failed(&e, name))?
    };
    let entry = if request_id.is_empty() {
        None
    } else {
        app_logs::read(
            &database,
            &manifest.name,
            &Query {
                limit: 1,
                after: None,
                request: Some(request_id.clone()),
            },
        )
        .await
        .map_err(|e| database_failed(&e, name))?
        .entries
        .first()
        .and_then(|entry| serde_json::to_value(entry).ok())
    };
    let printed = entry
        .as_ref()
        .and_then(|e| e.get("printed"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let uncaught = entry
        .as_ref()
        .and_then(|e| e.get("uncaught"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    // Every pool closed before Postgres is stopped (by the caller).
    drop(log);
    drop(registry);
    drop(own_databases);
    drop(database);

    let mut headers = Map::new();
    for name in parts.headers.keys() {
        let values: Vec<Value> = parts
            .headers
            .get_all(name)
            .iter()
            .map(|v| json!(String::from_utf8_lossy(v.as_bytes())))
            .collect();
        headers.insert(name.as_str().to_owned(), Value::Array(values));
    }
    let shown = Shown::of(&bytes);
    let standins: Vec<Value> = standins
        .iter()
        .map(|(resource, made)| json!({ "resource": resource, "made": made }))
        .collect();
    let json = json!({
        "as": asked.user.id,
        "method": asked.method.as_str(),
        "path": asked.args.path,
        "application": manifest.name,
        "version": number,
        "request_id": request_id,
        "status": parts.status.as_u16(),
        "headers": headers,
        "body": shown.text,
        "body_base64": shown.base64,
        "body_truncated": shown.truncated,
        "printed": printed,
        "uncaught": uncaught,
        "operations": operations,
        "migrations_applied": applied,
        "standins": standins,
        "warnings": said.warnings,
        "notes": said.notes,
    });
    let text = in_words(&json, &shown);
    Ok(Done { json, text })
}

/// The warning of a part of what the request recorded that did not come
/// in time.
fn incomplete(part: &str, what: &str) -> Value {
    json!({
        "reason": "records-incomplete",
        "part": part,
        "message": format!("{what}. The response above is the Application's all the same."),
    })
}

/// Makes and readies the stand-ins, then makes them the Resources of the
/// run, with the Grants of `vm.dev.json`. Each pulled Resource's name, and
/// whether its stand-in was made by this run.
async fn standins_of(
    project: &Path,
    file: &TestUsersFile,
    local: &Local,
    state: &postgres::State,
    port: u16,
    platform: &Platform,
    said: &mut Said,
) -> Result<Vec<(String, bool)>, Ending> {
    let name = WHO.command;
    let pulled = standins::pulled(project, name);
    said.warnings.extend(pulled.warnings);
    let address = local.superuser_address(state, port, "postgres");
    let ready = {
        let connection = Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT)
            .await
            .map_err(|e| database_failed(&e, name))?;
        let mut ready = Vec::with_capacity(pulled.files.len());
        for file in &pulled.files {
            ready.push(
                standins::ready(file, connection.client(), &address, &mut said.warnings)
                    .await
                    .map_err(|why| postgres_failed(&why, name))?,
            );
        }
        let kept: Vec<&str> = ready.iter().map(|s| s.database.as_str()).collect();
        standins::remove_others(connection.client(), &kept)
            .await
            .map_err(|why| postgres_failed(&why, name))?;
        ready
    };
    let resources = &platform.resources;
    for resource in resources.list() {
        resources
            .unregister(&resource.name)
            .await
            .map_err(|e| database_failed(&e, name))?;
    }
    let mut made = Vec::with_capacity(ready.len());
    for standin in ready {
        let connection = resources::Connection::new(
            LOOPBACK.to_owned(),
            port,
            standin.database,
            Login::new(standin.read.0, standin.read.1.copy()),
            Some(Login::new(standin.write.0, standin.write.1.copy())),
            None,
        );
        resources
            .register(
                NewResource {
                    description: format!("The local stand-in of {}", standin.resource),
                    owner: "the local run".to_owned(),
                    contact: standin.contact,
                    connection,
                    // Copied: the name is both registered and printed.
                    name: standin.resource.clone(),
                },
                BUILDER,
            )
            .await
            .map_err(|e| database_failed(&e, name))?;
        made.push((standin.resource, standin.made));
    }
    for grant in &file.grants {
        let level = resources::level_of(&grant.access);
        match (resources.get(&grant.resource), level) {
            (Some(_), Some(level)) => {
                resources
                    .grant(&grant.resource, &grant.group, level)
                    .await
                    .map_err(|e| database_failed(&e, name))?;
            }
            _ => said.notes.push(json!({
                "reason": "grant-without-resource",
                "resource": grant.resource,
                "group": grant.group,
                "message": format!(
                    "{} grants {} on {}, which has no stand-in here (it is not pulled): the Grant does nothing in the local run. Run vm pull {} to have it",
                    test_users::FILE, grant.group, grant.resource, grant.resource
                ),
            })),
        }
    }
    Ok(made)
}

/// The local Version: the latest one when the bundle, the Declared access
/// and the migrations are its own, else a new one sent as a deploy sends
/// it. Its number, and the migrations this run applied.
async fn local_version(
    platform: &Platform,
    manifest: &check::Manifest,
    version: crate::registry::NewVersion,
) -> Result<(u64, Vec<String>), Ending> {
    let name = WHO.command;
    let failed = |e: DbError| database_failed(&e, name);
    let registry = &platform.registry;
    let latest = match registry.application(&manifest.name).await.map_err(failed)? {
        None => {
            registry
                .create_application(&manifest.name, manifest.description.as_deref(), BUILDER)
                .await
                .map_err(failed)?;
            None
        }
        Some(record) => match record.latest_version {
            Some(number) => registry
                .version(&manifest.name, number)
                .await
                .map_err(failed)?,
            None => None,
        },
    };
    let ours: Vec<Value> = version
        .migrations
        .iter()
        .map(|m| json!({ "name": m.name, "sha256": m.sha256 }))
        .collect();
    let bundle_sha256 = hex(&Sha256::digest(version.bundle.as_bytes()));
    let declared = serde_json::to_value(&version.declared_access).ok();
    if let Some(latest) = &latest
        && latest.bundle_sha256 == bundle_sha256
        && Some(&latest.declared_access) == declared.as_ref()
        && latest.migrations == Value::Array(ours.clone())
    {
        return Ok((latest.number, Vec::new()));
    }
    let before: BTreeSet<&str> = latest
        .as_ref()
        .and_then(|l| l.migrations.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let added: Vec<String> = version
        .migrations
        .iter()
        .filter(|m| !before.contains(m.name.as_str()))
        .map(|m| m.name.clone())
        .collect();
    let diverged_at = latest.as_ref().and_then(|l| {
        let theirs = l.migrations.as_array()?;
        theirs
            .iter()
            .enumerate()
            .find(|(at, recorded)| ours.get(*at) != Some(*recorded))
            .and_then(|(_, recorded)| recorded.get("name").and_then(Value::as_str))
            .map(str::to_owned)
    });
    match registry
        .send_version(&manifest.name, version, BUILDER)
        .await
        .map_err(failed)?
    {
        Sent::Sent(record) => {
            registry
                .forget_versions(&manifest.name, KEPT_VERSIONS)
                .await
                .map_err(failed)?;
            Ok((record.number, added))
        }
        Sent::Diverged => {
            let migration = diverged_at.unwrap_or_default();
            Err(Ending::theirs(
                1,
                "local-migrations-diverged",
                &format!(
                    "migrations/ no longer begins with what the local own database has: it has the migration {migration}, which migrations/ does not have as it was (a migration not yet deployed was generated again). Removing the folder .vm/local/ starts the local databases anew (their test rows are lost); then run vm request again"
                ),
            ))
        }
        Sent::MigrationFailed {
            migration,
            code,
            message,
            ..
        } => {
            let said = format!(
                "The migration {migration} failed in the local own database: {message} (SQLSTATE {code}). Nothing was changed: the local own database is as it was. Correct src/db/schema.ts or the migration while it is not deployed, then run vm request again"
            );
            Err(deploy::detailed_refusal(
                "migration-failed",
                &said,
                &[
                    ("migration", &migration),
                    ("code", &code),
                    ("database_message", &message),
                ],
            ))
        }
        Sent::MigrationRefused {
            migration,
            rule,
            message,
            ..
        } => {
            let said = format!(
                "The migration {migration} breaks the platform's rule {rule}: {message}. Nothing was applied, locally as at a deploy. Correct the migration while it is not deployed, then run vm request again"
            );
            Err(deploy::detailed_refusal(
                "migration-refused",
                &said,
                &[("migration", &migration), ("rule", rule)],
            ))
        }
        Sent::OwnDatabaseUnavailable { step, code } => Err(postgres_failed(
            &format!(
                "the local own database could not be made ready ({step}{})",
                code.map(|c| format!(", SQLSTATE {c}")).unwrap_or_default()
            ),
            name,
        )),
        Sent::Busy | Sent::NoSuchApplication | Sent::NotAContributor => Err(tool_failed(
            "vm request",
            "could not make the local Version",
            &WHO,
        )),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

/// `vm local ...`.
pub(super) async fn local(args: &LocalArgs) -> Result<Done, Ending> {
    let (project, timeout, json) = match &args.command {
        LocalCommand::OperationLog(a) => (&a.project, a.timeout, a.output.json),
        LocalCommand::ChangeHistory(a) => (&a.project, a.timeout, a.output.json),
    };
    let project = project_path(project.as_deref(), WHO_LOCAL.command)?;
    let local = Local::of(&project);
    if !local.has_run() {
        // Nothing made, nothing downloaded.
        return Ok(match &args.command {
            LocalCommand::OperationLog(_) => Done {
                json: json!({ "entries": [] }),
                text: "No local run was made in this project yet (vm request makes one): the local Operation log is empty.\n".to_owned(),
            },
            LocalCommand::ChangeHistory(_) => Done {
                json: json!({ "changes": [] }),
                text: "No local run was made in this project yet (vm request makes one): the local Change history is empty.\n".to_owned(),
            },
        });
    }
    let mut signals = Signals::listen(&WHO_LOCAL)?;
    let own = Scratch::make(&WHO_LOCAL).map_err(|e| own_folder_failed(&e, &WHO_LOCAL))?;
    let limit = timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let command = Command {
        who: &WHO_LOCAL,
        json,
        limit,
    };
    let stage = Cell::new(Stage::Programs);
    let held = Held::default();
    let ended = match signals
        .within(
            limit,
            read_local(&project, &own.path, args, &command, &stage, &held),
        )
        .await
    {
        Bounded::Ended(ended) => ended,
        Bounded::TimedOut => Err(command.timed_out(stage.get())),
        Bounded::Interrupted(signal) => Err(command.interrupted(signal, stage.get())),
    };
    held.release().await;
    drop(own);
    ended
}

async fn read_local(
    project: &Path,
    own: &Path,
    args: &LocalArgs,
    command: &Command<'_>,
    stage: &Cell<Stage>,
    held: &Held,
) -> Result<Done, Ending> {
    let name = WHO_LOCAL.command;
    let (_local, state, port) = start(project, own, command, stage, held).await?;
    stage.set(Stage::Reading);
    let database = Arc::new(
        PlatformDatabase::open(&platform_address(&state, port), &pool_limits())
            .await
            .map_err(|why| postgres_failed(&why, name))?,
    );
    let failed = |e: DbError| database_failed(&e, name);
    match &args.command {
        LocalCommand::OperationLog(a) => {
            let entries = OperationLog::new(Arc::clone(&database))
                .newest(a.limit)
                .await
                .map_err(failed)?;
            drop(database);
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
        LocalCommand::ChangeHistory(a) => {
            let table = a.table.as_ref().map(|(s, t)| (s.as_str(), t.as_str()));
            let changes = crate::own_db::recorded_changes(&database, table, a.limit)
                .await
                .map_err(failed)?;
            drop(database);
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
