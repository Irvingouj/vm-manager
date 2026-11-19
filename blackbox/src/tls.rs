//! TLS for the tests of "The tool reaches an installation over HTTPS"
//! (CONTRACT.md): the program itself still listens in plain HTTP on a
//! loopback address, so what stands for an installation (or an identity
//! provider) behind TLS is a front of the harness's own.
//!
//! - [`Authority`] is a test certificate authority, made per test with
//!   rcgen (ring's cryptography); it issues [`Identity`]s (a certificate and
//!   its key) for names, valid now or expired. Its private key never leaves
//!   memory and is never printed; only certificates are written to files.
//! - [`Front`] listens on `127.0.0.1:<free port>`, ends TLS with an
//!   [`Identity`] (or, [`Front::plain`], ends nothing), and passes each
//!   connection's bytes on to a target address unchanged: a byte tunnel
//!   after the handshake, which copes with long-lived answers (`vibe logs
//!   --follow`). It records each connection: the server name the client
//!   asked for, how the handshake went, and the bytes the client sent after
//!   it (decrypted; raw for a plain front), so a test can say how many
//!   connections came, which `Host` they named, and that nothing was sent
//!   when the handshake failed. Its identity can be replaced while it runs
//!   (an installation's certificate replaced). Dropping it ends it and
//!   every connection it holds.
//! - [`tls_get`] and [`tls_exchange`] are the harness's own TLS client, for
//!   the harness's own checks and for its browser's `https` addresses
//!   ([`crate::http::Request::over_tls`]).
//! - [`Files`] is a folder of a test's own for the PEM files it names in
//!   `VIBE_CA_FILE`.
//! - [`Behind`] is an installation behind a front: authority A, a
//!   certificate for `api.<domain>` and `*.<domain>` under it, the front
//!   before the program, and A's PEM in a file; [`provider_behind_tls`] is
//!   the harness's identity provider with an `https://localhost:<port>`
//!   issuer, every endpoint under it, behind a front of its own.
//! - Front request reading is per connection ([`requests`]): the bytes of
//!   two connections are never read as one stream.

use std::fmt;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener as StdListener, TcpStream as StdStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, StreamOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_rustls::LazyConfigAcceptor;

use crate::oidc::{Person, Provider};
use crate::shared_database::token;
use crate::vm::DOMAIN;

/// The installation's API name the tool asks for: `api.<domain>`.
pub fn api_name() -> String {
    format!("api.{DOMAIN}")
}

/// The identity provider's host name behind its front: it resolves to
/// `127.0.0.1` on this machine without any system file being touched.
pub const PROVIDER_HOST: &str = "localhost";

/// At most this many connections a front serves at once; more are closed
/// at once.
pub const MAX_CONNECTIONS: usize = 256;
/// At most this many connections a front records; later ones are counted.
pub const MAX_RECORDED: usize = 4096;
/// At most this many bytes a front keeps of what one connection's client
/// sent; the rest is counted, not kept.
pub const KEEP_PER_CONNECTION: usize = 16 * 1024 * 1024;
/// The longest a front waits for a handshake, and for its target to accept
/// a connection.
pub const FRONT_WAIT: Duration = Duration::from_secs(10);
/// How often the front's accepting loop looks whether it must stop.
const POLL: Duration = Duration::from_millis(25);

/// A test certificate authority. Its key is never printed.
pub struct Authority {
    name: String,
    certified: CertifiedIssuer<'static, KeyPair>,
}

impl fmt::Debug for Authority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Authority({:?})", self.name)
    }
}

