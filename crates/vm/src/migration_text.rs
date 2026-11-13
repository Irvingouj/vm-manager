//! A migration's text as the platform sends it (blackbox/CONTRACT.md, "The
//! platform's part of an own database is out of a migration's reach: How
//! migrations are sent, and what is refused before anything is sent";
//! ADR 0017).
//!
//! Interface: [`statements`], a migration's text in, where its single
//! statements are in it (byte ranges), in order, or the rule it breaks
//! ([`Refusal`]).
//!
//! The text is split at `drizzle-kit`'s breakpoint lines, then each part
//! into single statements by Postgres's own parser (`pg_query`, the one the
//! statement check uses), which knows strings, comments, dollar quotes and
//! quoted identifiers as Postgres does. The host then sends each statement
//! alone over the extended protocol, which Postgres refuses to run several
//! commands in (`42601`): a statement the parser saw as one cannot carry a
//! second one past it.
//!
//! Refused, before anything of the Version is sent:
//!
//! - `transaction-control`: a statement the parser names a
//!   `TransactionStmt` (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`,
//!   `ROLLBACK`, `ABORT`, their `AND CHAIN` forms, `PREPARE TRANSACTION`,
//!   `COMMIT PREPARED`, `ROLLBACK PREPARED`, `SAVEPOINT`, `RELEASE`,
//!   `ROLLBACK TO`). The platform's one transaction for the Version must
//!   not be ended, chained or split behind its back;
//! - `unparseable`: text the parser cannot read, with its words, and the
//!   line of the statement it stopped in.
//!
//! Pure and CPU-bound: the caller runs it off the async workers.

use std::ops::Range;

use pg_query::NodeEnum;
use pg_query::protobuf::{Token, TransactionStmtKind};

use crate::own_db::BREAKPOINT;

/// The longest excerpt of a statement a message quotes, in characters.
const EXCERPT: usize = 80;

/// A rule of the platform's broken by a migration's text.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    /// As the answer names it: `transaction-control` or `unparseable`.
    pub rule: &'static str,
    /// What was found, where, and what to do instead.
    pub message: String,
}

/// Where the single statements of the migration `sql` are in it, in order,
/// as byte ranges (each a valid `str` boundary pair).
pub fn statements(sql: &str) -> Result<Vec<Range<usize>>, Refusal> {
    let mut found = Vec::new();
    let mut offset = 0_usize;
    for part in sql.split(BREAKPOINT) {
        let start = offset;
        offset = offset
            .saturating_add(part.len())
            .saturating_add(BREAKPOINT.len());
        if part.trim().is_empty() {
            continue;
        }
        let parsed = match pg_query::parse(part, 0) {
            Ok(parsed) => parsed,
            Err(error) => return Err(unparseable(sql, start, part, &error)),
        };
        for raw in &parsed.protobuf.stmts {
            let at = usize::try_from(raw.stmt_location).ok();
            let length = usize::try_from(raw.stmt_len).ok();
            let text = match (at, length) {
                (Some(at), Some(0)) => part.get(at..),
                (Some(at), Some(length)) => {
                    at.checked_add(length).and_then(|end| part.get(at..end))
                }
                _ => None,
            };
            let Some(text) = text else {
                return Err(Refusal {
                    rule: "unparseable",
                    message: format!(
                        "Postgres's parser read the migration's text from line {} but could not say where one of its statements is: write each statement plainly, ended by a semicolon",
                        line_of(sql, start)
                    ),
                });
            };
            let node = raw.stmt.as_ref().and_then(|s| s.node.as_ref());
            if let Some(NodeEnum::TransactionStmt(control)) = node {
                let here = start
                    .saturating_add(at.unwrap_or(0))
                    .saturating_add(leading(text));
                return Err(Refusal {
                    rule: "transaction-control",
                    message: format!(
                        "the statement on line {} ({}) is {}: it controls the transaction. The platform applies all of a Version's migrations in one transaction that it begins and commits itself, so that the Version applies whole or not at all; remove the statement (a migration that fails is undone with everything else of the Version)",
                        line_of(sql, here),
                        excerpt(text),
                        control_words(control.kind(), control.chain),
                    ),
                });
            }
            let from = start.saturating_add(at.unwrap_or(0));
            found.push(from..from.saturating_add(text.len()));
        }
    }
    Ok(found)
}

/// The refusal of a part the parser cannot read: Postgres's words, and the
/// line of the first statement of the part that fails alone with the same
/// words (found by cutting the part at its semicolons as Postgres's
/// scanner sees them), or of the part when none does.
fn unparseable(sql: &str, start: usize, part: &str, error: &pg_query::Error) -> Refusal {
    let words = match error {
        pg_query::Error::Parse(words) | pg_query::Error::Scan(words) => words.clone(),
        pg_query::Error::Conversion(_) => "the text holds a NUL character".to_owned(),
        other => other.to_string(),
    };
    let mut line = line_of(sql, start.saturating_add(leading(part)));
    if let Ok(scanned) = pg_query::scan(part) {
        let mut from = 0_usize;
        for token in &scanned.tokens {
            let end = usize::try_from(token.end).unwrap_or(usize::MAX);
            if token.token != i32::from(Token::Ascii59) {
                continue;
            }
            if let Some(chunk) = part.get(from..end)
                && let Err(pg_query::Error::Parse(alone)) = pg_query::parse(chunk, 0)
                && alone == words
            {
                line = line_of(
                    sql,
                    start.saturating_add(from).saturating_add(leading(chunk)),
                );
                break;
            }
            from = end;
        }
        if from > 0
            && let Some(rest) = part.get(from..)
            && let Err(pg_query::Error::Parse(alone)) = pg_query::parse(rest, 0)
            && alone == words
        {
            line = line_of(
                sql,
                start.saturating_add(from).saturating_add(leading(rest)),
            );
        }
    }
    Refusal {
        rule: "unparseable",
        message: format!(
            "Postgres's parser cannot read the migration's text at the statement beginning on line {line}: {words}. Correct its syntax; nothing of the Version was sent to the database"
        ),
    }
}

