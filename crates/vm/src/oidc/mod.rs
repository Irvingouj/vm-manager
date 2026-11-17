//! OIDC sign-in and per-subdomain sessions (blackbox/CONTRACT.md, "Sign-in
//! and sessions"; ADR 0011): the platform is the relying party, sessions
//! live in the platform database, and a browser holds opaque cookies, one
//! for the sign-in address (`auth.<domain>`) and one for each
//! Application's address, none valid anywhere else.
//!
//! The way through, as Ingress drives it:
//!
//! 1. [`Oidc::not_signed_in`]: a page of an Application without a session
//!    there is sent to the sign-in address, with a `state` held in a
//!    cookie of the Application's address; anything else is `401`.
//! 2. [`Oidc::sign_in_address`], `/sign-in`: with a platform session, a
//!    one-time code for the Application; without, the provider's
//!    authorization endpoint (code flow, PKCE `S256`, `state`, `nonce`),
//!    the sign-in under way remembered in memory ([`pending`]).
//! 3. `/callback`: the code exchanged, the ID token checked ([`token`]),
//!    the User's record and a platform session made ([`sessions`]), then
//!    on as in 2.
//! 4. [`Oidc::session_step`], `<application address>/_vm/session`: the
//!    one-time code used up, the Application's session cookie set.
//!
//! Then [`Oidc::user_of`] gives each request with an Application's cookie
//! its User, the session renewed first when it is due.
//!
//! The parts: [`settings`] (what it was started with), [`secret`] (values
//! that never print), [`http`] and [`provider`] (the calls to the identity
//! provider, bounded), [`token`], [`seal`] (the refresh token at rest),
//! [`pending`], [`sessions`], [`pages`].

pub mod http;
pub mod pages;
pub mod pending;
pub mod provider;
pub mod seal;
pub mod secret;
pub mod sessions;
pub mod settings;
pub mod token;

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{ACCEPT, HeaderValue};
use hyper::http::request::Parts;
use hyper::{Method, Response, StatusCode};
use url::form_urlencoded;

use crate::cookies;
use crate::origin::{Public, from_another_origin};
use crate::platform_db::PlatformDatabase;
use crate::registry::{Registry, check_name};
use pending::{NotTaken, Pending, UnderWay};
use provider::{Problem, Provider};
use seal::Sealer;
use secret::{Secret, digest, looks_random, s256, same};
use sessions::{Current, Sessions, log_database};
use settings::Settings;

/// How long the cookie of a sign-in under way lives, in seconds.
pub const SIGN_IN_COOKIE_SECONDS: u64 = 600;

/// The longest `next`, `state` or `application` taken, in bytes.
const MAX_PARAMETER_BYTES: usize = 4096;

/// The longest `state` of step 1 taken, in bytes.
const MAX_STATE_BYTES: usize = 512;

/// The name of the sign-in address under the domain.
pub const AUTH_NAME: &str = "auth";

/// The path of step 4 on every Application's address.
pub const SESSION_PATH: &str = "/_vm/session";

type Answer = Response<Full<Bytes>>;

/// OIDC sign-in, put together.
pub struct Oidc {
    settings: Arc<Settings>,
    provider: Arc<Provider>,
    sessions: Sessions,
    under_way: UnderWay,
    registry: Arc<Registry>,
    domain: Arc<str>,
    public: Public,
}

/// What a query string holds: the first value of each name.
struct Query(Vec<(String, String)>);

impl Query {
    fn of(parts: &Parts) -> Self {
        // Decoded into owned text: percent-decoding makes new strings;
        // once a request, of a query hyper bounds with the head.
        let pairs = parts
            .uri
            .query()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_default();
        Self(pairs)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .filter(|v| v.len() <= MAX_PARAMETER_BYTES)
    }
}

