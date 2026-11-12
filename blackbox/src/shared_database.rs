//! A shared database for the tests to register as a Resource (CONTRACT.md,
//! "Resources and Grants", "Operations on a shared database", "Writing to a
//! shared database, and the role as the boundary"): a database of its own
//! on one of the harness's Postgres instances, with a few tables of varied
//! types, a login that can only read it, a login that can also write, and a
//! careless "read" login that could in fact write, as a Platform
//! administrator might mistakenly register.
//!
//! Its owner (the harness, standing for IT) has also put in it what a real
//! shared database may hold and a hostile Application may call: a SECURITY
//! DEFINER function that writes ([`CLOSE_EVERY_INCIDENT`]), a function that
//! leaves state in its session ([`LEAVE_SESSION_STATE`]), and a trigger
//! that copies the identity label into each row of `notes`.
//!
//! Its name, its logins' names and passwords are unmistakable markers, so a
//! test can search everything the program said for them. Statement logging
//! is on for this database alone, so a test can read from the server's own
//! log what this database received, and show that a statement was not sent.

use std::fmt;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::postgres::{self, DISTINCT_LOOPBACK, PrivatePostgres};
use crate::sql::Sql;

/// A random lower-case hex token, different on every call.
pub fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

/// A login to a shared database. Its password is never printed.
pub struct Login {
    pub role: String,
    password: String,
}

impl Login {
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Login({:?}, password not shown)", self.role)
    }
}

/// The value in `incidents.title` of the row with id 3: a value from a row,
/// unmistakable, that must never reach the Operation log or the program's
/// log.
pub const ROW_MARKER_PREFIX: &str = "row-marker-";

/// A shared database on one of the harness's instances.
pub struct SharedDatabase {
    server: &'static PrivatePostgres,
    name: String,
    /// The login that can only read.
    pub reader: Login,
    /// A login registered as "read" that can in fact write: shows that a
    /// read runs in a read-only transaction whatever the login may do.
    pub careless: Login,
    /// The login that can read and write `incidents`, `kinds` and `notes`
    /// (not `staff_only`), and create temporary tables, as IT would hand
    /// over for a Resource's `write` login.
    pub writer: Login,
    /// The title of incident 3.
    pub row_marker: String,
}

impl fmt::Debug for SharedDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedDatabase({:?})", self.name)
    }
}

/// The setup of every shared database, run by the instance's superuser in
/// the new database. `%READER%`, `%CARELESS%`, `%WRITER%` and
/// `%ROW_MARKER%` are replaced first.
const SETUP: &str = r#"
CREATE TABLE incidents (
  id int4 PRIMARY KEY,
  title text NOT NULL,
  open bool NOT NULL,
  severity int2 NOT NULL,
  reported_at timestamptz NOT NULL
);
INSERT INTO incidents VALUES
  (1, 'Disk full on db-7', true, 2, '2026-03-01 09:30:00+00'),
  (2, 'Login page slow', false, 1, '2026-03-02 14:00:00+00'),
  (3, '%ROW_MARKER%', true, 3, '2026-03-03 08:15:00+00');

