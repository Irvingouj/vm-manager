//! Sessions in the platform database (blackbox/CONTRACT.md, "Sign-in and
//! sessions"): the User's record, the platform sessions, the sessions of
//! each Application made from them, and the one-time codes between the
//! two; and the renewal that reads a session's Groups again.
//!
//! **What is stored.** Only the SHA-256 of a cookie value or a code: what
//! is here signs nobody in. A session's refresh token is sealed
//! (crate::oidc::seal). Every time is the platform database's clock
//! (`now()`), so a session ends at its time whatever the program holds.
//!
//! **What bounds the tables.** A User holds at most [`MAX_SESSIONS`]
//! sessions: the oldest ends when one more begins, and that User's ended
//! ones are deleted then. An Application session is deleted with its
//! session (`ON DELETE CASCADE`), and a new one of the same session for
//! the same Application replaces the one before. A code is deleted when it
//! is shown, and those older than a minute whenever a code is made.
//!
//! **Renewal.** A session last renewed more than `--session-renewal` ago
//! is renewed by the first request that finds it so, with its refresh
//! token, before that request goes on; the other requests of the same
//! session wait for that one renewal ([`Turns`]), then read the session
//! again. The Groups read before are never used past their time: a
//! provider that does not answer is [`Current::Unavailable`] for every one
//! of them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::OwnedMutexGuard;
use tokio_postgres::{Client, Row};

use super::provider::{Problem, Provider};
use super::seal::Sealer;
use super::secret::Secret;
use super::token::Claims;
use crate::log;
use crate::platform_db::{DbError, PlatformDatabase, rfc3339};
use crate::user::User;

/// The most sessions a User holds.
pub const MAX_SESSIONS: i64 = 20;

/// How long a one-time code is good for, in seconds.
pub const CODE_SECONDS: i64 = 60;

/// The longest a request waits for its session's renewal under way.
pub const RENEWAL_WAIT: Duration = Duration::from_secs(25);

/// The longest a renewal may take with the provider, all its calls
/// together (each ends within 10 seconds).
pub const RENEWAL_LIMIT: Duration = Duration::from_secs(20);

/// The most sessions being renewed, or waited for, at once.
pub const MAX_RENEWALS: usize = 1024;

/// A session found by a cookie.
pub struct Found {
    pub id: i64,
    pub user: User,
    /// Whether it is due for renewal.
    pub due: bool,
}

/// What a request's session comes to.
pub enum Current {
    /// A session, its User as they are now.
    User { session: i64, user: User },
    /// None (none at all, ended, or ended at this renewal).
    None,
    /// It cannot be known now: the platform database or the provider did
    /// not answer.
    Unavailable,
}

/// A one-time code shown, used up.
pub struct TakenCode {
    pub session: i64,
    pub application: String,
    pub state_digest: Vec<u8>,
    /// Whether it was at most [`CODE_SECONDS`] old.
    pub fresh: bool,
}

/// The waits of the requests of each session being renewed: one turn per
/// session, held by the request that renews it. Behind a `std` mutex held
/// only to look up, insert or remove, never across an await. At most
/// [`MAX_RENEWALS`] entries: one more is refused (the request is `503`).
#[derive(Default)]
struct Turns {
    by_session: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

/// A request's turn at its session; let go when dropped, however the
/// request ended.
struct Turn<'a> {
    turns: &'a Turns,
    session: i64,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut map = self
            .turns
            .by_session
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        drop(self.guard.take());
        // Each waiter holds its own handle (taken under this lock); none
        // left but the map's: no one waits, the entry goes.
        if map
            .get(&self.session)
            .is_some_and(|gate| Arc::strong_count(gate) == 1)
        {
            map.remove(&self.session);
        }
    }
}

impl Turns {
    /// Waits for the turn at `session`, at most [`RENEWAL_WAIT`]; `None`
    /// when it did not come in time or there is no room.
    async fn wait(&self, session: i64) -> Option<Turn<'_>> {
        let gate = {
            let mut map = self
                .by_session
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if map.len() >= MAX_RENEWALS && !map.contains_key(&session) {
                // Entries left by requests that ended while waiting.
                map.retain(|_, gate| Arc::strong_count(gate) > 1);
            }
            if map.len() >= MAX_RENEWALS && !map.contains_key(&session) {
                return None;
            }
            Arc::clone(map.entry(session).or_default())
        };
        let mut turn = Turn {
            turns: self,
            session,
            guard: None,
        };
        turn.guard = Some(
            tokio::time::timeout(RENEWAL_WAIT, gate.lock_owned())
                .await
                .ok()?,
        );
        Some(turn)
    }
}

