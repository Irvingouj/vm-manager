//! What the tests of "Transactions through the SDK" share at the raw
//! interface (CONTRACT.md, "Transactions"):
//!
//! - [`Txs`], a program with a shared database registered as `devops`
//!   (read and write logins) and as [`RO`] (a read login only), Grants on
//!   both, and `fixtures/transactions.js` installed as [`APP`] (declaring
//!   `devops` and [`RO`] at `write`, and [`GHOST`], which is never
//!   registered) and as [`VIEWER`] (declaring `devops` at `read`), each with
//!   an own database made by [`migrations`];
//! - the steps the fixture runs ([`begin`], [`query_in`], [`commit`], ...),
//!   where `{ "$ref": <name> }` stands for a transaction id the fixture
//!   kept;
//! - checks of the answers (`{ ok: { tx } }`, `{ ok: {} }`,
//!   `no-transaction`) and of the Operation log entries of `begin`,
//!   `commit` and `rollback`;
//! - the sessions Postgres itself shows in `pg_stat_activity`, read from the
//!   harness's superuser connection: which of a login's sessions are still
//!   in a transaction.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::history::history_standard;
use crate::http::{Response, User};
use crate::own_database::{Migration, OwnDatabase, literal, version_body_with_migrations};
use crate::scenario::{READERS, RESOURCE, Scenario, WRITERS, lines};
use crate::shared_database::SharedDatabase;
use crate::sql::Sql;
use crate::vm::{INSTALLER, Vm, fixture_bundle};

/// The fixture every raw transaction test runs.
pub const FIXTURE: &str = "transactions.js";

/// The Application most tests use: `devops` and [`RO`] declared at
/// `write`, [`GHOST`] declared and never registered.
pub const APP: &str = "board";

/// The same fixture, declaring `devops` at `read`.
pub const VIEWER: &str = "viewer";

/// A Resource over the same shared database, registered with its read
/// login only.
pub const RO: &str = "ro";

/// A Resource [`APP`] declares that is never registered.
pub const GHOST: &str = "ghost";

/// How long a request of these tests may take: longer than the 10 s cap,
/// so that a test waits for what the host does on its own.
pub const TX_REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

/// The cap on how long a transaction may be open (CONTRACT.md, "What the
/// host does on its own").
pub const CAP: Duration = Duration::from_secs(10);

/// A User whose Groups hold `write` on `devops` and [`RO`], and `read` on
/// `devops`.
pub fn wanda() -> User {
    User::new("wanda").in_groups(&[WRITERS, READERS])
}

/// A User whose Groups hold `read` on `devops` only.
pub fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A User whose Groups hold no Grant.
pub fn nobody() -> User {
    User::new("nobody").in_groups(&["no-access"])
}

/// `0003_stash`: a table where a request leaves a value for the harness
/// (or another request) to read: `stash (k text PK, v text)`.
pub fn stash() -> Migration {
    Migration::new(
        "0003_stash",
        &[r#"CREATE TABLE "stash" (
	"k" text PRIMARY KEY NOT NULL,
	"v" text NOT NULL
);"#],
    )
}

/// The migrations of both Applications: [`history_standard`] (`things`,
/// `parents`, `counters`, ..., `run_sql`, `leave_session_state`,
/// `identity_after`) and [`stash`].
pub fn migrations() -> Vec<Migration> {
    let mut all = history_standard();
    all.push(stash());
    all
}

/// Where a transaction is begun: a shared database by its Resource name,
/// or the own database as an actor.
#[derive(Clone, Copy, Debug)]
pub enum Target<'a> {
    Shared(&'a str),
    Own(&'a str),
}

impl Target<'_> {
    /// `{ resource }` or `{ own: true, as }`.
    pub fn args(&self) -> Value {
        match self {
            Target::Shared(resource) => json!({ "resource": resource }),
            Target::Own(actor) => json!({ "own": true, "as": actor }),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Target::Shared(resource) => format!("the shared database {resource:?}"),
            Target::Own(actor) => format!("the own database as the {actor}"),
        }
    }
}