CREATE TABLE kinds (
  label text PRIMARY KEY,
  n int4, b bool, i2 int2, i4 int4, i8 int8, f4 float4, f8 float8,
  num numeric, t text, ts timestamptz, u uuid, jb jsonb, j json
);
INSERT INTO kinds VALUES
  ('every', NULL, true, -12, 2147483647, 9007199254740993, 1.5, -0.25,
   12345678901234567890.0123, 'héllo ☃ "quoted" \ back',
   '2026-01-02 03:04:05.678+00', 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11',
   '{"b": [1, 2.5, null], "a": "x"}', '{"k": true}'),
  ('nulls', NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE staff_only (note text);
INSERT INTO staff_only VALUES ('not for readers');

CREATE SEQUENCE ticket_numbers;

-- What the write tests write: who wrote each row (the login, by default),
-- and the identity label as the owner's own trigger reads it.
CREATE TABLE notes (
  id serial PRIMARY KEY,
  body text NOT NULL,
  written_by text NOT NULL DEFAULT current_user,
  session_login text NOT NULL DEFAULT session_user,
  label_user text,
  label_application text,
  label_request text
);
CREATE FUNCTION notes_label() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
  NEW.label_user := current_setting('vm.user', true);
  NEW.label_application := current_setting('vm.application', true);
  NEW.label_request := current_setting('vm.request', true);
  RETURN NEW;
END
$f$;
CREATE TRIGGER notes_label BEFORE INSERT OR UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION notes_label();

-- IT's function that writes with its owner's rights, callable by anyone
-- (as `hr.give_raise()` in docs/research/what-postgres-sees.md).
CREATE FUNCTION close_every_incident() RETURNS int4 LANGUAGE sql SECURITY DEFINER AS $f$
  UPDATE incidents SET open = false;
  INSERT INTO staff_only VALUES ('written by close_every_incident');
  SELECT count(*)::int4 FROM incidents;
$f$;

-- A function that leaves in its session everything a session can hold.
-- Each step that the transaction does not allow (a temporary table in a
-- read-only one) is skipped. The advisory lock is taken without waiting,
-- so that a session that kept it never blocks another.
CREATE FUNCTION leave_session_state() RETURNS int4 LANGUAGE plpgsql AS $f$
BEGIN
  BEGIN EXECUTE 'CREATE TEMP TABLE left_table (x int)'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'PREPARE left_prepared AS SELECT 1'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'DECLARE left_cursor CURSOR WITH HOLD FOR SELECT 1'; EXCEPTION WHEN others THEN NULL; END;
  BEGIN EXECUTE 'LISTEN left_channel'; EXCEPTION WHEN others THEN NULL; END;
  PERFORM pg_try_advisory_lock(71, 43);
  PERFORM set_config('search_path', 'left_schema, public', false);
  PERFORM set_config('statement_timeout', '4321ms', false);
  PERFORM set_config('work_mem', '7777kB', false);
  PERFORM set_config('leftover.setting', 'left-behind', false);
  RETURN pg_backend_pid();
END
$f$;

GRANT USAGE ON SCHEMA public TO %READER%, %CARELESS%, %WRITER%;
GRANT SELECT ON incidents, kinds, notes TO %READER%;
GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON incidents, kinds, notes TO %CARELESS%;
GRANT USAGE, UPDATE ON SEQUENCE ticket_numbers TO %CARELESS%;
GRANT USAGE ON SEQUENCE notes_id_seq TO %CARELESS%, %WRITER%;
GRANT CREATE ON SCHEMA public TO %CARELESS%;
GRANT SELECT, INSERT, UPDATE, DELETE ON incidents, kinds, notes TO %WRITER%;
"#;

/// A call of IT's SECURITY DEFINER function that closes every incident and
/// writes a row into `staff_only`: a write no read may make.
pub const CLOSE_EVERY_INCIDENT: &str = "close_every_incident()";

/// A call of IT's function that leaves in its session a temporary table
/// (where the transaction allows), a prepared statement, a cursor `WITH
/// HOLD`, a `LISTEN`, a session advisory lock (71, 43), and the settings
/// [`LEFT_SEARCH_PATH`], [`LEFT_STATEMENT_TIMEOUT`], [`LEFT_WORK_MEM`] and
/// `leftover.setting` = [`LEFT_SETTING`]. It answers its backend's pid.
pub const LEAVE_SESSION_STATE: &str = "leave_session_state()";

/// The values [`LEAVE_SESSION_STATE`] sets.
pub const LEFT_SEARCH_PATH: &str = "left_schema, public";
pub const LEFT_STATEMENT_TIMEOUT: &str = "4321ms";
pub const LEFT_WORK_MEM: &str = "7777kB";
pub const LEFT_SETTING: &str = "left-behind";

/// A SQL expression, valid in any `SELECT` list, that says as JSON text
/// what the session it runs in holds: its backend's `pid`, `current_user`,
/// `session_user`, whether the transaction is `read_only`, the settings
/// [`LEAVE_SESSION_STATE`] sets, how many temporary tables, prepared
/// statements, cursors and channels named `left...` it has, how many
/// advisory locks it holds, and the identity label (`vm_user`,
/// `vm_application`, `vm_request`, `vm_actor`; null when unset). See
/// [`Session`].
pub const OBSERVE_SESSION: &str = "json_build_object(
  'pid', pg_backend_pid(),
  'current_user', current_user::text,
  'session_user', session_user::text,
  'read_only', current_setting('transaction_read_only'),
  'search_path', current_setting('search_path'),
  'statement_timeout', current_setting('statement_timeout'),
  'work_mem', current_setting('work_mem'),
  'leftover_setting', nullif(current_setting('leftover.setting', true), ''),
  'temp_tables', (SELECT count(*) FROM pg_class WHERE relnamespace = pg_my_temp_schema()),
  'prepared', (SELECT count(*) FROM pg_prepared_statements WHERE name LIKE 'left%'),
  'cursors', (SELECT count(*) FROM pg_cursors WHERE name LIKE 'left%'),
  'listening', (SELECT count(*) FROM pg_listening_channels() c WHERE c LIKE 'left%'),
  'advisory_locks', (SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND pid = pg_backend_pid()),
  'vm_user', nullif(current_setting('vm.user', true), ''),
  'vm_application', nullif(current_setting('vm.application', true), ''),
  'vm_request', nullif(current_setting('vm.request', true), ''),
  'vm_actor', nullif(current_setting('vm.actor', true), '')
)::text";

