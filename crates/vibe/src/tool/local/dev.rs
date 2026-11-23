//! `vibe dev` (blackbox/CONTRACT.md, "The local loop, kept running"): the
//! project brought up as `vibe request` brings it up, then served on the
//! loopback address to the Builder's browser, as the test User chosen on
//! its page, and to the tool's own commands of the project, until a signal.
//!
//! **Parts.** [`Front`] answers every request (the server's [`Handler`]):
//! the `Host` first, then the local run's own paths (`/_vm/`), then the
//! Application's, handed to the installation's own Ingress. What changes
//! the project's state (a rebuild, a reset, a command of the tool's) is a
//! [`Job`], done one after the other by the command's own task ([`jobs`]),
//! never by a connection's; what is printed goes through [`printer`].
//!
//! **Serialization.** The state ([`State`]: the private Postgres and the
//! installation's parts on it, what the last bring-up did or how it failed,
//! which look at the project it is of) is behind one `tokio` read-write
//! lock. A request of the browser looks at the project, then holds the
//! state to read only for a moment, to compare the look with the state's;
//! when they differ it asks for a rebuild and waits for it. It then holds
//! the state to read again, from before it is handed to the Application
//! until the Application answered, so requests are served side by side. A
//! rebuild done between the two holds is served: the request is given the
//! project as it is then. A job holds the state to write: it waits for the
//! requests under way to end (at most [`WRITE_WAIT`]), and the lock, which
//! is fair, holds every request that comes meanwhile until the job is done
//! (each at most [`READ_WAIT`]). A rebuild looks at the project again
//! itself, holding the state to write, and brings it up only when that look
//! differs from the state's. Those waiting are bounded by the server's
//! connections ([`CONNECTIONS`]): one request a connection at a time.
//!
//! **The address.** The `Host` of a request is checked against the
//! Application's name as it is after the look at the project (and the
//! rebuild it caused): a request at the old name of an Application renamed
//! is `421`.
//!
//! **Files.** Every read or write of the file system goes off the tool's
//! one thread ([`run::off_thread`]): the connections are served on it.
//! `postgres.log` is emptied once larger than 16 MiB, looked at every
//! [`LOG_EVERY`]; `vibe.log` bounds itself (crate::log).
//!
//! **Bounds.** Jobs waiting: [`JOBS`] (a request that finds the queue full
//! waits for room). Lines waiting to be printed: [`LINES`] (the request
//! waits for room). A job takes at most [`JOB_WAIT`]. A form posted to the
//! page is at most [`FORM_BYTES`], read within [`BODY_WAIT`]; what a command
//! sends at most [`control::MAX_SENT_BYTES`].
//!
//! **Stopping.** `SIGINT`, `SIGTERM` or `SIGHUP`, or an output that cannot
//! be written: no connection is taken any more, those open are given
//! [`server::Limits::drain`] to finish their request then dropped, the job
//! under way is dropped (which stops the tools it runs), the parts are
//! closed, the private Postgres is stopped and waited for, the file that
//! finds this `vibe dev` removed, then the project's lock let go.

use std::io::{ErrorKind, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, COOKIE, HOST, HeaderName, HeaderValue, ORIGIN};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, RwLockWriteGuard, mpsc, oneshot, watch};

use super::super::project_tools::{
    Bounded, DEFAULT_TIMEOUT, Scratch, Signals, Who, own_folder_failed,
};
use super::super::test_users::TestUsersFile;
use super::super::theirs::Theirs;
use super::super::{Done, Ending, json_line};
use super::control::{self, ControlSecret};
use super::page::{self, COOKIE as TEST_USER_COOKIE};
use super::postgres::{self, Local, Programs, StateFailed};
use super::run::{self, Base, Brought, Held, Live, Pg, Size};
use super::walk::{self, Look};
use super::{
    Command, DRAIN_WAIT, DevArgs, Progress, Reading, Stage, WHO, WHO_DEV, WHO_LOCAL, WHO_RESET,
    one_line, read_records, request_output, reset_output, standins,
};
use crate::runner;
use crate::sent::Sent;
use crate::server::{self, Handler};
use crate::sign_in::ChosenTestUser;
use crate::user::User;

/// Connections served at once.
const CONNECTIONS: usize = 256;

/// Jobs waiting to be done.
const JOBS: usize = 64;

/// Lines waiting to be printed.
const LINES: usize = 256;

/// The longest a job waits for the requests under way to end.
const WRITE_WAIT: Duration = Duration::from_secs(60);

/// The longest a job may take.
const JOB_WAIT: Duration = Duration::from_secs(600);

/// The longest a request waits for the state (a job under way).
const READ_WAIT: Duration = Duration::from_secs(700);

/// The longest a line may take to be written, or to find room.
const OUTPUT_WAIT: Duration = Duration::from_secs(60);

/// How often `postgres.log` is looked at.
const LOG_EVERY: Duration = Duration::from_secs(10);

/// A look at the project that took longer than this is recorded in
/// `vibe.log`, beside every look that caused a rebuild.
const SLOW_LOOK: Duration = Duration::from_millis(50);

/// The largest form posted to the page.
const FORM_BYTES: usize = 64 * 1024;

/// The longest a body sent to the local run's own paths may take to come.
const BODY_WAIT: Duration = Duration::from_secs(30);

/// The private Postgres and the installation's parts on it.
struct Session {
    pg: Pg,
    live: Live,
}

/// What `vibe dev` serves from. See the module's text.
struct State {
    session: Option<Session>,
    /// What the last bring-up that ended well did, as a request that
    /// causes none reports it.
    last: Option<Brought>,
    /// How the bring-up of the project as it is now failed: what `vibe
    /// request` prints of it, and its exit code.
    failed: Option<(u8, Done)>,
    /// The look at the project the state is of.
    seen: Option<Look>,
}

/// Why `vibe dev` stops.
#[derive(Clone, Copy)]
enum Why {
    Signal,
    OutputClosed,
}

/// What the command's own task does for a request.
enum Job {
    /// Bring the project up again unless the state is of the project as it
    /// is now.
    Rebuild { reply: oneshot::Sender<()> },
    /// A command of the tool's: what it prints, and its exit code.
    Control {
        asked: Asked,
        reply: oneshot::Sender<(u8, Done)>,
    },
}

