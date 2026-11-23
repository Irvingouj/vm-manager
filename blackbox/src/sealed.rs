//! What the tests of "Resources and Grants in full: credentials encrypted"
//! (CONTRACT.md, "Resources and Grants in full", stage one) share:
//!
//! - [`Sealed`], a program on a platform database with a shared database of
//!   the test's own: Resources registered on it, Applications that declare
//!   them, an Application with an own database, operations on both, and a
//!   restart on the same platform database with another key or none;
//! - [`Markers`], the recognisable strings of a registration (host, port,
//!   database, roles, passwords) and the forms a dump could hold them in:
//!   whole, hex, base64;
//! - [`Dump`], every column of every table of the platform database read as
//!   the Postgres administrator, and the 64-lower-case-hex rule of
//!   `own_databases`;
//! - the stored values themselves: what a Resource's `connection` and an
//!   own database's passwords are (as the program before this ticket wrote
//!   them: clear), read and written back from outside, so that a test can
//!   make a platform database "from before".
//!
//! No test value is ever printed: a failure names a marker by what it is
//! (`the read login's password, as hex`), never by what it holds.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::api::{Api, version_body_declaring};
use crate::http::{REQUEST_TIMEOUT, Response, User};
use crate::own_database::{
    FIXTURE, Migration, OwnDatabase, own_query, version_body_with_migrations,
};
use crate::postgres::Platform;
use crate::scenario::{ADMINISTRATORS, READERS, administrator, op, resource_body, sql_on};
use crate::shared_database::{Login, SharedDatabase, token};
use crate::sql::Sql;
use crate::vm::{INSTALLER, Vm, VmOptions, fixture_bundle};

/// The Resource most of these tests register.
pub const DEVOPS: &str = "devops";

/// The Application that declares [`DEVOPS`] at `read`.
pub const BOARD: &str = "board";

/// The Application with an own database.
pub const SHOP: &str = "shop";

/// A User who may read [`DEVOPS`] once [`READERS`] holds a Grant on it.
pub fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A program on a platform database, and a shared database of the test's
/// own.
pub struct Sealed {
    pub vm: Vm,
    pub db: SharedDatabase,
}

impl Sealed {
    /// The program with the stand-in sign-in and [`ADMINISTRATORS`], from
    /// these options (the harness's key unless they say otherwise).
    pub fn start(options: &VmOptions) -> Self {
        Self {
            db: SharedDatabase::create(),
            vm: Vm::start(&Self::with_stand_in(options)),
        }
    }

    /// The program on this platform database, beside this shared database.
    pub fn start_on(platform: &Platform, db: SharedDatabase, options: &VmOptions) -> Self {
        Self {
            vm: Vm::start(&Self::with_stand_in(options).platform(platform)),
            db,
        }
    }

