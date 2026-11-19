//! The harness's real browser (CONTRACT.md, "A real browser in the gate"):
//! Firefox without a window, on a fresh profile of the test's own, driven
//! over Marionette, the protocol Firefox itself speaks, and ended with
//! every process it started, its profile's folder removed, whatever
//! happens to the test.
//!
//! - **Finding it** ([`required`], [`find_with`]): the program
//!   `VIBE_TEST_BROWSER` names (an absolute path, or a name looked up on
//!   `PATH`; a relative path is refused, [`NOT_ABSOLUTE`]), else
//!   `firefox` on `PATH`. It is asked `--version` (within
//!   [`VERSION_TIMEOUT`]) and must answer `Mozilla Firefox <major>.<...>`
//!   with a major of [`MIN_VERSION`] or later. Anything else is the failure
//!   [`NO_BROWSER`] begins: what was looked for and what to do. Never a
//!   skip: a browser test without a browser fails.
//! - **Where its profile goes**: a Firefox installed as a snap (Ubuntu's
//!   `firefox` leads into `/snap/`) cannot read a folder outside its own,
//!   and a profile under `/tmp` silently does not work there; so for a snap
//!   the profile is a fresh folder under `~/snap/firefox/common/`, else
//!   under the system's temporary folder. Its name begins with
//!   [`PROFILE_PREFIX`], so a folder of the harness is never mistaken for
//!   anyone else's.
//! - **Starting it**: `<firefox> --headless --marionette --no-remote
//!   --profile <folder>`, in a process group of its own, with the
//!   preferences of [`preferences`] written to `<folder>/user.js` first
//!   (each with its reason there), Marionette on a port the harness chose
//!   ([`crate::ports::chosen`]). The session it opens must be on that very
//!   profile (`moz:profile`), so a test never drives another browser that
//!   happens to listen there.
//! - **Ending it**: every browser has a guardian, a small `sh` whose
//!   standard input is a pipe only the test process writes (as the
//!   harness's Postgres has: `crate::postgres`). When the browser is
//!   dropped (the test passed, or failed and is unwinding), or when the
//!   test process is gone however it went (a kill included), that input
//!   closes and the guardian ends the browser's whole process group (a
//!   `TERM`, then a `KILL` for what is left after five seconds) and removes
//!   the profile's folder. A drop first asks Firefox to quit, waits for it,
//!   then lets the guardian go, and waits for the guardian, all bounded.
//! - **How many at once**: at most [`MAX_AT_ONCE`] browsers in one test
//!   process. A start that finds them all taken waits for one to end, at
//!   most [`SLOT_WAIT`], and then fails saying so. `cargo test` runs one
//!   test binary at a time, so this is the bound of the whole gate.
//!
//! Every wait has a timeout: the start ([`START_TIMEOUT`]), each command
//! ([`COMMAND_TIMEOUT`]; a page that does not load fails within it), and
//! the end ([`END_TIMEOUT`]). Messages read from Marionette are bounded
//! ([`MAX_MESSAGE`]).
//!
//! The protocol: Firefox greets with `<length>:{"applicationType":"gecko",
//! "marionetteProtocol":3}`; every message is `<decimal length of the JSON
//! in bytes>:<JSON>`; a command is `[0, <id>, "<name>", {<params>}]` and
//! its answer `[1, <id>, <error or null>, <result>]`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

use crate::program::run_to_exit;

/// The environment variable that names the browser.
pub const ENV: &str = "VIBE_TEST_BROWSER";

/// The oldest Firefox taken.
pub const MIN_VERSION: u32 = 128;

/// How every failure to find a browser begins.
pub const NO_BROWSER: &str = "No browser for the browser tests";

/// What to do, in every failure to find a browser.
pub const WHAT_TO_DO: &str = "Install Firefox 128 or later, or set VIBE_TEST_BROWSER to the Firefox to use (an absolute path, or a name on the PATH).";

/// Why a relative path in `VIBE_TEST_BROWSER` (one that holds a `/` and
/// does not begin with one) is refused: the tests and `scripts/gate.sh`
/// run in different folders, so the same relative path would name a
/// different file to each.
pub const NOT_ABSOLUTE: &str =
    "which is a relative path; a path in VIBE_TEST_BROWSER must be absolute";

/// How a profile folder of the harness's is named.
pub const PROFILE_PREFIX: &str = "vibe-blackbox-browser-";

/// The most browsers one test process runs at once.
pub const MAX_AT_ONCE: usize = 4;

/// How long a start waits for one of the [`MAX_AT_ONCE`] to end.
pub const SLOT_WAIT: Duration = Duration::from_secs(300);

/// How long `--version` may take.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a start may take, to the open session.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one command may take (a navigation: the page's load).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the end may take: the quit, then the guardian.
pub const END_TIMEOUT: Duration = Duration::from_secs(20);

/// The largest message read from Marionette.
pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;

/// How many ports a start tries for Marionette.
const PORT_TRIES: usize = 3;

/// The key of an element's reference in Marionette's answers.
const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

/// The Firefox found, and what it said of itself.
#[derive(Clone, Debug)]
pub struct Found {
    /// What is started: a path.
    pub program: PathBuf,
    /// What `--version` printed, trimmed.
    pub version: String,
    pub major: u32,
    /// Whether it is a snap (its profile must be under its own folder).
    pub snap: bool,
}

/// The browser of this machine, found once per test process: fails the
/// test (with what was looked for and what to do) when there is none.
pub fn required() -> &'static Found {
    static FOUND: OnceLock<Result<Found, String>> = OnceLock::new();
    match FOUND.get_or_init(|| find_with(std::env::var_os(ENV), std::env::var_os("PATH").as_ref()))
    {
        Ok(found) => found,
        Err(problem) => panic!("{problem}"),
    }
}