/// What a command of the tool's asks.
enum Asked {
    Request(Wanted),
    Read(Reading),
    Reset,
}

/// The request of a `vibe request` answered here, as it sent it.
struct Wanted {
    path: String,
    user: String,
    method: Method,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
}

/// What the command's own task needs to do a job.
struct Ctx<'a> {
    own: &'a Path,
    programs: &'a Programs,
    held: &'a Held,
    progress: &'a Progress,
}

/// What answers every request. See the module's text.
struct Front {
    project: Arc<Path>,
    port: u16,
    json: bool,
    /// The Application's name, as the last bring-up gave it. Held only to
    /// read or replace, never across an await.
    application: std::sync::RwLock<Arc<str>>,
    secret: Arc<ControlSecret>,
    state: RwLock<State>,
    jobs: mpsc::Sender<Job>,
    lines: mpsc::Sender<String>,
    /// Whether the project is looked at before each request: not once it
    /// had more entries than are looked at.
    looking: AtomicBool,
}

/// The parts' size: requests side by side, each within the time an
/// installation gives one.
fn dev_size() -> Size {
    Size {
        pool: 8,
        runner: runner::Limits {
            threads: std::num::NonZeroUsize::new(2).unwrap_or(std::num::NonZeroUsize::MIN),
            requests_per_thread: 8,
            queue_per_thread: 16,
            ..runner::Limits::default()
        },
    }
}

/// `vibe dev`.
pub(in super::super) async fn dev(args: &DevArgs) -> Result<Done, Ending> {
    let name = WHO_DEV.command;
    let project = super::project_path(args.project.as_deref(), name)?;
    run::test_users_of(&project, name)?;
    let local = Local::of(&project);
    if let Some(address) = control::address_of(&local) {
        return Err(already_running(&address));
    }
    let listeners = bind(args.port)?;
    let mut signals = Signals::listen(&WHO_DEV)?;
    let own = Scratch::make(&WHO_DEV).map_err(|e| own_folder_failed(&e, &WHO_DEV))?;
    let held = Held::default();
    let json = args.output.json;
    let command = Command {
        who: &WHO_DEV,
        json,
        limit: DEFAULT_TIMEOUT,
    };
    let progress = Progress::new(Stage::Node);
    let base = command.base(&project, &own.path);
    let started = match signals
        .within(DEFAULT_TIMEOUT, start(&base, &progress, &held))
        .await
    {
        Bounded::Ended(Ok(started)) => started,
        ended => {
            held.release().await;
            control::remove_left(&local);
            return match ended {
                Bounded::Ended(Err(ending)) => Err(ending),
                Bounded::TimedOut => Err(command.timed_out(progress.get())),
                // Interrupted before it listened: it ends as it would after.
                Bounded::Interrupted(_) | Bounded::Ended(Ok(_)) => Ok(Done::printed()),
            };
        }
    };
    let Started {
        programs,
        pg,
        live,
        brought,
        seen,
    } = started;
    let application: Arc<str> = Arc::from(brought.application.as_str());
    let secret = match write_control(&project, args.port, &application).await {
        Ok(secret) => secret,
        Err(failed) => {
            live.close().await;
            held.release().await;
            control::remove_left(&local);
            return Err(failed.ending(name));
        }
    };
    let (jobs_tx, jobs_rx) = mpsc::channel(JOBS);
    let (lines_tx, lines_rx) = mpsc::channel(LINES);
    let looking = seen != Look::TooMany;
    let front = Arc::new(Front {
        project: Arc::from(project.as_path()),
        port: args.port,
        json,
        application: std::sync::RwLock::new(Arc::clone(&application)),
        secret,
        state: RwLock::new(State {
            session: Some(Session { pg, live }),
            last: Some(brought.standing()),
            failed: None,
            seen: looking.then_some(seen),
        }),
        jobs: jobs_tx,
        lines: lines_tx,
        looking: AtomicBool::new(looking),
    });
    let (stop, _) = watch::channel(None::<Why>);
    let mut listening = front.line(
        &json!({
            "event": "listening",
            "url": front.address(&application),
            "application": &*application,
            "port": args.port,
            "version": brought.version,
        }),
        &format!(
            "vibe dev serves {} at {} (local Version {}), as the test User you choose there. It runs until interrupted (Ctrl-C).",
            Theirs::from(&*application),
            front.address(&application),
            brought.version
        ),
    );
    if !looking {
        listening.push_str(&front.line(&too_many_json(), &too_many_message()));
    }
    let printed = write_out(listening).await;
    if printed.is_err() {
        stop.send_replace(Some(Why::OutputClosed));
    } else {
        let ctx = Ctx {
            own: &own.path,
            programs: &programs,
            held: &held,
            progress: &progress,
        };
        let limits = server::Limits {
            connections: CONNECTIONS,
            header_read: Duration::from_secs(10),
            // A command answered here may take as long as its --timeout.
            connection: Duration::from_secs(3600),
            drain: Duration::from_secs(10),
        };
        let mut stopped = stop.subscribe();
        tokio::join!(
            stopper(&mut signals, &stop),
            server::serve_until(listeners, Arc::clone(&front), limits, None, async move {
                drop(stopped.wait_for(Option::is_some).await);
            }),
            jobs(&front, &ctx, jobs_rx, &stop),
            printer(lines_rx, &stop),
            keeper(&front.project, &stop),
        );
    }
    // Everything ended: no request holds the state.
    let session = match tokio::time::timeout(WRITE_WAIT, front.state.write()).await {
        Ok(mut state) => state.session.take(),
        Err(_) => None,
    };
    if let Some(session) = session {
        session.live.close().await;
        drop(session.pg);
    }
    control::remove_left(&local);
    held.release().await;
    drop(front);
    drop(own);
    match *stop.borrow() {
        Some(Why::OutputClosed) => Err(Ending::new(
            1,
            "output-closed",
            "vibe dev could not write its output, so it stopped, with everything it started".into(),
        )),
        _ => Ok(Done::printed()),
    }
}