    fn with_stand_in(options: &VmOptions) -> VmOptions {
        options
            .clone()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS)
    }

    /// Ends the program and starts it again on the same platform database
    /// from these options, with the same shared database.
    #[must_use]
    pub fn restarted_with(self, options: &VmOptions) -> Self {
        let platform = self.vm.platform().clone();
        let Self { vm, db } = self;
        drop(vm);
        Self {
            vm: Vm::start(&Self::with_stand_in(options).platform(&platform)),
            db,
        }
    }

    /// Ends the program and starts it again from the options it had.
    #[must_use]
    pub fn restarted(self) -> Self {
        let options = self.vm.options().clone();
        self.restarted_with(&options)
    }

    /// The platform database.
    pub fn platform(&self) -> &Platform {
        self.vm.platform()
    }

    /// The platform's API as the Platform administrator.
    pub fn admin(&self) -> Api<'_> {
        self.vm.api_as(&administrator())
    }

    /// Registers [`DEVOPS`]-like Resource `name` with the shared database's
    /// read and write logins, failing the test unless `201`.
    pub fn register_writable(&self, name: &str) {
        let mut body = resource_body(name, &self.db, &self.db.reader);
        body["connection"] = self
            .db
            .connection_with_write(&self.db.reader, &self.db.writer);
        self.register_body(name, &body);
    }

    /// Registers `name` with this login as its read login, failing the test
    /// unless `201`.
    pub fn register_with(&self, name: &str, login: &Login) {
        self.register_body(name, &resource_body(name, &self.db, login));
    }

    /// Registers this body, failing the test unless `201`.
    pub fn register_body(&self, name: &str, body: &Value) {
        let answer = self.admin().create_resource(body);
        if answer.status != 201 {
            self.vm.fail(&format!(
                "expected `201` registering the Resource {name:?} as a Platform administrator, got\n{}",
                answer.describe()
            ));
        }
    }

    /// Gives [`READERS`] `read` on `resource`, failing unless `200`.
    pub fn grant_read(&self, resource: &str) {
        let answer = self.admin().grant(resource, READERS, "read");
        if answer.status != 200 {
            self.vm.fail(&format!(
                "expected `200` granting {READERS:?} `read` on {resource:?}, got\n{}",
                answer.describe()
            ));
        }
    }

    /// Creates the Application `name` (shared with everyone) and sends
    /// `fixtures/operations.js` as its first Version, declaring these
    /// Resources as (name, access level) and carrying these migrations.
    pub fn install(&self, name: &str, declared: &[(&str, &str)], migrations: &[Migration]) {
        let api = self.vm.api_as(&User::new(INSTALLER));
        let created = api.create_application(name);
        if created.status != 201 {
            self.vm.fail(&format!(
                "expected `201` creating the Application {name:?}, got\n{}",
                created.describe()
            ));
        }
        api.share_with_everyone(name);
        let mut body = version_body_declaring(&fixture_bundle(FIXTURE), declared);
        if !migrations.is_empty() {
            body = version_body_with_migrations(&fixture_bundle(FIXTURE), declared, migrations);
        }
        let sent = api
            .request(&format!("/v1/applications/{name}/versions"))
            .method("POST")
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send();
        if sent.status != 201 {
            self.vm.fail(&format!(
                "expected `201` sending a Version of {name:?}, got\n{}",
                sent.describe()
            ));
        }
    }

    /// The usual setting: [`DEVOPS`] registered with its read and write
    /// logins, [`READERS`] granted `read`, [`BOARD`] declaring it at `read`.
    pub fn with_board(&self) {
        self.register_writable(DEVOPS);
        self.grant_read(DEVOPS);
        self.install(BOARD, &[(DEVOPS, "read")], &[]);
    }

    /// [`SHOP`], an Application with an own database made by these
    /// migrations; returns the own database.
    pub fn with_shop(&self, migrations: &[Migration]) -> OwnDatabase {
        self.install(SHOP, &[], migrations);
        OwnDatabase::find(self.platform(), SHOP).unwrap_or_else(|| {
            self.vm
                .fail("expected an own database of the Application `shop`, found none")
        })
    }

    /// These calls through `fixtures/operations.js` in `application` as
    /// `user`; the request's answer and the answer of each call.
    pub fn calls(&self, user: &User, application: &str, calls: &[Value]) -> (Response, Vec<Value>) {
        let response = self
            .vm
            .request(application, "/")
            .as_user(user)
            .method("POST")
            .json(&json!({ "calls": calls }))
            .timeout(REQUEST_TIMEOUT)
            .send();
        if response.status != 200 {
            self.vm.fail(&format!(
                "expected `200` from {application:?} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        let answers = json["answers"].as_array().cloned().unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected {{ answers: [...] }} from {application:?}, got {json}"
            ))
        });
        (response, answers)
    }

    /// One call.
    pub fn call(&self, user: &User, application: &str, call: &Value) -> (Response, Value) {
        let (response, answers) = self.calls(user, application, std::slice::from_ref(call));
        (response, answers.into_iter().next().unwrap_or(Value::Null))
    }

    /// `SELECT 1` on [`DEVOPS`] as [`rita`] through [`BOARD`]: the answer of
    /// the call.
    pub fn read_devops(&self) -> (Response, Value) {
        self.call(
            &rita(),
            BOARD,
            &op("query", &sql_on(DEVOPS, "SELECT 1 AS one")),
        )
    }

    /// The labels of `seeded` in [`SHOP`]'s own database, through [`SHOP`]
    /// as the User: the answer of the call.
    pub fn read_shop(&self) -> (Response, Value) {
        self.read_own(SHOP)
    }

    /// The labels of `seeded` in the own database of `application`
    /// (carrying `first_tables`), through it as the User.
    pub fn read_own(&self, application: &str) -> (Response, Value) {
        self.call(
            &rita(),
            application,
            &own_query("user", "SELECT label FROM seeded ORDER BY id"),
        )
    }

    /// The records of the program's log with this event, in order.
    pub fn logged(&self, event: &str) -> Vec<Value> {
        self.vm
            .records()
            .into_iter()
            .filter(|r| r["event"] == event)
            .collect()
    }

    /// The position of the first record of this event among the lines of the
    /// program's log, and the position of its `listening` record.
    pub fn before_listening(&self, event: &str) -> (Option<usize>, Option<usize>) {
        let lines = self.vm.lines();
        let at = |wanted: &str| {
            lines
                .iter()
                .position(|l| l.record.as_ref().is_some_and(|r| r["event"] == wanted))
        };
        (at(event), at("listening"))
    }
}