/// A `SELECT` that answers one row with one column `session`: what its
/// session holds ([`OBSERVE_SESSION`]), after sleeping `pause_ms` (so that
/// observations made together overlap and each takes a connection of its
/// own). For `query`.
pub fn observe_select(pause_ms: u32) -> String {
    format!(
        "select {OBSERVE_SESSION} as session from pg_sleep({})",
        f64::from(pause_ms) / 1000.0
    )
}

/// An `INSERT ... RETURNING` into `notes` that answers one row with one
/// column `session`: what its session holds ([`OBSERVE_SESSION`]), after
/// sleeping `pause_ms`. For `execute`, whose rows are what `RETURNING`
/// gives.
pub fn observe_insert(pause_ms: u32) -> String {
    format!(
        "insert into notes (body) select {OBSERVE_SESSION} from pg_sleep({}) returning body as session",
        f64::from(pause_ms) / 1000.0
    )
}

/// What [`OBSERVE_SESSION`] said, read from its JSON text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub pid: i64,
    pub current_user: String,
    pub session_user: String,
    pub read_only: String,
    pub search_path: String,
    pub statement_timeout: String,
    pub work_mem: String,
    pub leftover_setting: Option<String>,
    pub temp_tables: i64,
    pub prepared: i64,
    pub cursors: i64,
    pub listening: i64,
    pub advisory_locks: i64,
    pub vm_user: Option<String>,
    pub vm_application: Option<String>,
    pub vm_request: Option<String>,
    /// `vm.actor` (CONTRACT.md, "Operations on the own database").
    pub vm_actor: Option<String>,
}

impl Session {
    /// Reads the JSON text [`OBSERVE_SESSION`] gives; `None` if it is not
    /// that.
    pub fn parse(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        let n = |k: &str| v[k].as_i64();
        Some(Self {
            pid: n("pid")?,
            current_user: s("current_user")?,
            session_user: s("session_user")?,
            read_only: s("read_only")?,
            search_path: s("search_path")?,
            statement_timeout: s("statement_timeout")?,
            work_mem: s("work_mem")?,
            leftover_setting: s("leftover_setting"),
            temp_tables: n("temp_tables")?,
            prepared: n("prepared")?,
            cursors: n("cursors")?,
            listening: n("listening")?,
            advisory_locks: n("advisory_locks")?,
            vm_user: s("vm_user"),
            vm_application: s("vm_application"),
            vm_request: s("vm_request"),
            vm_actor: s("vm_actor"),
        })
    }

