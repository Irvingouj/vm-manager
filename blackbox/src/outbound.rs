//! What the tests of outbound calls share (CONTRACT.md, "Outbound calls";
//! ADR 0020): an HTTPS/1.1 server of the harness's own that a test scripts
//! and that records what reached it, and a program started the way the
//! contract gives for a test to call anything.
//!
//! Every address of a test machine is loopback, which the platform never
//! calls. The way the contract gives: start `vibe serve` with
//! `--outbound-private 127.0.0.1/32` and `--outbound-private ::1/128`,
//! trust a test authority with `VIBE_CA_FILE`, and list `localhost:<port>`.
//!
//! - [`Origin`] listens on a port of the loopback addresses, ends TLS with a
//!   [`Cert`] (or speaks plain HTTP, [`Wire::Plain`]), and answers each
//!   request as its script says ([`Reply`]): whole, in timed pieces, cut
//!   short, never ending, a redirect, nothing at all. It records, for each
//!   connection and each request, what arrived ([`Seen`], [`ConnSeen`]): the
//!   method, the target, every header as sent, the body's length, SHA-256
//!   and framing, whether the other end went away and when.
//! - [`Outbound`] is the program under test: started with the options of
//!   the contract's way, the fixture Application `outbound`
//!   (`fixtures/outbound.js`) installed, the list changed through its API
//!   as a Platform administrator, and its calls read back from the
//!   Operation log.
//!
//! The server is made of threads and blocking sockets; every wait has a
//! timeout, and dropping it ends every thread it started.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::admin::administrator;
use crate::body::{CHUNK, pattern_block};
use crate::http::{Response, User};
use crate::tls::{Authority, Files, Identity};
use crate::vm::{START_TIMEOUT, Vm, VmOptions, fixture_bundle};

/// The fixture Application of these tests, and its name.
pub const APP: &str = "outbound";

/// The User who calls through it.
pub fn bob() -> User {
    User::new("bob")
}

/// How long a harness wait is, at most, for something the program does at
/// once.
pub const SOON: Duration = Duration::from_secs(10);

/// The bytes the origin keeps of a request's body, at most: the first ones.
const KEPT_BODY: usize = 1024 * 1024;

/// The longest a request's head may be, at most.
const MAX_HEAD: usize = 4 * 1024 * 1024;

/// How long a connection waits for the next byte of a request (or the next
/// request) before it ends itself.
const IDLE: Duration = Duration::from_secs(60);

/// How the origin speaks.
#[derive(Clone)]
pub enum Wire {
    /// TLS with this certificate.
    Tls(Arc<ServerConfig>),
    /// No TLS: it reads what comes and answers a plain HTTP `200`.
    Plain,
    /// Accepts the connection and does nothing: no handshake, no byte.
    Silent,
}

/// A server certificate and its key, from a test [`Authority`].
pub struct Cert {
    chain: Vec<CertificateDer<'static>>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Cert {
    /// A certificate under `authority` for these DNS names and these
    /// addresses, valid now.
    pub fn for_names_and_addresses(authority: &Authority, names: &[&str], ips: &[IpAddr]) -> Self {
        let dns: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let mut params = CertificateParams::new(dns)
            .unwrap_or_else(|e| panic!("cannot make the parameters for {names:?}: {e}"));
        for ip in ips {
            params.subject_alt_names.push(SanType::IpAddress(*ip));
        }
        let mut dn = DistinguishedName::new();
        dn.push(
            DnType::CommonName,
            names.first().copied().unwrap_or("origin"),
        );
        params.distinguished_name = dn;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().unwrap_or_else(|e| panic!("cannot make a key: {e}"));
        let der = authority.sign(&params, &key);
        Self {
            chain: vec![der],
            key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        }
    }

    /// A certificate for `localhost` and the loopback addresses the tests
    /// call by address (`127.0.0.1`, `::1`, `127.0.0.2`).
    pub fn localhost(authority: &Authority) -> Self {
        Self::for_names_and_addresses(
            authority,
            &["localhost"],
            &[
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            ],
        )
    }

    /// A certificate of the harness's TLS module's making, read back.
    pub fn from_identity(identity: &Identity) -> Self {
        let pem = identity.key_pem();
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap_or_else(|e| panic!("cannot read the test key back: {e}"));
        Self {
            chain: identity.chain().to_vec(),
            key: PrivatePkcs8KeyDer::from(der),
        }
    }

    /// The wire of a server with this certificate.
    pub fn wire(&self) -> Wire {
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap_or_else(|e| panic!("the harness's TLS versions: {e}"))
                .with_no_client_auth()
                .with_single_cert(
                    self.chain.clone(),
                    PrivateKeyDer::Pkcs8(self.key.clone_key()),
                )
                .unwrap_or_else(|e| panic!("the harness's server certificate: {e}"));
        Wire::Tls(Arc::new(config))
    }
}