/// A secret made for this run, and the file that finds this `vibe dev`
/// written with it.
async fn write_control(
    project: &Path,
    port: u16,
    application: &str,
) -> Result<Arc<ControlSecret>, StateFailed> {
    let secret = Arc::new(ControlSecret::generate().map_err(StateFailed::NoRandomness)?);
    rewrite_control(project, port, application, &secret)
        .await
        .map_err(StateFailed::State)?;
    Ok(secret)
}

/// The file that finds this `vibe dev` written (again), off the tool's thread.
async fn rewrite_control(
    project: &Path,
    port: u16,
    application: &str,
    secret: &Arc<ControlSecret>,
) -> Result<(), String> {
    // Copied: the write owns what it writes on its own thread; once a
    // start or a rename.
    let project = project.to_path_buf();
    let application = application.to_owned();
    let secret = Arc::clone(secret);
    run::off_thread(move || control::write(&Local::of(&project), port, &application, &secret))
        .await
        .unwrap_or_else(|| Err("the file that finds vibe dev could not be written".to_owned()))
}

/// `already-running`: another `vibe dev` serves this project at `address`.
fn already_running(address: &str) -> Ending {
    Ending::new(
        1,
        "already-running",
        format!(
            "vibe dev already runs in this project, at {address}: use that one, or end it (Ctrl-C where it runs) before starting another"
        ),
    )
}

/// What starting did.
struct Started {
    programs: Programs,
    pg: Pg,
    live: Live,
    brought: Brought,
    seen: Look,
}

/// What `vibe dev` does before it listens: what `vibe request` does up to the
/// request.
async fn start(base: &Base<'_>, progress: &Progress, held: &Held) -> Result<Started, Ending> {
    let packages = run::tools(base, progress).await?;
    let programs = run::programs(base, progress).await?;
    let local = Local::of(base.project);
    // Another one may begin to listen while this one waits for the lock.
    // Its file is believed when it is still there, the same, one
    // LOCK_POLL later: a file a killed `vibe dev` left is removed by the
    // next holder of the lock right after it takes the lock, so it cannot
    // be seen twice that far apart, while a listening one's stays. A file
    // seen once is not turned to again; the wait goes on.
    let mut tried: Option<control::Found> = None;
    while let Some(other) = run::lock_project(base, &local, progress, held, tried.as_ref()).await? {
        tokio::time::sleep(postgres::LOCK_POLL).await;
        if control::found_while_waiting(&local, None).is_some_and(|again| again.same(&other)) {
            return Err(already_running(&other.address()));
        }
        tried = Some(other);
    }
    let pg = run::start_server(base, &programs, progress, held, false).await?;
    let live = Live::open(&pg, dev_size(), base.who).await?;
    let brought = async {
        let file = run::test_users(base.project, base.who).await?;
        let seen = look_at(base.project).await.look;
        let brought = run::bring_up(base, &pg, &live, &file, &packages, progress).await?;
        Ok::<_, Ending>((brought, seen))
    }
    .await;
    match brought {
        Ok((brought, seen)) => Ok(Started {
            programs,
            pg,
            live,
            brought,
            seen,
        }),
        Err(ending) => {
            live.close().await;
            Err(ending)
        }
    }
}

/// Listens on `127.0.0.1:<port>`, and `[::1]:<port>` where the machine has
/// it.
fn bind(port: u16) -> Result<Vec<TcpListener>, Ending> {
    let in_use = || {
        Ending::new(
            1,
            "port-in-use",
            format!(
                "Port {port} of this machine's loopback address is taken: give vibe dev another with --port <n>, or end what holds it"
            ),
        )
    };
    let failed = |error: std::io::Error| {
        Ending::new(
            1,
            "failed",
            format!(
                "vibe dev cannot listen on 127.0.0.1:{port} ({error}): give it another port with --port <n>"
            ),
        )
    };
    let mut bound = Vec::with_capacity(2);
    match std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => bound.push(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => return Err(in_use()),
        Err(error) => return Err(failed(error)),
    }
    match std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(listener) => bound.push(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => return Err(in_use()),
        // The machine has no ::1: 127.0.0.1 alone.
        Err(_) => {}
    }
    bound
        .into_iter()
        .map(|listener| {
            listener.set_nonblocking(true).map_err(failed)?;
            TcpListener::from_std(listener).map_err(failed)
        })
        .collect()
}

/// Waits for a signal, or for another reason to stop, and says why.
async fn stopper(signals: &mut Signals, stop: &watch::Sender<Option<Why>>) {
    let mut stopped = stop.subscribe();
    tokio::select! {
        bounded = signals.within(Duration::MAX, std::future::pending::<()>()) => {
            if let Bounded::Interrupted(_) = bounded {
                stop.send_if_modified(|why| {
                    if why.is_none() {
                        *why = Some(Why::Signal);
                        true
                    } else {
                        false
                    }
                });
            }
        }
        _ = stopped.wait_for(Option::is_some) => {}
    }
}

/// Writes `text` on standard output, off the async workers, within
/// [`OUTPUT_WAIT`]: an output that takes no more for that long counts as
/// closed. (Its writing thread is then left to the runtime's end, which
/// waits a bounded time for it: see `tool::run`.)
async fn write_out(text: String) -> Result<(), ()> {
    let written = tokio::time::timeout(
        OUTPUT_WAIT,
        run::off_thread(move || {
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes()).and_then(|()| out.flush())
        }),
    )
    .await;
    match written {
        Ok(Some(Ok(()))) => Ok(()),
        _ => Err(()),
    }
}

/// Prints the lines as they come; an output that cannot be written stops
/// `vibe dev`.
async fn printer(mut lines: mpsc::Receiver<String>, stop: &watch::Sender<Option<Why>>) {
    let mut stopped = stop.subscribe();
    loop {
        let line = tokio::select! {
            biased;
            line = lines.recv() => match line {
                Some(line) => line,
                None => return,
            },
            _ = stopped.wait_for(Option::is_some) => return,
        };
        let written = tokio::select! {
            written = write_out(line) => written,
            _ = stopped.wait_for(Option::is_some) => return,
        };
        if written.is_err() {
            stop.send_if_modified(|why| {
                if why.is_none() {
                    *why = Some(Why::OutputClosed);
                    true
                } else {
                    false
                }
            });
            return;
        }
    }
}

