//! Statement policy applied by the host before any Application SQL reaches
//! Postgres. Uses pg_query (libpg_query: the real Postgres parser, PG 17 grammar
//! in pg_query 6.x). This is hygiene and clear error messages, NOT the security
//! boundary: functions can run SQL text the parser never sees (see the
//! `parser_*` checks). The boundary is the login role, the READ ONLY
//! transaction and the signed identity.

use crate::host::Level;

/// Functions an Application has no business calling. Matched on the last name
/// component, case-insensitive. Deliberately incomplete: see `ts_stat` check.
const DENY_FUNCS: &[&str] = &[
    "set_config",
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_try_advisory_xact_lock",
    "pg_try_advisory_xact_lock_shared",
    "pg_notify",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "lo_create",
    "lo_creat",
    "lo_import",
    "lo_from_bytea",
    "lo_put",
    "lo_unlink",
    "query_to_xml",
    "query_to_xmlschema",
    "query_to_xml_and_xmlschema",
    "dblink",
    "dblink_exec",
    "pg_stat_get_activity",
    "attach",       // vm.attach: only the host calls it, and not through this path
    "attach_c",
];

const DENY_RELATIONS: &[&str] = &["pg_stat_activity", "pg_settings", "pg_locks", "keys"];

pub fn check(sql: &str, level: Level) -> Result<(), String> {
    let parsed = pg_query::parse(sql).map_err(|e| format!("parse error: {e}"))?;
    let types = parsed.statement_types();
    if types.len() != 1 {
        return Err(format!("exactly one statement allowed, got {}", types.len()));
    }
    let allowed: &[&str] = match level {
        Level::Read => &["SelectStmt"],
        Level::Write => &["SelectStmt", "InsertStmt", "UpdateStmt", "DeleteStmt", "MergeStmt"],
    };
    if !allowed.contains(&types[0]) {
        return Err(format!("statement kind {} not allowed", types[0]));
    }
    for f in parsed.functions() {
        let last = f.rsplit('.').next().unwrap_or(&f).to_ascii_lowercase();
        if DENY_FUNCS.contains(&last.as_str()) {
            return Err(format!("function {f} not allowed"));
        }
    }
    for t in parsed.tables() {
        let last = t.rsplit('.').next().unwrap_or(&t).to_ascii_lowercase();
        if DENY_RELATIONS.contains(&last.as_str()) {
            return Err(format!("relation {t} not allowed"));
        }
    }
    Ok(())
}
