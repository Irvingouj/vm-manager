//! The names a definition gives (blackbox/CONTRACT.md, "The definition:
//! Export names"): every export and every column key is made here, from a
//! name of the shared database's, and is built from ASCII letters, digits
//! and `_` only. Nothing else of the shared database's is ever written into
//! the definition as bare code.
//!
//! Camel case: the name is cut at every run of characters other than ASCII
//! letters and digits; the pieces (empty ones dropped) are joined, each
//! after the first with its first character in upper case; nothing else
//! changes case. What then begins with a digit, or is empty, gets a `_` in
//! front. A reserved word, or a name already given in the same set, gets
//! `_2`, `_3`, ...: the first that is free.

use std::collections::BTreeSet;

/// The words that cannot name a binding in a module: TypeScript's reserved
/// words and strict mode's, `await` (reserved in a module), and `eval` and
/// `arguments`, which strict code cannot bind. Column keys are held to the
/// same list, as the contract says.
const RESERVED: [&str; 49] = [
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// `parts`, each cut into pieces as camel case says, joined in camel case,
/// with a `_` in front when the result is empty or begins with a digit.
/// Only ASCII letters, digits and `_` are ever in it.
pub(super) fn camel(parts: &[&str]) -> String {
    let mut out = String::new();
    let pieces = parts
        .iter()
        .flat_map(|part| part.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|piece| !piece.is_empty());
    for (n, piece) in pieces.enumerate() {
        let mut chars = piece.chars();
        if n > 0
            && let Some(first) = chars.next()
        {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        } else {
            out.push_str(piece);
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// One set of names given: a module's exports, or one table's keys.
pub(super) struct Given {
    taken: BTreeSet<String>,
}

impl Given {
    /// A set in which `already` are taken before anything is given.
    pub(super) fn new(already: &[&str]) -> Self {
        Self {
            taken: already.iter().map(|&name| name.to_owned()).collect(),
        }
    }

    /// Gives `wanted` (as [`camel`] makes it), or, when it is reserved or
    /// taken, the first of `wanted_2`, `wanted_3`, ... that is free.
    pub(super) fn give(&mut self, wanted: String) -> String {
        let free = |name: &str, taken: &BTreeSet<String>| {
            !RESERVED.contains(&name) && !taken.contains(name)
        };
        let name = if free(&wanted, &self.taken) {
            wanted
        } else {
            // At most one more try than there are names taken.
            (2_usize..)
                .map(|n| format!("{wanted}_{n}"))
                .find(|candidate| free(candidate, &self.taken))
                .unwrap_or(wanted)
        };
        // Kept as taken and handed out: the name is short, made once.
        self.taken.insert(name.clone());
        name
    }
}
