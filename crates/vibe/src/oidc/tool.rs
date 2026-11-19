//! The tool signs in through the browser, on the platform's side
//! (blackbox/CONTRACT.md, "Signing the tool in").
//!
//! 1. `GET /tool?challenge=&port=&machine=` ([`Oidc::tool_begin`]): an
//!    authorization of its own at the provider, whatever this browser's
//!    platform session, sealed into `state` like every sign-in under way
//!    (crate::oidc::under_way). **Nothing is kept**: a link opened and
//!    never finished, however many, takes nothing of the platform's.
//! 2. Back at `/callback` (crate::oidc), the code exchanged and the ID
//!    token checked: the sign-in now **awaits an answer**, a row of
//!    `tool_sign_ins` ([`Oidc::tool_awaiting`]), and the page asks.
//! 3. `POST /tool` ([`Oidc::tool_answer`]): confirmed, declined, or a
//!    wrong check code.
//! 4. `POST /v1/tool-sign-ins` ([`Oidc::take_tool_sign_in`]), by the tool:
//!    the credential, once, for the verifier whose challenge was confirmed,
//!    with the grant when there is a port.
//!
//! **Bounds.** At most [`MAX_AWAITING`] sign-ins awaiting an answer for
//! each User: the oldest goes when one more is made. Each awaits at most
//! [`AWAIT_SECONDS`]; every older one is deleted whenever one is made, so
//! the table holds at most five for each User who signed in at the
//! provider in the last ten minutes. [`MAX_WRONG_CODES`] wrong check codes
//! end a sign-in. The form's body is read up to [`MAX_FORM_BYTES`] within
//! [`FORM_READ_TIMEOUT`]. `POST /v1/tool-sign-ins` looks at no more than
//! [`MAX_SAME_CHALLENGE`] sign-ins of one challenge.
//!
//! **What is kept, and how.** The page's `sign_in` value, the browser's
//! cookie and the grant only as their SHA-256; the refresh token sealed
//! with the `sign_in` value's digest as the associated data, then sealed
//! again with the credential's when the credential is made. Every
//! comparison of a value presented with one kept (the browser, the grant,
//! the check code) is made in constant time.
//!
//! **The check code** ([`check_code`]) is made from the challenge: the
//! first 40 bits of SHA-256 of a fixed label and the challenge's 32 bytes,
//! in eight characters of Crockford's base32 (digits and capital letters
//! without `I`, `L`, `O` and `U`). The tool shows it; the page never does.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::CONTENT_TYPE;
use hyper::http::request::Parts;
use hyper::{Response, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;
use url::form_urlencoded;

use super::connection::Connected;
use super::pages::{self, Person};
use super::provider::Tokens;
use super::secret::{Secret, digest, same};
use super::sessions::log_database;
use super::under_way::{MAX_MACHINE, ToolUnderWay};
use super::{Oidc, SIGN_IN_COOKIE_SECONDS};
use crate::api::Answer;
use crate::cookies;
use crate::log;
use crate::platform_db::DbError;
use crate::tool_credentials::{FromSignIn, MakeError};
use crate::user::User;

/// The most sign-ins of the tool awaiting an answer for one User.
pub const MAX_AWAITING: i64 = 5;

/// How long a sign-in of the tool awaits an answer, in seconds.
pub const AWAIT_SECONDS: i64 = 600;

/// The wrong check codes that end a sign-in.
pub const MAX_WRONG_CODES: i32 = 5;

/// The largest body of `POST /tool` read.
pub const MAX_FORM_BYTES: usize = 4096;

/// The longest a browser may take to send the body of `POST /tool`.
pub const FORM_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The most sign-ins of one challenge looked at by `POST /v1/tool-sign-ins`.
pub const MAX_SAME_CHALLENGE: i64 = 16;

/// The longest verifier or grant looked at; a longer one is known of
/// nothing.
const MAX_PRESENTED: usize = 512;

/// The lowest port a sign-in with a port may name.
pub const LOWEST_PORT: u16 = 1024;

const CHECK_CODE_LABEL: &[u8] = b"vm: the check code of a sign-in of the tool\0";

/// Crockford's base32: digits and capital letters without I, L, O and U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The check code of a challenge, without its hyphen: eight characters of
/// [`ALPHABET`].
pub fn check_code(challenge: &[u8; 32]) -> [u8; 8] {
    let mut hash = Sha256::new();
    hash.update(CHECK_CODE_LABEL);
    hash.update(challenge);
    let hashed: [u8; 32] = hash.finalize().into();
    let mut bits: u64 = 0;
    for byte in hashed.iter().take(5) {
        bits = (bits << 8) | u64::from(*byte);
    }
    let mut code = [b'0'; 8];
    for (at, slot) in code.iter_mut().enumerate() {
        let shift = 35_u32.saturating_sub(u32::try_from(at).unwrap_or(0).saturating_mul(5));
        let index = usize::try_from((bits >> shift) & 0x1f).unwrap_or(0);
        *slot = ALPHABET.get(index).copied().unwrap_or(b'0');
    }
    code
}

/// The check code as shown: `XXXX-XXXX`.
pub fn shown_code(code: &[u8; 8]) -> String {
    let (first, second) = code.split_at(4);
    format!(
        "{}-{}",
        String::from_utf8_lossy(first),
        String::from_utf8_lossy(second)
    )
}

/// A check code as typed: in either case, with or without the hyphen and
/// spaces; `None` when it is not eight characters then.
fn typed_code(text: &str) -> Option<[u8; 8]> {
    let mut code = [0_u8; 8];
    let mut count = 0_usize;
    for byte in text.bytes() {
        if byte == b'-' || byte.is_ascii_whitespace() {
            continue;
        }
        let slot = code.get_mut(count)?;
        *slot = byte.to_ascii_uppercase();
        count += 1;
    }
    (count == code.len()).then_some(code)
}

/// What `GET /tool` asks for, checked.
struct Asked {
    challenge: [u8; 32],
    port: Option<u16>,
    machine: String,
}

/// The query of `GET /tool`: a `challenge` of 43 base64url characters, a
/// `port` of 1024 to 65535 when there is one, a `machine` of at most 64
/// bytes (control characters dropped); each at most once. Other parameters
/// are not looked at. `None` when it is not so.
fn read_query(query: Option<&str>) -> Option<Asked> {
    let mut challenge = None;
    let mut port = None;
    let mut machine = None;
    for (name, value) in form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        let slot = match name.as_ref() {
            "challenge" => &mut challenge,
            "port" => &mut port,
            "machine" => &mut machine,
            _ => continue,
        };
        if slot.is_some() {
            return None;
        }
        // Owned: percent-decoding makes new text; three short values once
        // a sign-in.
        *slot = Some(value.into_owned());
    }
    let challenge = challenge?;
    let fine = challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !fine {
        return None;
    }
    let challenge =
        <[u8; 32]>::try_from(URL_SAFE_NO_PAD.decode(challenge).ok()?.as_slice()).ok()?;
    let port = match port {
        None => None,
        Some(text) => {
            if text.is_empty() || text.len() > 5 || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let port: u16 = text.parse().ok()?;
            if port < LOWEST_PORT {
                return None;
            }
            Some(port)
        }
    };
    let machine = machine?;
    if machine.len() > MAX_MACHINE {
        return None;
    }
    let machine: String = machine.chars().filter(|c| !c.is_control()).collect();
    Some(Asked {
        challenge,
        port,
        machine,
    })
}