/// What the origin received of one request.
#[derive(Clone, Debug)]
pub struct Seen {
    /// The connection it came on (the order they were accepted in, from 0).
    pub connection: usize,
    /// Its place on that connection, from 0.
    pub nth: usize,
    pub method: String,
    /// The path and the query, as sent.
    pub target: String,
    /// The protocol version of the request line (`HTTP/1.1`).
    pub version: String,
    /// Every header as sent, in order.
    pub headers: Vec<(String, String)>,
    /// A `content-length` the request stated.
    pub content_length: Option<u64>,
    /// The request's body was sent chunked.
    pub chunked: bool,
    /// How many body bytes arrived.
    pub body_len: u64,
    /// The SHA-256 of the body's bytes, in lower-case hex.
    pub body_sha256: String,
    /// The first bytes of the body (at most 1 MiB).
    pub body_head: Vec<u8>,
    /// The body ended as its framing says (not cut by the other end).
    pub body_complete: bool,
    /// The server name the client asked for in the handshake.
    pub sni: Option<String>,
    pub at: Instant,
}

impl Seen {
    /// The first value of this header, by name in any letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of this header.
    pub fn header_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The names of the headers, in lower case, sorted, once each.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .headers
            .iter()
            .map(|(n, _)| n.to_ascii_lowercase())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The path without the query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    /// The body as text (its kept head).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body_head).into_owned()
    }

    /// A line for a failure message.
    pub fn describe(&self) -> String {
        format!(
            "{} {} headers {:?}, body {} bytes (chunked {}, content-length {:?}, sha256 {})",
            self.method,
            self.target,
            self.headers,
            self.body_len,
            self.chunked,
            self.content_length,
            self.body_sha256
        )
    }
}

/// What the origin saw of one connection.
#[derive(Clone, Debug)]
pub struct ConnSeen {
    pub id: usize,
    pub opened_at: Instant,
    /// The server name of the handshake, when the client gave one.
    pub sni: Option<String>,
    /// The handshake completed.
    pub handshake_done: bool,
    /// How many requests came on it.
    pub requests: usize,
    /// When the origin saw the other end go away (a close or a reset).
    pub peer_closed_at: Option<Instant>,
    /// The TLS version negotiated (`TLSv1_2`, `TLSv1_3`).
    pub tls_version: Option<String>,
    /// Bytes read on a [`Wire::Plain`] connection.
    pub plain_bytes: usize,
}

/// A step of an answer, after its head.
#[derive(Clone, Debug)]
pub enum Step {
    /// These bytes as they are.
    Send(Vec<u8>),
    /// One chunk of a chunked body (framing added).
    Chunk(Vec<u8>),
    /// The end of a chunked body.
    End,
    /// This many bytes of the pattern ([`pattern_block`] of 64 KiB repeated).
    Pattern(u64),
    /// Nothing for this long (the other end going away ends the answer).
    Wait(Duration),
    /// Holds the connection open, sending nothing, until the other end goes
    /// away or this long has passed.
    Hold(Duration),
    /// Closes the connection at once.
    Close,
}

/// How an answer's body is framed.
#[derive(Clone, Copy, Debug)]
pub enum Frame {
    Length(u64),
    Chunked,
    /// `connection: close`, the body ends with the connection.
    UntilClose,
    /// No framing header at all.
    Bare,
}

/// What the origin answers.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub frame: Frame,
    /// Done before the head.
    pub before: Vec<Step>,
    /// Sent whole in place of the head, when given.
    pub raw_head: Option<Vec<u8>>,
    pub steps: Vec<Step>,
}

