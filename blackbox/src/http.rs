//! A small HTTP/1.1 client written for the tests, so that a test has full
//! control over what goes on the wire: the `Host` header or none, a header
//! given twice, any letter case, bytes a general-purpose client would refuse.
//!
//! Every request is sent on a fresh connection with `Connection: close`, and
//! every step has a deadline: a request that gets no complete answer within
//! [`REQUEST_TIMEOUT`] (or the request's own timeout) fails with a message
//! saying so.
//!
//! A large body is written on a second thread while the answer is read, so
//! that a program that answers before reading the whole body (a `413`) is
//! heard rather than lost in a reset connection.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The longest a request may take, from connecting to the last byte of the
/// answer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest answer the client reads. A larger one is an error, not a
/// bigger buffer.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// A request this large or larger is written on its own thread.
const WRITE_ON_A_THREAD_FROM: usize = 64 * 1024;

/// The size of each chunk of a chunked body.
const CHUNK_BYTES: usize = 64 * 1024;

/// A User as the stand-in sign-in states them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub groups: Vec<String>,
}

impl User {
    /// A User with no Groups.
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            groups: Vec::new(),
        }
    }

    /// The same User, member of these Groups.
    #[must_use]
    pub fn in_groups(mut self, groups: &[&str]) -> Self {
        self.groups = groups.iter().map(|g| (*g).to_owned()).collect();
        self
    }
}

/// One request, built step by step and sent with [`Request::send`].
///
/// Headers are sent in the order they were added, exactly as given: nothing
/// is merged, lower-cased or checked.
#[derive(Clone, Debug)]
pub struct Request {
    address: SocketAddr,
    method: String,
    target: String,
    host: Option<Vec<u8>>,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    chunked: bool,
    timeout: Duration,
}

impl Request {
    /// A `GET` of `target` (path and query) on the program at `address`,
    /// with this `Host`.
    pub fn new(address: SocketAddr, target: &str, host: &str) -> Self {
        Self {
            address,
            method: "GET".to_owned(),
            target: target.to_owned(),
            host: Some(host.as_bytes().to_vec()),
            headers: Vec::new(),
            body: Vec::new(),
            chunked: false,
            timeout: REQUEST_TIMEOUT,
        }
    }

    #[must_use]
    pub fn method(mut self, method: &str) -> Self {
        method.clone_into(&mut self.method);
        self
    }

    /// Sends the stand-in sign-in headers for this User: the User header,
    /// and the Groups header when they have Groups.
    #[must_use]
    pub fn as_user(self, user: &User) -> Self {
        let signed_in = self.header("x-vm-stand-in-user", &user.id);
        if user.groups.is_empty() {
            signed_in
        } else {
            signed_in.header("x-vm-stand-in-groups", user.groups.join(","))
        }
    }

    /// Adds a header, verbatim. Adding the same name twice sends it twice.
    #[must_use]
    pub fn header(mut self, name: &str, value: impl AsRef<[u8]>) -> Self {
        self.headers
            .push((name.to_owned(), value.as_ref().to_vec()));
        self
    }

    /// Replaces the `Host` header's value.
    #[must_use]
    pub fn host(mut self, host: &str) -> Self {
        self.host = Some(host.as_bytes().to_vec());
        self
    }

    /// Sends no `Host` header at all.
    #[must_use]
    pub fn without_host(mut self) -> Self {
        self.host = None;
        self
    }

    #[must_use]
    pub fn body(mut self, body: impl AsRef<[u8]>) -> Self {
        self.body = body.as_ref().to_vec();
        self
    }

    /// A JSON body, with `content-type: application/json`.
    #[must_use]
    pub fn json(self, value: &Value) -> Self {
        self.header("content-type", "application/json")
            .body(value.to_string())
    }

