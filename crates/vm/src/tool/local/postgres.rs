//! The project's private Postgres (blackbox/CONTRACT.md, "The local loop:
//! What a run does", steps 1 to 3): its programs ([`programs`]), the
//! project's local state under `.vm/local/` ([`Local`]: the lock, the
//! passwords, the data), and the server itself ([`Server`]), started for a
//! run and stopped before the command ends.
//!
//! **Listening.** On `127.0.0.1` only, on a port free at that moment, with no
//! Unix socket (`unix_socket_directories` empty), and `pg_hba.conf` holding
//! one line: `scram-sha-256` from `127.0.0.1`. Its superuser
//! ([`SUPERUSER`]) and the platform's role ([`PLATFORM_ROLE`], `CREATEDB
//! CREATEROLE`, no superuser) have passwords made from the system's
//! randomness at the first run, kept only in `.vm/local/state.json` (mode
//! `0600`; the folder is `0700`). initdb is given the superuser's in a file
//! of mode `0600` it reads and that is removed after; the platform's role
//! gets only its SCRAM verifier. No password is ever an argument, printed or
//! logged.
//!
//! **Stopping.** [`Server::stop`] asks for a fast shutdown (`SIGINT`) and
//! waits up to [`STOP_WAIT`]; then an immediate one (`SIGQUIT`); then
//! `SIGKILL` to its whole process group. A server dropped without being
//! stopped is sent `SIGKILL` (its group too). A server left running by a run
//! killed outright is found by its `postmaster.pid` and stopped by the next
//! run ([`stop_leftover`]), but only once it has shown it is this project's:
//! it accepts this project's superuser and password on the port the file
//! names and says its data folder is this project's.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};
use serde_json::{Value, json};

use super::super::child::{self, Kept};
use super::download::{self, Archive};
use crate::platform_db::{Connection, DatabaseAddress, DbError};
use tokio_postgres::error::SqlState;

/// The variable naming a Postgres the Builder has.
pub(super) const BIN_VARIABLE: &str = "VM_LOCAL_POSTGRES_BIN";

/// The variable naming a mirror of Maven Central.
pub(super) const MIRROR_VARIABLE: &str = "VM_LOCAL_POSTGRES_MIRROR";

/// The superuser initdb makes.
const SUPERUSER: &str = "vm_superuser";

/// The platform's role: what `VM_DATABASE_URL`'s role is in an
/// installation.
pub(super) const PLATFORM_ROLE: &str = "vm";

/// The platform database's name.
pub(super) const PLATFORM_DATABASE: &str = "vm";

/// The loopback address it listens on.
pub(super) const HOST: &str = "127.0.0.1";

/// How long `postgres --version` may take.
const VERSION_WAIT: Duration = Duration::from_secs(10);

/// How long initdb may take.
const INITDB_WAIT: Duration = Duration::from_secs(120);

/// How long the server may take to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(60);

/// How long each way of stopping is given before the next.
const STOP_WAIT: Duration = Duration::from_secs(15);

/// How long a connection may take to open (the leftover's check, the first
/// run's set-up).
pub(super) const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// Ports tried before starting is given up.
const PORT_ATTEMPTS: usize = 5;

/// How often a lock, a server's readiness or its end is looked at.
const POLL: Duration = Duration::from_millis(10);

/// How often the lock is tried while another command holds it.
const LOCK_POLL: Duration = Duration::from_millis(100);

/// What is kept of a program's words.
const WORDS_BYTES: usize = 16 * 1024;

/// Why the private Postgres cannot be had: `postgres-unavailable`, naming
/// both variables. `what` is the tool's own words.
pub(super) fn unavailable(what: &str, command: &str) -> super::super::Ending {
    super::super::Ending::new(
        1,
        "postgres-unavailable",
        format!(
            "The private Postgres of the local run cannot be had: {what}. Name the folder of a Postgres 16, 17 or 18 you have (its bin folder, with postgres, initdb and pg_ctl) with {BIN_VARIABLE}, or a mirror of Maven Central with {MIRROR_VARIABLE}, then run {command} again"
        ),
    )
}

