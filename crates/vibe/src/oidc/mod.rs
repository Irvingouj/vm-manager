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
//!    the sign-in under way sealed into the `state` ([`under_way`]): the
//!    platform keeps nothing of it.
//! 3. `/callback`: the code exchanged, the ID token checked ([`token`]),
//!    the User's record and a platform session made ([`sessions`]), then
//!    on as in 2.
//! 4. [`Oidc::session_step`], `<application address>/_vm/session`: the
//!    one-time code used up, the Application's session cookie set.
//!
//! The Dashboard (`dashboard.<domain>`, crate::dashboard) is signed in to
//! the same way, with `dashboard` where an Application's name is
//! ([`signs_in_to`]); its session is an Application session of that name.
//!
//! Then [`Oidc::user_of`] gives each request with an Application's cookie
//! its User, the session renewed first when it is due.
//!
//! OIDC is a connection kept in the platform database ([`connection`]),
//! put in use when it is made and at start: every request takes the one in
//! use when it begins. The tool signs in through the browser at `/tool`
//! ([`tool`]), with an authorization of its own.
//!
//! The parts: [`key`] (the installation's key), [`connection`] (the
//! connection and its settings, [`settings`]), [`secret`] (values that
//! never print), [`http`] and [`provider`] (the calls to the identity
//! provider, bounded), [`token`], [`seal`] (what is kept secret, at rest
//! and in `state`), [`under_way`], [`sessions`], [`tool`], [`pages`].

pub mod connection;
pub mod http;
pub mod key;
pub mod pages;
pub mod provider;
pub mod seal;
pub mod secret;
pub mod sessions;
pub mod settings;
pub mod token;
pub mod tool;
pub mod under_way;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::header::{ACCEPT, HeaderValue};
use hyper::http::request::Parts;
use hyper::{Method, Response, StatusCode};
use url::{Url, form_urlencoded};

use crate::cookies;
use crate::dashboard::DASHBOARD_NAME;
use crate::origin::{Public, from_another_origin};
use crate::platform_db::PlatformDatabase;
use crate::registry::{Registry, check_name};
use crate::tool_credentials::ToolCredentials;
use connection::{Connected, Live};
use key::InstallationKey;
use provider::{Configuration, Problem, Tokens};
use seal::{Sealer, Use};
use secret::{Secret, digest, looks_random, s256, same};
use sessions::{Current, Outcome, Sessions, log_database};
use under_way::{Begun, MAX_NEXT, MAX_STEP_ONE_STATE, NotTaken, Opened, States};

/// How long the cookie of a sign-in under way lives, in seconds.
pub const SIGN_IN_COOKIE_SECONDS: u64 = 600;

/// The longest `next`, `state` or `application` taken, in bytes.
const MAX_PARAMETER_BYTES: usize = 4096;

/// The name of the sign-in address under the domain.
pub const AUTH_NAME: &str = "auth";

/// The path of step 4 on every Application's address.
pub const SESSION_PATH: &str = "/_vm/session";

type Answer = Response<Full<Bytes>>;

/// OIDC sign-in, put together: what the program holds with an
/// installation's key, connected or not.
pub struct Oidc {
    /// The connection in use, if any.
    live: Arc<Live>,
    sessions: Sessions,
    states: States,
    /// Seals the connection's client secret at rest.
    client_secrets: Sealer,
    database: Arc<PlatformDatabase>,
    /// Where a sign-in of the tool makes its credential.
    credentials: Arc<ToolCredentials>,
    registry: Arc<Registry>,
    domain: Arc<str>,
    public: Public,
    /// How long a session lives from its sign-in.
    lifetime: Duration,
}

