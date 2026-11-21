//! A list's cursor (blackbox/CONTRACT.md, "Storage: Through the raw
//! interface", `list`): where the next page starts, sealed with a key the
//! platform database keeps (so that a cursor outlives a restart), bound to
//! the Resource, the prefix and the delimiter it was answered for. A cursor
//! made by hand, changed, or given back with another Resource, prefix or
//! delimiter does not open, and the list is `malformed`.
//!
//! Form: `<base64url of the position>.<base64url of its HMAC-SHA256>`, at
//! most [`CURSOR_BYTES`]. The position is the last key or folder answered,
//! which the Application was given already.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;

use super::Position;
use super::key::{KEY_BYTES, SEGMENT_BYTES};

/// The longest position: the last name a list passed, which may be a name
/// on the disk that is no key (a folder too long to hold one is passed
/// whole): a folder's keys (less than [`KEY_BYTES`]), a name and a `/`.
const POSITION_BYTES: usize = KEY_BYTES + SEGMENT_BYTES + 1;

/// The most bytes of a cursor: a position of [`POSITION_BYTES`] and its
/// flag (1,281 bytes) is 1,708 characters, its seal 43, and the dot.
pub const CURSOR_BYTES: usize = 2048;

/// The bytes of the key cursors are sealed with.
pub const KEY_LEN: usize = 32;

/// Seals and opens cursors. Its `Debug` shows nothing of its key.
pub struct Sealer {
    key: hmac::Key,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer(<hidden>)")
    }
}

impl Sealer {
    pub fn new(key: &[u8]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// The cursor of `position` for a list of `resource` with `prefix` and
    /// `delimiter`.
    pub fn seal(
        &self,
        resource: &str,
        prefix: &str,
        delimiter: bool,
        position: &Position,
    ) -> String {
        let payload = payload(position);
        let tag = hmac::sign(&self.key, &signed(resource, prefix, delimiter, &payload));
        let mut cursor = URL_SAFE_NO_PAD.encode(&payload);
        cursor.push('.');
        cursor.push_str(&URL_SAFE_NO_PAD.encode(tag.as_ref()));
        cursor
    }

    /// The position `cursor` holds, if this program sealed it for a list
    /// of `resource` with `prefix` and `delimiter`.
    pub fn open(
        &self,
        resource: &str,
        prefix: &str,
        delimiter: bool,
        cursor: &str,
    ) -> Option<Position> {
        if cursor.len() > CURSOR_BYTES {
            return None;
        }
        let (payload, tag) = cursor.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
        hmac::verify(
            &self.key,
            &signed(resource, prefix, delimiter, &payload),
            &tag,
        )
        .ok()?;
        let (flag, last) = payload.split_first()?;
        if last.len() > POSITION_BYTES {
            return None;
        }
        let last = String::from_utf8(last.to_vec()).ok()?;
        Some(Position {
            last,
            folder: *flag == 1,
        })
    }
}

/// A position as bytes: one byte, 1 for a folder, then the key.
fn payload(position: &Position) -> Vec<u8> {
    let mut payload = Vec::with_capacity(position.last.len() + 1);
    payload.push(u8::from(position.folder));
    payload.extend_from_slice(position.last.as_bytes());
    payload
}

/// What is signed: each part with its length, so that no two lists'
/// parts read the same.
fn signed(resource: &str, prefix: &str, delimiter: bool, payload: &[u8]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(32 + resource.len() + prefix.len() + payload.len());
    signed.extend_from_slice(b"vibe storage cursor 1\0");
    for part in [resource.as_bytes(), prefix.as_bytes(), payload] {
        let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
        signed.extend_from_slice(&len.to_be_bytes());
        signed.extend_from_slice(part);
    }
    signed.push(u8::from(delimiter));
    signed
}
