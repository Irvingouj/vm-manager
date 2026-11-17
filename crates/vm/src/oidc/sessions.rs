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
//! sessions: the oldest ends when one more begins. Every ended session,
//! anyone's, is deleted whenever anyone signs in (by the index on
//! `ends_at`), its sealed refresh token with it. An Application session is deleted with its
//! session (`ON DELETE CASCADE`), and a new one of the same session for
//! the same Application replaces the one before. A code is deleted when it
//! is shown, and those older than a minute whenever a code is made.
//!
//! **Renewal.** A session last renewed more than `--session-renewal` ago
//! is renewed before the request that finds it so goes on, with its
//! refresh token. The renewal is a task of the platform's, not the
//! request's ([`Sessions::renew`]):
//!
//! - **One per session.** The first request to find its session due
//!   starts the task; every request of that session that comes before it
//!   ends waits for it and takes its outcome (the User, no session, or
//!   unavailable). The provider is asked once, also when it fails.
//! - **Bounded.** At most [`MAX_RENEWALS`] sessions are being renewed at
//!   once; a request whose session's renewal cannot begin is
//!   [`Current::Unavailable`] (`503`). Every call to the provider ends by
//!   the renewal's deadline, [`RENEWAL_LIMIT`] after it began; a request
//!   waits at most [`RENEWAL_WAIT`], which is that and a second for the
//!   platform database's two calls.
//! - **Owned.** The tasks are held in a `JoinSet` of the [`Sessions`],
//!   reaped as new ones start. A request that goes away does not end its
//!   renewal: the new refresh token is written whatever became of the
//!   request. When the program stops, the [`Sessions`] go with the
//!   Ingress that holds them once every connection has ended, and the set
//!   ends every renewal still running then.
//! - **No lock across an await.** The map of renewals under way is behind
//!   a `std` mutex held only to look up, insert or remove; the outcome
//!   goes to the waiters through a `watch` channel.
//!
//! The Groups read before are never used past their time: a provider that
//! does not answer is [`Current::Unavailable`] for every request, and the
//! session stays.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;
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

/// The longest a renewal may take with the provider, from its start: the
/// configuration, the token endpoint and the keys together.
pub const RENEWAL_LIMIT: Duration = Duration::from_secs(10);

/// The longest a request waits for its session's renewal: the renewal's
/// limit, and a second for its reads and writes in the platform database
/// (each of which has its own limit besides). Past it the request is
/// [`Current::Unavailable`]; the renewal goes on to its end.
pub const RENEWAL_WAIT: Duration = Duration::from_secs(11);

/// The most sessions being renewed at once.
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

/// What a renewal came to, for every request that waited for it.
#[derive(Clone)]
enum Outcome {
    User(User),
    None,
    Unavailable,
}

/// The renewal's work, shared with its tasks.
struct Renewal {
    database: Arc<PlatformDatabase>,
    provider: Arc<Provider>,
    sealer: Sealer,
    renewal_seconds: i64,
    /// The renewals under way, by session: where each one's outcome comes.
    /// At most [`MAX_RENEWALS`]; an entry goes when its task ends, however
    /// it ends ([`Entry`]).
    under_way: Mutex<HashMap<i64, watch::Receiver<Option<Outcome>>>>,
}

/// A task's entry in [`Renewal::under_way`], removed when the task ends
/// (after its outcome is sent) or is ended.
struct Entry {
    renewal: Arc<Renewal>,
    session: i64,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.renewal
            .under_way
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.session);
    }
}

