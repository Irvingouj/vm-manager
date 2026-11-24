//! What the tests of "Resources and Grants in full", stage two (what a
//! Platform administrator does with a Resource) share: roles of a shared
//! database made for one purpose (a login limited to some tables, a login
//! able to write by each kind of evidence, a login that holds nothing),
//! a listener that accepts and never answers, what is seen of a role on the
//! server (its sessions, the statements it sent), a Resource that was
//! registered with logins that connected and then lost what it connects to,
//! and the bodies of the requests of the stage.
//!
//! Every login made here is on the shared database's instance and has a
//! password of its own, never printed.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::scenario::{CONTACT, Scenario};
use crate::server_log::{self, Sent};
use crate::shared_database::{Login, SharedDatabase, token};
use crate::sql::Sql;

/// The description the bodies here carry.
pub const DESCRIPTION: &str = "Incidents of the DevOps team";

/// The owner the bodies here carry.
pub const OWNER: &str = "The DevOps team";

/// The statement the Postgres administrator sends for a shared database's
/// server, in the database `postgres`.
fn at_server(db: &SharedDatabase) -> Sql {
    Sql::connect(&db.server().url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the Postgres of the shared database: {e}"))
}

/// A fresh role that can log in to `db` and holds nothing else, with
/// `attributes` (`SUPERUSER`, say).
pub fn new_login(db: &SharedDatabase, prefix: &str, attributes: &str) -> Login {
    let t = token();
    let login = Login::new(
        &format!("{prefix}_{t}"),
        &format!("pw-{prefix}-{t}{}", token()),
    );
    at_server(db).batch(&format!(
        "CREATE ROLE {} LOGIN {attributes} PASSWORD '{}';
         GRANT CONNECT ON DATABASE {} TO {}",
        login.role,
        login.password(),
        db.name(),
        login.role
    ));
    login
}

/// A login that connects and holds no privilege on any table: the usual
/// structure login, and a read login that "sees no table".
pub fn login_holding_nothing(db: &SharedDatabase) -> Login {
    new_login(db, "nothing", "")
}

/// A login with `SELECT` on these tables of `public` only (and `USAGE` on
/// the schema): "a login limited to some tables".
pub fn limited_reader(db: &SharedDatabase, tables: &[&str]) -> Login {
    let login = new_login(db, "limited", "");
    db.superuser().batch(&format!(
        "GRANT USAGE ON SCHEMA public TO {role}; GRANT SELECT ON {tables} TO {role}",
        role = login.role,
        tables = tables.join(", ")
    ));
    login
}

/// A login that is a member of the shared database's reader role, so that
/// it reads what the reader reads and is a role of its own (a password to
/// change, a role to drop) beside the reader that other Resources use.
pub fn reader_of_its_own(db: &SharedDatabase) -> Login {
    let login = new_login(db, "ownreader", "");
    db.superuser().batch(&format!(
        "GRANT {} TO {}; GRANT USAGE ON SCHEMA public TO {}",
        db.reader.role, login.role, login.role
    ));
    login
}

/// A kind of evidence that a login is able to write ("Registering a
/// database, checked").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    Superuser,
    Membership,
    Table,
    Sequence,
    Schema,
}

impl Evidence {
    /// The word the answer names it by.
    pub fn word(self) -> &'static str {
        match self {
            Evidence::Superuser => "superuser",
            Evidence::Membership => "membership",
            Evidence::Table => "table",
            Evidence::Sequence => "sequence",
            Evidence::Schema => "schema",
        }
    }

    /// Every kind, in the order the answer names them.
    pub const ALL: [Evidence; 5] = [
        Evidence::Superuser,
        Evidence::Membership,
        Evidence::Table,
        Evidence::Sequence,
        Evidence::Schema,
    ];
}

/// A login that can write by exactly these kinds of evidence (a superuser is
/// everything, so `Superuser` stands for itself; with it the others are not
/// asked for).
pub fn writing_login(db: &SharedDatabase, kinds: &[Evidence]) -> Login {
    let superuser = kinds.contains(&Evidence::Superuser);
    let login = new_login(db, "writes", if superuser { "SUPERUSER" } else { "" });
    let mut grants: Vec<String> = Vec::new();
    for kind in kinds {
        match kind {
            Evidence::Superuser => {}
            Evidence::Membership => {
                grants.push(format!("GRANT pg_write_all_data TO {}", login.role))
            }
            Evidence::Table => grants.push(format!("GRANT INSERT ON incidents TO {}", login.role)),
            Evidence::Sequence => {
                grants.push(format!(
                    "GRANT UPDATE ON SEQUENCE ticket_numbers TO {}",
                    login.role
                ));
            }
            Evidence::Schema => {
                grants.push(format!("GRANT CREATE ON SCHEMA public TO {}", login.role))
            }
        }
    }
    if !grants.is_empty() {
        db.superuser().batch(&grants.join("; "));
    }
    login
}