impl Reply {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            frame: Frame::Length(0),
            before: Vec::new(),
            raw_head: None,
            steps: Vec::new(),
        }
    }

    /// `200` with this body and its `content-length`.
    pub fn ok(body: &[u8]) -> Self {
        Self::new(200).body(body)
    }

    /// Adds a header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// This body, with its `content-length`.
    #[must_use]
    pub fn body(mut self, body: &[u8]) -> Self {
        self.frame = Frame::Length(u64::try_from(body.len()).unwrap_or(u64::MAX));
        self.steps = vec![Step::Send(body.to_vec())];
        self
    }

    /// These pieces as a chunked body, then its end.
    #[must_use]
    pub fn chunked(mut self, pieces: &[&[u8]]) -> Self {
        self.frame = Frame::Chunked;
        self.steps = pieces.iter().map(|p| Step::Chunk(p.to_vec())).collect();
        self.steps.push(Step::End);
        self
    }

    /// `len` bytes of the pattern with their `content-length`.
    #[must_use]
    pub fn pattern(mut self, len: u64) -> Self {
        self.frame = Frame::Length(len);
        self.steps = vec![Step::Pattern(len)];
        self
    }

    /// A body that states `declared` bytes, sends `sent` of them, and then
    /// closes the connection.
    #[must_use]
    pub fn cut_short(mut self, declared: u64, sent: &[u8]) -> Self {
        self.frame = Frame::Length(declared);
        self.steps = vec![Step::Send(sent.to_vec()), Step::Close];
        self
    }

    /// A body that states `declared` bytes, sends `sent` of them, and then
    /// holds the connection open for `hold` with no byte.
    #[must_use]
    pub fn stalls_after(mut self, declared: u64, sent: &[u8], hold: Duration) -> Self {
        self.frame = Frame::Length(declared);
        self.steps = vec![Step::Send(sent.to_vec()), Step::Hold(hold)];
        self
    }

    /// A chunked body that sends one chunk and then closes without its end.
    #[must_use]
    pub fn chunked_then_close(mut self, piece: &[u8]) -> Self {
        self.frame = Frame::Chunked;
        self.steps = vec![Step::Chunk(piece.to_vec()), Step::Close];
        self
    }

    /// A chunked body of these pieces, sent one by one; then, after `wait`
    /// (for the client to have them), the connection is closed without the
    /// body's end.
    #[must_use]
    pub fn chunked_pieces_then_close(mut self, pieces: &[&[u8]], wait: Duration) -> Self {
        self.frame = Frame::Chunked;
        self.steps = pieces.iter().map(|p| Step::Chunk(p.to_vec())).collect();
        if !wait.is_zero() {
            self.steps.push(Step::Wait(wait));
        }
        self.steps.push(Step::Close);
        self
    }

    /// Waits this long before the head.
    #[must_use]
    pub fn after(mut self, wait: Duration) -> Self {
        self.before.push(Step::Wait(wait));
        self
    }

    /// Never answers: holds the connection open, with no byte, for `hold`.
    pub fn never(hold: Duration) -> Self {
        let mut reply = Self::new(200);
        reply.before = vec![Step::Hold(hold)];
        reply
    }

    /// Closes the connection at once without a byte.
    pub fn closes() -> Self {
        let mut reply = Self::new(200);
        reply.before = vec![Step::Close];
        reply
    }

    /// These bytes as the head, and then the connection held open.
    pub fn raw(bytes: &[u8], hold: Duration) -> Self {
        let mut reply = Self::new(200);
        reply.raw_head = Some(bytes.to_vec());
        reply.steps = vec![Step::Hold(hold)];
        reply
    }

    /// A redirect with this status to this address.
    pub fn redirect(status: u16, location: &str) -> Self {
        Self::new(status).header("location", location)
    }

    /// `n` more headers, `x-n-<i>: v`.
    #[must_use]
    pub fn headers_numbered(mut self, n: usize) -> Self {
        for i in 0..n {
            self.headers.push((format!("x-n-{i}"), "v".to_owned()));
        }
        self
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",
            400 => "Bad Request",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Status",
        }
    }
}

type Script = dyn Fn(&Seen) -> Reply + Send + Sync;

struct Shared {
    wire: Wire,
    script: Mutex<Arc<Script>>,
    seen: Mutex<Vec<Seen>>,
    conns: Mutex<Vec<ConnSeen>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    stop: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scripted HTTPS/1.1 server on the loopback addresses. Dropping it ends
/// it and every connection it holds.
pub struct Origin {
    port: u16,
    shared: Arc<Shared>,
}

impl Origin {
    /// Listens on a free port of `127.0.0.1` and (where the machine has it)
    /// `::1`; answers `200 ok` until given a script.
    pub fn start(wire: Wire) -> Self {
        let port = crate::ports::chosen().unwrap_or_else(|e| panic!("no port for an origin: {e}"));
        let mut listeners = Vec::new();
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            if let Ok(l) = TcpListener::bind(SocketAddr::new(ip, port)) {
                listeners.push(l);
            }
        }
        assert!(
            !listeners.is_empty(),
            "cannot listen on 127.0.0.1:{port} for an origin"
        );
        Self::on(port, listeners, wire)
    }

    /// Listens on this address only, on a free port: to see that nothing
    /// reaches an address that is not opened.
    pub fn start_at(ip: IpAddr, wire: Wire) -> Self {
        let port = crate::ports::chosen().unwrap_or_else(|e| panic!("no port for an origin: {e}"));
        let listener = TcpListener::bind(SocketAddr::new(ip, port))
            .unwrap_or_else(|e| panic!("cannot listen on {ip}:{port}: {e}"));
        Self::on(port, vec![listener], wire)
    }

    fn on(port: u16, listeners: Vec<TcpListener>, wire: Wire) -> Self {
        let shared = Arc::new(Shared {
            wire,
            script: Mutex::new(Arc::new(|_: &Seen| Reply::ok(b"ok"))),
            seen: Mutex::new(Vec::new()),
            conns: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
        });
        for listener in listeners {
            listener
                .set_nonblocking(true)
                .unwrap_or_else(|e| panic!("cannot make the listener non-blocking: {e}"));
            let for_thread = Arc::clone(&shared);
            let handle = thread::spawn(move || accept_loop(&listener, &for_thread));
            locked(&shared.threads).push(handle);
        }
        Self { port, shared }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `localhost:<port>`: the entry of the list and the host of a call.
    pub fn host(&self) -> String {
        format!("localhost:{}", self.port)
    }

    /// `https://localhost:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.port)
    }

    /// Answers as `script` says from now on.
    pub fn script(&self, script: impl Fn(&Seen) -> Reply + Send + Sync + 'static) {
        *locked(&self.shared.script) = Arc::new(script);
    }

    /// Answers every request with this reply.
    pub fn always(&self, reply: Reply) {
        self.script(move |_| reply.clone());
    }

