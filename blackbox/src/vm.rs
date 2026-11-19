//! A running `vibe serve` (CONTRACT.md, "Serving"): started on a free port
//! against a platform database on the harness's Postgres, with chosen
//! fixture Applications installed through the platform's API, and ended when
//! the handle is dropped, also when the test panics.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::api::{Api, version_body};
use crate::http::{Request, Response, User};
use crate::log::{Line, LogCollector, WaitFailure};
use crate::postgres::Platform;
use crate::program::vm_binary;
use crate::records::{self, RequestRecords, describe, event, request_of, user_of};

/// The installation's domain the tests serve under.
pub const DOMAIN: &str = "vm.test";

/// How long the program may take to write its `listening` record.
pub const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a test waits for a record it expects in the log.
pub const LOG_TIMEOUT: Duration = Duration::from_secs(5);

/// How many lines of the program's log a failure message shows.
const LOG_LINES_SHOWN: usize = 200;

/// How many lines of the test Postgres's server log a failure to start
/// shows.
const POSTGRES_LOG_LINES_SHOWN: usize = 40;

/// The User who installs fixture Applications for the tests, and so is
/// their creator and only Contributor.
pub const INSTALLER: &str = "fixture-installer";

/// A fixture Application installed by the harness is its first Version.
pub const FIXTURE_VERSION: u64 = 1;

/// Whether `message` (from a start of `vibe serve` that failed) is the
/// program's own refusal of `address` as taken: it exited saying it cannot
/// listen there because the address is in use. Nothing else counts.
pub fn refused_address(address: SocketAddr, message: &str) -> bool {
    message.contains("exited (")
        && message.contains(&format!(
            "cannot listen on {address}: Address already in use"
        ))
}

/// The path of a fixture Application under `blackbox/fixtures/`.
pub fn fixture(file: &str) -> PathBuf {
    let path = crate::program::manifest_dir().join("fixtures").join(file);
    assert!(path.is_file(), "no fixture Application {}", path.display());
    path
}

/// The text of a fixture Application: the bundle a Version is sent with.
pub fn fixture_bundle(file: &str) -> String {
    let path = fixture(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the fixture {}: {e}", path.display()))
}

/// How to start `vibe serve`: "this program, with these fixture
/// Applications".
#[derive(Clone, Debug)]
pub struct VmOptions {
    domain: String,
    applications: Vec<(String, PathBuf)>,
    stand_in_sign_in: bool,
    administrators_group: Option<String>,
    tool_credential_lifetime: Option<u64>,
    platform: Option<Platform>,
    /// The address to listen on; `127.0.0.1:0` when none is given.
    listen: Option<SocketAddr>,
    /// More arguments, after the others (the OIDC settings, say).
    extra_args: Vec<String>,
    /// Environment variables the program is started with.
    env: Vec<EnvVar>,
    /// How the harness reaches a program that serves TLS itself
    /// (CONTRACT.md, "The program serves TLS itself"): `None` for one that
    /// listens in plain HTTP. Nothing of it is given to the program.
    tls: Option<TlsClient>,
    /// Texts never shown in a failure message of this program's: the body
    /// lines of a key the program was given, say. A failure message shows
    /// `<hidden>` in their place.
    hidden: Vec<Hidden>,
}

/// How the harness's clients reach a program that serves TLS itself: the
/// authorities they trust for its certificate, and a file holding them
/// for the tool's `VIBE_CA_FILE`.
#[derive(Clone, Debug)]
pub struct TlsClient {
    pub roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    pub ca_file: PathBuf,
}

/// A text never shown; its `Debug` shows nothing of it.
#[derive(Clone)]
struct Hidden(String);

impl std::fmt::Debug for Hidden {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<hidden>")
    }
}

/// An environment variable for the program. Its value may be a secret
/// (`VIBE_OIDC_CLIENT_SECRET`), so `Debug` shows only its name.
#[derive(Clone)]
struct EnvVar {
    name: String,
    value: String,
}

impl std::fmt::Debug for EnvVar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}=<hidden>", self.name)
    }
}