/// The program's stored form of what this ticket seals, read from outside as
/// the administrator of the platform database.
pub struct Stored {
    platform: Platform,
}

/// An own database's passwords as stored, each as the text Postgres prints
/// for the column's value (`None`: null).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnPasswords {
    pub platform: Option<String>,
    pub runtime: Option<String>,
    pub owner: Option<String>,
}

impl Stored {
    pub fn of(platform: &Platform) -> Self {
        Self {
            platform: platform.clone(),
        }
    }

    /// The administrator's connection to the platform database.
    pub fn admin(&self) -> Sql {
        Sql::connect(self.platform.url()).unwrap_or_else(|e| {
            panic!("cannot reach the platform database as its administrator: {e}")
        })
    }

    /// The bytes stored as a Resource's connection, if the Resource is
    /// there.
    pub fn connection(&self, resource: &str) -> Option<Vec<u8>> {
        let hex = self.admin().value(&format!(
            "SELECT encode(connection, 'hex') FROM resources WHERE name = {}",
            crate::own_database::literal(resource)
        ))?;
        Some(unhex(&hex))
    }

    /// Writes these bytes as a Resource's stored connection.
    pub fn set_connection(&self, resource: &str, bytes: &[u8]) {
        let count = self.admin().batch(&format!(
            "UPDATE resources SET connection = decode('{}', 'hex') WHERE name = {} RETURNING id",
            hex_of(bytes),
            crate::own_database::literal(resource)
        ));
        assert_eq!(
            count.len(),
            1,
            "the harness's own step: one Resource {resource:?} to write"
        );
    }

    /// The stored passwords of an Application's own database.
    pub fn own_passwords(&self, application: &str) -> OwnPasswords {
        let rows = self.admin().batch(&format!(
            "SELECT o.platform_password::text, o.runtime_password::text, o.owner_password::text
               FROM own_databases o JOIN applications a ON a.id = o.application_id
              WHERE a.name = {}",
            crate::own_database::literal(application)
        ));
        let row = rows.first().unwrap_or_else(|| {
            panic!("the harness's own step: no own_databases row of {application:?}")
        });
        OwnPasswords {
            platform: row.first().cloned().flatten(),
            runtime: row.get(1).cloned().flatten(),
            owner: row.get(2).cloned().flatten(),
        }
    }

    /// Writes these two passwords, as text, into an Application's row.
    pub fn set_own_passwords(&self, application: &str, platform: &str, runtime: &str) {
        let rows = self.admin().batch(&format!(
            "UPDATE own_databases o SET platform_password = {}, runtime_password = {}
               FROM applications a WHERE a.id = o.application_id AND a.name = {}
             RETURNING o.application_id",
            crate::own_database::literal(platform),
            crate::own_database::literal(runtime),
            crate::own_database::literal(application)
        ));
        assert_eq!(
            rows.len(),
            1,
            "the harness's own step: one own_databases row of {application:?} to write"
        );
    }

    /// Copies the stored connection of `from` into `to`'s row, byte for
    /// byte.
    pub fn copy_connection(&self, from: &str, to: &str) {
        let bytes = self
            .connection(from)
            .unwrap_or_else(|| panic!("the harness's own step: no Resource {from:?}"));
        self.set_connection(to, &bytes);
    }

    /// Copies the stored passwords of one Application's own database to
    /// another's row, as they are.
    pub fn copy_own_passwords(&self, from: &str, to: &str) {
        let rows = self.admin().batch(&format!(
            "UPDATE own_databases t SET platform_password = f.platform_password,
                                        runtime_password = f.runtime_password
               FROM own_databases f, applications fa, applications ta
              WHERE f.application_id = fa.id AND fa.name = {}
                AND t.application_id = ta.id AND ta.name = {}
             RETURNING t.application_id",
            crate::own_database::literal(from),
            crate::own_database::literal(to)
        ));
        assert_eq!(
            rows.len(),
            1,
            "the harness's own step: one row of {to:?} to write"
        );
    }
}