/// The targets a transaction can have in [`APP`] that most tests go
/// through: `devops`, the own database as the User, and as the
/// Application.
pub const TARGETS: [Target<'static>; 3] = [
    Target::Shared(RESOURCE),
    Target::Own("user"),
    Target::Own("application"),
];

/// `{ "$ref": name }`: the transaction id the fixture kept under `name`.
pub fn tx(name: &str) -> Value {
    json!({ "$ref": name })
}

/// A step: `operation` with these arguments.
pub fn step(operation: &str, args: Value) -> Value {
    let mut step = serde_json::Map::new();
    step.insert("op".to_owned(), json!(operation));
    step.insert("args".to_owned(), args);
    Value::Object(step)
}

/// `tx.begin` on `target` at `access`, keeping the id under `name`.
pub fn begin(target: Target<'_>, access: &str, name: &str) -> Value {
    let mut args = target.args();
    args["access"] = json!(access);
    begin_with(args, name)
}

/// `tx.begin` with these arguments, keeping the id under `name`.
pub fn begin_with(args: Value, name: &str) -> Value {
    let mut step = step("tx.begin", args);
    step["save"] = json!(name);
    step
}

/// `query` in the transaction kept under `name`.
pub fn query_in(name: &str, sql: &str) -> Value {
    step("query", json!({ "tx": tx(name), "sql": sql }))
}

/// `execute` in the transaction kept under `name`.
pub fn execute_in(name: &str, sql: &str) -> Value {
    step("execute", json!({ "tx": tx(name), "sql": sql }))
}

/// `operation` in the transaction kept under `name`, with parameters.
pub fn in_tx_with(operation: &str, name: &str, sql: &str, params: &Value) -> Value {
    step(
        operation,
        json!({ "tx": tx(name), "sql": sql, "params": params }),
    )
}

/// `operation` outside any transaction, on `target`.
pub fn outside(operation: &str, target: Target<'_>, sql: &str) -> Value {
    let mut args = target.args();
    args["sql"] = json!(sql);
    step(operation, args)
}

/// `tx.commit` of the transaction kept under `name`.
pub fn commit(name: &str) -> Value {
    step("tx.commit", json!({ "tx": tx(name) }))
}

/// `tx.rollback` of the transaction kept under `name`.
pub fn rollback(name: &str) -> Value {
    step("tx.rollback", json!({ "tx": tx(name) }))
}

/// A wait of `ms` in the fixture, with no statement running.
pub fn sleep(ms: u64) -> Value {
    json!({ "sleep": ms })
}

/// An uncaught Error thrown by the fixture: the request fails.
pub fn throw(message: &str) -> Value {
    json!({ "throw": message })
}

/// Steps made together, none awaited before all are made.
pub fn together(steps: &[Value]) -> Value {
    json!({ "together": steps })
}

/// A step that leaves `steps` to run `delay_ms` after the request ended.
pub fn later(delay_ms: u64, steps: &[Value]) -> Value {
    json!({ "later": { "delayMs": delay_ms, "steps": steps } })
}

/// An `execute` outside any transaction that writes `value` under `key`
/// into the own database's `stash`, as the Application: how a request
/// leaves a transaction id where the harness can read it.
pub fn stash_value(key: &str, value: &Value) -> Value {
    step(
        "execute",
        json!({
            "own": true, "as": "application",
            "sql": "insert into stash (k, v) values ($1, $2)",
            "params": [key, value],
        }),
    )
}

/// A statement that holds its connection `seconds` in the database, with
/// `marker` in its text.
pub fn slow_select(seconds: f64, marker: &str) -> String {
    format!("select '{marker}' as marker, pg_backend_pid() as pid from pg_sleep({seconds})")
}

/// The program with the Resources, Grants and Applications of the
/// transaction tests (see the module's header).
pub struct Txs {
    pub scenario: Scenario,
    /// [`APP`]'s own database.
    pub own: OwnDatabase,
}