impl Authority {
    /// A fresh authority whose certificate's common name is `name`.
    pub fn new(name: &str) -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new())
            .unwrap_or_else(|e| panic!("cannot make an authority's parameters: {e}"));
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, name);
        dn.push(DnType::OrganizationName, "vibe black-box harness");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate().unwrap_or_else(|e| panic!("cannot make a key: {e}"));
        let certified = CertifiedIssuer::self_signed(params, key)
            .unwrap_or_else(|e| panic!("cannot make the authority {name:?}: {e}"));
        Self {
            name: name.to_owned(),
            certified,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The authority's certificate in PEM form.
    pub fn pem(&self) -> String {
        self.certified.pem()
    }

    /// The authority's certificate in DER form.
    pub fn der(&self) -> CertificateDer<'static> {
        self.certified.der().clone()
    }

    /// A server certificate for these names (DNS names, `*.` wildcards
    /// allowed), valid now, issued under this authority.
    pub fn issue(&self, names: &[&str]) -> Identity {
        self.issue_with(names, Validity::Now)
    }

    /// As [`Authority::issue`], valid from 2020-01-01 to 2021-01-01: expired.
    pub fn issue_expired(&self, names: &[&str]) -> Identity {
        self.issue_with(names, Validity::Expired)
    }

    /// As [`Authority::issue`], valid from a day after now, for a year: not
    /// valid yet.
    pub fn issue_not_valid_yet(&self, names: &[&str]) -> Identity {
        self.issue_with(names, Validity::NotYet)
    }

    fn issue_with(&self, names: &[&str], validity: Validity) -> Identity {
        let owned: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let mut params = CertificateParams::new(owned.clone()).unwrap_or_else(|e| {
            panic!("cannot make a certificate's parameters for {names:?}: {e}")
        });
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, names.first().copied().unwrap_or("none"));
        params.distinguished_name = dn;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        match validity {
            Validity::Now => {}
            Validity::Expired => {
                params.not_before = date_time_ymd(2020, 1, 1);
                params.not_after = date_time_ymd(2021, 1, 1);
            }
            Validity::NotYet => {
                let since_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let day = Duration::from_secs(24 * 60 * 60);
                params.not_before = date_time_ymd(1970, 1, 1) + since_epoch + day;
                params.not_after = date_time_ymd(1970, 1, 1) + since_epoch + day * 366;
            }
        }
        let key = KeyPair::generate().unwrap_or_else(|e| panic!("cannot make a key: {e}"));
        let certificate = params
            .signed_by(&key, &self.certified)
            .unwrap_or_else(|e| panic!("cannot issue a certificate for {names:?}: {e}"));
        Identity {
            names: owned,
            authority: self.name.clone(),
            chain: vec![certificate.der().clone()],
            key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        }
    }
}

/// When a certificate [`Authority`] issues is valid.
#[derive(Clone, Copy, Debug)]
enum Validity {
    /// From long ago to far ahead.
    Now,
    /// 2020 only.
    Expired,
    /// From a day after now.
    NotYet,
}

/// A server certificate and its key. The key is never printed.
pub struct Identity {
    names: Vec<String>,
    authority: String,
    chain: Vec<CertificateDer<'static>>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Identity(names {:?}, under {:?}, key hidden)",
            self.names, self.authority
        )
    }
}

impl Identity {
    fn server_config(&self) -> Arc<ServerConfig> {
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap_or_else(|e| panic!("the harness's TLS versions: {e}"))
                .with_no_client_auth()
                .with_single_cert(
                    self.chain.clone(),
                    PrivateKeyDer::Pkcs8(self.key.clone_key()),
                )
                .unwrap_or_else(|e| panic!("the harness's server certificate {self:?}: {e}"));
        Arc::new(config)
    }
}

/// A PEM file holding a private key only (a fresh one, used nowhere): a
/// file with no certificate in it.
pub fn private_key_pem() -> String {
    KeyPair::generate()
        .unwrap_or_else(|e| panic!("cannot make a key: {e}"))
        .serialize_pem()
}

/// How a front's handshake with one client went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handshake {
    /// Not over yet.
    Pending,
    /// A plain front: no TLS.
    Plain,
    /// Done: the client accepted the certificate.
    Completed,
    /// Failed, with the front's own words for why.
    Failed(String),
}

/// What a front saw of one connection. Its `Debug` shows sizes only, never
/// the bytes (a credential may be among them).
#[derive(Clone)]
pub struct Seen {
    /// The server name (SNI) the client asked for, if it was a TLS client
    /// and asked for one.
    pub server_name: Option<String>,
    pub handshake: Handshake,
    /// What the client sent after the handshake (decrypted), or, on a plain
    /// front, everything it sent; the first [`KEEP_PER_CONNECTION`] bytes.
    pub received: Vec<u8>,
    /// How many bytes it sent in all.
    pub received_bytes: usize,
    /// The first bytes the client sent, before or without any handshake
    /// (raw): up to 16.
    pub first_bytes: Vec<u8>,
}

