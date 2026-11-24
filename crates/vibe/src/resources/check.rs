//! Checking the logins a Platform administrator supplies (blackbox/CONTRACT.md,
//! "Resources and Grants in full: Registering a database, checked"): every
//! login connects, at the same time, the way an operation connects; the read
//! and the structure login then run the probe, one statement that reads
//! `pg_catalog` only. Nothing is installed in the shared database, no lock on
//! the platform database is held, and every connection is closed before the
//! answer.
//!
//! Bounds: one login's connection 5 seconds, its probe 5 seconds, the whole
//! check 10 seconds (the caller enforces the last). A login that does not
//! answer by then is `timed-out`; a probe that does not, `probe-incomplete`.

use std::time::Duration;

use super::Connection;
use crate::executor::{self, Param, Password, ProbeFailure, Target};

/// One login's connection: 5 seconds.
const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// One login's probe: 5 seconds.
const PROBE_WAIT: Duration = Duration::from_secs(5);

/// Every probe is given up by this long after the check began: a quarter of
/// a second before the 10 seconds of the whole check, so that a login that
/// connected late and whose probe does not finish is `probe-incomplete`, and
/// the caller's 10 seconds never name a login that connected as `timed-out`.
const PROBES_BY: Duration = Duration::from_millis(9_750);

/// Which login this is, as the words the answers use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Read,
    Write,
    Structure,
}

impl Which {
    pub fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Structure => "structure",
        }
    }
}

/// A login that could not be used, and why.
#[derive(Clone, Copy, Debug)]
pub struct Failure {
    pub login: Which,
    /// `unreachable`, `refused`, `no-such-database` or `timed-out`.
    pub why: &'static str,
}

/// A warning the check gives; a registration or a replacement still
/// succeeds with it.
#[derive(Clone, Debug)]
pub struct Warning {
    pub reason: &'static str,
    pub login: Which,
    pub evidence: Option<Vec<&'static str>>,
    pub message: &'static str,
}

impl Warning {
    /// The warning as the answer writes it.
    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "reason": self.reason,
            "login": self.login.word(),
            "message": self.message,
        });
        if let Some(evidence) = &self.evidence
            && let Some(object) = value.as_object_mut()
        {
            object.insert("evidence".into(), serde_json::json!(evidence));
        }
        value
    }
}

/// What checking the connection came to.
#[derive(Debug, Default)]
pub struct Checked {
    pub failures: Vec<Failure>,
    pub warnings: Vec<Warning>,
}

/// The probe's answer, as the booleans the warnings are made of.
#[derive(Debug, Default)]
struct Probed {
    superuser: bool,
    write_membership: bool,
    write_execute_program: bool,
    write_server_files: bool,
    read_membership: bool,
    read_server_files: bool,
    read_execute_program: bool,
    table_write: bool,
    sequence_write: bool,
    schema_write: bool,
    table_read: bool,
    any_table: bool,
    sees_table: bool,
}

/// Checks every login `connection` gives, or only `only` of them (a
/// replacement of one login connects only what it is given, unless the
/// address changes). All at once: at most three connections for one check.
pub async fn check(connection: &Connection, only: &[Which]) -> Checked {
    let by = tokio::time::Instant::now() + PROBES_BY;
    let read = check_one(connection, Which::Read, &connection.read, only, by);
    let write = async {
        match &connection.write {
            Some(login) => check_one(connection, Which::Write, login, only, by).await,
            None => Checked::default(),
        }
    };
    let structure = async {
        match &connection.structure {
            Some(login) => check_one(connection, Which::Structure, login, only, by).await,
            None => Checked::default(),
        }
    };
    let (read, write, structure) = tokio::join!(read, write, structure);
    let mut checked = Checked::default();
    for part in [read, write, structure] {
        checked.failures.extend(part.failures);
        checked.warnings.extend(part.warnings);
    }
    checked
}