/// Finds the browser as the contract says, with these values of
/// `VIBE_TEST_BROWSER` and `PATH`.
pub fn find_with(named: Option<OsString>, path: Option<&OsString>) -> Result<Found, String> {
    let named = named.filter(|n| !n.is_empty());
    if let Some(name) = &named {
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
        let name = Path::new(name);
        if bytes.contains(&b'/') && !bytes.starts_with(b"/") {
            return Err(format!(
                "{NO_BROWSER}: looked for {ENV}={}, {NOT_ABSOLUTE}. {WHAT_TO_DO}",
                name.display()
            ));
        }
    }
    let (looked_for, program) = match &named {
        Some(name) => {
            let shown = Path::new(name).display().to_string();
            (format!("{ENV}={shown}"), resolve(Path::new(name), path))
        }
        None => (
            format!("`firefox` on the PATH ({ENV} is not set)"),
            resolve(Path::new("firefox"), path),
        ),
    };
    let Some(program) = program else {
        return Err(format!(
            "{NO_BROWSER}: looked for {looked_for}, and found no such program. {WHAT_TO_DO}"
        ));
    };
    let mut command = Command::new(&program);
    command.arg("--version");
    let finished = run_to_exit(command, VERSION_TIMEOUT).map_err(|problem| {
        format!(
            "{NO_BROWSER}: looked for {looked_for}, found {}, which could not be asked its version ({problem}). {WHAT_TO_DO}",
            program.display()
        )
    })?;
    let said = finished.stdout.trim().to_owned();
    if finished.status.is_none() {
        return Err(format!(
            "{NO_BROWSER}: looked for {looked_for}, found {}, which did not answer `--version` within {VERSION_TIMEOUT:?}. {WHAT_TO_DO}",
            program.display()
        ));
    }
    let Some(major) = firefox_major(&said) else {
        return Err(format!(
            "{NO_BROWSER}: looked for {looked_for}, found {}, which is not Firefox: `--version` said {:?}. {WHAT_TO_DO}",
            program.display(),
            first_line(&format!("{said}{}", finished.stderr.trim()))
        ));
    };
    if major < MIN_VERSION {
        return Err(format!(
            "{NO_BROWSER}: looked for {looked_for}, found {}, which is {said:?}, older than Firefox {MIN_VERSION}. {WHAT_TO_DO}",
            program.display()
        ));
    }
    let snap = is_snap(&program);
    Ok(Found {
        program,
        version: said,
        major,
        snap,
    })
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// The major version of `Mozilla Firefox <major>.<...>`, the line
/// `--version` prints; `None` for anything else.
pub fn firefox_major(said: &str) -> Option<u32> {
    let line = said.lines().find(|l| !l.trim().is_empty())?.trim();
    let rest = line.strip_prefix("Mozilla Firefox ")?;
    let major: String = rest.chars().take_while(char::is_ascii_digit).collect();
    major.parse().ok()
}

/// `name` as a program: a path that is an executable file, or a bare name
/// looked up on `path`.
fn resolve(name: &Path, path: Option<&OsString>) -> Option<PathBuf> {
    if name.components().count() > 1 || name.is_absolute() {
        return executable(name).then(|| name.to_path_buf());
    }
    let path = path?;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether `program` is a snap's Firefox: it is (or links to) something
/// under `/snap/`, or `/usr/bin/snap` itself (`/snap/bin/firefox` is a
/// link to it), or a script that runs `/snap/bin/...` (Ubuntu's
/// `/usr/bin/firefox`).
fn is_snap(program: &Path) -> bool {
    let canonical = fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    if canonical.starts_with("/snap/") || canonical == Path::new("/usr/bin/snap") {
        return true;
    }
    // A script: its first bytes only, bounded.
    let mut head = Vec::new();
    if let Ok(file) = fs::File::open(&canonical) {
        drop(file.take(64 * 1024).read_to_end(&mut head));
    }
    head.starts_with(b"#!") && String::from_utf8_lossy(&head).contains("/snap/bin/")
}

/// What a browser is started with.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Host names answered with the loopback address
    /// (`network.dns.localDomains`): every name of the installation the
    /// test opens. Names under `.localhost` need none.
    pub names: Vec<String>,
    /// Certificate authorities the browser trusts for the sites it opens,
    /// each its certificate in DER (never a key): an installation's TLS
    /// front of the test's own. Put into the fresh profile's certificate
    /// store after the start, before the first page; nothing else is
    /// trusted beyond the browser's own, and a certificate it cannot check
    /// is refused (`acceptInsecureCerts` false).
    pub authorities: Vec<Vec<u8>>,
}

impl Options {
    /// A browser that reaches these names on this machine.
    pub fn reaching(names: &[String]) -> Self {
        Self {
            names: names.to_vec(),
            authorities: Vec::new(),
        }
    }

    /// As it is, also trusting this authority (its certificate in DER).
    #[must_use]
    pub fn trusting(mut self, authority_der: &[u8]) -> Self {
        self.authorities.push(authority_der.to_vec());
        self
    }
}

/// The preferences a browser of the harness's is started with, one
/// `user_pref(...)` a line, each with its reason.
pub fn preferences(marionette_port: u16, options: &Options) -> String {
    let prefs: Vec<(&str, Value)> = vec![
        // Marionette on the port the harness chose.
        ("marionette.port", json!(marionette_port)),
        // The installation's names reach the program on this machine.
        ("network.dns.localDomains", json!(options.names.join(","))),
        // Nothing else is asked of the network: every request that is not
        // to the loopback address or to the installation's names goes to a
        // proxy at a port of the loopback address where nothing listens
        // (port 9, the discard port, which no test uses), and is refused at
        // once.
        ("network.proxy.type", json!(1)),
        ("network.proxy.http", json!("127.0.0.1")),
        ("network.proxy.http_port", json!(9)),
        ("network.proxy.ssl", json!("127.0.0.1")),
        ("network.proxy.ssl_port", json!(9)),
        ("network.proxy.share_proxy_settings", json!(false)),
        (
            "network.proxy.no_proxies_on",
            json!(no_proxies_on(&options.names)),
        ),
        // No name is looked up ahead, nor over DNS over HTTPS.
        ("network.dns.disablePrefetch", json!(true)),
        ("network.prefetch-next", json!(false)),
        ("network.predictor.enabled", json!(false)),
        ("network.http.speculative-parallel-limit", json!(0)),
        ("network.trr.mode", json!(5)),
        // No page of the browser's own, no question at the start.
        ("browser.shell.checkDefaultBrowser", json!(false)),
        ("browser.startup.homepage_override.mstone", json!("ignore")),
        ("browser.startup.page", json!(0)),
        ("browser.startup.homepage", json!("about:blank")),
        ("browser.newtabpage.enabled", json!(false)),
        ("browser.aboutwelcome.enabled", json!(false)),
        ("startup.homepage_welcome_url", json!("about:blank")),
        ("startup.homepage_welcome_url.additional", json!("")),
        ("browser.tabs.warnOnClose", json!(false)),
        ("browser.sessionstore.resume_from_crash", json!(false)),
        // Nothing sent to the browser's maker.
        ("datareporting.policy.dataSubmissionEnabled", json!(false)),
        ("datareporting.healthreport.uploadEnabled", json!(false)),
        ("toolkit.telemetry.reportingpolicy.firstRun", json!(false)),
        ("toolkit.telemetry.enabled", json!(false)),
        ("toolkit.telemetry.unified", json!(false)),
        ("toolkit.telemetry.server", json!("")),
        (
            "browser.newtabpage.activity-stream.feeds.telemetry",
            json!(false),
        ),
        ("browser.newtabpage.activity-stream.telemetry", json!(false)),
        ("browser.ping-centre.telemetry", json!(false)),
        ("app.normandy.enabled", json!(false)),
        ("app.normandy.api_url", json!("")),
        ("app.shield.optoutstudies.enabled", json!(false)),
        ("messaging-system.rsexperimentloader.enabled", json!(false)),
        ("browser.discovery.enabled", json!(false)),
        ("browser.region.network.url", json!("")),
        ("browser.region.update.enabled", json!(false)),
        ("geo.provider.network.url", json!("")),
        ("services.settings.server", json!("http://127.0.0.1:9/v1")),
        (
            "browser.crashReports.unsubmittedCheck.autoSubmit2",
            json!(false),
        ),
        // No update, no add-on fetched or updated.
        ("app.update.auto", json!(false)),
        ("app.update.enabled", json!(false)),
        ("app.update.disabledForTesting", json!(true)),
        ("extensions.update.enabled", json!(false)),
        ("extensions.getAddons.cache.enabled", json!(false)),
        ("extensions.systemAddon.update.enabled", json!(false)),
        ("extensions.blocklist.enabled", json!(false)),
        // No check of addresses against the maker's lists, no probe of the
        // network's portal or connectivity.
        ("browser.safebrowsing.malware.enabled", json!(false)),
        ("browser.safebrowsing.phishing.enabled", json!(false)),
        ("browser.safebrowsing.downloads.enabled", json!(false)),
        ("browser.safebrowsing.blockedURIs.enabled", json!(false)),
        ("network.captive-portal-service.enabled", json!(false)),
        ("network.connectivity-service.enabled", json!(false)),
        // A JSON answer is shown as its text, as a page, not in the
        // browser's own viewer (which replaces the document a test reads).
        ("devtools.jsonview.enabled", json!(false)),
        // An `http` address is loaded as it is, never tried over HTTPS
        // first or upgraded: the local run (`vibe dev`) serves its pages in
        // plain HTTP on a name under `.localhost`, and the harness's
        // identity provider answers in plain HTTP on `127.0.0.1`. Both are
        // Firefox's defaults on a desktop build today; written down so
        // that a build whose defaults differ walks the same way. The
        // installation's own pages are `https` (its TLS front).
        ("dom.security.https_only_mode", json!(false)),
        ("dom.security.https_first", json!(false)),
        ("browser.fixup.alternate.enabled", json!(false)),
        // The profile's `chrome/userChrome.css` is read ([`USER_CHROME`]:
        // a window as narrow as a phone's).
        (
            "toolkit.legacyUserProfileCustomizations.stylesheets",
            json!(true),
        ),
    ];
    let mut text = String::new();
    for (name, value) in prefs {
        text.push_str(&format!("user_pref({}, {value});\n", json!(name)));
    }
    text
}

/// The browser's own window is at least 500 CSS pixels wide (its root
/// element's `min-width`), so a window of a phone's 320 could not be had;
/// this sheet of the profile's takes that minimum away (read because of
/// `toolkit.legacyUserProfileCustomizations.stylesheets`). Pages are not
/// touched by it.
const USER_CHROME: &str = ":root { min-width: 0 !important; }\n";

/// What goes to no proxy: the loopback addresses, `localhost` and every
/// name under it, and the installation's names.
fn no_proxies_on(names: &[String]) -> String {
    let mut all = vec![
        "127.0.0.1".to_owned(),
        "localhost".to_owned(),
        ".localhost".to_owned(),
        "[::1]".to_owned(),
    ];
    all.extend(names.iter().cloned());
    all.join(",")
}

/// The slots of [`MAX_AT_ONCE`].
struct Slots {
    taken: Mutex<usize>,
    freed: Condvar,
}

fn slots() -> &'static Slots {
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    SLOTS.get_or_init(|| Slots {
        taken: Mutex::new(0),
        freed: Condvar::new(),
    })
}