/// A login that can read rows by this kind of evidence: `SELECT` on a table
/// (`Table`), membership of `pg_read_all_data` (`Membership`) or a
/// superuser (`Superuser`).
pub fn reading_login(db: &SharedDatabase, kinds: &[Evidence]) -> Login {
    let superuser = kinds.contains(&Evidence::Superuser);
    let login = new_login(db, "reads", if superuser { "SUPERUSER" } else { "" });
    let mut grants: Vec<String> = Vec::new();
    for kind in kinds {
        match kind {
            Evidence::Membership => {
                grants.push(format!("GRANT pg_read_all_data TO {}", login.role))
            }
            Evidence::Table => grants.push(format!("GRANT SELECT ON incidents TO {}", login.role)),
            _ => {}
        }
    }
    if !grants.is_empty() {
        db.superuser().batch(&grants.join("; "));
    }
    login
}

/// A role that exists and has a password, but may not connect to `db`
/// (`CONNECT` not granted): the server refuses it.
pub fn login_that_may_not_connect(db: &SharedDatabase) -> Login {
    let t = token();
    let login = Login::new(&format!("shut_{t}"), &format!("pw-shut-{t}{}", token()));
    at_server(db).batch(&format!(
        "CREATE ROLE {} LOGIN PASSWORD '{}'",
        login.role,
        login.password()
    ));
    login
}

/// The same role with another password in the body: the server refuses it.
pub fn with_wrong_password(login: &Login) -> Login {
    Login::new(&login.role, &format!("pw-wrong-{}", token()))
}

/// Changes `login`'s password at the server to a new one.
pub fn change_password(db: &SharedDatabase, login: &Login) {
    at_server(db).batch(&format!(
        "ALTER ROLE {} PASSWORD 'pw-changed-{}'",
        login.role,
        token()
    ));
}

/// Drops `login`'s role: its sessions ended, what it holds in the shared
/// database revoked there (where its privileges are), then the role at the
/// server.
pub fn drop_role(db: &SharedDatabase, login: &Login) {
    let server = at_server(db);
    server.batch(&format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = '{}'",
        login.role
    ));
    db.superuser()
        .batch(&format!("DROP OWNED BY {}", login.role));
    server.batch(&format!("DROP ROLE {}", login.role));
}

/// Drops the database itself, ending its sessions: what its Resources then
/// connect to is gone.
pub fn drop_database(db: &SharedDatabase) {
    at_server(db).batch(&format!("DROP DATABASE {} WITH (FORCE)", db.name()));
}

/// How many sessions `login`'s role has at its server now
/// (`pg_stat_activity`, read as the shared database's administrator).
pub fn sessions_of(db: &SharedDatabase, login: &Login) -> i64 {
    at_server(db)
        .value(&format!(
            "SELECT count(*) FROM pg_stat_activity WHERE usename = '{}'",
            login.role
        ))
        .and_then(|n| n.parse().ok())
        .unwrap_or(-1)
}

/// Waits up to `within` for `login` to have no session; returns how many it
/// had at the end.
pub fn wait_for_no_sessions(db: &SharedDatabase, login: &Login, within: Duration) -> i64 {
    let deadline = Instant::now() + within;
    loop {
        let n = sessions_of(db, login);
        if n == 0 || Instant::now() >= deadline {
            return n;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The statements `login` sent to `db`, in order, from the server's log
/// ([`server_log::log_everything`] first, so that parameters are logged).
pub fn sent_by_login(db: &SharedDatabase, login: &Login) -> Vec<Sent> {
    server_log::sent_by(&server_log::entries(db.server()), db.name(), &login.role)
}

/// How many connections the server logged for `login` to `db`
/// (`connection authorized: user=...`).
pub fn connections_of(db: &SharedDatabase, login: &Login) -> usize {
    server_log::entries(db.server())
        .iter()
        .filter(|e| e.database == db.name() && e.user == login.role)
        .filter(|e| e.text.contains("connection authorized"))
        .count()
}

/// A listener that accepts connections and never answers on them: a login
/// to it hangs. The sockets are held until it is dropped.
pub struct Hanging {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Hanging {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|e| panic!("cannot listen for the hanging server: {e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("cannot read the hanging server's address: {e}"));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut held: Vec<TcpStream> = Vec::new();
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    // Bounded: a test opens a handful of connections.
                    if held.len() < 64 {
                        held.push(stream);
                    }
                }
            }
        });
        Self {
            address,
            stop,
            handle: Some(handle),
        }
    }

    pub fn host(&self) -> String {
        self.address.ip().to_string()
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }
}

impl Drop for Hanging {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _woken = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _joined = handle.join();
        }
    }
}

