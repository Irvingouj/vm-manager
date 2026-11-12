//! What the tests of "A User's read of a shared database is authorized and
//! logged" and "Write access, and the database role as the boundary" share:
//! a program with a Platform administrators' Group, a shared database
//! registered as a Resource (with or without a write login), Grants, fixture
//! Applications that declare it, and checks on what `query` and `execute`
//! answer and on Operation log entries (CONTRACT.md, "Resources and
//! Grants", "Operations on a shared database", "The Operation log",
//! "Writing to a shared database, and the role as the boundary").

use std::sync::Barrier;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{Api, assert_recent_rfc3339, version_body_declaring};
use crate::http::{REQUEST_TIMEOUT, Response, User};
use crate::shared_database::{Login, Session, SharedDatabase};
use crate::vm::{INSTALLER, Vm, VmOptions, fixture_bundle};

/// The Group named with `--administrators-group`.
pub const ADMINISTRATORS: &str = "platform-admins";

/// The Resource most tests register.
pub const RESOURCE: &str = "devops";

/// The Group most tests grant `read`.
pub const READERS: &str = "readers";

/// The Group most write tests grant `write`.
pub const WRITERS: &str = "writers";

/// Whom to ask for access to the Resource: a `no-grant` refusal names it.
pub const CONTACT: &str = "Dana Ops, #devops-access on chat";

/// A Platform administrator: a User in [`ADMINISTRATORS`].
pub fn administrator() -> User {
    User::new("it-admin").in_groups(&[ADMINISTRATORS])
}

/// The body that registers a Resource of this name over `database` with
/// `login` as its read login.
pub fn resource_body(name: &str, database: &SharedDatabase, login: &Login) -> Value {
    json!({
        "name": name,
        "kind": "postgres",
        "description": format!("The {name} team's incident database"),
        "owner": "The DevOps team",
        "contact": CONTACT,
        "connection": database.connection(login),
    })
}

/// The body that registers a Resource of this name over `database` with
/// these `read` and `write` logins.
pub fn resource_body_with_write(
    name: &str,
    database: &SharedDatabase,
    read: &Login,
    write: &Login,
) -> Value {
    let mut body = resource_body(name, database, read);
    body["connection"] = database.connection_with_write(read, write);
    body
}

/// One call for `fixtures/operations.js`: this operation with these
/// arguments.
pub fn op(operation: &str, arguments: &Value) -> Value {
    json!({ "operation": operation, "arguments": arguments })
}

/// `{ resource, sql }`.
pub fn sql_on(resource: &str, sql: &str) -> Value {
    json!({ "resource": resource, "sql": sql })
}

/// A call of `execute` on [`RESOURCE`] with this statement.
pub fn execute_sql(sql: &str) -> Value {
    op("execute", &sql_on(RESOURCE, sql))
}

/// A call of `query` on [`RESOURCE`] with this statement.
pub fn query_sql(sql: &str) -> Value {
    op("query", &sql_on(RESOURCE, sql))
}

/// A program started with the stand-in sign-in and [`ADMINISTRATORS`] as
/// its administrators' Group, and a shared database of the test's own.
pub struct Scenario {
    pub vm: Vm,
    pub db: SharedDatabase,
}

impl Scenario {
    /// The program and the shared database; nothing registered, nothing
    /// installed.
    pub fn start() -> Self {
        Self::start_with(&VmOptions::new())
    }

