//! Ingress: takes an HTTP request off the wire and answers it.
//!
//! In order (blackbox/CONTRACT.md, "Responses the program itself gives"):
//! a request id; the `Host`; a malformed stand-in sign-in ([`SignIn`]);
//! then who the User is. On the platform's API (`api.<domain>`, [`Api`]),
//! `GET /v1/installation` and `POST /v1/tool-sign-ins` are anyone's; else
//! an `Authorization` header, when there is one, decides alone: a tool
//! credential ([`ToolCredentials`]) that is alive signs its User in (its
//! Groups read again first when it came from a sign-in and is due), a
//! setup credential can only connect OIDC, and anything else is the `401`
//! of anyone not signed in, whatever stand-in headers come with it. With
//! OIDC connected ([`Oidc`]) the sign-in address
//! (`auth.<domain>`) and every Application's `/_vm/session` are its, and
//! on an Application's address (`<application>.<domain>`) only that
//! Application's session cookie signs a request in; without, only the
//! [`SignIn`] counts. Then a request from another origin than the
//! Application's own is refused ([`from_another_origin`]), whatever the
//! sign-in; then the Dashboard's address (`dashboard.<domain>`, signed in
//! to as an Application is) goes to crate::dashboard; then the Application
//! as the registry holds it in memory ([`Registry::find`]; read back, or
//! `503`, when memory does not know it now); then whether the User can open it (a Contributor, or someone it
//! is shared with), so that one it is not shared with gets the `403` of the
//! platform's own before the Application is handed anything (the local run
//! serves every test User and does not ask); then its serving Version is
//! run by the Request runner, the platform's cookies taken
//! out of the request and kept out of the answer ([`cookies`]). Its own
//! refusals are short plain text. Every answer carries `x-vm-request-id`,
//! and the request's `request` record is written just before the answer
//! goes, after everything the Application did with it.
//!
//! **The request's body** is not collected: after every check above, it is
//! handed to the runtime as a stream fed from the connection as the code
//! reads it ([`Pump`]). Nothing is read, and no clock runs, until the body
//! is asked for — the code's first read, or the response taking it to send
//! — so a body nobody asks for is not read and not timed, and `100
//! Continue` is not sent. A stated `content-length` over
//! [`MAX_REQUEST_BODY_BYTES`] is the `413` before the code runs; a body
//! with no stated length that passes it, and a body no byte of which
//! arrives for [`BODY_STALLED_AFTER`] while it is being read, fail its
//! stream, and the request is answered `413`/`408` when the answer has not
//! been decided yet (a disable first). A body the code never reads is
//! dropped with the request. How an answer is framed is the platform's, and
//! a `HEAD`, `204` or `304` answer has no body on the wire
//! (blackbox/CONTRACT.md, "What an Application is handed").

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Body;
use hyper::header::{
    CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderName, HeaderValue, RETRY_AFTER, SET_COOKIE,
    TRANSFER_ENCODING,
};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use openworkers_core::{HttpMethod, HttpRequest, RequestBody};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio_postgres::error::SqlState;

use crate::api::{Answer, Api, Caller, SignedInWith};
use crate::app_logs::{Collected, Logs};
use crate::cookies;
use crate::dashboard::DASHBOARD_NAME;
use crate::gateway::Transactions;
use crate::log;
use crate::oidc::sessions::{Current, Outcome as Renewed};
use crate::oidc::{AUTH_NAME, Oidc, SESSION_PATH, asks_for_a_page, pages, signs_in_to};
use crate::origin::{Public, from_another_origin};
use crate::registry::{Found, Registry, check_name};
use crate::resources::{READ_BACK_WAIT, Resources};
use crate::runner::{AnswerBody, InFlight, Outcome, RequestRunner};
use crate::sent::{OwnedTask, Sent, SentControl, SentStream};
use crate::sign_in::{RESERVED_HEADER_PREFIX, SignIn, SignedIn, remove_reserved_headers};
use crate::tool_credentials::{self, Digest, Presented, ToolCredentials};
use crate::user::User;

/// The header that names a request, in every answer and every record.
pub const REQUEST_ID_HEADER: &str = "x-vm-request-id";

/// Marks, in a response's extensions (never sent), an answer to a request
/// the Application's code was given: for the local run's lines of what it
/// served (crate::tool's `local`).
#[derive(Clone, Copy, Debug)]
pub struct GivenToCode;

/// The name under the domain where the platform's API is.
const API_NAME: &str = "api";

/// The longest a disabled Application's page waits for the person's name
/// (see [`Ingress::disabled_answer`] for the budget it fits in).
const NAME_READ_WAIT: Duration = Duration::from_millis(300);

/// The longest a client may take to send a request's body (the platform's
/// own forms of the Dashboard, which still read theirs whole).
pub(crate) const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest a request's body may be, under `vibe serve` and in the local
/// run alike (blackbox/CONTRACT.md, "What an Application is handed"): 5 GiB
/// and 64 KiB (5,368,774,656 bytes); exactly that is taken.
pub const MAX_REQUEST_BODY_BYTES: usize = 5 * 1024 * 1024 * 1024 + 64 * 1024;

