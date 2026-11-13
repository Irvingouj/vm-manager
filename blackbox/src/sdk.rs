//! What the tests of "The SDK: a typed Manifest and Trusted context, queried
//! with Drizzle" share (CONTRACT.md, "The SDK"):
//!
//! - the SDK's tooling, run as the contract gives it (`node
//!   sdk/tooling/main.js manifest|bundle <project directory>`, from the
//!   repository root), each run with a timeout ([`run_tooling`]);
//! - the fixture projects under `fixtures/sdk/`, built once per test binary
//!   and kept ([`built`]): the bundle, the Manifest the `manifest` command
//!   printed, and the own database's migrations, hand-written SQL files in
//!   the project's `migrations/` (statements separated by
//!   [`crate::own_database::BREAKPOINT`]);
//! - projects written by a test into a directory of their own under the
//!   repository's `target/`, so that `@vm/sdk` and `drizzle-orm` resolve from
//!   the root `node_modules` ([`TempProject`]);
//! - [`Sdk`], a program with a shared database registered twice (`devops`,
//!   read by the fixture `board`, and `ops`, written by it) and a fixture
//!   project installed as an Application with the Manifest's name;
//! - the descriptions the fixtures give of values inside the Application
//!   (`fixtures/sdk/board/src/describe.ts`), built here to compare with
//!   ([`d`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::SOME_GIT_COMMIT;
use crate::http::{Response, User};
use crate::own_database::{Migration, OwnDatabase, ident, migrations_json};
use crate::postgres::shared;
use crate::program::{Finished, manifest_dir, run_to_exit};
use crate::scenario::{READERS, Scenario, WRITERS, administrator, assert_entry, lines};
use crate::sql::Sql;
use crate::vm::{INSTALLER, Vm, VmOptions};

/// How long one run of the tooling (or of `node`) may take. Bundling the
/// first time may be slow on a cold disk.
pub const TOOLING_TIMEOUT: Duration = Duration::from_secs(180);

/// How long a request to an SDK fixture may take: a fresh context runs the
/// whole bundle (Drizzle included) before the handler.
pub const SDK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The TimeZone the tests give the shared database: not UTC, and not the
/// zone of the own database.
pub const SHARED_TIME_ZONE: &str = "Asia/Kolkata";

/// The TimeZone the tests give each own database: not UTC, a negative
/// offset with half an hour.
pub const OWN_TIME_ZONE: &str = "America/St_Johns";

/// The Resource `board` declares at `write`, over the same shared database
/// as [`crate::scenario::RESOURCE`] (`devops`, declared at `read`).
pub const OPS: &str = "ops";

/// The Application name of `fixtures/sdk/board`, as its Manifest gives it.
pub const BOARD: &str = "sdk-board";

/// The Application name of `fixtures/sdk/leftover`.
pub const LEFTOVER: &str = "sdk-leftover";

/// The Application name of `fixtures/sdk/ledger`, the fixture of
/// transactions.
pub const LEDGER: &str = "sdk-ledger";

/// The repository's root: the npm workspace, and where the tooling is run
/// from.
pub fn repository_root() -> PathBuf {
    manifest_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("the blackbox crate has no parent directory"))
}

/// `sdk/tooling/main.js`, as CONTRACT.md names it.
pub fn tooling_main() -> PathBuf {
    repository_root()
        .join("sdk")
        .join("tooling")
        .join("main.js")
}

/// The directory of a fixture project: `fixtures/sdk/<name>`.
pub fn fixture_project(name: &str) -> PathBuf {
    let dir = manifest_dir().join("fixtures").join("sdk").join(name);
    assert!(dir.is_dir(), "no fixture project {}", dir.display());
    dir
}

/// What a run of `node` left: its exit code (`None` when it was killed at
/// the timeout or by a signal), its output, and the command, for messages.
#[derive(Debug)]
pub struct Tooled {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub command: String,
}

impl Tooled {
    fn of(finished: Finished, command: String) -> Self {
        Self {
            code: finished.status.and_then(|s| s.code()),
            stdout: finished.stdout,
            stderr: finished.stderr,
            command,
        }
    }