/// A sign-in awaiting an answer, as `POST /tool` finds it.
struct Awaiting {
    id: i64,
    browser: Vec<u8>,
    challenge: [u8; 32],
    port: Option<u16>,
    machine: String,
    user: String,
    name: Option<String>,
    email: Option<String>,
}

/// What `POST /tool` posted.
struct Posted {
    sign_in: String,
    answer: Answered,
    code: Option<String>,
}

enum Answered {
    Confirm,
    Decline,
}

/// The form of `POST /tool`: `sign_in`, `answer` (`confirm` or `decline`)
/// and `code`, each at most once; `None` when it is not such a form.
fn read_form(body: &[u8]) -> Option<Posted> {
    let mut sign_in = None;
    let mut answer = None;
    let mut code = None;
    for (name, value) in form_urlencoded::parse(body) {
        let slot = match name.as_ref() {
            "sign_in" => &mut sign_in,
            "answer" => &mut answer,
            "code" => &mut code,
            _ => continue,
        };
        if slot.is_some() {
            return None;
        }
        // Owned: decoded text; a few short values once a post.
        *slot = Some(value.into_owned());
    }
    let answer = match answer?.as_str() {
        "confirm" => Answered::Confirm,
        "decline" => Answered::Decline,
        _ => return None,
    };
    Some(Posted {
        sign_in: sign_in.filter(|s| !s.is_empty())?,
        answer,
        code,
    })
}