/// Empties `postgres.log` once it is past its bound, every [`LOG_EVERY`],
/// until `vibe dev` stops.
async fn keeper(project: &Arc<Path>, stop: &watch::Sender<Option<Why>>) {
    let mut stopped = stop.subscribe();
    loop {
        tokio::select! {
            _ = stopped.wait_for(Option::is_some) => return,
            () = tokio::time::sleep(LOG_EVERY) => {}
        }
        let project = Arc::clone(project);
        run::off_thread(move || postgres::trim_log(&Local::of(&project))).await;
    }
}

/// Does the jobs one after the other, until `vibe dev` stops.
async fn jobs(
    front: &Front,
    ctx: &Ctx<'_>,
    mut queue: mpsc::Receiver<Job>,
    stop: &watch::Sender<Option<Why>>,
) {
    let mut stopped = stop.subscribe();
    loop {
        let job = tokio::select! {
            _ = stopped.wait_for(Option::is_some) => return,
            job = queue.recv() => match job {
                Some(job) => job,
                None => return,
            },
        };
        tokio::select! {
            _ = stopped.wait_for(Option::is_some) => return,
            // A job past its time is dropped, which stops what it runs; its
            // caller is answered that it could not be done.
            _ = tokio::time::timeout(JOB_WAIT, front.run_job(ctx, job)) => {}
        }
    }
}

/// The error a command's report holds.
fn error_of(report: &Done) -> &Value {
    report.json.get("error").unwrap_or(&Value::Null)
}

/// `Done` again: what is printed is kept and sent.
fn copy(done: &Done) -> Done {
    Done {
        json: done.json.clone(),
        text: done.text.clone(),
    }
}

/// A command's result as it is sent back: its exit code and what it
/// prints.
fn outcome(result: Result<Done, Ending>) -> (u8, Done) {
    match result {
        Ok(done) => (0, done),
        Err(ending) => ending.printed(),
    }
}

fn too_many_message() -> String {
    format!(
        "The project has more than {} files and folders (those under node_modules/, .vibe/ and .git/ aside), so vibe dev does not look for changes before each request: a change is served once vibe dev is started again.",
        walk::MAX_ENTRIES
    )
}

fn too_many_json() -> Value {
    json!({ "event": "too-many-files", "message": too_many_message() })
}

/// A look at the project, and what it cost.
struct Looked {
    look: Look,
    entries: usize,
    took: Duration,
}

impl Looked {
    /// Writes it in `vibe.log`.
    fn record(&self) {
        crate::log::record(
            "project-looked-at",
            json!({
                "entries": self.entries,
                "micros": u64::try_from(self.took.as_micros()).unwrap_or(u64::MAX),
            }),
        );
    }
}

/// The project looked at, off the tool's thread. Recorded in `vibe.log` when
/// it took longer than [`SLOW_LOOK`] (a rebuild records the look that
/// caused it).
async fn look_at(project: &Path) -> Looked {
    // Copied: the walk owns its path on its own thread; once a look.
    let project = project.to_path_buf();
    let started = tokio::time::Instant::now();
    let (look, entries) = run::off_thread(move || walk::look(&project))
        .await
        .unwrap_or((Look::TooMany, 0));
    let looked = Looked {
        look,
        entries,
        took: started.elapsed(),
    };
    if looked.took > SLOW_LOOK {
        looked.record();
    }
    looked
}