/// The sessions.
pub struct Sessions {
    database: Arc<PlatformDatabase>,
    renewal: Arc<Renewal>,
    lifetime_seconds: i64,
    /// The renewals' tasks, owned here (see the module's text).
    tasks: Mutex<JoinSet<()>>,
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
            renewal: Arc::new(Renewal {
                database: Arc::clone(&database),
                provider,
                sealer,
                renewal_seconds: seconds(renewal),
                under_way: Mutex::new(HashMap::new()),
            }),
            database,
            lifetime_seconds: seconds(lifetime),
            tasks: Mutex::new(JoinSet::new()),
        }
    }

    /// Begins a platform session for the person of `claims`, under the
    /// cookie whose SHA-256 is `digest`: their record made or brought up to
    /// date, every ended session (anyone's) deleted, and theirs past the
    /// newest [`MAX_SESSIONS`] ended. Answers the session's id.
    pub async fn begin(
        &self,
        digest: &[u8; 32],
        claims: &Claims,
        refresh_token: Option<&Secret>,
    ) -> Result<i64, DbError> {
        let sealed = match refresh_token {
            Some(token) => Some(
                self.renewal
                    .sealer
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
                    .execute("DELETE FROM sessions WHERE ends_at <= now()", &[])
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
        let renewal = self.renewal.renewal_seconds;
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
        let renewal = self.renewal.renewal_seconds;
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

    /// The outcome of the session's renewal: the one under way, or one
    /// begun now (see the module's text).
    async fn renew(&self, session: i64) -> Current {
        let (mut outcome, start) = {
            let mut under_way = self
                .renewal
                .under_way
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(outcome) = under_way.get(&session) {
                (outcome.clone(), None)
            } else if under_way.len() >= MAX_RENEWALS {
                return Current::Unavailable;
            } else {
                let (sender, outcome) = watch::channel(None);
                under_way.insert(session, outcome.clone());
                (outcome, Some(sender))
            }
        };
        if let Some(sender) = start {
            let entry = Entry {
                renewal: Arc::clone(&self.renewal),
                session,
            };
            let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            while tasks.try_join_next().is_some() {}
            tasks.spawn(async move {
                let came = entry.renewal.run(session).await;
                sender.send_replace(Some(came));
                // The entry goes after the outcome is sent: a request that
                // comes later reads the session anew.
                drop(entry);
            });
        }
        let waited = tokio::time::timeout(RENEWAL_WAIT, outcome.wait_for(Option::is_some)).await;
        let Ok(Ok(came)) = waited else {
            return Current::Unavailable;
        };
        match came.as_ref() {
            // Copied: each request that waited takes its own User; once a
            // renewal per request, a name and the Groups.
            Some(Outcome::User(user)) => Current::User {
                session,
                user: user.clone(),
            },
            Some(Outcome::None) => Current::None,
            Some(Outcome::Unavailable) | None => Current::Unavailable,
        }
    }

    /// Ends a session, and every Application session made from it.
    pub async fn end(&self, session: i64) -> Result<(), DbError> {
        end_session(&self.database, session).await
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

impl Renewal {
    /// One renewal, to its end: the provider asked at most once, every call
    /// ended by [`RENEWAL_LIMIT`] from now.
    async fn run(&self, session: i64) -> Outcome {
        let deadline = Instant::now() + RENEWAL_LIMIT;
        let state = match self.renewal_state(session).await {
            Ok(Some(state)) => state,
            Ok(None) => return Outcome::None,
            Err(error) => {
                log_database(&error);
                return Outcome::Unavailable;
            }
        };
        if !state.due {
            // Renewed meanwhile, by a renewal that ended before this began.
            return Outcome::User(state.user);
        }
        let Some(sealed) = state.refresh_token.as_deref() else {
            return self.end_at_renewal(session, "no-refresh-token").await;
        };
        let Some(token) = self.sealer.open(sealed, &state.digest) else {
            return self
                .end_at_renewal(session, "unreadable-refresh-token")
                .await;
        };
        let tokens = match self.provider.refresh(&token, deadline).await {
            Ok(tokens) if tokens.claims.sub == state.user.id => tokens,
            Ok(_) => return self.end_at_renewal(session, "another-sub").await,
            Err(Problem::Unanswered(_)) => return Outcome::Unavailable,
            Err(Problem::Refused) => return self.end_at_renewal(session, "refused").await,
            Err(Problem::NoIdToken) => return self.end_at_renewal(session, "no-id-token").await,
            Err(Problem::Invalid(_)) => {
                return self.end_at_renewal(session, "invalid-id-token").await;
            }
        };
        let sealed = match &tokens.refresh_token {
            Some(token) => match self.sealer.seal(token, &state.digest) {
                Some(sealed) => Some(sealed),
                None => return Outcome::Unavailable,
            },
            None => None,
        };
        match self.renewed(session, &tokens.claims, sealed).await {
            Ok(true) => Outcome::User(User {
                id: tokens.claims.sub,
                groups: tokens.claims.groups,
            }),
            Ok(false) => Outcome::None,
            Err(error) => {
                log_database(&error);
                Outcome::Unavailable
            }
        }
    }

    async fn end_at_renewal(&self, session: i64, why: &'static str) -> Outcome {
        log::record(
            "session-ended",
            json!({ "session": session, "reason": why }),
        );
        match end_session(&self.database, session).await {
            Ok(()) => Outcome::None,
            Err(error) => {
                log_database(&error);
                Outcome::Unavailable
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
}

/// Ends a session, and every Application session made from it.
async fn end_session(database: &PlatformDatabase, session: i64) -> Result<(), DbError> {
    database
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