    /// Every request received so far, whole (their bodies read).
    pub fn requests(&self) -> Vec<Seen> {
        locked(&self.shared.seen).clone()
    }

    /// Waits until at least `n` requests were received, and returns them.
    pub fn wait_requests(&self, n: usize, within: Duration) -> Vec<Seen> {
        let deadline = Instant::now() + within;
        loop {
            let seen = self.requests();
            if seen.len() >= n {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "expected {n} request(s) at the origin within {within:?}, got {} ({:?})",
                seen.len(),
                seen.iter().map(Seen::describe).collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The only request received, after waiting for it.
    pub fn the_request(&self) -> Seen {
        let seen = self.wait_requests(1, SOON);
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one request at the origin, got {:?}",
            seen.iter().map(Seen::describe).collect::<Vec<_>>()
        );
        seen[0].clone()
    }

    /// Every connection accepted so far.
    pub fn connections(&self) -> Vec<ConnSeen> {
        locked(&self.shared.conns).clone()
    }

    /// How many connections were accepted.
    pub fn accepted(&self) -> usize {
        locked(&self.shared.conns).len()
    }

    /// The most requests that were at once between their head and the end
    /// of their answer.
    pub fn max_in_flight(&self) -> usize {
        self.shared.max_in_flight.load(Ordering::SeqCst)
    }

    /// Waits for a connection whose other end went away; how long after
    /// `since` it was seen, or `None` when none was within `within`.
    pub fn wait_peer_closed(&self, since: Instant, within: Duration) -> Option<Duration> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(at) = self
                .connections()
                .iter()
                .filter_map(|c| c.peer_closed_at)
                .min()
            {
                return Some(at.saturating_duration_since(since));
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Fails unless nothing at all connected, with what did in the message.
    pub fn assert_untouched(&self, context: &str) {
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            self.accepted(),
            0,
            "{context}: expected no connection at the origin, got {:?} and requests {:?}",
            self.connections(),
            self.requests()
                .iter()
                .map(Seen::describe)
                .collect::<Vec<_>>()
        );
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *locked(&self.shared.threads));
        for handle in handles {
            drop(handle.join());
        }
        // Connection threads started after the first take.
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *locked(&self.shared.threads));
        for handle in handles {
            drop(handle.join());
        }
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let id = {
                    let mut conns = locked(&shared.conns);
                    let next = conns.len();
                    conns.push(ConnSeen {
                        id: next,
                        opened_at: Instant::now(),
                        sni: None,
                        tls_version: None,
                        handshake_done: false,
                        requests: 0,
                        peer_closed_at: None,
                        plain_bytes: 0,
                    });
                    conns.len() - 1
                };
                let for_thread = Arc::clone(shared);
                let handle = thread::spawn(move || serve_connection(stream, id, &for_thread));
                locked(&shared.threads).push(handle);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// How a connection ended for the origin.
#[derive(Debug)]
enum End {
    /// The other end went away.
    Peer,
    /// The origin is being dropped, or nothing came for too long.
    Stopped,
}

trait Duplex: Read + Write {
    /// The server name the client asked for, once the handshake is done.
    fn sni(&self) -> Option<String> {
        None
    }

    /// The TLS version, once the handshake is done.
    fn tls_version(&self) -> Option<String> {
        None
    }
}

impl Duplex for TcpStream {}

impl Duplex for StreamOwned<ServerConnection, TcpStream> {
    fn sni(&self) -> Option<String> {
        self.conn.server_name().map(str::to_owned)
    }

    fn tls_version(&self) -> Option<String> {
        self.conn.protocol_version().map(|v| format!("{v:?}"))
    }
}

struct Conn<'a> {
    stream: Box<dyn Duplex + 'a>,
    buf: Vec<u8>,
    id: usize,
    shared: &'a Arc<Shared>,
    tls: bool,
    version: String,
}

impl Conn<'_> {
    fn note(&self, change: impl FnOnce(&mut ConnSeen)) {
        if let Some(c) = locked(&self.shared.conns).get_mut(self.id) {
            change(c);
        }
    }

    fn peer_gone(&self) -> End {
        self.note(|c| {
            if c.peer_closed_at.is_none() {
                c.peer_closed_at = Some(Instant::now());
            }
        });
        End::Peer
    }