impl Front {
    fn application(&self) -> Arc<str> {
        Arc::clone(
            &self
                .application
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// `http://<application>.localhost:<port>/`.
    fn address(&self, application: &str) -> String {
        format!("http://{application}.localhost:{}/", self.port)
    }

    /// One line of output: `json` with `--json`, else `words`.
    fn line(&self, json: &Value, words: &str) -> String {
        if self.json {
            json_line(json)
        } else {
            format!("{words}\n")
        }
    }

    /// Prints a line, after those before it.
    async fn say(&self, json: Value, words: String) {
        let line = self.line(&json, &words);
        // Bound: LINES waiting; when full, this waits for room up to
        // OUTPUT_WAIT, then the line is not printed (the output takes
        // nothing: it is about to count as closed).
        tokio::time::timeout(OUTPUT_WAIT, self.lines.send(line))
            .await
            .ok();
    }

    /// The project looked at, unless it is no longer looked at.
    async fn look(&self) -> Option<Looked> {
        if !self.looking.load(Ordering::Acquire) {
            return None;
        }
        let looked = look_at(&self.project).await;
        if looked.look == Look::TooMany {
            if self.looking.swap(false, Ordering::AcqRel) {
                self.say(too_many_json(), too_many_message()).await;
            }
            return None;
        }
        Some(looked)
    }

    /// The Application's name after a bring-up: the file that finds `vibe
    /// dev` says it too.
    async fn named(&self, application: &str) {
        let changed = {
            let mut name = self
                .application
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if &**name == application {
                false
            } else {
                *name = Arc::from(application);
                true
            }
        };
        if changed {
            rewrite_control(&self.project, self.port, application, &self.secret)
                .await
                .ok();
        }
    }

    async fn write_state(&self) -> Option<RwLockWriteGuard<'_, State>> {
        tokio::time::timeout(WRITE_WAIT, self.state.write())
            .await
            .ok()
    }

    async fn run_job(&self, ctx: &Ctx<'_>, job: Job) {
        match job {
            Job::Rebuild { reply } => {
                // Looked at again here, holding the state to write: the look
                // of the request that asked may be older than another's.
                if let Some(mut state) = self.write_state().await
                    && let Some(looked) = self.look().await
                    && state.seen != Some(looked.look)
                {
                    self.rebuild(ctx, &mut state, &looked, &WHO).await;
                }
                reply.send(()).ok();
            }
            Job::Control { asked, reply } => {
                let answered = match asked {
                    Asked::Request(wanted) => outcome(self.forwarded_request(ctx, &wanted).await),
                    Asked::Read(reading) => outcome(self.forwarded_read(ctx, &reading).await),
                    Asked::Reset => outcome(self.forwarded_reset(ctx).await),
                };
                reply.send(answered).ok();
            }
        }
    }

    /// Opens the private Postgres and the parts again (after a reset that
    /// failed half way).
    async fn session(&self, ctx: &Ctx<'_>, base: &Base<'_>) -> Result<Session, Ending> {
        let pg = run::start_server(base, ctx.programs, ctx.progress, ctx.held, false).await?;
        let live = Live::open(&pg, dev_size(), base.who).await?;
        Ok(Session { pg, live })
    }

    fn base<'a>(&'a self, ctx: &Ctx<'a>, who: &'a Who) -> Base<'a> {
        Base {
            project: &self.project,
            own: ctx.own,
            who,
            json: self.json,
        }
    }

    /// Brings the project up again (see `run::bring_up`), records how it
    /// went in `state` as of `seen`, and says so. What it did, when it
    /// ended well.
    async fn rebuild(
        &self,
        ctx: &Ctx<'_>,
        state: &mut State,
        looked: &Looked,
        who: &Who,
    ) -> Option<Brought> {
        if looked.took <= SLOW_LOOK {
            looked.record();
        }
        let base = self.base(ctx, who);
        let result = async {
            let file = run::test_users(base.project, who).await?;
            let packages = run::packages(base.project, who).await?;
            if state.session.is_none() {
                state.session = Some(self.session(ctx, &base).await?);
            }
            let Some(session) = state.session.as_ref() else {
                return Err(super::PgFailed::new(
                    "start",
                    "starting it (it is not running)",
                    crate::platform_db::Cause::Ended,
                )
                .ending(who.command));
            };
            run::bring_up(
                &base,
                &session.pg,
                &session.live,
                &file,
                &packages,
                ctx.progress,
            )
            .await
        }
        .await;
        state.seen = Some(looked.look);
        self.brought(state, result).await
    }

    /// Records in `state` how a bring-up went, and says so.
    async fn brought(&self, state: &mut State, result: Result<Brought, Ending>) -> Option<Brought> {
        match result {
            Ok(brought) => {
                state.failed = None;
                state.last = Some(brought.standing());
                self.named(&brought.application).await;
                let url = self.address(&brought.application);
                self.say(
                    json!({
                        "event": "rebuilt",
                        "application": brought.application,
                        "url": url,
                        "version": brought.version,
                        "migrations_applied": brought.migrations_applied,
                        "standins": brought.standins_json(),
                        "test_rows": brought.test_rows_json(),
                    }),
                    rebuilt_words(&brought, &url),
                )
                .await;
                Some(brought)
            }
            Err(ending) => {
                let (code, report) = ending.printed();
                let error = error_of(&report);
                let words = format!(
                    "The project could not be brought up again ({}): {}",
                    one_line(error.get("reason").unwrap_or(&Value::Null)),
                    one_line(error.get("message").unwrap_or(&Value::Null))
                );
                self.say(json!({ "event": "rebuild-failed", "error": error }), words)
                    .await;
                state.failed = Some((code, report));
                None
            }
        }
    }

    /// `vibe request`, answered here: served as a request of the browser is,
    /// after the same look at the project, as the User it names.
    async fn forwarded_request(&self, ctx: &Ctx<'_>, wanted: &Wanted) -> Result<Done, Ending> {
        let Some(mut state) = self.write_state().await else {
            return Err(busy(&WHO));
        };
        let mut caused = None;
        if let Some(looked) = self.look().await
            && state.seen != Some(looked.look)
        {
            caused = self.rebuild(ctx, &mut state, &looked, &WHO).await;
        }
        if let Some((code, report)) = &state.failed {
            return Err(Ending::from_printed(*code, copy(report)));
        }
        let brought = match caused {
            Some(brought) => brought,
            None => state
                .last
                .as_ref()
                .map(Brought::standing)
                .ok_or_else(|| busy(&WHO))?,
        };
        let Some(session) = state.session.as_ref() else {
            return Err(busy(&WHO));
        };
        let file = run::test_users(&self.project, &WHO).await?;
        let Some(user) = file.user(&wanted.user) else {
            return Err(run::no_such_test_user(&file, &wanted.user));
        };
        let asked = run::Asked {
            path: &wanted.path,
            user,
            method: wanted.method.clone(),
            headers: &wanted.headers,
            // A cheap copy: `Bytes` is shared.
            body: wanted.body.clone(),
        };
        let served = run::serve_and_collect(
            &session.live,
            &brought.application,
            &asked,
            ctx.progress,
            &WHO,
        )
        .await?;
        if served.given_to_code {
            self.say_request(
                &user.id,
                &wanted.method,
                &wanted.path,
                served.parts.status,
                &served.request_id,
            )
            .await;
        }
        Ok(request_output(&brought, &served, &asked))
    }

    /// `vibe local operation-log` or `change-history`, answered here, once
    /// what the requests before recorded is in.
    async fn forwarded_read(&self, ctx: &Ctx<'_>, reading: &Reading) -> Result<Done, Ending> {
        let Some(mut state) = self.write_state().await else {
            return Err(busy(&WHO_LOCAL));
        };
        if state.session.is_none() {
            let base = self.base(ctx, &WHO_LOCAL);
            state.session = Some(self.session(ctx, &base).await?);
        }
        let Some(session) = state.session.as_ref() else {
            return Err(busy(&WHO_LOCAL));
        };
        // No request runs (the state is held): every operation's task
        // under way is a finished request's.
        session.live.gateway.drain(DRAIN_WAIT).await;
        let application = self.application();
        if session.live.own.has_own_database(&application) {
            tokio::time::timeout(DRAIN_WAIT, session.live.own.ship_and_wait(&application))
                .await
                .ok();
        }
        session.live.ingress.logs.settled(DRAIN_WAIT).await;
        read_records(&session.live.database, reading, WHO_LOCAL.command).await
    }

    /// `vibe local reset`, done here: the parts closed, the private Postgres
    /// stopped and made anew, the project brought up on it.
    async fn forwarded_reset(&self, ctx: &Ctx<'_>) -> Result<Done, Ending> {
        let Some(mut state) = self.write_state().await else {
            return Err(busy(&WHO_RESET));
        };
        // Forgotten first: should the reset be dropped half way (past its
        // time), the next request finds the state of no look and brings the
        // project up again.
        state.seen = None;
        let looked = self.look().await;
        if let Some(session) = state.session.take() {
            session.live.close().await;
            drop(session.pg);
        }
        ctx.held.stop_server().await;
        let base = self.base(ctx, &WHO_RESET);
        let result = async {
            let file = run::test_users(base.project, &WHO_RESET).await?;
            let packages = run::packages(base.project, &WHO_RESET).await?;
            let pg = run::start_server(&base, ctx.programs, ctx.progress, ctx.held, true).await?;
            let live = Live::open(&pg, dev_size(), &WHO_RESET).await?;
            let session = state.session.insert(Session { pg, live });
            run::bring_up(
                &base,
                &session.pg,
                &session.live,
                &file,
                &packages,
                ctx.progress,
            )
            .await
        }
        .await;
        state.seen = looked.map(|looked| looked.look);
        match result {
            Ok(brought) => {
                state.failed = None;
                state.last = Some(brought.standing());
                self.named(&brought.application).await;
                Ok(reset_output(&brought))
            }
            Err(ending) => {
                let (code, report) = ending.printed();
                state.failed = Some((code, copy(&report)));
                Err(Ending::from_printed(code, report))
            }
        }
    }

    /// The line of a request the Application's code was given.
    async fn say_request(
        &self,
        user: &str,
        method: &Method,
        path: &str,
        status: StatusCode,
        request_id: &str,
    ) {
        self.say(
            json!({
                "event": "request",
                "as": user,
                "method": method.as_str(),
                "path": path,
                "status": status.as_u16(),
                "request_id": request_id,
            }),
            format!(
                "{} {} {} as {} (request {})",
                Theirs::from(method.as_str()),
                Theirs::from(path),
                status.as_u16(),
                Theirs::from(user),
                Theirs::from(request_id)
            ),
        )
        .await;
    }

    /// Answers one request. See the module's text.
    async fn answer(&self, request: Request<Incoming>) -> Response<Sent> {
        // Copied: compared again after the look, while the request is still
        // held; a few bytes, once a request.
        let host = request
            .headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let own_path = request.uri().path().starts_with("/_vm/");
        // The project looked at first (and brought up again when it
        // changed) for a request that may be the Application's: at its
        // name, or at another name of the loopback's `.localhost` (the name
        // it may have now).
        let ready = if own_path {
            None
        } else {
            match self.which(&host, &self.application()) {
                Which::Ours => Some(self.up_to_date().await),
                Which::Other if is_localhost_name(&host) => Some(self.up_to_date().await),
                Which::Other | Which::Loopback => None,
            }
        };
        // The name as it is now, after any rebuild.
        let application = self.application();
        let address = self.address(&application);
        match self.which(&host, &application) {
            Which::Ours => {}
            Which::Loopback => {
                let target = request.uri().path_and_query().map_or("/", |p| p.as_str());
                return page::redirect(
                    StatusCode::TEMPORARY_REDIRECT,
                    &format!("http://{application}.localhost:{}{target}", self.port),
                )
                .map(Sent::from);
            }
            Which::Other => return page::misdirected(&address).map(Sent::from),
        }
        if own_path {
            return self.own_path(request, &application).await.map(Sent::from);
        }
        if let Some(Err(answer)) = ready {
            return (*answer).map(Sent::from);
        }
        self.application_request(request, &address).await
    }

    /// Whose `host` is: the Application's, the loopback address's, or
    /// another's.
    fn which(&self, host: &str, application: &str) -> Which {
        let host = host.trim().to_ascii_lowercase();
        let (name, port) = if host.starts_with('[') {
            match host.split_once(']') {
                Some((inside, "")) => (format!("{inside}]"), None),
                Some((inside, after)) => match after.strip_prefix(':') {
                    Some(port) => (format!("{inside}]"), Some(port.to_owned())),
                    None => return Which::Other,
                },
                None => return Which::Other,
            }
        } else {
            match host.rsplit_once(':') {
                Some((name, port)) => (name.to_owned(), Some(port.to_owned())),
                None => (host.clone(), None),
            }
        };
        if let Some(port) = port
            && port.parse::<u16>().ok() != Some(self.port)
        {
            return Which::Other;
        }
        if name == format!("{application}.localhost") {
            Which::Ours
        } else if ["localhost", "127.0.0.1", "[::1]"].contains(&name.as_str()) {
            Which::Loopback
        } else {
            Which::Other
        }
    }

    /// A path under `/_vm/`: the page, the control paths, or nothing.
    async fn own_path(
        &self,
        request: Request<Incoming>,
        application: &str,
    ) -> Response<Full<Bytes>> {
        let path = request.uri().path();
        if path == page::CHOOSER {
            return match *request.method() {
                Method::GET | Method::HEAD => self.chooser(&request, application).await,
                Method::POST => self.choose(request, application).await,
                _ => page::method_not_allowed(),
            };
        }
        if let Some(what) = path.strip_prefix("/_vm/control/") {
            let held = request
                .headers()
                .get(control::SECRET_HEADER)
                .is_some_and(|given| self.secret.matches(given.as_bytes()));
            if held && request.method() == Method::POST {
                // Copied: the request's body is read after; a few bytes.
                let what = what.to_owned();
                return self.control(request, &what).await;
            }
        }
        page::not_found()
    }

    /// The chosen test User's id, from the cookie. The `Cookie` lines are
    /// read as bytes: another cookie's value may hold any byte.
    fn chosen(headers: &hyper::HeaderMap) -> Option<String> {
        headers
            .get_all(COOKIE)
            .iter()
            .flat_map(|v| cookie_pairs(v.as_bytes()))
            .filter_map(|pair| {
                let at = pair.iter().position(|b| *b == b'=')?;
                let (name, value) = (pair.get(..at)?, pair.get(at.saturating_add(1)..)?);
                (name == TEST_USER_COOKIE.as_bytes()).then_some(value)
            })
            .find_map(|value| page::percent_decode(std::str::from_utf8(value).ok()?, false))
    }

    /// `vibe.dev.json` as it is now, and (for the page, `with_pulled`) the
    /// pulled Resources, read off the async workers.
    async fn users(&self, with_pulled: bool) -> Result<(TestUsersFile, Vec<String>), Ending> {
        let project = Arc::clone(&self.project);
        run::off_thread(move || {
            let file = run::test_users_of(&project, WHO.command)?;
            let pulled = if with_pulled {
                standins::pulled(&project, WHO.command)
                    .files
                    .into_iter()
                    .map(|f| f.resource)
                    .collect()
            } else {
                Vec::new()
            };
            Ok((file, pulled))
        })
        .await
        .unwrap_or_else(|| Err(run::read_failed(WHO.command)))
    }

    /// `GET /_vm/test-user`.
    async fn chooser(
        &self,
        request: &Request<Incoming>,
        application: &str,
    ) -> Response<Full<Bytes>> {
        let next = request
            .uri()
            .query()
            .map(page::form_fields)
            .and_then(|fields| fields.into_iter().find(|(n, _)| n == "next"))
            .map(|(_, v)| v);
        let next = page::local_path(next.as_deref());
        let chosen = Self::chosen(request.headers());
        match self.users(true).await {
            Ok((file, pulled)) => page::chooser(
                StatusCode::OK,
                application,
                &file,
                &pulled,
                chosen.as_deref(),
                &next,
                &page::Said::Nothing,
            ),
            Err(ending) => page::failed(error_of(&ending.printed().1)),
        }
    }

    /// `POST /_vm/test-user`.
    async fn choose(&self, request: Request<Incoming>, application: &str) -> Response<Full<Bytes>> {
        let own_origin = format!("http://{application}.localhost:{}", self.port);
        if let Some(origin) = request.headers().get(ORIGIN)
            && !origin
                .as_bytes()
                .eq_ignore_ascii_case(own_origin.as_bytes())
        {
            return page::other_origin();
        }
        let chosen = Self::chosen(request.headers());
        let Some(body) = read_body(request.into_body(), FORM_BYTES).await else {
            return page::plain(
                StatusCode::BAD_REQUEST,
                "The form did not come whole, or is larger than 64 KiB.\n".to_owned(),
            );
        };
        let fields = page::form_fields(&String::from_utf8_lossy(&body));
        let field = |name: &str| {
            fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        let next = page::local_path(field("next"));
        let (file, pulled) = match self.users(true).await {
            Ok(read) => read,
            Err(ending) => return page::failed(error_of(&ending.printed().1)),
        };
        let user = field("user").unwrap_or_default();
        if file.user(user).is_some() {
            page::chosen(&next, user)
        } else {
            page::chooser(
                StatusCode::BAD_REQUEST,
                application,
                &file,
                &pulled,
                chosen.as_deref(),
                &next,
                &page::Said::NotInFile(user),
            )
        }
    }

    /// A command of the tool's, holding the secret.
    async fn control(&self, request: Request<Incoming>, what: &str) -> Response<Full<Bytes>> {
        let Some(body) = read_body(request.into_body(), control::MAX_SENT_BYTES).await else {
            return page::plain(StatusCode::BAD_REQUEST, "Not read whole.\n".to_owned());
        };
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return page::plain(StatusCode::BAD_REQUEST, "Not JSON.\n".to_owned());
        };
        let asked = match what {
            "request" => wanted_of(&value).map(Asked::Request),
            "read" => reading_of(&value).map(Asked::Read),
            "reset" => Some(Asked::Reset),
            _ => return page::not_found(),
        };
        let Some(asked) = asked else {
            return page::plain(StatusCode::BAD_REQUEST, "Not a command.\n".to_owned());
        };
        let (reply, answered) = oneshot::channel();
        let sent = tokio::time::timeout(READ_WAIT, self.jobs.send(Job::Control { asked, reply }));
        if !matches!(sent.await, Ok(Ok(()))) {
            return page::unavailable("vibe dev is stopping.");
        }
        let Ok(Ok((code, done))) = tokio::time::timeout(READ_WAIT, answered).await else {
            return page::unavailable("vibe dev could not do the command in time, or is stopping.");
        };
        page::json(json!({ "code": code, "json": done.json, "text": done.text }).to_string())
    }

    /// The project brought up to date for a request of the browser: a
    /// rebuild asked for, and waited for, when it changed. `Err` is what the
    /// request is answered instead.
    async fn up_to_date(&self) -> Result<(), Box<Response<Full<Bytes>>>> {
        let Some(looked) = self.look().await else {
            return Ok(());
        };
        {
            let state = tokio::time::timeout(READ_WAIT, self.state.read())
                .await
                .map_err(|_| Box::new(page::unavailable("vibe dev is busy: try again shortly.")))?;
            if state.seen == Some(looked.look) {
                return Ok(());
            }
        }
        let (reply, answered) = oneshot::channel();
        let sent = tokio::time::timeout(READ_WAIT, self.jobs.send(Job::Rebuild { reply }));
        if !matches!(sent.await, Ok(Ok(()))) {
            return Err(Box::new(page::unavailable("vibe dev is stopping.")));
        }
        match tokio::time::timeout(READ_WAIT, answered).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(Box::new(page::unavailable(
                "vibe dev could not bring the project up in time, or is stopping.",
            ))),
        }
    }

    /// A request for the Application.
    async fn application_request(
        &self,
        request: Request<Incoming>,
        address: &str,
    ) -> Response<Sent> {
        let users = self.users(false).await;
        let Ok(state) = tokio::time::timeout(READ_WAIT, self.state.read()).await else {
            return page::unavailable("vibe dev is busy: try again shortly.").map(Sent::from);
        };
        if let Some((_, report)) = &state.failed {
            return page::failed(error_of(report)).map(Sent::from);
        }
        let Some(session) = state.session.as_ref() else {
            return page::unavailable("vibe dev is starting the local databases over.")
                .map(Sent::from);
        };
        let file = match users {
            Ok((file, _)) => file,
            Err(ending) => return page::failed(error_of(&ending.printed().1)).map(Sent::from),
        };
        // Copied: the request owns its User, as the file has it now; once
        // a request.
        let user = Self::chosen(request.headers())
            .and_then(|id| file.user(&id).map(|u| (u.id.clone(), u.groups.clone())));
        let Some((id, groups)) = user else {
            let page_wanted = matches!(*request.method(), Method::GET | Method::HEAD)
                && request.headers().get_all(ACCEPT).iter().any(|a| {
                    a.as_bytes()
                        .to_ascii_lowercase()
                        .windows(9)
                        .any(|w| w == b"text/html")
                });
            if page_wanted {
                let target = request.uri().path_and_query().map_or("/", |p| p.as_str());
                return page::redirect(
                    StatusCode::SEE_OTHER,
                    &format!(
                        "{}?next={}",
                        page::CHOOSER,
                        page::percent_encode(target, true)
                    ),
                )
                .map(Sent::from);
            }
            return page::no_user(address).map(Sent::from);
        };
        let (mut parts, body) = request.into_parts();
        without_test_user_cookie(&mut parts.headers);
        // Copied: the line printed after names them; once a request.
        let method = parts.method.clone();
        let path = parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned());
        parts.extensions.insert(ChosenTestUser(User {
            // Copied: the line printed after names the User too.
            id: id.clone(),
            groups,
        }));
        let response = session
            .live
            .ingress
            .handle(Request::from_parts(parts, body))
            .await;
        drop(state);
        if response
            .extensions()
            .get::<crate::ingress::GivenToCode>()
            .is_some()
        {
            let request_id = response
                .headers()
                .get(crate::ingress::REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            self.say_request(&id, &method, &path, response.status(), request_id)
                .await;
        }
        response
    }
}