impl Txs {
    pub fn start() -> Self {
        let scenario = Scenario::start();
        scenario.register_writable(RESOURCE);
        scenario.register(RO);
        scenario.grant(RESOURCE, READERS, "read");
        scenario.grant(RESOURCE, WRITERS, "write");
        scenario.grant(RO, WRITERS, "write");
        install(
            &scenario.vm,
            APP,
            &[(RESOURCE, "write"), (RO, "write"), (GHOST, "write")],
        );
        install(&scenario.vm, VIEWER, &[(RESOURCE, "read")]);
        let own = OwnDatabase::find(scenario.vm.platform(), APP).unwrap_or_else(|| {
            scenario
                .vm
                .fail(&format!("expected an own database for {APP:?}"))
        });
        Self { scenario, own }
    }

    pub fn vm(&self) -> &Vm {
        &self.scenario.vm
    }

    /// The shared database behind `devops` and [`RO`].
    pub fn db(&self) -> &SharedDatabase {
        &self.scenario.db
    }

    /// Runs these steps in `application` as `user`; fails unless `200`.
    /// Returns the request's answer and each step's answer.
    pub fn run(&self, user: &User, application: &str, steps: &[Value]) -> (Response, Vec<Value>) {
        let (response, body) = self.run_body(user, application, steps);
        let answers = body["answers"].as_array().cloned().unwrap_or_default();
        (response, answers)
    }