type Page = Response<Full<Bytes>>;

impl Oidc {
    /// Step 1: `GET /tool`.
    pub(super) async fn tool_begin(
        &self,
        connected: &Connected,
        parts: &Parts,
        port: Option<u16>,
    ) -> Page {
        let Some(asked) = read_query(parts.uri.query()) else {
            return pages::tool_failed(
                StatusCode::BAD_REQUEST,
                "This address is not one `vibe sign-in` makes: it needs a challenge of 43 base64url characters, a port of 1024 to 65535 when there is one, and the machine's name of at most 64 bytes, each once.",
            );
        };
        let configuration = match connected.provider.configuration(None).await {
            Ok(configuration) => configuration,
            Err(problem) => {
                return pages::tool_failed(
                    StatusCode::BAD_GATEWAY,
                    &format!(
                        "The company's sign-in could not be used: {}.",
                        problem.reason()
                    ),
                );
            }
        };
        let Some(browser) = self.browser_value(parts) else {
            return pages::unavailable(None);
        };
        let Some(begun) = self.states.begin_tool(
            &digest(&browser),
            &asked.challenge,
            asked.port,
            &asked.machine,
        ) else {
            return pages::unavailable(None);
        };
        let authorization = self.authorization(connected, &configuration, &begun, port);
        let cookie = self.sign_in_cookie(&browser, SIGN_IN_COOKIE_SECONDS);
        pages::redirect(authorization.as_str(), cookie.into_iter().collect())
    }

