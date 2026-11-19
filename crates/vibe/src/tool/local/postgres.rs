//! The project's private Postgres (blackbox/CONTRACT.md, "The local loop:
//! What a run does", steps 1 to 3): its programs ([`programs`]), the
//! project's local state under `.vibe/local/` ([`Local`]: the lock, the
//! passwords, the data), and the server itself ([`Server`]), started for a
//! run and stopped before the command ends.
//!
//! **Listening.** On `127.0.0.1` only, on a port free at that moment, with no
//! Unix socket (`unix_socket_directories` empty), and `pg_hba.conf` holding
//! one line: `scram-sha-256` from `127.0.0.1`. Its superuser
//! ([`SUPERUSER`]) and the platform's role ([`PLATFORM_ROLE`], `CREATEDB
//! CREATEROLE`, no superuser) have passwords made from the system's
//! randomness at the first run, kept only in `.vibe/local/state.json` (mode
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
use super::PgFailed;
use super::download::{self, Archive};
use crate::platform_db::{Cause, Connection, DatabaseAddress};
use tokio_postgres::error::SqlState;

/// The variable naming a Postgres the Builder has.
pub(super) const BIN_VARIABLE: &str = "VIBE_LOCAL_POSTGRES_BIN";

/// The variable naming a mirror of Maven Central.
pub(super) const MIRROR_VARIABLE: &str = "VIBE_LOCAL_POSTGRES_MIRROR";

/// The superuser initdb makes.
const SUPERUSER: &str = "vm_superuser";

/// The platform's role: what `VIBE_DATABASE_URL`'s role is in an
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

/// How long a refused connection is tried again before what a
/// `postmaster.pid` names is taken for gone: a server of ours that was just
/// started listens within it.
const REFUSED_WAIT: Duration = Duration::from_secs(3);

/// Ports tried before starting is given up.
const PORT_ATTEMPTS: usize = 5;

/// How often a lock, a server's readiness or its end is looked at.
const POLL: Duration = Duration::from_millis(10);

/// How often the lock is tried while another command holds it.
pub(super) const LOCK_POLL: Duration = Duration::from_millis(100);

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
/// `$XDG_DATA_HOME/vibe/postgres/` when that is absolute, else
/// `$HOME/.local/share/vibe/postgres/`.
fn data_folder() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    absolute("XDG_DATA_HOME")
        .map(|d| d.join("vibe").join("postgres"))
        .or_else(|| absolute("HOME").map(|h| h.join(".local/share/vibe/postgres")))
}

/// The programs: those `VIBE_LOCAL_POSTGRES_BIN` names, or those in the
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
    // `postgres (PostgreSQL) 18.6`, and whatever a distribution adds after
    // the number (`(Homebrew)`, `(Debian 18.6-1)`) or in it (`18beta1`).
    let major = said
        .split_once("(PostgreSQL)")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(|number| {
            number
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|m| m.parse::<u32>().ok());
    match major {
        Some(16..=18) if ran.status.success() => Ok(()),
        Some(major) => Err(format!(
            "holds Postgres {major}, and the local run needs 16, 17 or 18"
        )),
        None => Err("has a postgres that does not say its version".to_owned()),
    }
}

/// The project's local state: `.vibe/local/`.
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

/// How waiting for the project's lock ended.
pub(super) enum Taken<T> {
    Lock(Lock),
    /// What the caller turned to instead.
    Other(T),
}

/// The lock of the project's local runs: held while the file is open; the
/// kernel lets go of it when the process ends, however it ends.
pub(super) struct Lock {
    _file: File,
}