impl fmt::Debug for Seen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Seen(server name {:?}, handshake {:?}, {} bytes received)",
            self.server_name, self.handshake, self.received_bytes
        )
    }
}

struct FrontState {
    config: Option<Arc<ServerConfig>>,
    seen: Vec<Seen>,
    not_recorded: usize,
}

struct FrontShared {
    state: Mutex<FrontState>,
    stop: AtomicBool,
}

impl FrontShared {
    fn lock(&self) -> MutexGuard<'_, FrontState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_seen(&self, index: Option<usize>, change: impl FnOnce(&mut Seen)) {
        if let Some(index) = index
            && let Some(seen) = self.lock().seen.get_mut(index)
        {
            change(seen);
        }
    }
}

/// A front's listening socket, bound and not yet serving: its port is known
/// before what it forwards to is (an identity provider whose issuer names
/// the front's port).
pub struct Bound {
    listener: StdListener,
    address: SocketAddr,
}

impl Bound {
    /// A socket on `127.0.0.1:<free port>`.
    pub fn new() -> Self {
        let listener = StdListener::bind("127.0.0.1:0")
            .unwrap_or_else(|e| panic!("a front cannot listen: {e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("a front has no address: {e}"));
        Self { listener, address }
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// Serves: ends TLS with `identity`, passes the bytes on to `target`.
    pub fn tls(self, identity: &Identity, target: SocketAddr) -> Front {
        Front::serve(self, Some(identity.server_config()), target)
    }

    /// Serves: passes every byte on to `target` as it came, no TLS.
    pub fn plain(self, target: SocketAddr) -> Front {
        Front::serve(self, None, target)
    }
}

impl Default for Bound {
    fn default() -> Self {
        Self::new()
    }
}

/// A TLS (or plain) front before a target address. Dropping it ends it.
pub struct Front {
    address: SocketAddr,
    shared: Arc<FrontShared>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Front {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Front({})", self.address)
    }
}

impl Front {
    /// A front on a free port that ends TLS with `identity` before `target`.
    pub fn tls(identity: &Identity, target: SocketAddr) -> Self {
        Bound::new().tls(identity, target)
    }

    /// A front on a free port that passes bytes to `target` as they came.
    pub fn plain(target: SocketAddr) -> Self {
        Bound::new().plain(target)
    }

    fn serve(bound: Bound, config: Option<Arc<ServerConfig>>, target: SocketAddr) -> Self {
        let Bound { listener, address } = bound;
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|e| panic!("a front cannot listen: {e}"));
        let shared = Arc::new(FrontShared {
            state: Mutex::new(FrontState {
                config,
                seen: Vec::new(),
                not_recorded: 0,
            }),
            stop: AtomicBool::new(false),
        });
        let serving = Arc::clone(&shared);
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .unwrap_or_else(|e| panic!("a front cannot make its runtime: {e}"));
            runtime.block_on(accept_loop(listener, serving, target));
        });
        Self {
            address,
            shared,
            thread: Some(thread),
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// From the next connection on, ends TLS with this identity: the
    /// installation's certificate replaced.
    pub fn replace_identity(&self, identity: &Identity) {
        self.shared.lock().config = Some(identity.server_config());
    }

    /// Every connection seen so far, in the order they came. Fails if more
    /// came than could be recorded.
    pub fn connections(&self) -> Vec<Seen> {
        let state = self.shared.lock();
        assert!(
            state.not_recorded == 0,
            "the front got more than {MAX_RECORDED} connections and recorded only the first"
        );
        state.seen.clone()
    }

    /// Waits (at most [`FRONT_WAIT`]) until no handshake is pending, then
    /// gives every connection seen.
    pub fn settled(&self) -> Vec<Seen> {
        let deadline = Instant::now() + FRONT_WAIT;
        loop {
            let seen = self.connections();
            if seen.iter().all(|s| s.handshake != Handshake::Pending) || Instant::now() >= deadline
            {
                return seen;
            }
            thread::sleep(POLL);
        }
    }

    /// How many connections came.
    pub fn accepted(&self) -> usize {
        self.connections().len()
    }

    /// How many handshakes completed.
    pub fn completed(&self) -> usize {
        self.connections()
            .iter()
            .filter(|s| s.handshake == Handshake::Completed)
            .count()
    }

    /// The requests the clients sent, read connection by connection (a
    /// connection may carry several, one after another), in order.
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.connections()
            .iter()
            .flat_map(|s| requests(&s.received))
            .collect()
    }

    /// The request lines (`GET /v1/me HTTP/1.1`) of the requests the clients
    /// sent, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.line).collect()
    }

