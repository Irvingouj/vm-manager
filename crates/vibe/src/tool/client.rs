//! The tool's HTTP client: one HTTP/1.1 request to the installation's API
//! (blackbox/CONTRACT.md, "The tool: Signing in", "The tool reaches an
//! installation over HTTPS"): over HTTPS to `api.<domain>` (its port, and the
//! `--resolve` address in place of the system's resolution when given), the
//! certificate checked for that name against the authorities of
//! crate::trust; or plain to a loopback address with `--connect`. `Host` is
//! `api.<domain>`, with `:<port>` over HTTPS on a port not 443.
//!
//! The certificate's check cannot be turned off. A handshake that fails is
//! [`Failure::Certificate`] before any byte of the request is written: rustls
//! sends nothing of the application's data until the handshake is done, and
//! the request is written only after it. Which certificate failure it was is
//! read from rustls's error by its type (crate::trust::refusal).
//!
//! Built on hyper's client connection, which the program already has for
//! serving, driven on the tool's own thread: the connection's future is
//! polled beside the request's, never spawned, so nothing outlives the
//! call. Every wait has an end: [`CONNECT_TIMEOUT`] to be let in (resolving,
//! connecting and the TLS handshake together),
//! [`ANSWER_TIMEOUT`] for the whole answer after that (or the end the
//! caller gives, with [`send_waiting`]); and the answer's body is read up
//! to [`MAX_ANSWER_BYTES`] (hyper bounds its head). The body sent is the
//! caller's, which bounds it.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use super::credential_file::Secret;

/// The longest the installation may take to accept the connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest the installation may take to answer in full, once
/// connected.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest answer body read; a longer one is not an answer the tool
/// understands.
pub const MAX_ANSWER_BYTES: usize = 8 * 1024 * 1024;

/// The port of HTTPS when none is given.
pub const HTTPS_PORT: u16 = 443;

/// How an installation is reached.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Plain HTTP to this loopback address (`--connect`).
    Plain(SocketAddr),
    /// HTTPS to `api.<domain>` on this port, at the `--resolve` address
    /// when given, else where the system resolves the name.
    Https { port: u16, resolve: Option<IpAddr> },
}

/// Where an installation is.
#[derive(Clone, Copy)]
pub struct Installation<'a> {
    pub domain: &'a str,
    pub route: Route,
}

impl Installation<'_> {
    /// `api.<domain>`.
    pub fn api_name(&self) -> String {
        format!("api.{}", self.domain)
    }

    /// The options of `vibe sign-in` that reach it, from `--domain` on.
    pub fn options(&self) -> String {
        let domain = super::shell_word(self.domain);
        match self.route {
            Route::Plain(connect) => format!("--domain {domain} --connect {connect}"),
            Route::Https { port, resolve } => {
                let mut options = format!("--domain {domain}");
                if let Some(ip) = resolve {
                    put!(options, " --resolve {ip}");
                }
                if port != HTTPS_PORT {
                    put!(options, " --port {port}");
                }
                options
            }
        }
    }

    /// The `Host` header's value.
    fn host(&self) -> String {
        match self.route {
            Route::Https { port, .. } if port != HTTPS_PORT => {
                format!("api.{}:{port}", self.domain)
            }
            Route::Plain(_) | Route::Https { .. } => self.api_name(),
        }
    }
}

/// Where it is, as a message names it: the `--connect` address, or
/// `api.<domain>:<port>` (and the `--resolve` address).
impl fmt::Display for Installation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.route {
            Route::Plain(connect) => write!(f, "{connect}"),
            Route::Https {
                port,
                resolve: None,
            } => write!(f, "api.{}:{port}", self.domain),
            Route::Https {
                port,
                resolve: Some(ip),
            } => write!(f, "api.{}:{port} (at {ip})", self.domain),
        }
    }
}

