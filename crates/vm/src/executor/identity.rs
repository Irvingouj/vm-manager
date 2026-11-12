//! The identity the host attaches in an Application's own database
//! (blackbox/CONTRACT.md, "Identity and Change history in the own
//! database"), the host's half: what the functions of the platform's
//! schema `vm` (crate::own_db, `own_db_schema.sql`) verify.
//!
//! Each own database has its own two keys, made inside it and kept in a
//! table of `vm` the runtime role cannot read; the host reads them once
//! over the owner role's connection (a result, which Postgres does not
//! log) and holds them in an [`IdentityKey`], which never prints. The keys
//! never travel to Postgres in a statement or a parameter.
//!
//! Per transaction the host asks the database for the transaction's key
//! (`vm.transaction_key()`: backend, virtual transaction id, start time),
//! and sets `vm.ctx` to the tag of the identity bound to that key, followed
//! by the identity itself. What the log can show of it, sent again in any
//! other transaction, has the wrong tag there.

use std::fmt;

use sha2::{Digest, Sha256};

/// The length of each of the two keys, in bytes.
const KEY_LENGTH: usize = 32;

/// An own database's two keys. No `Display`; its `Debug` shows nothing of
/// them.
pub struct IdentityKey {
    k1: [u8; KEY_LENGTH],
    k2: [u8; KEY_LENGTH],
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityKey(<hidden>)")
    }
}

impl IdentityKey {
    /// The keys as read from `vm.keys`; `None` unless each is 32 bytes.
    pub fn from_parts(k1: &[u8], k2: &[u8]) -> Option<Self> {
        Some(Self {
            k1: k1.try_into().ok()?,
            k2: k2.try_into().ok()?,
        })
    }

    /// The value of `vm.ctx` that attaches `identity` (from [`identity`])
    /// to the transaction whose key is `transaction_key`: the 64 hex digits
    /// of `sha256(k2 || sha256(k1 || "vm.ctx v1\n" || transaction_key ||
    /// "\n" || identity))`, then `identity`. `vm.attached()` computes the
    /// same.
    pub fn attach(&self, transaction_key: &str, identity: &str) -> String {
        let inner = Sha256::new()
            .chain_update(self.k1)
            .chain_update(b"vm.ctx v1\n")
            .chain_update(transaction_key.as_bytes())
            .chain_update(b"\n")
            .chain_update(identity.as_bytes())
            .finalize();
        let tag = Sha256::new()
            .chain_update(self.k2)
            .chain_update(inner)
            .finalize();
        let mut ctx = crate::registry::hex(&tag);
        ctx.push_str(identity);
        ctx
    }
}

/// Who a transaction is for, as `vm.ctx` carries it: a JSON object with
/// `user_id`, `application`, `actor`, `request` and `migration`, in ASCII
/// only (every other character escaped), so that its bytes are the same
/// whatever the own database's encoding.
pub fn identity(
    user_id: &str,
    application: &str,
    actor: &str,
    request: Option<&str>,
    migration: Option<&str>,
) -> String {
    let text = serde_json::json!({
        "user_id": user_id,
        "application": application,
        "actor": actor,
        "request": request,
        "migration": migration,
    })
    .to_string();
    ascii_only(&text)
}

/// `json` with every character outside ASCII written as JSON's `\uXXXX`
/// (a pair of them past the Basic Multilingual Plane). Only valid inside
/// strings, which is where serde_json leaves such characters.
fn ascii_only(json: &str) -> String {
    if json.is_ascii() {
        return json.to_owned();
    }
    let mut out = String::with_capacity(json.len().saturating_add(16));
    let mut units = [0_u16; 2];
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut units).iter() {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_never_prints() {
        let key = IdentityKey::from_parts(&[7; 32], &[9; 32]).unwrap();
        assert_eq!(format!("{key:?}"), "IdentityKey(<hidden>)");
        assert!(IdentityKey::from_parts(&[7; 31], &[9; 32]).is_none());
    }

    #[test]
    fn the_tag_is_bound_to_the_transaction_and_the_identity() {
        let key = IdentityKey::from_parts(&[1; 32], &[2; 32]).unwrap();
        let who = identity("alice", "board", "user", Some("r1"), None);
        let ctx = key.attach("10/1/5", &who);
        assert_eq!(ctx.len(), 64 + who.len());
        assert!(ctx.ends_with(&who));
        assert_ne!(ctx, key.attach("10/2/5", &who));
        let other = IdentityKey::from_parts(&[1; 32], &[3; 32]).unwrap();
        assert_ne!(ctx, other.attach("10/1/5", &who));
    }

    #[test]
    fn the_identity_is_ascii_json() {
        let who = identity("zoë 😀", "board", "migration", None, Some("0001_x"));
        assert!(who.is_ascii(), "{who}");
        let back: serde_json::Value = serde_json::from_str(&who).unwrap();
        assert_eq!(back["user_id"], "zoë 😀");
        assert_eq!(back["request"], serde_json::Value::Null);
        assert_eq!(back["migration"], "0001_x");
    }
}
