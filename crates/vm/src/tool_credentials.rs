//! Tool credentials (blackbox/CONTRACT.md, "The tool: The credential" and
//! "On the platform's API"): what the Builder's command-line tool signs in
//! with on `api.<domain>`.
//!
//! - A credential is made by the installation from the operating system's
//!   randomness ([`CREDENTIAL_BYTES`] bytes), handed out once, in the answer
//!   that makes it, and never again. It is a [`Credential`], a type that does
//!   not print.
//! - The platform database keeps only its SHA-256 ([`Digest`]), with the
//!   User and the Groups of the moment it was made and when it expires. A
//!   presented credential is looked up by its digest; with 256 random bits
//!   behind it, nothing is learnt from how long a lookup takes.
//! - Whether it has expired is decided by the platform database's clock at
//!   each request (`expires_at > now()`), so a credential ends at its time
//!   whatever the program has in memory.
//! - What bounds the table: a credential signed out is deleted at once;
//!   every time a credential is made the expired ones are deleted, and the
//!   User's live ones past the newest [`MAX_LIVE`] (those made longest ago).
//!   What is left is at most [`MAX_LIVE`] live credentials a User, each for
//!   at most the lifetime.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hyper::HeaderMap;
use hyper::header::{AUTHORIZATION, Entry, HeaderValue};
use sha2::{Digest as _, Sha256};
use tokio_postgres::Client;

use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::user::User;

/// How many random bytes a credential is made of.
pub const CREDENTIAL_BYTES: usize = 32;

/// What every credential's text starts with, so that one found somewhere it
/// should not be can be recognised.
pub const PREFIX: &str = "vmtc_";

/// The longest `Authorization` value looked at; anything longer signs
/// nobody in, without being hashed.
const MAX_PRESENTED: usize = 512;

/// The most live credentials a User holds: one a machine, as many machines
/// as anyone uses. Making one more ends the one made longest ago.
pub const MAX_LIVE: i64 = 16;

/// The first key of the advisory lock taken per User while a credential is
/// made: "vm tc".
const USER_LOCK: i32 = 0x766d_7463;

/// A credential lives this long unless `vm serve --tool-credential-lifetime`
/// says otherwise: 30 days.
pub const DEFAULT_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A tool credential's text. It has no `Debug` or `Display`: the only way to
/// its text is [`Credential::expose`], called where the one answer that
/// holds it is written.
pub struct Credential(String);

impl Credential {
    /// A new credential from the operating system's randomness.
    fn new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; CREDENTIAL_BYTES];
        getrandom::fill(&mut bytes)?;
        let mut text = String::with_capacity(PREFIX.len() + 2 * CREDENTIAL_BYTES);
        text.push_str(PREFIX);
        for byte in bytes {
            text.push(hex_digit(byte >> 4));
            text.push(hex_digit(byte & 0x0f));
        }
        Ok(Self(text))
    }

    /// The text, for the one answer that hands it out.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).unwrap_or('0')
}

/// The SHA-256 of a credential's text: what the platform database keeps,
/// and how the credential of a request is named once it is signed in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl Digest {
    fn of(text: &str) -> Self {
        Self(Sha256::digest(text.as_bytes()).into())
    }
}

/// Not the digest itself: nothing is gained by printing it.
impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Digest(..)")
    }
}

/// A credential just made, and what was made with it.
pub struct Made {
    pub credential: Credential,
    /// RFC 3339, UTC.
    pub expires_at: String,
}

/// Why a credential could not be made.
#[derive(Debug)]
pub enum MakeError {
    /// The operating system gave no randomness.
    Randomness,
    Database(DbError),
}

impl From<DbError> for MakeError {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

/// What the `Authorization` header of a request says.
pub enum Presented {
    /// There is none: the request is signed in some other way, or not.
    Absent,
    /// There is one, and it decides alone. `None` when it cannot be a
    /// credential at all (another scheme, empty, given twice, too long).
    Present(Option<Digest>),
}

/// The tool credentials in the platform database.
pub struct ToolCredentials {
    database: Arc<PlatformDatabase>,
    /// How long a credential lives, in whole seconds.
    lifetime_seconds: i64,
}

impl ToolCredentials {
    /// `lifetime` is how long each credential made lives; at least a
    /// second.
    pub fn new(database: Arc<PlatformDatabase>, lifetime: Duration) -> Self {
        let lifetime_seconds = i64::try_from(lifetime.as_secs()).unwrap_or(i64::MAX).max(1);
        Self {
            database,
            lifetime_seconds,
        }
    }