/// One of the [`MAX_AT_ONCE`]; given back when dropped.
struct Slot;

impl Slot {
    fn take() -> Result<Self, String> {
        let slots = slots();
        let deadline = Instant::now() + SLOT_WAIT;
        let mut taken = slots.taken.lock().unwrap_or_else(PoisonError::into_inner);
        while *taken >= MAX_AT_ONCE {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "the harness runs at most {MAX_AT_ONCE} browsers at once, and none of them ended within {SLOT_WAIT:?}"
                ));
            }
            taken = slots
                .freed
                .wait_timeout(taken, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *taken += 1;
        Ok(Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let slots = slots();
        let mut taken = slots.taken.lock().unwrap_or_else(PoisonError::into_inner);
        *taken = taken.saturating_sub(1);
        slots.freed.notify_one();
    }
}

/// The guardian's script: ignore the signals a Ctrl-C or a group kill sends;
/// read the browser's process group from the first line; wait for standard
/// input to close; then end the group (`TERM`, and `KILL` to what is left
/// after five seconds; at most ten seconds in all) and remove the profile's
/// folder. The folder is passed as an argument, never pasted into the
/// script. The group is signalled with the system's `kill` program (`env
/// kill`), not the shell's own: `dash`'s takes neither `--` nor a negative
/// number after `-s`.
const GUARDIAN_SCRIPT: &str = r#"trap '' INT TERM HUP
IFS= read -r group || group=
while read -r _ignored; do :; done
case "$group" in
  ''|*[!0-9]*) ;;
  *)
    env kill -s TERM -- "-$group" 2>/dev/null
    n=0
    while env kill -s 0 -- "-$group" 2>/dev/null; do
      n=$((n + 1))
      if [ "$n" -gt 50 ]; then env kill -s KILL -- "-$group" 2>/dev/null; fi
      if [ "$n" -gt 100 ]; then break; fi
      sleep 0.1
    done
    ;;