/// One login's check; nothing when `only` does not ask for it.
async fn check_one(
    connection: &Connection,
    which: Which,
    login: &super::Login,
    only: &[Which],
    by: tokio::time::Instant,
) -> Checked {
    let mut checked = Checked::default();
    if !only.contains(&which) {
        return checked;
    }
    let target = Target::new(
        connection.host().to_owned(),
        connection.port(),
        connection.database().to_owned(),
        login.role().to_owned(),
        Password::new(login.password().to_owned()),
    );
    if which == Which::Write {
        // Connected, and asked nothing: the check of a write login is that
        // it connects.
        if let Err(failure) = executor::reach(&target, CONNECT_WAIT).await {
            checked.failures.push(Failure {
                login: which,
                why: failure.why(),
            });
        }
        return checked;
    }
    match executor::probe(
        &target,
        PROBE,
        &probe_params(login.role(), connection.database()),
        CONNECT_WAIT,
        PROBE_WAIT,
        by,
    )
    .await
    {
        Ok(rows) => {
            if let Some(row) = rows.rows.first() {
                checked.warnings.extend(warnings_of(which, &fill(row)));
            } else {
                checked.warnings.push(incomplete(which));
            }
        }
        Err(ProbeFailure::Incomplete) => checked.warnings.push(incomplete(which)),
        Err(failure) => checked.failures.push(Failure {
            login: which,
            why: failure.why(),
        }),
    }
    checked
}

/// The probe did not finish: the login connected, and what it would warn of
/// is not known.
fn incomplete(which: Which) -> Warning {
    Warning {
        reason: "probe-incomplete",
        login: which,
        evidence: None,
        message: "the probe of the login did not finish in 5 seconds: the login connected and is taken, and the warning it would give is not known until the next registration or replacement of the logins",
    }
}