impl Default for VmOptions {
    fn default() -> Self {
        Self {
            domain: DOMAIN.to_owned(),
            applications: Vec::new(),
            stand_in_sign_in: false,
            administrators_group: None,
            tool_credential_lifetime: None,
            platform: None,
            listen: None,
            extra_args: Vec::new(),
            env: Vec::new(),
            tls: None,
            hidden: Vec::new(),
        }
    }
}

impl VmOptions {
    /// A fresh platform database, no Applications, the stand-in sign-in off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a fixture Application at start: `file` under
    /// `blackbox/fixtures/`, created as `name` by [`INSTALLER`] and sent as
    /// its first Version.
    #[must_use]
    pub fn application(mut self, name: &str, file: &str) -> Self {
        self.applications.push((name.to_owned(), fixture(file)));
        self
    }

    /// Starts the program with `--stand-in-sign-in`.
    #[must_use]
    pub fn stand_in_sign_in(mut self) -> Self {
        self.stand_in_sign_in = true;
        self
    }

    /// Starts the program with `--administrators-group <group>`.
    #[must_use]
    pub fn administrators_group(mut self, group: &str) -> Self {
        self.administrators_group = Some(group.to_owned());
        self
    }

    /// Starts the program with `--tool-credential-lifetime <seconds>`: tool
    /// credentials made by it end this long after they were made.
    #[must_use]
    pub fn tool_credential_lifetime(mut self, seconds: u64) -> Self {
        self.tool_credential_lifetime = Some(seconds);
        self
    }

    /// Starts the program against this platform database rather than a
    /// fresh one: to start it again on what an earlier program left.
    #[must_use]
    pub fn platform(mut self, platform: &Platform) -> Self {
        self.platform = Some(platform.clone());
        self
    }

    /// Starts the program listening on this address: to start it again
    /// where the tool's credential file says it is.
    #[must_use]
    pub fn listen(mut self, address: SocketAddr) -> Self {
        self.listen = Some(address);
        self
    }

    /// Adds these arguments after the others.
    #[must_use]
    pub fn args_extra(mut self, args: &[&str]) -> Self {
        self.extra_args.extend(args.iter().map(|a| (*a).to_owned()));
        self
    }

    /// The value after this argument among the extra ones, if given.
    pub fn extra_arg_value(&self, name: &str) -> Option<&str> {
        self.extra_args
            .iter()
            .position(|a| a == name)
            .and_then(|at| self.extra_args.get(at + 1))
            .map(String::as_str)
    }

