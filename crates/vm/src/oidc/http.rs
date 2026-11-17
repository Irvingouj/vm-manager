//! Calls to the identity provider: HTTP/1.1 through hyper's client
//! connection, over TLS (rustls with ring, Mozilla's roots compiled in) for
//! `https`, plain for a loopback `http` issuer (crate::oidc::settings).
//!
//! **Bounds.** Every call, from resolving the name to the last byte of the
//! answer, ends within [`CALL_TIMEOUT`], and by the caller's deadline when
//! it gives one (a renewal's 10 seconds, all its calls together); the
//! answer's body is read up to
//! [`MAX_BODY_BYTES`], and one that is larger is no answer. No redirect is
//! followed. The connection is driven inside the call's own future, so
//! nothing is left running when the call ends, however it ends.
//!
//! What is sent may hold secrets (the client secret, a code, a PKCE
//! verifier, a refresh token); none of it is ever in an error, which says
//! only what kind of failure it was.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue, USER_AGENT};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::Instant;
use url::Url;

/// The longest one call to the identity provider may take, all of it.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of an answer's body read.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Why a call brought no answer that can be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// It could not be connected to, or the connection failed.
    Unreachable,
    /// It did not answer within [`CALL_TIMEOUT`].
    TimedOut,
    /// Its answer is larger than [`MAX_BODY_BYTES`].
    TooLarge,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::TimedOut => "timed-out",
            Self::TooLarge => "too-large",
        }
    }
}

/// An answer: its status and its body.
pub struct Answer {
    pub status: u16,
    pub body: Bytes,
}

/// A client for the identity provider's addresses.
pub struct Client {
    tls: Arc<rustls::ClientConfig>,
}

impl Client {
    /// The error is a message for the operator.
    pub fn new() -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS for the identity provider cannot be set up ({e})"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    /// `GET url`, asking for JSON; ended by `deadline` too when given.
    pub async fn get(&self, url: &Url, deadline: Option<Instant>) -> Result<Answer, Failure> {
        self.call(Method::GET, url, None, None, deadline).await
    }

    /// `POST url` with a form body, and `authorization` when given; ended
    /// by `deadline` too when given.
    pub async fn post_form(
        &self,
        url: &Url,
        form: String,
        authorization: Option<HeaderValue>,
        deadline: Option<Instant>,
    ) -> Result<Answer, Failure> {
        self.call(Method::POST, url, Some(form), authorization, deadline)
            .await
    }

    async fn call(
        &self,
        method: Method,
        url: &Url,
        form: Option<String>,
        authorization: Option<HeaderValue>,
        deadline: Option<Instant>,
    ) -> Result<Answer, Failure> {
        let own = Instant::now() + CALL_TIMEOUT;
        let ends = deadline.map_or(own, |deadline| deadline.min(own));
        tokio::time::timeout_at(ends, self.exchange(method, url, form, authorization))
            .await
            .map_err(|_| Failure::TimedOut)?
    }

    async fn exchange(
        &self,
        method: Method,
        url: &Url,
        form: Option<String>,
        authorization: Option<HeaderValue>,
    ) -> Result<Answer, Failure> {
        let host = url.host_str().ok_or(Failure::Unreachable)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let tls = url.scheme() == "https";
        let port = url.port_or_known_default().ok_or(Failure::Unreachable)?;
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|_| Failure::Unreachable)?;
        let target = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let authority = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(target)
            .header(HOST, authority)
            .header(USER_AGENT, "vm")
            .header(ACCEPT, "application/json");
        if form.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        if let Some(mut authorization) = authorization {
            authorization.set_sensitive(true);
            builder = builder.header(AUTHORIZATION, authorization);
        }
        let request = builder
            .body(Full::new(Bytes::from(form.unwrap_or_default())))
            .map_err(|_| Failure::Unreachable)?;
        if tls {
            let name = rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(|_| Failure::Unreachable)?;
            let stream = tokio_rustls::TlsConnector::from(Arc::clone(&self.tls))
                .connect(name, stream)
                .await
                .map_err(|_| Failure::Unreachable)?;
            send(TokioIo::new(stream), request).await
        } else {
            send(TokioIo::new(stream), request).await
        }
    }
}

/// The request on one connection, and its answer read whole.
async fn send<T>(io: TokioIo<T>, request: Request<Full<Bytes>>) -> Result<Answer, Failure>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|_| Failure::Unreachable)?;
    let answer = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| Failure::Unreachable)?;
        let status = response.status().as_u16();
        let body = Limited::new(response.into_body(), MAX_BODY_BYTES)
            .collect()
            .await
            .map_err(|error| {
                if error.is::<http_body_util::LengthLimitError>() {
                    Failure::TooLarge
                } else {
                    Failure::Unreachable
                }
            })?
            .to_bytes();
        Ok(Answer { status, body })
    };
    // The connection is driven here, beside the answer, and dropped with
    // this future: nothing is spawned.
    tokio::pin!(connection);
    tokio::pin!(answer);
    let mut connection_over = false;
    loop {
        tokio::select! {
            got = &mut answer => return got,
            _ = &mut connection, if !connection_over => connection_over = true,
        }
    }
}
