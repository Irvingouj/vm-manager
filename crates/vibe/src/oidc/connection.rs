//! The OIDC connection (blackbox/CONTRACT.md, "Connecting OIDC"; ADR 0018):
//! the platform's registration at the identity provider, kept in the
//! platform database, its client secret sealed under the installation's key
//! (crate::oidc::seal, [`Use::ClientSecret`](super::seal::Use)).
//!
//! **Live.** The running program holds the connection in use in [`Live`]: a
//! `std` lock held only to clone or replace an `Arc`, never across an
//! await. A connection made (`PUT /v1/oidc`) is stored first, then put in
//! use with a fresh [`Provider`] (nothing of the earlier one's kept
//! configuration or keys); each request takes the connection in use when
//! it begins. Each stored connection has a generation, one more than the
//! one before under the row's lock, and the program takes one only over an
//! older one, so two connections made at once end with the later one in
//! use.
//!
//! **Over an earlier one.** When the issuer or the client id differ, every
//! session (with its Application sessions and codes), every tool
//! credential that came from a sign-in and every sign-in of the tool
//! awaiting an answer is deleted in the same transaction as the connection
//! is stored. Otherwise they stay, and their next renewal is made with the
//! new settings (the renewal takes the connection in use when it runs).

use std::sync::{Arc, PoisonError, RwLock};

use hyper::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use super::Oidc;
use super::provider::Provider;
use super::secret::ClientSecret;
use super::settings::{self, DEFAULT_GROUPS_CLAIM, Settings};
use crate::api::Answer;
use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};

/// The longest client id taken, in bytes.
pub const MAX_CLIENT_ID: usize = 256;

/// The longest client secret taken, in bytes.
pub const MAX_CLIENT_SECRET: usize = 1024;

/// The longest Group (and claim name) taken, in bytes.
pub const MAX_GROUP: usize = 256;

/// The longest issuer or scopes taken, in bytes.
const MAX_TEXT: usize = 2048;

/// What `connected_by` says of a connection made with the setup credential.
pub const BY_SETUP: &str = "setup-credential";

/// The associated data of the sealed client secret: the digest of a fixed
/// label, so a value sealed for another use does not open as it.
fn binding() -> [u8; 32] {
    Sha256::digest(b"vm: the OIDC connection's client secret at rest").into()
}

/// A connection as the platform database holds it.
pub struct Stored {
    pub issuer: String,
    pub client_id: String,
    /// Sealed.
    pub client_secret: Vec<u8>,
    pub groups_claim: String,
    pub scopes: Vec<String>,
    pub administrators_group: String,
    /// RFC 3339, UTC.
    pub connected_at: String,
    pub connected_by: String,
    pub generation: i64,
}

impl Stored {
    /// What `GET /v1/oidc` and `PUT /v1/oidc` answer: never the secret.
    pub fn view(&self) -> Value {
        json!({
            "issuer": self.issuer,
            "client_id": self.client_id,
            "groups_claim": self.groups_claim,
            "scopes": self.scopes,
            "administrators_group": self.administrators_group,
            "connected_at": self.connected_at,
            "connected_by": self.connected_by,
        })
    }
}

const COLUMNS: &str = concat!(
    "issuer, client_id, client_secret, groups_claim, scopes, administrators_group, ",
    rfc3339!("connected_at"),
    ", connected_by, generation"
);

fn stored(row: &tokio_postgres::Row) -> Result<Stored, DbError> {
    let bad = |_| DbError::unexpected("the OIDC connection");
    Ok(Stored {
        issuer: row.try_get(0).map_err(bad)?,
        client_id: row.try_get(1).map_err(bad)?,
        client_secret: row.try_get(2).map_err(bad)?,
        groups_claim: row.try_get(3).map_err(bad)?,
        scopes: row.try_get(4).map_err(bad)?,
        administrators_group: row.try_get(5).map_err(bad)?,
        connected_at: row.try_get(6).map_err(bad)?,
        connected_by: row.try_get(7).map_err(bad)?,
        generation: row.try_get(8).map_err(bad)?,
    })
}

