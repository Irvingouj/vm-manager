//! Sealing with AES-256-GCM under a key derived with HKDF-SHA256 from the
//! installation's key (`VM_SECRET_KEY`, crate::oidc::key), which is never in
//! the platform database. Each use has a key of its own, by its own HKDF
//! salt ([`Use`]):
//!
//! - **A session's refresh token at rest** in the platform database. The
//!   session's cookie digest is the associated data, so a sealed token
//!   moved to another session's row does not open there.
//!   A tool credential's refresh token likewise, its digest the associated
//!   data; and one of a sign-in of the tool awaiting an answer, the digest
//!   of its `sign_in` value.
//! - **A sign-in under way**, carried in the `state` sent to the identity
//!   provider (crate::oidc::under_way), the SHA-256 of the sign-in
//!   address's cookie as the associated data; a sign-in of the tool under
//!   way with a key of its own.
//! - **The OIDC connection's client secret** at rest
//!   (crate::oidc::connection), a fixed label's digest as the associated
//!   data.
//!
//! So a dump of the platform database gives no secret. Another key leaves
//! everything sealed unreadable: the connection is not used until OIDC is
//! connected again, and each session and tool credential ends at its next
//! renewal. A new client secret changes nothing sealed.
//!
//! Sealed form: `1 || nonce (12 random bytes) || ciphertext || tag`; a
//! random nonce for each sealing, which AES-GCM allows for far more than
//! the 2^32 sealings one key could see here.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use ring::rand::{SecureRandom, SystemRandom};

use super::key::InstallationKey;
use super::secret::Secret;

/// The first byte of the stored form.
const FORM: u8 = 1;

/// What a key is for.
#[derive(Clone, Copy)]
pub enum Use {
    RefreshTokens,
    SignInState,
    ToolSignInState,
    ClientSecret,
}

impl Use {
    fn salt(self) -> &'static [u8] {
        match self {
            Self::RefreshTokens => b"vm: refresh tokens at rest",
            Self::SignInState => b"vm: a sign-in under way",
            Self::ToolSignInState => b"vm: a sign-in of the tool under way",
            Self::ClientSecret => b"vm: the OIDC connection's client secret",
        }
    }
}

/// Seals and opens, with the key of one [`Use`].
pub struct Sealer {
    key: LessSafeKey,
    random: SystemRandom,
}

impl Sealer {
    /// The key of `purpose`, derived from the installation's key. The
    /// error is a message for the operator.
    pub fn new(key: &InstallationKey, purpose: Use) -> Result<Self, String> {
        let prk = Salt::new(HKDF_SHA256, purpose.salt()).extract(key.expose());
        let okm = prk
            .expand(&[b"AES-256-GCM, form 1"], &AES_256_GCM)
            .map_err(|_| "a key for sealing cannot be derived".to_owned())?;
        Ok(Self {
            key: LessSafeKey::new(UnboundKey::from(okm)),
            random: SystemRandom::new(),
        })
    }

    /// `token` sealed for the session whose cookie digest is `session`;
    /// `None` when the operating system gives no randomness.
    pub fn seal(&self, token: &Secret, session: &[u8; 32]) -> Option<Vec<u8>> {
        // Sealed in place: a copy, once a sign-in or renewal.
        self.seal_bytes(token.expose().as_bytes().to_vec(), session)
    }

    /// `plain` sealed with `bound` as the associated data; `None` when the
    /// operating system gives no randomness.
    pub fn seal_bytes(&self, plain: Vec<u8>, bound: &[u8; 32]) -> Option<Vec<u8>> {
        let mut nonce = [0_u8; NONCE_LEN];
        self.random.fill(&mut nonce).ok()?;
        let mut sealed = plain;
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(bound),
                &mut sealed,
            )
            .ok()?;
        let mut stored = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
        stored.push(FORM);
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&sealed);
        Some(stored)
    }

    /// The token sealed in `stored` for the session `session`; `None` when
    /// it does not open (another key, another session, damaged).
    pub fn open(&self, stored: &[u8], session: &[u8; 32]) -> Option<Secret> {
        let opened = self.open_bytes(stored, session)?;
        let text = String::from_utf8(opened).ok()?;
        Some(Secret::from_text(text))
    }

    /// What `stored` holds, sealed with `bound` as the associated data;
    /// `None` when it does not open (another key, another binding,
    /// damaged).
    pub fn open_bytes(&self, stored: &[u8], bound: &[u8; 32]) -> Option<Vec<u8>> {
        let (form, rest) = stored.split_first()?;
        if *form != FORM {
            return None;
        }
        let (nonce, sealed) = rest.split_at_checked(NONCE_LEN)?;
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        // Opened in place: a copy of the sealed bytes, once a renewal or
        // a callback.
        let mut buffer = sealed.to_vec();
        let length = self
            .key
            .open_in_place(nonce, Aad::from(bound), &mut buffer)
            .ok()?
            .len();
        buffer.truncate(length);
        Some(buffer)
    }
}