    /// Standard output as one JSON value, if it is one.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(self.stdout.trim()).ok()
    }

    /// The run, for failure messages; standard output cut at 4000 bytes.
    pub fn describe(&self) -> String {
        let mut stdout = self.stdout.clone();
        if stdout.len() > 4000 {
            let mut cut = 4000;
            while !stdout.is_char_boundary(cut) {
                cut -= 1;
            }
            stdout.truncate(cut);
            stdout.push_str("... (cut)");
        }
        format!(
            "command: {}\nexit code: {:?}\nstandard output:\n{stdout}\nstandard error:\n{}",
            self.command, self.code, self.stderr
        )
    }
}

/// Runs `node` with these arguments from the repository's root, at most
/// [`TOOLING_TIMEOUT`].
pub fn node(args: &[&str]) -> Tooled {
    let root = repository_root();
    let mut command = Command::new("node");
    command.current_dir(&root).args(args);
    let text = format!("(in {}) node {}", root.display(), args.join(" "));
    let finished = run_to_exit(command, TOOLING_TIMEOUT)
        .unwrap_or_else(|problem| panic!("cannot run {text}: {problem}"));
    Tooled::of(finished, text)
}

/// `node sdk/tooling/main.js <command> <project>`, from the repository's
/// root, with the project given relative to it when it is inside it.
pub fn run_tooling(command: &str, project: &Path) -> Tooled {
    let root = repository_root();
    let project = project
        .strip_prefix(&root)
        .map_or_else(|_| project.to_path_buf(), Path::to_path_buf);
    let project = project.to_string_lossy().into_owned();
    node(&["sdk/tooling/main.js", command, &project])
}

/// A project built by the tooling: what a deploy would send.
#[derive(Debug)]
pub struct Built {
    /// The Manifest's `name`: the Application's name.
    pub name: String,
    /// What the `manifest` command printed.
    pub manifest: Value,
    pub declared_access: Value,
    pub bundle: String,
    pub migrations: Vec<Migration>,
}

/// The fixture project `fixtures/sdk/<name>`, built once per test binary:
/// `manifest` and `bundle` run on it, its migrations read. Fails the test
/// with the tooling's output if either command fails.
pub fn built(name: &str) -> Arc<Built> {
    type Cell = Arc<OnceLock<Result<Arc<Built>, String>>>;
    static BUILT: OnceLock<Mutex<HashMap<String, Cell>>> = OnceLock::new();
    let cell: Cell = {
        let mut all = BUILT
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(all.entry(name.to_owned()).or_default())
    };
    match cell.get_or_init(|| build(&fixture_project(name)).map(Arc::new)) {
        Ok(built) => Arc::clone(built),
        Err(problem) => panic!("cannot build the fixture project {name}: {problem}"),
    }
}

/// Runs `manifest` and `bundle` on the project in `dir` and reads its
/// migrations.
pub fn build(dir: &Path) -> Result<Built, String> {
    let manifest_run = run_tooling("manifest", dir);
    if manifest_run.code != Some(0) {
        return Err(format!(
            "expected `manifest` to exit 0 on {}\n{}",
            dir.display(),
            manifest_run.describe()
        ));
    }
    let manifest = manifest_run.json().ok_or_else(|| {
        format!(
            "expected `manifest` to print one JSON value\n{}",
            manifest_run.describe()
        )
    })?;
    let name = manifest["name"]
        .as_str()
        .ok_or_else(|| format!("expected a `name` in what `manifest` printed: {manifest}"))?
        .to_owned();
    let bundle_run = run_tooling("bundle", dir);
    if bundle_run.code != Some(0) {
        return Err(format!(
            "expected `bundle` to exit 0 on {}\n{}",
            dir.display(),
            bundle_run.describe()
        ));
    }
    if bundle_run.stdout.trim().is_empty() {
        return Err(format!(
            "expected `bundle` to print the bundle\n{}",
            bundle_run.describe()
        ));
    }
    Ok(Built {
        name,
        declared_access: manifest["declared_access"].clone(),
        manifest,
        bundle: bundle_run.stdout,
        migrations: migrations_in(&dir.join("migrations")),
    })
}

/// The `*.sql` files of `dir` in name order, each a [`Migration`] named by
/// its file name without `.sql`. None when there is no such directory.
pub fn migrations_in(dir: &Path) -> Vec<Migration> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sql = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            Migration { name, sql }
        })
        .collect()
}

/// The body of a Version of this built project: its bundle, the
/// `declared_access` the `manifest` command printed, unchanged,
/// [`SOME_GIT_COMMIT`], and its migrations.
pub fn version_body_of(built: &Built) -> Value {
    json!({
        "bundle": built.bundle,
        "declared_access": built.declared_access,
        "git_commit": SOME_GIT_COMMIT,
        "migrations": migrations_json(&built.migrations),
    })
}