/// The connection the platform database holds, if any.
pub async fn read(database: &PlatformDatabase) -> Result<Option<Stored>, DbError> {
    let row = database
        .pool()
        .run(async |client: &mut Client| {
            client
                .query_opt(
                    &format!("SELECT {COLUMNS} FROM oidc_connection WHERE id = 1"),
                    &[],
                )
                .await
                .map_err(DbError::from)
        })
        .await?;
    row.as_ref().map(stored).transpose()
}

/// Who makes a connection.
#[derive(Clone, Copy)]
pub enum By<'a> {
    /// The setup credential whose SHA-256 this is.
    Setup(&'a [u8; 32]),
    /// A Platform administrator.
    User(&'a str),
}

impl By<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Setup(_) => BY_SETUP,
            Self::User(id) => id,
        }
    }
}

/// A connection in use: its settings, its provider, and the Group whose
/// members are Platform administrators.
pub struct Connected {
    pub settings: Arc<Settings>,
    pub provider: Arc<Provider>,
    pub administrators: Arc<str>,
}

impl Connected {
    /// The error is a message for the operator.
    pub fn new(settings: Settings, administrators: &str) -> Result<Self, String> {
        let settings = Arc::new(settings);
        Ok(Self {
            provider: Arc::new(Provider::new(Arc::clone(&settings))?),
            settings,
            administrators: Arc::from(administrators),
        })
    }
}

/// The connection in use, with its generation.
#[derive(Default)]
pub struct Live(RwLock<Option<(i64, Arc<Connected>)>>);

impl Live {
    pub fn get(&self) -> Option<Arc<Connected>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, connected)| Arc::clone(connected))
    }

    /// Puts `connected` in use, unless one of a later generation is.
    fn set(&self, generation: i64, connected: Arc<Connected>) {
        let mut current = self.0.write().unwrap_or_else(PoisonError::into_inner);
        if current.as_ref().is_none_or(|(g, _)| generation > *g) {
            *current = Some((generation, connected));
        }
    }
}

/// A connection asked for, checked.
struct Asked {
    settings: Settings,
    administrators_group: String,
}

/// Whether `text` is 1 to `max` bytes without control characters.
fn fits(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

const FORM: &str = r#"{ "issuer": "<URL>", "client_id": "<id>", "client_secret": "<secret>", "administrators_group": "<Group>", "groups_claim": "<claim>" (optional), "scopes": "<scope> <scope> ..." (optional) }"#;

fn malformed(problem: &str) -> Answer {
    Answer::error(
        StatusCode::BAD_REQUEST,
        "malformed",
        &format!("the body is not the JSON asked for ({problem}): send {FORM}"),
    )
}

/// The body of `PUT /v1/oidc`, read; the refusal when it is not one. No
/// message repeats anything of the body: it holds the client secret.
fn read_body(body: &[u8]) -> Result<Asked, Answer> {
    let value: Value = serde_json::from_slice(body).map_err(|_| malformed("it is not JSON"))?;
    let Value::Object(object) = value else {
        return Err(malformed("it is not a JSON object"));
    };
    let required = |key: &'static str| -> Result<&str, Answer> {
        object
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| malformed(&format!("{key} is missing, or is not a string")))
    };
    let optional = |key: &'static str| -> Result<Option<&str>, Answer> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(_) => Err(malformed(&format!("{key} is not a string"))),
        }
    };
    let issuer = required("issuer")?;
    let client_id = required("client_id")?;
    let client_secret = required("client_secret")?;
    let administrators_group = required("administrators_group")?;
    let groups_claim = optional("groups_claim")?;
    let scopes = optional("scopes")?;
    if !fits(client_id, MAX_CLIENT_ID) {
        return Err(malformed(
            "client_id must be 1 to 256 bytes without control characters",
        ));
    }
    if !fits(client_secret, MAX_CLIENT_SECRET) {
        return Err(malformed(
            "client_secret must be 1 to 1,024 bytes without control characters",
        ));
    }
    if !fits(administrators_group, MAX_GROUP) {
        return Err(malformed(
            "administrators_group must be a Group: 1 to 256 bytes without control characters",
        ));
    }
    if groups_claim.is_some_and(|claim| !fits(claim, MAX_GROUP)) {
        return Err(malformed(
            "groups_claim must be 1 to 256 bytes without control characters",
        ));
    }
    if scopes.is_some_and(|s| s.len() > MAX_TEXT || s.chars().any(char::is_control)) {
        return Err(malformed(
            "scopes must be one string of scopes separated by spaces",
        ));
    }
    if issuer.len() > MAX_TEXT {
        return Err(invalid_issuer("it is longer than 2,048 bytes"));
    }
    settings::check_issuer(issuer).map_err(invalid_issuer)?;
    Ok(Asked {
        settings: Settings {
            issuer: issuer.to_owned(),
            client_id: client_id.to_owned(),
            client_secret: ClientSecret::new(client_secret.to_owned()),
            groups_claim: groups_claim.unwrap_or(DEFAULT_GROUPS_CLAIM).to_owned(),
            scopes: settings::scopes(scopes),
        },
        administrators_group: administrators_group.to_owned(),
    })
}

