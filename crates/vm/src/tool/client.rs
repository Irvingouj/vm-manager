//! The tool's HTTP client: one plain HTTP/1.1 request to the installation's
//! API at a loopback address (blackbox/CONTRACT.md, "The tool: Signing
//! in"), with `Host: api.<domain>`.
//!
//! Built on hyper's client connection, which the program already has for
//! serving, driven on the tool's own thread: the connection's future is
//! polled beside the request's, never spawned, so nothing outlives the
//! call. Every wait has an end: [`CONNECT_TIMEOUT`] to be let in,
//! [`ANSWER_TIMEOUT`] for the whole answer after that (or the end the
//! caller gives, with [`send_waiting`]); and the answer's body is read up
//! to [`MAX_ANSWER_BYTES`] (hyper bounds its head). The body sent is the
//! caller's, which bounds it.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
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

/// Where an installation is.
pub struct Installation<'a> {
    pub domain: &'a str,
    pub connect: SocketAddr,
}

/// How a request is signed in.
#[derive(Clone, Copy)]
pub enum Sign<'a> {
    /// With the tool credential.
    Credential(&'a Secret),
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
    let stream =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(installation.connect)).await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return Err(Failure::Unreachable(error.to_string())),
            Err(_) => {
                return Err(Failure::Unreachable(format!(
                    "it did not accept the connection within {} s",
                    CONNECT_TIMEOUT.as_secs()
                )));
            }
        };
    match tokio::time::timeout(answer_timeout, exchange(stream, request)).await {
        Ok(answered) => answered,
        Err(_) => Err(Failure::Unreachable(format!(
            "it did not answer within {} s",
            answer_timeout.as_secs()
        ))),
    }
}

fn request(
    installation: &Installation<'_>,
    method: Method,
    path: &str,
    sign: Sign<'_>,
    body: Option<Bytes>,
) -> Result<Request<Full<Bytes>>, ()> {
    let host = HeaderValue::try_from(format!("api.{}", installation.domain)).map_err(|_| ())?;
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, host);
    match sign {
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
async fn exchange(stream: TcpStream, request: Request<Full<Bytes>>) -> Result<Reply, Failure> {
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