    /// The values of the `Host` headers of the requests the clients sent, in
    /// order.
    pub fn hosts(&self) -> Vec<String> {
        self.requests()
            .iter()
            .flat_map(|r| r.header_values("host"))
            .collect()
    }

    /// What the front saw, for a failure message: each connection's server
    /// name, handshake and request lines, credentials masked.
    pub fn describe(&self) -> String {
        let mut out = format!("the front on {} saw:\n", self.address);
        for (at, seen) in self.connections().iter().enumerate() {
            out.push_str(&format!(
                "  #{at}: server name {:?}, handshake {:?}, {} bytes; requests {:?}\n",
                seen.server_name,
                seen.handshake,
                seen.received_bytes,
                request_lines(&seen.received)
            ));
        }
        crate::connection::masked(&out, &[])
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

async fn accept_loop(listener: StdListener, shared: Arc<FrontShared>, target: SocketAddr) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(e) => panic!("a front cannot listen: {e}"),
    };
    let mut tasks = JoinSet::new();
    while !shared.stop.load(Ordering::SeqCst) {
        while tasks.try_join_next().is_some() {}
        let Ok(Ok((stream, _))) = tokio::time::timeout(POLL, listener.accept()).await else {
            continue;
        };
        if tasks.len() >= MAX_CONNECTIONS {
            drop(stream);
            continue;
        }
        let index = {
            let mut state = shared.lock();
            if state.seen.len() < MAX_RECORDED {
                state.seen.push(Seen {
                    server_name: None,
                    handshake: Handshake::Pending,
                    received: Vec::new(),
                    received_bytes: 0,
                    first_bytes: Vec::new(),
                });
                Some(state.seen.len() - 1)
            } else {
                state.not_recorded += 1;
                None
            }
        };
        tasks.spawn(serve_connection(stream, index, Arc::clone(&shared), target));
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn serve_connection(
    client: TcpStream,
    index: Option<usize>,
    shared: Arc<FrontShared>,
    target: SocketAddr,
) {
    // The first bytes as they came, before any handshake: whether the
    // client began with TLS or with plain HTTP.
    let mut first = [0_u8; 16];
    let peeked = tokio::time::timeout(FRONT_WAIT, client.peek(&mut first)).await;
    if let Ok(Ok(n)) = peeked {
        let bytes = first.get(..n).unwrap_or_default().to_vec();
        shared.with_seen(index, |s| s.first_bytes = bytes);
    }
    let config = shared.lock().config.clone();
    let Some(config) = config else {
        shared.with_seen(index, |s| s.handshake = Handshake::Plain);
        if let Some(upstream) = connect(target).await {
            pump(client, upstream, index, &shared).await;
        }
        return;
    };
    let acceptor = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), client);
    let start = match tokio::time::timeout(FRONT_WAIT, acceptor).await {
        Ok(Ok(start)) => start,
        Ok(Err(e)) => {
            let why = format!("no ClientHello: {e}");
            shared.with_seen(index, |s| s.handshake = Handshake::Failed(why));
            return;
        }
        Err(_) => {
            shared.with_seen(index, |s| {
                s.handshake = Handshake::Failed("no ClientHello in time".to_owned());
            });
            return;
        }
    };
    let name = start.client_hello().server_name().map(str::to_owned);
    shared.with_seen(index, |s| s.server_name = name);
    let tls = match tokio::time::timeout(FRONT_WAIT, start.into_stream(config)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(e)) => {
            let why = e.to_string();
            shared.with_seen(index, |s| s.handshake = Handshake::Failed(why));
            return;
        }
        Err(_) => {
            shared.with_seen(index, |s| {
                s.handshake = Handshake::Failed("the handshake did not end in time".to_owned());
            });
            return;
        }
    };
    shared.with_seen(index, |s| s.handshake = Handshake::Completed);
    if let Some(upstream) = connect(target).await {
        pump(tls, upstream, index, &shared).await;
    }
}

