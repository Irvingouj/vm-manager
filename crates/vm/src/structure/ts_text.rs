//! The one way text of the shared database's enters the definition
//! (blackbox/CONTRACT.md, "The definition"): every name (of a schema, a
//! table, a view, a column, an enum, a label, a constraint, a type) and
//! every expression (a default's, a generated column's) is written by
//! [`literal`], as a TypeScript string literal or as the text of a template
//! literal, so that it is data and never code.
//!
//! In either form a character is written as itself only when it is printable
//! ASCII other than the quote, the backslash, the backtick and `$`, or any
//! other character that is neither a control character, a line terminator
//! (U+2028, U+2029) nor a bidirectional mark; everything else is an escape
//! (`\\`, `\"`, `` \` ``, `\$`, `\u{<hex>}`). So a string cannot be closed,
//! a template cannot be closed or substituted into (`${`), no line ends
//! inside either, and `*/` or `</script>` is only text in a string. The
//! definition writes nothing of the shared database's into a comment.

use std::fmt::Write as _;

/// The two forms text is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Form {
    /// `"<text>"`.
    String,
    /// The text between the backticks of a template literal (the caller
    /// writes the tag and the backticks).
    Template,
}

/// `text`, written as `form` says. For [`Form::String`] the quotes are
/// written too.
pub(super) fn literal(text: &str, form: Form) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    if form == Form::String {
        out.push('"');
    }
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            c if escaped(c) => {
                // Writing to a String does not fail.
                write!(out, "\\u{{{:x}}}", u32::from(c)).ok();
            }
            c => out.push(c),
        }
    }
    if form == Form::String {
        out.push('"');
    }
    out
}

/// Whether `c` is written as an escape whatever the form: a control
/// character (C0, DEL, C1: line ends included), a line terminator, or a
/// bidirectional mark or override, or a character outside the BMP's
/// plainly printable range that a reader could mistake (the BOM, U+FEFF).
fn escaped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}
