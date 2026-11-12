//! A shared database for the tests to register as a Resource (CONTRACT.md,
//! "Resources and Grants", "Operations on a shared database"): a database of
//! its own on one of the harness's Postgres instances, with a few tables of
//! varied types, a login that can only read it, and a careless "read" login
//! that could in fact write, as a Platform administrator might mistakenly
//! register.
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
    /// The title of incident 3.
    pub row_marker: String,
}

impl fmt::Debug for SharedDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedDatabase({:?})", self.name)
    }
}

/// The setup of every shared database, run by the instance's superuser in
/// the new database. `%READER%`, `%CARELESS%` and `%ROW_MARKER%` are
/// replaced first.
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

GRANT USAGE ON SCHEMA public TO %READER%, %CARELESS%;
GRANT SELECT ON incidents, kinds TO %READER%;
GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON incidents, kinds TO %CARELESS%;
GRANT USAGE, UPDATE ON SEQUENCE ticket_numbers TO %CARELESS%;
GRANT CREATE ON SCHEMA public TO %CARELESS%;
"#;

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
        let row_marker = format!("{ROW_MARKER_PREFIX}{token}");
        let superuser = Sql::connect(&server.url("postgres"))
            .unwrap_or_else(|e| panic!("cannot reach the Postgres for shared databases: {e}"));
        superuser.batch(&format!(
            "CREATE ROLE {r} LOGIN PASSWORD '{rp}';
             CREATE ROLE {c} LOGIN PASSWORD '{cp}';",
            r = reader.role,
            rp = reader.password,
            c = careless.role,
            cp = careless.password,
        ));
        superuser.batch(&format!("CREATE DATABASE {name}"));
        superuser.batch(&format!(
            "ALTER DATABASE {name} SET log_statement = 'all';
             ALTER DATABASE {name} SET timezone = 'UTC';
             ALTER DATABASE {name} SET datestyle = 'ISO, MDY';
             REVOKE ALL ON DATABASE {name} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {name} TO {r}, {c};",
            r = reader.role,
            c = careless.role,
        ));
        drop(superuser);
        let database = Self {
            server,
            name,
            reader,
            careless,
            row_marker,
        };
        let setup = SETUP
            .replace("%READER%", &database.reader.role)
            .replace("%CARELESS%", &database.careless.role)
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
    /// every table, the relations in every schema, the sequence, the
    /// settings of the database and its roles, and the roles' attributes.
    pub fn fingerprint(&self) -> String {
        let sql = format!(
            "SELECT concat_ws(' # ',
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t.id), '') FROM incidents t),
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t.label), '') FROM kinds t),
               (SELECT coalesce(string_agg(t::text, '|' ORDER BY t::text), '') FROM staff_only t),
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
                   AND (setrole = 0 OR setrole IN (SELECT oid FROM pg_roles WHERE rolname IN ('{r}', '{c}')))),
               (SELECT string_agg(rolname || rolsuper::text || rolcreaterole::text || rolcreatedb::text
                                  || coalesce(array_to_string(rolconfig, ';'), ''), ',' ORDER BY rolname)
                  FROM pg_roles WHERE rolname IN ('{r}', '{c}')))",
            r = self.reader.role,
            c = self.careless.role
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
            ("the read login's role", self.reader.role.clone()),
            ("the careless login's role", self.careless.role.clone()),
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