/// The connection of a Resource as the program before this ticket stored it
/// (clear JSON as bytes), for a shared database with these logins: what
/// [`Stored::set_connection`] writes to make a platform database "from
/// before".
pub fn clear_connection(db: &SharedDatabase, read: &Login, write: Option<&Login>) -> Vec<u8> {
    json!({
        "host": db.host(),
        "port": db.port(),
        "database": db.name(),
        "read": { "role": read.role, "password": read.password() },
        "write": write.map(|w| json!({ "role": w.role, "password": w.password() })),
        "structure": null,
    })
    .to_string()
    .into_bytes()
}

/// 64 lower-case hex digits no other value of the test shares: the form an
/// own database's password had before this ticket.
pub fn old_form_password() -> String {
    format!("{}{}{}{}", token(), token(), token(), token())
}

/// Makes `old_form_password` the password of both the platform's and the
/// runtime role of this own database and stores them in clear in the
/// Application's row, as the program before this ticket left them. Returns
/// the two.
pub fn make_own_passwords_clear(
    stored: &Stored,
    own: &OwnDatabase,
    application: &str,
) -> (String, String) {
    let platform_password = old_form_password();
    let runtime_password = old_form_password();
    let instance = own.instance();
    instance.batch(&format!(
        "ALTER ROLE {} PASSWORD {}; ALTER ROLE {} PASSWORD {}",
        crate::own_database::ident(&format!("{}_platform", own.name)),
        crate::own_database::literal(&platform_password),
        crate::own_database::ident(&own.runtime),
        crate::own_database::literal(&runtime_password),
    ));
    stored.set_own_passwords(application, &platform_password, &runtime_password);
    (platform_password, runtime_password)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16)
                .unwrap_or_else(|e| panic!("the harness's own step: not hex: {e}"))
        })
        .collect()
}

/// One recognisable string, by what it is.
#[derive(Clone)]
pub struct Marker {
    pub what: String,
    value: String,
    /// A number searched only standing alone (a port), whole.
    number: bool,
}

impl std::fmt::Debug for Marker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Marker({})", self.what)
    }
}

/// The recognisable strings of a registration, and the forms of them a dump
/// could hold.
#[derive(Clone, Debug, Default)]
pub struct Markers(Vec<Marker>);

impl Markers {
    /// The shared database's host (when it is unmistakable), port, database,
    /// the three roles and the three passwords.
    pub fn of(db: &SharedDatabase) -> Self {
        let mut markers: Vec<Marker> = db
            .credential_markers()
            .into_iter()
            .map(|(what, value)| Marker {
                what: what.to_owned(),
                value,
                number: false,
            })
            .collect();
        markers.push(Marker {
            what: "the port".to_owned(),
            value: db.port().to_string(),
            number: true,
        });
        Self(markers)
    }

    /// Also this string, called `what`.
    #[must_use]
    pub fn and(mut self, what: &str, value: &str) -> Self {
        self.0.push(Marker {
            what: what.to_owned(),
            value: value.to_owned(),
            number: false,
        });
        self
    }

    /// The markers' own values, for a test that must be sure it searches for
    /// something. Never print them.
    pub fn count(&self) -> usize {
        self.0.len()
    }

    /// What `text` holds of them, each by what it is and the form it is in.
    pub fn found_in(&self, text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for marker in &self.0 {
            if marker.number {
                // A port is looked for as the clear form writes it
                // (`"port":<n>,` in JSON), not as any number standing alone:
                // a sealed value read as bytes (`escape`, with its octal
                // escapes) holds digit runs by chance.
                let n = &marker.value;
                if text.contains(&format!(":{n},")) || text.contains(&format!(":{n}}}")) {
                    found.push(format!("{}, whole", marker.what));
                }
                continue;
            }
            for (form, needle) in forms(&marker.value) {
                if text.contains(&needle) {
                    found.push(format!("{}, {form}", marker.what));
                }
            }
        }
        found
    }
}