async fn connect(target: SocketAddr) -> Option<TcpStream> {
    match tokio::time::timeout(FRONT_WAIT, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => Some(stream),
        _ => None,
    }
}

/// Copies both ways until both sides have ended, recording what the client
/// sent.
async fn pump<C>(client: C, upstream: TcpStream, index: Option<usize>, shared: &FrontShared)
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = upstream.into_split();
    let up = async {
        let mut buffer = vec![0_u8; 16 * 1024];
        loop {
            let n = match client_read.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = buffer.get(..n).unwrap_or_default();
            shared.with_seen(index, |s| {
                s.received_bytes += n;
                let room = KEEP_PER_CONNECTION.saturating_sub(s.received.len());
                s.received
                    .extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
            });
            if upstream_write.write_all(chunk).await.is_err() {
                break;
            }
        }
        drop(upstream_write.shutdown().await);
    };
    let down = async {
        drop(tokio::io::copy(&mut upstream_read, &mut client_write).await);
        drop(client_write.shutdown().await);
    };
    tokio::join!(up, down);
}

/// One HTTP/1 request as a front saw it: its request line and its header
/// lines (the body is skipped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenRequest {
    pub line: String,
    pub headers: Vec<(String, String)>,
}

impl SeenRequest {
    /// The values of the headers of this name (any letter case).
    pub fn header_values(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .collect()
    }
}

/// Whether `line` is an HTTP/1 request line: a method in capitals, a target
/// and `HTTP/1.x`.
fn is_request_line(line: &str) -> bool {
    let parts: Vec<&str> = line.split(' ').collect();
    parts.len() == 3
        && !parts[0].is_empty()
        && parts[0].chars().all(|c| c.is_ascii_uppercase())
        && parts[2].starts_with("HTTP/1.")
}

/// The position just after the first `\r\n` at or after `from`.
fn after_line(bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|at| from + at + 2)
}

/// Where a chunked body that begins at `from` ends, if all of it is here.
fn chunked_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    loop {
        let line_end = after_line(bytes, at)?;
        let size_text = String::from_utf8_lossy(bytes.get(at..line_end - 2)?).into_owned();
        let size_text = size_text
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let size = usize::from_str_radix(&size_text, 16).ok()?;
        if size == 0 {
            // Trailers, then an empty line.
            let mut trailer = line_end;
            loop {
                let end = after_line(bytes, trailer)?;
                if end == trailer + 2 {
                    return Some(end);
                }
                trailer = end;
            }
        }
        at = line_end.checked_add(size)?.checked_add(2)?;
        if at > bytes.len() {
            return None;
        }
    }
}

/// The HTTP/1 requests in what one connection's client sent, one after
/// another: each head read up to its empty line, its body skipped by
/// `Content-Length` or chunked framing. Reading stops at the first thing
/// that is not a whole request (bytes that are no HTTP, say a TLS
/// handshake, give none).
pub fn requests(bytes: &[u8]) -> Vec<SeenRequest> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(head_len) = bytes
            .get(at..)
            .and_then(|rest| rest.windows(4).position(|w| w == b"\r\n\r\n"))
        else {
            break;
        };
        let head =
            String::from_utf8_lossy(bytes.get(at..at + head_len).unwrap_or_default()).into_owned();
        let mut lines = head.split("\r\n");
        let line = lines.next().unwrap_or_default().to_owned();
        if !is_request_line(&line) {
            break;
        }
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
            .collect();
        let request = SeenRequest { line, headers };
        let body_at = at + head_len + 4;
        let chunked = request
            .header_values("transfer-encoding")
            .iter()
            .any(|v| v.to_ascii_lowercase().contains("chunked"));
        let end = if chunked {
            chunked_end(bytes, body_at)
        } else {
            let length = request
                .header_values("content-length")
                .first()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            body_at.checked_add(length)
        };
        found.push(request);
        match end {
            Some(end) if end <= bytes.len() => at = end,
            _ => break,
        }
    }
    found
}