    /// Reads the session out of an answer `{ ok: { rows: [[<json text>]] } }`
    /// to [`observe_select`] or [`observe_insert`]; `None` if it is not one.
    pub fn from_answer(answer: &Value) -> Option<Self> {
        answer["ok"]["rows"][0][0].as_str().and_then(Self::parse)
    }

    /// What of [`LEAVE_SESSION_STATE`]'s leavings (and of any advisory lock)
    /// this session still holds, by name; empty when none.
    pub fn leftovers(&self) -> Vec<String> {
        let mut found = Vec::new();
        if self.search_path == LEFT_SEARCH_PATH {
            found.push(format!("search_path = {:?}", self.search_path));
        }
        if self.statement_timeout == LEFT_STATEMENT_TIMEOUT {
            found.push(format!("statement_timeout = {:?}", self.statement_timeout));
        }
        if self.work_mem == LEFT_WORK_MEM {
            found.push(format!("work_mem = {:?}", self.work_mem));
        }
        if self.leftover_setting.is_some() {
            found.push(format!("leftover.setting = {:?}", self.leftover_setting));
        }
        for (what, count) in [
            ("temporary tables", self.temp_tables),
            ("prepared statements named left...", self.prepared),
            ("cursors named left...", self.cursors),
            ("channels listened to named left...", self.listening),
            ("advisory locks", self.advisory_locks),
        ] {
            if count != 0 {
                found.push(format!("{count} {what}"));
            }
        }
        found
    }
}

impl SharedDatabase {
    /// A new shared database on the instance for shared databases
    /// ([`postgres::for_shared_databases`]).
    pub fn create() -> Self {
        Self::create_on(postgres::for_shared_databases())
    }