fn invalid_issuer(why: &str) -> Answer {
    Answer::error(
        StatusCode::BAD_REQUEST,
        "invalid-issuer",
        &format!(
            "the issuer cannot be used ({why}): give https://<host>..., or http:// on a loopback host (127.0.0.1, [::1], localhost), with no user, query or fragment"
        ),
    )
}

/// The setup credential named was not the live one: `401`.
pub fn setup_refused() -> Answer {
    Answer::error(
        StatusCode::UNAUTHORIZED,
        "not-signed-in",
        "the setup credential is used up, replaced or expired: run `vm setup` again",
    )
}

impl Oidc {
    /// Takes the connection the platform database holds into use, at
    /// start; `false` when its client secret does not open with this
    /// installation's key (it was made under another).
    pub fn take_stored(&self, stored: &Stored) -> Result<bool, String> {
        let Some(secret) = self
            .client_secrets
            .open_bytes(&stored.client_secret, &binding())
            .and_then(|plain| String::from_utf8(plain).ok())
        else {
            return Ok(false);
        };
        let connected = Connected::new(
            Settings {
                issuer: stored.issuer.clone(),
                client_id: stored.client_id.clone(),
                client_secret: ClientSecret::new(secret),
                groups_claim: stored.groups_claim.clone(),
                scopes: stored.scopes.clone(),
            },
            &stored.administrators_group,
        )?;
        self.live.set(stored.generation, Arc::new(connected));
        Ok(true)
    }