/// A chunk of a request's body on its way to the code: at most 64 KiB
/// (blackbox/CONTRACT.md, "Storage: Bounds"). A frame from the connection
/// may be larger, and is sent in pieces of this size.
const BODY_CHUNK: usize = 64 * 1024;

/// Chunks of a request's body in flight between the connection and the
/// runtime's own stream: when the channel is full the pump waits, so the
/// connection is read only as the code (or the host, for a body being sent)
/// takes chunks. Two here, and the runtime's own stream holds at most
/// `stream_buffer_size + 1` = 17 chunks between its pump and the code: at
/// most 19 × 64 KiB ≈ 1.2 MiB is read ahead of the reader, under the 4 MiB
/// the contract states.
const BODY_CHUNKS: usize = 2;

/// How long a read of the request's body may take without a byte while it is
/// being read, before its stream fails and the request is answered `408`
/// (blackbox/CONTRACT.md, "What an Application is handed").
const BODY_STALLED_AFTER: Duration = Duration::from_secs(10);

pub struct Ingress {
    /// The installation's domain, in lower case.
    pub domain: Arc<str>,
    pub registry: Arc<Registry>,
    /// The Resources and their Grants: a request reads back the Grants
    /// memory does not know on the Resources its serving Version declares.
    pub resources: Arc<Resources>,
    pub api: Api,
    pub sign_in: Box<dyn SignIn>,
    /// OIDC sign-in and sessions, when the program has an installation's
    /// key: once connected, it alone signs a request to an Application in,
    /// and the sign-in address (`auth.<domain>`) is its.
    pub oidc: Option<Arc<Oidc>>,
    /// How a browser reaches the installation: the origin a request to an
    /// Application must come from.
    pub public: Public,
    /// Tool credentials: a sign-in on the API only.
    pub credentials: Arc<ToolCredentials>,
    pub runner: RequestRunner,
    /// Where each request the Application's code was given gets its log
    /// entry, handed over without waiting.
    pub logs: Arc<Logs>,
    /// The largest request body read; past it the answer is 413.
    pub max_request_body_bytes: usize,
    /// Whether a request is given to an Application only when its User can
    /// open it (`vibe serve`); the local run serves every test User of the
    /// project.
    pub decides_opening: bool,
    /// Where each request the Application's code was given is counted
    /// (crate::usage), without waiting; `None` where nothing is counted
    /// (the local run).
    pub usage: Option<Arc<crate::usage::Usage>>,
}

/// What the request record knows.
struct Known<'a> {
    request: &'a str,
    application: Option<&'a str>,
    version: Option<u64>,
    user: Option<&'a str>,
    method: &'a str,
    path: &'a str,
}

impl Ingress {
    /// The `403` page of an Application not shared with `id`. With OIDC
    /// the person's name is read for it (when that fails, the page shows
    /// the id alone) and it links to the sign-in address's sign-out; the
    /// stand-in keeps no record of people and has no sign-out.
    async fn not_shared_page(
        &self,
        application: &str,
        id: &str,
        port: Option<u16>,
    ) -> Response<Full<Bytes>> {
        let (name, sign_out) = self.who_is_signed_in(id, port, None).await;
        pages::not_shared(
            &self.domain,
            application,
            id,
            name.as_deref(),
            sign_out.as_deref(),
        )
    }

    /// With OIDC, the name of the person `id` (read for a page; when that
    /// fails, the page shows the id alone) and the sign-in address's
    /// sign-out; the stand-in keeps no record of people and has no
    /// sign-out.
    /// With `limit`, the read of the person's name is given that long at
    /// most, and the page names them by id alone when it does not come.
    async fn who_is_signed_in(
        &self,
        id: &str,
        port: Option<u16>,
        limit: Option<Duration>,
    ) -> (Option<String>, Option<String>) {
        if self.oidc.as_ref().is_some_and(|o| o.connected().is_some()) {
            // A page of the platform's own, not a request of the
            // Application's: one read, bounded by the pool's own limits and
            // by `limit` when given.
            let read = self.registry.person_name(id);
            let name = match limit {
                Some(limit) => tokio::time::timeout(limit, read)
                    .await
                    .ok()
                    .and_then(Result::ok),
                None => read.await.ok(),
            }
            .flatten();
            let sign_out = format!(
                "{}/sign-out",
                self.public
                    .origin(&format!("{AUTH_NAME}.{}", self.domain), port)
            );
            (name, Some(sign_out))
        } else {
            (None, None)
        }
    }