    /// A new shared database on this instance.
    pub fn create_on(server: &'static PrivatePostgres) -> Self {
        let token = token();
        let name = format!("shared_db_{token}");
        let reader = Login {
            role: format!("reader_role_{token}"),
            password: format!("pw-reader-{}{}", token, self::token()),
        };
        let careless = Login {
            role: format!("careless_role_{token}"),
            password: format!("pw-careless-{}{}", token, self::token()),
        };
        let writer = Login {
            role: format!("writer_role_{token}"),
            password: format!("pw-writer-{}{}", token, self::token()),
        };
        let row_marker = format!("{ROW_MARKER_PREFIX}{token}");
        let superuser = Sql::connect(&server.url("postgres"))
            .unwrap_or_else(|e| panic!("cannot reach the Postgres for shared databases: {e}"));
        superuser.batch(&format!(
            "CREATE ROLE {r} LOGIN PASSWORD '{rp}';
             CREATE ROLE {c} LOGIN PASSWORD '{cp}';
             CREATE ROLE {w} LOGIN PASSWORD '{wp}';",
            r = reader.role,
            rp = reader.password,
            c = careless.role,
            cp = careless.password,
            w = writer.role,
            wp = writer.password,
        ));
        superuser.batch(&format!("CREATE DATABASE {name}"));
        superuser.batch(&format!(
            "ALTER DATABASE {name} SET log_statement = 'all';
             ALTER DATABASE {name} SET timezone = 'UTC';
             ALTER DATABASE {name} SET datestyle = 'ISO, MDY';
             REVOKE ALL ON DATABASE {name} FROM PUBLIC;
             GRANT CONNECT, TEMPORARY ON DATABASE {name} TO {r}, {c}, {w};",
            r = reader.role,
            c = careless.role,
            w = writer.role,
        ));
        drop(superuser);
        let database = Self {
            server,
            name,
            reader,
            careless,
            writer,
            row_marker,
        };
        let setup = SETUP
            .replace("%READER%", &database.reader.role)
            .replace("%CARELESS%", &database.careless.role)
            .replace("%WRITER%", &database.writer.role)
            .replace("%ROW_MARKER%", &database.row_marker);
        database.superuser().batch(&setup);
        database
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The host a Resource's connection names.
    pub fn host(&self) -> String {
        self.server.host()
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// Whether the host is unmistakable (not 127.0.0.1), so that a test can
    /// search for it.
    pub fn host_is_distinct(&self) -> bool {
        self.host() == DISTINCT_LOOPBACK.to_string()
    }

    /// The `connection` of a Resource registered with this login as its
    /// `read` login.
    pub fn connection(&self, login: &Login) -> Value {
        json!({
            "host": self.host(),
            "port": self.port(),
            "database": self.name,
            "read": { "role": login.role, "password": login.password },
        })
    }

    /// The `connection` of a Resource registered with these `read` and
    /// `write` logins.
    pub fn connection_with_write(&self, read: &Login, write: &Login) -> Value {
        let mut connection = self.connection(read);
        connection["write"] = json!({ "role": write.role, "password": write.password });
        connection
    }

    /// A connection address to this database as `login`.
    pub fn url_as(&self, login: &Login) -> String {
        self.server.url_as(&login.role, &login.password, &self.name)
    }

    /// A connection of the harness's own to this database as `login`: to
    /// show what the login itself may do.
    pub fn connect_as(&self, login: &Login) -> Sql {
        Sql::connect(&self.url_as(login)).unwrap_or_else(|e| {
            panic!(
                "cannot reach the shared database {} as {}: {e}",
                self.name, login.role
            )
        })
    }

    /// The instance's superuser, connected to this database: to look at it
    /// from outside the program.
    pub fn superuser(&self) -> Sql {
        Sql::connect(&self.server.url(&self.name))
            .unwrap_or_else(|e| panic!("cannot reach the shared database {}: {e}", self.name))
    }

    /// The entries of the server's log about this database, in order: each
    /// statement it received (`statement:` or `execute ...:`, with its
    /// parameters), each error, each connection. An entry is one prefixed
    /// line and the unprefixed lines that continue it.
    pub fn server_log(&self) -> Vec<String> {
        let mine = format!(" db={} ", self.name);
        let mut entries: Vec<String> = Vec::new();
        for line in self.server.server_log().lines() {
            let starts_entry = line.as_bytes().first().is_some_and(u8::is_ascii_digit);
            match entries.last_mut() {
                Some(last) if !starts_entry => {
                    last.push('\n');
                    last.push_str(line);
                }
                _ => entries.push(line.to_owned()),
            }
        }
        entries.retain(|e| e.contains(&mine));
        entries
    }

    /// The statements this database received, as logged.
    pub fn statements(&self) -> Vec<String> {
        self.server_log()
            .into_iter()
            .filter(|e| e.contains("LOG:  statement: ") || e.contains("LOG:  execute "))
            .collect()
    }

    /// Whether any entry of the server's log about this database contains
    /// `text`: a statement received, its parameters, or an error about it.
    pub fn received(&self, text: &str) -> bool {
        self.server_log().iter().any(|e| e.contains(text))
    }

    /// Waits up to `timeout` for the server's log to show `text` received.
    /// The server writes the line when the statement starts, so a short
    /// wait suffices.
    pub fn wait_until_received(&self, text: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.received(text) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Fails unless the server's log shows `text` received: the check that
    /// statement logging works, before a test relies on something being
    /// absent from it.
    pub fn assert_received(&self, text: &str, context: &str) {
        assert!(
            self.wait_until_received(text, Duration::from_secs(5)),
            "{context}: expected the shared database {} to have received a statement containing {text:?}, but its server log holds no such entry. Its statements:\n{}",
            self.name,
            self.statements().join("\n")
        );
    }

    /// Fails if the server's log shows `text` received.
    pub fn assert_not_received(&self, text: &str, context: &str) {
        let found: Vec<String> = self
            .server_log()
            .into_iter()
            .filter(|e| e.contains(text))
            .collect();
        assert!(
            found.is_empty(),
            "{context}: expected nothing containing {text:?} to reach the shared database {}, but its server log shows:\n{}",
            self.name,
            found.join("\n")
        );
    }

    /// Everything a statement could change, as one string: the rows of
    /// every table, the relations in every schema and their privileges, the
    /// sequence, the large objects, the schemas, the functions, the
    /// settings of the database and its roles, the roles' attributes and
    /// memberships.
    pub fn fingerprint(&self) -> String {
        let sql = format!(
            "SELECT concat_ws(' # ',
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t.id), '') FROM incidents t),
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t.label), '') FROM kinds t),
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t::text), '') FROM staff_only t),
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t.id), '') FROM notes t),
               (SELECT 'large objects: ' || count(*) FROM pg_largeobject_metadata),
               (SELECT string_agg(n.nspname, ',' ORDER BY n.nspname) FROM pg_namespace n
                 WHERE n.nspname NOT LIKE 'pg_temp%' AND n.nspname NOT LIKE 'pg_toast%'),
               (SELECT string_agg(p.proname || ':' || md5(p.prosrc), ',' ORDER BY p.proname, p.oid)
                  FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                 WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')),
               (SELECT string_agg(c.relname || ':' || coalesce(array_to_string(c.relacl, ';'), ''), ','
                                  ORDER BY c.relname)
                  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = 'public'),
               (SELECT string_agg(n.nspname || '.' || c.relname || ':' || c.relkind::text, ','
                                  ORDER BY n.nspname, c.relname)
                  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
                   AND n.nspname NOT LIKE 'pg_toast%'
                   AND n.nspname NOT LIKE 'pg_temp%'),
               (SELECT last_value::text || is_called::text FROM ticket_numbers),
               (SELECT coalesce(string_agg(array_to_string(setconfig, ';'), ',' ORDER BY setrole, setdatabase), '')
                  FROM pg_db_role_setting
                 WHERE setdatabase IN (0, (SELECT oid FROM pg_database WHERE datname = current_database()))
                   AND (setrole = 0 OR setrole IN (SELECT oid FROM pg_roles WHERE rolname IN ('{r}', '{c}', '{w}')))),
               (SELECT string_agg(rolname || rolsuper::text || rolcreaterole::text || rolcreatedb::text
                                  || coalesce(array_to_string(rolconfig, ';'), ''), ',' ORDER BY rolname)
                  FROM pg_roles WHERE rolname IN ('{r}', '{c}', '{w}')),
               (SELECT string_agg(r.rolname || '>' || m.rolname, ',' ORDER BY r.rolname, m.rolname)
                  FROM pg_auth_members a JOIN pg_roles r ON r.oid = a.roleid JOIN pg_roles m ON m.oid = a.member
                 WHERE m.rolname IN ('{r}', '{c}', '{w}') OR r.rolname IN ('{r}', '{c}', '{w}')))",
            r = self.reader.role,
            c = self.careless.role,
            w = self.writer.role
        );
        self.superuser().value(&sql).unwrap_or_default()
    }

    /// The statements running in this database now, other than the
    /// asker's own.
    pub fn running(&self) -> Vec<String> {
        self.superuser()
            .batch(
                "SELECT query FROM pg_stat_activity
                  WHERE datname = current_database() AND pid <> pg_backend_pid()
                    AND state = 'active'",
            )
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten())
            .collect()
    }

    /// The sessions connected to this database now as `role`, other than the
    /// asker's own: their backend pids.
    pub fn sessions_of(&self, role: &str) -> Vec<i64> {
        self.superuser()
            .batch(&format!(
                "SELECT pid FROM pg_stat_activity
                  WHERE datname = current_database() AND pid <> pg_backend_pid()
                    AND usename = '{role}' ORDER BY pid"
            ))
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten()?.parse().ok())
            .collect()
    }

    /// The advisory locks held now in this database by sessions of these
    /// roles, each as `pid role classid/objid mode`: what the harness's own
    /// connection sees of the locks a login's sessions still hold.
    pub fn advisory_locks_of(&self, roles: &[&str]) -> Vec<String> {
        let roles = roles
            .iter()
            .map(|r| format!("'{r}'"))
            .collect::<Vec<_>>()
            .join(", ");
        self.superuser()
            .batch(&format!(
                "SELECT a.pid || ' ' || a.usename || ' ' || l.classid || '/' || l.objid || ' ' || l.mode
                   FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                  WHERE l.locktype = 'advisory' AND l.granted
                    AND a.datname = current_database() AND a.usename IN ({roles})"
            ))
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten())
            .collect()
    }

    /// The statements of this database's server log with the login each
    /// came from (the `user=` of its line), in order.
    pub fn statements_by_login(&self) -> Vec<(String, String)> {
        self.statements()
            .into_iter()
            .map(|entry| (login_of_entry(&entry).unwrap_or_default(), entry))
            .collect()
    }

    /// Fails unless every statement in this database's server log that
    /// contains `marker` came from `login`, and at least one did.
    pub fn assert_marker_came_from(&self, marker: &str, login: &Login, context: &str) {
        self.assert_received(marker, context);
        let wrong: Vec<String> = self
            .statements_by_login()
            .into_iter()
            .filter(|(by, entry)| entry.contains(marker) && *by != login.role)
            .map(|(by, entry)| format!("  from {by:?}: {entry}"))
            .collect();
        assert!(
            wrong.is_empty(),
            "{context}: expected every statement containing {marker:?} to reach the shared database {} as the login {}, but:\n{}",
            self.name,
            login.role,
            wrong.join("\n")
        );
    }

    /// The connection values that must never appear in anything the
    /// program says, by what they are: both logins' passwords and roles,
    /// the database's name, the host when it is unmistakable. The port is
    /// searched for separately ([`find_port`]), as a number standing alone.
    pub fn credential_markers(&self) -> Vec<(&'static str, String)> {
        let mut markers = vec![
            ("the read login's password", self.reader.password.clone()),
            (
                "the careless login's password",
                self.careless.password.clone(),
            ),
            ("the write login's password", self.writer.password.clone()),
            ("the read login's role", self.reader.role.clone()),
            ("the careless login's role", self.careless.role.clone()),
            ("the write login's role", self.writer.role.clone()),
            ("the database's name", self.name.clone()),
        ];
        if self.host_is_distinct() {
            markers.push(("the host", self.host()));
        }
        markers
    }

    /// Which credentials `text` holds: the markers it contains, and the
    /// port if it appears as a number standing alone.
    pub fn credentials_in(&self, text: &str) -> Vec<&'static str> {
        let mut found: Vec<&'static str> = self
            .credential_markers()
            .into_iter()
            .filter(|(_, value)| text.contains(value.as_str()))
            .map(|(what, _)| what)
            .collect();
        if find_port(text, self.port()) {
            found.push("the port");
        }
        found
    }
}

