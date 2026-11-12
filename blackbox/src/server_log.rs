//! The harness's Postgres server log read as entries (CONTRACT.md,
//! "Identity and Change history in the own database: The server log"):
//! turning statement logging fully on for a whole instance, splitting the
//! log into entries by the harness's `log_line_prefix` (`%m [%p] db=%d
//! user=%u: `), and taking out the statements one role sent to one
//! database, with their parameters, so that a test can send them again.

use std::time::{Duration, Instant};

use crate::own_database::literal;
use crate::postgres::PrivatePostgres;
use crate::sql::Sql;

/// One entry of the server log: one prefixed line and the lines that
/// continue it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub pid: String,
    pub database: String,
    pub user: String,
    /// What follows the prefix: `LOG:  statement: ...`, `DETAIL:  ...`.
    pub text: String,
}

/// Turns statement logging fully on for the whole instance (`ALTER
/// SYSTEM`, then a reload), and waits until a new connection sees it:
/// every statement, every duration, parameters logged whole, also on
/// error. For a test binary of its own: it changes the instance its other
/// tests share.
pub fn log_everything(postgres: &PrivatePostgres) {
    let superuser = Sql::connect(&postgres.url("postgres"))
        .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
    for setting in [
        "ALTER SYSTEM SET log_statement = 'all'",
        "ALTER SYSTEM SET log_min_duration_statement = 0",
        "ALTER SYSTEM SET log_parameter_max_length = -1",
        "ALTER SYSTEM SET log_parameter_max_length_on_error = -1",
        "ALTER SYSTEM SET log_min_error_statement = 'error'",
        "SELECT pg_reload_conf()",
    ] {
        superuser.batch(setting);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let fresh = Sql::connect(&postgres.url("postgres"))
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
        let seen = fresh.value(
            "SELECT current_setting('log_statement') || ' ' || current_setting('log_min_duration_statement')
                    || ' ' || current_setting('log_parameter_max_length')",
        );
        if seen.as_deref() == Some("all 0 -1") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the harness's own check: statement logging did not come on within 10 s (seen {seen:?})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The server log of `postgres` as entries, in order.
pub fn entries(postgres: &PrivatePostgres) -> Vec<Entry> {
    parse(&postgres.server_log())
}

/// Splits a log written with the harness's `log_line_prefix` into entries.
/// Lines that do not start with a date continue the entry before them (the
/// server starts them with a tab).
pub fn parse(log: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in log.lines() {
        if let Some(entry) = parse_prefixed(line) {
            entries.push(entry);
        } else if let Some(last) = entries.last_mut() {
            last.text.push('\n');
            last.text.push_str(line.strip_prefix('\t').unwrap_or(line));
        }
    }
    entries
}

fn parse_prefixed(line: &str) -> Option<Entry> {
    if !line.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let (_, rest) = line.split_once(" [")?;
    let (pid, rest) = rest.split_once("] db=")?;
    let (database, rest) = rest.split_once(" user=")?;
    let (user, text) = rest.split_once(": ")?;
    Some(Entry {
        pid: pid.to_owned(),
        database: database.to_owned(),
        user: user.to_owned(),
        text: text.to_owned(),
    })
}

/// A statement as the log shows it was sent: its text, and its parameters
/// (`None` for `NULL`) when it was sent with any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub pid: String,
    pub sql: String,
    pub params: Vec<Option<String>>,
}

impl Sent {
    /// The statement with its parameters written in as literals: what an
    /// attacker reading the log would send again.
    pub fn as_sql(&self) -> String {
        substitute(&self.sql, &self.params)
    }
}

/// The statements `user` sent to `database`, in order, as `log_statement
/// = all` logs them: `statement: <sql>` (the simple protocol) and
/// `execute <name>: <sql>` (the extended one) with the `Parameters:` detail
/// that follows it from the same backend. Duration-only entries, which
/// repeat these, are left out.
pub fn sent_by(entries: &[Entry], database: &str, user: &str) -> Vec<Sent> {
    let mine: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.database == database && e.user == user)
        .collect();
    let mut sent = Vec::new();
    for (i, entry) in mine.iter().enumerate() {
        let sql = if let Some(sql) = entry.text.strip_prefix("LOG:  statement: ") {
            sql.to_owned()
        } else if let Some(rest) = entry.text.strip_prefix("LOG:  execute ") {
            match rest.split_once(": ") {
                Some((_, sql)) => sql.to_owned(),
                None => continue,
            }
        } else {
            continue;
        };
        let params = mine
            .get(i + 1..)
            .unwrap_or_default()
            .iter()
            .find(|e| e.pid == entry.pid)
            .and_then(|e| e.text.strip_prefix("DETAIL:  Parameters: "))
            .map(parse_parameters)
            .unwrap_or_default();
        sent.push(Sent {
            pid: entry.pid.clone(),
            sql,
            params,
        });
    }
    sent
}