/// How many bytes of `text` come before its first character that is not
/// white space.
fn leading(text: &str) -> usize {
    text.len().saturating_sub(text.trim_start().len())
}

/// The line, counted from 1, of the byte `at` of `text`.
fn line_of(text: &str, at: usize) -> usize {
    let before = text.get(..at).unwrap_or(text);
    before.matches('\n').count().saturating_add(1)
}

/// The statement on one line, cut to [`EXCERPT`] characters.
fn excerpt(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let one_line = words.join(" ");
    if one_line.chars().count() <= EXCERPT {
        return format!("\"{one_line}\"");
    }
    let cut: String = one_line.chars().take(EXCERPT).collect();
    format!("\"{cut}...\"")
}

/// What a transaction statement does, in Postgres's words.
fn control_words(kind: TransactionStmtKind, chain: bool) -> &'static str {
    match (kind, chain) {
        (TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart, _) => {
            "BEGIN or START TRANSACTION"
        }
        (TransactionStmtKind::TransStmtCommit, false) => "COMMIT or END",
        (TransactionStmtKind::TransStmtCommit, true) => "COMMIT AND CHAIN",
        (TransactionStmtKind::TransStmtRollback, false) => "ROLLBACK or ABORT",
        (TransactionStmtKind::TransStmtRollback, true) => "ROLLBACK AND CHAIN",
        (TransactionStmtKind::TransStmtSavepoint, _) => "SAVEPOINT",
        (TransactionStmtKind::TransStmtRelease, _) => "RELEASE SAVEPOINT",
        (TransactionStmtKind::TransStmtRollbackTo, _) => "ROLLBACK TO SAVEPOINT",
        (TransactionStmtKind::TransStmtPrepare, _) => "PREPARE TRANSACTION",
        (TransactionStmtKind::TransStmtCommitPrepared, _) => "COMMIT PREPARED",
        (TransactionStmtKind::TransStmtRollbackPrepared, _) => "ROLLBACK PREPARED",
        (TransactionStmtKind::Undefined, _) => "a transaction statement",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(sql: &str) -> Option<&'static str> {
        statements(sql).err().map(|r| r.rule)
    }

    fn texts(sql: &str) -> Vec<&str> {
        statements(sql)
            .unwrap()
            .into_iter()
            .map(|r| sql.get(r).unwrap())
            .collect()
    }

    #[test]
    fn a_migration_is_split_into_single_statements_at_breakpoints_and_semicolons() {
        let sql = "CREATE TABLE a (x int);\n--> statement-breakpoint\nINSERT INTO a VALUES (1); INSERT INTO a VALUES (2);\n--> statement-breakpoint\n\n";
        let found = texts(sql);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].trim().starts_with("CREATE TABLE a"));
        assert!(found[2].trim().starts_with("INSERT INTO a VALUES (2)"));
        assert!(statements("-- only a comment\n").unwrap().is_empty());
    }

    #[test]
    fn every_form_of_transaction_control_is_found() {
        for text in [
            "COMMIT",
            "commit work;",
            "END;",
            "COMMIT AND CHAIN",
            "BEGIN",
            "START TRANSACTION READ ONLY",
            "ROLLBACK AND CHAIN",
            "ABORT",
            "SAVEPOINT a",
            "RELEASE a",
            "ROLLBACK TO a",
            "PREPARE TRANSACTION 'x'",
            "COMMIT PREPARED 'x'",
            "ROLLBACK PREPARED 'x'",
            "SELECT 1; COMMIT; SELECT 2",
            "/* x */ COMMIT /* y */",
        ] {
            assert_eq!(rule(text), Some("transaction-control"), "{text}");
        }
    }

    #[test]
    fn the_words_elsewhere_are_not_transaction_control() {
        for text in [
            "SELECT 'COMMIT; BEGIN'",
            "-- COMMIT;\nSELECT 1",
            "SELECT $q$COMMIT;$q$",
            "CREATE TABLE \"commit\" (\"begin\" text, release text, abort boolean)",
            "CREATE PROCEDURE p() LANGUAGE plpgsql AS $f$ BEGIN COMMIT; END $f$",
            "DO $f$ BEGIN COMMIT; END $f$",
        ] {
            assert_eq!(rule(text), None, "{text}");
        }
    }

    #[test]
    fn unreadable_text_is_refused_with_postgres_s_words_and_its_line() {
        let refused =
            statements("SELECT 1;\n--> statement-breakpoint\nSELECT 2;\nCREATE TABLE ( broken;")
                .unwrap_err();
        assert_eq!(refused.rule, "unparseable");
        assert!(
            refused.message.contains("syntax error"),
            "{}",
            refused.message
        );
        assert!(refused.message.contains("line 4"), "{}", refused.message);
        let refused = statements("SELECT 'never closed;").unwrap_err();
        assert!(
            refused.message.contains("unterminated"),
            "{}",
            refused.message
        );
    }

    #[test]
    fn excerpts_are_one_line_and_short() {
        assert_eq!(excerpt("COMMIT\n  AND\n CHAIN"), "\"COMMIT AND CHAIN\"");
        assert_eq!(excerpt(&"x".repeat(200)).chars().count(), EXCERPT + 5);
    }
}
