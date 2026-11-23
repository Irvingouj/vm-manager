//! The raw client the tests of a request's and a response's body use
//! (CONTRACT.md, "What an Application is handed"; the tests
//! `blackbox/tests/body_request.rs`, `body_response.rs` and
//! `body_sent_ends.rs`).
//!
//! [`crate::http::Request`] sends a whole request and reads a whole answer
//! of at most 4 MiB. The body tests must hold bytes back, send them late or
//! never, read them slowly, watch when the head arrives, and see a
//! connection closed short of its `content-length`: [`Raw`] is a plain
//! `TcpStream` with exactly that control and nothing else. Every step has a
//! deadline, so a failing test fails within seconds and never hangs.
//!
//! [`memory_of`] reads the program's resident memory from `/proc`, and
//! [`Checksum`] streams a SHA-256, so a body of hundreds of MiB passes
//! without the test holding it.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::http::User;

/// The most a request's body may state, under `vibe serve` and in the local
/// run alike (CONTRACT.md, "What an Application is handed"): 5 GiB and
/// 64 KiB. Exactly this is taken; a stated length over it is `413` before
/// the code runs.
pub const REQUEST_BODY_BOUND: u64 = 5 * 1024 * 1024 * 1024 + 64 * 1024;

/// One mebibyte, the size the tests' smaller bodies use.
pub const MIB: u64 = 1024 * 1024;

/// 64 KiB: the platform's own chunk, and the one this client writes.
pub const CHUNK: usize = 64 * 1024;

/// How long a single write to the program may take. A healthy flow never
/// comes near it (loopback takes a chunk in microseconds); it is there so
/// that a program that stopped reading without closing ends a failing test
/// instead of hanging it. It is short enough that the slow test which never
/// reads the answer still fails within its own 45 seconds.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// A `len`-byte block whose byte `i` is `i % 251`: the same block sent and
/// hashed on both sides of a large body, so neither side holds it.
pub fn pattern_block(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).expect("below 256"))
        .collect()
}

/// The program's resident memory as `/proc/<pid>/status` states it, in
/// bytes: `VmHWM` (the high-water mark, which only grows) or `VmRSS`
/// (now). Fails the test when the process or the field is not there.
pub fn memory_of(pid: u32, field: &str) -> u64 {
    let path = format!("/proc/{pid}/status");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next().and_then(|name| name.strip_suffix(':')) == Some(field) {
            let stated = fields
                .next()
                .unwrap_or_else(|| panic!("no number in {line:?}"));
            let kb: u64 = stated
                .parse()
                .unwrap_or_else(|e| panic!("{path}: bad {field} value {stated:?}: {e}"));
            return kb.saturating_mul(1024);
        }
    }
    panic!("{path} has no {field} line");
}

/// A SHA-256 fed piece by piece, for a body too large to hold: the
/// checksum of a body built from [`pattern_block`] repeated is that block
/// hashed as many times.
#[derive(Default)]
pub struct Checksum(Sha256);

impl Checksum {
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Feeds these bytes.
    pub fn take(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The checksum so far, in lower-case hex.
    pub fn hex(self) -> String {
        self.0
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// How a request's body is framed on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// A stated `Content-Length`; the body itself is the test's to send, or
    /// never.
    Length(u64),
    /// `Transfer-Encoding: chunked`: no length is stated.
    Chunked,
    /// No body at all.
    None,
}

/// The answer's head, once read whole.
#[derive(Clone, Debug)]
pub struct Head {
    pub status: u16,
    /// Every header, as (name in lower case, value), in order.
    headers: Vec<(String, String)>,
}

impl Head {
    /// The first value of a header, by name in any letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The length the answer states, when it states one.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|v| v.parse::<u64>().ok())
    }

    /// Whether the answer's body is sent chunked.
    pub fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"))
    }

    /// The answer's `x-vm-request-id`, which every answer carries.
    pub fn request_id(&self) -> String {
        match self.headers_named("x-vm-request-id").as_slice() {
            [id] if !id.is_empty() => (*id).to_owned(),
            _ => panic!("expected one x-vm-request-id on the answer, got {self:?}"),
        }
    }