/// The role named in a server log entry's prefix (`... db=<db> user=<role>: ...`).
pub fn login_of_entry(entry: &str) -> Option<String> {
    let after = entry.split(" user=").nth(1)?;
    let role = after.split(':').next()?;
    Some(role.to_owned())
}

/// Whether `port` appears in `text` as a number standing alone: its digits,
/// with no letter or digit right before or after (so a timestamp or a hex
/// id that happens to contain the digits does not count).
pub fn find_port(text: &str, port: u16) -> bool {
    find_standalone(text, &port.to_string())
}

/// Whether `token` appears in `text` with no ASCII letter or digit right
/// before or after it.
pub fn find_standalone(text: &str, digits: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text.get(from..).and_then(|rest| rest.find(digits)) {
        let start = from + at;
        let end = start + digits.len();
        let before = start
            .checked_sub(1)
            .and_then(|i| bytes.get(i))
            .is_some_and(u8::is_ascii_alphanumeric);
        let after = bytes.get(end).is_some_and(u8::is_ascii_alphanumeric);
        if !before && !after {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_is_found_only_standing_alone() {
        assert!(find_port("127.0.0.1:41234/db", 41234));
        assert!(find_port("\"port\":41234}", 41234));
        assert!(find_port("41234", 41234));
        assert!(!find_port("\"time_ms\":1794141234567", 41234));
        assert!(!find_port("id-a41234b", 41234));
        assert!(!find_port("412345", 41234));
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 16);
    }
}
