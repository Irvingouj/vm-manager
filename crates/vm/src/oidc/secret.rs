//! Values that must never be shown: the client secret, cookie values,
//! one-time codes, `state`, `nonce`, PKCE verifiers, refresh tokens, ID
//! tokens. Each is a [`Secret`] or a [`ClientSecret`], types with no
//! `Debug` and no `Display`: the only way to the text is `expose`, called
//! where the value is sent or hashed. The platform database holds only the
//! SHA-256 of a cookie value or a code ([`digest`]).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

/// How many random bytes a value the platform makes holds: 256 bits.
const RANDOM_BYTES: usize = 32;

/// The length of such a value as base64url text.
pub const RANDOM_TEXT_LENGTH: usize = 43;

/// A value never to be shown.
pub struct Secret(String);

impl Secret {
    /// 256 random bits from the operating system, as base64url text;
    /// `None` when the operating system gives no randomness.
    pub fn random() -> Option<Self> {
        let mut bytes = [0_u8; RANDOM_BYTES];
        SystemRandom::new().fill(&mut bytes).ok()?;
        Some(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// A value received (a token, a code), kept as a secret from now on.
    pub fn from_text(text: String) -> Self {
        Self(text)
    }

    /// The text, where it is sent or hashed.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Its SHA-256.
    pub fn digest(&self) -> [u8; 32] {
        digest(&self.0)
    }
}

/// The client secret (`VM_OIDC_CLIENT_SECRET`): in no log, no message, no
/// page.
pub struct ClientSecret(String);

impl ClientSecret {
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The text, where the client authenticates and where the key that
    /// seals refresh tokens is derived.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// The SHA-256 of `text`.
pub fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// Whether two byte strings are equal, in a time that does not depend on
/// where they differ (their lengths are not secret).
pub fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let differ = a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(differ) == 0
}

/// Whether `text` is a value the platform could have made: 43 characters of
/// base64url.
pub fn looks_random(text: &str) -> bool {
    text.len() == RANDOM_TEXT_LENGTH
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `BASE64URL(SHA-256(verifier))`: the PKCE `S256` challenge.
pub fn s256(verifier: &Secret) -> String {
    URL_SAFE_NO_PAD.encode(verifier.digest())
}
