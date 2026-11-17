//! Ingress: takes an HTTP request off the wire and answers it.
//!
//! In order (blackbox/CONTRACT.md, "Responses the program itself gives"):
//! a request id; the `Host`; a malformed stand-in sign-in ([`SignIn`]);
//! then who the User is. On the platform's API (`api.<domain>`, [`Api`]) an
//! `Authorization` header, when there is one, decides alone: a tool
//! credential ([`ToolCredentials`]) that is alive signs its User in, and
//! anything else is the `401` of anyone not signed in, whatever stand-in
//! headers come with it. With OIDC ([`Oidc`]) the sign-in address
//! (`auth.<domain>`) and every Application's `/_vm/session` are its, and
//! on an Application's address (`<application>.<domain>`) only that
//! Application's session cookie signs a request in; without, only the
//! [`SignIn`] counts. Then a request from another origin than the
//! Application's own is refused ([`from_another_origin`]), whatever the
//! sign-in; then the serving Version, which the Application registry holds
//! in memory, is run by the Request runner, the platform's cookies taken
//! out of the request and kept out of the answer ([`cookies`]). Its own
//! refusals are short plain text. Every answer carries `x-vm-request-id`,
//! and the request's `request` record is written just before the answer
//! goes, after everything the Application did with it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::{CONTENT_TYPE, HOST, HeaderName, HeaderValue, RETRY_AFTER, SET_COOKIE};
use hyper::{HeaderMap, Request, Response, StatusCode};
use openworkers_core::{HttpMethod, HttpRequest, RequestBody};
use serde_json::json;

use crate::api::{Answer, Api, SignedInWith};
use crate::app_logs::{Collected, Logs};
use crate::cookies;
use crate::gateway::Transactions;
use crate::log;
use crate::oidc::sessions::Current;
use crate::oidc::{AUTH_NAME, Oidc, SESSION_PATH};
use crate::origin::{Public, from_another_origin};
use crate::registry::{Registry, check_name};
use crate::runner::{InFlight, Outcome, RequestRunner};
use crate::sign_in::{RESERVED_HEADER_PREFIX, SignIn, SignedIn, remove_reserved_headers};
use crate::tool_credentials::{self, Presented, ToolCredentials};

/// The header that names a request, in every answer and every record.
pub const REQUEST_ID_HEADER: &str = "x-vm-request-id";

/// Marks, in a response's extensions (never sent), an answer to a request
/// the Application's code was given: for the local run's lines of what it
/// served (crate::tool's `local`).
#[derive(Clone, Copy, Debug)]
pub struct GivenToCode;

/// The name under the domain where the platform's API is.
const API_NAME: &str = "api";