/// What `Oidc::new` is made with besides the key.
pub struct OidcParts {
    pub database: Arc<PlatformDatabase>,
    pub credentials: Arc<ToolCredentials>,
    pub registry: Arc<Registry>,
    pub domain: Arc<str>,
    pub public: Public,
    /// How long the Groups read are used before the provider is asked
    /// again (`--session-renewal`).
    pub renewal: Duration,
    /// How long a session lives (`--session-lifetime`).
    pub lifetime: Duration,
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

/// Whether a session can be had at the address of `name`: an Application's
/// name (one could have), or the Dashboard's (crate::dashboard), which is
/// signed in to exactly as an Application is.
pub fn signs_in_to(name: &str) -> bool {
    name == DASHBOARD_NAME || check_name(name).is_ok()
}

/// Whether a request asks for a page: a `GET` or `HEAD` whose `Accept` has
/// `text/html`.
pub fn asks_for_a_page(parts: &Parts) -> bool {
    (parts.method == Method::GET || parts.method == Method::HEAD)
        && parts.headers.get_all(ACCEPT).iter().any(|a| {
            a.as_bytes()
                .to_ascii_lowercase()
                .windows(9)
                .any(|w| w == b"text/html")
        })
}

impl Oidc {
    /// Not connected yet: [`Oidc::take_stored`] or [`Oidc::connect`]
    /// connects it. The error is a message for the operator.
    pub fn new(key: &InstallationKey, with: OidcParts) -> Result<Self, String> {
        let live = Arc::new(Live::default());
        let sessions = Sessions::new(
            Arc::clone(&with.database),
            Arc::clone(&live),
            Sealer::new(key, Use::RefreshTokens)?,
            with.renewal,
            with.lifetime,
        );
        Ok(Self {
            live,
            sessions,
            states: States::new(key)?,
            client_secrets: Sealer::new(key, Use::ClientSecret)?,
            database: with.database,
            credentials: with.credentials,
            registry: with.registry,
            domain: with.domain,
            public: with.public,
            lifetime: with.lifetime,
        })
    }

    /// The connection in use, if any.
    pub fn connected(&self) -> Option<Arc<Connected>> {
        self.live.get()
    }