    /// What a disabled Application answers: its page to a page's request,
    /// else one sentence (blackbox/CONTRACT.md, "What Platform
    /// administrators see and stop: What a disabled Application answers").
    ///
    /// The read of the person's name is bounded by [`NAME_READ_WAIT`]: a
    /// request stopped in flight is answered within 1 second of the
    /// disable's `200`, and the runner may already have waited its 500 ms
    /// for the engine (crate::runner's `STOPPED_WAIT`): 500 + 300 ms leaves
    /// 200 ms for the rest.
    async fn disabled_answer(
        &self,
        parts: &hyper::http::request::Parts,
        application: &str,
        id: &str,
        port: Option<u16>,
    ) -> Response<Full<Bytes>> {
        if asks_for_a_page(parts) {
            let (name, sign_out) = self.who_is_signed_in(id, port, Some(NAME_READ_WAIT)).await;
            pages::disabled(
                &self.domain,
                application,
                id,
                name.as_deref(),
                sign_out.as_deref(),
            )
        } else {
            disabled(application)
        }
    }

    /// Answers one request: off the wire (`vibe serve`, whose body is
    /// [`hyper::body::Incoming`]) or handed over in the process (the local
    /// run's).
    pub async fn handle<B>(&self, request: Request<B>) -> Response<Sent>
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let id: Arc<str> = uuid::Uuid::new_v4().to_string().into();
        let (mut parts, body) = request.into_parts();
        // The connection's side of a body being sent, put there by
        // crate::server; absent in the local run's in-process requests,
        // which have no connection to end.
        let control = parts.extensions.remove::<SentControl>();
        let known = Known {
            request: &id,
            application: None,
            version: None,
            user: None,
            method: parts.method.as_str(),
            path: parts.uri.path(),
        };