    /// Starts the program with this environment variable set, in place of
    /// any value given for it before.
    #[must_use]
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.retain(|e| e.name != name);
        self.env.push(EnvVar {
            name: name.to_owned(),
            value: value.to_owned(),
        });
        self
    }

    /// Starts the program without this environment variable.
    #[must_use]
    pub fn without_env(mut self, name: &str) -> Self {
        self.env.retain(|e| e.name != name);
        self
    }

    /// The value given for this environment variable, if any. It may be a
    /// secret: never print it.
    pub fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.value.as_str())
    }

    /// The harness reaches the program over TLS, trusting these
    /// authorities: the program serves TLS itself (its two files are given
    /// with [`VmOptions::env`]). Requests the harness makes of it
    /// ([`Vm::api_request`], [`Vm::request`]) go over TLS, asking for the
    /// `Host`'s name.
    #[must_use]
    pub fn reached_over_tls(mut self, client: TlsClient) -> Self {
        self.tls = Some(client);
        self
    }

    /// How the harness reaches the program over TLS, when it serves TLS
    /// itself.
    pub fn tls_client(&self) -> Option<&TlsClient> {
        self.tls.as_ref()
    }

    /// Never shows `text` in a failure message of this program's.
    #[must_use]
    pub fn hide(mut self, text: &str) -> Self {
        if !text.is_empty() {
            self.hidden.push(Hidden(text.to_owned()));
        }
        self
    }

    /// `text` with every hidden text of these options replaced by
    /// `<hidden>`.
    pub fn masked(&self, text: &str) -> String {
        self.hidden
            .iter()
            .fold(text.to_owned(), |t, h| t.replace(&h.0, "<hidden>"))
    }

    /// The same options without the extra arguments and environment, and
    /// with the stand-in sign-in on: what installs the fixture Applications
    /// before a program without the stand-in starts. It listens in plain
    /// HTTP, as ever.
    fn installer(&self) -> Self {
        Self {
            extra_args: Vec::new(),
            env: Vec::new(),
            tls: None,
            ..self.clone()
        }
        .stand_in_sign_in()
    }

    /// The `vibe` arguments for these options. `VIBE_DATABASE_URL` goes in the
    /// environment, never here.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--domain".to_owned(),
            self.domain.clone(),
            "--listen".to_owned(),
            self.listen
                .map_or_else(|| "127.0.0.1:0".to_owned(), |a| a.to_string()),
        ];
        if self.stand_in_sign_in {
            args.push("--stand-in-sign-in".to_owned());
        }
        if let Some(group) = &self.administrators_group {
            args.push("--administrators-group".to_owned());
            args.push(group.clone());
        }
        if let Some(seconds) = self.tool_credential_lifetime {
            args.push("--tool-credential-lifetime".to_owned());
            args.push(seconds.to_string());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Ends the process when dropped, also while a test panics.
struct ProcessGuard(Child);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// A running `vibe serve`. Dropping it ends the process.
pub struct Vm {
    // Fields drop in this order: the process ends first, so that the log
    // reader sees the end of the stream and can be joined.
    process: ProcessGuard,
    log: LogCollector,
    address: SocketAddr,
    domain: String,
    command_line: String,
    /// The index of the `listening` line in the log.
    listening_at: usize,
    listening: Value,
    platform: Platform,
    /// What it was started with, the fixture Applications left out: what a
    /// restart starts again with.
    options: VmOptions,
}

impl Vm {
    /// Starts `vibe serve` with these options, waits for its `listening`
    /// record, and installs the fixture Applications through the platform's
    /// API. Fails the test with the program's log if any of it goes wrong.
    ///
    /// The platform's API needs a signed-in User, so with the stand-in off
    /// the fixtures are installed by an earlier start of the program with
    /// the stand-in on, against the same platform database, which is then
    /// ended (CONTRACT.md: a restart loses nothing).
    pub fn start(options: &VmOptions) -> Self {
        let platform = options.platform.clone().unwrap_or_else(Platform::fresh);
        if options.stand_in_sign_in {
            let vm = Self::launch(options, &platform);
            vm.install_all(&options.applications);
            return vm;
        }
        if !options.applications.is_empty() {
            let installer = Self::launch(&options.installer(), &platform);
            installer.install_all(&options.applications);
            drop(installer);
        }
        Self::launch(options, &platform)
    }

    /// Starts the program with the stand-in sign-in on and these
    /// Applications, given as (name, fixture file).
    pub fn with_stand_in(applications: &[(&str, &str)]) -> Self {
        let options = applications
            .iter()
            .fold(VmOptions::new().stand_in_sign_in(), |o, (name, file)| {
                o.application(name, file)
            });
        Self::start(&options)
    }

    /// Starts the program with the stand-in sign-in on, against what an
    /// earlier program left in this platform database.
    pub fn start_on(platform: &Platform) -> Self {
        Self::start(&VmOptions::new().stand_in_sign_in().platform(platform))
    }

    /// Ends this program and starts it again with the same options,
    /// against the same platform database.
    #[must_use]
    pub fn restart(self) -> Self {
        let options = self.options.clone().platform(&self.platform);
        drop(self);
        Self::start(&options)
    }

    /// The options this program was started with, without its fixture
    /// Applications.
    pub fn options(&self) -> &VmOptions {
        &self.options
    }

    fn install_all(&self, applications: &[(String, PathBuf)]) {
        for (name, path) in applications {
            let bundle = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("cannot read the fixture {}: {e}", path.display()));
            self.install(name, &bundle);
        }
    }

    /// Creates the Application `name` as [`INSTALLER`], shares it with
    /// everyone (the tests that use a fixture are not about who can open it;
    /// those that are make their Applications themselves) and sends `bundle`
    /// as a Version, failing the test with the program's log unless each is
    /// done.
    pub fn install(&self, name: &str, bundle: &str) {
        let api = self.api_as(&User::new(INSTALLER));
        let created = api.create_application(name);
        if created.status != 201 {
            self.fail(&format!(
                "expected `201` creating the fixture Application {name:?} through the platform's API, got\n{}",
                created.describe()
            ));
        }
        if !crate::history_earlier::earlier_program_chosen() {
            api.share_with_everyone(name);
        }
        let sent = api.send_version(name, &version_body(bundle));
        if sent.status != 201 {
            self.fail(&format!(
                "expected `201` sending the fixture Application {name:?}'s bundle as a Version, got\n{}",
                sent.describe()
            ));
        }
    }

    /// Starts `vibe serve` with the stand-in sign-in's options as given and no
    /// fixture Applications, and waits up to `within` for its `listening`
    /// record: `Err` (the program ended, its log in the message) when it
    /// does not listen by then, for a test in which not starting is allowed.
    pub fn try_start(options: &VmOptions, within: Duration) -> Result<Self, String> {
        let platform = options.platform.clone().unwrap_or_else(Platform::fresh);
        Self::try_launch(options, &platform, within)
    }

    /// As [`Vm::start`] with the stand-in sign-in on, listening at
    /// 127.0.0.1 on a port the harness chooses rather than one the system
    /// gives (`:0`): for a test that starts the program again at the same
    /// address. When the program ends refusing that port as taken
    /// ([`refused_address`]) it is started again on another, at most
    /// [`crate::dev::PORT_TRIES`] times, saying so on standard error; any
    /// other failure fails the test as [`Vm::start`] does.
    pub fn start_at_a_chosen_port(options: &VmOptions) -> Self {
        Self::start_choosing(options, || {
            SocketAddr::from(([127, 0, 0, 1], crate::dev::free_port()))
        })
    }

    /// As [`Vm::start_at_a_chosen_port`], with the addresses `choose` gives:
    /// for the harness's own checks, which hand it a port they hold.
    pub fn start_choosing(options: &VmOptions, mut choose: impl FnMut() -> SocketAddr) -> Self {
        assert!(
            options.stand_in_sign_in,
            "the harness's own check: a start at a chosen port is for the stand-in sign-in"
        );
        let platform = options.platform.clone().unwrap_or_else(Platform::fresh);
        let mut attempt = 1;
        loop {
            let address = choose();
            let tried = options.clone().listen(address);
            match Self::try_launch(&tried, &platform, START_TIMEOUT) {
                Ok(vm) => {
                    vm.install_all(&tried.applications);
                    return vm;
                }
                Err(message) => {
                    if attempt < crate::dev::PORT_TRIES && refused_address(address, &message) {
                        crate::dev::say_port_taken("`vibe serve`", address.port(), attempt);
                        attempt += 1;
                        continue;
                    }
                    panic!("{message}");
                }
            }
        }
    }

    /// Starts `vibe serve` and waits for its `listening` record.
    fn launch(options: &VmOptions, platform: &Platform) -> Self {
        Self::try_launch(options, platform, START_TIMEOUT)
            .unwrap_or_else(|message| panic!("{message}"))
    }

    /// Starts `vibe serve` and waits up to `within` for its `listening`
    /// record.
    fn try_launch(
        options: &VmOptions,
        platform: &Platform,
        within: Duration,
    ) -> Result<Self, String> {
        let args = options.args();
        let command_line = format!(
            "VIBE_DATABASE_URL=<{} on the test Postgres> {} {}",
            platform.name(),
            vm_binary().display(),
            args.join(" ")
        );
        let mut child = Command::new(vm_binary())
            .args(&args)
            .env("VIBE_DATABASE_URL", platform.url())
            .env_remove("VIBE_OIDC_CLIENT_SECRET")
            .env_remove("VIBE_SECRET_KEY")
            .env_remove("VIBE_SETUP_CREDENTIAL")
            .envs(options.env.iter().map(|e| (&e.name, &e.value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start `{command_line}`: {e}"));
        let stderr = child.stderr.take().expect("the child's stderr is piped");
        let mut process = ProcessGuard(child);
        let log = LogCollector::spawn(stderr);
        let found = log.wait_for(within, |lines| {
            lines.iter().enumerate().find_map(|(at, line)| {
                line.record
                    .as_ref()
                    .filter(|r| event(r) == "listening")
                    .map(|r| (at, r.clone()))
            })
        });
        let (listening_at, listening) = match found {
            Ok(found) => found,
            Err(failure) => {
                let why = match failure {
                    WaitFailure::TimedOut => format!("did not write it within {within:?}"),
                    WaitFailure::Overflowed => "wrote too many lines first".to_owned(),
                    WaitFailure::Ended => {
                        let status = process
                            .0
                            .wait()
                            .map_or_else(|e| e.to_string(), |s| s.to_string());
                        format!("exited ({status}) before writing it")
                    }
                };
                let message = format!(
                    "expected `vibe serve` to write a `listening` record, but it {why}.\ncommand: {command_line}\nits standard error:\n{}\nthe last {POSTGRES_LOG_LINES_SHOWN} lines of the test Postgres's server log:\n{}",
                    options.masked(&log.tail(LOG_LINES_SHOWN)),
                    crate::postgres::shared_log_tail(POSTGRES_LOG_LINES_SHOWN)
                );
                // End the process before the log reader is joined, so that
                // the reader sees the end of the stream at once.
                drop(process);
                drop(log);
                return Err(message);
            }
        };
        let Some(address) = listening["address"]
            .as_str()
            .and_then(|a| a.parse::<SocketAddr>().ok())
        else {
            drop(process);
            drop(log);
            return Err(format!(
                "expected the `listening` record to hold an ip:port `address`, got {listening}"
            ));
        };
        Ok(Self {
            process,
            log,
            address,
            domain: options.domain.clone(),
            command_line,
            listening_at,
            listening,
            platform: platform.clone(),
            options: VmOptions {
                applications: Vec::new(),
                ..options.clone()
            },
        })
    }

    /// The platform database this program was started against.
    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    /// The platform's API, as this User (stand-in sign-in).
    pub fn api_as(&self, user: &User) -> Api<'_> {
        Api::new(self, user)
    }

    /// The `Host` of the platform's API: `api.<domain>`.
    pub fn api_host(&self) -> String {
        format!("api.{}", self.domain)
    }

    /// A `GET` of `target` on the platform's API, not signed in; over TLS
    /// when the program serves TLS itself ([`VmOptions::reached_over_tls`]).
    pub fn api_request(&self, target: &str) -> Request {
        self.reached(Request::new(self.address, target, &self.api_host()))
    }

    /// `request` over TLS, asking for its `Host`'s name and trusting the
    /// authorities of [`VmOptions::reached_over_tls`], when the program
    /// serves TLS itself; as it is otherwise.
    fn reached(&self, request: Request) -> Request {
        match &self.options.tls {
            Some(client) => {
                let name = request.host_name();
                request.over_tls(&name, &client.roots)
            }
            None => request,
        }
    }

    /// How the harness reaches this program over TLS, when it serves TLS
    /// itself.
    pub fn tls_client(&self) -> Option<&TlsClient> {
        self.options.tls.as_ref()
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The `listening` record.
    pub fn listening(&self) -> &Value {
        &self.listening
    }

    /// The `Host` of an Application: `<application>.<domain>`.
    pub fn host_of(&self, application: &str) -> String {
        format!("{application}.{}", self.domain)
    }

    /// A `GET` of `target` on an Application, not signed in; over TLS when
    /// the program serves TLS itself.
    pub fn request(&self, application: &str, target: &str) -> Request {
        self.reached(Request::new(
            self.address,
            target,
            &self.host_of(application),
        ))
    }

    /// A `GET` of `target` on an Application as this User.
    pub fn get_as(&self, user: &User, application: &str, target: &str) -> Response {
        self.request(application, target).as_user(user).send()
    }

    /// Every JSON record in the log so far, in order. A copy, so that the
    /// test can look at it while the program goes on writing.
    pub fn records(&self) -> Vec<Value> {
        self.log
            .with_lines(|lines| lines.iter().filter_map(|l| l.record.clone()).collect())
    }

    /// The JSON records written after the `listening` record, in order.
    pub fn records_after_listening(&self) -> Vec<Value> {
        self.log.with_lines(|lines| {
            lines
                .iter()
                .skip(self.listening_at + 1)
                .filter_map(|l| l.record.clone())
                .collect()
        })
    }

    /// Every raw line of standard error so far.
    pub fn lines(&self) -> Vec<Line> {
        self.log.with_lines(<[Line]>::to_vec)
    }

    /// Waits for the `request` record of the request this answers, and
    /// returns every record that carries its id.
    pub fn records_of(&self, response: &Response) -> RequestRecords {
        self.records_of_id(&response.request_id())
    }

    /// Waits for the `request` record of this request id, and returns every
    /// record that carries the id.
    pub fn records_of_id(&self, id: &str) -> RequestRecords {
        let found = self.log.wait_for(LOG_TIMEOUT, |lines| {
            let records = || lines.iter().filter_map(|l| l.record.as_ref());
            let request = records()
                .find(|r| event(r) == "request" && request_of(r) == Some(id))?
                .clone();
            let all = records()
                .filter(|r| request_of(r) == Some(id))
                .cloned()
                .collect();
            Some(RequestRecords {
                id: id.to_owned(),
                request,
                all,
            })
        });
        found.unwrap_or_else(|failure| {
            self.fail(&format!(
                "expected a `request` record for request {id:?} within {LOG_TIMEOUT:?} ({failure:?})"
            ))
        })
    }

    /// Waits for a record that `matches`, and returns it.
    pub fn wait_for_record(&self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        self.log
            .wait_for(LOG_TIMEOUT, |lines| {
                lines
                    .iter()
                    .filter_map(|l| l.record.as_ref())
                    .find(|r| matches(r))
                    .cloned()
            })
            .unwrap_or_else(|failure| {
                self.fail(&format!(
                    "expected {what} in the log within {LOG_TIMEOUT:?} ({failure:?})"
                ))
            })
    }

    /// Fails the test with this message and the program's log.
    pub fn fail(&self, message: &str) -> ! {
        panic!(
            "{message}\ncommand: {}\nthe program's log:\n{}",
            self.command_line,
            self.options.masked(&self.log.tail(LOG_LINES_SHOWN))
        )
    }

    /// Fails the test unless the log so far keeps two promises of
    /// CONTRACT.md for every request:
    ///
    /// - the `request` record is written last for its request: no
    ///   `printed`, `uncaught` or `host-call` record carries a request's id
    ///   after that request's `request` record;
    /// - a record carrying a request's id names that request's User, and
    ///   an allowed host call always belongs to a request.
    pub fn assert_log_keeps_each_request_to_itself(&self) {
        let records = self.records();
        let users: HashMap<&str, Option<&str>> = records
            .iter()
            .filter(|r| event(r) == "request")
            .filter_map(|r| Some((request_of(r)?, user_of(r))))
            .collect();
        let mut finished: HashSet<&str> = HashSet::new();
        let mut late = Vec::new();
        let mut misattributed = Vec::new();
        let mut allowed_outside = Vec::new();
        for record in &records {
            if event(record) == "request" {
                if let Some(id) = request_of(record) {
                    finished.insert(id);
                }
                continue;
            }
            if !records::is_from_application_code(record) {
                continue;
            }
            match request_of(record) {
                Some(id) => {
                    if finished.contains(id) {
                        late.push(record);
                    }
                    if let Some(user) = users.get(id)
                        && user_of(record) != *user
                    {
                        misattributed.push(record);
                    }
                }
                None => {
                    if event(record) == "host-call" && record["outcome"] == "allowed" {
                        allowed_outside.push(record);
                    }
                }
            }
        }
        if !late.is_empty() || !misattributed.is_empty() || !allowed_outside.is_empty() {
            self.fail(&format!(
                "expected each request's records to stay with it.\nrecords written after their request's `request` record:\n{}records naming a User other than their request's:\n{}host calls allowed with no request:\n{}",
                describe(&late),
                describe(&misattributed),
                describe(&allowed_outside)
            ));
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "--- the log of `{}` ---\n{}--- end of the log ---",
                self.command_line,
                self.options.masked(&self.log.tail(LOG_LINES_SHOWN))
            );
        }
        // Then `process` is dropped, ending the program, and `log` joins its
        // reader.
        let _ = &self.process;
    }
}