    /// Every value of a header, by name in any letter case.
    fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// What became of the answer's body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyEnd {
    /// It ended exactly as the head framed it.
    Whole,
    /// The connection was closed (or reset) with the body short of that:
    /// an end the client can tell, by `content-length`.
    ClosedShort,
    /// `within` passed with the body still coming.
    StillOpen,
}

/// What a look at the connection, taking nothing, finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peeked {
    /// It reads back closed: nothing is there and the end has come, or it
    /// was reset. A clean end behind buffered bytes reads as [`Peeked::Data`]
    /// until the bytes are drained.
    Closed,
    /// Bytes are buffered, waiting to be taken.
    Data,
    /// Nothing is buffered and the connection is open.
    Nothing,
}

/// What a read of the connection gave.
#[derive(PartialEq, Eq)]
enum Filled {
    /// That many bytes, appended to the buffer.
    Bytes(usize),
    /// Nothing yet: the wait ran out, or nothing was there.
    Nothing,
    /// The connection is closed; a reset counts.
    Closed,
}

/// Where a chunked answer's body has got to.
#[derive(Clone, Copy)]
enum Chunked {
    /// A size line is next.
    Size,
    /// That many bytes of a chunk's data are still to come.
    Data(u64),
    /// The chunk's data has ended; its `CRLF` is next.
    AfterData,
    /// The last, empty chunk has been read; the final `CRLF` is next.
    Trailer,
    /// The body has ended cleanly.
    Done,
}

/// "The platform sends the length of the body it has, or sends it chunked"
/// (CONTRACT.md, "How a response is framed"): the answer states exactly
/// `len`, or none and is chunked; never both.
pub fn assert_framed_truly(head: &Head, len: u64, what: &str) {
    match (head.content_length(), head.chunked()) {
        (Some(stated), false) => assert_eq!(stated, len, "{what}: content-length"),
        (None, true) => {}
        other => panic!(
            "{what}: expected content-length {len} (and not chunked) or chunked and none, got {other:?}"
        ),
    }
}

/// One connection to the program, speaking HTTP/1.1 by hand.
pub struct Raw {
    stream: TcpStream,
    /// Everything read so far: the head while it is not whole yet, the
    /// body's bytes after it (drained as the framing takes them).
    buffer: Vec<u8>,
    head: Option<Head>,
    /// Body bytes already handed out.
    taken: u64,
    chunked: Chunked,
    /// Whether `send_head` leaves out `Connection: close`, so that the
    /// connection is kept for the next request (HTTP/1.1's default).
    keep_alive: bool,
    /// How many interim (`1xx`, `101` aside) responses have come before the
    /// answer's head, and when the first did.
    interims: usize,
    first_interim: Option<Instant>,
}

impl Raw {
    /// Connects to the program; each request's head says `Connection:
    /// close`.
    pub fn connect(address: SocketAddr) -> Self {
        Self::open(address, false)
    }

    /// Connects to the program and keeps the connection for more than one
    /// request: heads leave out `Connection: close`, and a response read
    /// whole leaves what follows it in the buffer for the next one.
    pub fn connect_keep_alive(address: SocketAddr) -> Self {
        Self::open(address, true)
    }

    fn open(address: SocketAddr, keep_alive: bool) -> Self {
        let stream = TcpStream::connect(address)
            .unwrap_or_else(|e| panic!("cannot connect to {address}: {e}"));
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .expect("the write timeout can be set");
        Self {
            stream,
            buffer: Vec::new(),
            head: None,
            taken: 0,
            chunked: Chunked::Size,
            keep_alive,
            interims: 0,
            first_interim: None,
        }
    }

    /// How many interim responses (`100 Continue` and the like) arrived
    /// before the answer's head: they are read and skipped, not taken for
    /// the answer.
    pub fn interims(&self) -> usize {
        self.interims
    }