/// The request lines of the requests in what one connection's client sent.
pub fn request_lines(bytes: &[u8]) -> Vec<String> {
    requests(bytes).into_iter().map(|r| r.line).collect()
}

/// The harness's own TLS client: connects to `address`, asks for
/// `server_name`, trusts only these authorities, sends `GET <target>` with
/// `Host: <host>` and `Connection: close`, and returns what came back.
/// `Err` with the handshake's words when it fails.
pub fn tls_get(
    address: SocketAddr,
    server_name: &str,
    trusted: &[&Authority],
    target: &str,
    host: &str,
) -> Result<String, String> {
    let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let roots: Vec<CertificateDer<'static>> = trusted.iter().map(|a| a.der()).collect();
    let answer = tls_exchange(address, server_name, &roots, request.as_bytes(), FRONT_WAIT)?;
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

/// The harness's own TLS client at its plainest: connects to `address`
/// (within `timeout`), asks for `server_name`, trusts only `roots`, sends
/// `wire` as it is, and reads until the server closes (each read within
/// `timeout`). `Err` with the handshake's words when it fails.
pub fn tls_exchange(
    address: SocketAddr,
    server_name: &str,
    roots: &[CertificateDer<'static>],
    wire: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut store = RootCertStore::empty();
    for root in roots {
        store
            .add(root.clone())
            .map_err(|e| format!("cannot trust a root: {e}"))?;
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("the harness's TLS versions: {e}"))?
            .with_root_certificates(store)
            .with_no_client_auth();
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| format!("not a server name {server_name:?}: {e}"))?;
    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("cannot begin TLS: {e}"))?;
    let socket = StdStream::connect_timeout(&address, timeout)
        .map_err(|e| format!("cannot connect to {address}: {e}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .and_then(|()| socket.set_write_timeout(Some(timeout)))
        .map_err(|e| format!("cannot set timeouts: {e}"))?;
    let mut stream = StreamOwned::new(connection, socket);
    stream
        .write_all(wire)
        .and_then(|()| stream.flush())
        .map_err(|e| format!("TLS: {e}"))?;
    let mut answer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => answer.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            // A server that closes without TLS's own goodbye, after it
            // answered: the answer stands.
            Err(e)
                if !answer.is_empty()
                    && matches!(
                        e.kind(),
                        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                    ) =>
            {
                break;
            }
            Err(e) => return Err(format!("TLS: {e}")),
        }
    }
    Ok(answer)
}

/// A folder of a test's own for PEM files (certificates; a private key
/// only for the test of a file that holds none). Removed when dropped.
pub struct Files {
    dir: PathBuf,
}

impl Files {
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("vm-blackbox-tls-{}", token()));
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `contents` to `name` in the folder; returns its path.
    pub fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.join(name);
        fs::write(&path, contents)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        path
    }

    /// A file holding these authorities' certificates, one after another.
    pub fn authorities(&self, name: &str, authorities: &[&Authority]) -> PathBuf {
        let pem: String = authorities.iter().map(|a| a.pem()).collect();
        self.write(name, pem.as_bytes())
    }
}

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.dir));
    }
}

/// The path as text, for an environment variable or a message.
pub fn text_of(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("not UTF-8: {path:?}"))
        .to_owned()
}

/// An installation behind TLS: authority A, a certificate for
/// `api.<domain>` and `*.<domain>` under it, a front with it before the
/// program's address, and A's certificate in a file for `VIBE_CA_FILE`.
pub struct Behind {
    // The front first, so it ends before the files go.
    pub front: Front,
    pub authority: Authority,
    pub files: Files,
    ca_file: PathBuf,
}

impl Behind {
    /// Before the program listening at `target`.
    pub fn before(target: SocketAddr) -> Self {
        let authority = Authority::new("Harness authority A");
        let identity = authority.issue(&[&api_name(), &format!("*.{DOMAIN}")]);
        Self::with_identity(authority, &identity, target)
    }

