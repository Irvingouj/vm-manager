//! What the tests of "Local loop in the browser, with test rows, reset and
//! working offline" share (CONTRACT.md, "The local loop, kept running: the
//! browser, test rows and reset"):
//!
//! - [`Dev`], a `vm dev` of a [`Local`] machine started on a port the
//!   system gave as free, its `--json` lines read as they come, stopped by a
//!   signal, and ended by its guard (a `SIGTERM`, then a kill) when a test
//!   fails before it stops it;
//! - [`Browser`], what a browser does, done by the harness's own HTTP
//!   client: it connects to `127.0.0.1:<port>`, names the Application in
//!   `Host`, keeps the cookies it is given by hand, and never follows a
//!   redirect by itself;
//! - the listening sockets of a port, read from `/proc/net/tcp` and
//!   `/proc/net/tcp6` ([`listeners`]);
//! - percent-encoding and decoding ([`percent_encode`], [`percent_decode`])
//!   and HTML's entities taken back ([`html_unescaped`]).

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::http::{Request, Response};
use crate::local::{LOCAL_TIMEOUT, Local, Postmaster, postmaster_of};
use crate::tool::{Run, Running};

/// How long the harness gives one request to `vm dev`: a request that finds
/// the project changed waits for a build.
pub const DEV_REQUEST_TIMEOUT: Duration = Duration::from_secs(150);

/// How long `vm dev` is given to end after a signal.
pub const STOP_WAIT: Duration = Duration::from_secs(60);

/// A port that nothing listens on at this moment, as the system gives one.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("cannot bind 127.0.0.1:0");
    listener
        .local_addr()
        .expect("the listener has an address")
        .port()
}

/// The JSON objects of the complete lines of `text` (a line not yet ended
/// is left out). Fails the test on a complete line that is not a JSON
/// object.
pub fn json_lines(text: &str) -> Vec<Value> {
    let complete = match text.rfind('\n') {
        Some(end) => text.get(..end).unwrap_or_default(),
        None => "",
    };
    complete
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("expected each line of `vm dev --json` a JSON object ({e}), got {line:?}")
            });
            assert!(
                value.is_object(),
                "expected each line of `vm dev --json` a JSON object, got {line:?}"
            );
            value
        })
        .collect()
}

/// A `vm dev` that runs. See the module's text.
pub struct Dev<'a> {
    pub local: &'a Local,
    pub dir: PathBuf,
    pub port: u16,
    pub application: String,
    pub json: bool,
    running: Option<Running>,
}

/// The arguments of `vm dev --project <dir> --port <port> [--json]`.
pub fn dev_args(dir: &Path, port: u16, json: bool) -> Vec<String> {
    let mut args = vec![
        "dev".to_owned(),
        "--project".to_owned(),
        dir.to_str()
            .unwrap_or_else(|| panic!("not UTF-8: {dir:?}"))
            .to_owned(),
        "--port".to_owned(),
        port.to_string(),
    ];
    if json {
        args.push("--json".to_owned());
    }
    args
}

impl<'a> Dev<'a> {
    /// Starts `vm dev --json` on a free port in the project `dir`, whose
    /// Application is named `application`, and waits for its `listening`
    /// line. Fails the test, saying how it ended, unless it listens.
    pub fn start(local: &'a Local, dir: &Path, application: &str) -> Self {
        Self::start_with(local, dir, application, free_port(), true)
    }

    /// As [`Dev::start`], on this port, with `--json` or in words.
    pub fn start_with(
        local: &'a Local,
        dir: &Path,
        application: &str,
        port: u16,
        json: bool,
    ) -> Self {
        let mut dev = Self::spawn(local, dir, application, port, json);
        dev.wait_until_listening();
        dev
    }

    /// Starts `vm dev` and returns at once, before it listens.
    pub fn spawn(local: &'a Local, dir: &Path, application: &str, port: u16, json: bool) -> Self {
        local.watch(dir);
        let args = dev_args(dir, port, json);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let running = local.spawn(&refs, &[]);
        Self {
            local,
            dir: dir.to_path_buf(),
            port,
            application: application.to_owned(),
            json,
            running: Some(running),
        }
    }

    fn running(&self) -> &Running {
        self.running
            .as_ref()
            .unwrap_or_else(|| panic!("the harness's own check: `vm dev` was already stopped"))
    }

    /// Whether `vm dev` has ended.
    pub fn has_ended(&mut self) -> bool {
        self.running.as_mut().is_none_or(Running::has_ended)
    }

    /// The process id of `vm dev`.
    pub fn pid(&self) -> u32 {
        self.running().pid()
    }

    /// What it has written on standard output so far.
    pub fn stdout(&self) -> String {
        self.running().stdout_so_far()
    }