    /// Sends the body with `Transfer-Encoding: chunked` instead of a
    /// `Content-Length`, so that the program learns its size only by
    /// reading it.
    #[must_use]
    pub fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }

    /// Allows this request longer than [`REQUEST_TIMEOUT`] (a large body).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sends the request and returns the answer, or fails the test with a
    /// message that says which request got no answer and why.
    pub fn send(&self) -> Response {
        match self.try_send() {
            Ok(response) => response,
            Err(problem) => panic!("{self} got no answer: {problem}"),
        }
    }

    /// Sends the request; an error says what went wrong on the wire.
    pub fn try_send(&self) -> Result<Response, String> {
        let deadline = Instant::now() + self.timeout;
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .map_err(|e| format!("cannot connect to {}: {e}", self.address))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        let wire = self.wire_bytes();
        if wire.len() < WRITE_ON_A_THREAD_FROM {
            stream
                .write_all(&wire)
                .map_err(|e| format!("cannot send the request: {e}"))?;
            let raw = read_to_end_before(&mut stream, deadline, self.timeout)?;
            return Response::parse(&raw, self.to_string());
        }
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        let (read, wrote) = thread::scope(|scope| {
            let writing = scope.spawn(move || writer.write_all(&wire));
            let read = read_to_end_before(&mut stream, deadline, self.timeout);
            // Unblocks the writer if the program answered without reading
            // everything and left the connection open.
            drop(stream.shutdown(Shutdown::Both));
            let wrote = writing
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("the writer panicked")));
            (read, wrote)
        });
        match read {
            Ok(raw) if !raw.is_empty() => Response::parse(&raw, self.to_string()),
            Ok(_) => Err(format!(
                "the connection closed with no answer (writing the request: {wrote:?})"
            )),
            Err(problem) => Err(format!("{problem} (writing the request: {wrote:?})")),
        }
    }

    fn wire_bytes(&self) -> Vec<u8> {
        let mut wire = Vec::with_capacity(256 + self.body.len());
        wire.extend_from_slice(format!("{} {} HTTP/1.1\r\n", self.method, self.target).as_bytes());
        if let Some(host) = &self.host {
            wire.extend_from_slice(b"Host: ");
            wire.extend_from_slice(host);
            wire.extend_from_slice(b"\r\n");
        }
        for (name, value) in &self.headers {
            wire.extend_from_slice(name.as_bytes());
            wire.extend_from_slice(b": ");
            wire.extend_from_slice(value);
            wire.extend_from_slice(b"\r\n");
        }
        if self.chunked {
            wire.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
        } else if !self.body.is_empty() || self.method != "GET" {
            wire.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        wire.extend_from_slice(b"Connection: close\r\n\r\n");
        if self.chunked {
            for chunk in self.body.chunks(CHUNK_BYTES) {
                wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                wire.extend_from_slice(chunk);
                wire.extend_from_slice(b"\r\n");
            }
            wire.extend_from_slice(b"0\r\n\r\n");
        } else {
            wire.extend_from_slice(&self.body);
        }
        wire
    }
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.target)?;
        match &self.host {
            Some(host) => write!(f, " (Host: {})", String::from_utf8_lossy(host))?,
            None => write!(f, " (no Host)")?,
        }
        let user_headers: Vec<String> = self
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-vm-stand-in-user"))
            .map(|(_, value)| format!("{:?}", String::from_utf8_lossy(value)))
            .collect();
        if user_headers.is_empty() {
            write!(f, " not signed in")
        } else {
            write!(f, " as {}", user_headers.join(" and "))
        }
    }
}

fn read_to_end_before(
    stream: &mut TcpStream,
    deadline: Instant,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "no complete answer within {timeout:?} (received so far: {:?})",
                String::from_utf8_lossy(&raw)
            ));
        }
        stream
            .set_read_timeout(Some(left))
            .map_err(|e| e.to_string())?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                let read = chunk.get(..n).unwrap_or_default();
                if raw.len() + read.len() > MAX_RESPONSE_BYTES {
                    return Err(format!(
                        "the answer is larger than {MAX_RESPONSE_BYTES} bytes"
                    ));
                }
                raw.extend_from_slice(read);
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // A reset after a complete answer still leaves a usable answer.
            Err(e) if !raw.is_empty() && e.kind() == ErrorKind::ConnectionReset => return Ok(raw),
            Err(e) => return Err(format!("reading the answer failed: {e}")),
        }
    }
}

/// An answer from the program.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// The request this answers, for messages.
    request: String,
}