/// A password of the local state's. It has no `Debug` and no `Display`.
pub(super) struct Secret(String);

impl Secret {
    pub(super) fn generate() -> Result<Self, String> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|e| format!("no randomness for a password ({e})"))?;
        Ok(Self(random.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// A copy, for an address or a role that is given it.
    pub(super) fn copy(&self) -> String {
        self.0.clone()
    }

    /// Its SCRAM verifier: what Postgres is told instead of the password.
    pub(super) fn verifier(&self) -> String {
        postgres_protocol::password::scram_sha_256(self.0.as_bytes())
    }
}

/// The directory of the Postgres programs.
pub(super) struct Programs {
    bin: PathBuf,
}

impl Programs {
    fn program(&self, name: &str) -> PathBuf {
        self.bin.join(name)
    }
}

/// The user's own folder the programs are downloaded into:
/// `$XDG_DATA_HOME/vm/postgres/` when that is absolute, else
/// `$HOME/.local/share/vm/postgres/`.
fn data_folder() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    absolute("XDG_DATA_HOME")
        .map(|d| d.join("vm").join("postgres"))
        .or_else(|| absolute("HOME").map(|h| h.join(".local/share/vm/postgres")))
}

/// The programs: those `VM_LOCAL_POSTGRES_BIN` names, or those in the
/// user's own folder, downloaded there first if they are not (`say` tells
/// the Builder before it starts). `own` is the command's own folder.
pub(super) async fn programs(
    own: &Path,
    command: &str,
    say: impl FnOnce(&str),
) -> Result<Programs, super::super::Ending> {
    if let Some(bin) = std::env::var_os(BIN_VARIABLE).filter(|v| !v.is_empty()) {
        let bin = PathBuf::from(bin);
        let programs = Programs { bin };
        return match checked(&programs, own).await {
            Ok(()) => Ok(programs),
            Err(why) => Err(unavailable(
                &format!(
                    "{BIN_VARIABLE} names {}, which {why}",
                    programs.bin.display()
                ),
                command,
            )),
        };
    }
    let Some(archive) = Archive::for_this_machine() else {
        return Err(unavailable(
            &format!(
                "this program has no build of Postgres for {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            command,
        ));
    };
    let Some(folder) = data_folder() else {
        return Err(unavailable(
            "neither XDG_DATA_HOME nor HOME is an absolute path, so there is no folder of yours to keep it in",
            command,
        ));
    };
    let bin = folder.join(archive.folder_name()).join("bin");
    if !bin.join("postgres").is_file() {
        let base = std::env::var(MIRROR_VARIABLE)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| download::MAVEN_CENTRAL.to_owned());
        say(&format!(
            "Postgres {} is not on this machine yet: downloading it, once, from {base} into {} (it is kept there for every project)",
            download::VERSION,
            folder.display()
        ));
        download::download(&archive, &base, &folder)
            .await
            .map_err(|why| unavailable(&format!("the download failed: {why}"), command))?;
    }
    let programs = Programs { bin };
    match checked(&programs, own).await {
        Ok(()) => Ok(programs),
        Err(why) => Err(unavailable(
            &format!("{} {why}", programs.bin.display()),
            command,
        )),
    }
}