/// `next` as the platform takes it: a path of one leading `/` (not `//`
/// nor `/\`), no control bytes; else `/`. Bytes a `Location` cannot hold
/// as they are (a space, anything not ASCII) are percent-encoded.
pub fn path_only(next: Option<&str>) -> String {
    let Some(next) = next else {
        return "/".to_owned();
    };
    let fine = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.bytes().any(|b| b.is_ascii_control());
    if !fine {
        return "/".to_owned();
    }
    let mut out = String::with_capacity(next.len());
    for byte in next.bytes() {
        if byte.is_ascii_graphic() && !b"\"<>`".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Whether a request asks for a page: a `GET` or `HEAD` whose `Accept` has
/// `text/html`.
fn asks_for_a_page(parts: &Parts) -> bool {
    (parts.method == Method::GET || parts.method == Method::HEAD)
        && parts.headers.get_all(ACCEPT).iter().any(|a| {
            a.as_bytes()
                .to_ascii_lowercase()
                .windows(9)
                .any(|w| w == b"text/html")
        })
}

impl Oidc {
    /// The error is a message for the operator.
    pub fn new(
        settings: Settings,
        database: Arc<PlatformDatabase>,
        registry: Arc<Registry>,
        domain: Arc<str>,
        public: Public,
    ) -> Result<Self, String> {
        let settings = Arc::new(settings);
        let provider = Arc::new(Provider::new(Arc::clone(&settings))?);
        let sealer = Sealer::new(&settings.client_secret)?;
        let sessions = Sessions::new(
            database,
            Arc::clone(&provider),
            sealer,
            settings.renewal,
            settings.lifetime,
        );
        Ok(Self {
            settings,
            provider,
            sessions,
            under_way: UnderWay::default(),
            registry,
            domain,
            public,
        })
    }

    fn secure(&self) -> bool {
        self.public.secure()
    }

    fn auth_origin(&self, port: Option<u16>) -> String {
        self.public
            .origin(&format!("{AUTH_NAME}.{}", self.domain), port)
    }

    fn app_origin(&self, application: &str, port: Option<u16>) -> String {
        self.public
            .origin(&format!("{application}.{}", self.domain), port)
    }

    fn callback(&self, port: Option<u16>) -> String {
        format!("{}/callback", self.auth_origin(port))
    }

    /// The Dashboard's address: where a failure without an Application
    /// sends the person to try again.
    fn dashboard(&self, port: Option<u16>) -> String {
        format!(
            "{}/",
            self.public
                .origin(&format!("dashboard.{}", self.domain), port)
        )
    }

    fn session_cookie(&self, value: &Secret) -> Option<HeaderValue> {
        cookies::set_cookie(
            cookies::session_name(self.secure()),
            value.expose(),
            self.settings.lifetime.as_secs(),
            self.secure(),
        )
    }

    fn sign_in_cookie(&self, value: &str, seconds: u64) -> Option<HeaderValue> {
        cookies::set_cookie(
            cookies::sign_in_name(self.secure()),
            value,
            seconds,
            self.secure(),
        )
    }

    /// The User of a request with this Application's cookie.
    pub async fn user_of(&self, parts: &Parts, application: &str) -> Current {
        let Some(value) = cookies::find(&parts.headers, cookies::session_name(self.secure()))
        else {
            return Current::None;
        };
        let found = self.sessions.application(&digest(value), application).await;
        self.sessions.current(found).await
    }

    /// Step 1: a request to an Application without a session there. A
    /// page of a name an Application could have is sent to the sign-in
    /// address; anything else is `401`.
    pub fn not_signed_in(
        &self,
        parts: &Parts,
        application: Option<&str>,
        port: Option<u16>,
    ) -> Option<Answer> {
        let application = application.filter(|a| check_name(a).is_ok())?;
        if !asks_for_a_page(parts) {
            return None;
        }
        // A sign-in already begun in this browser keeps its value, so
        // that two pages of one Application signing in at once both land.
        let kept = cookies::find(&parts.headers, cookies::sign_in_name(self.secure()))
            .filter(|v| looks_random(v))
            .map(str::to_owned);
        // Owned: the value is written into the address and the cookie;
        // once a request, 43 bytes.
        let state = match kept {
            Some(state) => state,
            None => match Secret::random() {
                Some(state) => state.expose().to_owned(),
                None => return Some(pages::unavailable(&self.dashboard(port))),
            },
        };
        let next = parts.uri.path_and_query().map_or("/", |p| p.as_str());
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("application", application)
            .append_pair("next", next)
            .append_pair("state", &state)
            .finish();
        let location = format!("{}/sign-in?{query}", self.auth_origin(port));
        let cookie = self.sign_in_cookie(&state, SIGN_IN_COOKIE_SECONDS);
        Some(pages::redirect(&location, cookie.into_iter().collect()))
    }

    /// Every request to the sign-in address.
    pub async fn sign_in_address(&self, parts: &Parts, port: Option<u16>) -> Answer {
        if from_another_origin(&parts.method, &parts.headers, &self.auth_origin(port)) {
            return pages::plain(
                StatusCode::FORBIDDEN,
                "Refused: this request comes from another origin.\n",
            );
        }
        let read = parts.method == Method::GET || parts.method == Method::HEAD;
        match (parts.uri.path(), read) {
            ("/sign-in", true) => self.sign_in(parts, port).await,
            ("/callback", true) => self.callback_step(parts, port).await,
            ("/me", true) => self.me(parts).await,
            ("/sign-out", true) => pages::sign_out_form(),
            ("/sign-out", false) if parts.method == Method::POST => self.sign_out(parts).await,
            ("/sign-in" | "/callback" | "/me" | "/sign-out", _) => pages::method_not_allowed(),
            _ => pages::not_found(),
        }
    }

    /// The platform session of this browser, renewed when due.
    async fn platform_session(&self, parts: &Parts) -> Current {
        let Some(value) = cookies::find(&parts.headers, cookies::session_name(self.secure()))
        else {
            return Current::None;
        };
        let found = self.sessions.platform(&digest(value)).await;
        self.sessions.current(found).await
    }

    /// Step 2.
    async fn sign_in(&self, parts: &Parts, port: Option<u16>) -> Answer {
        let query = Query::of(parts);
        let dashboard = self.dashboard(port);
        let Some(application) = query.get("application").filter(|a| check_name(a).is_ok()) else {
            return pages::failed(
                StatusCode::BAD_REQUEST,
                "The address names no Application that could exist.",
                &dashboard,
            );
        };
        let next = path_only(query.get("next"));
        let again = format!("{}{next}", self.app_origin(application, port));
        let Some(state) = query.get("state").filter(|s| {
            !s.is_empty() && s.len() <= MAX_STATE_BYTES && !s.chars().any(char::is_control)
        }) else {
            return pages::failed(
                StatusCode::BAD_REQUEST,
                "The address does not say which sign-in this is.",
                &again,
            );
        };
        match self.platform_session(parts).await {
            Current::User { session, .. } => {
                return self
                    .to_application(session, application, &next, state, port, Vec::new())
                    .await;
            }
            Current::Unavailable => return pages::unavailable(&again),
            Current::None => {}
        }
        let configuration = match self.provider.configuration().await {
            Ok(configuration) => configuration,
            Err(_) => {
                return pages::failed(
                    StatusCode::BAD_GATEWAY,
                    "The company's sign-in could not be reached, or did not answer in time.",
                    &again,
                );
            }
        };
        let kept = cookies::find(&parts.headers, cookies::sign_in_name(self.secure()))
            .filter(|v| looks_random(v))
            .map(str::to_owned);
        // Owned as in step 1: once a sign-in, 43 bytes.
        let browser = match kept {
            Some(browser) => browser,
            None => match Secret::random() {
                Some(browser) => browser.expose().to_owned(),
                None => return pages::unavailable(&again),
            },
        };
        let (Some(provider_state), Some(nonce), Some(verifier)) =
            (Secret::random(), Secret::random(), Secret::random())
        else {
            return pages::unavailable(&again);
        };
        // Copied: the kept configuration's address, with this sign-in's
        // query added; once a sign-in.
        let mut authorization = configuration.authorization_endpoint.clone();
        authorization
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.settings.client_id)
            .append_pair("redirect_uri", &self.callback(port))
            .append_pair("scope", &self.settings.scope())
            .append_pair("state", provider_state.expose())
            .append_pair("nonce", nonce.expose())
            .append_pair("code_challenge", &s256(&verifier))
            .append_pair("code_challenge_method", "S256");
        // What the callback needs is kept until it comes: copied once a
        // sign-in, each bounded (MAX_PARAMETER_BYTES).
        let begun = self.under_way.begin(
            &provider_state,
            Pending {
                browser: digest(&browser),
                nonce,
                verifier,
                application: application.to_owned(),
                next,
                state: state.to_owned(),
                began: Instant::now(),
            },
        );
        if !begun {
            return pages::unavailable(&again);
        }
        let cookie = self.sign_in_cookie(&browser, SIGN_IN_COOKIE_SECONDS);
        pages::redirect(authorization.as_str(), cookie.into_iter().collect())
    }

    /// Step 2 with a platform session: a one-time code for a known
    /// Application, sent to its `/_vm/session`; `404` for an unknown one.
    async fn to_application(
        &self,
        session: i64,
        application: &str,
        next: &str,
        state: &str,
        port: Option<u16>,
        cookies: Vec<HeaderValue>,
    ) -> Answer {
        let again = format!("{}{next}", self.app_origin(application, port));
        if self.registry.serving(application).is_none() {
            let mut answer = pages::no_application(application, &self.dashboard(port));
            for cookie in cookies {
                answer
                    .headers_mut()
                    .append(hyper::header::SET_COOKIE, cookie);
            }
            return answer;
        }
        let Some(code) = Secret::random() else {
            return pages::unavailable(&again);
        };
        if let Err(error) = self
            .sessions
            .make_code(session, application, &code.digest(), &digest(state))
            .await
        {
            log_database(&error);
            return pages::unavailable(&again);
        }
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("code", code.expose())
            .append_pair("state", state)
            .append_pair("next", next)
            .finish();
        let location = format!(
            "{}{SESSION_PATH}?{query}",
            self.app_origin(application, port)
        );
        pages::redirect(&location, cookies)
    }

    /// Step 3.
    async fn callback_step(&self, parts: &Parts, port: Option<u16>) -> Answer {
        let query = Query::of(parts);
        let dashboard = self.dashboard(port);
        let Some(state) = query.get("state") else {
            return pages::failed(
                StatusCode::BAD_REQUEST,
                "The sign-in came back without saying which sign-in it was.",
                &dashboard,
            );
        };
        let browser =
            cookies::find(&parts.headers, cookies::sign_in_name(self.secure())).map(digest);
        let pending = match self.under_way.take(&digest(state), browser.as_ref()) {
            Ok(pending) => pending,
            Err(NotTaken::Unknown) => {
                return pages::failed(
                    StatusCode::BAD_REQUEST,
                    "This browser did not start this sign-in, or it was already used.",
                    &dashboard,
                );
            }
            Err(NotTaken::TooOld) => {
                return pages::failed(
                    StatusCode::BAD_REQUEST,
                    "The sign-in took more than 10 minutes.",
                    &dashboard,
                );
            }
        };
        let again = format!(
            "{}{}",
            self.app_origin(&pending.application, port),
            pending.next
        );
        if query
            .get("iss")
            .is_some_and(|iss| iss != self.settings.issuer)
        {
            return pages::failed(
                StatusCode::BAD_REQUEST,
                "The sign-in came back from another identity provider.",
                &again,
            );
        }
        if let Some(error) = query.get("error") {
            let shown = provider_error(error);
            return pages::failed(
                StatusCode::BAD_REQUEST,
                &format!("The company's sign-in did not sign you in ({shown})."),
                &again,
            );
        }
        let Some(code) = query.get("code").filter(|c| !c.is_empty()) else {
            return pages::failed(
                StatusCode::BAD_REQUEST,
                "The sign-in came back without a code.",
                &again,
            );
        };
        // Held as a secret from here; once a sign-in.
        let code = Secret::from_text(code.to_owned());
        let exchanged = self
            .provider
            .exchange(
                &code,
                &pending.verifier,
                &self.callback(port),
                &pending.nonce,
            )
            .await;
        let tokens = match exchanged {
            Ok(tokens) => tokens,
            Err(Problem::Unanswered(why)) => {
                return pages::failed(
                    StatusCode::BAD_GATEWAY,
                    &format!("The company's sign-in could not be used: {why}."),
                    &again,
                );
            }
            Err(Problem::NoIdToken) => {
                return pages::failed(
                    StatusCode::BAD_GATEWAY,
                    "The company's sign-in answered without an ID token.",
                    &again,
                );
            }
            Err(Problem::Refused) => {
                return pages::failed(
                    StatusCode::BAD_REQUEST,
                    "The company's sign-in refused the code it gave.",
                    &again,
                );
            }
            Err(Problem::Invalid(why)) => {
                return pages::failed(
                    StatusCode::BAD_REQUEST,
                    &format!("The ID token does not hold: {why}."),
                    &again,
                );
            }
        };
        let Some(value) = Secret::random() else {
            return pages::unavailable(&again);
        };
        let session = match self
            .sessions
            .begin(
                &value.digest(),
                &tokens.claims,
                tokens.refresh_token.as_ref(),
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                log_database(&error);
                return pages::unavailable(&again);
            }
        };
        let cookies: Vec<HeaderValue> = self.session_cookie(&value).into_iter().collect();
        self.to_application(
            session,
            &pending.application,
            &pending.next,
            &pending.state,
            port,
            cookies,
        )
        .await
    }

    /// `GET /me`.
    async fn me(&self, parts: &Parts) -> Answer {
        let session = match self.platform_session(parts).await {
            Current::User { session, .. } => session,
            Current::None => return pages::plain(StatusCode::UNAUTHORIZED, "Not signed in.\n"),
            Current::Unavailable => {
                return pages::plain(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Unavailable: try again shortly.\n",
                );
            }
        };
        match self.sessions.record(session).await {
            Ok(Some(record)) => pages::json(record.to_string()),
            Ok(None) => pages::plain(StatusCode::UNAUTHORIZED, "Not signed in.\n"),
            Err(error) => {
                log_database(&error);
                pages::plain(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Unavailable: try again shortly.\n",
                )
            }
        }
    }

    /// `POST /sign-out`: the platform session ends, and every Application
    /// session made from it.
    async fn sign_out(&self, parts: &Parts) -> Answer {
        if let Some(value) = cookies::find(&parts.headers, cookies::session_name(self.secure()))
            && let Err(error) = self.sessions.end_by_cookie(&digest(value)).await
        {
            log_database(&error);
            return pages::unavailable("/sign-out");
        }
        let clear = cookies::set_cookie(cookies::session_name(self.secure()), "", 0, self.secure());
        pages::signed_out(clear)
    }

    /// Step 4: `GET <application address>/_vm/session`.
    pub async fn session_step(
        &self,
        parts: &Parts,
        application: &str,
        port: Option<u16>,
    ) -> Answer {
        let query = Query::of(parts);
        let again = format!("{}/", self.app_origin(application, port));
        let refused = |what: &str| pages::failed(StatusCode::BAD_REQUEST, what, &again);
        if parts.method != Method::GET && parts.method != Method::HEAD {
            return refused("This address takes only a GET.");
        }
        let Some(code) = query.get("code").filter(|c| !c.is_empty()) else {
            return refused("The address holds no sign-in code.");
        };
        let taken = match self.sessions.take_code(&digest(code)).await {
            Ok(Some(taken)) => taken,
            Ok(None) => return refused("The sign-in code is unknown, or was already used."),
            Err(error) => {
                log_database(&error);
                return pages::unavailable(&again);
            }
        };
        let cookie = cookies::find(&parts.headers, cookies::sign_in_name(self.secure()));
        let state = query.get("state");
        let bound = match (state, cookie) {
            (Some(state), Some(cookie)) => {
                same(state.as_bytes(), cookie.as_bytes())
                    && same(&digest(state), &taken.state_digest)
            }
            _ => false,
        };
        if taken.application != application || !taken.fresh || !bound {
            return refused(
                "The sign-in code is not for this browser, this Application or this moment.",
            );
        }
        let Some(value) = Secret::random() else {
            return pages::unavailable(&again);
        };
        match self
            .sessions
            .begin_application(taken.session, application, &value.digest())
            .await
        {
            Ok(true) => {}
            Ok(false) => return refused("The session this code was made from has ended."),
            Err(error) => {
                log_database(&error);
                return pages::unavailable(&again);
            }
        }
        let next = path_only(query.get("next"));
        let location = format!("{}{next}", self.app_origin(application, port));
        let set = [self.session_cookie(&value), self.sign_in_cookie("", 0)];
        pages::redirect(&location, set.into_iter().flatten().collect())
    }
}

/// A provider's `error` as a page may show it: a short code of letters,
/// digits and `_.-`, else a word saying there was one.
fn provider_error(text: &str) -> String {
    let fine = !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
    if fine {
        text.to_owned()
    } else {
        "an error".to_owned()
    }
}
