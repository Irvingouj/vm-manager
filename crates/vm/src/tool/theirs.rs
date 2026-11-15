//! Text of the installation's (blackbox/CONTRACT.md, "The tool: Every
//! command"): what one User wrote (a description, a contact, a name, a
//! refusal's message) must not move the cursor, recolour or hide text on
//! another's terminal.
//!
//! Everything the tool shows that came from the installation is read as a
//! [`Theirs`]. Its only way into words is [`fmt::Display`], which prints
//! every control character (C0, DEL and C1, the line end included) as
//! `\u{<hex>}`. Its JSON form (`Serialize`) is the text itself, which JSON
//! escapes on its own. It has no way to hand out its raw text for printing.

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

impl fmt::Display for Theirs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            if c.is_control() {
                write!(f, "\\u{{{:x}}}", u32::from(c))?;
            } else {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}