/// That `programs` holds `postgres`, `initdb` and `pg_ctl`, and that
/// `postgres` is 16, 17 or 18; else why not, in words.
async fn checked(programs: &Programs, own: &Path) -> Result<(), String> {
    for name in ["postgres", "initdb", "pg_ctl"] {
        if !programs.program(name).is_file() {
            return Err(format!("has no {name}"));
        }
    }
    let ran = tokio::time::timeout(
        VERSION_WAIT,
        child::run(
            programs.program("postgres"),
            own,
            own,
            &[OsStr::new("--version")],
            Kept::up_to(WORDS_BYTES),
            Kept::up_to(WORDS_BYTES),
        ),
    )
    .await
    .map_err(|_| "has a postgres that did not say its version in time".to_owned())?
    .map_err(|e| format!("has a postgres that cannot be run ({e})"))?;
    let said = ran.out.text();
    let major = said
        .split_whitespace()
        .last()
        .and_then(|v| v.split(['.', 'b', 'r']).next())
        .and_then(|m| m.parse::<u32>().ok());
    match major {
        Some(16..=18) if ran.status.success() => Ok(()),
        Some(major) => Err(format!(
            "holds Postgres {major}, and the local run needs 16, 17 or 18"
        )),
        None => Err("has a postgres that does not say its version".to_owned()),
    }
}

/// The project's local state: `.vm/local/`.
pub(super) struct Local {
    pub dir: PathBuf,
}

/// The passwords of the local state, and whether the first run's set-up
/// was done.
pub(super) struct State {
    pub superuser: Secret,
    pub platform: Secret,
    pub ready: bool,
}

/// The lock of the project's local runs: held while the file is open; the
/// kernel lets go of it when the process ends, however it ends.
pub(super) struct Lock {
    _file: File,
}

impl Local {
    pub(super) fn of(project: &Path) -> Self {
        Self {
            dir: project.join(".vm").join("local"),
        }
    }

    /// Whether a run ever made the private Postgres here.
    pub(super) fn has_run(&self) -> bool {
        self.data().join("PG_VERSION").is_file() && self.dir.join("state.json").is_file()
    }

    /// The data folder.
    pub(super) fn data(&self) -> PathBuf {
        self.dir.join("postgres")
    }