    /// Its `--json` lines so far.
    pub fn lines(&self) -> Vec<Value> {
        json_lines(&self.stdout())
    }

    /// Its `--json` lines so far whose `event` is this one.
    pub fn events(&self, event: &str) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|l| l["event"] == event)
            .collect()
    }

    /// Waits until it says it listens: with `--json` the `listening` line,
    /// in words its address. Fails the test, with what it printed, if it
    /// ends first or takes longer than a first local run may.
    pub fn wait_until_listening(&mut self) {
        let deadline = Instant::now() + LOCAL_TIMEOUT;
        loop {
            let said = if self.json {
                !self.events("listening").is_empty()
            } else {
                self.stdout().contains(&self.url())
            };
            if said {
                return;
            }
            if self.has_ended() || Instant::now() >= deadline {
                let run = self.running.take().map(|r| r.wait(Duration::from_secs(5)));
                panic!(
                    "expected `vm dev` to say that it listens at {}\n{}",
                    self.url(),
                    run.map_or_else(String::new, |r| r.describe())
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits up to `limit` for a `--json` line for which `matches` holds.
    pub fn wait_for_line(
        &self,
        what: &str,
        limit: Duration,
        matches: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(found) = self.lines().into_iter().find(|l| matches(l)) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "expected `vm dev` to print {what} within {limit:?}, it printed:\n{}",
                self.stdout()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// `127.0.0.1:<port>`.
    pub fn address(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// `<application>.localhost:<port>`.
    pub fn host(&self) -> String {
        format!("{}.localhost:{}", self.application, self.port)
    }

    /// `http://<application>.localhost:<port>/`.
    pub fn url(&self) -> String {
        format!("http://{}/", self.host())
    }

    /// A `GET` of `target` with the Application's name in `Host` and no
    /// cookie.
    pub fn request(&self, target: &str) -> Request {
        Request::new(self.address(), target, &self.host()).timeout(DEV_REQUEST_TIMEOUT)
    }

    /// A browser with no cookie yet.
    pub fn browser(&self) -> Browser {
        Browser::new(self.address(), &self.host())
    }

    /// The Postgres it runs, as its `postmaster.pid` says.
    pub fn postmaster(&self) -> Postmaster {
        postmaster_of(&self.dir).unwrap_or_else(|| {
            panic!(
                "expected the Postgres of `vm dev` to listen while it runs, under .vm/local/\n{}",
                self.stdout()
            )
        })
    }

    /// Sends `signal` and waits for it to end.
    pub fn stop(mut self, signal: &str) -> Run {
        let running = self
            .running
            .take()
            .unwrap_or_else(|| panic!("the harness's own check: `vm dev` was already stopped"));
        running.signal(signal);
        running.wait(STOP_WAIT)
    }

    /// Kills it outright (`SIGKILL`), leaving what it started.
    pub fn kill(mut self) -> Run {
        let running = self
            .running
            .take()
            .unwrap_or_else(|| panic!("the harness's own check: `vm dev` was already stopped"));
        running.signal("KILL");
        running.wait(Duration::from_secs(30))
    }
}

impl Drop for Dev<'_> {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            if !running.has_ended() {
                // A test that failed first: asked to end as a Builder would,
                // then killed by `wait` at its end; what is left is the
                // `Local`'s to kill.
                drop(
                    std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(running.pid().to_string())
                        .status(),
                );
            }
            drop(running.wait(Duration::from_secs(30)));
        }
    }
}

/// A browser, as the harness plays one. See the module's text.
#[derive(Clone, Debug)]
pub struct Browser {
    pub address: SocketAddr,
    pub host: String,
    /// The cookies it holds, name and value, in the order first given.
    pub cookies: Vec<(String, String)>,
    /// `Accept` of every request but those that say otherwise.
    pub accept: String,
}

impl Browser {
    pub fn new(address: SocketAddr, host: &str) -> Self {
        Self {
            address,
            host: host.to_owned(),
            cookies: Vec::new(),
            accept: "text/html,application/xhtml+xml,*/*;q=0.8".to_owned(),
        }
    }

    /// The `Origin` of its pages: `http://<host>`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.host)
    }

    /// The value of the cookie `name`, when it holds one.
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    /// Holds this cookie as if a page had set it.
    pub fn set_cookie(&mut self, name: &str, value: &str) {
        self.cookies.retain(|(n, _)| n != name);
        self.cookies.push((name.to_owned(), value.to_owned()));
    }

    /// A request of `method` and `target`, with `Host`, `Accept` and its
    /// cookies, not yet sent.
    pub fn build(&self, method: &str, target: &str) -> Request {
        let mut request = Request::new(self.address, target, &self.host)
            .method(method)
            .timeout(DEV_REQUEST_TIMEOUT)
            .header("Accept", &self.accept);
        if !self.cookies.is_empty() {
            let cookie: Vec<String> = self
                .cookies
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect();
            request = request.header("Cookie", cookie.join("; "));
        }
        request
    }

    /// Sends a request built by [`Browser::build`] (and perhaps changed)
    /// and keeps the cookies of its answer.
    pub fn send(&mut self, request: &Request) -> Response {
        let response = request.send();
        for set in response.headers_named("set-cookie") {
            let pair = set.split(';').next().unwrap_or_default();
            if let Some((name, value)) = pair.split_once('=') {
                let gone = set
                    .split(';')
                    .any(|a| a.trim().eq_ignore_ascii_case("max-age=0"));
                if gone {
                    self.cookies.retain(|(n, _)| n != name.trim());
                } else {
                    self.set_cookie(name.trim(), value.trim());
                }
            }
        }
        response
    }

    pub fn get(&mut self, target: &str) -> Response {
        let request = self.build("GET", target);
        self.send(&request)
    }

    /// A `POST` of a form (`application/x-www-form-urlencoded`) with this
    /// `Origin` (none when `None`).
    pub fn post_form(
        &mut self,
        target: &str,
        fields: &[(&str, &str)],
        origin: Option<&str>,
    ) -> Response {
        let body: Vec<String> = fields
            .iter()
            .map(|(n, v)| format!("{}={}", percent_encode(n), percent_encode(v)))
            .collect();
        let mut request = self
            .build("POST", target)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.join("&"));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        self.send(&request)
    }

    /// Chooses the test User `id` on `/_vm/test-user`, from its own origin;
    /// fails the test unless it is answered `303`.
    pub fn choose(&mut self, id: &str) -> Response {
        let origin = self.origin();
        let response = self.post_form("/_vm/test-user", &[("user", id)], Some(&origin));
        assert_eq!(
            response.status,
            303,
            "expected choosing the test User {id:?} answered 303\n{}",
            response.describe()
        );
        response
    }
}

