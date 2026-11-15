//! Text of the installation's (blackbox/CONTRACT.md, "The tool: Every
//! command"): what one User wrote (a description, a contact, a name, a
//! refusal's message) must not move the cursor, recolour or hide text on
//! another's terminal.
//!
//! Everything the tool shows that came from the installation is read as a
//! [`Theirs`]. Its only way into words is [`fmt::Display`], which prints
//! every control character (C0, DEL and C1, the line end included) and every
//! bidirectional mark or override ([`is_escaped`]) as
//! `\u{<hex>}`. Its JSON form (`Serialize`) is the text itself, which JSON
//! escapes as JSON does; the tool's `json_line` escapes the rest of what
//! [`is_escaped`] names. It has no way to hand out its raw text for printing.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
pub struct Theirs(String);

impl Theirs {
    /// Whether the text is exactly `other`; for deciding, not for printing.
    pub fn is(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl From<&str> for Theirs {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

/// Whether `c` is printed escaped rather than as itself, in words and in
/// JSON alike: a control character (C0, DEL, C1, the line end included), or
/// a bidirectional mark or override (U+061C, U+200E, U+200F, U+202A to
/// U+202E, U+2066 to U+2069), which can make text read in another order
/// than it is.
pub fn is_escaped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

impl fmt::Display for Theirs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            if is_escaped(c) {
                write!(f, "\\u{{{:x}}}", u32::from(c))?;
            } else {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}