esac
rm -rf -- "$1""#;

/// The process that ends the browser and removes its profile once it is
/// told to or the test process is gone.
struct Guardian {
    stdin: Option<ChildStdin>,
    child: Child,
}

impl Guardian {
    fn spawn(profile: &Path) -> Result<Self, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(GUARDIAN_SCRIPT)
            .arg("vm-blackbox-browser-guardian")
            .arg(profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start the browser's guardian (sh): {e}"))?;
        let stdin = child.stdin.take();
        Ok(Self { stdin, child })
    }

    /// Tells the guardian which process group is the browser's.
    fn watch(&mut self, group: u32) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "the browser's guardian has no input".to_owned())?;
        stdin
            .write_all(format!("{group}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("cannot tell the browser's guardian its group: {e}"))
    }

    /// Lets the guardian end the browser now; waits for it a bounded time.
    /// Whether it ended in that time.
    fn release(&mut self, within: Duration) -> bool {
        drop(self.stdin.take());
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        drop(self.child.kill());
        drop(self.child.wait());
        false
    }
}

/// A process of the machine as `/proc/<pid>/stat` says: its id, parent,
/// process group, and start time (so that a pid used again is not taken
/// for it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub started: u64,
}

impl Process {
    /// The process `pid` as it is now, if it runs (a zombie does not).
    pub fn of(pid: u32) -> Option<Self> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // After the name, which is in parentheses and may hold anything.
        let rest = stat.get(stat.rfind(')')? + 1..)?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // Fields from the state on: state, ppid, pgrp, ..., starttime is
        // the 20th after the state.
        let state = *fields.first()?;
        if state == "Z" || state == "X" {
            return None;
        }
        Some(Self {
            pid,
            parent: fields.get(1)?.parse().ok()?,
            group: fields.get(2)?.parse().ok()?,
            started: fields.get(19)?.parse().ok()?,
        })
    }

    /// Whether this very process (the same pid and start) still runs.
    pub fn runs(&self) -> bool {
        Self::of(self.pid).is_some_and(|now| now.started == self.started)
    }
}

/// Every process of the machine that runs now.
pub fn every_process() -> Vec<Process> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(Process::of)
        .collect()
}

/// The processes that run now in the process group `group` or below the
/// process `root`, by parent.
pub fn tree_of(root: u32, group: u32) -> Vec<Process> {
    let all = every_process();
    let mut found: BTreeSet<u32> = all
        .iter()
        .filter(|p| p.group == group || p.pid == root)
        .map(|p| p.pid)
        .collect();
    // Descendants by parent, to a fixed point (bounded by the processes
    // there are).
    for _ in 0..all.len() {
        let before = found.len();
        for p in &all {
            if found.contains(&p.parent) {
                found.insert(p.pid);
            }
        }
        if found.len() == before {
            break;
        }
    }
    all.into_iter().filter(|p| found.contains(&p.pid)).collect()
}

/// A reference to an element of the page, as Marionette gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element(pub String);

/// An error Marionette answered a command with.
#[derive(Clone, Debug)]
pub struct CommandError {
    pub command: String,
    pub error: String,
    pub message: String,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.command, self.error, self.message)
    }
}

