//! The setup credential (blackbox/CONTRACT.md, "The setup credential"):
//! what `vm setup` prints, once, for connecting OIDC when nobody can sign
//! in yet.
//!
//! - `vmsc_` and 32 random bytes in base64url; the platform database keeps
//!   only its SHA-256, and when it ends ([`LIFETIME_SECONDS`] after it was
//!   made, by the database's clock).
//! - [`make`] deletes every earlier one in the same transaction: there is
//!   at most one, and a second `vm setup` ends the first one's.
//! - It is good only for `PUT /v1/oidc` ([`is_live`] first, then used up
//!   in the transaction that stores the connection,
//!   crate::oidc::connection); anything else made with it is `401`.

use tokio_postgres::Client;

use crate::oidc::secret::Secret;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::tool_credentials::SETUP_PREFIX;

/// How long a setup credential is good for: 24 hours.
pub const LIFETIME_SECONDS: i64 = 24 * 60 * 60;

/// A setup credential just made: its text (never printed but once) and
/// when it ends (RFC 3339, UTC).
pub struct Made {
    pub credential: Secret,
    pub expires_at: String,
}

/// Makes the setup credential, ending any earlier one. `None` when the
/// operating system gives no randomness.
pub async fn make(database: &PlatformDatabase) -> Result<Option<Made>, DbError> {
    let Some(random) = Secret::random() else {
        return Ok(None);
    };
    let credential = Secret::from_text(format!("{SETUP_PREFIX}{}", random.expose()));
    let digest = credential.digest();
    let row = database
        .pool()
        .run(async |client: &mut Client| {
            let transaction = client.transaction().await.map_err(DbError::from)?;
            transaction
                .execute("DELETE FROM setup_credentials", &[])
                .await
                .map_err(DbError::from)?;
            let row = transaction
                .query_one(
                    concat!(
                        "INSERT INTO setup_credentials (digest, expires_at)
                         VALUES ($1, now() + $2::bigint * interval '1 second')
                         RETURNING ",
                        rfc3339!("expires_at")
                    ),
                    &[&digest.as_slice(), &LIFETIME_SECONDS],
                )
                .await
                .map_err(DbError::from)?;
            transaction.commit().await.map_err(DbError::from)?;
            Ok(row)
        })
        .await?;
    let expires_at: String = row
        .try_get(0)
        .map_err(|_| DbError::unexpected("the setup credential's end"))?;
    Ok(Some(Made {
        credential,
        expires_at,
    }))
}

/// Whether the setup credential whose SHA-256 is `digest` is the live one.
pub async fn is_live(database: &PlatformDatabase, digest: &[u8; 32]) -> Result<bool, DbError> {
    let row = database
        .pool()
        .run(async |client: &mut Client| {
            client
                .query_opt(
                    "SELECT 1 FROM setup_credentials WHERE digest = $1 AND expires_at > now()",
                    &[&digest.as_slice()],
                )
                .await
                .map_err(DbError::from)
        })
        .await?;
    Ok(row.is_some())
}