/// Creates the Application of `built` (with the Manifest's name and
/// description) as [`INSTALLER`] and sends its Version, failing the test
/// with the program's log unless both are `201`. Then gives its own
/// database the TimeZone [`OWN_TIME_ZONE`] and the DateStyle `ISO, MDY`,
/// before any request can open a connection to it, and returns it.
pub fn install(vm: &Vm, built: &Built) -> OwnDatabase {
    let api = vm.api_as(&User::new(INSTALLER));
    let created = api.create_application_with(&json!({
        "name": built.name,
        "description": built.manifest["description"],
    }));
    if created.status != 201 {
        vm.fail(&format!(
            "expected `201` creating the Application {:?} named by the Manifest, got\n{}",
            built.name,
            created.describe()
        ));
    }
    let sent = api
        .request(&format!("/v1/applications/{}/versions", built.name))
        .method("POST")
        .json(&version_body_of(built))
        .timeout(Duration::from_secs(60))
        .send();
    if sent.status != 201 {
        vm.fail(&format!(
            "expected `201` sending the built project {:?} as a Version (its bundle, the declared_access `manifest` printed, its migrations), got\n{}",
            built.name,
            sent.describe()
        ));
    }
    let own = OwnDatabase::find(vm.platform(), &built.name).unwrap_or_else(|| {
        vm.fail(&format!(
            "expected an own database for {:?} after its first Version",
            built.name
        ))
    });
    set_time_zone(&own.name, OWN_TIME_ZONE);
    own
}