impl Response {
    fn parse(raw: &[u8], request: String) -> Result<Self, String> {
        let head_end = find(raw, b"\r\n\r\n").ok_or_else(|| {
            format!(
                "the answer has no complete head: {:?}",
                String::from_utf8_lossy(raw)
            )
        })?;
        let head = String::from_utf8_lossy(raw.get(..head_end).unwrap_or_default()).into_owned();
        let rest = raw.get(head_end + 4..).unwrap_or_default();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| format!("not an HTTP status line: {status_line:?}"))?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let mut response = Self {
            status,
            headers,
            body: Vec::new(),
            request,
        };
        let chunked = response
            .header("transfer-encoding")
            .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"));
        response.body = if chunked {
            decode_chunked(rest)?
        } else if let Some(length) = response.header("content-length") {
            let length: usize = length
                .parse()
                .map_err(|_| format!("bad content-length {length:?}"))?;
            rest.get(..length)
                .ok_or_else(|| format!("the body is shorter than its content-length {length}"))?
                .to_vec()
        } else {
            rest.to_vec()
        };
        Ok(response)
    }

    /// The first value of a header, by name in any letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of a header, by name in any letter case.
    pub fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The `x-vm-request-id` header, which every answer carries.
    pub fn request_id(&self) -> String {
        match self.headers_named("x-vm-request-id").as_slice() {
            [id] if !id.is_empty() => (*id).to_owned(),
            other => panic!(
                "expected exactly one non-empty x-vm-request-id header on the answer to {}, found {other:?}\n{}",
                self.request,
                self.describe()
            ),
        }
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON, or a failed test that shows the answer.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "expected a JSON body in the answer to {} ({e})\n{}",
                self.request,
                self.describe()
            )
        })
    }

    /// Fails the test, showing the answer, unless the status is `expected`.
    pub fn expect_status(&self, expected: u16) -> &Self {
        assert!(
            self.status == expected,
            "expected status {expected} for {}, got {}\n{}",
            self.request,
            self.status,
            self.describe()
        );
        self
    }

    /// The request this answers, as text.
    pub fn request(&self) -> &str {
        &self.request
    }

    /// The whole answer as text, for failure messages.
    pub fn describe(&self) -> String {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(n, v)| format!("  {n}: {v}"))
            .collect();
        format!(
            "status {}\n{}\nbody: {}",
            self.status,
            headers.join("\n"),
            self.text()
        )
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end = find(rest, b"\r\n").ok_or("a chunk has no size line")?;
        let size_line =
            String::from_utf8_lossy(rest.get(..line_end).unwrap_or_default()).into_owned();
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size {size_line:?}"))?;
        rest = rest.get(line_end + 2..).unwrap_or_default();
        if size == 0 {
            return Ok(body);
        }
        body.extend_from_slice(rest.get(..size).ok_or("a chunk is cut short")?);
        rest = rest.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Response {
        Response::parse(raw.as_bytes(), "a test request".to_owned()).unwrap()
    }

    #[test]
    fn a_body_framed_by_content_length_is_read() {
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Vm-Request-Id: r1\r\n\r\nhello");
        assert_eq!(r.status, 200);
        assert_eq!(r.text(), "hello");
        assert_eq!(r.request_id(), "r1");
    }

    #[test]
    fn a_chunked_body_is_decoded() {
        let r = parse(
            "HTTP/1.1 500 Internal Server Error\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n",
        );
        assert_eq!(r.status, 500);
        assert_eq!(r.text(), "abcde");
    }

    #[test]
    fn headers_are_found_in_any_letter_case_and_repeats_are_kept() {
        let r = parse("HTTP/1.1 200 OK\r\nA: 1\r\na: 2\r\n\r\n");
        assert_eq!(r.header("A"), Some("1"));
        assert_eq!(r.headers_named("a"), vec!["1", "2"]);
    }

    #[test]
    fn headers_go_on_the_wire_verbatim_and_in_order() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let wire = Request::new(address, "/p?q=1", "app.vm.test")
            .header("X-VM-Stand-In-User", "a")
            .header("x-vm-stand-in-user", "b")
            .wire_bytes();
        assert_eq!(
            String::from_utf8(wire).unwrap(),
            "GET /p?q=1 HTTP/1.1\r\nHost: app.vm.test\r\nX-VM-Stand-In-User: a\r\nx-vm-stand-in-user: b\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn a_chunked_body_goes_on_the_wire_in_chunks() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let wire = Request::new(address, "/", "x")
            .method("POST")
            .body("abc")
            .chunked()
            .wire_bytes();
        assert_eq!(
            String::from_utf8(wire).unwrap(),
            "POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n"
        );
    }

    #[test]
    fn a_request_without_host_sends_none() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let wire = Request::new(address, "/", "x").without_host().wire_bytes();
        assert!(
            !String::from_utf8(wire)
                .unwrap()
                .to_ascii_lowercase()
                .contains("host:")
        );
    }
}