    /// The renewal of a tool credential from a sign-in, found due.
    pub async fn renew_tool(&self, digest: [u8; 32]) -> Outcome {
        self.sessions.renew_tool(digest).await
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

    fn session_cookie(&self, value: &Secret) -> Option<HeaderValue> {
        cookies::set_cookie(
            cookies::session_name(self.secure()),
            value.expose(),
            self.lifetime.as_secs(),
            self.secure(),
        )
    }

    /// The value of this browser's cookie of the sign-in address, kept when
    /// it is one the platform made, else a new one; `None` when the
    /// operating system gives no randomness.
    fn browser_value(&self, parts: &Parts) -> Option<String> {
        let kept = cookies::find(&parts.headers, cookies::sign_in_name(self.secure()))
            .filter(|v| looks_random(v))
            .map(str::to_owned);
        // Owned: the value is written into the cookie and hashed; once a
        // sign-in, 43 bytes.
        match kept {
            Some(browser) => Some(browser),
            None => Secret::random().map(|b| b.expose().to_owned()),
        }
    }

    /// The provider's authorization endpoint for a sign-in begun so.
    fn authorization(
        &self,
        connected: &Connected,
        configuration: &Configuration,
        begun: &Begun,
        port: Option<u16>,
    ) -> Url {
        // Copied: the kept configuration's address, with this sign-in's
        // query added; once a sign-in.
        let mut authorization = configuration.authorization_endpoint.clone();
        authorization
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &connected.settings.client_id)
            .append_pair("redirect_uri", &self.callback(port))
            .append_pair("scope", &connected.settings.scope())
            .append_pair("state", begun.state.expose())
            .append_pair("nonce", begun.nonce.expose())
            .append_pair("code_challenge", &s256(&begun.verifier))
            .append_pair("code_challenge_method", "S256");
        authorization
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
        let application = application.filter(|a| signs_in_to(a))?;
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
                None => return Some(pages::unavailable(&self.domain, None)),
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
    pub async fn sign_in_address<B>(&self, parts: &Parts, body: B, port: Option<u16>) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        if from_another_origin(&parts.method, &parts.headers, &self.auth_origin(port)) {
            return pages::plain(
                StatusCode::FORBIDDEN,
                "Refused: this request comes from another origin.\n",
            );
        }
        let Some(connected) = self.connected() else {
            return pages::not_found(&self.domain);
        };
        let read = parts.method == Method::GET || parts.method == Method::HEAD;
        let post = parts.method == Method::POST;
        match (parts.uri.path(), read) {
            ("/sign-in", true) => self.sign_in(&connected, parts, port).await,
            ("/callback", true) => self.callback_step(&connected, parts, port).await,
            ("/tool", true) => self.tool_begin(&connected, parts, port).await,
            ("/tool", false) if post => self.tool_answer(parts, body).await,
            ("/me", true) => self.me(parts).await,
            ("/sign-out", true) => pages::sign_out_form(&self.domain),
            ("/sign-out", false) if post => self.sign_out(parts).await,
            ("/sign-in" | "/callback" | "/me" | "/sign-out" | "/tool", _) => {
                pages::method_not_allowed(&self.domain)
            }
            _ => pages::not_found(&self.domain),
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
    async fn sign_in(&self, connected: &Connected, parts: &Parts, port: Option<u16>) -> Answer {
        let query = Query::of(parts);
        let Some(application) = query.get("application").filter(|a| signs_in_to(a)) else {
            return pages::failed(
                &self.domain,
                StatusCode::BAD_REQUEST,
                "The address names no Application that could exist.",
                None,
            );
        };
        let next = path_only(query.get("next"));
        let again = format!("{}{next}", self.app_origin(application, port));
        let again = Some(again.as_str());
        let Some(state) = query.get("state").filter(|s| {
            !s.is_empty() && s.len() <= MAX_STEP_ONE_STATE && !s.chars().any(char::is_control)
        }) else {
            return pages::failed(
                &self.domain,
                StatusCode::BAD_REQUEST,
                "The address does not say which sign-in this is, or says it in more than 128 bytes.",
                again,
            );
        };
        match self.platform_session(parts).await {
            Current::User { session, .. } => {
                return self
                    .to_application(session, application, &next, state, port, Vec::new())
                    .await;
            }
            Current::Unavailable => return pages::unavailable(&self.domain, again),
            Current::None => {}
        }
        let configuration = match connected.provider.configuration(None).await {
            Ok(configuration) => configuration,
            Err(problem) => {
                return pages::failed(
                    &self.domain,
                    StatusCode::BAD_GATEWAY,
                    &format!(
                        "The company's sign-in could not be used: {}.",
                        problem.reason()
                    ),
                    again,
                );
            }
        };
        let Some(browser) = self.browser_value(parts) else {
            return pages::unavailable(&self.domain, again);
        };
        // Through the provider, a long `next` is `/`: the `state` carries
        // it, and some providers cut a long `state`.
        let next = if next.len() > MAX_NEXT {
            "/".to_owned()
        } else {
            next
        };
        let Some(begun) = self
            .states
            .begin(&digest(&browser), state, application, &next)
        else {
            return pages::unavailable(&self.domain, again);
        };
        let authorization = self.authorization(connected, &configuration, &begun, port);
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
        let again = Some(again.as_str());
        if application != DASHBOARD_NAME && !self.registry.may_serve(application) {
            let mut answer = pages::no_application(&self.domain, application);
            for cookie in cookies {
                answer
                    .headers_mut()
                    .append(hyper::header::SET_COOKIE, cookie);
            }
            return answer;
        }
        let Some(code) = Secret::random() else {
            return pages::unavailable(&self.domain, again);
        };
        if let Err(error) = self
            .sessions
            .make_code(session, application, &code.digest(), &digest(state))
            .await
        {
            log_database(&error);
            return pages::unavailable(&self.domain, again);
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

    /// Step 3, for the sign-in of an Application or of the tool.
    async fn callback_step(
        &self,
        connected: &Connected,
        parts: &Parts,
        port: Option<u16>,
    ) -> Answer {
        let query = Query::of(parts);
        // Until the `state` opens, it is not known which Application the
        // sign-in was for: these pages link nowhere.
        let Some(state) = query.get("state") else {
            return pages::failed(
                &self.domain,
                StatusCode::BAD_REQUEST,
                "The sign-in came back without saying which sign-in it was.",
                None,
            );
        };
        let browser =
            cookies::find(&parts.headers, cookies::sign_in_name(self.secure())).map(digest);
        let opened = match self.states.open(state, browser.as_ref()) {
            Ok(opened) => opened,
            Err(NotTaken::Unreadable) => {
                return pages::failed(
                    &self.domain,
                    StatusCode::BAD_REQUEST,
                    "This browser did not start this sign-in.",
                    None,
                );
            }
            Err(NotTaken::TooOld) => {
                return pages::failed(
                    &self.domain,
                    StatusCode::BAD_REQUEST,
                    "The sign-in took more than 10 minutes.",
                    None,
                );
            }
        };
        let pending = match opened {
            Opened::Application(pending) => pending,
            Opened::Tool(tool) => {
                // The `state` opened with it, so it is there.
                let Some(browser) = browser else {
                    return pages::tool_over(&self.domain);
                };
                return match self
                    .exchanged(connected, &query, &tool.verifier(), &tool.nonce(), port)
                    .await
                {
                    Ok(tokens) => self.tool_awaiting(&tool, &browser, &tokens).await,
                    Err((status, what)) => pages::tool_failed(&self.domain, status, &what),
                };
            }
        };
        let again = format!(
            "{}{}",
            self.app_origin(&pending.application, port),
            pending.next
        );
        let again = Some(again.as_str());
        let tokens = match self
            .exchanged(
                connected,
                &query,
                &pending.verifier(),
                &pending.nonce(),
                port,
            )
            .await
        {
            Ok(tokens) => tokens,
            Err((status, what)) => return pages::failed(&self.domain, status, &what, again),
        };
        let Some(value) = Secret::random() else {
            return pages::unavailable(&self.domain, again);
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
                return pages::unavailable(&self.domain, again);
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

    /// What came back at `/callback`, its code exchanged and the ID token
    /// checked; else the status and the words of the page that says why.
    async fn exchanged(
        &self,
        connected: &Connected,
        query: &Query,
        verifier: &Secret,
        nonce: &Secret,
        port: Option<u16>,
    ) -> Result<Tokens, (StatusCode, String)> {
        let refused = |what: &str| Err((StatusCode::BAD_REQUEST, what.to_owned()));
        if query
            .get("iss")
            .is_some_and(|iss| iss != connected.settings.issuer)
        {
            return refused("The sign-in came back from another identity provider.");
        }
        if let Some(error) = query.get("error") {
            let shown = provider_error(error);
            return refused(&format!(
                "The company's sign-in did not sign you in ({shown})."
            ));
        }
        let Some(code) = query.get("code").filter(|c| !c.is_empty()) else {
            return refused("The sign-in came back without a code.");
        };
        // Held as a secret from here; once a sign-in.
        let code = Secret::from_text(code.to_owned());
        let exchanged = connected
            .provider
            .exchange(&code, verifier, &self.callback(port), nonce)
            .await;
        match exchanged {
            Ok(tokens) => Ok(tokens),
            Err(Problem::Unanswered(why)) => Err((
                StatusCode::BAD_GATEWAY,
                format!("The company's sign-in could not be used: {why}."),
            )),
            Err(Problem::NoIdToken) => Err((
                StatusCode::BAD_GATEWAY,
                "The company's sign-in answered without an ID token.".to_owned(),
            )),
            Err(Problem::Refused) => refused("The company's sign-in refused the code it gave."),
            Err(Problem::Invalid(why)) => refused(&format!("The ID token does not hold: {why}.")),
        }
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
            return pages::unavailable(&self.domain, Some("/sign-out"));
        }
        let clear = cookies::set_cookie(cookies::session_name(self.secure()), "", 0, self.secure());
        pages::signed_out(&self.domain, clear)
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
        let again = Some(again.as_str());
        let refused =
            |what: &str| pages::failed(&self.domain, StatusCode::BAD_REQUEST, what, again);
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
                return pages::unavailable(&self.domain, again);
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
            return pages::unavailable(&self.domain, again);
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
                return pages::unavailable(&self.domain, again);
            }
        }
        let next = path_only(query.get("next"));
        let location = format!("{}{next}", self.app_origin(application, port));
        // The cookie of step 1 is left to run out: another page of this
        // Application signing in at once in this browser holds the same
        // value, and lands with it.
        pages::redirect(&location, self.session_cookie(&value).into_iter().collect())
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