    /// Reads what is there into the buffer, waiting 100 ms at most for it.
    /// `Ok(0)`: nothing yet. `Err`: the end.
    fn fill(&mut self) -> Result<usize, End> {
        if self.shared.stop.load(Ordering::SeqCst) {
            return Err(End::Stopped);
        }
        let mut chunk = vec![0_u8; 64 * 1024];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(self.peer_gone()),
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(n)
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(0),
            Err(_) => Err(self.peer_gone()),
        }
    }

    /// Reads until the buffer holds `\r\n\r\n`; the head's text (the buffer
    /// keeps what follows).
    fn read_head(&mut self, idle: Duration) -> Result<String, End> {
        let mut last = Instant::now();
        loop {
            if let Some(at) = find(&self.buf, b"\r\n\r\n") {
                let head: Vec<u8> = self.buf.drain(..at + 4).collect();
                return Ok(String::from_utf8_lossy(&head).into_owned());
            }
            if self.buf.len() > MAX_HEAD {
                return Err(End::Stopped);
            }
            if self.fill()? > 0 {
                last = Instant::now();
            } else if last.elapsed() > idle {
                return Err(End::Stopped);
            }
        }
    }

    fn read_line(&mut self) -> Result<String, End> {
        let mut last = Instant::now();
        loop {
            if let Some(at) = find(&self.buf, b"\r\n") {
                let line: Vec<u8> = self.buf.drain(..at + 2).collect();
                return Ok(String::from_utf8_lossy(&line[..at]).into_owned());
            }
            if self.fill()? > 0 {
                last = Instant::now();
            } else if last.elapsed() > IDLE {
                return Err(End::Stopped);
            }
        }
    }

    /// Takes `n` body bytes into the sink.
    fn take_body(&mut self, mut n: u64, sink: &mut BodySink) -> Result<(), End> {
        let mut last = Instant::now();
        while n > 0 {
            if self.buf.is_empty() {
                if self.fill()? > 0 {
                    last = Instant::now();
                } else if last.elapsed() > IDLE {
                    return Err(End::Stopped);
                }
                continue;
            }
            let now = usize::try_from(n).unwrap_or(usize::MAX).min(self.buf.len());
            let bytes: Vec<u8> = self.buf.drain(..now).collect();
            sink.take(&bytes);
            n -= u64::try_from(now).unwrap_or(0);
        }
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), End> {
        match self
            .stream
            .write_all(bytes)
            .and_then(|()| self.stream.flush())
        {
            Ok(()) => Ok(()),
            Err(_) => Err(self.peer_gone()),
        }
    }

    /// Waits `for_how_long`, noticing the other end going away.
    fn wait(&mut self, for_how_long: Duration, stop_when_gone: bool) -> Result<(), End> {
        let until = Instant::now() + for_how_long;
        while Instant::now() < until {
            match self.fill() {
                Ok(_) => {}
                Err(e) => {
                    return if stop_when_gone { Err(e) } else { Ok(()) };
                }
            }
        }
        Ok(())
    }
}

struct BodySink {
    sha: Sha256,
    len: u64,
    head: Vec<u8>,
}

