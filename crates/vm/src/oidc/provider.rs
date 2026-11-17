//! The identity provider as the platform uses it (blackbox/CONTRACT.md,
//! "Sign-in and sessions: Starting with OIDC" and "The way through
//! sign-in"): its configuration and its keys, fetched when first needed,
//! and the token endpoint, for a code and for a refresh token.
//!
//! **Kept.** The configuration (`<issuer>/.well-known/openid-configuration`)
//! and the keys (its `jwks_uri`) are each kept for at most [`KEPT_FOR`],
//! then fetched again when next needed. A token that names a key not among
//! them has the keys fetched again, at most once every [`REFETCH_EVERY`].
//! What is kept is bounded: one configuration, at most
//! [`token::MAX_KEYS`] keys, each from an answer of at most
//! [`http::MAX_BODY_BYTES`]. The caches are behind `std` mutexes held only
//! to read or replace them, never across a call; two requests that both
//! find nothing kept may both fetch, which is harmless.
//!
//! Every call ends within [`http::CALL_TIMEOUT`].

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hyper::header::HeaderValue;
use serde_json::Value;
use url::Url;
use url::form_urlencoded::{Serializer, byte_serialize};

use super::http::{Client, Failure};
use super::secret::Secret;
use super::settings::Settings;
use super::token::{self, Claims, Expected, Keys, Refused};
use crate::log;

/// How long the configuration and the keys are kept.
pub const KEPT_FOR: Duration = Duration::from_secs(60 * 60);

/// The keys are fetched again for an unknown key at most this often.
pub const REFETCH_EVERY: Duration = Duration::from_secs(60);

/// The longest an endpoint's address may be.
const MAX_ADDRESS_BYTES: usize = 2048;

/// The longest refresh token kept; a longer one is as none.
const MAX_REFRESH_TOKEN_BYTES: usize = 16 * 1024;