impl Handler for Front {
    fn handle(&self, request: Request<Incoming>) -> impl Future<Output = Response<Sent>> + Send {
        self.answer(request)
    }
}

/// Whether `host` names something under `.localhost` (with or without a
/// port).
fn is_localhost_name(host: &str) -> bool {
    let name = host.trim().to_ascii_lowercase();
    let name = name
        .rsplit_once(':')
        .map_or(name.as_str(), |(name, _)| name);
    name.ends_with(".localhost")
}

/// Whose a `Host` is.
enum Which {
    Ours,
    Loopback,
    Other,
}

/// `vibe dev` could not take the command now.
fn busy(who: &Who) -> Ending {
    Ending::new(
        1,
        "failed",
        format!(
            "vibe dev, which runs in this project, could not do {} now (it is busy or starting the local databases over): run it again",
            who.command
        ),
    )
}

/// The cookies of one `Cookie` line, each without the spaces around it.
fn cookie_pairs(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|b| *b == b';')
        .map(<[u8]>::trim_ascii)
        .filter(|pair| !pair.is_empty())
}

/// The cookie of the chosen test User taken out of `Cookie` (the others
/// stay, byte for byte; a header left empty is left out).
fn without_test_user_cookie(headers: &mut hyper::HeaderMap) {
    let mut kept: Vec<u8> = Vec::new();
    for line in headers.get_all(COOKIE) {
        for pair in cookie_pairs(line.as_bytes()) {
            let name = pair
                .iter()
                .position(|b| *b == b'=')
                .and_then(|at| pair.get(..at))
                .unwrap_or(pair);
            if name == TEST_USER_COOKIE.as_bytes() {
                continue;
            }
            if !kept.is_empty() {
                kept.extend_from_slice(b"; ");
            }
            kept.extend_from_slice(pair);
        }
    }
    headers.remove(COOKIE);
    // Made of bytes that were in header values already, and "; ".
    if !kept.is_empty()
        && let Ok(value) = HeaderValue::from_bytes(&kept)
    {
        headers.insert(COOKIE, value);
    }
}