    /// As [`Scenario::start`], from these options (a platform database of
    /// the test's choosing, say), with the stand-in on and
    /// [`ADMINISTRATORS`].
    pub fn start_with(options: &VmOptions) -> Self {
        let options = options
            .clone()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS);
        Self {
            db: SharedDatabase::create(),
            vm: Vm::start(&options),
        }
    }

    /// The usual setting: [`RESOURCE`] registered with the read login,
    /// [`READERS`] granted `read` on it, and the Application `board`
    /// (`fixtures/query.js`) declaring it at `read`.
    pub fn ready() -> Self {
        let scenario = Self::start();
        scenario.register(RESOURCE);
        scenario.grant(RESOURCE, READERS, "read");
        scenario.install("board", "query.js", &[(RESOURCE, "read")]);
        scenario
    }

    /// The setting of the write tests: [`RESOURCE`] registered with the
    /// read and the write login, [`READERS`] granted `read` and [`WRITERS`]
    /// granted `write` on it, the Application `board`
    /// (`fixtures/operations.js`) declaring it at `write`, and `viewer`
    /// (the same fixture) declaring it at `read`.
    pub fn writable() -> Self {
        let scenario = Self::start();
        scenario.register_writable(RESOURCE);
        scenario.grant(RESOURCE, READERS, "read");
        scenario.grant(RESOURCE, WRITERS, "write");
        scenario.install("board", "operations.js", &[(RESOURCE, "write")]);
        scenario.install("viewer", "operations.js", &[(RESOURCE, "read")]);
        scenario
    }

    /// Registers `name` over the shared database with its read and write
    /// logins, failing the test unless it is `201`.
    pub fn register_writable(&self, name: &str) -> Value {
        self.register_body(
            name,
            &resource_body_with_write(name, &self.db, &self.db.reader, &self.db.writer),
        )
    }

    /// Registers this body, failing the test unless it is `201`.
    pub fn register_body(&self, name: &str, body: &Value) -> Value {
        let answer = self.admin().create_resource(body);
        if answer.status != 201 {
            self.vm.fail(&format!(
                "expected `201` registering the Resource {name:?} as a Platform administrator, got\n{}",
                answer.describe()
            ));
        }
        answer.json()
    }

    /// One `execute` with these arguments through `fixtures/operations.js`;
    /// returns the request's answer and what the host answered.
    pub fn execute(&self, user: &User, application: &str, args: &Value) -> (Response, Value) {
        self.one(user, application, &op("execute", args))
    }

    /// One call through `fixtures/operations.js` (see [`op`]); returns the
    /// request's answer and what the host answered.
    pub fn one(&self, user: &User, application: &str, call: &Value) -> (Response, Value) {
        let (response, answers) = self.run(user, application, &json!({ "calls": [call] }));
        let answer = answers.into_iter().next().unwrap_or(Value::Null);
        (response, answer)
    }

    /// Requests sent at once from as many threads, each `(user,
    /// application, body)` as for [`Scenario::run`]; returns each one's
    /// answer and the answers of its calls, in the order given.
    pub fn at_once(&self, requests: &[(User, &str, Value)]) -> Vec<(Response, Vec<Value>)> {
        self.at_once_within(requests, REQUEST_TIMEOUT)
    }

    /// As [`Scenario::at_once`], waiting up to `timeout` for each answer.
    pub fn at_once_within(
        &self,
        requests: &[(User, &str, Value)],
        timeout: Duration,
    ) -> Vec<(Response, Vec<Value>)> {
        let start = Barrier::new(requests.len());
        thread::scope(|scope| {
            let handles: Vec<_> = requests
                .iter()
                .map(|(user, application, body)| {
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        self.run_within(user, application, body, timeout)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| panic!("a request thread failed"))
                })
                .collect()
        })
    }

    /// Makes `per_request` calls of `call` together in each of
    /// `requests.len()` requests sent at once, `(user, application)` each:
    /// enough operations at the same time to take every connection a pool
    /// of that many or fewer holds. Returns every answer, failing the test
    /// unless each request was answered `200`.
    pub fn spread(
        &self,
        requests: &[(User, &str)],
        call: &Value,
        per_request: usize,
    ) -> Vec<Value> {
        let bodies: Vec<(User, &str, Value)> = requests
            .iter()
            .map(|(user, application)| {
                (
                    user.clone(),
                    *application,
                    json!({ "calls": vec![call; per_request], "mode": "together" }),
                )
            })
            .collect();
        self.at_once(&bodies)
            .into_iter()
            .flat_map(|(_, answers)| answers)
            .collect()
    }

    /// As [`Scenario::spread`] with a call that observes its session
    /// ([`crate::shared_database::observe_select`] or `observe_insert`):
    /// what each session said, failing the test unless every call answered
    /// one.
    pub fn observe_spread(
        &self,
        requests: &[(User, &str)],
        call: &Value,
        per_request: usize,
    ) -> Vec<Session> {
        self.spread(requests, call, per_request)
            .iter()
            .map(|answer| {
                Session::from_answer(answer).unwrap_or_else(|| {
                    self.vm.fail(&format!(
                        "expected an observation of the session, {{ ok: {{ rows: [[<json>]] }} }}, got {answer}"
                    ))
                })
            })
            .collect()
    }

    /// The platform's API as the Platform administrator.
    pub fn admin(&self) -> Api<'_> {
        self.vm.api_as(&administrator())
    }

    /// Registers `name` over the shared database with its read login,
    /// failing the test unless it is `201`.
    pub fn register(&self, name: &str) -> Value {
        self.register_with(name, &self.db.reader)
    }

    /// Registers `name` with this login as its read login.
    pub fn register_with(&self, name: &str, login: &Login) -> Value {
        let answer = self
            .admin()
            .create_resource(&resource_body(name, &self.db, login));
        if answer.status != 201 {
            self.vm.fail(&format!(
                "expected `201` registering the Resource {name:?} as a Platform administrator, got\n{}",
                answer.describe()
            ));
        }
        answer.json()
    }

    /// Gives `group` a Grant at `access` on `resource`, failing the test
    /// unless it is `200`.
    pub fn grant(&self, resource: &str, group: &str, access: &str) {
        let answer = self.admin().grant(resource, group, access);
        if answer.status != 200 {
            self.vm.fail(&format!(
                "expected `200` granting {group:?} {access:?} on {resource:?} as a Platform administrator, got\n{}",
                answer.describe()
            ));
        }
    }

    /// Creates the Application `name` as [`INSTALLER`] and sends the fixture
    /// `file` as its first Version, declaring these Resources as (name,
    /// access level).
    pub fn install(&self, name: &str, file: &str, declared: &[(&str, &str)]) {
        let api = self.vm.api_as(&User::new(INSTALLER));
        let created = api.create_application(name);
        if created.status != 201 {
            self.vm.fail(&format!(
                "expected `201` creating the Application {name:?}, got\n{}",
                created.describe()
            ));
        }
        let sent = api.send_version(
            name,
            &version_body_declaring(&fixture_bundle(file), declared),
        );
        if sent.status != 201 {
            self.vm.fail(&format!(
                "expected `201` sending {file} as a Version of {name:?} declaring {declared:?}, got\n{}",
                sent.describe()
            ));
        }
    }

    /// `POST <path>` on the Application as this User, with this JSON body;
    /// fails unless it is `200` with a JSON body, and returns both.
    pub fn post(
        &self,
        user: &User,
        application: &str,
        path: &str,
        body: &Value,
    ) -> (Response, Value) {
        self.post_within(user, application, path, body, REQUEST_TIMEOUT)
    }

    /// As [`Scenario::post`], waiting up to `timeout` for the answer (for
    /// statements ended at the time limit).
    pub fn post_within(
        &self,
        user: &User,
        application: &str,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> (Response, Value) {
        let response = self
            .vm
            .request(application, path)
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(timeout)
            .send();
        if response.status != 200 {
            self.vm.fail(&format!(
                "expected `200` from {application:?} for POST {path} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        (response, json)
    }

    /// Runs these calls through `fixtures/query.js` (see its header for
    /// `mode` and the rest) and returns the request's answer and the
    /// answers of the calls.
    pub fn run(&self, user: &User, application: &str, body: &Value) -> (Response, Vec<Value>) {
        self.run_within(user, application, body, REQUEST_TIMEOUT)
    }

    /// As [`Scenario::run`], waiting up to `timeout` for the answer.
    pub fn run_within(
        &self,
        user: &User,
        application: &str,
        body: &Value,
        timeout: Duration,
    ) -> (Response, Vec<Value>) {
        let (response, json) = self.post_within(user, application, "/", body, timeout);
        let answers = json["answers"].as_array().cloned().unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected {{ answers: [...] }} from {application:?}, got {json}"
            ))
        });
        (response, answers)
    }

    /// One `query` with these arguments through `fixtures/query.js`;
    /// returns the request's answer and what the host answered.
    pub fn query(&self, user: &User, application: &str, args: &Value) -> (Response, Value) {
        let (response, answers) = self.run(user, application, &json!({ "calls": [args] }));
        let answer = answers.into_iter().next().unwrap_or(Value::Null);
        (response, answer)
    }

    /// Every entry of the Operation log, read by the administrator.
    pub fn log(&self) -> Vec<Value> {
        self.admin().operation_log_entries()
    }

    /// The entries of the Operation log for the request this answers.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        let id = response.request_id();
        self.log()
            .into_iter()
            .filter(|e| e["request"].as_str() == Some(id.as_str()))
            .collect()
    }

    /// The one entry of the request this answers, which must have exactly
    /// one.
    pub fn entry_of(&self, response: &Response) -> Value {
        let entries = self.entries_of(response);
        match entries.as_slice() {
            [entry] => entry.clone(),
            _ => self.vm.fail(&format!(
                "expected exactly one Operation log entry for request {}, got {}:\n{}",
                response.request_id(),
                entries.len(),
                lines(&entries)
            )),
        }
    }
}