        let Some((host, port)) = parts.headers.get(HOST).and_then(host_name) else {
            return finish(
                &known,
                own(StatusCode::BAD_REQUEST, "Bad request: no Host header.\n"),
            );
        };
        let stated = self.sign_in.user_of(&parts);
        if let SignedIn::Malformed(_) = stated {
            return finish(
                &known,
                own(
                    StatusCode::BAD_REQUEST,
                    "Bad request: the sign-in is malformed.\n",
                ),
            );
        }
        let name = host
            .strip_suffix(&*self.domain)
            .and_then(|rest| rest.strip_suffix('.'))
            .filter(|name| !name.contains('.'));
        if name == Some(API_NAME) {
            let anyone = matches!(parts.uri.path(), "/v1/installation" | "/v1/tool-sign-ins");
            // On the API an `Authorization` header decides alone; the
            // stand-in's headers beside it count for nothing.
            let signed = if anyone {
                Signed::Anyone
            } else {
                match tool_credentials::presented(&parts.headers) {
                    Presented::Absent => match stated {
                        SignedIn::User(user) => Signed::User(user, SignedInWith::Otherwise),
                        SignedIn::NotSignedIn | SignedIn::Malformed(_) => {
                            return finish(&known, not_signed_in());
                        }
                    },
                    Presented::Present(None) => return finish(&known, not_signed_in()),
                    Presented::Setup(digest) => Signed::Setup(digest),
                    Presented::Present(Some(digest)) => {
                        match self.credentials.user_of(digest).await {
                            Ok(Some(found)) if !found.due => {
                                Signed::User(found.user, SignedInWith::ToolCredential(digest))
                            }
                            Ok(Some(_)) => match self.renewed(digest).await {
                                Ok(user) => {
                                    Signed::User(user, SignedInWith::ToolCredential(digest))
                                }
                                Err(answer) => return finish(&known, *answer),
                            },
                            Ok(None) => return finish(&known, not_signed_in()),
                            Err(error) => {
                                let answer = Answer::unavailable(&error);
                                return finish(&known, api_response(answer));
                            }
                        }
                    }
                }
            };
            let (caller, user) = match &signed {
                Signed::User(user, with) => (Caller::User(user, *with), Some(user.id.as_str())),
                Signed::Setup(digest) => (Caller::Setup(*digest), None),
                Signed::Anyone => (Caller::Anyone, None),
            };
            let known = Known { user, ..known };
            let answer = self
                .api
                .handle(
                    caller,
                    &id,
                    &parts.method,
                    parts.uri.path(),
                    parts.uri.query(),
                    body,
                )
                .await;
            return finish(&known, api_response(answer));
        }
        let user = match self.oidc.as_ref().filter(|o| o.connected().is_some()) {
            Some(oidc) => {
                if name == Some(AUTH_NAME) {
                    return finish(&known, oidc.sign_in_address(&parts, body, port).await);
                }
                if let Some(name) = name
                    && parts.uri.path() == SESSION_PATH
                {
                    return finish(&known, oidc.session_step(&parts, name, port).await);
                }
                // Only the Application's own cookie, and only on its own
                // address, signs a request in.
                let current = match name.filter(|n| signs_in_to(n)) {
                    Some(name) => oidc.user_of(&parts, name).await,
                    None => Current::None,
                };
                match current {
                    Current::User { user, .. } => user,
                    Current::None => {
                        let answer = oidc
                            .not_signed_in(&parts, name, port)
                            .unwrap_or_else(not_signed_in);
                        return finish(&known, answer);
                    }
                    Current::Unavailable => return finish(&known, unavailable()),
                }
            }
            // On an Application's address a tool credential is no sign-in.
            None => match stated {
                SignedIn::User(user) => user,
                SignedIn::NotSignedIn | SignedIn::Malformed(_) => {
                    return finish(&known, not_signed_in());
                }
            },
        };
        // After the sign-in: a request from another origin than the
        // Application's own is refused, its code not run.
        if from_another_origin(
            &parts.method,
            &parts.headers,
            &self.public.origin(&host, port),
        ) {
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            return finish(
                &known,
                own(
                    StatusCode::FORBIDDEN,
                    "Refused: this request comes from another origin than the Application's own.\n",
                ),
            );
        }
        // The Dashboard (`vibe serve`'s; the local run has none), past its
        // sign-in and the origin rule, as an Application.
        if self.decides_opening && name == Some(DASHBOARD_NAME) {
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            return finish(&known, self.dashboard(&parts, body, &user, port).await);
        }
        // The Application as memory holds it (no round trip to the
        // platform database): who can open it and which Version serves.
        // Memory that does not know it now (a write whose answer was lost)
        // reads it back, and nothing is served on a guess meanwhile.
        let application = name.filter(|n| check_name(n).is_ok());
        // All the readings of this request share one limit, from the first.
        let mut deadline: Option<tokio::time::Instant> = None;
        let (mut found, mut stop) = match application {
            Some(name) => self.registry.find_for_request(name),
            None => (Found::NoSuchApplication, None),
        };
        if let (Found::NotKnown, Some(name)) = (&found, application) {
            let until =
                *deadline.get_or_insert_with(|| tokio::time::Instant::now() + READ_BACK_WAIT);
            found = match self.registry.read_back(name, until).await {
                // Still not held (a forgetting while it was read): not
                // known, never a guess.
                Ok(_) => {
                    let (now, its_stop) = self.registry.find_for_request(name);
                    stop = its_stop;
                    now
                }
                Err(error) => {
                    // A write on its way is no failure: it is waited out by
                    // asking again at a later request.
                    if error.code() != Some(&SqlState::LOCK_NOT_AVAILABLE) {
                        log::record(
                            "platform-database-failed",
                            json!({ "error": error.to_string() }),
                        );
                    }
                    Found::NotKnown
                }
            };
        }
        let kept = match found {
            Found::Known(kept) => kept,
            Found::NotKnown => {
                let known = Known {
                    user: Some(&user.id),
                    application,
                    ..known
                };
                return finish(&known, application_unknown());
            }
            Found::NoSuchApplication => {
                let known = Known {
                    user: Some(&user.id),
                    ..known
                };
                return finish(&known, no_application());
            }
        };
        // One not shared with the User is the `403`, before the Application
        // is handed anything, and before its serving Version is looked for.
        if self.decides_opening
            && let Some(name) = application
            && !kept.opens(&user)
        {
            let known = Known {
                user: Some(&user.id),
                application: Some(name),
                ..known
            };
            let answer = if asks_for_a_page(&parts) {
                self.not_shared_page(name, &user.id, port).await
            } else {
                not_shared(name)
            };
            return finish(&known, answer);
        }
        // Disabled by a Platform administrator: told so, after the `403`
        // (only someone who can open it learns it) and before whether a
        // Version serves. Nothing of its code runs.
        if let Some(name) = application
            && kept.disabled()
        {
            let known = Known {
                user: Some(&user.id),
                application: Some(name),
                ..known
            };
            let answer = self.disabled_answer(&parts, name, &user.id, port).await;
            return finish(&known, answer);
        }
        // Memory knows it, so it was answered with its stop.
        let stop = stop.unwrap_or_default();
        // The request holds this Version until it ends, whatever is sent
        // meanwhile.
        let Some(serving) = kept.serving().map(Arc::clone) else {
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            return finish(&known, no_application());
        };
        // The Grants memory does not know on the Resources this Version
        // declares are read back before any of its code runs, so that once a
        // write of them is settled the next request's operations are decided
        // by the Grants the database has. With nothing forgotten this costs
        // one atomic load. Readings not done in time leave those Grants not
        // known: their operations are refused `grants-unknown`.
        if self.resources.any_grants_unknown() {
            let until =
                *deadline.get_or_insert_with(|| tokio::time::Instant::now() + READ_BACK_WAIT);
            for declared in serving.declared.keys() {
                if let Some(resource) = self.resources.get(declared)
                    && !resource.grants_known()
                {
                    self.resources.read_back(&resource, until).await;
                }
            }
        }
        // The request's stated length, read here for the request: a put of
        // its own body checks the file's bound against it before a byte is
        // read ("Storage: Bounds"). The `413` it may call for is answered
        // after every other check, the method's `405` included (below).
        let stated_length = parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<u64>().ok());
        let in_flight = Arc::new(InFlight {
            request: Arc::clone(&id),
            user,
            application: Arc::clone(&serving.application),
            version: serving.version,
            declared: Arc::clone(&serving.declared),
            transactions: Transactions::default(),
            collected: Collected::default(),
            bodies: std::sync::Arc::new(crate::storage::bodies::Bodies::default()),
            calls: std::sync::Arc::new(crate::gateway::RequestCalls::default()),
            // The request's stated length: a put of its own body checks
            // the file's bound against it before a byte is read.
            content_length: stated_length,
            streams: std::sync::OnceLock::new(),
            stop,
            fetches: crate::outbound::Turns::default(),
            ends: std::sync::OnceLock::new(),
        });
        let known = Known {
            user: Some(&in_flight.user.id),
            application: Some(&in_flight.application),
            version: Some(serving.version),
            ..known
        };

        let Ok(http_method) = parts.method.as_str().parse::<HttpMethod>() else {
            return finish(
                &known,
                own(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed.\n"),
            );
        };
        // A stated length over the bound is `413` before the code runs
        // (blackbox/CONTRACT.md, "What an Application is handed"), after
        // every other check: a request that is not signed in, not shared
        // with its User, to an Application that is disabled or not there,
        // or with a method the platform does not hand on, is answered as
        // that, whatever length it states. A body that states none has its
        // bound checked as it is read, by the pump: it fails the stream,
        // and the request is answered `413` when the answer has not been
        // decided yet.
        if stated_length.is_some_and(|stated| {
            stated > u64::try_from(MAX_REQUEST_BODY_BYTES).unwrap_or(u64::MAX)
        }) {
            return finish(
                &known,
                own(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "The request body is too large.\n",
                ),
            );
        }
        // The body is handed to the runtime as a stream, fed from the
        // connection as the code reads it: nothing is collected here, and
        // the checks above came before its first byte. A request with no
        // body (or `content-length: 0`) is `None`, as before. Nothing is
        // read, and no clock runs, until the body is asked for (the code's
        // first read, or the response taking it to send): the runtime says
        // when that is, on `asked`.
        let (asked_tx, asked_rx) = watch::channel(false);
        let (body, mut pump) = if body.is_end_stream() {
            (RequestBody::None, None)
        } else {
            // Bound: `BODY_CHUNKS` chunks; when the channel is full the pump
            // waits, so the connection is read only as the code (or the
            // body's sending) takes its chunks.
            let (chunks_tx, chunks_rx) = mpsc::channel(BODY_CHUNKS);
            let pump = Pump::start(body, chunks_tx, self.max_request_body_bytes, asked_rx);
            (RequestBody::Stream(chunks_rx), Some(pump))
        };
        remove_reserved_headers(&mut parts.headers);
        tool_credentials::remove_credentials(&mut parts.headers);
        cookies::remove_platform_cookies(&mut parts.headers);
        let path_and_query = parts.uri.path_and_query().map_or("/", |p| p.as_str());
        let request = HttpRequest {
            method: http_method,
            url: format!(
                "http://{}.{}{}",
                serving.application, self.domain, path_and_query
            ),
            headers: for_application(&parts.headers),
            body,
        };

        let outcome = self
            .runner
            .run(Arc::clone(&in_flight), serving, request, asked_tx)
            .await;
        // How the body ended, when it has: a failure found before the
        // answer was decided is the platform's to answer for, whatever the
        // code did; one found later ends the connection, which the client
        // can tell by the `content-length` it was promised.
        let body_failed = pump.as_ref().and_then(Pump::ended);
        let ran = matches!(
            outcome,
            Outcome::Answered(_) | Outcome::Failed | Outcome::Stopped { given: true }
        );
        // Disabled while it was in flight, whatever its code did: answered
        // as a disabled Application is, unless its answer had begun to go.
        let outcome = if in_flight.stop.is_stopped() {
            Outcome::Stopped { given: ran }
        } else {
            outcome
        };
        let platform_answer = match body_failed {
            Some(BodyEnd::TooLarge) => Some((
                StatusCode::PAYLOAD_TOO_LARGE,
                "The request body is too large.\n",
            )),
            Some(BodyEnd::Stalled) => Some((
                StatusCode::REQUEST_TIMEOUT,
                "The request body did not arrive in time.\n",
            )),
            Some(BodyEnd::Ended | BodyEnd::Gone | BodyEnd::GivenUp) | None => None,
        };
        // A disable comes first (CONTRACT.md, "What an Application is
        // handed"): a request to an Application disabled meanwhile is
        // answered as disabled, also when its body had failed.
        let mut response = match (outcome, platform_answer) {
            (Outcome::Stopped { .. }, _) => self
                .disabled_answer(&parts, &in_flight.application, &in_flight.user.id, port)
                .await
                .map(Sent::from),
            (_, Some((status, text))) => own(status, text).map(Sent::from),
            (Outcome::Answered(answer), None) => {
                // An answer with no body on the wire has none, whatever the
                // code answered (CONTRACT.md, "What an Application is
                // handed"): nothing is sent, a `204` carries no
                // `content-length`, and a body given back is not asked for.
                let bodiless = parts.method == Method::HEAD || matches!(answer.status, 204 | 304);
                // The length the platform states for a body it sends
                // itself: a file's size; `None` for the request's own body
                // given back (its stated one, or none and the body is sent
                // chunked).
                let answer_length = match &answer.body {
                    AnswerBody::Streamed { length, .. } => *length,
                    AnswerBody::Whole(_) => None,
                };
                let mut response = match answer.body {
                    AnswerBody::Whole(_) if bodiless => {
                        Response::new(Sent::Whole(Full::new(Bytes::new())))
                    }
                    AnswerBody::Whole(body) => Response::new(Sent::Whole(Full::new(body))),
                    AnswerBody::Streamed { .. } if bodiless => {
                        // Not asked for: the runtime closed the stream, and
                        // the pump (not moved into the response) is dropped
                        // with the request, so nothing is read from the
                        // client.
                        Response::new(Sent::Whole(Full::new(Bytes::new())))
                    }
                    AnswerBody::Streamed { chunks, held, .. } => {
                        // A body the host itself feeds, which the code did
                        // not read: the host sends it, and what feeds it
                        // goes on past the request's code — the connection's
                        // own pump for the request's body given back, the
                        // file's pump taken over by the answer for a
                        // Storage file's. The connection watches the sending
                        // (see crate::sent).
                        let mut owned = held;
                        if let Some(task) = pump.as_mut().and_then(Pump::take_task) {
                            owned.push(task);
                        }
                        Response::new(Sent::Stream(SentStream::new(
                            chunks,
                            control.clone(),
                            Some(Arc::clone(&in_flight.stop)),
                            owned,
                        )))
                    }
                };
                *response.status_mut() = StatusCode::from_u16(answer.status)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                // How the answer is framed is the platform's: the code's own
                // framing headers are never sent (CONTRACT.md, "How a
                // response is framed").
                copy_headers(response.headers_mut(), &in_flight, answer.headers);
                if bodiless {
                    response.headers_mut().remove(CONTENT_LENGTH);
                    response.headers_mut().remove(TRANSFER_ENCODING);
                } else if matches!(response.body(), Sent::Stream(_)) {
                    // The host sets `content-length` itself: the file's
                    // size for a file, whatever the code set; for the
                    // request's own body given back, the request's stated
                    // one, and when the request stated none, none — and
                    // the body is sent chunked.
                    match answer_length.or(stated_length) {
                        Some(length) => {
                            if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
                                response.headers_mut().insert(CONTENT_LENGTH, value);
                            }
                        }
                        None => {
                            response.headers_mut().remove(CONTENT_LENGTH);
                        }
                    }
                }
                response
            }
            (Outcome::Failed, None) => own(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The Application failed. The reason is in the program's log.\n",
            )
            .map(Sent::from),
            (Outcome::Busy, None) => own(
                StatusCode::SERVICE_UNAVAILABLE,
                "Busy: try again shortly.\n",
            )
            .map(Sent::from),
        };
        // The Application's code was given the request: its log entry,
        // made now and handed over without waiting, before the request
        // record. The head is decided now; a body it streams goes on after
        // this, and the records do not wait for it.
        if ran {
            self.logs.request_ended(
                &in_flight,
                &parts.method,
                parts.uri.path(),
                response.status(),
            );
            response.extensions_mut().insert(GivenToCode);
            if let Some(usage) = &self.usage {
                usage.count(&in_flight.application, &in_flight.user.id);
            }
        }
        finish(&known, response)
    }
}