/// How a request is signed in.
#[derive(Clone, Copy)]
pub enum Sign<'a> {
    /// With the tool credential (or the setup credential, which has the
    /// same shape).
    Credential(&'a Secret),
    /// Not signed in.
    Anyone,
    /// With the stand-in: the User and, if any, the Groups, comma-separated.
    StandIn {
        user: &'a str,
        groups: Option<&'a str>,
    },
}

/// What the installation answered.
pub struct Reply {
    pub status: StatusCode,
    /// The body, when it is JSON (by its content type) and parses.
    pub json: Option<Value>,
}

/// Why there is no answer.
pub enum Failure {
    /// Nothing could be asked, or nothing came back in time: why, in a few
    /// words.
    Unreachable(String),
    /// Something answered, but not in HTTP the tool understands.
    Unexpected(String),
    /// The TLS handshake failed, on the certificate or otherwise: what it
    /// was, in words. Nothing of the request was sent.
    Certificate(String),
    /// `VIBE_CA_FILE` cannot be used: crate::trust's message. Nothing was
    /// sent.
    CaFile(String),
}

/// Sends one request and reads its whole answer, waiting for it
/// [`ANSWER_TIMEOUT`] at most.
pub async fn send(
    installation: &Installation<'_>,
    method: Method,
    path: &str,
    sign: Sign<'_>,
    body: Option<Bytes>,
) -> Result<Reply, Failure> {
    send_waiting(installation, method, path, sign, body, ANSWER_TIMEOUT).await
}

/// As [`send`], waiting for the whole answer `answer_timeout` at most once
/// connected.
pub async fn send_waiting(
    installation: &Installation<'_>,
    method: Method,
    path: &str,
    sign: Sign<'_>,
    body: Option<Bytes>,
    answer_timeout: Duration,
) -> Result<Reply, Failure> {
    let request = request(installation, method, path, sign, body)
        .map_err(|()| Failure::Unexpected("the request could not be written".into()))?;
    match installation.route {
        Route::Plain(connect) => {
            let stream = within_connect(TcpStream::connect(connect)).await?;
            answered(answer_timeout, exchange(stream, request)).await
        }
        Route::Https { port, resolve } => {
            // The authorities first: a file that cannot be used ends the
            // command before anything is sent.
            let config = crate::trust::client_config().map_err(Failure::CaFile)?;
            let server_name = rustls::pki_types::ServerName::try_from(installation.api_name())
                .map_err(|_| {
                    Failure::Unexpected(format!(
                        "{} is not a name TLS can check",
                        installation.api_name()
                    ))
                })?;
            let stream = within_connect(async {
                let tcp = match resolve {
                    Some(ip) => TcpStream::connect(SocketAddr::new(ip, port)).await,
                    None => TcpStream::connect((installation.api_name(), port)).await,
                }
                .map_err(|error| Failure::Unreachable(error.to_string()))?;
                tokio_rustls::TlsConnector::from(config)
                    .connect(server_name, tcp)
                    .await
                    .map_err(|error| refused(installation, &error))
            })
            .await?;
            answered(answer_timeout, exchange(stream, request)).await
        }
    }
}