    /// Step 2: someone signed in at the provider; the sign-in now awaits an
    /// answer, and the page asks.
    pub(super) async fn tool_awaiting(
        &self,
        under_way: &ToolUnderWay,
        browser: &[u8; 32],
        tokens: &Tokens,
    ) -> Page {
        let Some(value) = Secret::random() else {
            return pages::unavailable(None);
        };
        let row_digest = value.digest();
        let sealed = match &tokens.refresh_token {
            Some(token) => match self.sessions.seal_token(token, &row_digest) {
                Some(sealed) => Some(sealed),
                None => return pages::unavailable(None),
            },
            None => None,
        };
        let claims = &tokens.claims;
        let port = under_way.port.map(i32::from);
        let stored = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                // The User's row, made or updated first, is locked to the
                // end: two sign-ins of one User take turns.
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .execute(
                        "INSERT INTO users (id, name, email) VALUES ($1, $2, $3)
                         ON CONFLICT (id) DO UPDATE
                             SET name = excluded.name, email = excluded.email, updated_at = now()",
                        &[&claims.sub, &claims.name, &claims.email],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM tool_sign_ins
                         WHERE made_at <= now() - $1::bigint * interval '1 second'",
                        &[&AWAIT_SECONDS],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "INSERT INTO tool_sign_ins (digest, browser, challenge, port, machine,
                             user_id, name, email, groups, refresh_token)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                        &[
                            &row_digest.as_slice(),
                            &browser.as_slice(),
                            &under_way.challenge.as_slice(),
                            &port,
                            &under_way.machine,
                            &claims.sub,
                            &claims.name,
                            &claims.email,
                            &claims.groups,
                            &sealed,
                        ],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM tool_sign_ins WHERE user_id = $1 AND id IN (
                             SELECT id FROM tool_sign_ins WHERE user_id = $1
                             ORDER BY id DESC OFFSET $2)",
                        &[&claims.sub, &MAX_AWAITING],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)
            })
            .await;
        if let Err(error) = stored {
            log_database(&error);
            return pages::unavailable(None);
        }
        pages::tool_ask(
            &Person {
                id: &claims.sub,
                name: claims.name.as_deref(),
                email: claims.email.as_deref(),
            },
            &under_way.machine,
            under_way.port,
            value.expose(),
            false,
        )
    }

    /// Step 3: `POST /tool`, from the page (its origin was checked
    /// already).
    pub(super) async fn tool_answer<B>(&self, parts: &Parts, body: B) -> Page
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let form = parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("application/x-www-form-urlencoded")
            });
        if !form {
            return pages::tool_not_a_form();
        }
        let read = Limited::new(body, MAX_FORM_BYTES).collect();
        let Ok(Ok(collected)) = tokio::time::timeout(FORM_READ_TIMEOUT, read).await else {
            return pages::tool_not_a_form();
        };
        let Some(posted) = read_form(&collected.to_bytes()) else {
            return pages::tool_not_a_form();
        };
        let Some(browser) =
            cookies::find(&parts.headers, cookies::sign_in_name(self.secure())).map(digest)
        else {
            return pages::tool_over();
        };
        let sign_in = digest(&posted.sign_in);
        let awaiting = match self.awaiting(&sign_in).await {
            Ok(Some(awaiting)) if same(&awaiting.browser, &browser) => awaiting,
            Ok(_) => return pages::tool_over(),
            Err(error) => {
                log_database(&error);
                return pages::unavailable(None);
            }
        };
        let done = match (posted.answer, awaiting.port) {
            (Answered::Decline, _) => self.declined(&awaiting).await,
            (Answered::Confirm, Some(port)) => self.confirmed_with_port(&awaiting, port).await,
            (Answered::Confirm, None) => {
                let right = posted
                    .code
                    .as_deref()
                    .and_then(typed_code)
                    .is_some_and(|typed| same(&typed, &check_code(&awaiting.challenge)));
                if right {
                    self.confirmed(&awaiting).await
                } else {
                    self.wrong_code(&awaiting, &posted.sign_in).await
                }
            }
        };
        done.unwrap_or_else(|error| {
            log_database(&error);
            pages::unavailable(None)
        })
    }

    /// The sign-in awaiting an answer whose `sign_in` value has this
    /// SHA-256, if it is not over.
    async fn awaiting(&self, sign_in: &[u8; 32]) -> Result<Option<Awaiting>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT id, browser, challenge, port, machine, user_id, name, email
                         FROM tool_sign_ins
                         WHERE digest = $1 AND NOT confirmed
                           AND made_at > now() - $2::bigint * interval '1 second'",
                        &[&sign_in.as_slice(), &AWAIT_SECONDS],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let bad = |_| DbError::unexpected("a sign-in of the tool");
        let challenge: Vec<u8> = row.try_get(2).map_err(bad)?;
        let port: Option<i32> = row.try_get(3).map_err(bad)?;
        Ok(Some(Awaiting {
            id: row.try_get(0).map_err(bad)?,
            browser: row.try_get(1).map_err(bad)?,
            challenge: <[u8; 32]>::try_from(challenge.as_slice())
                .map_err(|_| DbError::unexpected("a sign-in of the tool's challenge"))?,
            port: port.and_then(|p| u16::try_from(p).ok()),
            machine: row.try_get(4).map_err(bad)?,
            user: row.try_get(5).map_err(bad)?,
            name: row.try_get(6).map_err(bad)?,
            email: row.try_get(7).map_err(bad)?,
        }))
    }

    async fn declined(&self, awaiting: &Awaiting) -> Result<Page, DbError> {
        self.end_sign_in(awaiting.id).await?;
        Ok(pages::tool_declined())
    }

    async fn end_sign_in(&self, id: i64) -> Result<u64, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute("DELETE FROM tool_sign_ins WHERE id = $1", &[&id])
                    .await
                    .map_err(DbError::from)
            })
            .await
    }

    /// Marks it confirmed, with the grant's SHA-256 when there is one;
    /// `false` when it was answered meanwhile.
    async fn confirm(&self, id: i64, grant: Option<&[u8; 32]>) -> Result<bool, DbError> {
        let grant = grant.map(<[u8; 32]>::as_slice);
        let updated = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                // Never one that five wrong check codes ended: the count
                // and the end are one transaction under the row's lock
                // ([`Self::wrong_code`]), and this waits for it.
                client
                    .execute(
                        "UPDATE tool_sign_ins SET confirmed = true, grant_digest = $2
                         WHERE id = $1 AND NOT confirmed AND wrong_codes < $3",
                        &[&id, &grant, &MAX_WRONG_CODES],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        Ok(updated == 1)
    }

    /// Confirmed with a port: the browser goes on to the tool on this
    /// computer with a one-time grant. Only ever to `127.0.0.1`.
    async fn confirmed_with_port(&self, awaiting: &Awaiting, port: u16) -> Result<Page, DbError> {
        let Some(grant) = Secret::random() else {
            return Ok(pages::unavailable(None));
        };
        if !self.confirm(awaiting.id, Some(&grant.digest())).await? {
            return Ok(pages::tool_over());
        }
        let location = format!("http://127.0.0.1:{port}/?grant={}", grant.expose());
        Ok(pages::see_other(&location))
    }

    /// Confirmed with the right check code: the tool takes it at its next
    /// poll.
    async fn confirmed(&self, awaiting: &Awaiting) -> Result<Page, DbError> {
        if !self.confirm(awaiting.id, None).await? {
            return Ok(pages::tool_over());
        }
        Ok(pages::tool_signed_in())
    }

    /// A wrong check code: counted; the page again, or, at the fifth, the
    /// sign-in over.
    async fn wrong_code(&self, awaiting: &Awaiting, sign_in: &str) -> Result<Page, DbError> {
        let id = awaiting.id;
        // One act: the row locked, the count read, and then either counted
        // or, at the fifth, the sign-in deleted, in one transaction. A
        // confirm waits on the lock and then finds the row gone.
        let counted = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let row = transaction
                    .query_opt(
                        "SELECT wrong_codes FROM tool_sign_ins
                         WHERE id = $1 AND NOT confirmed FOR UPDATE",
                        &[&id],
                    )
                    .await
                    .map_err(DbError::from)?;
                let Some(row) = row else {
                    return Ok(None);
                };
                let wrong: i32 = row
                    .try_get(0)
                    .map_err(|_| DbError::unexpected("a count of wrong check codes"))?;
                let wrong = wrong.saturating_add(1);
                if wrong >= MAX_WRONG_CODES {
                    transaction
                        .execute("DELETE FROM tool_sign_ins WHERE id = $1", &[&id])
                        .await
                        .map_err(DbError::from)?;
                } else {
                    transaction
                        .execute(
                            "UPDATE tool_sign_ins SET wrong_codes = $2 WHERE id = $1",
                            &[&id, &wrong],
                        )
                        .await
                        .map_err(DbError::from)?;
                }
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Some(wrong))
            })
            .await?;
        let Some(wrong) = counted else {
            return Ok(pages::tool_over());
        };
        if wrong >= MAX_WRONG_CODES {
            log::record(
                "tool-sign-in-ended",
                json!({ "reason": "wrong-check-codes" }),
            );
            return Ok(pages::tool_over());
        }
        Ok(pages::tool_ask(
            &Person {
                id: &awaiting.user,
                name: awaiting.name.as_deref(),
                email: awaiting.email.as_deref(),
            },
            &awaiting.machine,
            None,
            sign_in,
            true,
        ))
    }

    /// Step 4: `POST /v1/tool-sign-ins`, by anyone, with `{ verifier,
    /// grant? }`.
    pub async fn take_tool_sign_in(&self, body: &[u8]) -> Answer {
        let not_confirmed = || {
            Answer::error(
                StatusCode::CONFLICT,
                "sign-in-not-confirmed",
                "no sign-in of the tool with this verifier has been confirmed in the browser (yet): ask again in a few seconds",
            )
        };
        let (verifier, grant) = match read_take(body) {
            Ok(read) => read,
            Err(answer) => return answer,
        };
        if verifier.len() > MAX_PRESENTED || grant.as_ref().is_some_and(|g| g.len() > MAX_PRESENTED)
        {
            return not_confirmed();
        }
        let Some(connected) = self.connected() else {
            return not_confirmed();
        };
        let challenge: [u8; 32] = Sha256::digest(verifier.as_bytes()).into();
        let found = match self.confirmed_of(&challenge).await {
            Ok(found) => found,
            Err(error) => return Answer::unavailable(&error),
        };
        let grant = grant.as_deref().map(digest);
        // The two ways are kept apart: a request that carries a grant,
        // whatever it is, takes only a sign-in begun with a port, and only
        // with that sign-in's grant; one without takes only a sign-in begun
        // without a port.
        let Some(taken) = found.into_iter().find(|c| match (c.with_port, &grant) {
            // Begun with a port, asked with a grant: the grant it was
            // confirmed with, and no other.
            (true, Some(given)) => c.grant.as_ref().is_some_and(|kept| same(kept, given)),
            // Begun with a port, asked without a grant: never.
            (true, None) => false,
            // Begun without a port, asked with a grant: never.
            (false, Some(_)) => false,
            // Begun without a port, asked without a grant: its check code
            // was typed in the browser.
            (false, None) => true,
        }) else {
            return not_confirmed();
        };
        let user = User {
            id: taken.user,
            groups: taken.groups,
        };
        let refresh = taken
            .refresh_token
            .as_deref()
            .and_then(|sealed| self.sessions.open_token(sealed, &taken.digest));
        let seal = |credential: &[u8; 32]| -> Result<Option<Vec<u8>>, ()> {
            match &refresh {
                Some(token) => self
                    .sessions
                    .seal_token(token, credential)
                    .map(Some)
                    .ok_or(()),
                None => Ok(None),
            }
        };
        let made = self
            .credentials
            .make_from_sign_in(
                &user,
                &FromSignIn {
                    sign_in: taken.id,
                    seal: &seal,
                },
            )
            .await;
        let made = match made {
            Ok(Some(made)) => made,
            Ok(None) => return not_confirmed(),
            Err(MakeError::Database(error)) => return Answer::unavailable(&error),
            Err(MakeError::Randomness) => {
                log::record("randomness-failed", json!({ "for": "a tool credential" }));
                return Answer::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "the platform could not make a credential: try again shortly",
                );
            }
        };
        let administrator = user
            .groups
            .iter()
            .any(|g| g.as_str() == &*connected.administrators);
        Answer::json(
            StatusCode::CREATED,
            &json!({
                "credential": made.credential.expose(),
                "user": user.id,
                "groups": user.groups,
                "platform_administrator": administrator,
                "expires_at": made.expires_at,
            }),
        )
    }

    /// The confirmed sign-ins of this challenge that are not over, oldest
    /// first, at most [`MAX_SAME_CHALLENGE`].
    async fn confirmed_of(&self, challenge: &[u8; 32]) -> Result<Vec<Confirmed>, DbError> {
        let rows = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query(
                        "SELECT id, digest, port IS NOT NULL, grant_digest, user_id, groups,
                                refresh_token
                         FROM tool_sign_ins
                         WHERE challenge = $1 AND confirmed
                           AND made_at > now() - $2::bigint * interval '1 second'
                         ORDER BY id LIMIT $3",
                        &[&challenge.as_slice(), &AWAIT_SECONDS, &MAX_SAME_CHALLENGE],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let bad = |_| DbError::unexpected("a confirmed sign-in of the tool");
        let mut confirmed = Vec::with_capacity(rows.len());
        for row in &rows {
            let digest: Vec<u8> = row.try_get(1).map_err(bad)?;
            let grant: Option<Vec<u8>> = row.try_get(3).map_err(bad)?;
            confirmed.push(Confirmed {
                id: row.try_get(0).map_err(bad)?,
                digest: <[u8; 32]>::try_from(digest.as_slice())
                    .map_err(|_| DbError::unexpected("a sign-in of the tool's digest"))?,
                with_port: row.try_get(2).map_err(bad)?,
                grant: grant.and_then(|g| <[u8; 32]>::try_from(g.as_slice()).ok()),
                user: row.try_get(4).map_err(bad)?,
                groups: row.try_get(5).map_err(bad)?,
                refresh_token: row.try_get(6).map_err(bad)?,
            });
        }
        Ok(confirmed)
    }
}

