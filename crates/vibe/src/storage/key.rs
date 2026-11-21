//! Keys (blackbox/CONTRACT.md, "Storage: Keys"; ADR 0019, "The key"): the
//! rules a key keeps, checked by the host, the same for every backend,
//! before anything reaches one. A [`Key`] can only be made by checking
//! them, so a backend is never handed a name that breaks one.
//!
//! Also here: what a `list`'s prefix is ([`check_prefix`]), the name of a
//! file or folder on a disk that can be part of a key ([`is_segment`]), and
//! how a key is written in an Operation log entry ([`quoted`]).

use std::fmt::Write as _;

use unicode_normalization::is_nfc;

/// The most bytes of UTF-8 a key has.
pub const KEY_BYTES: usize = 1024;

/// The most segments a key has.
pub const SEGMENTS: usize = 64;

/// The most bytes one segment has (a file system's longest name).
pub const SEGMENT_BYTES: usize = 255;

/// What every name of the platform's own on a Resource's directory begins
/// with (`.vibe-resource`, `.vibe-staging/`): no segment of a key may.
pub const RESERVED: &str = ".vibe-";

/// Whether `segment` begins with [`RESERVED`], in any letter case (on a
/// disk that does not tell letter case apart, `.VIBE-resource` is the
/// marker itself).
fn is_reserved(segment: &str) -> bool {
    segment
        .as_bytes()
        .get(..RESERVED.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(RESERVED.as_bytes()))
}

/// A rule of "Keys", in the table's order: when several are broken, the
/// first is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Empty,
    TooLong,
    TooDeep,
    EmptySegment,
    SegmentTooLong,
    DotSegment,
    Character,
    NotNfc,
    Reserved,
}

impl Rule {
    /// The rule's name, as `invalid-key` gives it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "too-long",
            Self::TooDeep => "too-deep",
            Self::EmptySegment => "empty-segment",
            Self::SegmentTooLong => "segment-too-long",
            Self::DotSegment => "dot-segment",
            Self::Character => "character",
            Self::NotNfc => "not-nfc",
            Self::Reserved => "reserved",
        }
    }

    /// What the key must be, in words: never the key itself.
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "a key must not be empty",
            Self::TooLong => "a key is at most 1,024 bytes of UTF-8",
            Self::TooDeep => "a key has at most 64 segments separated by /",
            Self::EmptySegment => {
                "a key has no empty segment: no / at its start or its end, and no //"
            }
            Self::SegmentTooLong => "no segment of a key is longer than 255 bytes",
            Self::DotSegment => "no segment of a key is . or ..",
            Self::Character => "a key holds no control character, no \\ and no bidirectional mark",
            Self::NotNfc => {
                "a key must be in Unicode normalisation form C (NFC): normalise it before using it"
            }
            Self::Reserved => {
                "no segment of a key begins with .vibe-: those names are the platform's"
            }
        }
    }
}

/// A key that keeps every rule of "Keys". Made only by [`Key::new`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// `text` as a key, or the first rule it breaks.
    pub fn new(text: String) -> Result<Self, Rule> {
        check(&text)?;
        Ok(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its segments, in order: never empty, `.` or `..`.
    pub fn segments(&self) -> std::str::Split<'_, char> {
        self.0.split('/')
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Whether `c` is a character the rule `character` names other than `\`:
/// a control (U+0000 to U+001F, U+007F to U+009F) or a bidirectional mark.
pub fn is_named_character(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// The first rule of the table `text` breaks, if any. Each rule is looked
/// at over the whole key before the next, so the first in the table wins.
pub fn check(text: &str) -> Result<(), Rule> {
    if text.is_empty() {
        return Err(Rule::Empty);
    }
    if text.len() > KEY_BYTES {
        return Err(Rule::TooLong);
    }
    if text.split('/').count() > SEGMENTS {
        return Err(Rule::TooDeep);
    }
    if text.split('/').any(str::is_empty) {
        return Err(Rule::EmptySegment);
    }
    if text.split('/').any(|s| s.len() > SEGMENT_BYTES) {
        return Err(Rule::SegmentTooLong);
    }
    if text.split('/').any(|s| s == "." || s == "..") {
        return Err(Rule::DotSegment);
    }
    if text.chars().any(|c| c == '\\' || is_named_character(c)) {
        return Err(Rule::Character);
    }
    if !is_nfc(text) {
        return Err(Rule::NotNfc);
    }
    if text.split('/').any(is_reserved) {
        return Err(Rule::Reserved);
    }
    Ok(())
}

/// Whether `prefix` is a prefix of `list`: `""`, or a text that with one
/// more character (`x`) is a key that is taken; the rule that key breaks
/// otherwise.
pub fn check_prefix(prefix: &str) -> Result<(), Rule> {
    if prefix.is_empty() {
        return Ok(());
    }
    let mut probe = String::with_capacity(prefix.len() + 1);
    probe.push_str(prefix);
    probe.push('x');
    check(&probe)
}

/// Whether `name`, a name read from a folder on a disk, can be one segment
/// of a key: names that cannot (`.vibe-` names, controls, `\`, names not in
/// NFC, longer than 255 bytes, not UTF-8) are never listed or reached.
pub fn is_segment(name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    !name.contains('/') && check(name).is_ok()
}

/// `key` as an Operation log entry's `statement` writes it: a JSON string
/// in which `"` is `\"`, `\` is `\\` and every character the rule
/// `character` names is `\uXXXX` (lower-case hex), so that an entry is safe
/// to print; a key longer than [`KEY_BYTES`] is cut after its first 1,024
/// bytes, at a character's end, and `...` follows the closing quote.
pub fn quoted(key: &str) -> String {
    let mut cut = key.len().min(KEY_BYTES);
    while !key.is_char_boundary(cut) {
        cut -= 1;
    }
    let (kept, more) = key.split_at(cut);
    let mut out = String::with_capacity(kept.len() + 8);
    out.push('"');
    for c in kept.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if is_named_character(c) => {
                // Writing to a String cannot fail.
                write!(out, "\\u{:04x}", u32::from(c)).ok();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    if !more.is_empty() {
        out.push_str("...");
    }
    out
}