/// Who a request to the API is from, once Ingress has looked.
enum Signed {
    User(User, SignedInWith),
    Setup(Digest),
    Anyone,
}

impl Ingress {
    /// The User of a tool credential from a sign-in found due, its Groups
    /// read again (crate::oidc::sessions); else the answer: `401` when it
    /// ended, `503` when the provider (or the connection) cannot be asked.
    async fn renewed(&self, digest: Digest) -> Result<User, Box<Response<Full<Bytes>>>> {
        let outcome = match &self.oidc {
            Some(oidc) => oidc.renew_tool(*digest.bytes()).await,
            None => Renewed::Unavailable,
        };
        match outcome {
            Renewed::User(user) => Ok(user),
            Renewed::None => Err(Box::new(not_signed_in())),
            Renewed::Unavailable => Err(Box::new(api_response(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the company's sign-in cannot be asked for your Groups just now: try again shortly",
            )))),
        }
    }
}

/// The `Host` without its port, in lower case, and its port; `None` when
/// there is none.
fn host_name(value: &HeaderValue) -> Option<(String, Option<u16>)> {
    let text = value.to_str().ok()?.trim();
    let (name, port) = match text.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => (name, port.parse().ok()),
        _ => (text, None),
    };
    (!name.is_empty()).then(|| (name.to_ascii_lowercase(), port))
}

