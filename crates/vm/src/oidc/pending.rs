//! The sign-ins under way, between step 2 and step 3 (blackbox/CONTRACT.md,
//! "The way through sign-in"): what the platform must remember of each
//! authorization request until the browser comes back to the callback.
//!
//! Kept in memory, keyed by the SHA-256 of the `state` sent to the
//! provider, each bound to the browser that started it by the SHA-256 of a
//! cookie on the sign-in address. One is taken (and removed) by the first
//! callback that shows its `state` from that browser.
//!
//! **Bounds.** At most [`MAX_UNDER_WAY`] at once; one older than
//! [`MAX_AGE`] is no sign-in (and is removed when another begins). When
//! every place is taken by one younger than that, a new sign-in cannot
//! begin, and its browser is answered `503`. Held behind a `std` mutex,
//! only to look up, insert or remove, never across an await. A restart
//! forgets them: those browsers start again.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::secret::{Secret, same};

/// The most sign-ins under way at once.
pub const MAX_UNDER_WAY: usize = 10_000;

/// The longest between step 2 and step 3.
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// What is remembered of one sign-in under way.
pub struct Pending {
    /// The SHA-256 of the sign-in address's cookie of the browser that
    /// started it.
    pub browser: [u8; 32],
    pub nonce: Secret,
    pub verifier: Secret,
    /// Where the browser goes once signed in: an Application's name, the
    /// path (checked) and the `state` of step 1.
    pub application: String,
    pub next: String,
    pub state: String,
    pub began: Instant,
}

/// Why a callback's `state` takes no sign-in under way.
#[derive(Debug, PartialEq, Eq)]
pub enum NotTaken {
    /// No such sign-in, or another browser's.
    Unknown,
    /// It began more than [`MAX_AGE`] ago.
    TooOld,
}

/// The sign-ins under way.
#[derive(Default)]
pub struct UnderWay {
    by_state: Mutex<HashMap<[u8; 32], Pending>>,
}

impl UnderWay {
    /// Remembers a sign-in under `state`; `false` when there is no room.
    pub fn begin(&self, state: &Secret, pending: Pending) -> bool {
        let mut map = self.by_state.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_UNDER_WAY {
            map.retain(|_, p| p.began.elapsed() <= MAX_AGE);
        }
        if map.len() >= MAX_UNDER_WAY {
            return false;
        }
        map.insert(state.digest(), pending);
        true
    }

    /// Takes the sign-in a callback's `state` names, if this browser (the
    /// SHA-256 of its cookie) started it. Another browser's is left where
    /// it is.
    pub fn take(&self, state: &[u8; 32], browser: Option<&[u8; 32]>) -> Result<Pending, NotTaken> {
        let mut map = self.by_state.lock().unwrap_or_else(PoisonError::into_inner);
        let ours = map
            .get(state)
            .zip(browser)
            .is_some_and(|(pending, browser)| same(&pending.browser, browser));
        if !ours {
            return Err(NotTaken::Unknown);
        }
        let pending = map.remove(state).ok_or(NotTaken::Unknown)?;
        if pending.began.elapsed() > MAX_AGE {
            return Err(NotTaken::TooOld);
        }
        Ok(pending)
    }
}