/// `value` percent-encoded: every byte but the unreserved ones
/// (`A-Z a-z 0-9 - . _ ~`) as `%XX`.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// `value` with each `%XX` taken back to its byte, and, in a form, each
/// `+` to a space; `None` when that is not UTF-8 or a `%` is not followed by
/// two hex digits.
pub fn percent_decode(value: &str, form: bool) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        match b {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            b'+' if form => {
                out.push(b' ');
                at += 1;
            }
            _ => {
                out.push(b);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// `text` with HTML's character references for `& < > " '` taken back (the
/// named ones and the numeric ones in decimal or hex), so that a test can
/// look for a text in a page whatever way it was escaped.
pub fn html_unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        rest = rest.get(at..).unwrap_or_default();
        let end = rest.find(';').filter(|e| *e <= 10);
        let entity = end.and_then(|e| rest.get(1..e));
        let decoded = entity.and_then(|name| match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let number = name.strip_prefix('#')?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = rest.get(e + 1..).unwrap_or_default();
            }
            _ => {
                out.push('&');
                rest = rest.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// A socket that listens, as `/proc/net/tcp` or `/proc/net/tcp6` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    /// `127.0.0.1`, `0.0.0.0`, `::1`, `::`, or another address, as text.
    pub address: String,
    pub port: u16,
}

fn ipv4_of(hex: &str) -> Option<String> {
    let n = u32::from_str_radix(hex, 16).ok()?;
    // The kernel writes the address in the machine's byte order.
    let bytes = n.to_le_bytes();
    Some(Ipv4Addr::from(bytes).to_string())
}

fn ipv6_of(hex: &str) -> Option<String> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    for (word, chunk) in hex.as_bytes().chunks(8).enumerate() {
        let n = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        for (i, b) in n.to_le_bytes().iter().enumerate() {
            *bytes.get_mut(word * 4 + i)? = *b;
        }
    }
    Some(std::net::Ipv6Addr::from(bytes).to_string())
}

/// Every socket of this machine that listens on `port` (TCP, IPv4 and IPv6).
pub fn listeners(port: u16) -> Vec<Listener> {
    let mut found = Vec::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(state)) = (fields.get(1), fields.get(3)) else {
                continue;
            };
            if *state != "0A" {
                continue;
            }
            let Some((address, hex_port)) = local.split_once(':') else {
                continue;
            };
            let Ok(p) = u16::from_str_radix(hex_port, 16) else {
                continue;
            };
            if p != port {
                continue;
            }
            let address = if v6 {
                ipv6_of(address)
            } else {
                ipv4_of(address)
            };
            if let Some(address) = address {
                found.push(Listener { address, port: p });
            }
        }
    }
    found
}