/// What went wrong with the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// It could not be reached, did not answer in time, or answered what
    /// cannot be read (larger than the bound, not JSON, an error of its
    /// own, a configuration or keys without what is needed).
    Unanswered(&'static str),
    /// The token endpoint refused (`invalid_grant`, and every other
    /// refusal it gives with a `4xx`).
    Refused,
    /// Its answer has no ID token.
    NoIdToken,
    /// The ID token does not hold.
    Invalid(&'static str),
}

/// The provider's configuration, as far as the platform uses it.
pub struct Configuration {
    pub authorization_endpoint: Url,
    token_endpoint: Url,
    jwks_uri: Url,
    /// Whether the client authenticates with `client_secret_post`: when
    /// the configuration names only that.
    post_secret: bool,
}

/// What a token endpoint answered that holds.
pub struct Tokens {
    pub claims: Claims,
    pub refresh_token: Option<Secret>,
}

struct Kept<T> {
    value: Arc<T>,
    at: Instant,
}

#[derive(Default)]
struct KeyState {
    kept: Option<Kept<Keys>>,
    /// When the keys were last asked for, whatever came of it.
    asked: Option<Instant>,
}

/// The identity provider.
pub struct Provider {
    settings: Arc<Settings>,
    http: Client,
    configuration: Mutex<Option<Kept<Configuration>>>,
    keys: Mutex<KeyState>,
}

impl Provider {
    pub fn new(settings: Arc<Settings>) -> Result<Self, String> {
        Ok(Self {
            settings,
            http: Client::new()?,
            configuration: Mutex::new(None),
            keys: Mutex::new(KeyState::default()),
        })
    }

    /// The configuration, kept or fetched.
    pub async fn configuration(&self) -> Result<Arc<Configuration>, Problem> {
        {
            let kept = self
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(kept) = kept.as_ref().filter(|k| k.at.elapsed() < KEPT_FOR) {
                return Ok(Arc::clone(&kept.value));
            }
        }
        let address = self
            .settings
            .configuration_address()
            .ok_or(Problem::Unanswered(
                "the configuration's address cannot be made",
            ))?;
        let json = self.get_json(&address, "configuration").await?;
        let configuration = Arc::new(self.read_configuration(&json)?);
        *self
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Kept {
            value: Arc::clone(&configuration),
            at: Instant::now(),
        });
        Ok(configuration)
    }

    fn read_configuration(&self, json: &Value) -> Result<Configuration, Problem> {
        let unreadable = Problem::Unanswered("its configuration lacks what is needed");
        if json.get("issuer").and_then(Value::as_str) != Some(self.settings.issuer.as_str()) {
            return Err(Problem::Unanswered(
                "its configuration names another issuer",
            ));
        }
        let plain = self.settings.plain_http_allowed();
        let endpoint = |name: &str| -> Option<Url> {
            let text = json.get(name)?.as_str()?;
            if text.len() > MAX_ADDRESS_BYTES {
                return None;
            }
            let url = Url::parse(text).ok()?;
            let scheme_ok = url.scheme() == "https" || (plain && url.scheme() == "http");
            (scheme_ok && url.host().is_some() && url.fragment().is_none()).then_some(url)
        };
        let methods: Vec<&str> = json
            .get("token_endpoint_auth_methods_supported")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let post_secret =
            methods.contains(&"client_secret_post") && !methods.contains(&"client_secret_basic");
        Ok(Configuration {
            authorization_endpoint: endpoint("authorization_endpoint").ok_or(unreadable)?,
            token_endpoint: endpoint("token_endpoint").ok_or(unreadable)?,
            jwks_uri: endpoint("jwks_uri").ok_or(unreadable)?,
            post_secret,
        })
    }

    /// `GET` an address that answers JSON.
    async fn get_json(&self, address: &Url, what: &'static str) -> Result<Value, Problem> {
        let answer = self.http.get(address).await.map_err(|f| failed(what, f))?;
        if answer.status != 200 {
            log_problem(what, "status", Some(answer.status));
            return Err(Problem::Unanswered("it answered with an error"));
        }
        serde_json::from_slice(&answer.body).map_err(|_| {
            log_problem(what, "not-json", None);
            Problem::Unanswered("it answered what is not JSON")
        })
    }

    /// The keys: those kept, unless `again`, else fetched.
    async fn keys(&self, configuration: &Configuration, again: bool) -> Result<Arc<Keys>, Problem> {
        {
            let mut state = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
            if !again && let Some(kept) = state.kept.as_ref().filter(|k| k.at.elapsed() < KEPT_FOR)
            {
                return Ok(Arc::clone(&kept.value));
            }
            state.asked = Some(Instant::now());
        }
        let json = self.get_json(&configuration.jwks_uri, "keys").await?;
        let keys =
            Arc::new(Keys::from_jwks(&json).ok_or(Problem::Unanswered("its keys cannot be read"))?);
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kept = Some(Kept {
            value: Arc::clone(&keys),
            at: Instant::now(),
        });
        Ok(keys)
    }

    /// Whether the keys may be fetched again for an unknown key now; if so,
    /// the time is taken, so that others wait their minute.
    fn may_ask_again(&self) -> bool {
        let mut state = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        if state.asked.is_some_and(|at| at.elapsed() < REFETCH_EVERY) {
            return false;
        }
        state.asked = Some(Instant::now());
        true
    }

    /// An ID token checked with the provider's keys, fetched again once
    /// when it names one not among them and a minute has passed.
    async fn verified(
        &self,
        configuration: &Configuration,
        id_token: &Secret,
        nonce: Option<&Secret>,
    ) -> Result<Claims, Problem> {
        let expected = Expected {
            issuer: &self.settings.issuer,
            client_id: &self.settings.client_id,
            groups_claim: &self.settings.groups_claim,
            nonce,
        };
        let keys = self.keys(configuration, false).await?;
        match token::verify(id_token.expose(), &keys, &expected) {
            Err(Refused::UnknownKey) if self.may_ask_again() => {
                let keys = self.keys(configuration, true).await?;
                token::verify(id_token.expose(), &keys, &expected).map_err(refused)
            }
            other => other.map_err(refused),
        }
    }

    /// The code of step 3 exchanged, and the ID token checked with `nonce`.
    pub async fn exchange(
        &self,
        code: &Secret,
        verifier: &Secret,
        redirect_uri: &str,
        nonce: &Secret,
    ) -> Result<Tokens, Problem> {
        let configuration = self.configuration().await?;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.expose()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier.expose()),
        ];
        self.token_call(&configuration, &form, Some(nonce)).await
    }

    /// A renewal with the session's refresh token; the ID token checked
    /// without a `nonce`.
    pub async fn refresh(&self, refresh_token: &Secret) -> Result<Tokens, Problem> {
        let configuration = self.configuration().await?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
        ];
        self.token_call(&configuration, &form, None).await
    }

    /// The body of a call to the token endpoint, and its `Authorization`:
    /// the client authenticated with `client_secret_basic`, or
    /// `client_secret_post` when the configuration names only that.
    fn token_request(
        &self,
        configuration: &Configuration,
        fields: &[(&str, &str)],
    ) -> Result<(String, Option<HeaderValue>), Problem> {
        let settings = &self.settings;
        let mut form = Serializer::new(String::new());
        form.extend_pairs(fields);
        if configuration.post_secret {
            form.append_pair("client_id", &settings.client_id)
                .append_pair("client_secret", settings.client_secret.expose());
            return Ok((form.finish(), None));
        }
        // RFC 6749, 2.3.1: each form-encoded, then joined and base64.
        let id: String = byte_serialize(settings.client_id.as_bytes()).collect();
        let secret: String = byte_serialize(settings.client_secret.expose().as_bytes()).collect();
        let pair = Secret::from_text(format!("{id}:{secret}"));
        let value = format!("Basic {}", STANDARD.encode(pair.expose()));
        let mut header = HeaderValue::from_str(&value)
            .map_err(|_| Problem::Unanswered("the client cannot authenticate"))?;
        header.set_sensitive(true);
        Ok((form.finish(), Some(header)))
    }

    async fn token_call(
        &self,
        configuration: &Configuration,
        fields: &[(&str, &str)],
        nonce: Option<&Secret>,
    ) -> Result<Tokens, Problem> {
        let (form, authorization) = self.token_request(configuration, fields)?;
        let answer = self
            .http
            .post_form(&configuration.token_endpoint, form, authorization)
            .await
            .map_err(|f| failed("token", f))?;
        if (400..500).contains(&answer.status) {
            // The provider's `error` code only, never its description.
            let error = serde_json::from_slice::<Value>(&answer.body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(error_code));
            log::record(
                "identity-provider-refused",
                serde_json::json!({ "status": answer.status, "error": error }),
            );
            return Err(Problem::Refused);
        }
        if answer.status != 200 {
            log_problem("token", "status", Some(answer.status));
            return Err(Problem::Unanswered("it answered with an error"));
        }
        let json: Value = serde_json::from_slice(&answer.body).map_err(|_| {
            log_problem("token", "not-json", None);
            Problem::Unanswered("it answered what is not JSON")
        })?;
        let Some(id_token) = json.get("id_token").and_then(Value::as_str) else {
            log_problem("token", "no-id-token", None);
            return Err(Problem::NoIdToken);
        };
        // Held as secrets from here on; once a sign-in or renewal.
        let id_token = Secret::from_text(id_token.to_owned());
        let refresh_token = json
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= MAX_REFRESH_TOKEN_BYTES)
            .map(|t| Secret::from_text(t.to_owned()));
        let claims = self.verified(configuration, &id_token, nonce).await?;
        Ok(Tokens {
            claims,
            refresh_token,
        })
    }
}

/// An `error` code of the provider's as the log may show it: a short
/// token of letters, digits and `_.-`, else nothing of it.
fn error_code(text: &str) -> String {
    let fine = !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
    if fine {
        text.to_owned()
    } else {
        "(other)".to_owned()
    }
}

fn refused(refused: Refused) -> Problem {
    match refused {
        Refused::UnknownKey => {
            Problem::Invalid("it is signed with a key the provider does not publish")
        }
        Refused::Invalid(why) => Problem::Invalid(why),
    }
}

fn failed(what: &'static str, failure: Failure) -> Problem {
    log_problem(what, failure.as_str(), None);
    Problem::Unanswered(match failure {
        Failure::Unreachable => "it could not be reached",
        Failure::TimedOut => "it did not answer within 10 seconds",
        Failure::TooLarge => "it answered more than 1 MiB",
    })
}

/// A record of what went wrong with a call: which, and what kind; nothing
/// that was sent or received.
fn log_problem(call: &str, problem: &str, status: Option<u16>) {
    log::record(
        "identity-provider-unanswered",
        serde_json::json!({ "call": call, "problem": problem, "status": status }),
    );
}