/// `ALTER DATABASE <database> SET timezone, datestyle` on the test Postgres
/// that holds the own databases. Connections opened afterwards get them.
pub fn set_time_zone(database: &str, zone: &str) {
    let superuser = Sql::connect(&shared().url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
    superuser.batch(&format!(
        "ALTER DATABASE {db} SET timezone = '{zone}'; ALTER DATABASE {db} SET datestyle = 'ISO, MDY';",
        db = ident(database)
    ));
}

/// What the SDK tests add to the shared database of
/// [`crate::shared_database`]: `teams` and `services` (for a join and a
/// relational query) and `vals` (one column of each type of CONTRACT.md's
/// table "Values", the same as the fixture's own `vals`), readable by the
/// read and the write login. `%READER%` and `%WRITER%` are replaced first.
pub const SHARED_SETUP: &str = r#"
CREATE TABLE teams (id int4 PRIMARY KEY, name text NOT NULL);
CREATE TABLE services (id int4 PRIMARY KEY, name text NOT NULL, team_id int4 NOT NULL REFERENCES teams (id), tier int2 NOT NULL);
INSERT INTO teams VALUES (1, 'Platform'), (2, 'Payments');
INSERT INTO services VALUES (10, 'db-7', 1, 1), (11, 'checkout', 2, 2), (12, 'ledger', 2, 1);
CREATE TABLE vals (
  id serial PRIMARY KEY,
  label text NOT NULL UNIQUE,
  i4 integer, i2 smallint, big bigint, big_num bigint, num numeric, r4 real, f8 double precision,
  flag boolean, t text, vc varchar(64), u uuid,
  ts timestamp(6), tstz timestamp(6) with time zone, ts_str timestamp(6), tstz_str timestamp(6) with time zone,
  d date, j json, jb jsonb, texts text[], ints integer[]
);
INSERT INTO vals (label, i4, i2, big, big_num, num, r4, f8, flag, t, vc, u, ts, tstz, ts_str, tstz_str, d, j, jb, texts, ints) VALUES
  ('every', 2147483647, -32768, 9007199254740993, 9007199254740991, 12345678901234567890.012345678901234567890, 1.5, 3.141592653589793, true,
   'héllo ☃ "quoted" \ back', 'varchar value', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
   '2026-01-02 03:04:05.678412', '2026-01-02 03:04:05.678412+00', '2026-01-02 03:04:05.678412', '2026-01-02 03:04:05.678412+00', '2026-01-02',
   '{"a": [1, "x", null], "b": "123"}', '"123"',
   ARRAY['a,b', 'q"uote', 'back\slash', '{brace}', '', NULL, 'NULL', ' spaced '], ARRAY[1, NULL, -3]);
INSERT INTO vals (label) VALUES ('nulls');
INSERT INTO vals (label, j, jb, texts, ints) VALUES
  ('json-number', '123', '123', '{}', '{}'),
  ('json-bool', 'false', 'true', NULL, NULL),
  ('json-null', 'null', 'null', NULL, NULL),
  ('json-nested', '[null, "123", {}]', '{"k": null, "s": "123", "e": []}', NULL, NULL);
GRANT SELECT ON teams, services, vals TO %READER%, %WRITER%;
"#;

/// The User most SDK tests send: `read` on `devops` (through
/// [`READERS`]) and `write` on `ops` (through [`WRITERS`]).
pub fn ann() -> User {
    User::new("ann").in_groups(&[READERS, WRITERS])
}

/// A User whose Groups hold no Grant on anything.
pub fn nobody() -> User {
    User::new("nobody").in_groups(&["no-access"])
}

/// A program with a fixture project installed, and for `board` the shared
/// database it reads and writes.
pub struct Sdk {
    pub scenario: Scenario,
    /// The installed Application's name.
    pub application: String,
    pub own: OwnDatabase,
}

impl Sdk {
    /// The program, a shared database with [`SHARED_SETUP`] in the
    /// TimeZone [`SHARED_TIME_ZONE`], registered as `devops` and as [`OPS`]
    /// (each with its read and write login), [`READERS`] granted `read` on
    /// `devops` and [`WRITERS`] `write` on `ops`, and `fixtures/sdk/board`
    /// installed.
    pub fn board() -> Self {
        Self::with_shared("board")
    }

    /// As [`Sdk::board`], with the fixture project `fixtures/sdk/<name>`
    /// installed instead: one that declares `devops` and [`OPS`] as
    /// `board` does.
    pub fn with_shared(name: &str) -> Self {
        let built = built(name);
        let scenario = Scenario::start();
        scenario.db.superuser().batch(
            &SHARED_SETUP
                .replace("%READER%", &scenario.db.reader.role)
                .replace("%WRITER%", &scenario.db.writer.role),
        );
        scenario.db.superuser().batch(&format!(
            "ALTER DATABASE {} SET timezone = '{SHARED_TIME_ZONE}'",
            ident(scenario.db.name())
        ));
        scenario.register_writable(crate::scenario::RESOURCE);
        scenario.register_writable(OPS);
        scenario.grant(crate::scenario::RESOURCE, READERS, "read");
        scenario.grant(OPS, WRITERS, "write");
        let own = install(&scenario.vm, &built);
        Self {
            application: built.name.clone(),
            scenario,
            own,
        }
    }

    /// The program with `fixtures/<name>` installed and nothing registered.
    pub fn only(name: &str) -> Self {
        let built = built(name);
        let scenario = Scenario::start_with(&VmOptions::new());
        let own = install(&scenario.vm, &built);
        Self {
            application: built.name.clone(),
            scenario,
            own,
        }
    }

    pub fn vm(&self) -> &Vm {
        &self.scenario.vm
    }

    /// A `GET` of `path` on the installed Application as `user`.
    pub fn get(&self, user: &User, path: &str) -> Response {
        self.vm()
            .request(&self.application, path)
            .as_user(user)
            .timeout(SDK_REQUEST_TIMEOUT)
            .send()
    }

    /// As [`Sdk::get`], failing the test unless the answer is `200` with a
    /// JSON body; returns both.
    pub fn get_ok(&self, user: &User, path: &str) -> (Response, Value) {
        let response = self.get(user, path);
        if response.status != 200 {
            self.vm().fail(&format!(
                "expected `200` with JSON from {:?} for GET {path} as {:?}, got\n{}",
                self.application,
                user.id,
                response.describe()
            ));
        }
        let body = response.json();
        (response, body)
    }

    /// Every entry of the Operation log.
    pub fn log(&self) -> Vec<Value> {
        self.scenario.log()
    }

    /// The Operation log entries of the request this answers, oldest first.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        self.scenario.entries_of(response)
    }

    /// The platform's API as the Platform administrator.
    pub fn admin_api(&self) -> crate::api::Api<'_> {
        self.vm().api_as(&administrator())
    }
}

/// What one Operation log entry of an SDK statement must say.
#[derive(Clone, Copy, Debug)]
pub struct Op<'a> {
    /// `query` or `execute`.
    pub operation: &'a str,
    /// The Resource; `None` for the own database.
    pub resource: Option<&'a str>,
    /// `user` or `application`.
    pub actor: &'a str,
}