/// A confirmed sign-in of the tool.
struct Confirmed {
    id: i64,
    digest: [u8; 32],
    with_port: bool,
    grant: Option<[u8; 32]>,
    user: String,
    groups: Vec<String>,
    refresh_token: Option<Vec<u8>>,
}

/// The body of `POST /v1/tool-sign-ins`: a JSON object with a string
/// `verifier` and, if any, a string `grant`.
fn read_take(body: &[u8]) -> Result<(String, Option<String>), Answer> {
    let malformed = |problem: &str| {
        Answer::error(
            StatusCode::BAD_REQUEST,
            "malformed",
            &format!(
                "the body is not the JSON asked for ({problem}): send {{ \"verifier\": \"<verifier>\", \"grant\": \"<grant>\" (with a port) }}"
            ),
        )
    };
    let value: Value = serde_json::from_slice(body).map_err(|_| malformed("it is not JSON"))?;
    let Value::Object(mut object) = value else {
        return Err(malformed("it is not a JSON object"));
    };
    let verifier = match object.remove("verifier") {
        Some(Value::String(verifier)) => verifier,
        _ => return Err(malformed("verifier is missing, or is not a string")),
    };
    let grant = match object.remove("grant") {
        None | Some(Value::Null) => None,
        Some(Value::String(grant)) => Some(grant),
        Some(_) => return Err(malformed("grant is not a string")),
    };
    Ok((verifier, grant))
}
