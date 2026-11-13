//! Ingress: takes an HTTP request off the wire and answers it.
//!
//! In order (blackbox/CONTRACT.md, "Responses the program itself gives"):
//! a request id; the `Host`; who the User is ([`SignIn`]); then either the
//! platform's API (`api.<domain>`, [`Api`]) or an Application
//! (`<application>.<domain>`), whose serving Version the Application
//! registry holds in memory, run by the Request runner. Its own
//! refusals are short plain text. Every answer carries `x-vm-request-id`,
//! and the request's `request` record is written just before the answer
//! goes, after everything the Application did with it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HOST, HeaderName, HeaderValue};
use hyper::{HeaderMap, Request, Response, StatusCode};
use openworkers_core::{HttpMethod, HttpRequest, RequestBody};
use serde_json::json;

use crate::api::Api;
use crate::gateway::Transactions;
use crate::log;
use crate::registry::Registry;
use crate::runner::{InFlight, Outcome, RequestRunner};
use crate::sign_in::{RESERVED_HEADER_PREFIX, SignIn, SignedIn, remove_reserved_headers};

/// The header that names a request, in every answer and every record.
pub const REQUEST_ID_HEADER: &str = "x-vm-request-id";

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
    pub runner: RequestRunner,
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
    pub async fn handle(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
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

        let Some(host) = parts.headers.get(HOST).and_then(host_name) else {
            return finish(
                &known,
                own(StatusCode::BAD_REQUEST, "Bad request: no Host header.\n"),
            );
        };
        let user = match self.sign_in.user_of(&parts.headers) {
            SignedIn::User(user) => user,
            SignedIn::Malformed(_) => {
                return finish(
                    &known,
                    own(
                        StatusCode::BAD_REQUEST,
                        "Bad request: the sign-in is malformed.\n",
                    ),
                );
            }
            SignedIn::NotSignedIn => {
                return finish(&known, own(StatusCode::UNAUTHORIZED, "Not signed in.\n"));
            }
        };

        let name = host
            .strip_suffix(&*self.domain)
            .and_then(|rest| rest.strip_suffix('.'))
            .filter(|name| !name.contains('.'));
        if name == Some(API_NAME) {
            let known = Known {
                user: Some(&user.id),
                ..known
            };
            let answer = self
                .api
                .handle(
                    &user,
                    &parts.method,
                    parts.uri.path(),
                    parts.uri.query(),
                    body,
                )
                .await;
            let mut response = Response::new(Full::new(answer.body));
            *response.status_mut() = answer.status;
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            return finish(&known, response);
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
        finish(&known, response)
    }
}

/// The `Host` without its port, in lower case; `None` when there is none.
fn host_name(value: &HeaderValue) -> Option<String> {
    let text = value.to_str().ok()?.trim();
    let name = match text.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => text,
    };
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
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

/// One of the program's own answers.
fn own(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
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
