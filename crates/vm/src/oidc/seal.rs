//! A session's refresh token at rest in the platform database: sealed with
//! AES-256-GCM, under a key derived with HKDF-SHA256 from the client secret
//! (`VM_OIDC_CLIENT_SECRET`), which is never in the platform database. The
//! session's cookie digest is the associated data, so a sealed token moved
//! to another session's row does not open there.
//!
//! So the platform database alone gives no refresh token; with the client
//! secret too, one could also use the tokens directly at the provider, so
//! the key adds nothing then. A new client secret leaves the sealed tokens
//! unreadable: each session ends at its next renewal, and its browser
//! signs in again.
//!
//! Stored form: `1 || nonce (12 random bytes) || ciphertext || tag`.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use ring::rand::{SecureRandom, SystemRandom};

use super::secret::{ClientSecret, Secret};

/// The first byte of the stored form.
const FORM: u8 = 1;

/// Seals and opens refresh tokens.
pub struct Sealer {
    key: LessSafeKey,
    random: SystemRandom,
}

impl Sealer {
    /// The key derived from the client secret. The error is a message for
    /// the operator.
    pub fn new(secret: &ClientSecret) -> Result<Self, String> {
        let prk = Salt::new(HKDF_SHA256, b"vm: refresh tokens at rest")
            .extract(secret.expose().as_bytes());
        let okm = prk
            .expand(&[b"AES-256-GCM, form 1"], &AES_256_GCM)
            .map_err(|_| "the key for refresh tokens cannot be derived".to_owned())?;
        Ok(Self {
            key: LessSafeKey::new(UnboundKey::from(okm)),
            random: SystemRandom::new(),
        })
    }

    /// `token` sealed for the session whose cookie digest is `session`;
    /// `None` when the operating system gives no randomness.
    pub fn seal(&self, token: &Secret, session: &[u8; 32]) -> Option<Vec<u8>> {
        let mut nonce = [0_u8; NONCE_LEN];
        self.random.fill(&mut nonce).ok()?;
        // Sealed in place: a copy, once a sign-in or renewal.
        let mut sealed = token.expose().as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(session),
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
        let (form, rest) = stored.split_first()?;
        if *form != FORM {
            return None;
        }
        let (nonce, sealed) = rest.split_at_checked(NONCE_LEN)?;
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        // Opened in place: a copy of the row's bytes, once a renewal.
        let mut buffer = sealed.to_vec();
        let opened = self
            .key
            .open_in_place(nonce, Aad::from(session), &mut buffer)
            .ok()?;
        let text = String::from_utf8(opened.to_vec()).ok()?;
        Some(Secret::from_text(text))
    }
}