/// The longest a client may take to send a request's body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Ingress {
    /// The installation's domain, in lower case.
    pub domain: Arc<str>,
    pub registry: Arc<Registry>,
    pub api: Api,
    pub sign_in: Box<dyn SignIn>,
    /// OIDC sign-in and sessions, when the program was started with it:
    /// then it alone signs a request to an Application in, and the sign-in
    /// address (`auth.<domain>`) is its.
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
    /// Answers one request: off the wire (`vm serve`, whose body is
    /// [`hyper::body::Incoming`]) or handed over in the process (the local
    /// run's).
    pub async fn handle<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let id: Arc<str> = uuid::Uuid::new_v4().to_string().into();
        let (mut parts, body) = request.into_parts();
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
            // On the API an `Authorization` header decides alone; the
            // stand-in's headers beside it count for nothing.
            let (user, with) = match tool_credentials::presented(&parts.headers) {
                Presented::Absent => match stated {
                    SignedIn::User(user) => (user, SignedInWith::Otherwise),
                    SignedIn::NotSignedIn | SignedIn::Malformed(_) => {
                        return finish(&known, not_signed_in());
                    }
                },
                Presented::Present(None) => return finish(&known, not_signed_in()),
                Presented::Present(Some(digest)) => match self.credentials.user_of(digest).await {
                    Ok(Some(user)) => (user, SignedInWith::ToolCredential(digest)),
                    Ok(None) => return finish(&known, not_signed_in()),
                    Err(error) => {
                        let answer = Answer::unavailable(&error);
                        return finish(&known, api_response(answer));
                    }
                },
            };
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            let answer = self
                .api
                .handle(
                    &user,
                    with,
                    &id,
                    &parts.method,
                    parts.uri.path(),
                    parts.uri.query(),
                    body,
                )
                .await;
            return finish(&known, api_response(answer));
        }
        let user = match &self.oidc {
            Some(oidc) => {
                if name == Some(AUTH_NAME) {
                    return finish(&known, oidc.sign_in_address(&parts, port).await);
                }
                if let Some(name) = name
                    && parts.uri.path() == SESSION_PATH
                {
                    return finish(&known, oidc.session_step(&parts, name, port).await);
                }
                // Only the Application's own cookie, and only on its own
                // address, signs a request in.
                let current = match name.filter(|n| check_name(n).is_ok()) {
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
        // From memory: no round trip to the platform database. The request
        // holds this Version until it ends, whatever is sent meanwhile.
        let Some(serving) = name.and_then(|name| self.registry.serving(name)) else {
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            return finish(
                &known,
                own(
                    StatusCode::NOT_FOUND,
                    "Not found: no Application at this address.\n",
                ),
            );
        };
        let in_flight = Arc::new(InFlight {
            request: Arc::clone(&id),
            user,
            application: Arc::clone(&serving.application),
            version: serving.version,
            declared: Arc::clone(&serving.declared),
            transactions: Transactions::default(),
            collected: Collected::default(),
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
        let read = Limited::new(body, self.max_request_body_bytes).collect();
        let body = match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(collected)) => collected.to_bytes(),
            Err(_) => {
                return finish(
                    &known,
                    own(
                        StatusCode::REQUEST_TIMEOUT,
                        "The request body did not arrive in time.\n",
                    ),
                );
            }
            Ok(Err(_)) => {
                return finish(
                    &known,
                    own(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "The request body is too large.\n",
                    ),
                );
            }
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
            body: if body.is_empty() {
                RequestBody::None
            } else {
                RequestBody::Bytes(body)
            },
        };

        let outcome = self
            .runner
            .run(Arc::clone(&in_flight), serving, request)
            .await;
        let ran = !matches!(outcome, Outcome::Busy);
        let response = match outcome {
            Outcome::Answered(answer) => {
                let mut response = Response::new(Full::new(answer.body));
                *response.status_mut() = StatusCode::from_u16(answer.status)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let headers = response.headers_mut();
                for (name, value) in answer.headers {
                    let (Ok(name), Ok(value)) =
                        (HeaderName::try_from(name), HeaderValue::try_from(value))
                    else {
                        continue;
                    };
                    // The platform's headers are the platform's to set.
                    if name.as_str().starts_with(RESERVED_HEADER_PREFIX) {
                        continue;
                    }
                    // Nor can the code set the platform's cookies, or a
                    // cookie the other Applications would be sent.
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
                response
            }
            Outcome::Failed => own(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The Application failed. The reason is in the program's log.\n",
            ),
            Outcome::Busy => own(
                StatusCode::SERVICE_UNAVAILABLE,
                "Busy: try again shortly.\n",
            ),
        };
        // The Application's code was given the request: its log entry,
        // made now and handed over without waiting, before the request
        // record.
        let mut response = response;
        if ran {
            self.logs.request_ended(
                &in_flight,
                &parts.method,
                parts.uri.path(),
                response.status(),
            );
            response.extensions_mut().insert(GivenToCode);
        }
        finish(&known, response)
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

/// Writes the request record, then names the request on its answer.
fn finish(known: &Known<'_>, mut response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
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
    if let Ok(id) = HeaderValue::from_str(known.request) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), id);
    }
    response
}