    /// `PUT /v1/oidc`, once who asks has been let through and the program
    /// can take a connection: the body (`400`), the provider (`422`), then
    /// the connection stored and put in use. A refusal changes nothing.
    pub async fn connect(&self, body: &[u8], by: By<'_>) -> Answer {
        let asked = match read_body(body) {
            Ok(asked) => asked,
            Err(refused) => return refused,
        };
        let connected = match Connected::new(asked.settings, &asked.administrators_group) {
            Ok(connected) => connected,
            Err(message) => {
                log::record("oidc-connect-failed", json!({ "error": message }));
                return Answer::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "the platform could not prepare its calls to the identity provider: try again shortly",
                );
            }
        };
        // The provider is asked first; the configuration fetched is kept
        // by the provider that goes into use.
        if let Err(problem) = connected.provider.configuration(None).await {
            return Answer::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "identity-provider-unusable",
                &format!(
                    "the identity provider at {} cannot be used: {}. Nothing was changed",
                    connected.settings.issuer,
                    problem.reason()
                ),
            );
        }
        let Some(sealed) = self.client_secrets.seal_bytes(
            connected
                .settings
                .client_secret
                .expose()
                .as_bytes()
                .to_vec(),
            &binding(),
        ) else {
            log::record("randomness-failed", json!({ "for": "the OIDC connection" }));
            return Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the platform could not seal the client secret: try again shortly",
            );
        };
        let stored = match store(&self.database, &connected, &sealed, by).await {
            Ok(Some(stored)) => stored,
            Ok(None) => return setup_refused(),
            Err(error) => return Answer::unavailable(&error),
        };
        log::record(
            "oidc-connected",
            json!({
                "issuer": stored.issuer,
                "client_id": stored.client_id,
                "administrators_group": stored.administrators_group,
                "by": stored.connected_by,
            }),
        );
        let view = stored.view();
        self.live.set(stored.generation, Arc::new(connected));
        Answer::json(StatusCode::OK, &view)
    }
}

/// Stores the connection in one transaction: the setup credential used up
/// (`None` when it is not live any more), the connection written, and when
/// the issuer or the client id changed, what came from the earlier one
/// deleted.
async fn store(
    database: &PlatformDatabase,
    connected: &Connected,
    sealed: &[u8],
    by: By<'_>,
) -> Result<Option<Stored>, DbError> {
    let settings = &connected.settings;
    let administrators: &str = &connected.administrators;
    let by_name = by.name();
    database
        .pool()
        .run(async |client: &mut Client| {
            let transaction = client.transaction().await.map_err(DbError::from)?;
            if let By::Setup(digest) = by {
                let used = transaction
                    .execute(
                        "DELETE FROM setup_credentials WHERE digest = $1 AND expires_at > now()",
                        &[&digest.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)?;
                if used != 1 {
                    return Ok(None);
                }
            }
            let earlier = transaction
                .query_opt(
                    "SELECT issuer, client_id FROM oidc_connection WHERE id = 1 FOR UPDATE",
                    &[],
                )
                .await
                .map_err(DbError::from)?;
            let row = transaction
                .query_one(
                    &format!(
                        "INSERT INTO oidc_connection (id, issuer, client_id, client_secret,
                             groups_claim, scopes, administrators_group, connected_by)
                         VALUES (1, $1, $2, $3, $4, $5, $6, $7)
                         ON CONFLICT (id) DO UPDATE SET
                             issuer = excluded.issuer, client_id = excluded.client_id,
                             client_secret = excluded.client_secret,
                             groups_claim = excluded.groups_claim, scopes = excluded.scopes,
                             administrators_group = excluded.administrators_group,
                             connected_by = excluded.connected_by, connected_at = now(),
                             generation = oidc_connection.generation + 1
                         RETURNING {COLUMNS}"
                    ),
                    &[
                        &settings.issuer,
                        &settings.client_id,
                        &sealed,
                        &settings.groups_claim,
                        &settings.scopes,
                        &administrators,
                        &by_name,
                    ],
                )
                .await
                .map_err(DbError::from)?;
            let changed = earlier.is_some_and(|row| {
                row.try_get::<_, String>(0).ok().as_deref() != Some(settings.issuer.as_str())
                    || row.try_get::<_, String>(1).ok().as_deref()
                        != Some(settings.client_id.as_str())
            });
            if changed {
                for statement in [
                    "DELETE FROM sessions",
                    "DELETE FROM tool_credentials WHERE from_sign_in",
                    "DELETE FROM tool_sign_ins",
                ] {
                    transaction
                        .execute(statement, &[])
                        .await
                        .map_err(DbError::from)?;
                }
            }
            transaction.commit().await.map_err(DbError::from)?;
            stored(&row).map(Some)
        })
        .await
}
