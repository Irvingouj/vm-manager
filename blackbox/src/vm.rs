//! A running `vm serve` (CONTRACT.md, "Serving"): started on a free port
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

/// The User who installs fixture Applications for the tests, and so is
/// their creator and only Contributor.
pub const INSTALLER: &str = "fixture-installer";

/// A fixture Application installed by the harness is its first Version.
pub const FIXTURE_VERSION: u64 = 1;

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

/// How to start `vm serve`: "this program, with these fixture
/// Applications".
#[derive(Clone, Debug)]
pub struct VmOptions {
    domain: String,
    applications: Vec<(String, PathBuf)>,
    stand_in_sign_in: bool,
    administrators_group: Option<String>,
    platform: Option<Platform>,
}

impl Default for VmOptions {
    fn default() -> Self {
        Self {
            domain: DOMAIN.to_owned(),
            applications: Vec::new(),
            stand_in_sign_in: false,
            administrators_group: None,
            platform: None,
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

    /// Starts the program against this platform database rather than a
    /// fresh one: to start it again on what an earlier program left.
    #[must_use]
    pub fn platform(mut self, platform: &Platform) -> Self {
        self.platform = Some(platform.clone());
        self
    }

    /// The `vm` arguments for these options. `VM_DATABASE_URL` goes in the
    /// environment, never here.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--domain".to_owned(),
            self.domain.clone(),
            "--listen".to_owned(),
            "127.0.0.1:0".to_owned(),
        ];
        if self.stand_in_sign_in {
            args.push("--stand-in-sign-in".to_owned());
        }
        if let Some(group) = &self.administrators_group {
            args.push("--administrators-group".to_owned());
            args.push(group.clone());
        }
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

/// A running `vm serve`. Dropping it ends the process.
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
    /// Starts `vm serve` with these options, waits for its `listening`
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
            let installer = Self::launch(&options.clone().stand_in_sign_in(), &platform);
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

    /// Creates the Application `name` as [`INSTALLER`] and sends `bundle` as
    /// a Version, failing the test with the program's log unless both are
    /// answered `201`.
    pub fn install(&self, name: &str, bundle: &str) {
        let api = self.api_as(&User::new(INSTALLER));
        let created = api.create_application(name);
        if created.status != 201 {
            self.fail(&format!(
                "expected `201` creating the fixture Application {name:?} through the platform's API, got\n{}",
                created.describe()
            ));
        }
        let sent = api.send_version(name, &version_body(bundle));
        if sent.status != 201 {
            self.fail(&format!(
                "expected `201` sending the fixture Application {name:?}'s bundle as a Version, got\n{}",
                sent.describe()
            ));
        }
    }

    /// Starts `vm serve` and waits for its `listening` record.
    fn launch(options: &VmOptions, platform: &Platform) -> Self {
        let args = options.args();
        let command_line = format!(
            "VM_DATABASE_URL=<{} on the test Postgres> {} {}",
            platform.name(),
            vm_binary().display(),
            args.join(" ")
        );
        let mut child = Command::new(vm_binary())
            .args(&args)
            .env("VM_DATABASE_URL", platform.url())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start `{command_line}`: {e}"));
        let stderr = child.stderr.take().expect("the child's stderr is piped");
        let mut process = ProcessGuard(child);
        let log = LogCollector::spawn(stderr);
        let found = log.wait_for(START_TIMEOUT, |lines| {
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
                    WaitFailure::TimedOut => format!("did not write it within {START_TIMEOUT:?}"),
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
                    "expected `vm serve` to write a `listening` record, but it {why}.\ncommand: {command_line}\nits standard error:\n{}",
                    log.tail(LOG_LINES_SHOWN)
                );
                // End the process before the log reader is joined, so that
                // the reader sees the end of the stream at once.
                drop(process);
                drop(log);
                panic!("{message}");
            }
        };
        let address = listening["address"]
            .as_str()
            .and_then(|a| a.parse::<SocketAddr>().ok())
            .unwrap_or_else(|| {
                panic!(
                    "expected the `listening` record to hold an ip:port `address`, got {listening}"
                )
            });
        Self {
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
        }
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

    /// A `GET` of `target` on the platform's API, not signed in.
    pub fn api_request(&self, target: &str) -> Request {
        Request::new(self.address, target, &self.api_host())
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

    /// A `GET` of `target` on an Application, not signed in.
    pub fn request(&self, application: &str, target: &str) -> Request {
        Request::new(self.address, target, &self.host_of(application))
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
            self.log.tail(LOG_LINES_SHOWN)
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
                self.log.tail(LOG_LINES_SHOWN)
            );
        }
        // Then `process` is dropped, ending the program, and `log` joins its
        // reader.
        let _ = &self.process;
    }
}