/// `connecting` (resolving, connecting and any handshake) within
/// [`CONNECT_TIMEOUT`].
async fn within_connect<T, E>(connecting: impl Future<Output = Result<T, E>>) -> Result<T, Failure>
where
    Failure: From<E>,
{
    match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => Err(Failure::from(error)),
        Err(_) => Err(Failure::Unreachable(format!(
            "it did not accept the connection within {} s",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Unreachable(error.to_string())
    }
}

/// The exchange, waited for `answer_timeout` at most.
async fn answered(
    answer_timeout: Duration,
    exchanging: impl Future<Output = Result<Reply, Failure>>,
) -> Result<Reply, Failure> {
    match tokio::time::timeout(answer_timeout, exchanging).await {
        Ok(answered) => answered,
        Err(_) => Err(Failure::Unreachable(format!(
            "it did not answer within {} s",
            answer_timeout.as_secs()
        ))),
    }
}

/// A TLS handshake with `installation` that failed: which way, in words
/// that say what to do.
fn refused(installation: &Installation<'_>, error: &std::io::Error) -> Failure {
    use crate::trust::{Refusal, VARIABLE};
    let name = installation.api_name();
    Failure::Certificate(match crate::trust::refusal(error) {
        Some(Refusal::OtherName) => format!(
            "the certificate of {installation} is for another name, not for {name}, the name asked for: check --domain, or have the installation's certificate made for {name}; nothing was sent"
        ),
        Some(Refusal::UnknownAuthority) => format!(
            "the certificate of {installation} is issued by a certificate authority this program does not trust: if it is your company's own authority, put that authority's certificate (PEM) in a file and name it with {VARIABLE}, then run the command again; nothing was sent"
        ),
        Some(Refusal::Expired) => format!(
            "the certificate of {installation} has expired: whoever runs the installation must renew it; nothing was sent"
        ),
        Some(Refusal::NotValidYet) => format!(
            "the certificate of {installation} is not valid yet (check this machine's clock): nothing was sent"
        ),
        Some(Refusal::Other) | None => format!(
            "the TLS handshake with {installation} failed ({error}): check that it is an installation that speaks HTTPS on this port; nothing was sent"
        ),
    })
}

fn request(
    installation: &Installation<'_>,
    method: Method,
    path: &str,
    sign: Sign<'_>,
    body: Option<Bytes>,
) -> Result<Request<Full<Bytes>>, ()> {
    let host = HeaderValue::try_from(installation.host()).map_err(|_| ())?;
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, host);
    match sign {
        Sign::Anyone => {}
        Sign::Credential(secret) => {
            let mut value =
                HeaderValue::try_from(format!("Bearer {}", secret.expose())).map_err(|_| ())?;
            value.set_sensitive(true);
            builder = builder.header(AUTHORIZATION, value);
        }
        Sign::StandIn { user, groups } => {
            builder = builder.header(
                crate::sign_in::STAND_IN_USER_HEADER,
                HeaderValue::from_bytes(user.as_bytes()).map_err(|_| ())?,
            );
            if let Some(groups) = groups {
                builder = builder.header(
                    crate::sign_in::STAND_IN_GROUPS_HEADER,
                    HeaderValue::from_bytes(groups.as_bytes()).map_err(|_| ())?,
                );
            }
        }
    }
    let body = match body {
        Some(bytes) => {
            builder = builder.header(CONTENT_TYPE, "application/json");
            Full::new(bytes)
        }
        None => Full::new(Bytes::new()),
    };
    builder.body(body).map_err(|_| ())
}

/// The handshake, the request and its answer, on one connection that ends
/// with this call.
async fn exchange<S>(stream: S, request: Request<Full<Bytes>>) -> Result<Reply, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| failure(&e))?;
    let answer = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| failure(&e))?;
        let (parts, body) = response.into_parts();
        let body = Limited::new(body, MAX_ANSWER_BYTES)
            .collect()
            .await
            .map_err(|_| {
                Failure::Unexpected(format!(
                    "its answer was cut short or larger than {} MiB",
                    MAX_ANSWER_BYTES / (1024 * 1024)
                ))
            })?
            .to_bytes();
        let is_json = parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        let json = if is_json {
            serde_json::from_slice(&body).ok()
        } else {
            None
        };
        Ok(Reply {
            status: parts.status,
            json,
        })
    };
    tokio::pin!(connection);
    tokio::pin!(answer);
    let mut connection_over = false;
    loop {
        tokio::select! {
            answered = &mut answer => return answered,
            // The connection ending first ends the answer too, with its
            // own error; until then both are driven here.
            _ = &mut connection, if !connection_over => connection_over = true,
        }
    }
}

fn failure(error: &hyper::Error) -> Failure {
    if error.is_parse() || error.is_parse_status() || error.is_parse_too_large() {
        Failure::Unexpected("its answer is not HTTP the tool understands".into())
    } else {
        Failure::Unreachable(format!("the connection failed: {error}"))
    }
}