    /// As [`Txs::run`], also returning the whole body (`kept` among it).
    pub fn run_body(&self, user: &User, application: &str, steps: &[Value]) -> (Response, Value) {
        let response = self.send(user, application, steps);
        if response.status != 200 {
            self.vm().fail(&format!(
                "expected `200` with {{ answers }} from {application:?} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let body = response.json();
        if !body["answers"].is_array() {
            self.vm().fail(&format!(
                "expected {{ answers: [...] }} from {application:?}, got {body}"
            ));
        }
        (response, body)
    }

    /// Sends these steps, whatever the answer.
    pub fn send(&self, user: &User, application: &str, steps: &[Value]) -> Response {
        self.vm()
            .request(application, "/")
            .as_user(user)
            .method("POST")
            .json(&json!({ "steps": steps }))
            .timeout(TX_REQUEST_TIMEOUT)
            .send()
    }

    /// Every entry of the Operation log.
    pub fn log(&self) -> Vec<Value> {
        self.scenario.log()
    }

    /// The Operation log entries of the request this answers, oldest first.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        self.scenario.entries_of(response)
    }

    /// The Operation log entries of the request this answers, waiting up
    /// to 5 s until there are at least `at_least` of them (a rollback the
    /// host makes when the request ends may be written just after the
    /// answer).
    pub fn entries_eventually(&self, response: &Response, at_least: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let entries = self.entries_of(response);
            if entries.len() >= at_least || Instant::now() >= deadline {
                return entries;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The value the own database's `stash` holds under `key`, waiting up
    /// to `timeout` for it.
    pub fn stashed(&self, key: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let sql = self.own.superuser();
        loop {
            let found = sql.value(&format!("SELECT v FROM stash WHERE k = {}", literal(key)));
            if found.is_some() || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// How many rows of `notes` in the shared database hold `marker` in
    /// their body.
    pub fn shared_notes_with(&self, marker: &str) -> usize {
        count(
            &self.db().superuser(),
            &format!(
                "SELECT count(*) FROM notes WHERE body LIKE {}",
                literal(&format!("%{marker}%"))
            ),
        )
    }

    /// How many rows of `things` in [`APP`]'s own database hold `marker` in
    /// their title.
    pub fn own_things_with(&self, marker: &str) -> usize {
        count(
            &self.own.superuser(),
            &format!(
                "SELECT count(*) FROM things WHERE title LIKE {}",
                literal(&format!("%{marker}%"))
            ),
        )
    }

    /// How many rows hold `marker` where a write on `target` puts them
    /// (see [`insert_marker`]).
    pub fn rows_with(&self, target: Target<'_>, marker: &str) -> usize {
        match target {
            Target::Shared(_) => self.shared_notes_with(marker),
            Target::Own(_) => self.own_things_with(marker),
        }
    }

    /// The harness's superuser connected to the database of `target`.
    pub fn superuser_of(&self, target: Target<'_>) -> Sql {
        match target {
            Target::Shared(_) => self.db().superuser(),
            Target::Own(_) => self.own.superuser(),
        }
    }

    /// The database name and the logins a transaction on `target` runs as.
    pub fn logins_of(&self, target: Target<'_>) -> (String, Vec<String>) {
        match target {
            Target::Shared(_) => (
                self.db().name().to_owned(),
                vec![self.db().reader.role.clone(), self.db().writer.role.clone()],
            ),
            Target::Own(_) => (self.own.name.clone(), vec![self.own.runtime.clone()]),
        }
    }

    /// The sessions of `target`'s logins that Postgres shows in a
    /// transaction (any state but `idle`), waiting up to `timeout` for
    /// there to be none; returns what is left, empty when none.
    pub fn sessions_in_transaction(&self, target: Target<'_>, timeout: Duration) -> Vec<String> {
        let (database, roles) = self.logins_of(target);
        let sql = self.superuser_of(target);
        wait_for_none(timeout, || in_transaction(&sql, &database, &roles))
    }
}

/// Creates `name` as [`INSTALLER`] and sends [`FIXTURE`] as its first
/// Version, declaring these Resources, with [`migrations`].
pub fn install(vm: &Vm, name: &str, declared: &[(&str, &str)]) {
    let api = vm.api_as(&User::new(INSTALLER));
    let created = api.create_application(name);
    if created.status != 201 {
        vm.fail(&format!(
            "expected `201` creating the Application {name:?}, got\n{}",
            created.describe()
        ));
    }
    let sent = api
        .request(&format!("/v1/applications/{name}/versions"))
        .method("POST")
        .json(&version_body_with_migrations(
            &fixture_bundle(FIXTURE),
            declared,
            &migrations(),
        ))
        .timeout(Duration::from_secs(60))
        .send();
    if sent.status != 201 {
        vm.fail(&format!(
            "expected `201` sending {FIXTURE} as a Version of {name:?} declaring {declared:?}, got\n{}",
            sent.describe()
        ));
    }
}

/// The statement that writes one row holding `marker` on `target`: a row
/// of `notes` on a shared database, of `things` on the own database.
pub fn insert_marker(target: Target<'_>, marker: &str) -> String {
    match target {
        Target::Shared(_) => format!("insert into notes (body) values ('{marker}') returning id"),
        Target::Own(_) => format!("insert into things (title) values ('{marker}') returning id"),
    }
}

/// The statement that reads how many rows hold `marker` on `target`, as
/// one row with one column `n`.
pub fn count_marker(target: Target<'_>, marker: &str) -> String {
    match target {
        Target::Shared(_) => {
            format!("select count(*)::int4 as n from notes where body = '{marker}'")
        }
        Target::Own(_) => {
            format!("select count(*)::int4 as n from things where title = '{marker}'")
        }
    }
}

/// The one number a [`count_marker`] answer holds.
pub fn counted(answer: &Value, context: &str) -> i64 {
    answer["ok"]["rows"][0][0].as_i64().unwrap_or_else(|| {
        panic!("{context}: expected {{ ok: {{ rows: [[<count>]] }} }}, got {answer}")
    })
}

fn count(sql: &Sql, statement: &str) -> usize {
    sql.value(statement)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("the harness's own count failed: {statement}"))
}

/// The sessions of these roles in `database` that Postgres shows in a
/// transaction: `backend_xid`/`xact_start` set, or a state `idle in
/// transaction` / `active`. Each as `pid role state query`.
pub fn in_transaction(sql: &Sql, database: &str, roles: &[String]) -> Vec<String> {
    let roles = roles
        .iter()
        .map(|r| literal(r))
        .collect::<Vec<_>>()
        .join(", ");
    sql.batch(&format!(
        "SELECT pid || ' ' || usename || ' ' || coalesce(state, '?') || ' ' || coalesce(query, '')
           FROM pg_stat_activity
          WHERE datname = {} AND usename IN ({roles}) AND pid <> pg_backend_pid()
            AND (xact_start IS NOT NULL OR state LIKE 'idle in transaction%')
          ORDER BY pid",
        literal(database)
    ))
    .into_iter()
    .filter_map(|row| row.into_iter().next().flatten())
    .collect()
}

/// Calls `look` until it gives nothing or `timeout` has passed; returns
/// its last answer.
pub fn wait_for_none(timeout: Duration, look: impl Fn() -> Vec<String>) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let found = look();
        if found.is_empty() || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fails if the server log of `db` shows `text` in anything a login other
/// than the harness's own superuser sent: the program's statements, not
/// the harness's own counts and checks (which carry the same markers).
pub fn assert_program_never_sent(db: &SharedDatabase, text: &str, context: &str) {
    let found: Vec<String> = db
        .server_log()
        .into_iter()
        .filter(|e| {
            e.contains(text)
                && crate::shared_database::login_of_entry(e).as_deref()
                    != Some(crate::postgres::SUPERUSER)
        })
        .collect();
    assert!(
        found.is_empty(),
        "{context}: expected nothing containing {text:?} sent to the shared database {} by the program, but its server log shows:\n{}",
        db.name(),
        found.join("\n")
    );
}

/// A step that makes `inner` without awaiting it: the operation may still
/// run when the request ends.
pub fn fire(inner: &Value) -> Value {
    json!({ "fire": inner })
}

/// Fails unless `answer` is `{ ok: { tx: "<id>" } }` and nothing else;
/// returns the id.
pub fn assert_began(answer: &Value, context: &str) -> String {
    let id = answer["ok"]["tx"].as_str().unwrap_or_else(|| {
        panic!("{context}: expected tx.begin to answer {{ ok: {{ tx: \"<id>\" }} }}, got {answer}")
    });
    assert!(
        !id.is_empty(),
        "{context}: expected a transaction id, got {answer}"
    );
    assert_eq!(
        answer["ok"].as_object().map(serde_json::Map::len),
        Some(1),
        "{context}: expected {{ ok: {{ tx }} }} and nothing else, got {answer}"
    );
    id.to_owned()
}

/// Fails unless `answer` is exactly `{ ok: {} }`.
pub fn assert_ended(answer: &Value, context: &str) {
    assert_eq!(
        answer,
        &json!({ "ok": {} }),
        "{context}: expected tx.commit / tx.rollback to answer {{ ok: {{}} }}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "no-transaction", message } }`.
pub fn assert_no_transaction(answer: &Value, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("no-transaction"),
        "{context}: expected {{ error: {{ kind: \"no-transaction\", message }} }}, got {answer}"
    );
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{context}: expected a message with no-transaction, got {answer}"
    );
    assert!(
        answer.get("ok").is_none() && answer.get("refused").is_none(),
        "{context}: expected only the error, got {answer}"
    );
}

/// What the Operation log entry of a `begin`, `commit` or `rollback` must
/// say beyond the usual fields.
#[derive(Clone, Copy, Debug)]
pub struct TxEntry<'a> {
    /// `begin`, `commit` or `rollback`.
    pub operation: &'a str,
    /// The transaction's id; `None`: `transaction` is null.
    pub transaction: Option<&'a str>,
    pub outcome: &'a str,
    pub reason: Option<&'a str>,
    pub error: Option<&'a str>,
    /// The Resource, `None` for the own database.
    pub resource: Option<&'a str>,
    pub actor: &'a str,
    pub request: &'a str,
    pub user: &'a str,
    pub application: &'a str,
}

/// Fails unless `entry` is the Operation log entry `want` describes:
/// `operation` and `statement` the same word, `rows` null, `transaction`
/// the id (or null), the outcome, reason and error, the target, actor,
/// request, User and Application.
pub fn assert_tx_entry(entry: &Value, want: &TxEntry<'_>, context: &str) {
    let fail =
        |what: &str| -> ! { panic!("{context}: {what}\nexpected {want:?}\ngot the entry {entry}") };
    if entry["operation"].as_str() != Some(want.operation) {
        fail(&format!("expected `operation` {:?}", want.operation));
    }
    if entry["statement"].as_str() != Some(want.operation) {
        fail(&format!(
            "expected `statement` to be the same word as the operation, {:?}",
            want.operation
        ));
    }
    if !entry["rows"].is_null() {
        fail("expected `rows` null");
    }
    match want.transaction {
        Some(id) if entry["transaction"].as_str() != Some(id) => {
            fail("expected `transaction` to be the id Application code holds");
        }
        None if !entry["transaction"].is_null() => fail("expected `transaction` null"),
        _ => {}
    }
    if entry.get("transaction").is_none() {
        fail("expected the field `transaction` on every entry");
    }
    if entry["outcome"].as_str() != Some(want.outcome) {
        fail(&format!("expected `outcome` {:?}", want.outcome));
    }
    if entry["reason"].as_str() != want.reason {
        fail(&format!("expected `reason` {:?}", want.reason));
    }
    if entry["error"].as_str() != want.error {
        fail(&format!("expected `error` {:?}", want.error));
    }
    if entry["resource"].as_str() != want.resource {
        fail(&format!("expected `resource` {:?}", want.resource));
    }
    if entry["own_database"].as_bool() != Some(want.resource.is_none()) {
        fail(&format!(
            "expected `own_database` {}",
            want.resource.is_none()
        ));
    }
    if entry["actor"].as_str() != Some(want.actor) {
        fail(&format!("expected `actor` {:?}", want.actor));
    }
    if entry["request"].as_str() != Some(want.request) {
        fail("expected `request` to be the request's id");
    }
    if entry["user"].as_str() != Some(want.user) {
        fail("expected `user` to be the User in flight");
    }
    if entry["application"].as_str() != Some(want.application) {
        fail("expected `application`");
    }
}

/// The entries among `entries` whose `operation` is one of these.
pub fn of_operations<'v>(entries: &'v [Value], operations: &[&str]) -> Vec<&'v Value> {
    entries
        .iter()
        .filter(|e| {
            e["operation"]
                .as_str()
                .is_some_and(|o| operations.contains(&o))
        })
        .collect()
}

/// Fails unless exactly one of `entries` is a `begin` with `transaction`
/// null, and it is `failed` with this `error` (CONTRACT.md: a `tx.begin`
/// that opened nothing has an entry with no id).
pub fn assert_refused_begin_entry(entries: &[Value], error: &str, context: &str) {
    let found: Vec<&Value> = entries
        .iter()
        .filter(|e| e["operation"] == "begin" && e["transaction"].is_null())
        .collect();
    let [one] = found.as_slice() else {
        panic!(
            "{context}: expected exactly one `begin` entry with `transaction` null, got {}:\n{}",
            found.len(),
            lines(entries)
        )
    };
    assert_eq!(one["outcome"], "failed", "{context}: {one}");
    assert_eq!(one["error"], error, "{context}: {one}");
    assert!(
        one.get("transaction").is_some(),
        "{context}: the field `transaction` is there, null: {one}"
    );
}

/// Fails unless exactly one of `entries` is a `rollback` of transaction
/// `id`, and it is `allowed` (CONTRACT.md: the host's own rollback is
/// `allowed`, its `error` saying why); returns it.
pub fn the_rollback<'v>(entries: &'v [Value], id: &str, context: &str) -> &'v Value {
    let found: Vec<&Value> = entries
        .iter()
        .filter(|e| e["operation"] == "rollback" && e["transaction"].as_str() == Some(id))
        .collect();
    match found.as_slice() {
        [one] => {
            assert_eq!(
                one["outcome"], "allowed",
                "{context}: the host's own rollback entry has `outcome` `allowed`: {one}"
            );
            one
        }
        _ => panic!(
            "{context}: expected exactly one `rollback` entry of the transaction {id:?} in the Operation log, got {}:\n{}",
            found.len(),
            lines(entries)
        ),
    }
}
