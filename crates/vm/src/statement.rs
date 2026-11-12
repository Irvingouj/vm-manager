//! The statement without its values (blackbox/CONTRACT.md, "The Operation
//! log"): what the Operation log records of the SQL an Application sent.
//!
//! Interface: [`without_values`]. Every constant written into the text is
//! replaced by a placeholder `$n`, numbered after the highest parameter the
//! statement already has; comments are removed; everything else is kept as
//! written. A statement Postgres's parser refuses is recorded as
//! [`UNPARSED`], with nothing of it kept.
//!
//! What counts as a constant is decided by Postgres's own scanner (the
//! `pg_query` crate, which compiles the server's lexer and grammar), not by a
//! lexer of ours: quoted, `E''`, `U&''`, `N''`, bit and hex strings,
//! dollar-quoted bodies, strings continued over a newline, every form of
//! number, and the boolean literals. Comments go whole, nested ones
//! included. The shared database's connections are opened with
//! `standard_conforming_strings` on (`executor`), the setting this scanner
//! uses, so the two read backslashes in a string alike.
//!
//! This works on tokens rather than on the parse tree: `pg_query`'s own
//! `normalize` replaces only the constants of statement kinds it knows, and
//! keeps comments.

use pg_query::protobuf::Token;

/// What is recorded for a statement that cannot be parsed.
pub const UNPARSED: &str = "<unparsed>";

/// The longest statement that is parsed for the log. A longer one is
/// recorded as [`UNPARSED`]: parsing is done inside the Application's call,
/// and its cost grows with the text.
pub const MAX_PARSED_BYTES: usize = 1024 * 1024;

/// The longest text recorded. What is past it is cut (at a character
/// boundary) and marked; it holds no constant, so nothing is revealed by
/// where the cut falls.
pub const MAX_RECORDED_BYTES: usize = 16 * 1024;

/// `sql` with every constant replaced and its comments removed, or
/// [`UNPARSED`].
pub fn without_values(sql: &str) -> String {
    if sql.len() > MAX_PARSED_BYTES || pg_query::parse(sql, 0).is_err() {
        return UNPARSED.to_owned();
    }
    let Ok(scanned) = pg_query::scan(sql) else {
        return UNPARSED.to_owned();
    };
    let mut tokens = Vec::with_capacity(scanned.tokens.len());
    for token in &scanned.tokens {
        let (Ok(start), Ok(end)) = (usize::try_from(token.start), usize::try_from(token.end))
        else {
            return UNPARSED.to_owned();
        };
        tokens.push((start, end, token.token()));
    }
    tokens.sort_by_key(|(start, _, _)| *start);
    rebuild(sql, &tokens).unwrap_or_else(|| UNPARSED.to_owned())
}

/// The text again from its tokens, constants replaced and comments
/// dropped. `None` when the tokens do not account for every character that
/// is not white space: then something was not understood, and nothing of
/// the text is kept.
fn rebuild(sql: &str, tokens: &[(usize, usize, Token)]) -> Option<String> {
    let mut next = highest_parameter(sql, tokens).checked_add(1)?;
    let mut text = String::with_capacity(sql.len().min(MAX_RECORDED_BYTES));
    let mut at = 0;
    for &(start, end, kind) in tokens {
        let between = sql.get(at..start)?;
        if !between.chars().all(char::is_whitespace) {
            return None;
        }
        text.push_str(between);
        if is_constant(kind) {
            text.push('$');
            text.push_str(&next.to_string());
            next = next.checked_add(1)?;
        } else if matches!(kind, Token::CComment | Token::SqlComment) {
            text.push(' ');
        } else {
            text.push_str(sql.get(start..end)?);
        }
        at = end;
    }
    let rest = sql.get(at..)?;
    if !rest.chars().all(char::is_whitespace) {
        return None;
    }
    let text = text.trim();
    Some(cut(text))
}

/// The tokens that are values written into the text.
fn is_constant(kind: Token) -> bool {
    matches!(
        kind,
        Token::Sconst
            | Token::Usconst
            | Token::Bconst
            | Token::Xconst
            | Token::Iconst
            | Token::Fconst
            | Token::TrueP
            | Token::FalseP
    )
}

/// The highest `$n` the statement has, or 0.
fn highest_parameter(sql: &str, tokens: &[(usize, usize, Token)]) -> u64 {
    tokens
        .iter()
        .filter(|(_, _, kind)| *kind == Token::Param)
        .filter_map(|&(start, end, _)| sql.get(start..end)?.strip_prefix('$')?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// `text`, cut to [`MAX_RECORDED_BYTES`] at a character boundary.
fn cut(text: &str) -> String {
    if text.len() <= MAX_RECORDED_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_RECORDED_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut kept = text.get(..end).unwrap_or_default().to_owned();
    kept.push_str(" ...");
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_with_only_parameters_is_kept_as_written() {
        let sql = "select title from incidents where title = $1 or id = $2";
        assert_eq!(without_values(sql), sql);
    }

    #[test]
    fn every_kind_of_constant_is_replaced() {
        let cases = [
            (
                "select id from t where title <> 'lit' and id < 7319004417 and id > -8812770065",
                "select id from t where title <> $1 and id < $2 and id > -$3",
            ),
            (
                "select $$dollar$$, $tag$ in $$ x $$ $tag$, E'it\\'s', e'\\x41', 'a''b'",
                "select $1, $2, $3, $4, $5",
            ),
            (
                "select U&'d\\0061t' UESCAPE '!', N'nat', B'101', X'1F'",
                "select $1 UESCAPE $2, N$3, $4, $5",
            ),
            (
                "select 0x1F, 0o17, 0b101, 1_000, .5, 5., 1e5, 1.5e-3",
                "select $1, $2, $3, $4, $5, $6, $7, $8",
            ),
            (
                "select interval '1 day', date '2020-01-01', 'x'::int4, true, false, null",
                "select interval $1, date $2, $3::int4, $4, $5, null",
            ),
            ("select 'a'\n  'continued' as c", "select $1 as c"),
            (
                "alter role current_user set work_mem = '8MB'",
                "alter role current_user set work_mem = $1",
            ),
            (
                "copy incidents from program 'secret'",
                "copy incidents from program $1",
            ),
            (
                "do $$ begin delete from t where x = 'secret'; end $$",
                "do $1",
            ),
            ("select $2, 'x', $1", "select $2, $3, $1"),
        ];
        for (sql, recorded) in cases {
            assert_eq!(without_values(sql), recorded, "{sql}");
        }
    }

    #[test]
    fn comments_are_removed_nested_ones_whole() {
        let recorded = without_values("select 1 /* a 'v1' /* b 'v2' */ c */ , 2 -- 'v3'\n from t");
        assert!(!recorded.contains('v'), "{recorded}");
        assert!(recorded.starts_with("select $1"), "{recorded}");
        assert!(recorded.ends_with("from t"), "{recorded}");
    }

    #[test]
    fn what_postgres_cannot_parse_is_recorded_as_unparsed() {
        for sql in [
            "selec 'v' from t",
            "select 'a' 'same line'",
            "select 'unterminated",
            "select '\u{0}'",
            "select 1 /* unterminated",
        ] {
            assert_eq!(without_values(sql), UNPARSED, "{sql:?}");
        }
    }

    #[test]
    fn a_long_statement_is_cut() {
        let sql = format!("select {}a from t", "a, ".repeat(10_000));
        let recorded = without_values(&sql);
        assert!(
            recorded.len() <= MAX_RECORDED_BYTES + 4,
            "{}",
            recorded.len()
        );
        assert!(recorded.ends_with(" ..."));
    }
}