/// The probe's one row as the booleans the warnings are made of.
fn fill(row: &serde_json::Value) -> Probed {
    let get = |at: usize| {
        row.get(at)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    Probed {
        superuser: get(0),
        write_membership: get(1),
        write_execute_program: get(2),
        write_server_files: get(3),
        read_membership: get(4),
        read_server_files: get(5),
        read_execute_program: get(6),
        table_write: get(7),
        sequence_write: get(8),
        schema_write: get(9),
        table_read: get(10),
        any_table: get(11),
        sees_table: get(12),
    }
}

/// The warnings one probed login gives, in the contract's order: what it can
/// write or read, and then, for the read login, that it reaches no table.
fn warnings_of(which: Which, probed: &Probed) -> Vec<Warning> {
    let mut warnings = Vec::new();
    match which {
        Which::Read => {
            let mut evidence: Vec<&'static str> = Vec::new();
            for (kind, held) in [
                ("superuser", probed.superuser),
                (
                    "membership",
                    probed.write_membership
                        || probed.write_execute_program
                        || probed.write_server_files,
                ),
                (
                    "table",
                    probed.table_write || (probed.superuser && probed.any_table),
                ),
                ("sequence", probed.sequence_write),
                ("schema", probed.schema_write),
            ] {
                if held {
                    evidence.push(kind);
                }
            }
            if !evidence.is_empty() {
                warnings.push(Warning {
                    reason: "read-login-can-write",
                    login: Which::Read,
                    evidence: Some(evidence),
                    message: "the read login is able to write at the shared database. query runs in a read-only transaction whatever the login may do, so nothing is written through it; a login that cannot write is what the platform asks for, so ask its owner whether it can be given a login that only reads",
                });
            }
            // Whatever else it holds, a read login with no privilege on any
            // table of the database is warned of that too (the contract's
            // two warnings, not one instead of the other).
            if probed.any_table && !probed.sees_table {
                warnings.push(Warning {
                    reason: "login-sees-no-table",
                    login: Which::Read,
                    evidence: None,
                    message: "the read login holds a privilege on no table of the shared database: nothing is listed in a pull and no query reaches anything unless the write login does. Ask the Resource's contact whether the login should be given a privilege on the tables it needs",
                });
            }
        }
        Which::Structure => {
            let mut evidence: Vec<&'static str> = Vec::new();
            for (kind, held) in [
                ("superuser", probed.superuser),
                (
                    "membership",
                    probed.read_membership
                        || probed.read_server_files
                        || probed.read_execute_program,
                ),
                ("table", probed.table_read),
            ] {
                if held {
                    evidence.push(kind);
                }
            }
            if !evidence.is_empty() {
                warnings.push(Warning {
                    reason: "structure-login-can-read",
                    login: Which::Structure,
                    evidence: Some(evidence),
                    message: "the structure login is able to read rows at the shared database. It is there so that pulls never use a login that can read rows: ask its owner whether it can be given a login that holds nothing, or only the catalogs a pull needs",
                });
            }
        }
        Which::Write => {}
    }
    warnings
}

/// The probe's parameters: the role, the database, and no table, schema or
/// role in its text.
fn probe_params(role: &str, database: &str) -> [Param; 17] {
    let fixed = [
        // The three write memberships.
        "pg_write_all_data",
        "pg_execute_server_program",
        "pg_write_server_files",
        // The three read ones.
        "pg_read_all_data",
        "pg_read_server_files",
        "pg_execute_server_program",
        // The privileges the probe asks whether the login holds.
        "INSERT,UPDATE,DELETE,TRUNCATE",
        "INSERT,UPDATE",
        "UPDATE",
        "CREATE",
        // Any privilege on a table, and on a column: what a pull lists by.
        "SELECT,INSERT,UPDATE,DELETE,TRUNCATE",
        "SELECT,INSERT,UPDATE",
        // Reading rows.
        "SELECT",
    ];
    [
        Param::Text(role.to_owned()),
        Param::Text(fixed[0].to_owned()),
        Param::Text(fixed[1].to_owned()),
        Param::Text(fixed[2].to_owned()),
        Param::Text(fixed[3].to_owned()),
        Param::Text(fixed[4].to_owned()),
        Param::Text(fixed[5].to_owned()),
        Param::Text(fixed[6].to_owned()),
        Param::Text(fixed[7].to_owned()),
        Param::Text(fixed[8].to_owned()),
        Param::Text(fixed[9].to_owned()),
        Param::Text(database.to_owned()),
        Param::Text(fixed[10].to_owned()),
        Param::Text(fixed[11].to_owned()),
        // The schemas no warning counts: the system's own.
        Param::Text("information_schema".to_owned()),
        Param::Text("pg\\_%".to_owned()),
        Param::Text(fixed[12].to_owned()),
    ]
}

/// The probe: one statement that reads `pg_catalog` only, with the role,
/// the database, every privilege list and Postgres's own schema name as
/// parameters (no table, schema or role in its text). Each function is
/// asked about the login by its oid, so no parameter's type is ambiguous.
const PROBE: &str = "\
SELECT r.rolsuper,
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $2), 'MEMBER'), false),
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $3), 'MEMBER'), false),
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $4), 'MEMBER'), false),
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $5), 'MEMBER'), false),
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $6), 'MEMBER'), false),
       coalesce(pg_catalog.pg_has_role((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), (SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $7), 'MEMBER'), false),
       EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                  AND c.relkind IN ('r', 'p', 'v')
                  AND CASE WHEN c.relkind IN ('r', 'p', 'v') THEN
                        (pg_catalog.has_table_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $8)
                         OR pg_catalog.has_any_column_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $9))
                      ELSE false END),
       EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                  AND c.relkind = 'S'
                  AND CASE WHEN c.relkind = 'S' THEN
                        pg_catalog.has_sequence_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $10)
                      ELSE false END),
       (EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n
                 WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                   AND pg_catalog.has_schema_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), n.oid, $11))
        OR pg_catalog.has_database_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), $12, $11)),
       EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                  AND c.relkind IN ('r', 'p', 'v', 'm')
                  AND CASE WHEN c.relkind IN ('r', 'p', 'v', 'm') THEN
                        (pg_catalog.has_table_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $17)
                         OR pg_catalog.has_any_column_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $17))
                      ELSE false END),
       EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                  AND c.relkind IN ('r', 'p', 'v', 'm')),
       EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname NOT LIKE $16 AND n.nspname <> $15
                  AND c.relkind IN ('r', 'p', 'v', 'm')
                  AND CASE WHEN c.relkind IN ('r', 'p', 'v', 'm') THEN
                        (pg_catalog.has_table_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $13)
                         OR pg_catalog.has_any_column_privilege((SELECT x.oid FROM pg_catalog.pg_roles x WHERE x.rolname = $1), c.oid, $14))
                      ELSE false END)
  FROM pg_catalog.pg_roles r
 WHERE r.rolname = $1";