    /// Makes `.vm/local/` (mode `0700`) unless it is there.
    pub(super) fn make(&self) -> Result<(), String> {
        let cannot = |e: std::io::Error| format!("{} cannot be made ({e})", self.dir.display());
        if let Some(parent) = self.dir.parent() {
            DirBuilder::new()
                .recursive(true)
                .create(parent)
                .map_err(cannot)?;
        }
        match DirBuilder::new().mode(0o700).create(&self.dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                match fs::symlink_metadata(&self.dir) {
                    Ok(meta) if meta.is_dir() => Ok(()),
                    _ => Err(format!(
                        "{} is not a folder: remove it, then run the command again",
                        self.dir.display()
                    )),
                }
            }
            Err(error) => Err(cannot(error)),
        }
    }

    /// Takes the lock, trying every [`LOCK_POLL`] while another command
    /// holds it (`waiting` is called once, then); the caller bounds the
    /// wait.
    pub(super) async fn lock(&self, waiting: impl FnOnce()) -> Result<Lock, String> {
        let path = self.dir.join("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("{} cannot be opened ({e})", path.display()))?;
        let mut waiting = Some(waiting);
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Lock { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {
                    if let Some(waiting) = waiting.take() {
                        waiting();
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(error) => return Err(format!("{} cannot be locked ({error})", path.display())),
            }
        }
    }

    /// The state, made (new passwords) when it is not there.
    pub(super) fn state(&self) -> Result<State, String> {
        let path = self.dir.join("state.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| self.broken())?;
                let text = |key: &str| {
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|s| Secret(s.to_owned()))
                        .ok_or_else(|| self.broken())
                };
                Ok(State {
                    superuser: text("superuser")?,
                    platform: text("platform")?,
                    ready: value.get("ready").and_then(Value::as_bool).unwrap_or(false),
                })
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if self.data().exists() {
                    return Err(self.broken());
                }
                let state = State {
                    superuser: Secret::generate()?,
                    platform: Secret::generate()?,
                    ready: false,
                };
                self.write_state(&state)?;
                Ok(state)
            }
            Err(error) => Err(format!("{} cannot be read ({error})", path.display())),
        }
    }

    fn broken(&self) -> String {
        format!(
            "{} cannot be read: the local databases cannot be opened. Removing the folder .vm/local/ starts them anew (their test rows are lost)",
            self.dir.join("state.json").display()
        )
    }

    /// Writes the state whole: to a new file of mode `0600`, then renamed.
    pub(super) fn write_state(&self, state: &State) -> Result<(), String> {
        let path = self.dir.join("state.json");
        let partial = self.dir.join(".state.json.partial");
        let text = json!({
            "superuser": state.superuser.0,
            "platform": state.platform.0,
            "ready": state.ready,
        })
        .to_string();
        let written = (|| {
            fs::remove_file(&partial).ok();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&partial)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&partial, &path)
        })();
        written.map_err(|e| format!("{} cannot be written ({e})", path.display()))
    }

    /// Makes the data folder with initdb, unless it is there; then makes
    /// `pg_hba.conf` its one line. `own` is the command's own folder.
    pub(super) async fn initdb(
        &self,
        programs: &Programs,
        state: &State,
        own: &Path,
    ) -> Result<(), String> {
        let data = self.data();
        if data.join("PG_VERSION").is_file() {
            return Ok(());
        }
        if data.exists() {
            fs::remove_dir_all(&data)
                .map_err(|e| format!("{} cannot be made anew ({e})", data.display()))?;
        }
        let password_file = self.dir.join(".initdb-password");
        fs::remove_file(&password_file).ok();
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&password_file)
            .and_then(|mut file| file.write_all(state.superuser.0.as_bytes()));
        if let Err(error) = written {
            fs::remove_file(&password_file).ok();
            return Err(format!(
                "{} cannot be written ({error})",
                password_file.display()
            ));
        }
        let pwfile = format!("--pwfile={}", password_file.display());
        let args: [&OsStr; 11] = [
            OsStr::new("-D"),
            data.as_os_str(),
            OsStr::new("-U"),
            OsStr::new(SUPERUSER),
            OsStr::new(&pwfile),
            OsStr::new("--auth=scram-sha-256"),
            OsStr::new("-E"),
            OsStr::new("UTF8"),
            OsStr::new("--no-locale"),
            OsStr::new("--no-sync"),
            OsStr::new("--no-instructions"),
        ];
        let ran = tokio::time::timeout(
            INITDB_WAIT,
            child::run(
                programs.program("initdb"),
                &self.dir,
                own,
                &args,
                Kept::up_to(WORDS_BYTES),
                Kept::up_to(WORDS_BYTES),
            ),
        )
        .await;
        fs::remove_file(&password_file).ok();
        let failed = |why: String| {
            fs::remove_dir_all(&data).ok();
            why
        };
        let ran = ran
            .map_err(|_| {
                failed(format!(
                    "initdb did not end within {} s",
                    INITDB_WAIT.as_secs()
                ))
            })?
            .map_err(|e| failed(format!("initdb cannot be run ({e})")))?;
        if !ran.status.success() {
            let words = ran.err.text();
            let first: Vec<&str> = words
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(4)
                .collect();
            return Err(failed(format!("initdb failed: {}", first.join(" / "))));
        }
        fs::write(
            data.join("pg_hba.conf"),
            format!("# Written by vm: password logins from {HOST} only.\nhost all all {HOST}/32 scram-sha-256\n"),
        )
        .map_err(|e| failed(format!("pg_hba.conf cannot be written ({e})")))
    }

    /// The address of `database` as the superuser.
    pub(super) fn superuser_address(
        &self,
        state: &State,
        port: u16,
        database: &str,
    ) -> DatabaseAddress {
        DatabaseAddress::new(HOST, port, SUPERUSER, state.superuser.copy(), database)
    }

    /// The first run's set-up, once the server is up: the platform's role.
    pub(super) async fn set_up(&self, state: &mut State, port: u16) -> Result<(), String> {
        if state.ready {
            return Ok(());
        }
        let address = self.superuser_address(state, port, "postgres");
        let connection = Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT)
            .await
            .map_err(|e| {
                format!("the private Postgres cannot be reached as its superuser ({e})")
            })?;
        let verifier = state.platform.verifier();
        let statement = format!(
            "DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = '{PLATFORM_ROLE}') THEN
                 CREATE ROLE {PLATFORM_ROLE} LOGIN CREATEDB CREATEROLE NOSUPERUSER;
               END IF;
             END $$;
             ALTER ROLE {PLATFORM_ROLE} PASSWORD '{verifier}'"
        );
        tokio::time::timeout(CONNECT_WAIT, connection.client().batch_execute(&statement))
            .await
            .map_err(|_| "the platform's role was not made in time".to_owned())?
            .map_err(|e| format!("the platform's role cannot be made ({})", DbError::from(e)))?;
        drop(connection);
        state.ready = true;
        self.write_state(state)
    }

    /// Stops a server a run killed outright left running, once it has shown
    /// it is this project's (see the module's text). Nothing when there is
    /// no `postmaster.pid`.
    pub(super) async fn stop_leftover(&self, state: &State) -> Result<(), String> {
        let pid_file = self.data().join("postmaster.pid");
        let Ok(text) = fs::read_to_string(&pid_file) else {
            return Ok(());
        };
        let mut lines = text.lines();
        let pid = lines.next().and_then(|l| l.trim().parse::<i32>().ok());
        let port = lines.nth(2).and_then(|l| l.trim().parse::<u16>().ok());
        let (Some(pid), Some(port)) = (pid.and_then(Pid::from_raw), port) else {
            return Ok(());
        };
        if test_kill_process(pid).is_err() {
            // Nothing runs under that number: Postgres removes a file left
            // by a server that is gone.
            return Ok(());
        }
        let address = self.superuser_address(state, port, "postgres");
        let ours = async {
            // A server still starting up (or recovering from being killed)
            // says so, and is waited for.
            let deadline = tokio::time::Instant::now() + READY_WAIT;
            let connection = loop {
                match Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT).await {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.code() == Some(&SqlState::CANNOT_CONNECT_NOW)
                            && tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(LOCK_POLL).await;
                    }
                    Err(error) => {
                        return Err(format!("it does not take this project's login ({error})"));
                    }
                }
            };
            let row = tokio::time::timeout(
                CONNECT_WAIT,
                connection.client().query_one("SHOW data_directory", &[]),
            )
            .await
            .map_err(|_| "it does not say its data folder in time".to_owned())?
            .map_err(|e| format!("it does not say its data folder ({})", DbError::from(e)))?;
            let said: String = row
                .try_get(0)
                .map_err(|e| format!("it does not say its data folder ({e})"))?;
            let theirs = fs::canonicalize(&said)
                .map_err(|e| format!("its data folder {said} cannot be found ({e})"))?;
            let mine = fs::canonicalize(self.data())
                .map_err(|e| format!("the data folder cannot be found ({e})"))?;
            if theirs == mine {
                Ok(())
            } else {
                Err(format!("its data folder is {said}"))
            }
        };
        if let Err(why) = ours.await {
            return Err(format!(
                "a process ({}) holds the data folder {} and is not this project's Postgres as far as can be told ({why}): stop it, then run the command again",
                pid.as_raw_nonzero(),
                self.data().display()
            ));
        }
        for signal in [Signal::INT, Signal::QUIT] {
            kill_process(pid, signal).ok();
            if gone(pid, STOP_WAIT).await {
                return Ok(());
            }
        }
        kill_process_group(pid, Signal::KILL).ok();
        kill_process(pid, Signal::KILL).ok();
        if gone(pid, STOP_WAIT).await {
            Ok(())
        } else {
            Err(format!(
                "the Postgres a killed run left ({pid:?}) does not stop: stop it, then run the command again"
            ))
        }
    }
}