/// A statement on a shared Resource as the User.
pub fn shared_op<'a>(operation: &'a str, resource: &'a str) -> Op<'a> {
    Op {
        operation,
        resource: Some(resource),
        actor: "user",
    }
}

/// A statement on the own database as `actor`.
pub fn own_op<'a>(operation: &'a str, actor: &'a str) -> Op<'a> {
    Op {
        operation,
        resource: None,
        actor,
    }
}

/// Whether `sql` holds a constant written into its text (a quoted string or
/// a number standing outside a quoted identifier and outside `$n`): the
/// Operation log records such a statement with the constant replaced, so
/// it cannot be compared with the text Drizzle built.
pub fn has_constants(sql: &str) -> bool {
    let mut in_identifier = false;
    let mut previous: Option<char> = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if in_identifier {
            if c == '"' {
                in_identifier = false;
            }
        } else if c == '"' {
            in_identifier = true;
        } else if c == '\'' {
            return true;
        } else if c.is_ascii_alphabetic()
            && !previous.is_some_and(|p| p.is_ascii_alphanumeric() || p == '_' || p == '$')
        {
            // A word: `true`, `false` and `null` are constants.
            let mut word = String::from(c);
            while let Some(&n) = chars.peek() {
                if !(n.is_ascii_alphanumeric() || n == '_') {
                    break;
                }
                word.push(n);
                chars.next();
            }
            if ["true", "false", "null"].contains(&word.to_ascii_lowercase().as_str()) {
                return true;
            }
            previous = word.chars().last();
            continue;
        } else if c.is_ascii_digit() {
            let glued = previous.is_some_and(|p| p == '$' || p.is_ascii_alphanumeric() || p == '_');
            if !glued {
                return true;
            }
            // The rest of `$12` or of a name.
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
        previous = Some(c);
    }
    false
}

/// Fails unless the request `response` answers has exactly one Operation
/// log entry per statement the fixture reported in `body["statements"]`
/// (the SQL Drizzle built), in that order, each saying `want`'s operation,
/// Resource and actor, `allowed` for `user` in `application` (Version 1),
/// with the statement Drizzle built when it holds no constant, and with no
/// parameter's value anywhere in it.
pub fn assert_one_entry_per_statement(
    sdk: &Sdk,
    response: &Response,
    body: &Value,
    user: &User,
    want: &[Op<'_>],
    context: &str,
) {
    let statements = body["statements"].as_array().cloned().unwrap_or_else(|| {
        panic!("{context}: expected `statements` in the fixture's answer, got {body}")
    });
    assert_eq!(
        statements.len(),
        want.len(),
        "{context}: the fixture reports {} statements, the test expects {}: {}",
        statements.len(),
        want.len(),
        lines(&statements)
    );
    let entries = sdk.entries_of(response);
    assert_eq!(
        entries.len(),
        statements.len(),
        "{context}: expected exactly one Operation log entry per statement Drizzle sent ({}), got {}.\nstatements:\n{}entries:\n{}",
        statements.len(),
        entries.len(),
        lines(&statements),
        lines(&entries)
    );
    let request = response.request_id();
    for (index, ((statement, entry), op)) in statements.iter().zip(&entries).zip(want).enumerate() {
        let sql = statement["sql"].as_str().unwrap_or_default();
        let exact = (!has_constants(sql)).then_some(sql);
        let expected = crate::scenario::Expected {
            request: Some(&request),
            user: Some(&user.id),
            application: &sdk.application,
            version: 1,
            resource: op.resource,
            actor: Some(op.actor),
            own_database: Some(op.resource.is_none()),
            operation: op.operation,
            statement: exact,
            outcome: "allowed",
            reason: None,
            error: None,
            ran: Some(true),
            rows: None,
        };
        assert_entry(
            entry,
            &expected,
            &format!("{context}, statement {index} ({sql})"),
        );
        assert_entry_holds_no_parameter(entry, statement, &format!("{context}, statement {index}"));
    }
}

/// Fails if the Operation log `entry` holds the value of a parameter of
/// `statement` (as the fixture reported it: strings and BigInts of four
/// characters or more). A value that is exactly one of the entry's own
/// fields (an `actor` of `user` passed as a parameter, say) is looked for
/// only in its `statement`.
pub fn assert_entry_holds_no_parameter(entry: &Value, statement: &Value, context: &str) {
    let own_values: Vec<&str> = entry
        .as_object()
        .map(|o| o.values().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let text = entry.to_string();
    let recorded = entry["statement"].as_str().unwrap_or_default();
    for param in statement["params"].as_array().into_iter().flatten() {
        let value = match param["type"].as_str() {
            Some("string") => param["value"].as_str(),
            Some("bigint") => param["text"].as_str(),
            _ => None,
        };
        let Some(value) = value.filter(|v| v.len() >= 4) else {
            continue;
        };
        let leaked = if own_values.contains(&value) {
            recorded.contains(value)
        } else {
            text.contains(value)
        };
        assert!(
            !leaked,
            "{context}: the Operation log entry holds the parameter {value:?}: {entry}"
        );
    }
}

/// A project written by a test, in a directory of its own under the
/// repository's `target/`, removed when dropped.
pub struct TempProject {
    pub dir: PathBuf,
}

impl TempProject {
    /// An empty project directory.
    pub fn new(label: &str) -> Self {
        let dir = repository_root()
            .join("target")
            .join("blackbox-sdk-projects")
            .join(format!("{label}-{}", crate::shared_database::token()));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        Self { dir }
    }

    /// Writes `text` to `relative` inside the project, making directories.
    pub fn write(&self, relative: &str, text: &str) -> &Self {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
        }
        std::fs::write(&path, text)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        self
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    /// `manifest` on this project.
    pub fn manifest(&self) -> Tooled {
        run_tooling("manifest", &self.dir)
    }

    /// `bundle` on this project.
    pub fn bundle(&self) -> Tooled {
        run_tooling("bundle", &self.dir)
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.dir));
    }
}