impl BodySink {
    fn take(&mut self, bytes: &[u8]) {
        self.sha.update(bytes);
        self.len += u64::try_from(bytes.len()).unwrap_or(0);
        let room = KEPT_BODY.saturating_sub(self.head.len());
        self.head.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn serve_connection(tcp: TcpStream, id: usize, shared: &Arc<Shared>) {
    drop(tcp.set_nodelay(true));
    drop(tcp.set_nonblocking(false));
    drop(tcp.set_read_timeout(Some(Duration::from_millis(100))));
    drop(tcp.set_write_timeout(Some(Duration::from_secs(10))));
    match &shared.wire {
        Wire::Plain => serve_plain(tcp, id, shared, true),
        Wire::Silent => serve_plain(tcp, id, shared, false),
        Wire::Tls(config) => {
            let Ok(connection) = ServerConnection::new(Arc::clone(config)) else {
                return;
            };
            let stream = StreamOwned::new(connection, tcp);
            let mut conn = Conn {
                stream: Box::new(stream),
                buf: Vec::new(),
                id,
                shared,
                tls: true,
                version: String::new(),
            };
            drop(requests_on(&mut conn));
        }
    }
}

fn serve_plain(mut tcp: TcpStream, id: usize, shared: &Arc<Shared>, reply: bool) {
    let began = Instant::now();
    let mut chunk = vec![0_u8; 4096];
    let mut answered = false;
    while !shared.stop.load(Ordering::SeqCst) && began.elapsed() < Duration::from_secs(20) {
        match tcp.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(c) = locked(&shared.conns).get_mut(id) {
                    c.plain_bytes += n;
                }
                if reply && !answered {
                    answered = true;
                    drop(tcp.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\ncontent-type: text/plain\r\n\r\nplain",
                    ));
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    if let Some(c) = locked(&shared.conns).get_mut(id)
        && c.peer_closed_at.is_none()
    {
        c.peer_closed_at = Some(Instant::now());
    }
}

fn requests_on(conn: &mut Conn<'_>) -> Result<(), End> {
    let mut nth = 0;
    loop {
        let head = conn.read_head(IDLE)?;
        let sni = conn.stream.sni();
        let tls_version = conn.stream.tls_version();
        conn.version = head
            .split("\r\n")
            .next()
            .and_then(|l| l.split(' ').nth(2))
            .unwrap_or("")
            .to_owned();
        conn.note(|c| {
            c.tls_version.clone_from(&tls_version);
            c.handshake_done = true;
            c.sni.clone_from(&sni);
        });
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or("").to_owned();
        let target = parts.next().unwrap_or("").to_owned();
        let mut headers = Vec::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.to_owned(), value.trim().to_owned()));
            }
        }
        let find_header = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let content_length = find_header("content-length").and_then(|v| v.parse::<u64>().ok());
        let chunked = find_header("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let in_flight = conn.shared.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        conn.shared
            .max_in_flight
            .fetch_max(in_flight, Ordering::SeqCst);
        let finished = serve_request(
            conn,
            nth,
            &method,
            &target,
            &headers,
            content_length,
            chunked,
            sni,
        );
        conn.shared.in_flight.fetch_sub(1, Ordering::SeqCst);
        nth += 1;
        if !finished? {
            return Ok(());
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "test code: the parts of one request head"
)]
fn serve_request(
    conn: &mut Conn<'_>,
    nth: usize,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    content_length: Option<u64>,
    chunked: bool,
    sni: Option<String>,
) -> Result<bool, End> {
    let mut sink = BodySink {
        sha: Sha256::new(),
        len: 0,
        head: Vec::new(),
    };
    let mut complete = true;
    if chunked {
        loop {
            let line = match conn.read_line() {
                Ok(l) => l,
                Err(e) => {
                    complete = false;
                    record(
                        conn,
                        nth,
                        method,
                        target,
                        headers,
                        content_length,
                        chunked,
                        sink,
                        complete,
                        sni,
                    );
                    return Err(e);
                }
            };
            let size_text = line.split(';').next().unwrap_or("0").trim().to_owned();
            let size = u64::from_str_radix(&size_text, 16).unwrap_or(0);
            if size == 0 {
                // Trailers up to the blank line.
                while conn.read_line().is_ok_and(|l| !l.is_empty()) {}
                break;
            }
            if let Err(e) = conn.take_body(size, &mut sink) {
                record(
                    conn,
                    nth,
                    method,
                    target,
                    headers,
                    content_length,
                    chunked,
                    sink,
                    false,
                    sni,
                );
                return Err(e);
            }
            drop(conn.read_line());
        }
    } else if let Some(n) = content_length
        && let Err(e) = conn.take_body(n, &mut sink)
    {
        record(
            conn,
            nth,
            method,
            target,
            headers,
            content_length,
            chunked,
            sink,
            false,
            sni,
        );
        return Err(e);
    }
    let seen = record(
        conn,
        nth,
        method,
        target,
        headers,
        content_length,
        chunked,
        sink,
        complete,
        sni,
    );
    let script = Arc::clone(&locked(&conn.shared.script));
    let reply = script(&seen);
    answer(conn, &reply, method)
}

#[expect(
    clippy::too_many_arguments,
    reason = "test code: the parts of one request"
)]
fn record(
    conn: &Conn<'_>,
    nth: usize,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    content_length: Option<u64>,
    chunked: bool,
    sink: BodySink,
    complete: bool,
    sni: Option<String>,
) -> Seen {
    let sha: String = sink
        .sha
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let seen = Seen {
        connection: conn.id,
        nth,
        method: method.to_owned(),
        target: target.to_owned(),
        version: conn.version.clone(),
        headers: headers.to_vec(),
        content_length,
        chunked,
        body_len: sink.len,
        body_sha256: sha,
        body_head: sink.head,
        body_complete: complete,
        sni: sni.or_else(|| {
            locked(&conn.shared.conns)
                .get(conn.id)
                .and_then(|c| c.sni.clone())
        }),
        at: Instant::now(),
    };
    locked(&conn.shared.seen).push(seen.clone());
    conn.note(|c| c.requests += 1);
    let _ = conn.tls;
    seen
}

fn pattern_steps(conn: &mut Conn<'_>, mut len: u64) -> Result<(), End> {
    let block = pattern_block(CHUNK);
    let block_len = u64::try_from(block.len()).unwrap_or(1);
    while len > 0 {
        let now = len.min(block_len);
        let n = usize::try_from(now).unwrap_or(block.len());
        conn.send(&block[..n])?;
        len -= now;
    }
    Ok(())
}

/// Plays a reply; whether the connection goes on for another request.
fn answer(conn: &mut Conn<'_>, reply: &Reply, method: &str) -> Result<bool, End> {
    for step in &reply.before {
        if !play(conn, step)? {
            return Ok(false);
        }
    }
    if let Some(raw) = &reply.raw_head {
        conn.send(raw)?;
    } else {
        let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reply.reason());
        for (name, value) in &reply.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        match reply.frame {
            Frame::Length(n) => head.push_str(&format!("content-length: {n}\r\n")),
            Frame::Chunked => head.push_str("transfer-encoding: chunked\r\n"),
            Frame::UntilClose => head.push_str("connection: close\r\n"),
            Frame::Bare => {}
        }
        head.push_str("\r\n");
        conn.send(head.as_bytes())?;
    }
    if method == "HEAD" {
        return Ok(true);
    }
    for step in &reply.steps {
        if !play(conn, step)? {
            return Ok(false);
        }
    }
    Ok(!matches!(reply.frame, Frame::UntilClose))
}

/// One step; `false` when the connection is closed by it.
fn play(conn: &mut Conn<'_>, step: &Step) -> Result<bool, End> {
    match step {
        Step::Send(bytes) => conn.send(bytes)?,
        Step::Chunk(bytes) => {
            let mut wire = format!("{:x}\r\n", bytes.len()).into_bytes();
            wire.extend_from_slice(bytes);
            wire.extend_from_slice(b"\r\n");
            conn.send(&wire)?;
        }
        Step::End => conn.send(b"0\r\n\r\n")?,
        Step::Pattern(len) => pattern_steps(conn, *len)?,
        Step::Wait(d) => conn.wait(*d, true)?,
        Step::Hold(d) => conn.wait(*d, true)?,
        Step::Close => return Ok(false),
    }
    Ok(true)
}