/// Whether the machine has the IPv6 loopback address `::1`.
pub fn has_ipv6_loopback() -> bool {
    fs::read_to_string("/proc/net/if_inet6").is_ok_and(|t| {
        t.lines()
            .any(|l| l.starts_with("00000000000000000000000000000001"))
    })
}

/// The files of `.vm/local/` of the project in `dir`, outside the data
/// folder of its Postgres (when `data` names it) and but for `vm.log` and
/// `postgres.log`, whose bytes hold `needle`.
pub fn local_files_holding(dir: &Path, data: Option<&Path>, needle: &[u8]) -> Vec<PathBuf> {
    let local = dir.join(".vm").join("local");
    let data = data.and_then(|d| fs::canonicalize(d).ok());
    crate::local::files_holding(&local, needle)
        .into_iter()
        .filter(|f| {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let inside_data = data
                .as_ref()
                .is_some_and(|d| fs::canonicalize(f).is_ok_and(|c| c.starts_with(d)));
            !inside_data && name != "vm.log" && name != "postgres.log"
        })
        .collect()
}

/// The runs of exactly 64 lower-case hex digits in `bytes` (a run of more
/// is not one): what the secret of the file that finds `vm dev` is, and
/// what a password written the same way would be.
pub fn hex64_runs(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| !b.is_ascii_hexdigit())
        .filter(|run| run.len() == 64 && run.iter().all(|b| !b.is_ascii_uppercase()))
        .map(|run| String::from_utf8_lossy(run).into_owned())
        .collect()
}

/// Every file of `.vm/local/` of the project in `dir` but those of its
/// Postgres (inside `data`, and `postgres.log`) and `vm.log`.
pub fn local_files_but_postgres(dir: &Path, data: &Path) -> Vec<PathBuf> {
    let data = fs::canonicalize(data).ok();
    let mut files = Vec::new();
    crate::local::every_file(&dir.join(".vm").join("local"), &mut files);
    files
        .into_iter()
        .filter(|f| {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let inside_data = data
                .as_ref()
                .is_some_and(|d| fs::canonicalize(f).is_ok_and(|c| c.starts_with(d)));
            !inside_data && name != "vm.log" && name != "postgres.log"
        })
        .collect()
}

/// Fails unless nothing a run of `local` started is running (after up to
/// five seconds), the Postgres `postmaster` names (when given) is stopped
/// and its port closed, nothing listens on `port` any more, and the
/// machine's `TMPDIR` is empty.
pub fn assert_nothing_left(
    local: &Local,
    postmaster: Option<&Postmaster>,
    port: u16,
    context: &str,
) {
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "{context}: expected no process the local run started still running, found {left:#?}"
    );
    if let Some(postmaster) = postmaster {
        assert!(
            !crate::project::is_running(postmaster.pid),
            "{context}: expected the private Postgres ({}) stopped",
            postmaster.pid
        );
        assert!(
            !crate::local::port_open(postmaster.port),
            "{context}: expected the private Postgres's port {} closed",
            postmaster.port
        );
    }
    assert!(
        !crate::local::port_open(port),
        "{context}: expected nothing listening on {port} any more"
    );
    assert_eq!(
        crate::generate::listed(&local.tmp()),
        Vec::<String>::new(),
        "{context}: expected the run's own folder in TMPDIR removed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_goes_there_and_back() {
        let id = "eve <i>\"x\"</i> é/1?&=+";
        let encoded = percent_encode(id);
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b))
        );
        assert_eq!(percent_decode(&encoded, false).as_deref(), Some(id));
        assert_eq!(percent_decode("a+b", true).as_deref(), Some("a b"));
        assert_eq!(percent_decode("%zz", false), None);
    }

    #[test]
    fn html_entities_are_taken_back() {
        assert_eq!(
            html_unescaped("&lt;i&gt; &quot;x&#34; &#x27;y&#39; &amp;amp; & alone"),
            "<i> \"x\" 'y' &amp; & alone"
        );
    }

    #[test]
    fn json_lines_leave_out_a_line_not_yet_ended() {
        let lines = json_lines("{\"event\":\"listening\"}\n{\"event\":\"req");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn addresses_of_proc_net_are_read() {
        assert_eq!(ipv4_of("0100007F").as_deref(), Some("127.0.0.1"));
        assert_eq!(
            ipv6_of("00000000000000000000000001000000").as_deref(),
            Some("::1")
        );
    }

    #[test]
    fn a_run_of_64_hex_digits_is_found_and_a_longer_or_upper_case_one_is_not() {
        let secret = "0123456789abcdef".repeat(4);
        let longer = format!("{secret}0");
        let upper = secret.to_uppercase();
        let file = format!(
            "{{\"port\":7480,\"secret\":\"{secret}\",\"x\":\"{longer}\",\"y\":\"{upper}\"}}"
        );
        assert_eq!(hex64_runs(file.as_bytes()), [secret]);
    }
}
