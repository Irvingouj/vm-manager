//! A running `vm serve` (CONTRACT.md, "Serving"): started on a free port with
//! chosen fixture Applications, ended when the handle is dropped, also when
//! the test panics.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::http::{Request, Response, User};
use crate::log::{Line, LogCollector, WaitFailure};
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

/// The path of a fixture Application under `blackbox/fixtures/`.
pub fn fixture(file: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(file);
    assert!(path.is_file(), "no fixture Application {}", path.display());
    path
}

/// How to start `vm serve`.
#[derive(Clone, Debug)]
pub struct VmOptions {
    domain: String,
    applications: Vec<(String, PathBuf)>,
    stand_in_sign_in: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        Self {
            domain: DOMAIN.to_owned(),
            applications: Vec::new(),
            stand_in_sign_in: false,
        }
    }
}

impl VmOptions {
    /// No Applications yet, and the stand-in sign-in off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a fixture Application: `file` under `blackbox/fixtures/`, served
    /// as `name`.
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

    /// The `vm` arguments for these options.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--domain".to_owned(),
            self.domain.clone(),
            "--listen".to_owned(),
            "127.0.0.1:0".to_owned(),
        ];
        for (name, path) in &self.applications {
            args.push("--application".to_owned());
            args.push(format!("{name}={}", path.display()));
        }
        if self.stand_in_sign_in {
            args.push("--stand-in-sign-in".to_owned());
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
}

impl Vm {
    /// Starts `vm serve` with these options and waits for its `listening`
    /// record, failing the test with the program's log if it does not come
    /// within [`START_TIMEOUT`].
    pub fn start(options: &VmOptions) -> Self {
        let args = options.args();
        let command_line = format!("{} {}", vm_binary().display(), args.join(" "));
        let mut child = Command::new(vm_binary())
            .args(&args)
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
        }
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