/// The headers as the runtime takes them: one value per name, repeated
/// headers joined as the Fetch standard joins them.
fn for_application(headers: &HeaderMap) -> HashMap<String, String> {
    let mut joined: HashMap<String, String> = HashMap::with_capacity(headers.keys_len());
    for (name, value) in headers {
        let value = String::from_utf8_lossy(value.as_bytes());
        joined
            .entry(name.as_str().to_owned())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(&value);
            })
            .or_insert_with(|| value.into_owned());
    }
    joined
}

/// The `404` of an address that is no Application's, or of one with no
/// serving Version.
fn no_application() -> Response<Full<Bytes>> {
    own(
        StatusCode::NOT_FOUND,
        "Not found: no Application at this address.\n",
    )
}

/// The `503` of a request to an Application the program does not know now
/// (who may open it, or which Version serves it) and cannot read back.
fn application_unknown() -> Response<Full<Bytes>> {
    let mut response = own(
        StatusCode::SERVICE_UNAVAILABLE,
        "Unavailable: this Application cannot be served just now. Try again shortly.\n",
    );
    response.headers_mut().insert(
        RETRY_AFTER,
        HeaderValue::from_static(crate::oidc::pages::RETRY_AFTER_SECONDS),
    );
    response
}

/// The `403` of an Application not shared with the User, for a request
/// that is not a page: one sentence naming the Application (a DNS label of
/// at most 63 characters, so the body is at most 160 bytes), nothing of
/// the User's.
fn not_shared(application: &str) -> Response<Full<Bytes>> {
    let mut response = own(StatusCode::FORBIDDEN, "");
    *response.body_mut() = Full::new(Bytes::from(format!(
        "The Application {application} is not shared with you: ask one of its Contributors to share it.\n"
    )));
    response
}