impl Local {
    pub(super) fn of(project: &Path) -> Self {
        Self {
            dir: project.join(".vibe").join("local"),
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

    /// Makes `.vibe/local/` (mode `0700`) unless it is there.
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
                    _ => Err(format!("{} is not a folder", self.dir.display())),
                }
            }
            Err(error) => Err(cannot(error)),
        }
    }

    /// Empties the private Postgres's data folder, for a reset: what the
    /// local runs made goes with it. The caller holds the lock and has
    /// stopped the server.
    pub(super) fn empty_data(&self) -> Result<(), String> {
        let data = self.data();
        empty_folder(&data)
            .map_err(|error| format!("{} cannot be emptied ({error})", data.display()))
    }

    /// New passwords, for a reset: the state written anew, whole.
    pub(super) fn renew_state(&self) -> Result<State, StateFailed> {
        let state = State {
            superuser: Secret::generate().map_err(StateFailed::NoRandomness)?,
            platform: Secret::generate().map_err(StateFailed::NoRandomness)?,
            ready: false,
        };
        self.write_state(&state).map_err(StateFailed::State)?;
        Ok(state)
    }

    /// Takes the lock, trying every [`LOCK_POLL`] while another command
    /// holds it; the caller bounds the wait. While it is held, `other` is
    /// asked at each try whether what holds it is something the caller
    /// would rather turn to (a `vibe dev` that now listens): if so, the wait
    /// ends with that. `waiting` is called once, when the lock was found
    /// held at two tries in a row: a command that only looks whether the
    /// lock is held takes it for a moment, which is not worth a line.
    pub(super) async fn lock<T>(
        &self,
        waiting: impl FnOnce(),
        mut other: impl FnMut() -> Option<T>,
    ) -> Result<Taken<T>, String> {
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
        let mut held_before = false;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Taken::Lock(Lock { _file: file })),
                Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {
                    if let Some(found) = other() {
                        return Ok(Taken::Other(found));
                    }
                    if held_before && let Some(waiting) = waiting.take() {
                        waiting();
                    }
                    held_before = true;
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(error) => return Err(format!("{} cannot be locked ({error})", path.display())),
            }
        }
    }

    /// The state, made (new passwords) when it is not there.
    pub(super) fn state(&self) -> Result<State, StateFailed> {
        let path = self.dir.join("state.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| StateFailed::State(self.broken()))?;
                let text = |key: &str| {
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|s| Secret(s.to_owned()))
                        .ok_or_else(|| StateFailed::State(self.broken()))
                };
                Ok(State {
                    superuser: text("superuser")?,
                    platform: text("platform")?,
                    ready: value.get("ready").and_then(Value::as_bool).unwrap_or(false),
                })
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if self.data().exists() {
                    return Err(StateFailed::State(self.broken()));
                }
                let state = State {
                    superuser: Secret::generate().map_err(StateFailed::NoRandomness)?,
                    platform: Secret::generate().map_err(StateFailed::NoRandomness)?,
                    ready: false,
                };
                self.write_state(&state).map_err(StateFailed::State)?;
                Ok(state)
            }
            Err(error) => Err(StateFailed::State(format!(
                "{} cannot be read ({error})",
                path.display()
            ))),
        }
    }

    fn broken(&self) -> String {
        format!(
            "{} cannot be read: the local databases cannot be opened",
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
    /// `pg_hba.conf` its one line. `own` is the command's own folder. Its
    /// step is `initdb`: making the data folder with initdb; it fails
    /// `timed-out` past [`INITDB_WAIT`], else `ended` (initdb failed). A
    /// data folder or `pg_hba.conf` that cannot be written is `local-state`.
    pub(super) async fn initdb(
        &self,
        programs: &Programs,
        state: &State,
        own: &Path,
    ) -> Result<(), StartFailed> {
        const STEP: &str = "initdb";
        const WHAT: &str = "making its data folder with initdb";
        let ended = |said: String| PgFailed::new(STEP, WHAT, Cause::Ended).said(said);
        let data = self.data();
        if data.join("PG_VERSION").is_file() {
            return Ok(());
        }
        // A folder left without a cluster (a reset's, or an initdb that
        // failed) is emptied, not removed: initdb takes an empty folder.
        empty_folder(&data).map_err(|e| {
            StartFailed::State(format!("{} cannot be made anew ({e})", data.display()))
        })?;
        // In the command's own folder (private, in TMPDIR), not the project.
        let password_file = own.join(".initdb-password");
        fs::remove_file(&password_file).ok();
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&password_file)
            .and_then(|mut file| file.write_all(state.superuser.0.as_bytes()));
        if let Err(error) = written {
            fs::remove_file(&password_file).ok();
            return Err(StartFailed::Own(format!(
                "{} cannot be written ({error})",
                password_file.display()
            )));
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
        let failed = |failed: StartFailed| {
            fs::remove_dir_all(&data).ok();
            failed
        };
        let ran = ran
            .map_err(|_| failed(PgFailed::timed_out(STEP, WHAT, INITDB_WAIT).into()))?
            .map_err(|e| failed(ended(format!("initdb cannot be run ({e})")).into()))?;
        if !ran.status.success() {
            let words = ran.err.text();
            let first: Vec<&str> = words
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(4)
                .collect();
            return Err(failed(
                ended(format!("initdb failed: {}", first.join(" / "))).into(),
            ));
        }
        fs::write(
            data.join("pg_hba.conf"),
            format!("# Written by vibe: password logins from {HOST} only.\nhost all all {HOST}/32 scram-sha-256\n"),
        )
        .map_err(|e| failed(StartFailed::State(format!("pg_hba.conf cannot be written ({e})"))))
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

    /// The first run's set-up, once the server is up: the platform's role
    /// (step `platform-role`: making the platform's role, as the
    /// superuser). The caller records it done (`state.ready`).
    pub(super) async fn set_up(&self, state: &State, port: u16) -> Result<(), PgFailed> {
        const STEP: &str = "platform-role";
        const WHAT: &str = "making the platform's role";
        let address = self.superuser_address(state, port, "postgres");
        let connection = Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT)
            .await
            .map_err(|e| PgFailed::db(STEP, "connecting as its superuser", &e))?;
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
            .map_err(|_| PgFailed::timed_out(STEP, WHAT, CONNECT_WAIT))?
            .map_err(|e| PgFailed::postgres(STEP, WHAT, &e))?;
        drop(connection);
        Ok(())
    }

    /// Stops a server a run killed outright left running, once it has shown
    /// it is this project's: it takes this project's superuser and
    /// password on the port `postmaster.pid` names and says its data folder
    /// is this one. A file whose server does not is not believed: nobody is
    /// signalled, the file is removed (Postgres would not start while the
    /// process it names lives), and the run goes on. Nothing when there is
    /// no `postmaster.pid`.
    pub(super) async fn stop_leftover(&self, state: &State) -> Result<(), String> {
        let pid_file = self.data().join("postmaster.pid");
        let Ok(text) = fs::read_to_string(&pid_file) else {
            return Ok(());
        };
        let not_believed = || {
            fs::remove_file(&pid_file)
                .or_else(|e| {
                    if e.kind() == ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| format!("{} cannot be removed ({e})", pid_file.display()))
        };
        let mut lines = text.lines();
        let pid = lines
            .next()
            .and_then(|l| l.trim().parse::<i32>().ok())
            .and_then(Pid::from_raw);
        let port = lines.nth(2).and_then(|l| l.trim().parse::<u16>().ok());
        let (Some(pid), Some(port)) = (pid, port) else {
            return not_believed();
        };
        if test_kill_process(pid).is_err() {
            return not_believed();
        }
        match self.answers_as_ours(state, port).await {
            Answer::Ours => {}
            Answer::NotOurs => {
                // A Postgres whose working folder is this data folder (it
                // works in it) is ours even when it does not answer: it is
                // left alone, and its file is not removed.
                let cwd = fs::read_link(format!("/proc/{}/cwd", pid.as_raw_nonzero()))
                    .ok()
                    .and_then(|c| fs::canonicalize(c).ok());
                let data = fs::canonicalize(self.data()).ok();
                if cwd.is_some() && cwd == data {
                    return Err(
                        "a Postgres of this project's data is running and does not answer as this project's: wait for the command that started it to end and run it again, or stop it".to_owned(),
                    );
                }
                return not_believed();
            }
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
                "this project's Postgres that a killed run left (process {}) does not stop, even when killed: run the command again in a moment",
                pid.as_raw_nonzero()
            ))
        }
    }

    /// Whether a server on `port` is this project's: it takes this
    /// project's superuser and password, and says its data folder is this
    /// one. One starting up or shutting down (`57P03`) is waited for up to
    /// [`READY_WAIT`]; a refused connection is tried again for
    /// [`REFUSED_WAIT`].
    async fn answers_as_ours(&self, state: &State, port: u16) -> Answer {
        let address = self.superuser_address(state, port, "postgres");
        let started = tokio::time::Instant::now();
        let connection = loop {
            match Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT).await {
                Ok(connection) => break connection,
                Err(error) => {
                    let waited = started.elapsed();
                    let again = match error.code() {
                        Some(code) => *code == SqlState::CANNOT_CONNECT_NOW && waited < READY_WAIT,
                        // No SQLSTATE: nothing answered (refused, or no
                        // answer in time).
                        None => !error.is_timeout() && waited < REFUSED_WAIT,
                    };
                    if !again {
                        return Answer::NotOurs;
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
            }
        };
        let said = tokio::time::timeout(
            CONNECT_WAIT,
            connection.client().query_one("SHOW data_directory", &[]),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|row| row.try_get::<_, String>(0).ok());
        drop(connection);
        let theirs = said.and_then(|s| fs::canonicalize(s).ok());
        let mine = fs::canonicalize(self.data()).ok();
        if theirs.is_some() && theirs == mine {
            Answer::Ours
        } else {
            Answer::NotOurs
        }
    }
}

/// Removes everything inside `folder`, which stays; nothing when it is not
/// there. No link is followed: a link inside is removed, not its target.
fn empty_folder(folder: &Path) -> std::io::Result<()> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Why the local state could not be had: `.vibe/local/` that cannot be used
/// (`local-state`), or no randomness for a password or a secret (`failed`:
/// removing `.vibe/local/` would not help).
pub(super) enum StateFailed {
    State(String),
    NoRandomness(String),
}

impl StateFailed {
    pub(super) fn ending(&self, command: &str) -> super::super::Ending {
        match self {
            Self::State(why) => super::local_state_failed(why, command),
            Self::NoRandomness(why) => super::super::Ending::new(
                1,
                "failed",
                format!(
                    "{command} could not make a secret: {why}. The operating system gave no randomness, which is not a fault of .vibe/local/; run {command} again"
                ),
            ),
        }
    }
}

/// Why the private Postgres could not be made or started: Postgres's own
/// failure (`postgres-failed`); `.vibe/local/` that cannot be written
/// (`local-state`); or something of the program's own that is not
/// Postgres's, the command's own folder or a free port (`failed`).
pub(super) enum StartFailed {
    Pg(PgFailed),
    State(String),
    Own(String),
}

impl From<PgFailed> for StartFailed {
    fn from(failed: PgFailed) -> Self {
        Self::Pg(failed)
    }
}

impl StartFailed {
    pub(super) fn ending(&self, command: &str) -> super::super::Ending {
        match self {
            Self::Pg(failed) => failed.ending(command),
            Self::State(why) => super::local_state_failed(why, command),
            Self::Own(why) => super::super::Ending::new(
                1,
                "failed",
                format!(
                    "{command} could not start the private Postgres: {why}. Run {command} again"
                ),
            ),
        }
    }
}

/// Why a server started was not ready.
enum Ready {
    Ended(std::process::ExitStatus),
    /// Not within [`READY_WAIT`].
    TimedOut,
}

/// What a server named by a `postmaster.pid` showed itself to be.
enum Answer {
    Ours,
    NotOurs,
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
    /// Its step is `start`: starting it until it says it is ready; it
    /// fails `timed-out` past [`READY_WAIT`], else `ended`.
    pub(super) async fn start(local: &Local, programs: &Programs) -> Result<Self, StartFailed> {
        const STEP: &str = "start";
        const WHAT: &str = "starting";
        let ended = |said: String| PgFailed::new(STEP, WHAT, Cause::Ended).said(said);
        let log_path = local.dir.join("postgres.log");
        let mut last = ended(String::new());
        for _ in 0..PORT_ATTEMPTS {
            let port = free_port().map_err(StartFailed::Own)?;
            // In append mode (`O_APPEND`), which Postgres inherits: each of
            // its writes goes to the end of the file as it is at that
            // moment, so emptying the file while it runs ([`trim_log`])
            // makes its next line the first, not one after a hole of the
            // size the file had. Checked by hand: with `vibe dev` running,
            // the file emptied by truncate(1), the next line Postgres wrote
            // was the file's first (76 bytes, no NUL byte). Emptied here,
            // so that a run starts it anew.
            let log = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(&log_path)
                .and_then(|file| file.set_len(0).map(|()| file))
                .map_err(|e| {
                    StartFailed::State(format!("{} cannot be written ({e})", log_path.display()))
                })?;
            let log_too = log.try_clone().map_err(|e| {
                StartFailed::State(format!("{} cannot be written ({e})", log_path.display()))
            })?;
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
                    // Everything in it can be made again (blackbox/CONTRACT.md,
                    // "The local databases do not wait on the disk"): nothing
                    // waits on the disk, and the local databases are not kept
                    // through a crash of the machine or a loss of power. A
                    // commit is still with the operating system before it is
                    // answered (synchronous_commit stays on), so a kill of vibe
                    // or of Postgres loses nothing answered.
                    "-c",
                    "fsync=off",
                    "-c",
                    "full_page_writes=off",
                ])
                .current_dir(local.data())
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(log_too)
                .process_group(0)
                .kill_on_drop(true);
            let child = command
                .spawn()
                .map_err(|e| ended(format!("postgres cannot be started ({e})")))?;
            let pid = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw);
            let mut server = Self { child, pid, port };
            match server.ready(local).await {
                Ok(()) => return Ok(server),
                Err(cause) => {
                    server.stop().await;
                    let log = fs::read_to_string(&log_path).unwrap_or_default();
                    let in_use = log.contains("could not bind") || log.contains("in use");
                    let first: Vec<&str> = log
                        .lines()
                        .filter(|l| l.contains("FATAL") || l.contains("ERROR"))
                        .take(3)
                        .collect();
                    let said = match &cause {
                        Ready::Ended(status) => {
                            format!("postgres ended ({status}) before it was ready")
                        }
                        Ready::TimedOut => "postgres did not say it was ready".to_owned(),
                    };
                    let said = if first.is_empty() {
                        said
                    } else {
                        format!("{said}: {}", first.join(" / "))
                    };
                    last = match cause {
                        Ready::Ended(_) => ended(said),
                        Ready::TimedOut => PgFailed::timed_out(STEP, WHAT, READY_WAIT).said(said),
                    };
                    if !in_use {
                        break;
                    }
                }
            }
        }
        Err(last.into())
    }

    /// Waits until `postmaster.pid` says the server is ready.
    async fn ready(&mut self, local: &Local) -> Result<(), Ready> {
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
                return Err(Ready::Ended(status));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Ready::TimedOut);
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

/// Empties `postgres.log` of `local` once it is larger than
/// [`crate::log::MAX_FILE_BYTES`], while the server writes it (see
/// [`Server::start`] for why that is safe). It reads the file system: the
/// caller runs it off the async workers.
pub(super) fn trim_log(local: &Local) {
    let path = local.dir.join("postgres.log");
    let large = fs::symlink_metadata(&path)
        .is_ok_and(|meta| meta.is_file() && meta.len() > crate::log::MAX_FILE_BYTES);
    if large && let Ok(file) = OpenOptions::new().write(true).open(&path) {
        file.set_len(0).ok();
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