    /// Waits up to `within` for an interim response to arrive; how long it
    /// took, or `None` when the answer's head (or the end of the
    /// connection, or `within`) came first.
    pub fn wait_interim(&mut self, within: Duration) -> Option<Duration> {
        let began = Instant::now();
        loop {
            self.parse_head();
            if self.interims > 0 {
                return Some(began.elapsed());
            }
            if self.head.is_some() {
                return None;
            }
            let left = within.saturating_sub(began.elapsed());
            if left.is_zero() || self.fill(left) == Filled::Closed {
                return None;
            }
        }
    }

    /// Takes whatever arrives within `within` into the buffer and says how
    /// many bytes are buffered (unparsed) afterwards: a test that expects
    /// nothing on the wire (no body after a head, no interim response yet)
    /// asserts it is `0`.
    pub fn bytes_within(&mut self, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.fill(left) == Filled::Closed {
                return self.buffer.len();
            }
        }
    }

    /// Sets how long one write may take (all handles of the connection
    /// share it). A test that wants to see a write fail only because the
    /// program closed the connection sets it above the test's own length.
    pub fn set_write_timeout(&self, within: Duration) {
        self.stream
            .set_write_timeout(Some(within))
            .expect("the write timeout can be set");
    }

    /// Closes the connection now, for every handle of it (dropping a
    /// [`Raw`] does not while a [`Writer`] of it is alive): the client has
    /// gone away.
    pub fn go_away(&self) {
        // A connection already gone is what this wants: the error is moot.
        self.stream.shutdown(std::net::Shutdown::Both).ok();
    }

    /// Forgets the answer read so far, keeping the bytes that follow it in
    /// the buffer, for the next answer of a kept connection.
    fn forget_answer(&mut self) {
        self.head = None;
        self.taken = 0;
        self.chunked = Chunked::Size;
    }

    /// A second handle to the same connection, for writing the request's
    /// body on a thread of the test's own while the answer is read.
    pub fn split_writer(&self) -> Writer {
        Writer {
            stream: self
                .stream
                .try_clone()
                .expect("the connection can be shared"),
        }
    }

    /// Writes `bytes`; an error (the program closed the connection, say) is
    /// returned, not a failed test: a test that held the body back may
    /// find the program gone.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        write_all(&mut self.stream, bytes)
    }

    /// One chunk of a chunked request body.
    pub fn send_chunk(&mut self, chunk: &[u8]) -> Result<(), String> {
        send_chunk(&mut self.stream, chunk)
    }

    /// The last, empty chunk that ends a chunked request body.
    pub fn finish_chunks(&mut self) -> Result<(), String> {
        write_all(&mut self.stream, b"0\r\n\r\n")
    }

    /// Sends a request head: method, target, `Host`, the stand-in sign-in
    /// headers of `user` when given, the extra headers, and the framing
    /// asked for. Nothing of the body is sent: that is the test's, with
    /// [`Raw::write`] and [`Raw::send_chunk`], or never.
    pub fn send_head(
        &mut self,
        method: &str,
        target: &str,
        host: &str,
        user: Option<&User>,
        framing: Framing,
        extra: &[(&str, &str)],
    ) {
        if self.head.is_some() {
            self.forget_answer();
        }
        let mut wire = Vec::new();
        wire.extend_from_slice(format!("{method} {target} HTTP/1.1\r\n").as_bytes());
        wire.extend_from_slice(b"Host: ");
        wire.extend_from_slice(host.as_bytes());
        wire.extend_from_slice(b"\r\n");
        if let Some(user) = user {
            wire.extend_from_slice(b"x-vm-stand-in-user: ");
            wire.extend_from_slice(user.id.as_bytes());
            wire.extend_from_slice(b"\r\n");
            if !user.groups.is_empty() {
                wire.extend_from_slice(b"x-vm-stand-in-groups: ");
                wire.extend_from_slice(user.groups.join(",").as_bytes());
                wire.extend_from_slice(b"\r\n");
            }
        }
        for (name, value) in extra {
            wire.extend_from_slice(name.as_bytes());
            wire.extend_from_slice(b": ");
            wire.extend_from_slice(value.as_bytes());
            wire.extend_from_slice(b"\r\n");
        }
        match framing {
            Framing::Length(length) => {
                wire.extend_from_slice(format!("Content-Length: {length}\r\n").as_bytes());
            }
            Framing::Chunked => wire.extend_from_slice(b"Transfer-Encoding: chunked\r\n"),
            Framing::None => {}
        }
        if !self.keep_alive {
            wire.extend_from_slice(b"Connection: close\r\n");
        }
        wire.extend_from_slice(b"\r\n");
        write_all(&mut self.stream, &wire)
            .unwrap_or_else(|e| panic!("cannot send the head of {method} {target}: {e}"));
    }

    /// Whether the answer's head has arrived whole yet. Reads whatever is
    /// there, without waiting: bytes past the head stay in the buffer for
    /// the body to come.
    pub fn head_arrived(&mut self) -> bool {
        if self.head.is_some() {
            return true;
        }
        self.fill(Duration::ZERO);
        self.parse_head()
    }

    /// The answer's head, failing the test unless it arrives whole within
    /// `within` (a message says what arrived instead).
    pub fn read_head(&mut self, within: Duration) -> Head {
        self.try_read_head(within).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The answer's head, or what went wrong (nothing within `within`, or
    /// the connection closed first), for a test that has threads to stop
    /// before it fails.
    pub fn try_read_head(&mut self, within: Duration) -> Result<Head, String> {
        let deadline = Instant::now() + within;
        loop {
            if self.parse_head() {
                return Ok(self.head.clone().expect("the head was parsed"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "no answer within {within:?}; what arrived so far ({} bytes):\n{:?}",
                    self.buffer.len(),
                    String::from_utf8_lossy(&self.buffer)
                ));
            }
            if self.fill(left) == Filled::Closed {
                return Err(format!(
                    "the connection closed with no answer; what arrived so far:\n{:?}",
                    String::from_utf8_lossy(&self.buffer)
                ));
            }
        }
    }

    /// The answer's head, when it has already arrived.
    pub fn head(&self) -> &Head {
        self.head.as_ref().expect("the head has been read")
    }

    /// Looks at the connection without taking a byte from it.
    pub fn peek(&mut self) -> Peeked {
        self.stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .expect("the read timeout can be set");
        let mut byte = [0_u8; 1];
        match self.stream.peek(&mut byte) {
            Ok(0) => Peeked::Closed,
            Ok(_) => Peeked::Data,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Peeked::Nothing
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::BrokenPipe
                ) =>
            {
                Peeked::Closed
            }
            Err(e) => panic!("looking at the answer failed: {e}"),
        }
    }

    /// Whether the program has closed its end of the connection, taking no
    /// byte: a reset reads back closed, and the kernel's table
    /// (`/proc/net/tcp`) says `CLOSE_WAIT` or `CLOSE` for a clean end that
    /// still has bytes buffered behind it, which `peek` cannot tell from an
    /// open connection.
    pub fn peer_closed(&mut self) -> bool {
        if self.peek() == Peeked::Closed {
            return true;
        }
        let (Ok(local), Ok(remote)) = (self.stream.local_addr(), self.stream.peer_addr()) else {
            return true;
        };
        tcp_rows()
            .iter()
            .any(|row| row.local == local.port() && row.remote == remote.port() && row.closed())
    }

    /// Waits, taking no byte, until the program has closed its end
    /// ([`Raw::peer_closed`]); returns how long that took, or `None` when
    /// `within` passed with it still open.
    pub fn wait_closed_without_taking(&mut self, within: Duration) -> Option<Duration> {
        let began = Instant::now();
        loop {
            if self.peer_closed() {
                return Some(began.elapsed());
            }
            if began.elapsed() >= within {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// One whole exchange on this connection: the head (with a stated
    /// length when `body` is given, written at once: for bodies the socket
    /// buffers take), the answer's head, and its body to its end, kept in
    /// the returned bytes.
    pub fn exchange(
        &mut self,
        method: &str,
        target: &str,
        host: &str,
        user: Option<&User>,
        body: Option<&[u8]>,
        within: Duration,
    ) -> (Head, Vec<u8>, BodyEnd) {
        let framing = match body {
            Some(body) => Framing::Length(u64::try_from(body.len()).expect("a length")),
            None => Framing::None,
        };
        self.send_head(method, target, host, user, framing, &[]);
        if let Some(body) = body {
            self.write(body)
                .unwrap_or_else(|e| panic!("cannot send the body of {method} {target}: {e}"));
        }
        let head = self.read_head(within);
        let mut got = Vec::new();
        let end = self.drain_body(within, |bytes| got.extend_from_slice(bytes));
        (head, got, end)
    }

    /// Reads the answer's body to its end as the head frames it (or to the
    /// connection's end when the head frames none), handing each piece to
    /// `take` and keeping nothing. [`BodyEnd::ClosedShort`] is a finding
    /// the test asserts on, not an error here.
    pub fn drain_body(&mut self, within: Duration, mut take: impl FnMut(&[u8])) -> BodyEnd {
        let Some(head) = self.head.as_ref() else {
            panic!("the head must be read before the body is drained");
        };
        let framing = match (head.content_length(), head.chunked()) {
            (Some(length), _) => Framing::Length(length),
            (None, true) => Framing::Chunked,
            (None, false) => Framing::None,
        };
        if framing == Framing::Length(0) {
            return BodyEnd::Whole;
        }
        let deadline = Instant::now() + within;
        loop {
            match framing {
                Framing::Length(length) => {
                    while self.taken < length && !self.buffer.is_empty() {
                        let left = usize::try_from(length - self.taken)
                            .expect("a content-length below usize");
                        let now = left.min(self.buffer.len());
                        take(&self.buffer[..now]);
                        self.buffer.drain(..now);
                        self.taken += u64::try_from(now).expect("bounded by the length");
                    }
                    if self.taken >= length {
                        return BodyEnd::Whole;
                    }
                }
                Framing::Chunked => {
                    if self.take_chunked(&mut take) {
                        return BodyEnd::Whole;
                    }
                }
                Framing::None => {
                    if !self.buffer.is_empty() {
                        take(&self.buffer);
                        self.buffer.clear();
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return BodyEnd::StillOpen;
            }
            if self.fill(left) == Filled::Closed {
                return match framing {
                    Framing::None => BodyEnd::Whole,
                    _ => BodyEnd::ClosedShort,
                };
            }
        }
    }

    /// Takes what a chunked answer has buffered, one frame at a time;
    /// returns whether the body has ended cleanly.
    fn take_chunked(&mut self, take: &mut impl FnMut(&[u8])) -> bool {
        loop {
            match self.chunked {
                Chunked::Done => return true,
                Chunked::Size => {
                    let Some(line_end) = find_crlf(&self.buffer) else {
                        return false;
                    };
                    let line = String::from_utf8_lossy(&self.buffer[..line_end]).into_owned();
                    self.buffer.drain(..line_end + 2);
                    let size_text = line.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(size_text, 16)
                        .unwrap_or_else(|_| panic!("a chunk size that is not hex: {line:?}"));
                    self.chunked = if size == 0 {
                        Chunked::Trailer
                    } else {
                        Chunked::Data(size)
                    };
                }
                Chunked::Data(left) => {
                    if self.buffer.is_empty() {
                        return false;
                    }
                    let now = usize::try_from(left)
                        .unwrap_or(usize::MAX)
                        .min(self.buffer.len());
                    take(&self.buffer[..now]);
                    self.buffer.drain(..now);
                    let now = u64::try_from(now).expect("bounded by the chunk");
                    self.chunked = if left == now {
                        Chunked::AfterData
                    } else {
                        Chunked::Data(left - now)
                    };
                }
                Chunked::AfterData | Chunked::Trailer => {
                    if self.buffer.len() < 2 {
                        return false;
                    }
                    if !self.buffer.starts_with(b"\r\n") {
                        panic!(
                            "the answer's chunk is not followed by CRLF: {:?}",
                            String::from_utf8_lossy(&self.buffer)
                        );
                    }
                    let done = matches!(self.chunked, Chunked::Trailer);
                    self.buffer.drain(..2);
                    self.chunked = if done { Chunked::Done } else { Chunked::Size };
                }
            }
        }
    }

    /// Reads whatever is there into the buffer, waiting at most `wait`
    /// (1 ms at the least).
    fn fill(&mut self, wait: Duration) -> Filled {
        let mut chunk = [0_u8; CHUNK];
        // A zero timeout is an error for the socket and `O_NONBLOCK` would be
        // shared with the writing handle: wait the least there is instead.
        let wait = wait.max(Duration::from_millis(1));
        self.stream
            .set_read_timeout(Some(wait))
            .expect("the read timeout can be set");
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Filled::Closed,
                Ok(n) => {
                    let read = chunk.get(..n).expect("read within the buffer").to_vec();
                    self.buffer.extend_from_slice(&read);
                    return Filled::Bytes(n);
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Filled::Nothing;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // A reset with nothing new is the connection closed.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => return Filled::Closed,
                Err(e) => panic!("reading the answer failed: {e}"),
            }
        }
    }

    /// Parses the head out of the buffer when it is whole; the bytes past
    /// it stay for the body. Says whether the head is there.
    fn parse_head(&mut self) -> bool {
        if self.head.is_some() {
            return true;
        }
        loop {
            let Some(head_end) = find_subsequence(&self.buffer, b"\r\n\r\n") else {
                return false;
            };
            let head = parse_head(&self.buffer[..head_end]);
            self.buffer.drain(..head_end + 4);
            if (100..200).contains(&head.status) && head.status != 101 {
                self.interims += 1;
                self.first_interim.get_or_insert_with(Instant::now);
                continue;
            }
            self.head = Some(head);
            return true;
        }
    }
}

/// One row of the kernel's table of TCP sockets.
struct TcpRow {
    local: u16,
    remote: u16,
    state: u8,
}

impl TcpRow {
    /// `CLOSE` (7), `CLOSE_WAIT` (8) or `LAST_ACK` (9): the other end has
    /// closed.
    fn closed(&self) -> bool {
        matches!(self.state, 0x07..=0x09)
    }
}

/// The kernel's TCP sockets, from `/proc/net/tcp` and `/proc/net/tcp6`.
fn tcp_rows() -> Vec<TcpRow> {
    let mut rows = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let port = |at: usize| {
                fields
                    .get(at)
                    .and_then(|f| f.rsplit_once(':'))
                    .and_then(|(_, port)| u16::from_str_radix(port, 16).ok())
            };
            let state = fields.get(3).and_then(|s| u8::from_str_radix(s, 16).ok());
            if let (Some(local), Some(remote), Some(state)) = (port(1), port(2), state) {
                rows.push(TcpRow {
                    local,
                    remote,
                    state,
                });
            }
        }
    }
    rows
}

/// How many connections the program (the end listening on `port`) still
/// holds open: its sockets that are established, or whose other end has
/// closed and which it has not yet closed itself.
pub fn sockets_open_on(port: u16) -> usize {
    tcp_rows()
        .iter()
        .filter(|row| row.local == port && matches!(row.state, 0x01 | 0x07 | 0x08))
        .count()
}

/// Runs `work` while sampling the program's resident memory every 20 ms;
/// returns what `work` returned and the highest `VmRSS` seen, in bytes.
/// (`VmHWM` is the process's whole life's peak, which a start-up spike can
/// hide a transient buffer under.)
pub fn peak_rss_during<T>(pid: u32, work: impl FnOnce() -> T) -> (T, u64) {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let done = AtomicBool::new(false);
    let peak = AtomicU64::new(memory_of(pid, "VmRSS"));
    std::thread::scope(|scope| {
        let sampling = scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max(memory_of(pid, "VmRSS"), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let outcome = work();
        done.store(true, Ordering::Relaxed);
        sampling.join().expect("the sampler ended");
        (outcome, peak.load(Ordering::Relaxed))
    })
}

/// The writing half of a connection, on a thread of the test's own.
pub struct Writer {
    stream: TcpStream,
}

impl Writer {
    /// Writes `bytes`; an error (the program closed the connection, say) is
    /// returned, not a failed test.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        write_all(&mut self.stream, bytes)
    }

    /// One chunk of a chunked request body.
    pub fn send_chunk(&mut self, chunk: &[u8]) -> Result<(), String> {
        send_chunk(&mut self.stream, chunk)
    }

    /// The last, empty chunk that ends a chunked request body.
    pub fn finish_chunks(&mut self) -> Result<(), String> {
        write_all(&mut self.stream, b"0\r\n\r\n")
    }
}

/// Writes one chunk of a chunked request body.
fn send_chunk(stream: &mut TcpStream, chunk: &[u8]) -> Result<(), String> {
    let mut wire = Vec::with_capacity(chunk.len() + 32);
    wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
    wire.extend_from_slice(chunk);
    wire.extend_from_slice(b"\r\n");
    write_all(stream, &wire)
}

fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    stream.write_all(bytes).map_err(|e| e.to_string())
}