/// The forms of `value` a dump could hold: whole, hex in either case, and
/// base64 (standard and URL-safe) of the string as a part of a longer one,
/// at each of the three alignments, with the characters that depend on its
/// neighbours left out.
pub fn forms(value: &str) -> Vec<(&'static str, String)> {
    let mut forms = vec![
        ("whole", value.to_owned()),
        ("as hex", hex_of(value.as_bytes())),
        ("as upper-case hex", hex_of(value.as_bytes()).to_uppercase()),
    ];
    for shift in 0..3_usize {
        let mut bytes = vec![0_u8; shift];
        bytes.extend_from_slice(value.as_bytes());
        let skip = [0, 2, 3][shift];
        for (form, encoded) in [
            ("as base64", STANDARD.encode(&bytes)),
            ("as URL-safe base64", URL_SAFE_NO_PAD.encode(&bytes)),
        ] {
            let end = encoded.len().saturating_sub(2);
            if let Some(core) = encoded.get(skip..end).filter(|c| c.len() >= 12) {
                forms.push((form, core.to_owned()));
            }
        }
    }
    forms
}

/// Whether `text` holds a run of 64 or more lower-case hex digits: the form
/// an own database's password had before this ticket.
pub fn has_hex64_run(text: &str) -> bool {
    let mut run = 0;
    for byte in text.bytes() {
        if byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) {
            run += 1;
            if run >= 64 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Every column of every table of the platform database, read as the
/// Postgres administrator, as text. A `bytea` column is read twice: as its
/// bytes (`escape`) and as hex.
pub struct Dump {
    /// Table, then column (a `bytea` one also as `column (hex)`), then the
    /// values of its rows.
    cells: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl Dump {
    pub fn of(platform: &Platform) -> Self {
        let sql = Sql::connect(platform.url()).unwrap_or_else(|e| {
            panic!("cannot reach the platform database as its administrator: {e}")
        });
        let mut cells: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        let columns = sql.batch(
            "SELECT table_schema, table_name, column_name, data_type
               FROM information_schema.columns
              WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                AND (table_schema, table_name) IN (
                    SELECT table_schema, table_name FROM information_schema.tables
                     WHERE table_type = 'BASE TABLE')
              ORDER BY table_schema, table_name, ordinal_position",
        );
        for row in columns {
            let get = |at: usize| row.get(at).cloned().flatten().unwrap_or_default();
            let (schema, table, column, kind) = (get(0), get(1), get(2), get(3));
            let from = format!(
                "{}.{}",
                crate::own_database::ident(&schema),
                crate::own_database::ident(&table)
            );
            let name = crate::own_database::ident(&column);
            let expressions: Vec<(String, String)> = if kind == "bytea" {
                vec![
                    (column.clone(), format!("encode({name}, 'escape')")),
                    (format!("{column} (hex)"), format!("encode({name}, 'hex')")),
                ]
            } else {
                vec![(column.clone(), format!("{name}::text"))]
            };
            for (label, expression) in expressions {
                let values: Vec<String> = sql
                    .batch(&format!("SELECT {expression} FROM {from}"))
                    .into_iter()
                    .map(|r| r.into_iter().next().flatten().unwrap_or_default())
                    .collect();
                cells
                    .entry(table.clone())
                    .or_default()
                    .insert(label, values);
            }
        }
        Self { cells }
    }

    /// The number of rows of a table.
    pub fn rows_of(&self, table: &str) -> usize {
        self.cells
            .get(table)
            .and_then(|columns| columns.values().map(Vec::len).max())
            .unwrap_or(0)
    }

    /// Every cell of every table, one per line.
    pub fn text(&self) -> String {
        self.cells
            .values()
            .flat_map(|columns| columns.values().flatten())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every cell of one table, one per line (the hex reading of a `bytea`
    /// column left out when `with_hex` is false).
    pub fn table_text(&self, table: &str, with_hex: bool) -> String {
        self.cells
            .get(table)
            .map(|columns| {
                columns
                    .iter()
                    .filter(|(label, _)| with_hex || !label.ends_with(" (hex)"))
                    .flat_map(|(_, values)| values.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// Where each marker is found, as `table.column` with the markers' names
    /// (never their values), for a failure message.
    pub fn where_found(&self, markers: &Markers) -> Vec<(String, Vec<String>)> {
        let mut found = Vec::new();
        for (table, columns) in &self.cells {
            for (column, values) in columns {
                let names = markers.found_in(&values.join("\n"));
                if !names.is_empty() {
                    found.push((format!("{table}.{column}"), names));
                }
            }
        }
        found
    }

    /// The columns of `own_databases` that hold a run of 64 lower-case hex
    /// digits.
    pub fn old_form_columns(&self) -> Vec<String> {
        self.cells
            .get("own_databases")
            .map(|columns| {
                columns
                    .iter()
                    .filter(|(label, _)| !label.ends_with(" (hex)"))
                    .filter(|(_, values)| values.iter().any(|v| has_hex64_run(v)))
                    .map(|(label, _)| label.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The contract's own test of a dump: reads every column of every table of
/// the platform database as its administrator, and fails unless it holds
/// none of the recognisable strings (whole, as hex or base64) and no run of
/// 64 lower-case hex digits in `own_databases`. It also fails if there is
/// no Resource row and no own database row to have looked at, so that it
/// cannot pass over an empty database.
pub fn assert_dump_holds_no_credential(platform: &Platform, markers: &Markers, context: &str) {
    let dump = Dump::of(platform);
    assert!(
        dump.rows_of("resources") >= 1,
        "{context}: the harness's own check: the dump holds no Resource row to look at"
    );
    assert!(
        dump.rows_of("own_databases") >= 1,
        "{context}: the harness's own check: the dump holds no own database row to look at"
    );
    assert!(markers.count() > 0, "{context}: no markers to look for");
    let found = markers.found_in(&dump.text());
    let old = dump.old_form_columns();
    assert!(
        found.is_empty() && old.is_empty(),
        "{context}: expected a dump of the platform database to hold no recognisable credential and no run of 64 lower-case hex digits in `own_databases`, but it holds: {found:?}; runs of 64 hex digits in the columns {old:?}"
    );
}

/// A test key that is not the harness's: 32 bytes in base64.
pub fn another_key() -> crate::connection::SecretKey {
    crate::connection::SecretKey::fresh()
}

/// A platform database "from before this ticket": a program that has run on
/// it with [`DEVOPS`] registered (read and write logins), [`READERS`]
/// granted `read`, [`BOARD`] declaring it, and [`SHOP`] with an own
/// database, whose stored values were then written back in the clear form
/// the program before this ticket kept: the connection as JSON bytes, the
/// two passwords of the own database as 64 lower-case hex digits, which are
/// also the roles' passwords in Postgres. The program is not running.
pub struct Before {
    pub platform: Platform,
    pub db: SharedDatabase,
    /// The recognisable strings of the registration and the two passwords
    /// of the own database.
    pub markers: Markers,
    /// The connection bytes stored for [`DEVOPS`].
    pub connection: Vec<u8>,
    /// The stored passwords of [`SHOP`]'s own database: platform's, runtime.
    pub passwords: (String, String),
}

impl Before {
    pub fn make() -> Self {
        let s = Sealed::start(&VmOptions::new());
        s.with_board();
        let own = s.with_shop(&[crate::own_database::first_tables()]);
        let platform = s.platform().clone();
        let Sealed { vm, db } = s;
        drop(vm);
        let stored = Stored::of(&platform);
        let connection = clear_connection(&db, &db.reader, Some(&db.writer));
        stored.set_connection(DEVOPS, &connection);
        let passwords = make_own_passwords_clear(&stored, &own, SHOP);
        let markers = Markers::of(&db)
            .and("the platform role's password of before", &passwords.0)
            .and("the runtime role's password of before", &passwords.1);
        Self {
            platform,
            db,
            markers,
            connection,
            passwords,
        }
    }

    /// The program on this platform database from these options; the
    /// shared database, the stored values and the markers stay with the
    /// returned [`Old`].
    pub fn start(self, options: &VmOptions) -> (Sealed, Old) {
        let Self {
            platform,
            db,
            markers,
            connection,
            passwords,
        } = self;
        let program = Sealed::start_on(&platform, db, options);
        (
            program,
            Old {
                platform,
                markers,
                connection,
                passwords,
            },
        )
    }
}

/// What [`Before`] wrote.
pub struct Old {
    pub platform: Platform,
    pub markers: Markers,
    pub connection: Vec<u8>,
    pub passwords: (String, String),
}

impl Old {
    /// Whether [`DEVOPS`]'s stored connection and [`SHOP`]'s stored
    /// passwords are still exactly what [`Before`] wrote.
    pub fn is_still_clear(&self) -> bool {
        let stored = Stored::of(&self.platform);
        let passwords = stored.own_passwords(SHOP);
        stored.connection(DEVOPS).as_deref() == Some(self.connection.as_slice())
            && passwords.platform.as_deref() == Some(self.passwords.0.as_str())
            && passwords.runtime.as_deref() == Some(self.passwords.1.as_str())
    }
}
