//! The installation's key (blackbox/CONTRACT.md, "The installation's key";
//! ADR 0018): `VIBE_SECRET_KEY`, 32 random bytes in base64, in the
//! environment of `vibe serve` only. Every key that seals something is made
//! from it, one for each use (crate::oidc::seal).
//!
//! [`InstallationKey`] has no `Debug` and no `Display`, and no message
//! about the variable ever repeats its value.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The variable that holds the key.
pub const VARIABLE: &str = "VIBE_SECRET_KEY";

/// How many bytes the key is.
pub const KEY_BYTES: usize = 32;

/// The installation's key.
pub struct InstallationKey([u8; KEY_BYTES]);

impl InstallationKey {
    /// The key in `VIBE_SECRET_KEY`: `None` when the variable is not set; a
    /// message naming the variable, never its value, when it is set to
    /// anything but 32 bytes in base64 (empty included).
    pub fn from_environment() -> Result<Option<Self>, String> {
        let Some(value) = std::env::var_os(VARIABLE) else {
            return Ok(None);
        };
        let wrong = || {
            format!(
                "{VARIABLE} is not an installation's key: it must be 32 random bytes in base64, as `openssl rand -base64 32` makes them"
            )
        };
        let value = value.into_string().map_err(|_| wrong())?;
        let bytes = STANDARD.decode(value.trim()).map_err(|_| wrong())?;
        let key = <[u8; KEY_BYTES]>::try_from(bytes.as_slice()).map_err(|_| wrong())?;
        Ok(Some(Self(key)))
    }

    /// The bytes, where a key for one use is made from them.
    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }
}