/// A disabled Application's answer to a request that is not a page: one
/// sentence naming it (a DNS label of at most 63 characters, so the body
/// is at most 133 bytes), and no `retry-after`.
fn disabled(application: &str) -> Response<Full<Bytes>> {
    let mut response = own(StatusCode::SERVICE_UNAVAILABLE, "");
    *response.body_mut() = Full::new(Bytes::from(format!(
        "The Application {application} has been disabled by a Platform administrator.\n"
    )));
    response
}

/// The `401` of anyone not signed in, whatever was asked for and however
/// the sign-in failed.
fn not_signed_in() -> Response<Full<Bytes>> {
    own(StatusCode::UNAUTHORIZED, "Not signed in.\n")
}

/// `503`: whether the request is signed in cannot be known now.
fn unavailable() -> Response<Full<Bytes>> {
    let mut response = own(
        StatusCode::SERVICE_UNAVAILABLE,
        "Unavailable: the sign-in cannot be checked just now. Try again shortly.\n",
    );
    response.headers_mut().insert(
        RETRY_AFTER,
        HeaderValue::from_static(crate::oidc::pages::RETRY_AFTER_SECONDS),
    );
    response
}

/// An answer of the API as an HTTP response.
fn api_response(answer: Answer) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(answer.body));
    *response.status_mut() = answer.status;
    if answer.status != StatusCode::NO_CONTENT {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    if answer.status == StatusCode::SERVICE_UNAVAILABLE {
        response.headers_mut().insert(
            RETRY_AFTER,
            HeaderValue::from_static(crate::oidc::pages::RETRY_AFTER_SECONDS),
        );
    }
    response
}

/// One of the program's own answers.
fn own(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    crate::oidc::pages::own_headers(response.headers_mut());
    response
}

/// The headers a response's body is framed with: the platform's to set,
/// never the code's (blackbox/CONTRACT.md, "How a response is framed").
const FRAMING_HEADERS: [&str; 7] = [
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
];

/// The Application's own headers on its answer: the platform's headers are
/// the platform's to set, the framing headers are the platform's alone, and
/// no code sets the platform's cookies or a cookie the other Applications
/// would be sent.
fn copy_headers(headers: &mut HeaderMap, in_flight: &InFlight, code: Vec<(String, String)>) {
    for (name, value) in code {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value))
        else {
            continue;
        };
        if name.as_str().starts_with(RESERVED_HEADER_PREFIX) {
            continue;
        }
        if FRAMING_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if name == SET_COOKIE
            && let Some(dropped) = cookies::judge_set_cookie(value.as_bytes())
        {
            log::record(
                "set-cookie-dropped",
                json!({
                    "application": &*in_flight.application,
                    "reason": dropped.reason(),
                }),
            );
            continue;
        }
        headers.append(name, value);
    }
}

/// Writes the request record, then names the request on its answer.
fn finish<B: Into<Sent>>(known: &Known<'_>, response: Response<B>) -> Response<Sent> {
    log::record(
        "request",
        json!({
            "request": known.request,
            "application": known.application,
            "version": known.version,
            "user": known.user,
            "method": known.method,
            "path": known.path,
            "status": response.status().as_u16(),
        }),
    );
    let mut response = response.map(Into::into);
    if let Ok(id) = HeaderValue::from_str(known.request) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), id);
    }
    response
}

/// How a request's body ended, as far as the platform must answer for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyEnd {
    /// The body arrived whole.
    Ended,
    /// The body was given up before its end: nobody asked for it, or its
    /// reader went away, or the sending stopped. Nothing is owed for it
    /// (CONTRACT.md, "What an Application is handed": a body nobody has
    /// asked for is not read and not timed; a body the code stopped reading
    /// is dropped).
    GivenUp,
    /// A body with no stated length passed [`MAX_REQUEST_BODY_BYTES`].
    TooLarge,
    /// No byte arrived for [`BODY_STALLED_AFTER`] while it was being read.
    Stalled,
    /// The client's side failed otherwise (it went away, say): no answer of
    /// the platform's is owed for it.
    Gone,
}