    /// Makes a credential for `user`, as they and their Groups are now, and
    /// deletes every expired one.
    pub async fn make(&self, user: &User) -> Result<Made, MakeError> {
        let credential = Credential::new().map_err(|_| MakeError::Randomness)?;
        let digest = Digest::of(credential.expose());
        let lifetime = self.lifetime_seconds;
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                // One transaction, under a lock per User held to its end:
                // two sign-ins of one User at once take turns, so neither
                // counts without the other's credential and the User never
                // holds more than MAX_LIVE.
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .execute(
                        "SELECT pg_advisory_xact_lock($1, hashtext($2))",
                        &[&USER_LOCK, &user.id],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM tool_credentials WHERE expires_at <= now()",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
                let row = transaction
                    .query_one(
                        concat!(
                            "INSERT INTO tool_credentials (digest, user_id, groups, expires_at)
                             VALUES ($1, $2, $3, now() + $4::bigint * interval '1 second')
                             RETURNING ",
                            rfc3339!("expires_at")
                        ),
                        &[&digest.0.as_slice(), &user.id, &user.groups, &lifetime],
                    )
                    .await
                    .map_err(DbError::from)?;
                // The User's live credentials past the newest MAX_LIVE end:
                // those made longest ago.
                transaction
                    .execute(
                        "DELETE FROM tool_credentials
                         WHERE user_id = $1 AND made IN (
                             SELECT made FROM tool_credentials
                             WHERE user_id = $1 AND expires_at > now()
                             ORDER BY made DESC OFFSET $2)",
                        &[&user.id, &MAX_LIVE],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(row)
            })
            .await?;
        let expires_at: String = row
            .try_get(0)
            .map_err(|_| DbError::unexpected("a tool credential's expiry"))?;
        Ok(Made {
            credential,
            expires_at,
        })
    }

    /// The User a credential signs in, as they were when it was made;
    /// `None` when it is unknown, expired or signed out.
    pub async fn user_of(&self, digest: Digest) -> Result<Option<User>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT user_id, groups FROM tool_credentials
                         WHERE digest = $1 AND expires_at > now()",
                        &[&digest.0.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let unexpected = |_| DbError::unexpected("a tool credential's User");
        Ok(Some(User {
            id: row.try_get(0).map_err(unexpected)?,
            groups: row.try_get(1).map_err(unexpected)?,
        }))
    }

    /// Ends a credential at once.
    pub async fn end(&self, digest: Digest) -> Result<(), DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "DELETE FROM tool_credentials WHERE digest = $1",
                        &[&digest.0.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        Ok(())
    }
}

/// What the `Authorization` headers of a request present. Only `Bearer
/// <credential>` (the scheme in any letter case), once, can sign in.
pub fn presented(headers: &HeaderMap) -> Presented {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Presented::Absent;
    };
    if values.next().is_some() || value.len() > MAX_PRESENTED {
        return Presented::Present(None);
    }
    let digest = value
        .to_str()
        .ok()
        .and_then(|text| text.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, credential)| credential.trim())
        .filter(|credential| !credential.is_empty())
        .map(Digest::of);
    Presented::Present(digest)
}

/// Takes out of `headers` every `Authorization` value that holds, or looks
/// like, a tool credential, so that none reaches an Application's code
/// (blackbox/CONTRACT.md, "The tool: On the platform's API"); the others are
/// left as they were, in their order. Decided by the prefix alone: nothing
/// is looked up.
///
/// The rule is wider than what [`presented`] reads, so that whatever it
/// would take for a credential is never handed on: each comma-separated
/// part of each value, trimmed, whose scheme (up to the first whitespace)
/// is `Bearer` in any letter case and whose rest, trimmed, begins with
/// [`PREFIX`] in any letter case. A value that is not UTF-8 is read with
/// its stray bytes replaced, which changes no ASCII.
pub fn remove_credentials(headers: &mut HeaderMap) {
    let holds = |value: &HeaderValue| {
        String::from_utf8_lossy(value.as_bytes())
            .split(',')
            .any(looks_like_credential)
    };
    if !headers.get_all(AUTHORIZATION).iter().any(holds) {
        return;
    }
    // Only when there is one to take out: the rest are moved back, not
    // copied.
    let kept: Vec<HeaderValue> = match headers.entry(AUTHORIZATION) {
        Entry::Occupied(entry) => entry.remove_entry_mult().1.filter(|v| !holds(v)).collect(),
        Entry::Vacant(_) => Vec::new(),
    };
    for value in kept {
        headers.append(AUTHORIZATION, value);
    }
}

fn looks_like_credential(part: &str) -> bool {
    let part = part.trim();
    let Some((scheme, rest)) = part.split_once(char::is_whitespace) else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer")
        && rest
            .trim()
            .as_bytes()
            .get(..PREFIX.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(PREFIX.as_bytes()))
}