    /// Before `target`, with this identity (one of `authority`'s, or not),
    /// `authority`'s certificate in the file.
    pub fn with_identity(authority: Authority, identity: &Identity, target: SocketAddr) -> Self {
        let files = Files::new();
        let ca_file = files.authorities("authority-a.pem", &[&authority]);
        Self {
            front: Front::tls(identity, target),
            authority,
            files,
            ca_file,
        }
    }

    pub fn port(&self) -> u16 {
        self.front.port()
    }

    /// The file holding authority A's certificate.
    pub fn ca_file(&self) -> &Path {
        &self.ca_file
    }

    /// `VIBE_CA_FILE` naming A's file.
    pub fn ca_env(&self) -> (&'static str, String) {
        ("VIBE_CA_FILE", text_of(&self.ca_file))
    }

    /// `--domain <domain> --resolve 127.0.0.1 --port <the front's port>`.
    pub fn place(&self) -> Vec<String> {
        place_at(self.port())
    }
}

/// `--domain <domain> --resolve 127.0.0.1 --port <port>`.
pub fn place_at(port: u16) -> Vec<String> {
    vec![
        "--domain".to_owned(),
        DOMAIN.to_owned(),
        "--resolve".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
    ]
}

/// The harness's identity provider with the issuer
/// `https://localhost:<port>`, behind a TLS front on that port whose
/// certificate for `localhost` is issued under `authority`. Every endpoint
/// its configuration names is under the issuer, so `https` through the
/// front: a browser that is to reach its authorization endpoint trusts
/// `authority` ([`crate::browser::Browser::trust`]).
pub fn provider_behind_tls(people: &[Person], authority: &Authority) -> (Provider, Front) {
    provider_behind_tls_with(people, |names| authority.issue(names))
}

/// As [`provider_behind_tls`], the front's certificate made by `issue` from
/// the names it is for (`localhost`): one for another name, expired, or not
/// valid yet, say.
pub fn provider_behind_tls_with(
    people: &[Person],
    issue: impl FnOnce(&[&str]) -> Identity,
) -> (Provider, Front) {
    let bound = Bound::new();
    let issuer = format!("https://{PROVIDER_HOST}:{}", bound.port());
    let provider = Provider::start_with_issuer(people, &issuer);
    let identity = issue(&[PROVIDER_HOST]);
    let front = bound.tls(&identity, provider.address());
    (provider, front)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines_and_hosts_are_read_from_bytes() {
        let bytes = b"GET /v1/me HTTP/1.1\r\nHost: api.vm.test:4430\r\naccept: */*\r\n\r\nPOST /v1/x HTTP/1.1\r\nhost: api.vm.test:4430\r\n\r\n{}";
        assert_eq!(
            request_lines(bytes),
            vec!["GET /v1/me HTTP/1.1", "POST /v1/x HTTP/1.1"]
        );
        let hosts: Vec<String> = requests(bytes)
            .iter()
            .flat_map(|r| r.header_values("Host"))
            .collect();
        assert_eq!(hosts, vec!["api.vm.test:4430", "api.vm.test:4430"]);
        assert!(request_lines(b"\x16\x03\x01\x02\x00").is_empty());
    }

    #[test]
    fn requests_on_one_connection_are_read_past_bodies_without_a_line_end() {
        let bytes = b"PUT /v1/oidc HTTP/1.1\r\nhost: a\r\ncontent-length: 2\r\n\r\n{}GET /v1/me HTTP/1.1\r\nhost: b\r\n\r\nPOST /c HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\nDELETE /d HTTP/1.1\r\n\r\n";
        assert_eq!(
            request_lines(bytes),
            vec![
                "PUT /v1/oidc HTTP/1.1",
                "GET /v1/me HTTP/1.1",
                "POST /c HTTP/1.1",
                "DELETE /d HTTP/1.1"
            ]
        );
    }

    #[test]
    fn an_identity_does_not_print_its_key() {
        let authority = Authority::new("t");
        let identity = authority.issue(&["api.vm.test"]);
        let shown = format!("{identity:?} {authority:?}");
        assert!(shown.contains("key hidden"), "{shown}");
        assert!(!shown.contains("PRIVATE"), "{shown}");
    }
}
