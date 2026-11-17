//! A sign-in under way, between step 2 and step 3 (blackbox/CONTRACT.md,
//! "Sign-in and sessions: The way through sign-in"): **the platform keeps
//! nothing of it.** What the callback needs travels in the `state` sent to
//! the identity provider, which brings it back:
//!
//! - when it began (whole seconds since 1970), so that the 10 minutes are
//!   checked from what is sealed;
//! - a seed of 256 random bits, from which the `nonce` and the PKCE
//!   verifier are derived (each the SHA-256 of a label of its own and the
//!   seed): neither can be had from the other, nor from the `state`
//!   without the key;
//! - the `state` of step 1 (at most [`MAX_STEP_ONE_STATE`] bytes), the
//!   Application's name (a DNS label, at most 63 bytes), and `next` (at
//!   most [`MAX_NEXT`] bytes).
//!
//! In a fixed layout, no field names: `began (8, big-endian) || seed (32)
//! || length (1) || state || length (1) || name || next`. It is sealed
//! (crate::oidc::seal, the key of [`Use::SignInState`], made from the
//! client secret with its own HKDF salt) with the SHA-256 of the sign-in
//! address's cookie as the associated data, so it opens only in the
//! browser that began it, and nobody without the key can read one or make
//! one. Sealed and in base64url, a sign-in of a 43-byte step-1 `state`, a
//! name of 6 bytes and a `next` of `/` is a `state` of 162 characters.
//!
//! So sign-ins begun and never finished, however many, take nothing of the
//! platform's; and one under way goes on across a start of the program with
//! the same client secret. A `state` can be shown again within its 10
//! minutes, in its browser only: the provider's code it comes with is good
//! once.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

use super::seal::{Sealer, Use};
use super::secret::{ClientSecret, Secret};

/// The longest `state` of step 1 taken, in bytes.
pub const MAX_STEP_ONE_STATE: usize = 128;

/// The longest `next` carried through the provider, in bytes; a longer one
/// is `/`.
pub const MAX_NEXT: usize = 1024;

/// How long a sign-in may stay under way, in seconds.
pub const MAX_AGE_SECONDS: u64 = 10 * 60;

/// A clock this much behind the one that sealed a `state` still takes it.
const CLOCK_SKEW_SECONDS: u64 = 60;

/// The longest sealed `state` read, as text: more than any this platform
/// makes.
const MAX_SEALED_TEXT: usize = 4096;

const SEED_BYTES: usize = 32;

/// What the callback needs, opened.
pub struct UnderWay {
    seed: [u8; SEED_BYTES],
    /// The `state` of step 1.
    pub state: String,
    pub application: String,
    pub next: String,
}

/// Why a `state` was not taken.
pub enum NotTaken {
    /// It does not open in this browser: another browser's, changed, not
    /// one of the platform's.
    Unreadable,
    /// It opened, and was begun more than 10 minutes ago.
    TooOld,
}

/// What step 2 sends to the provider.
pub struct Begun {
    /// The sealed `state`.
    pub state: Secret,
    pub nonce: Secret,
    pub verifier: Secret,
}

fn derived(label: &[u8], seed: &[u8; SEED_BYTES]) -> Secret {
    let mut hash = Sha256::new();
    hash.update(label);
    hash.update(seed);
    Secret::from_text(URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn nonce_of(seed: &[u8; SEED_BYTES]) -> Secret {
    derived(b"vm: the nonce of a sign-in\0", seed)
}

fn verifier_of(seed: &[u8; SEED_BYTES]) -> Secret {
    derived(b"vm: the PKCE verifier of a sign-in\0", seed)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl UnderWay {
    pub fn nonce(&self) -> Secret {
        nonce_of(&self.seed)
    }

    pub fn verifier(&self) -> Secret {
        verifier_of(&self.seed)
    }
}

/// Seals and opens the `state` of sign-ins under way.
pub struct States {
    sealer: Sealer,
}

impl States {
    /// The error is a message for the operator.
    pub fn new(secret: &ClientSecret) -> Result<Self, String> {
        Ok(Self {
            sealer: Sealer::new(secret, Use::SignInState)?,
        })
    }

    /// A sign-in begun now in the browser whose sign-in-address cookie
    /// has the SHA-256 `browser`. `None` when the operating system gives no
    /// randomness, or a part is longer than its bound (the caller has
    /// checked them).
    pub fn begin(
        &self,
        browser: &[u8; 32],
        state: &str,
        application: &str,
        next: &str,
    ) -> Option<Begun> {
        if state.len() > MAX_STEP_ONE_STATE || next.len() > MAX_NEXT {
            return None;
        }
        let state_length = u8::try_from(state.len()).ok()?;
        let name_length = u8::try_from(application.len()).ok()?;
        let mut seed = [0_u8; SEED_BYTES];
        SystemRandom::new().fill(&mut seed).ok()?;
        let mut plain =
            Vec::with_capacity(8 + SEED_BYTES + 2 + state.len() + application.len() + next.len());
        plain.extend_from_slice(&now_seconds().to_be_bytes());
        plain.extend_from_slice(&seed);
        plain.push(state_length);
        plain.extend_from_slice(state.as_bytes());
        plain.push(name_length);
        plain.extend_from_slice(application.as_bytes());
        plain.extend_from_slice(next.as_bytes());
        let sealed = self.sealer.seal_bytes(plain, browser)?;
        Some(Begun {
            state: Secret::from_text(URL_SAFE_NO_PAD.encode(sealed)),
            nonce: nonce_of(&seed),
            verifier: verifier_of(&seed),
        })
    }

    /// The sign-in a `state` holds, in the browser whose sign-in-address
    /// cookie has the SHA-256 `browser` (`None`: it has none).
    pub fn open(&self, text: &str, browser: Option<&[u8; 32]>) -> Result<UnderWay, NotTaken> {
        let browser = browser.ok_or(NotTaken::Unreadable)?;
        if text.len() > MAX_SEALED_TEXT {
            return Err(NotTaken::Unreadable);
        }
        let sealed = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| NotTaken::Unreadable)?;
        let plain = self
            .sealer
            .open_bytes(&sealed, browser)
            .ok_or(NotTaken::Unreadable)?;
        let read = read(&plain).ok_or(NotTaken::Unreadable)?;
        let now = now_seconds();
        let too_old = now.saturating_sub(read.began) > MAX_AGE_SECONDS
            || read.began > now.saturating_add(CLOCK_SKEW_SECONDS);
        if too_old {
            return Err(NotTaken::TooOld);
        }
        Ok(read.under_way)
    }
}

struct Read {
    began: u64,
    under_way: UnderWay,
}

/// The layout read back; `None` when it is not one (which a sealed value
/// that opened never is, unless the key is shared with another layout).
fn read(plain: &[u8]) -> Option<Read> {
    let (began, rest) = plain.split_first_chunk::<8>()?;
    let (seed, rest) = rest.split_first_chunk::<SEED_BYTES>()?;
    let (state_length, rest) = rest.split_first()?;
    let (state, rest) = rest.split_at_checked(usize::from(*state_length))?;
    let (name_length, rest) = rest.split_first()?;
    let (application, next) = rest.split_at_checked(usize::from(*name_length))?;
    // Owned text out of the opened bytes: once a callback, each bounded.
    Some(Read {
        began: u64::from_be_bytes(*began),
        under_way: UnderWay {
            seed: *seed,
            state: String::from_utf8(state.to_vec()).ok()?,
            application: String::from_utf8(application.to_vec()).ok()?,
            next: String::from_utf8(next.to_vec()).ok()?,
        },
    })
}