/// SHA-256 of the first `len` bytes of the pattern, in hex: what a
/// [`Reply::pattern`] of that length carries.
pub fn pattern_sha256(len: u64) -> String {
    let block = pattern_block(CHUNK);
    let block_len = u64::try_from(block.len()).unwrap_or(1);
    let mut sha = Sha256::new();
    let mut left = len;
    while left > 0 {
        let now = left.min(block_len);
        sha.update(&block[..usize::try_from(now).unwrap_or(block.len())]);
        left -= now;
    }
    sha.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of these bytes, in hex.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ------------------------------------------------------------------ the program

/// The options of the contract's way to call anything from a test.
pub const OPEN_LOOPBACK: [&str; 4] = [
    "--outbound-private",
    "127.0.0.1/32",
    "--outbound-private",
    "::1/128",
];

/// `value` as a path segment: every byte but unreserved ones percent-encoded.
pub fn segment(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `PUT /v1/outbound-hosts/<entry>` on this program as a Platform
/// administrator, which must be `200`.
pub fn allow_on(vm: &Vm, entry: &str) -> Value {
    let answer = vm
        .api_as(&administrator())
        .request(&format!("/v1/outbound-hosts/{}", segment(entry)))
        .method("PUT")
        .send();
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` listing {entry:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// `DELETE /v1/outbound-hosts/<entry>` on this program as a Platform
/// administrator, which must be `200`.
pub fn disallow_on(vm: &Vm, entry: &str) -> Value {
    let answer = vm
        .api_as(&administrator())
        .request(&format!("/v1/outbound-hosts/{}", segment(entry)))
        .method("DELETE")
        .send();
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` removing {entry:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// What a program of these tests is started with: a test authority, the
/// file that names it in `VIBE_CA_FILE`, and the options (the stand-in, the
/// administrators' Group, the extra arguments). For a test that starts the
/// program some other way (with Storage, say).
pub struct Setup {
    pub options: VmOptions,
    pub authority: Authority,
    pub files: Files,
    pub ca_file: PathBuf,
}

impl Setup {
    /// With these extra arguments.
    pub fn new(args: &[&str]) -> Self {
        let authority = Authority::new("outbound test authority");
        let files = Files::new();
        let ca_file = files.authorities("ca.pem", &[&authority]);
        let options = crate::admin::options()
            .args_extra(args)
            .env("VIBE_CA_FILE", &crate::tls::text_of(&ca_file));
        Self {
            options,
            authority,
            files,
            ca_file,
        }
    }

    /// A good certificate for `localhost` and the loopback addresses.
    pub fn cert(&self) -> Cert {
        Cert::localhost(&self.authority)
    }

    /// The program, with [`APP`] and these more.
    pub fn start(self, more: &[(&str, &str)]) -> Outbound {
        Outbound::started(
            &self.options,
            self.authority,
            self.files,
            self.ca_file,
            more,
        )
    }
}

/// The program under test with the fixture Application [`APP`], the test
/// authority its `VIBE_CA_FILE` trusts, and the harness's way to change the
/// list and read the Operation log.
pub struct Outbound {
    pub vm: Vm,
    pub authority: Authority,
    pub ca_file: PathBuf,
    _files: Files,
}

impl Outbound {
    /// Started with `--outbound-private` for the loopback addresses.
    pub fn start() -> Self {
        Self::start_with(&OPEN_LOOPBACK, &[])
    }

    /// Started without `--outbound-private`: nothing of the loopback is
    /// opened.
    pub fn start_closed() -> Self {
        Self::start_with(&[], &[])
    }

    /// Started with these extra arguments and these extra applications
    /// (name, fixture file) installed beside [`APP`].
    pub fn start_with(args: &[&str], more: &[(&str, &str)]) -> Self {
        Setup::new(args).start(more)
    }

    /// Started with these options (the test authority's file named in
    /// `VIBE_CA_FILE` already).
    pub fn started(
        options: &VmOptions,
        authority: Authority,
        files: Files,
        ca_file: PathBuf,
        more: &[(&str, &str)],
    ) -> Self {
        let vm = Vm::try_start(options, START_TIMEOUT).unwrap_or_else(|e| {
            let asked = if options.args().iter().any(|a| a == "--outbound-private") {
                "the program did not start with --outbound-private"
            } else {
                "the program did not start"
            };
            panic!("{asked}: {e}")
        });
        vm.install(APP, &fixture_bundle("outbound.js"));
        for (name, file) in more {
            vm.install(name, &fixture_bundle(file));
        }
        Self {
            vm,
            authority,
            ca_file,
            _files: files,
        }
    }

    /// A good certificate for `localhost` and the loopback addresses.
    pub fn cert(&self) -> Cert {
        Cert::localhost(&self.authority)
    }

    /// An origin with a good certificate.
    pub fn origin(&self) -> Origin {
        Origin::start(self.cert().wire())
    }

    /// `PUT /v1/outbound-hosts/<entry>` as a Platform administrator, which
    /// must be `200`.
    pub fn allow(&self, entry: &str) -> Value {
        allow_on(&self.vm, entry)
    }

    /// `DELETE /v1/outbound-hosts/<entry>` as a Platform administrator,
    /// which must be `200`.
    pub fn disallow(&self, entry: &str) -> Value {
        disallow_on(&self.vm, entry)
    }

    /// `POST /call` of the fixture as bob with this spec; the response.
    pub fn call_response(&self, spec: &Value) -> Response {
        self.vm
            .request(APP, "/call")
            .as_user(&bob())
            .method("POST")
            .json(spec)
            .timeout(Duration::from_secs(90))
            .send()
    }

    /// The fixture's report of a call: `{ resolved }` or `{ rejected }`.
    pub fn call(&self, spec: &Value) -> Value {
        self.report(&self.call_response(spec))
    }

    /// The JSON of a response of the fixture, which must be `200`.
    pub fn report(&self, response: &Response) -> Value {
        if response.status != 200 {
            self.vm.fail(&format!(
                "expected the fixture to answer `200` with a report, got\n{}",
                response.describe()
            ));
        }
        response.json()
    }

    /// Every Operation log entry of [`APP`], oldest first.
    pub fn entries(&self) -> Vec<Value> {
        crate::admin::every_entry(&self.vm, &format!("application={APP}"), 1000).0
    }

    /// The entries of the request with this id.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        let id = response.request_id();
        self.entries()
            .into_iter()
            .filter(|e| e["request"].as_str() == Some(id.as_str()))
            .collect()
    }

    /// The entries of the request, which must be exactly one.
    pub fn entry_of(&self, response: &Response) -> Value {
        let entries = self.entries_of(response);
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one Operation log entry for the request, got {entries:?}"
        );
        entries[0].clone()
    }
}

/// The refusal of a rejected report, or what was there.
pub fn refusal_of(report: &Value) -> &Value {
    &report["rejected"]["refused"]
}

/// Fails unless the report is a rejection with a refusal of this reason and
/// host, having the properties the contract gives (`refusal`, `fix`).
pub fn assert_refused(report: &Value, reason: &str, host: Option<&str>, context: &str) {
    let refused = refusal_of(report);
    assert_eq!(
        refused["refusal"]["reason"].as_str(),
        Some(reason),
        "{context}: expected the fetch to reject with a refusal {reason:?}, got {report}"
    );
    if let Some(host) = host {
        assert_eq!(
            refused["refusal"]["host"].as_str(),
            Some(host),
            "{context}: expected the refusal's `host` {host:?}, got {report}"
        );
    }
    assert!(
        refused["fix"].as_str().is_some_and(|f| !f.is_empty()),
        "{context}: expected a `fix` sentence, got {report}"
    );
}

/// Fails unless the report is a rejection with a failure of this kind.
pub fn assert_failure(report: &Value, kind: &str, context: &str) {
    assert_eq!(
        report["rejected"]["failure"]["kind"].as_str(),
        Some(kind),
        "{context}: expected the fetch to reject with a failure of kind {kind:?}, got {report}"
    );
}

/// Fails unless the report is a resolved answer of this status.
pub fn assert_resolved(report: &Value, status: u64, context: &str) {
    assert_eq!(
        report["resolved"]["status"].as_u64(),
        Some(status),
        "{context}: expected the fetch to resolve with status {status}, got {report}"
    );
}

/// What an entry of a `fetch` must hold of the fields every entry has.
pub fn assert_fetch_entry(
    entry: &Value,
    outcome: &str,
    host: Option<&str>,
    reason: Option<&str>,
    error: Option<&str>,
    context: &str,
) {
    let want = |field: &str, expected: Value| {
        assert_eq!(
            entry[field], expected,
            "{context}: expected `{field}` {expected} in the entry {entry}"
        );
    };
    want("operation", json!("fetch"));
    want("resource", Value::Null);
    want("own_database", json!(false));
    want("actor", json!("user"));
    want("statement", Value::Null);
    want("rows", Value::Null);
    want("outcome", json!(outcome));
    want("host", host.map_or(Value::Null, |h| json!(h)));
    want("reason", reason.map_or(Value::Null, |r| json!(r)));
    want("error", error.map_or(Value::Null, |e| json!(e)));
}

/// A `Result` of waiting for the program's log to hold a `host-call` record
/// of `fetch` with this outcome for the request.
pub fn host_call_records(vm: &Vm, response: &Response) -> Vec<Value> {
    let id = response.request_id();
    vm.records()
        .into_iter()
        .filter(|r| {
            r["event"] == "host-call"
                && r["operation"] == "fetch"
                && r["request"].as_str() == Some(id.as_str())
        })
        .collect()
}

/// Whether an I/O error is a timeout (for the harness's own waits).
pub fn timed_out(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// Every `host-call` record of `fetch` in the program's log so far.
pub fn fetch_records(vm: &Vm) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| r["event"] == "host-call" && r["operation"] == "fetch")
        .collect()
}