/// The `connection` of a body: this database with these logins.
pub fn connection(
    db: &SharedDatabase,
    read: &Login,
    write: Option<&Login>,
    structure: Option<&Login>,
) -> Value {
    let mut connection = db.connection(read);
    if let Some(write) = write {
        connection["write"] = login_json(write);
    }
    if let Some(structure) = structure {
        connection["structure"] = login_json(structure);
    }
    connection
}

/// `{ "role", "password" }`.
pub fn login_json(login: &Login) -> Value {
    json!({ "role": login.role, "password": login.password() })
}

/// The body that registers `name` with this `connection`.
pub fn body(name: &str, connection: &Value) -> Value {
    json!({
        "name": name,
        "kind": "postgres",
        "description": DESCRIPTION,
        "owner": OWNER,
        "contact": CONTACT,
        "connection": connection,
    })
}

/// The body that registers `name` over `db` with the reader as its read
/// login.
pub fn reader_body(name: &str, db: &SharedDatabase) -> Value {
    body(name, &connection(db, &db.reader, None, None))
}

/// Every string of a connection that no answer, message or log record may
/// hold, as `(what, text)`: host, port as `:<n>`, database, roles and
/// passwords. The host is only a marker when it is unmistakable.
pub fn secrets_of(db: &SharedDatabase, logins: &[&Login]) -> Vec<(String, String)> {
    let mut all = vec![("the database".to_owned(), db.name().to_owned())];
    if db.host_is_distinct() {
        all.push(("the host".to_owned(), db.host()));
    }
    for login in logins {
        all.push(("a role".to_owned(), login.role.clone()));
        all.push(("a password".to_owned(), login.password().to_owned()));
    }
    all
}

/// The names of `secrets` found in `text` (by what they are, never what
/// they hold).
pub fn found_in(secrets: &[(String, String)], text: &str) -> Vec<String> {
    secrets
        .iter()
        .filter(|(_, value)| text.contains(value.as_str()))
        .map(|(what, _)| what.clone())
        .collect()
}

/// A Resource a test needs that nobody can reach: registered with a
/// database of its own and logins that connect, then the database dropped.
/// The contract's "What changes in earlier sections" gives this way: a
/// registration whose login cannot connect is `422`.
pub fn register_gone(scenario: &Scenario, name: &str) -> SharedDatabase {
    let db = SharedDatabase::create();
    scenario.register_body(name, &reader_body(name, &db));
    drop_database(&db);
    db
}

/// `register_gone` with the read login's role dropped instead of the
/// database: the server refuses it.
pub fn register_refused_later(scenario: &Scenario, name: &str) -> SharedDatabase {
    let db = SharedDatabase::create();
    let login = reader_of_its_own(&db);
    scenario.register_body(name, &body(name, &connection(&db, &login, None, None)));
    change_password(&db, &login);
    db
}

/// A login of this exact role and password, made for `db`: for a test that
/// searches for the role's name.
pub fn named_login(db: &SharedDatabase, role: &str, password: &str) -> Login {
    at_server(db).batch(&format!(
        "CREATE ROLE {role} LOGIN PASSWORD '{password}';
         GRANT CONNECT ON DATABASE {} TO {role};
         GRANT USAGE ON SCHEMA public TO {role}",
        db.name()
    ));
    Login::new(role, password)
}

/// A login of its own that reads `incidents`, `kinds` and `notes` and nothing
/// else (the reader's privileges, directly held, so that the role it was
/// made like can be dropped).
pub fn reader_direct(db: &SharedDatabase) -> Login {
    limited_reader(db, &["incidents", "kinds", "notes"])
}

/// A login of its own that reads and writes `incidents`, `kinds` and `notes`
/// (the writer's privileges, directly held).
pub fn writer_direct(db: &SharedDatabase) -> Login {
    let login = new_login(db, "wdirect", "");
    db.superuser().batch(&format!(
        "GRANT USAGE ON SCHEMA public TO {role};
         GRANT SELECT, INSERT, UPDATE, DELETE ON incidents, kinds, notes TO {role};
         GRANT USAGE ON SEQUENCE notes_id_seq TO {role}",
        role = login.role
    ));
    login
}

/// A request of the administrator's API with this method and JSON body.
pub fn call(
    api: &crate::api::Api<'_>,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> crate::http::Response {
    let mut request = api.request(path).method(method);
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send()
}

/// `PUT /v1/resources/<name>/connection` with this body.
pub fn put_connection(
    api: &crate::api::Api<'_>,
    name: &str,
    body: &Value,
) -> crate::http::Response {
    call(
        api,
        "PUT",
        &format!("/v1/resources/{name}/connection"),
        Some(body),
    )
}