/// A body of at most `limit` bytes, within [`BODY_WAIT`].
async fn read_body(body: Incoming, limit: usize) -> Option<Bytes> {
    let read = Limited::new(body, limit).collect();
    match tokio::time::timeout(BODY_WAIT, read).await {
        Ok(Ok(collected)) => Some(collected.to_bytes()),
        _ => None,
    }
}

/// Hex digits back to their bytes.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(digits, 16).ok()
        })
        .collect()
}

/// The request a `vibe request` sent.
fn wanted_of(value: &Value) -> Option<Wanted> {
    let mut headers = Vec::new();
    for pair in value["headers"].as_array()? {
        let name = HeaderName::from_bytes(pair.get(0)?.as_str()?.as_bytes()).ok()?;
        let value = HeaderValue::from_bytes(&unhex(pair.get(1)?.as_str()?)?).ok()?;
        headers.push((name, value));
    }
    Some(Wanted {
        path: value["path"].as_str()?.to_owned(),
        user: value["as"].as_str()?.to_owned(),
        method: Method::from_bytes(value["method"].as_str()?.as_bytes()).ok()?,
        headers,
        body: Bytes::from(unhex(value["body"].as_str()?)?),
    })
}

/// What a `vibe local` sent.
fn reading_of(value: &Value) -> Option<Reading> {
    let limit = u16::try_from(value["limit"].as_u64()?).ok()?;
    match value["read"].as_str()? {
        "operation-log" => Some(Reading::OperationLog { limit }),
        "change-history" => {
            let table = match &value["table"] {
                Value::Null => None,
                pair => Some((
                    pair.get(0)?.as_str()?.to_owned(),
                    pair.get(1)?.as_str()?.to_owned(),
                )),
            };
            Some(Reading::ChangeHistory { limit, table })
        }
        _ => None,
    }
}

/// A rebuild in words, on one line.
fn rebuilt_words(brought: &Brought, url: &str) -> String {
    let mut words = format!(
        "Rebuilt: {} at {url}, local Version {}",
        Theirs::from(brought.application.as_str()),
        brought.version
    );
    if !brought.migrations_applied.is_empty() {
        let names: Vec<String> = brought
            .migrations_applied
            .iter()
            .map(|m| Theirs::from(m.as_str()).to_string())
            .collect();
        words.push_str(&format!("; migrations applied: {}", names.join(", ")));
    }
    let made: Vec<String> = brought
        .standins
        .iter()
        .filter(|(_, made)| *made)
        .map(|(resource, _)| Theirs::from(resource.as_str()).to_string())
        .collect();
    if !made.is_empty() {
        words.push_str(&format!("; stand-ins made: {}", made.join(", ")));
    }
    let loaded: Vec<String> = brought
        .test_rows
        .iter()
        .map(|(file, rows)| format!("{} ({rows} rows)", Theirs::from(file.as_str())))
        .collect();
    if !loaded.is_empty() {
        words.push_str(&format!("; test rows loaded: {}", loaded.join(", ")));
    }
    words.push('.');
    words
}