/// A Firefox of the test's own (see the module's documentation). Dropping
/// it ends it.
pub struct Firefox {
    stream: Option<TcpStream>,
    next_id: u64,
    child: Child,
    guardian: Option<Guardian>,
    profile: PathBuf,
    log: PathBuf,
    port: u16,
    found: &'static Found,
    /// From the start of the program to the open session (and the
    /// authorities trusted).
    start_took: Duration,
    /// Whether it was ended already.
    ended: bool,
    slot: Option<Slot>,
}

impl fmt::Debug for Firefox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Firefox")
            .field("pid", &self.child.id())
            .field("profile", &self.profile)
            .field("marionette", &self.port)
            .finish_non_exhaustive()
    }
}

/// A unique folder name for a profile.
fn profile_name() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut bytes = [0_u8; 6];
    // Without randomness the process id and the count keep it unique in
    // this process; the random part only keeps it from another's.
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).is_err() {
        bytes = [0_u8; 6];
    }
    format!(
        "{PROFILE_PREFIX}{}-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed),
        crate::oidc::hex(&bytes)
    )
}

/// Where profiles of `found` go: under the snap's own folder for a snap.
pub fn profiles_dir(found: &Found) -> PathBuf {
    if found.snap
        && let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
    {
        return PathBuf::from(home).join("snap/firefox/common");
    }
    std::env::temp_dir()
}

impl Firefox {
    /// Starts a browser that reaches these names; fails the test (saying
    /// why, with the browser's own output) unless it runs.
    pub fn start(names: &[String]) -> Self {
        Self::start_with(&Options::reaching(names))
    }

    /// As [`Firefox::start`], with these options.
    pub fn start_with(options: &Options) -> Self {
        Self::try_start(options).unwrap_or_else(|problem| panic!("{problem}"))
    }

    /// A browser started with these options, or why not.
    pub fn try_start(options: &Options) -> Result<Self, String> {
        let found = required();
        let slot = Slot::take()?;
        let mut slot = Some(slot);
        let mut last = String::new();
        for _ in 0..PORT_TRIES {
            let taken = slot.take().ok_or("no slot")?;
            match Self::launch(found, options, taken) {
                Ok(firefox) => return Ok(firefox),
                Err((problem, back)) => {
                    last = problem;
                    match back {
                        Some(back) => slot = Some(back),
                        None => break,
                    }
                }
            }
        }
        Err(last)
    }