/// Values one per line, for failure messages.
pub fn lines(values: &[Value]) -> String {
    let mut text = String::new();
    for value in values {
        text.push_str("  ");
        text.push_str(&value.to_string());
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("  (none)\n");
    }
    text
}

/// Fails unless `answer` is `{ ok: { columns, rows, row_count } }` with
/// these columns and rows, `row_count` being the number of rows.
pub fn assert_rows(answer: &Value, columns: &[&str], rows: &Value, context: &str) {
    let ok = &answer["ok"];
    assert!(
        ok.is_object(),
        "{context}: expected the query to run and answer {{ ok: {{ columns, rows, row_count }} }}, got {answer}"
    );
    assert_eq!(
        ok["columns"],
        json!(columns),
        "{context}: expected the columns {columns:?}, got {answer}"
    );
    assert_eq!(
        &ok["rows"], rows,
        "{context}: expected the rows {rows}, got {answer}"
    );
    let count = rows.as_array().map_or(0, Vec::len);
    assert_eq!(
        ok["row_count"].as_u64(),
        u64::try_from(count).ok(),
        "{context}: expected row_count {count}, got {answer}"
    );
}

/// Fails unless `answer` is a refusal with this reason, carrying exactly
/// these other fields in `refusal` (`fields` is an object; its keys and
/// values must all be there, and no other key but `reason`), and a `fix`
/// that is a non-empty sentence.
pub fn assert_refusal(answer: &Value, reason: &str, fields: &Value, context: &str) {
    let refusal = &answer["refused"]["refusal"];
    assert_eq!(
        refusal["reason"].as_str(),
        Some(reason),
        "{context}: expected the host to refuse with reason {reason:?}, got {answer}"
    );
    let mut expected = fields.clone();
    expected["reason"] = json!(reason);
    assert_eq!(
        refusal, &expected,
        "{context}: expected the refusal to be exactly {expected}, got {answer}"
    );
    let fix = answer["refused"]["fix"].as_str().unwrap_or_default();
    assert!(
        fix.trim().len() >= 10,
        "{context}: expected the refusal to carry a `fix`, a sentence a Coding agent can act on, got {answer}"
    );
    assert!(
        answer.get("ok").is_none() && answer.get("error").is_none(),
        "{context}: a refusal must carry neither `ok` nor `error`, got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "database", code, message } }`
/// with this SQLSTATE and a message.
pub fn assert_database_error(answer: &Value, code: &str, context: &str) {
    let error = &answer["error"];
    assert_eq!(
        error["kind"].as_str(),
        Some("database"),
        "{context}: expected {{ error: {{ kind: \"database\", ... }} }}, got {answer}"
    );
    assert_eq!(
        error["code"].as_str(),
        Some(code),
        "{context}: expected the SQLSTATE {code}, got {answer}"
    );
    assert!(
        error["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{context}: expected Postgres's message, got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "limit", limit } }`.
pub fn assert_limit(answer: &Value, limit: &str, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("limit"),
        "{context}: expected the statement to be ended at a limit, {{ error: {{ kind: \"limit\", limit: {limit:?} }} }}, got {answer}"
    );
    assert_eq!(
        answer["error"]["limit"].as_str(),
        Some(limit),
        "{context}: expected the limit {limit:?}, got {answer}"
    );
}

/// Fails unless `answer` is `{ ok: { columns, rows, row_count,
/// rows_affected } }` with these columns, rows and rows affected,
/// `row_count` being the number of rows.
pub fn assert_executed(
    answer: &Value,
    columns: &[&str],
    rows: &Value,
    rows_affected: u64,
    context: &str,
) {
    assert!(
        answer["ok"].is_object(),
        "{context}: expected the statement to run and answer {{ ok: {{ columns, rows, row_count, rows_affected }} }}, got {answer}"
    );
    assert_rows(answer, columns, rows, context);
    assert_eq!(
        answer["ok"]["rows_affected"].as_u64(),
        Some(rows_affected),
        "{context}: expected rows_affected {rows_affected}, got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: <kind>, ... } }`.
pub fn assert_error_kind(answer: &Value, kind: &str, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some(kind),
        "{context}: expected {{ error: {{ kind: {kind:?} }} }}, got {answer}"
    );
    assert!(
        answer.get("ok").is_none() && answer.get("refused").is_none(),
        "{context}: expected only an error, got {answer}"
    );
}

/// Fails unless `answer` is the statement check's refusal, `{ error: {
/// kind: "statement", reason, message } }`, with this reason.
pub fn assert_statement_refused(answer: &Value, reason: &str, context: &str) {
    assert_error_kind(answer, "statement", context);
    assert_eq!(
        answer["error"]["reason"].as_str(),
        Some(reason),
        "{context}: expected the statement check to refuse with reason {reason:?}, got {answer}"
    );
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{context}: expected a message saying what the check refused, got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "malformed", message } }`.
pub fn assert_malformed(answer: &Value, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("malformed"),
        "{context}: expected {{ error: {{ kind: \"malformed\", message }} }}, got {answer}"
    );
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{context}: expected a message saying what is malformed, got {answer}"
    );
}

/// What an Operation log entry must say. `None` for `reason`, `error`,
/// `rows` and `duration` means the field is null or absent.
#[derive(Clone, Debug)]
pub struct Expected<'a> {
    pub request: Option<&'a str>,
    pub user: Option<&'a str>,
    pub application: &'a str,
    pub version: u64,
    pub resource: &'a str,
    /// `query` or `execute`.
    pub operation: &'a str,
    pub statement: Option<&'a str>,
    pub outcome: &'a str,
    pub reason: Option<&'a str>,
    pub error: Option<&'a str>,
    /// Whether the statement ran: then `duration_ms` is a number (and
    /// `rows` a number or null); if not, both are null. `None`: not checked
    /// (a statement that failed: CONTRACT.md does not say whether it ran).
    pub ran: Option<bool>,
    pub rows: Option<u64>,
}

impl<'a> Expected<'a> {
    /// An allowed query of `user` in `application` (Version 1) on
    /// [`RESOURCE`], which ran.
    pub fn allowed(request: &'a str, user: &'a str, application: &'a str) -> Self {
        Self {
            request: Some(request),
            user: Some(user),
            application,
            version: 1,
            resource: RESOURCE,
            operation: "query",
            statement: None,
            outcome: "allowed",
            reason: None,
            error: None,
            ran: Some(true),
            rows: None,
        }
    }

    /// A refused query, which did not run.
    pub fn refused(request: &'a str, user: &'a str, application: &'a str, reason: &'a str) -> Self {
        Self {
            outcome: "refused",
            reason: Some(reason),
            ran: Some(false),
            ..Self::allowed(request, user, application)
        }
    }

    /// An allowed `execute` of `user` in `application` (Version 1) on
    /// [`RESOURCE`], which ran and affected `rows` rows.
    pub fn executed(request: &'a str, user: &'a str, application: &'a str, rows: u64) -> Self {
        Self {
            operation: "execute",
            rows: Some(rows),
            ..Self::allowed(request, user, application)
        }
    }

    /// An operation the statement check refused: `failed`, error
    /// `statement`, never sent.
    pub fn check_refused(
        request: &'a str,
        user: &'a str,
        application: &'a str,
        operation: &'a str,
    ) -> Self {
        Self {
            operation,
            ran: Some(false),
            ..Self::failed(request, user, application, "statement")
        }
    }

    /// A query that failed in the database or at a limit.
    pub fn failed(request: &'a str, user: &'a str, application: &'a str, error: &'a str) -> Self {
        Self {
            outcome: "failed",
            error: Some(error),
            ran: None,
            ..Self::allowed(request, user, application)
        }
    }
}

fn absent_or_null(entry: &Value, key: &str) -> bool {
    entry.get(key).is_none_or(Value::is_null)
}

/// Fails unless `entry` is an Operation log entry saying what `want` says,
/// with every field of the contract.
pub fn assert_entry(entry: &Value, want: &Expected<'_>, context: &str) {
    let fail =
        |what: &str| -> ! { panic!("{context}: {what}\nexpected {want:?}\ngot the entry {entry}") };
    if !entry["id"].is_u64() {
        fail("expected a numeric `id`");
    }
    assert_recent_rfc3339(
        &entry["time"],
        120,
        &format!("{context}, the entry's `time`"),
    );
    if entry["request"].as_str() != want.request {
        fail("expected `request` to be the request's id (or null with no request)");
    }
    if entry["user"].as_str() != want.user {
        fail("expected `user` to be the User's id (or null with no request)");
    }
    if entry["application"].as_str() != Some(want.application) {
        fail("expected `application` to name the Application");
    }
    if entry["version"].as_u64() != Some(want.version) {
        fail("expected `version` to be the Version's number");
    }
    if entry["resource"].as_str() != Some(want.resource) {
        fail("expected `resource` to name the Resource asked for");
    }
    if entry["operation"].as_str() != Some(want.operation) {
        fail(&format!("expected `operation` {:?}", want.operation));
    }
    match want.statement {
        Some(statement) if entry["statement"].as_str() != Some(statement) => {
            fail(&format!("expected `statement` {statement:?}"));
        }
        _ if !entry["statement"].is_string() => fail("expected `statement` as a string"),
        _ => {}
    }
    if entry["outcome"].as_str() != Some(want.outcome) {
        fail(&format!("expected `outcome` {:?}", want.outcome));
    }
    match want.reason {
        Some(reason) if entry["reason"].as_str() != Some(reason) => {
            fail(&format!("expected `reason` {reason:?}"));
        }
        None if !absent_or_null(entry, "reason") => fail("expected no `reason` unless refused"),
        _ => {}
    }
    match want.error {
        Some(error) if entry["error"].as_str() != Some(error) => {
            fail(&format!("expected `error` {error:?}"));
        }
        None if !absent_or_null(entry, "error") => fail("expected no `error` unless failed"),
        _ => {}
    }
    if want.ran == Some(true) {
        if !entry["duration_ms"].is_u64() {
            fail(
                "expected `duration_ms`, a whole number of milliseconds, for a statement that ran",
            );
        }
        match want.rows {
            Some(rows) if entry["rows"].as_u64() != Some(rows) => {
                fail(&format!("expected `rows` {rows}"))
            }
            None if !(entry["rows"].is_u64() || entry["rows"].is_null()) => {
                fail("expected `rows` to be a number or null");
            }
            _ => {}
        }
    } else if want.ran == Some(false)
        && (!entry["rows"].is_null() || !entry["duration_ms"].is_null())
    {
        fail("expected `rows` and `duration_ms` null for a statement that did not run");
    }
}

/// Fails if any of these texts holds any of `markers`, saying which text
/// holds which.
pub fn assert_none_holds(texts: &[(String, String)], markers: &[(&str, String)], context: &str) {
    let mut found = Vec::new();
    for (whence, text) in texts {
        for (what, marker) in markers {
            if text.contains(marker.as_str()) {
                found.push(format!("  {what} ({marker:?}) in {whence}: {text}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "{context}: expected none of these values anywhere, but:\n{}",
        found.join("\n")
    );
}