fn parse_head(raw: &[u8]) -> Head {
    let text = String::from_utf8_lossy(raw).into_owned();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("not an HTTP status line: {status_line:?}"));
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Head { status, headers }
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    find_subsequence(bytes, b"\r\n")
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// A connection whose buffer holds `raw`, as a read would have left it,
    /// without any program behind it: the other end of the pair is returned
    /// too, so that a test can keep it open or close it.
    fn fed(raw: &str) -> (Raw, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = listener.local_addr().expect("the listener's address");
        let stream = TcpStream::connect(address).expect("the client's end");
        let (server, _) = listener.accept().expect("the listener's end");
        let fed = Raw {
            stream,
            buffer: raw.as_bytes().to_vec(),
            head: None,
            taken: 0,
            chunked: Chunked::Size,
            keep_alive: true,
            interims: 0,
            first_interim: None,
        };
        (fed, server)
    }

    #[test]
    fn the_head_is_parsed_and_its_body_framing_seen() {
        let (mut raw, _server) =
            fed("HTTP/1.1 413 Too Large\r\nContent-Length: 4\r\nX-Vm-Request-Id: r1\r\n\r\nnope");
        assert!(raw.parse_head());
        let head = raw.head();
        assert_eq!(head.status, 413);
        assert_eq!(head.header("content-length"), Some("4"));
        assert_eq!(head.content_length(), Some(4));
        assert!(!head.chunked());
        assert_eq!(head.request_id(), "r1");
        let mut taken = Vec::new();
        let end = raw.drain_body(Duration::from_secs(1), |bytes| {
            taken.extend_from_slice(bytes)
        });
        assert_eq!(end, BodyEnd::Whole);
        assert_eq!(taken, b"nope");
    }

    #[test]
    fn a_chunked_answer_is_decoded_to_its_end() {
        let (mut raw, _server) = fed(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n",
        );
        assert!(raw.parse_head());
        assert!(raw.head().chunked());
        assert!(raw.head().content_length().is_none());
        let mut taken = Vec::new();
        let end = raw.drain_body(Duration::from_secs(1), |bytes| {
            taken.extend_from_slice(bytes)
        });
        assert_eq!(end, BodyEnd::Whole);
        assert_eq!(taken, b"abcde");
    }

    #[test]
    fn a_body_short_of_its_content_length_is_still_open_then() {
        // The other end stays open: the body is short and still coming.
        let (mut raw, _server) = fed("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        assert!(raw.parse_head());
        let end = raw.drain_body(Duration::from_millis(50), |_| ());
        assert_eq!(end, BodyEnd::StillOpen);
    }

    #[test]
    fn an_empty_body_ends_at_once_and_none_reads_to_the_close() {
        let (mut empty, _server) = fed("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert!(empty.parse_head());
        assert_eq!(
            empty.drain_body(Duration::from_millis(50), |_| ()),
            BodyEnd::Whole
        );

        // The other end closes before the body is read: the close is the end
        // when the head frames none.
        let (mut unframed, server) = fed("HTTP/1.1 200 OK\r\n\r\nwhatever");
        drop(server);
        assert!(unframed.parse_head());
        let mut taken = Vec::new();
        let end = unframed.drain_body(Duration::from_millis(50), |bytes| {
            taken.extend_from_slice(bytes)
        });
        assert_eq!(end, BodyEnd::Whole);
        assert_eq!(taken, b"whatever");
    }

    #[test]
    fn two_answers_on_one_connection_are_read_one_after_the_other() {
        let (mut raw, _server) = fed(
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcHTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nde\r\n0\r\n\r\n",
        );
        assert!(raw.parse_head());
        let mut first = Vec::new();
        assert_eq!(
            raw.drain_body(Duration::from_secs(1), |b| first.extend_from_slice(b)),
            BodyEnd::Whole
        );
        assert_eq!(first, b"abc");
        raw.forget_answer();
        assert!(raw.parse_head());
        assert_eq!(raw.head().status, 201);
        let mut second = Vec::new();
        assert_eq!(
            raw.drain_body(Duration::from_secs(1), |b| second.extend_from_slice(b)),
            BodyEnd::Whole
        );
        assert_eq!(second, b"de");
    }

    #[test]
    fn a_peer_that_closed_with_bytes_unread_is_seen_closed_without_taking_them() {
        let (mut raw, mut server) = fed("");
        server.write_all(b"unread bytes").expect("written");
        drop(server);
        let after = raw.wait_closed_without_taking(Duration::from_secs(2));
        assert!(after.is_some(), "a closed peer is seen from the table");
        assert_eq!(raw.peek(), Peeked::Data, "the bytes are still there");
    }

    #[test]
    fn an_interim_response_is_skipped_and_counted() {
        let (mut raw, _server) =
            fed("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert!(raw.parse_head());
        assert_eq!(raw.head().status, 200);
        assert_eq!(raw.interims(), 1);
    }

    #[test]
    fn an_open_peer_is_not_seen_closed() {
        let (mut raw, _server) = fed("");
        assert!(!raw.peer_closed());
    }

    #[test]
    fn the_stand_in_headers_are_sent_as_the_sign_in_states_them() {
        // The head writer's stand-in lines, checked against what the
        // contract says the sign-in headers are.
        let user = User::new("bob").in_groups(&["staff", "finance"]);
        let mut wire = Vec::new();
        wire.extend_from_slice(b"x-vm-stand-in-user: ");
        wire.extend_from_slice(user.id.as_bytes());
        wire.extend_from_slice(b"\r\nx-vm-stand-in-groups: ");
        wire.extend_from_slice(user.groups.join(",").as_bytes());
        wire.extend_from_slice(b"\r\n");
        assert_eq!(
            String::from_utf8(wire).unwrap(),
            "x-vm-stand-in-user: bob\r\nx-vm-stand-in-groups: staff,finance\r\n"
        );
    }

    #[test]
    fn a_pattern_block_hashes_the_same_whichever_side_feeds_it() {
        let block = pattern_block(CHUNK);
        assert_eq!(block.len(), CHUNK);
        let mut sent = Checksum::new();
        for _ in 0..3 {
            sent.take(&block);
        }
        let mut got = Checksum::new();
        for byte in block.iter().cycle().take(3 * CHUNK) {
            got.take(&[*byte]);
        }
        let sent = sent.hex();
        assert_eq!(sent, got.hex());
        assert_ne!(sent, Checksum::new().hex());
    }
}