/// `$1 = 'a''b', $2 = NULL` as `[Some("a'b"), None]`, by number.
pub fn parse_parameters(text: &str) -> Vec<Option<String>> {
    let mut params: Vec<(usize, Option<String>)> = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches([',', ' ']);
        let Some(after) = rest.strip_prefix('$') else {
            break;
        };
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse::<usize>() else {
            break;
        };
        let Some(after) = after
            .get(digits.len()..)
            .and_then(|a| a.strip_prefix(" = "))
        else {
            break;
        };
        if let Some(after) = after.strip_prefix("NULL") {
            params.push((number, None));
            rest = after;
            continue;
        }
        let Some(mut after) = after.strip_prefix('\'') else {
            break;
        };
        let mut value = String::new();
        loop {
            match after.find('\'') {
                Some(at) => {
                    value.push_str(after.get(..at).unwrap_or_default());
                    let tail = after.get(at + 1..).unwrap_or_default();
                    if let Some(more) = tail.strip_prefix('\'') {
                        value.push('\'');
                        after = more;
                    } else {
                        after = tail;
                        break;
                    }
                }
                None => {
                    value.push_str(after);
                    after = "";
                    break;
                }
            }
        }
        params.push((number, Some(value)));
        rest = after;
    }
    params.sort_by_key(|(n, _)| *n);
    params.into_iter().map(|(_, v)| v).collect()
}