/// The sessions.
pub struct Sessions {
    database: Arc<PlatformDatabase>,
    provider: Arc<Provider>,
    sealer: Sealer,
    renewal_seconds: i64,
    lifetime_seconds: i64,
    turns: Turns,
}

fn unexpected(what: &'static str) -> impl Fn(tokio_postgres::Error) -> DbError {
    move |_| DbError::unexpected(what)
}

fn found(row: &Row) -> Result<Found, DbError> {
    let bad = unexpected("a session");
    Ok(Found {
        id: row.try_get(0).map_err(&bad)?,
        user: User {
            id: row.try_get(1).map_err(&bad)?,
            groups: row.try_get(2).map_err(&bad)?,
        },
        due: row.try_get(3).map_err(&bad)?,
    })
}

impl Sessions {
    pub fn new(
        database: Arc<PlatformDatabase>,
        provider: Arc<Provider>,
        sealer: Sealer,
        renewal: Duration,
        lifetime: Duration,
    ) -> Self {
        let seconds = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX).max(1);
        Self {
            database,
            provider,
            sealer,
            renewal_seconds: seconds(renewal),
            lifetime_seconds: seconds(lifetime),
            turns: Turns::default(),
        }
    }

    /// Begins a platform session for the person of `claims`, under the
    /// cookie whose SHA-256 is `digest`: their record made or brought up to
    /// date, their ended sessions deleted, and those past the newest
    /// [`MAX_SESSIONS`] ended. Answers the session's id.
    pub async fn begin(
        &self,
        digest: &[u8; 32],
        claims: &Claims,
        refresh_token: Option<&Secret>,
    ) -> Result<i64, DbError> {
        let sealed = match refresh_token {
            Some(token) => Some(
                self.sealer
                    .seal(token, digest)
                    .ok_or_else(|| DbError::unexpected("no randomness to seal a refresh token"))?,
            ),
            None => None,
        };
        let lifetime = self.lifetime_seconds;
        self.database
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
                        "DELETE FROM sessions WHERE user_id = $1 AND ends_at <= now()",
                        &[&claims.sub],
                    )
                    .await
                    .map_err(DbError::from)?;
                let row = transaction
                    .query_one(
                        "INSERT INTO sessions (digest, user_id, groups, refresh_token, ends_at)
                         VALUES ($1, $2, $3, $4, now() + $5::bigint * interval '1 second')
                         RETURNING id",
                        &[
                            &digest.as_slice(),
                            &claims.sub,
                            &claims.groups,
                            &sealed,
                            &lifetime,
                        ],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM sessions WHERE user_id = $1 AND id IN (
                             SELECT id FROM sessions WHERE user_id = $1
                             ORDER BY id DESC OFFSET $2)",
                        &[&claims.sub, &MAX_SESSIONS],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                row.try_get(0)
                    .map_err(|_| DbError::unexpected("a session's id"))
            })
            .await
    }

    /// The platform session of a cookie, if it has not ended.
    pub async fn platform(&self, digest: &[u8; 32]) -> Result<Option<Found>, DbError> {
        let renewal = self.renewal_seconds;
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT id, user_id, groups,
                                renewed_at + $2::bigint * interval '1 second' <= now()
                         FROM sessions WHERE digest = $1 AND ends_at > now()",
                        &[&digest.as_slice(), &renewal],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        row.as_ref().map(found).transpose()
    }

    /// The session of an Application's cookie, if it is that Application's
    /// and its platform session has not ended.
    pub async fn application(
        &self,
        digest: &[u8; 32],
        application: &str,
    ) -> Result<Option<Found>, DbError> {
        let renewal = self.renewal_seconds;
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT s.id, s.user_id, s.groups,
                                s.renewed_at + $3::bigint * interval '1 second' <= now()
                         FROM application_sessions a JOIN sessions s ON s.id = a.session_id
                         WHERE a.digest = $1 AND a.application = $2 AND s.ends_at > now()",
                        &[&digest.as_slice(), &application, &renewal],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        row.as_ref().map(found).transpose()
    }

    /// The session as it is now: renewed first when it is due.
    pub async fn current(&self, found: Result<Option<Found>, DbError>) -> Current {
        match found {
            Ok(Some(found)) if !found.due => Current::User {
                session: found.id,
                user: found.user,
            },
            Ok(Some(found)) => self.renew(found.id).await,
            Ok(None) => Current::None,
            Err(error) => {
                log_database(&error);
                Current::Unavailable
            }
        }
    }

    /// Renews a session due for it, the requests of one session taking
    /// turns: whoever comes after a renewal finds it done.
    async fn renew(&self, session: i64) -> Current {
        let Some(_turn) = self.turns.wait(session).await else {
            return Current::Unavailable;
        };
        let state = match self.renewal_state(session).await {
            Ok(Some(state)) => state,
            Ok(None) => return Current::None,
            Err(error) => {
                log_database(&error);
                return Current::Unavailable;
            }
        };
        if !state.due {
            return Current::User {
                session,
                user: state.user,
            };
        }
        let token = state
            .refresh_token
            .as_deref()
            .and_then(|sealed| self.sealer.open(sealed, &state.digest));
        let Some(token) = token else {
            return self.end_at_renewal(session, "no-refresh-token").await;
        };
        let renewed = tokio::time::timeout(RENEWAL_LIMIT, self.provider.refresh(&token))
            .await
            .unwrap_or(Err(Problem::Unanswered("the renewal took too long")));
        let tokens = match renewed {
            Ok(tokens) if tokens.claims.sub == state.user.id => tokens,
            Ok(_) => return self.end_at_renewal(session, "another-sub").await,
            Err(Problem::Unanswered(_)) => return Current::Unavailable,
            Err(Problem::Refused) => return self.end_at_renewal(session, "refused").await,
            Err(Problem::NoIdToken) => return self.end_at_renewal(session, "no-id-token").await,
            Err(Problem::Invalid(_)) => {
                return self.end_at_renewal(session, "invalid-id-token").await;
            }
        };
        let sealed = match &tokens.refresh_token {
            Some(token) => match self.sealer.seal(token, &state.digest) {
                Some(sealed) => Some(sealed),
                None => return Current::Unavailable,
            },
            None => None,
        };
        match self.renewed(session, &tokens.claims, sealed).await {
            Ok(true) => Current::User {
                session,
                user: User {
                    id: tokens.claims.sub,
                    groups: tokens.claims.groups,
                },
            },
            Ok(false) => Current::None,
            Err(error) => {
                log_database(&error);
                Current::Unavailable
            }
        }
    }

    async fn end_at_renewal(&self, session: i64, why: &'static str) -> Current {
        log::record(
            "session-ended",
            json!({ "session": session, "reason": why }),
        );
        match self.end(session).await {
            Ok(()) => Current::None,
            Err(error) => {
                log_database(&error);
                Current::Unavailable
            }
        }
    }

    async fn renewal_state(&self, session: i64) -> Result<Option<RenewalState>, DbError> {
        let renewal = self.renewal_seconds;
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "SELECT user_id, groups,
                                renewed_at + $2::bigint * interval '1 second' <= now(),
                                refresh_token, digest
                         FROM sessions WHERE id = $1 AND ends_at > now()",
                        &[&session, &renewal],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let bad = unexpected("a session");
        let digest: Vec<u8> = row.try_get(4).map_err(&bad)?;
        Ok(Some(RenewalState {
            user: User {
                id: row.try_get(0).map_err(&bad)?,
                groups: row.try_get(1).map_err(&bad)?,
            },
            due: row.try_get(2).map_err(&bad)?,
            refresh_token: row.try_get(3).map_err(&bad)?,
            digest: <[u8; 32]>::try_from(digest.as_slice())
                .map_err(|_| DbError::unexpected("a session's digest"))?,
        }))
    }

    /// Records a renewal; `false` when the session ended meanwhile.
    async fn renewed(
        &self,
        session: i64,
        claims: &Claims,
        refresh_token: Option<Vec<u8>>,
    ) -> Result<bool, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                let updated = transaction
                    .execute(
                        "UPDATE sessions
                         SET groups = $2, renewed_at = now(),
                             refresh_token = coalesce($3, refresh_token)
                         WHERE id = $1 AND ends_at > now()",
                        &[&session, &claims.groups, &refresh_token],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction
                    .execute(
                        "UPDATE users SET name = $2, email = $3, updated_at = now() WHERE id = $1",
                        &[&claims.sub, &claims.name, &claims.email],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(updated == 1)
            })
            .await
    }

    /// Ends a session, and every Application session made from it.
    pub async fn end(&self, session: i64) -> Result<(), DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute("DELETE FROM sessions WHERE id = $1", &[&session])
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        Ok(())
    }

    /// Ends the platform session of a cookie, if there is one.
    pub async fn end_by_cookie(&self, digest: &[u8; 32]) -> Result<(), DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "DELETE FROM sessions WHERE digest = $1",
                        &[&digest.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        Ok(())
    }

    /// Makes a one-time code for `application` from `session`, bound to the
    /// step-1 cookie's value by its SHA-256; deletes the codes older than a
    /// minute.
    pub async fn make_code(
        &self,
        session: i64,
        application: &str,
        code: &[u8; 32],
        state: &[u8; 32],
    ) -> Result<(), DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .execute(
                        "DELETE FROM session_codes
                         WHERE made_at < now() - $1::bigint * interval '1 second'",
                        &[&CODE_SECONDS],
                    )
                    .await
                    .map_err(DbError::from)?;
                client
                    .execute(
                        "INSERT INTO session_codes (digest, session_id, application, state_digest)
                         VALUES ($1, $2, $3, $4)",
                        &[&code.as_slice(), &session, &application, &state.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        Ok(())
    }

    /// The code whose SHA-256 is `code`, used up whatever comes of it.
    pub async fn take_code(&self, code: &[u8; 32]) -> Result<Option<TakenCode>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        "DELETE FROM session_codes WHERE digest = $1
                         RETURNING session_id, application, state_digest,
                                   made_at > now() - $2::bigint * interval '1 second'",
                        &[&code.as_slice(), &CODE_SECONDS],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let bad = unexpected("a one-time code");
        Ok(Some(TakenCode {
            session: row.try_get(0).map_err(&bad)?,
            application: row.try_get(1).map_err(&bad)?,
            state_digest: row.try_get(2).map_err(&bad)?,
            fresh: row.try_get(3).map_err(&bad)?,
        }))
    }

    /// Begins `application`'s session from `session` under the cookie whose
    /// SHA-256 is `digest`, replacing the one before; `false` when the
    /// platform session has ended.
    pub async fn begin_application(
        &self,
        session: i64,
        application: &str,
        digest: &[u8; 32],
    ) -> Result<bool, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .execute(
                        "DELETE FROM application_sessions WHERE session_id = $1 AND application = $2",
                        &[&session, &application],
                    )
                    .await
                    .map_err(DbError::from)?;
                let made = transaction
                    .execute(
                        "INSERT INTO application_sessions (digest, session_id, application)
                         SELECT $3, id, $2 FROM sessions WHERE id = $1 AND ends_at > now()",
                        &[&session, &application, &digest.as_slice()],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(made == 1)
            })
            .await
    }

    /// `/me`'s answer for a session.
    pub async fn record(&self, session: i64) -> Result<Option<Value>, DbError> {
        let row = self
            .database
            .pool()
            .run(async |client: &mut Client| {
                client
                    .query_opt(
                        concat!(
                            "SELECT u.id, u.name, u.email, s.groups, ",
                            rfc3339!("u.first_sign_in"),
                            ", ",
                            rfc3339!("s.started_at"),
                            ", ",
                            rfc3339!("s.renewed_at"),
                            " FROM sessions s JOIN users u ON u.id = s.user_id
                              WHERE s.id = $1 AND s.ends_at > now()"
                        ),
                        &[&session],
                    )
                    .await
                    .map_err(DbError::from)
            })
            .await?;
        let Some(row) = row else { return Ok(None) };
        let bad = unexpected("a User's record");
        Ok(Some(json!({
            "id": row.try_get::<_, String>(0).map_err(&bad)?,
            "name": row.try_get::<_, Option<String>>(1).map_err(&bad)?,
            "email": row.try_get::<_, Option<String>>(2).map_err(&bad)?,
            "groups": row.try_get::<_, Vec<String>>(3).map_err(&bad)?,
            "first_sign_in": row.try_get::<_, String>(4).map_err(&bad)?,
            "session_started": row.try_get::<_, String>(5).map_err(&bad)?,
            "session_renewed": row.try_get::<_, String>(6).map_err(&bad)?,
        })))
    }
}

struct RenewalState {
    user: User,
    due: bool,
    refresh_token: Option<Vec<u8>>,
    digest: [u8; 32],
}

/// The platform database could not be used: the log says why (its message
/// holds no value of a request's).
pub fn log_database(error: &DbError) {
    log::record(
        "sessions-unavailable",
        json!({ "error": error.to_string() }),
    );
}