/// `PATCH /v1/resources/<name>` with this body.
pub fn patch_resource(
    api: &crate::api::Api<'_>,
    name: &str,
    body: &Value,
) -> crate::http::Response {
    call(api, "PATCH", &format!("/v1/resources/{name}"), Some(body))
}

/// `GET /v1/admin/resources/<name>`.
pub fn admin_resource(api: &crate::api::Api<'_>, name: &str) -> crate::http::Response {
    call(api, "GET", &format!("/v1/admin/resources/{name}"), None)
}

/// `GET /v1/admin/resources` with this query (`""` or `?limit=5`).
pub fn admin_resources(api: &crate::api::Api<'_>, query: &str) -> crate::http::Response {
    call(api, "GET", &format!("/v1/admin/resources{query}"), None)
}

/// A program on which `devops` is not served for this problem
/// (`no-secret-key`, `does-not-open`, `not-sealed`), with Application `board`
/// declaring it.
pub fn not_served(problem: &str) -> crate::sealed::Sealed {
    match problem {
        "no-secret-key" => {
            let s = crate::sealed::Sealed::start(&crate::vm::VmOptions::new());
            s.with_board();
            s.restarted_with(&crate::vm::VmOptions::new().without_secret_key())
        }
        "does-not-open" => {
            let s = crate::sealed::Sealed::start(&crate::vm::VmOptions::new());
            s.with_board();
            s.restarted_with(
                &crate::vm::VmOptions::new()
                    .env("VIBE_SECRET_KEY", crate::sealed::another_key().expose()),
            )
        }
        _ => {
            let before = crate::sealed::Before::make();
            let held = crate::sealed::Stored::of(&before.platform).admin();
            held.batch("BEGIN");
            held.batch("SELECT id FROM resources WHERE name = 'devops' FOR UPDATE");
            let (s, _old) = before.start(&crate::vm::VmOptions::new());
            held.batch("ROLLBACK");
            s
        }
    }
}

/// The program's log records of this `event`, waiting up to 5 seconds until
/// `expected` of them are there (the harness reads the program's output on
/// a thread of its own, so a record written just before an answer may not
/// have arrived). The caller asserts the exact count: an extra record still
/// fails. With `expected` 0 it waits half a second and then reads, so that
/// a record that is late is still seen.
pub fn records_waited(vm: &crate::vm::Vm, event: &str, expected: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    if expected == 0 {
        std::thread::sleep(Duration::from_millis(500));
    }
    loop {
        let found: Vec<Value> = vm
            .records()
            .into_iter()
            .filter(|r| r["event"] == event)
            .collect();
        if found.len() >= expected || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Reshapes a fresh shared database for the tests of "A login limited to
/// some tables": its tables are `incidents`, `audit` and `payroll` and no
/// other (`kinds`, `notes` and `staff_only` are dropped), `incidents` has a
/// column `owner_id` with a foreign key to `payroll`, and `payroll` has a
/// column `salary`. The harness's own roles keep what they hold on
/// `incidents`; nothing is granted on the other two.
pub fn payroll_shape(db: &SharedDatabase) {
    db.superuser().batch(
        "DROP TABLE kinds, notes, staff_only CASCADE;
         CREATE TABLE payroll (id int4 PRIMARY KEY, salary int4 NOT NULL);
         CREATE TABLE audit (id int4 PRIMARY KEY, what text);
         ALTER TABLE incidents ADD COLUMN owner_id int4 CONSTRAINT incidents_owner_fk REFERENCES payroll (id);
         INSERT INTO payroll VALUES (1, 100000);",
    );
}

/// A fresh login that holds exactly these privileges (each as `SELECT ON
/// incidents` or `INSERT (title) ON incidents`) and `USAGE` on the schema
/// `public`, and nothing else.
pub fn login_with_privileges(db: &SharedDatabase, grants: &[&str]) -> Login {
    let login = new_login(db, "priv", "");
    let mut statements = vec![format!("GRANT USAGE ON SCHEMA public TO {}", login.role)];
    for grant in grants {
        statements.push(format!("GRANT {grant} TO {}", login.role));
    }
    db.superuser().batch(&statements.join("; "));
    login
}

/// `PUT /v1/resources/<name>/location` with this body.
pub fn put_location(api: &crate::api::Api<'_>, name: &str, body: &Value) -> crate::http::Response {
    call(
        api,
        "PUT",
        &format!("/v1/resources/{name}/location"),
        Some(body),
    )
}

/// The body of a location: `{ "backend": "directory", "directory": <path> }`.
pub fn location_body(directory: &str) -> Value {
    json!({ "backend": "directory", "directory": directory })
}

/// `DELETE /v1/resources/<name>`, no body.
pub fn delete_resource(api: &crate::api::Api<'_>, name: &str) -> crate::http::Response {
    call(api, "DELETE", &format!("/v1/resources/{name}"), None)
}