    /// One start on a port of its own. On failure, the slot back when the
    /// start may be tried again on another port.
    fn launch(
        found: &'static Found,
        options: &Options,
        slot: Slot,
    ) -> Result<Self, (String, Option<Slot>)> {
        let port = crate::ports::chosen().map_err(|e| (e, None))?;
        let dir = profiles_dir(found);
        fs::create_dir_all(&dir)
            .map_err(|e| (format!("cannot make {}: {e}", dir.display()), None))?;
        let profile = dir.join(profile_name());
        fs::create_dir(&profile)
            .map_err(|e| (format!("cannot make {}: {e}", profile.display()), None))?;
        let mut guardian = match Guardian::spawn(&profile) {
            Ok(g) => g,
            Err(e) => {
                drop(fs::remove_dir_all(&profile));
                return Err((e, None));
            }
        };
        let fail = |problem: String, mut guardian: Guardian| {
            guardian.release(END_TIMEOUT);
            (problem, None)
        };
        if let Err(e) = fs::write(profile.join("user.js"), preferences(port, options)) {
            return Err(fail(format!("cannot write the preferences: {e}"), guardian));
        }
        let chrome = profile.join("chrome");
        if let Err(e) = fs::create_dir(&chrome)
            .and_then(|()| fs::write(chrome.join("userChrome.css"), USER_CHROME))
        {
            return Err(fail(format!("cannot write userChrome.css: {e}"), guardian));
        }
        let log = profile.join("harness-browser.log");
        let opened = fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f)));
        let (out, err) = match opened {
            Ok(files) => files,
            Err(e) => {
                return Err(fail(
                    format!("cannot make the browser's log: {e}"),
                    guardian,
                ));
            }
        };
        let began = Instant::now();
        let mut command = Command::new(&found.program);
        command
            .arg("--headless")
            .arg("--marionette")
            .arg("--no-remote")
            .arg("--profile")
            .arg(&profile)
            .env("MOZ_HEADLESS", "1")
            .env("MOZ_CRASHREPORTER_DISABLE", "1")
            .env_remove("MOZ_MARIONETTE")
            .env_remove("MOZ_REMOTE_ALLOW_SYSTEM_ACCESS");
        if !options.authorities.is_empty() {
            // Marionette's chrome scope, only to put the authorities into
            // the profile's certificate store (see `trust`): a browser
            // given none runs without it.
            command.env("MOZ_REMOTE_ALLOW_SYSTEM_ACCESS", "1");
        }
        let spawned = command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn();
        let child = match spawned {
            Ok(child) => child,
            Err(e) => {
                return Err(fail(
                    format!("cannot start {}: {e}", found.program.display()),
                    guardian,
                ));
            }
        };
        if let Err(e) = guardian.watch(child.id()) {
            // The guardian cannot be told: end the group here.
            let mut firefox = Self {
                stream: None,
                next_id: 0,
                child,
                guardian: Some(guardian),
                profile,
                log,
                port,
                found,
                start_took: Duration::ZERO,
                ended: false,
                slot: Some(slot),
            };
            firefox.end_group_here();
            return Err((e, None));
        }
        let mut firefox = Self {
            stream: None,
            next_id: 0,
            child,
            guardian: Some(guardian),
            profile,
            log,
            port,
            found,
            start_took: Duration::ZERO,
            ended: false,
            slot: Some(slot),
        };
        let connected = firefox
            .connect()
            .and_then(|()| firefox.trust(&options.authorities).map_err(|e| (e, false)));
        match connected {
            Ok(()) => {
                firefox.start_took = began.elapsed();
                eprintln!(
                    "the harness's browser started (to its open session) in {} ms",
                    firefox.start_took.as_millis()
                );
                Ok(firefox)
            }
            Err((problem, retry)) => {
                let shown = format!(
                    "{problem}\nthe browser: {} ({}), profile {}\nits output (the end):\n{}",
                    found.program.display(),
                    found.version,
                    firefox.profile.display(),
                    firefox.log_tail()
                );
                if retry {
                    // Ends this one; its slot goes to the next try.
                    let slot = firefox.end_keeping_slot();
                    Err((shown, slot))
                } else {
                    drop(firefox);
                    Err((shown, None))
                }
            }
        }
    }

    /// Connects to Marionette and opens the session, checking that it is
    /// this browser's. `Err` with whether another port may be tried.
    fn connect(&mut self) -> Result<(), (String, bool)> {
        let deadline = Instant::now() + START_TIMEOUT;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let stream = loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err((
                    format!("the browser ended ({status}) before Marionette listened"),
                    false,
                ));
            }
            if Instant::now() >= deadline {
                return Err((
                    format!(
                        "Marionette did not listen on port {} within {START_TIMEOUT:?}",
                        self.port
                    ),
                    true,
                ));
            }
            if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) {
                break stream;
            }
            thread::sleep(Duration::from_millis(100));
        };
        let set = stream
            .set_read_timeout(Some(COMMAND_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(COMMAND_TIMEOUT)))
            .and_then(|()| stream.set_nodelay(true));
        if let Err(e) = set {
            return Err((format!("cannot set the connection's timeouts: {e}"), false));
        }
        self.stream = Some(stream);
        let hello = self.read_message().map_err(|e| (e, true))?;
        if hello["applicationType"] != "gecko" || hello["marionetteProtocol"] != 3 {
            return Err((
                format!("port {} greeted with {hello}, not Marionette 3", self.port),
                true,
            ));
        }
        let session = self
            .command(
                "WebDriver:NewSession",
                &json!({ "capabilities": { "alwaysMatch": {
                    "acceptInsecureCerts": false,
                    "pageLoadStrategy": "normal",
                    "timeouts": { "implicit": 0, "pageLoad": 30_000, "script": 20_000 }
                } } }),
            )
            .map_err(|e| (e.to_string(), true))?;
        let capabilities = &session["capabilities"];
        let profile = capabilities["moz:profile"].as_str().unwrap_or_default();
        let ours = fs::canonicalize(&self.profile).unwrap_or_else(|_| self.profile.clone());
        let theirs = fs::canonicalize(profile).unwrap_or_else(|_| PathBuf::from(profile));
        if theirs != ours {
            return Err((
                format!(
                    "the browser on port {} runs on the profile {profile:?}, not this test's {}: another browser listens there",
                    self.port,
                    self.profile.display()
                ),
                true,
            ));
        }
        Ok(())
    }

    /// Puts each authority into the profile's certificate store, trusted
    /// to issue sites' certificates (`C,,`), through Marionette's chrome
    /// scope; the certificate is passed as the script's argument, never
    /// pasted into it.
    fn trust(&mut self, authorities: &[Vec<u8>]) -> Result<(), String> {
        if authorities.is_empty() {
            return Ok(());
        }
        self.command("Marionette:SetContext", &json!({ "value": "chrome" }))
            .map_err(|e| format!("cannot reach the browser's chrome scope: {e}"))?;
        for der in authorities {
            let encoded = base64::engine::general_purpose::STANDARD.encode(der);
            self.command(
                "WebDriver:ExecuteScript",
                &json!({
                    "script": "const db = Cc['@mozilla.org/security/x509certdb;1'].getService(Ci.nsIX509CertDB); db.addCertFromBase64(arguments[0], 'C,,'); return true;",
                    "args": [encoded]
                }),
            )
            .map_err(|e| format!("cannot trust the test's authority in the browser: {e}"))?;
        }
        self.command("Marionette:SetContext", &json!({ "value": "content" }))
            .map_err(|e| format!("cannot go back to the page's scope: {e}"))?;
        Ok(())
    }

    /// How long its start took, from the program's start to the open
    /// session.
    pub fn start_took(&self) -> Duration {
        self.start_took
    }

    /// The browser's main process.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The profile's folder.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// What was found and is run.
    pub fn found(&self) -> &'static Found {
        self.found
    }

    /// Every process of this browser that runs now: its process group and
    /// whatever descends from its main process.
    pub fn processes(&self) -> Vec<Process> {
        tree_of(self.pid(), self.pid())
    }

    /// The end of the browser's own output, at most 8 KiB.
    pub fn log_tail(&self) -> String {
        let bytes = fs::read(&self.log).unwrap_or_default();
        let from = bytes.len().saturating_sub(8 * 1024);
        String::from_utf8_lossy(bytes.get(from..).unwrap_or_default()).into_owned()
    }

    fn read_message(&mut self) -> Result<Value, String> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| "not connected to Marionette".to_owned())?;
        let mut length = String::new();
        loop {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .map_err(|e| format!("reading from Marionette: {e}"))?;
            if &byte == b":" {
                break;
            }
            if !byte[0].is_ascii_digit() || length.len() >= 12 {
                return Err(format!(
                    "Marionette sent what is not a message's length: {length:?} then {:?}",
                    char::from(byte[0])
                ));
            }
            length.push(char::from(byte[0]));
        }
        let length: usize = length
            .parse()
            .map_err(|e| format!("Marionette's length {length:?}: {e}"))?;
        if length > MAX_MESSAGE {
            return Err(format!(
                "Marionette sent a message of {length} bytes, more than {MAX_MESSAGE}"
            ));
        }
        let mut body = vec![0_u8; length];
        stream
            .read_exact(&mut body)
            .map_err(|e| format!("reading from Marionette: {e}"))?;
        serde_json::from_slice(&body).map_err(|e| format!("Marionette sent what is not JSON: {e}"))
    }

    /// Sends a command and returns its result, or the error Marionette
    /// answered (a connection that fails is an error too).
    pub fn command(&mut self, name: &str, params: &Value) -> Result<Value, CommandError> {
        let broken = |message: String| CommandError {
            command: name.to_owned(),
            error: "harness".to_owned(),
            message,
        };
        self.next_id += 1;
        let id = self.next_id;
        let body = json!([0, id, name, params]).to_string();
        let wire = format!("{}:{body}", body.len());
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| broken("not connected".to_owned()))?;
        stream
            .write_all(wire.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| broken(format!("writing to Marionette: {e}")))?;
        // Answers to earlier commands (none should come) are passed over, a
        // bounded number of them.
        for _ in 0..16 {
            let message = self.read_message().map_err(broken)?;
            if message[0] != 1 || message[1] != id {
                continue;
            }
            if !message[2].is_null() {
                return Err(CommandError {
                    command: name.to_owned(),
                    error: message[2]["error"].as_str().unwrap_or("?").to_owned(),
                    message: message[2]["message"].as_str().unwrap_or("").to_owned(),
                });
            }
            return Ok(message[3].clone());
        }
        Err(broken(format!("no answer with the id {id}")))
    }

    /// As [`Firefox::command`]; a failure fails the test, showing the page.
    pub fn must(&mut self, name: &str, params: &Value) -> Value {
        match self.command(name, params) {
            Ok(value) => value,
            Err(e) => {
                let page = self.describe();
                panic!("the browser refused a command: {e}\n{page}")
            }
        }
    }

    /// Opens `url` as a person types it, and waits for the page to load.
    pub fn navigate(&mut self, url: &str) {
        self.must("WebDriver:Navigate", &json!({ "url": url }));
    }

    /// The address of the page shown now.
    pub fn url(&mut self) -> String {
        self.command("WebDriver:GetCurrentURL", &json!({}))
            .ok()
            .and_then(|v| v["value"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The page as the browser holds it now, serialized.
    pub fn source(&mut self) -> String {
        self.must("WebDriver:GetPageSource", &json!({}))["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Runs `script` in the page (its `return` value), with `args`.
    pub fn script(&mut self, script: &str, args: &[Value]) -> Value {
        self.must(
            "WebDriver:ExecuteScript",
            &json!({ "script": script, "args": args }),
        )["value"]
            .clone()
    }

    /// The text of the document's body, as a person reads it.
    pub fn text(&mut self) -> String {
        self.command(
            "WebDriver:ExecuteScript",
            &json!({ "script": "return document.body ? document.body.innerText : '';", "args": [] }),
        )
        .ok()
        .and_then(|v| v["value"].as_str().map(str::to_owned))
        .unwrap_or_default()
    }

    /// The status of the answer the page shown now came with, as the
    /// browser's own record of the navigation has it.
    pub fn status(&mut self) -> Option<u64> {
        self.script(
            "const n = performance.getEntriesByType('navigation')[0]; return n ? n.responseStatus : null;",
            &[],
        )
        .as_u64()
    }

    /// The elements `css` finds on the page.
    pub fn find_all(&mut self, css: &str) -> Vec<Element> {
        let found = self.must(
            "WebDriver:FindElements",
            &json!({ "using": "css selector", "value": css }),
        );
        found
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|e| e[ELEMENT_KEY].as_str().map(|id| Element(id.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The first element `css` finds, if any.
    pub fn find(&mut self, css: &str) -> Option<Element> {
        self.find_all(css).into_iter().next()
    }

    /// The one element `css` finds; fails the test, showing the page,
    /// unless there is exactly one.
    pub fn the(&mut self, css: &str) -> Element {
        let mut all = self.find_all(css);
        if all.len() != 1 {
            let page = self.describe();
            panic!(
                "expected exactly one element `{css}` on the page, found {}\n{page}",
                all.len()
            );
        }
        all.remove(0)
    }

    /// The element `css` finds within `inside`.
    pub fn find_within(&mut self, inside: &Element, css: &str) -> Vec<Element> {
        let found = self.must(
            "WebDriver:FindElements",
            &json!({ "using": "css selector", "value": css, "element": inside.0 }),
        );
        found
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|e| e[ELEMENT_KEY].as_str().map(|id| Element(id.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An element's text as rendered.
    pub fn text_of(&mut self, element: &Element) -> String {
        self.must("WebDriver:GetElementText", &json!({ "id": element.0 }))["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// An element's attribute, if it has it.
    pub fn attribute(&mut self, element: &Element, name: &str) -> Option<String> {
        self.must(
            "WebDriver:GetElementAttribute",
            &json!({ "id": element.0, "name": name }),
        )["value"]
            .as_str()
            .map(str::to_owned)
    }

    /// Clicks it as a person does (scrolled into view, the pointer on it).
    pub fn click(&mut self, element: &Element) {
        self.must("WebDriver:ElementClick", &json!({ "id": element.0 }));
    }

    /// Types `text` into it as a person does, key by key.
    pub fn type_into(&mut self, element: &Element, text: &str) {
        self.must(
            "WebDriver:ElementSendKeys",
            &json!({ "id": element.0, "text": text }),
        );
    }

    /// Sets the window's size, in the browser's own pixels.
    pub fn set_window(&mut self, width: u32, height: u32) {
        self.must(
            "WebDriver:SetWindowRect",
            &json!({ "width": width, "height": height }),
        );
    }

    /// Makes the window this many CSS pixels wide (and 800 high). Whether
    /// the page sees it so is the test's to check ([`no_wider_than`]).
    pub fn narrow(&mut self, width: u32) {
        self.set_window(width, 800);
    }

    /// Waits until `ready` says so, at most `within`; fails the test,
    /// showing the page, when it does not.
    pub fn wait_for<T>(
        &mut self,
        what: &str,
        within: Duration,
        mut ready: impl FnMut(&mut Self) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + within;
        loop {
            if let Some(value) = ready(self) {
                return value;
            }
            if Instant::now() >= deadline {
                let page = self.describe();
                panic!("waited {within:?} for {what}, in vain\n{page}");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits until the page shown is one whose address `matches` and that
    /// has loaded.
    pub fn wait_for_page(&mut self, what: &str, matches: impl Fn(&str) -> bool) -> String {
        self.wait_for(what, COMMAND_TIMEOUT, |f| {
            let url = f.url();
            let loaded = f
                .command(
                    "WebDriver:ExecuteScript",
                    &json!({ "script": "return document.readyState;", "args": [] }),
                )
                .ok()
                .is_some_and(|v| v["value"] == "complete");
            (loaded && matches(&url)).then_some(url)
        })
    }

    /// The page shown now, for a failure message: its address (secrets in
    /// it masked), status and text (the first 3,000 characters).
    pub fn describe(&mut self) -> String {
        let url = self.url();
        let status = self
            .command(
                "WebDriver:ExecuteScript",
                &json!({ "script": "const n = performance.getEntriesByType('navigation')[0]; return n ? n.responseStatus : null;", "args": [] }),
            )
            .ok()
            .map(|v| v["value"].to_string())
            .unwrap_or_default();
        let text: String = self.text().chars().take(3000).collect();
        crate::http::mask_values(&format!(
            "the browser's page: {url} (status {status})\n--- its text ---\n{text}\n---"
        ))
    }

    /// Ends the browser now, as a drop does.
    pub fn end(self) {
        drop(self);
    }

    /// Ends it, giving its slot back to the caller.
    fn end_keeping_slot(mut self) -> Option<Slot> {
        self.shut_down();
        self.ended = true;
        self.slot.take()
    }

    /// Asks Firefox to quit, waits for it, lets the guardian end what is
    /// left and remove the profile, and reaps the main process. Bounded.
    fn shut_down(&mut self) {
        if self.stream.is_some() {
            if let Some(stream) = self.stream.as_ref() {
                drop(stream.set_read_timeout(Some(Duration::from_secs(5))));
            }
            drop(self.command("Marionette:Quit", &json!({ "flags": ["eForceQuit"] })));
            self.stream = None;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let ended = self
            .guardian
            .take()
            .is_some_and(|mut g| g.release(END_TIMEOUT));
        if !ended {
            self.end_group_here();
        }
        drop(self.child.kill());
        drop(self.child.wait());
    }

    /// Without the guardian: `KILL` to the browser's process group, by its
    /// number, and the profile removed.
    fn end_group_here(&mut self) {
        let mut command = Command::new("kill");
        command
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{}", self.child.id()));
        drop(run_to_exit(command, Duration::from_secs(5)));
        drop(self.child.kill());
        drop(self.child.wait());
        if let Some(mut guardian) = self.guardian.take() {
            guardian.release(Duration::from_secs(1));
        }
        drop(fs::remove_dir_all(&self.profile));
    }
}

impl Drop for Firefox {
    fn drop(&mut self) {
        if !self.ended {
            self.shut_down();
        }
    }
}

/// Whether the document shown is no wider than its window: `Err` saying
/// the two widths when it is wider, or when the window is not `width` CSS
/// pixels wide as the page sees it.
pub fn no_wider_than(firefox: &mut Firefox, width: u64) -> Result<(), String> {
    let seen = firefox.script(
        "return [window.innerWidth, document.documentElement.scrollWidth, document.documentElement.clientWidth];",
        &[],
    );
    let inner = seen[0].as_u64();
    let scroll = seen[1].as_u64();
    if inner != Some(width) {
        return Err(format!(
            "expected the window {width} CSS pixels wide as the page sees it (window.innerWidth), got {}",
            seen[0]
        ));
    }
    match scroll {
        Some(scroll) if scroll <= width => Ok(()),
        _ => Err(format!(
            "the document is wider than its window: document.documentElement.scrollWidth {} > window.innerWidth {width}",
            seen[1]
        )),
    }
}

/// Every profile folder of the harness's left in the places profiles go.
pub fn profiles_left(found: &Found) -> Vec<PathBuf> {
    let mut left = Vec::new();
    for dir in [profiles_dir(found), std::env::temp_dir()] {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.filter_map(Result::ok) {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(PROFILE_PREFIX))
                {
                    left.push(entry.path());
                }
            }
        }
    }
    left.sort();
    left.dedup();
    left
}

/// Every process that runs now whose command line names `path` (a
/// browser's main process names its profile; so does its guardian).
pub fn processes_naming(path: &Path) -> Vec<Process> {
    let needle = path.as_os_str().as_encoded_bytes();
    if needle.is_empty() {
        return Vec::new();
    }
    every_process()
        .into_iter()
        .filter(|p| {
            fs::read(format!("/proc/{}/cmdline", p.pid))
                .is_ok_and(|line| line.windows(needle.len()).any(|w| w == needle))
        })
        .collect()
}