/// Whether the process `pid` is gone within `limit`.
async fn gone(pid: Pid, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if test_kill_process(pid).is_err() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The private Postgres, running. Stop it with [`Server::stop`]; dropped
/// running, it is killed.
pub(super) struct Server {
    child: tokio::process::Child,
    pid: Option<Pid>,
    pub port: u16,
}

impl Server {
    /// Starts the server on the data folder of `local`, on a free port.
    pub(super) async fn start(local: &Local, programs: &Programs) -> Result<Self, String> {
        let log_path = local.dir.join("postgres.log");
        let mut last = String::new();
        for _ in 0..PORT_ATTEMPTS {
            let port = free_port()?;
            let log = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&log_path)
                .map_err(|e| format!("{} cannot be written ({e})", log_path.display()))?;
            let log_too = log
                .try_clone()
                .map_err(|e| format!("{} cannot be written ({e})", log_path.display()))?;
            let mut command = tokio::process::Command::new(programs.program("postgres"));
            command
                .arg("-D")
                .arg(local.data())
                .arg("-p")
                .arg(port.to_string())
                .args([
                    "-c",
                    "listen_addresses=127.0.0.1",
                    "-c",
                    "unix_socket_directories=",
                    "-c",
                    "max_connections=60",
                    "-c",
                    "shared_buffers=32MB",
                    // Test rows: a commit need not wait for the disk.
                    "-c",
                    "synchronous_commit=off",
                ])
                .current_dir(local.data())
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(log_too)
                .process_group(0)
                .kill_on_drop(true);
            let child = command
                .spawn()
                .map_err(|e| format!("postgres cannot be started ({e})"))?;
            let pid = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw);
            let mut server = Self { child, pid, port };
            match server.ready(local).await {
                Ok(()) => return Ok(server),
                Err(why) => {
                    server.stop().await;
                    let log = fs::read_to_string(&log_path).unwrap_or_default();
                    let in_use = log.contains("could not bind") || log.contains("in use");
                    let first: Vec<&str> = log
                        .lines()
                        .filter(|l| l.contains("FATAL") || l.contains("ERROR"))
                        .take(3)
                        .collect();
                    last = format!("{why}: {}", first.join(" / "));
                    if !in_use {
                        break;
                    }
                }
            }
        }
        Err(format!("the private Postgres did not start ({last})"))
    }

    /// Waits until `postmaster.pid` says the server is ready.
    async fn ready(&mut self, local: &Local) -> Result<(), String> {
        let pid_file = local.data().join("postmaster.pid");
        let deadline = tokio::time::Instant::now() + READY_WAIT;
        loop {
            let ready = fs::read_to_string(&pid_file)
                .ok()
                .and_then(|t| t.lines().nth(7).map(|l| l.trim() == "ready"))
                .unwrap_or(false);
            if ready {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!("postgres ended ({status}) before it was ready"));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "postgres was not ready within {} s",
                    READY_WAIT.as_secs()
                ));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Stops the server (see the module's text) and waits for it.
    pub(super) async fn stop(mut self) {
        if let Some(pid) = self.pid {
            for signal in [Signal::INT, Signal::QUIT] {
                kill_process(pid, signal).ok();
                if tokio::time::timeout(STOP_WAIT, self.child.wait())
                    .await
                    .is_ok()
                {
                    // Its backends ended before it did; anything else of
                    // its group goes too.
                    kill_process_group(pid, Signal::KILL).ok();
                    self.pid = None;
                    return;
                }
            }
            kill_process_group(pid, Signal::KILL).ok();
        }
        self.child.start_kill().ok();
        tokio::time::timeout(STOP_WAIT, self.child.wait())
            .await
            .ok();
        self.pid = None;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Not stopped: the whole group is killed now (the child itself
        // again by tokio as it is dropped).
        if let Some(pid) = self.pid.take() {
            kill_process_group(pid, Signal::KILL).ok();
        }
    }
}

/// A port free on [`HOST`] at this moment.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind((HOST, 0))
        .map_err(|e| format!("no port is free on {HOST} ({e})"))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("no port is free on {HOST} ({e})"))
}