/// The request's body, read off the connection into the runtime's stream by
/// a task of its own, with an owner: dropping the [`Pump`] ends the task,
/// which drops the rest of the body unread. The task is handed to the
/// response when the response is the body itself, so that a body being sent
/// goes on being read after the code has ended.
struct Pump {
    task: Option<OwnedTask>,
    ended: Arc<Mutex<Option<BodyEnd>>>,
}

impl Pump {
    /// Starts reading `body` into `chunks`, at most `max` bytes, once
    /// `asked` says the body is wanted.
    fn start<B>(
        body: B,
        chunks: mpsc::Sender<Result<Bytes, String>>,
        max: usize,
        asked: watch::Receiver<bool>,
    ) -> Self
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let ended = Arc::new(Mutex::new(None));
        let reported = Arc::clone(&ended);
        let task = tokio::spawn(async move {
            read_body(body, chunks, max, reported, asked).await;
        });
        Self {
            task: Some(OwnedTask::new(task)),
            ended,
        }
    }

    /// How the body ended, once it has; `None` while it is being read.
    fn ended(&self) -> Option<BodyEnd> {
        *self
            .ended
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes the task, for a body the response sends itself: it must go on
    /// feeding the body after the request's code has ended.
    fn take_task(&mut self) -> Option<OwnedTask> {
        self.task.take()
    }
}

/// Reads the request's body into `chunks` a piece of at most [`BODY_CHUNK`]
/// at a time, under the body's bound and the no-byte wait, once `asked` says
/// the code (or the response) wants it. A failure is put on the stream (the
/// code's read fails with it) and kept for the platform's answer
/// ([`Pump::ended`]).
async fn read_body<B>(
    body: B,
    chunks: mpsc::Sender<Result<Bytes, String>>,
    max: usize,
    ended: Arc<Mutex<Option<BodyEnd>>>,
    mut asked: watch::Receiver<bool>,
) where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    // Nothing is taken from the client, and no clock runs, before the body
    // is asked for. This wait has no timeout of its own and needs none: its
    // owner ends it. The signal's sender is dropped when the request is
    // never given to the code (the runner answers `Busy`, or the
    // Application is disabled while the request waits), and the pump task
    // itself is aborted when the request's handling ends (`Pump` is dropped,
    // or the `SentStream` that took it).
    if asked.wait_for(|asked| *asked).await.is_err() {
        finish_body(&ended, BodyEnd::GivenUp);
        return;
    }
    let mut body = Limited::new(body, max);
    loop {
        let frame = match tokio::time::timeout(BODY_STALLED_AFTER, body.frame()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(None) => {
                finish_body(&ended, BodyEnd::Ended);
                return;
            }
            Ok(Some(Err(error))) => {
                // Only the bound has an answer of the platform's; another
                // failure is the client's own (it went away).
                let kind = if error.downcast_ref::<LengthLimitError>().is_some() {
                    BodyEnd::TooLarge
                } else {
                    BodyEnd::Gone
                };
                finish_body(&ended, kind);
                // No timeout of its own: when the code is reading, the
                // runtime takes the chunk at once; otherwise the message is
                // dropped with the receiver, and the pump is aborted at the
                // request's end, or (for a body being sent) by the 30-second
                // watchdog or the response body being dropped.
                chunks.send(Err(BODY_FAILED.to_owned())).await.ok();
                return;
            }
            Err(_) => {
                finish_body(&ended, BodyEnd::Stalled);
                // As above: bounded by the reader or by the pump's owner,
                // not by a timeout here.
                chunks.send(Err(BODY_FAILED.to_owned())).await.ok();
                return;
            }
        };
        let Some(bytes) = frame.into_data().ok() else {
            // Trailers: nothing of the body itself.
            continue;
        };
        // `split_to` is a view of the frame, never a copy: the frame from
        // the connection may be larger than a chunk, and each piece is at
        // most `BODY_CHUNK`.
        let mut rest = bytes;
        while !rest.is_empty() {
            let take = rest.len().min(BODY_CHUNK);
            let piece = rest.split_to(take);
            // Waits while the room is full (backpressure), and is ended by
            // its owner: the receiver going away (the code stopped reading,
            // the request ended) makes it fail at once; a body being sent
            // cannot stall here for ever either (the response body's drop,
            // or its 30-second watchdog, aborts the pump).
            if chunks.send(Ok(piece)).await.is_err() {
                // The body was dropped before it was read: nothing more.
                finish_body(&ended, BodyEnd::GivenUp);
                return;
            }
        }
    }
}

/// What a code read of a failed body sees.
const BODY_FAILED: &str =
    "the request's body failed: it is too large, or it stalled, or the client went away";

/// Notes how the body ended, once.
fn finish_body(ended: &Mutex<Option<BodyEnd>>, kind: BodyEnd) {
    let mut ended = ended
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if ended.is_none() {
        *ended = Some(kind);
    }
}