/// `sql` with each `$n` replaced by the literal of parameter `n` (or
/// `NULL`). Placeholders inside string literals are replaced too: the
/// harness's statements have none.
pub fn substitute(sql: &str, params: &[Option<String>]) -> String {
    let mut out = String::new();
    let mut chars = sql.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|(_, d)| d.is_ascii_digit()) {
            let mut digits = String::new();
            while let Some((_, d)) = chars.peek().copied() {
                if d.is_ascii_digit() {
                    digits.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let value = digits
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| params.get(i));
            match value {
                Some(Some(text)) => out.push_str(&literal(text)),
                Some(None) => out.push_str("NULL"),
                None => {
                    out.push('$');
                    out.push_str(&digits);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The statements of a simple-protocol text, split on the semicolons that
/// are outside quotes, dollar quotes and comments; empty ones left out.
pub fn split_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i).copied().unwrap_or(0) {
            b'\'' | b'"' => {
                let quote = bytes.get(i).copied().unwrap_or(0);
                i += 1;
                while i < bytes.len() {
                    if bytes.get(i) == Some(&quote) {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes.get(i) != Some(&b'\n') {
                    i += 1;
                }
            }
            b'$' => {
                let tag_end = bytes
                    .get(i + 1..)
                    .unwrap_or_default()
                    .iter()
                    .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                    .map(|p| i + 1 + p);
                if let Some(end) = tag_end
                    && bytes.get(end) == Some(&b'$')
                    && !bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                {
                    let tag = sql.get(i..=end).unwrap_or_default();
                    match sql.get(end + 1..).and_then(|rest| rest.find(tag)) {
                        Some(at) => i = end + 1 + at + tag.len() - 1,
                        None => i = bytes.len(),
                    }
                }
            }
            b';' => {
                let piece = sql.get(start..i).unwrap_or_default().trim();
                if !piece.is_empty() {
                    statements.push(piece.to_owned());
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let piece = sql.get(start..).unwrap_or_default().trim();
    if !piece.is_empty() {
        statements.push(piece.to_owned());
    }
    statements
}

/// Whether a statement only starts, ends or shapes a transaction or
/// resets a session (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`,
/// `ROLLBACK`, `ABORT`, `SAVEPOINT`, `RELEASE`, `SET TRANSACTION`,
/// `DISCARD`, `DEALLOCATE`, `PREPARE TRANSACTION`).
pub fn is_transaction_control(statement: &str) -> bool {
    let upper = statement.trim_start().to_ascii_uppercase();
    let first = upper.split_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "BEGIN"
            | "START"
            | "COMMIT"
            | "END"
            | "ROLLBACK"
            | "ABORT"
            | "SAVEPOINT"
            | "RELEASE"
            | "DISCARD"
            | "DEALLOCATE"
    ) || upper.starts_with("SET TRANSACTION")
        || upper.starts_with("SET SESSION CHARACTERISTICS")
        || upper.starts_with("PREPARE TRANSACTION")
}

/// Whether a statement begins a transaction.
pub fn begins_transaction(statement: &str) -> bool {
    let upper = statement.trim_start().to_ascii_uppercase();
    upper.starts_with("BEGIN") || upper.starts_with("START TRANSACTION")
}

/// Whether a statement ends a transaction.
pub fn ends_transaction(statement: &str) -> bool {
    let upper = statement.trim_start().to_ascii_uppercase();
    ["COMMIT", "END", "ROLLBACK", "ABORT"]
        .iter()
        .any(|w| upper.starts_with(w))
}

/// The statements sent, grouped by backend and by transaction: each group
/// is what one backend sent from a `BEGIN` to its `COMMIT` or `ROLLBACK`
/// (or one statement sent outside a transaction block), each statement
/// with its parameters written in, transaction control left out.
pub fn transactions(sent: &[Sent]) -> Vec<Vec<String>> {
    let mut pids: Vec<&str> = sent.iter().map(|s| s.pid.as_str()).collect();
    pids.sort_unstable();
    pids.dedup();
    let mut groups = Vec::new();
    for pid in pids {
        let mut open: Option<Vec<String>> = None;
        for s in sent.iter().filter(|s| s.pid == pid) {
            for statement in split_statements(&s.as_sql()) {
                if begins_transaction(&statement) {
                    if let Some(group) = open.take()
                        && !group.is_empty()
                    {
                        groups.push(group);
                    }
                    open = Some(Vec::new());
                } else if ends_transaction(&statement) {
                    if let Some(group) = open.take()
                        && !group.is_empty()
                    {
                        groups.push(group);
                    }
                } else if is_transaction_control(&statement) {
                    // Left out.
                } else if let Some(group) = open.as_mut() {
                    group.push(statement);
                } else {
                    groups.push(vec![statement]);
                }
            }
        }
        if let Some(group) = open
            && !group.is_empty()
        {
            groups.push(group);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "2026-11-12 10:00:00.001 UTC [41] db=app user=rt: LOG:  statement: BEGIN READ ONLY
2026-11-12 10:00:00.002 UTC [41] db=app user=rt: LOG:  execute <unnamed>: SELECT vm.attach($1, $2)
2026-11-12 10:00:00.003 UTC [42] db=app user=rt: LOG:  statement: SELECT 1
2026-11-12 10:00:00.004 UTC [41] db=app user=rt: DETAIL:  Parameters: $1 = 'it''s', $2 = NULL
2026-11-12 10:00:00.005 UTC [41] db=app user=rt: LOG:  duration: 0.1 ms  execute <unnamed>: SELECT vm.attach($1, $2)
2026-11-12 10:00:00.006 UTC [41] db=app user=rt: LOG:  execute s0: SELECT title
\tFROM things WHERE id = $1
2026-11-12 10:00:00.007 UTC [41] db=app user=rt: DETAIL:  Parameters: $1 = '7'
2026-11-12 10:00:00.008 UTC [41] db=app user=rt: LOG:  statement: COMMIT; DISCARD ALL
2026-11-12 10:00:00.009 UTC [43] db=other user=rt: LOG:  statement: SELECT 2
";

    #[test]
    fn statements_and_parameters_are_read_from_the_log() {
        let entries = parse(LOG);
        assert_eq!(entries.len(), 9);
        assert_eq!(
            entries[5].text,
            "LOG:  execute s0: SELECT title\nFROM things WHERE id = $1"
        );
        let sent = sent_by(&entries, "app", "rt");
        assert_eq!(sent.len(), 5, "{sent:#?}");
        assert_eq!(sent[1].params, vec![Some("it's".to_owned()), None]);
        assert_eq!(sent[1].as_sql(), "SELECT vm.attach('it''s', NULL)");
        assert_eq!(sent[3].as_sql(), "SELECT title\nFROM things WHERE id = '7'");
        let groups = transactions(&sent);
        assert_eq!(
            groups,
            vec![
                vec![
                    "SELECT vm.attach('it''s', NULL)".to_owned(),
                    "SELECT title\nFROM things WHERE id = '7'".to_owned()
                ],
                vec!["SELECT 1".to_owned()],
            ]
        );
    }

    #[test]
    fn statements_are_split_outside_quotes() {
        assert_eq!(
            split_statements("SELECT 'a;b'; SELECT $q$;$q$; -- x;\nSELECT \"c;d\""),
            vec!["SELECT 'a;b'", "SELECT $q$;$q$", "-- x;\nSELECT \"c;d\""]
        );
        assert_eq!(
            substitute("f($1, $10, $2)", &[Some("a".into()), None]),
            "f('a', $10, NULL)"
        );
        assert!(is_transaction_control("discard all"));
        assert!(!is_transaction_control("SELECT 1"));
    }
}