/// The `tsconfig.json` CONTRACT.md gives a project.
pub const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true,
    "paths": { "@vm/own-structure": ["./src/db/schema.ts"] }
  },
  "include": ["manifest.ts", "src", "resources"]
}
"#;

/// Descriptions of values as `fixtures/sdk/board/src/describe.ts` gives
/// them, to compare with.
pub mod d {
    use serde_json::{Map, Value, json};

    pub fn null() -> Value {
        json!({ "type": "null" })
    }

    pub fn string(value: &str) -> Value {
        json!({ "type": "string", "value": value })
    }

    pub fn boolean(value: bool) -> Value {
        json!({ "type": "boolean", "value": value })
    }

    /// A number, by the text JavaScript's `String(n)` gives it.
    pub fn number(text: &str) -> Value {
        let value: Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("not a JSON number {text:?}: {e}"));
        json!({ "type": "number", "value": value, "text": text })
    }

    pub fn bigint(text: &str) -> Value {
        json!({ "type": "bigint", "text": text })
    }

    /// A `Date`, by its `toISOString()`.
    pub fn date(iso: &str) -> Value {
        json!({ "type": "date", "iso": iso })
    }

    pub fn array(items: &[Value]) -> Value {
        json!({ "type": "array", "items": items })
    }

    pub fn object(entries: &[(&str, Value)]) -> Value {
        let map: Map<String, Value> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        json!({ "type": "object", "entries": map })
    }

    /// An array of strings, `None` for a null element.
    pub fn strings(items: &[Option<&str>]) -> Value {
        array(
            &items
                .iter()
                .map(|i| i.map_or_else(null, string))
                .collect::<Vec<_>>(),
        )
    }

    /// An array of whole numbers, `None` for a null element.
    pub fn numbers(items: &[Option<i64>]) -> Value {
        array(
            &items
                .iter()
                .map(|i| i.map_or_else(null, |n| number(&n.to_string())))
                .collect::<Vec<_>>(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_told_from_parameters_and_names() {
        assert!(!has_constants(
            r#"select "id", "title" from "incidents" where "incidents"."open" = $1 limit $2"#
        ));
        assert!(!has_constants(r#"select "t2"."x1" from "t2""#));
        assert!(has_constants(r#"select * from "t" where "x" = 'a'"#));
        assert!(has_constants(r#"select coalesce("a", 0) from "t""#));
        assert!(has_constants(r#"select "a" + 1 from "t""#));
        assert!(has_constants(
            r#"select "a" from "t" left join lateral (select 1) "x" on true"#
        ));
        assert!(!has_constants(
            r#"select "trueish", "t1"."nullable" from "t1""#
        ));
        assert!(!has_constants(
            r#"insert into "notes" ("id", "body") values (default, $1)"#
        ));
    }
}
