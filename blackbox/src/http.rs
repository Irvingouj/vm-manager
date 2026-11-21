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

/// The largest answer the client reads, its head included. A larger one is
/// an error, not a bigger buffer. A body of 4 MiB (the most a page of the
/// Operation log holds) and its head fit.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024 + 64 * 1024;

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
    /// A `Content-Length` to send in place of the body's own (a body held
    /// back: the request says more than it sends).
    declared_length: Option<usize>,
    /// Sent over TLS, asking for this server name and trusting only these
    /// roots ([`Request::over_tls`]); plain HTTP when `None`.
    tls: Option<(String, Vec<rustls::pki_types::CertificateDer<'static>>)>,
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
            declared_length: None,
            tls: None,
        }
    }

    /// Sends the request over TLS (the harness's own client,
    /// [`crate::tls::tls_exchange`]), asking for `server_name` and trusting
    /// only `roots`: what a browser does for an `https` address.
    #[must_use]
    pub fn over_tls(
        mut self,
        server_name: &str,
        roots: &[rustls::pki_types::CertificateDer<'static>],
    ) -> Self {
        self.tls = Some((server_name.to_owned(), roots.to_vec()));
        self
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

    /// Sends `Authorization: Bearer <credential>`: a sign-in with a tool
    /// credential (CONTRACT.md, "The tool: On the platform's API").
    #[must_use]
    pub fn bearer(self, credential: &str) -> Self {
        self.header("authorization", format!("Bearer {credential}"))
    }

    /// Adds a header, verbatim. Adding the same name twice sends it twice.
    #[must_use]
    pub fn header(mut self, name: &str, value: impl AsRef<[u8]>) -> Self {
        self.headers
            .push((name.to_owned(), value.as_ref().to_vec()));
        self
    }

    /// The name of the `Host` header's value, without a port (empty when
    /// there is none): the server name a TLS client asks for.
    pub fn host_name(&self) -> String {
        let host = String::from_utf8_lossy(self.host.as_deref().unwrap_or_default()).into_owned();
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name.to_owned(),
            _ => host,
        }
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

    /// Says `Content-Length: <length>` whatever the body is: with a body
    /// shorter than that, the rest is never sent, and the program waits for
    /// it until its own limit.
    #[must_use]
    pub fn declare_length(mut self, length: usize) -> Self {
        self.declared_length = Some(length);
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
        if let Some((server_name, roots)) = &self.tls {
            let wire = self.wire_bytes();
            // A large body is written while the answer is read, as over
            // plain HTTP: a server may answer (`413`) before it has read it
            // all, and close.
            let raw = if wire.len() < WRITE_ON_A_THREAD_FROM {
                crate::tls::tls_exchange(self.address, server_name, roots, &wire, self.timeout)?
            } else {
                crate::tls::tls_exchange_while_writing(
                    self.address,
                    server_name,
                    roots,
                    &wire,
                    self.timeout,
                )?
            };
            return Response::parse_answer(&raw, self.to_string(), self.method == "HEAD");
        }
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
            return Response::parse_answer(&raw, self.to_string(), self.method == "HEAD");
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
            Ok(raw) if !raw.is_empty() => {
                Response::parse_answer(&raw, self.to_string(), self.method == "HEAD")
            }
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
        if let Some(length) = self.declared_length {
            wire.extend_from_slice(format!("Content-Length: {length}\r\n").as_bytes());
        } else if self.chunked {
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
        write!(f, "{} {}", self.method, mask_values(&self.target))?;
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
    /// Parses an answer; the answer to a `HEAD` has no body whatever its
    /// `Content-Length` or `Transfer-Encoding` say.
    fn parse_answer(raw: &[u8], request: String, answers_head: bool) -> Result<Self, String> {
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
        response.body = if answers_head {
            Vec::new()
        } else if chunked {
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

    /// Parses an answer written by a unit test of the harness.
    pub fn parse_for_tests(raw: &str) -> Self {
        Self::parse_answer(raw.as_bytes(), "a test answer".to_owned(), false)
            .unwrap_or_else(|e| panic!("{e}"))
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
            .map(|(n, v)| format!("  {n}: {}", mask_header(n, v)))
            .collect();
        mask_values(&format!(
            "status {}\n{}\nbody: {}",
            self.status,
            headers.join("\n"),
            self.text()
        ))
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

/// The names whose values a failure message never shows in full: as a
/// query or form parameter (`name=value`) or a JSON member
/// (`"name": "value"`).
pub const MASKED_NAMES: [&str; 9] = [
    "grant",
    "verifier",
    "sign_in",
    "code",
    "state",
    "credential",
    "refresh_token",
    "access_token",
    "id_token",
];

fn hidden(value: &str) -> String {
    format!("<hidden, {} characters>", value.chars().count())
}

/// A header's value for a failure message: a cookie's value, a
/// `Set-Cookie`'s value and a bearer are shown by their length.
pub fn mask_header(name: &str, value: &str) -> String {
    if name.eq_ignore_ascii_case("cookie") {
        return value
            .split(';')
            .map(|pair| match pair.split_once('=') {
                Some((n, v)) => format!("{n}={}", hidden(v)),
                None => pair.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(";");
    }
    if name.eq_ignore_ascii_case("set-cookie") {
        let (first, rest) = value.split_once(';').unwrap_or((value, ""));
        let first = match first.split_once('=') {
            Some((n, v)) => format!("{n}={}", hidden(v)),
            None => first.to_owned(),
        };
        return if rest.is_empty() {
            first
        } else {
            format!("{first};{rest}")
        };
    }
    if name.eq_ignore_ascii_case("authorization") {
        return match value.split_once(' ') {
            Some((scheme, v)) => format!("{scheme} {}", hidden(v)),
            None => hidden(value),
        };
    }
    value.to_owned()
}

/// `text` with the value of every parameter or JSON member named in
/// [`MASKED_NAMES`], and of every `Bearer`, replaced by its length: for
/// failure messages, which never show them in full.
pub fn mask_values(text: &str) -> String {
    let mut out = text.to_owned();
    for name in MASKED_NAMES {
        out = mask_after(
            &out,
            &format!("{name}="),
            |c| c.is_whitespace() || "&\"'<>);,#".contains(c),
            |before| before.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_')),
        );
        for quoted in [format!("\"{name}\":\""), format!("\"{name}\": \"")] {
            out = mask_after(&out, &quoted, |c| c == '"', |_| true);
        }
    }
    for bearer in ["Bearer ", "bearer "] {
        out = mask_after(
            &out,
            bearer,
            |c| c.is_whitespace() || "\"',".contains(c),
            |_| true,
        );
    }
    out
}

/// Replaces what follows each `marker` (where `starts` takes the character
/// before it) up to the first character `ends` takes with its length.
fn mask_after(
    text: &str,
    marker: &str,
    ends: impl Fn(char) -> bool,
    starts: impl Fn(Option<char>) -> bool,
) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        let (before, from) = rest.split_at(at);
        result.push_str(before);
        let after = from.get(marker.len()..).unwrap_or_default();
        result.push_str(marker);
        let previous = result
            .get(..result.len() - marker.len())
            .and_then(|r| r.chars().last());
        if !starts(previous) {
            rest = after;
            continue;
        }
        let run = after
            .char_indices()
            .find(|(_, c)| ends(*c))
            .map_or(after.len(), |(i, _)| i);
        let value = after.get(..run).unwrap_or_default();
        if value.starts_with("<hidden, ") || value.is_empty() {
            rest = after;
            continue;
        }
        result.push_str(&hidden(value));
        rest = after.get(run..).unwrap_or_default();
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_that_must_not_be_shown_are_shown_by_their_length() {
        assert_eq!(
            mask_values("GET /?grant=abcd&port=1 and /cb?code=xy&state=s1 error_code=7"),
            "GET /?grant=<hidden, 4 characters>&port=1 and /cb?code=<hidden, 2 characters>&state=<hidden, 2 characters> error_code=7"
        );
        assert_eq!(
            mask_values(r#"{"verifier":"abc","grant": "de","sign_in":"x","other":"y"}"#),
            r#"{"verifier":"<hidden, 3 characters>","grant": "<hidden, 2 characters>","sign_in":"<hidden, 1 characters>","other":"y"}"#
        );
        assert_eq!(
            mask_values("authorization: Bearer vmtc_secret"),
            "authorization: Bearer <hidden, 11 characters>"
        );
        assert_eq!(
            mask_values(&mask_values("code=abc")),
            "code=<hidden, 3 characters>"
        );
        assert_eq!(
            mask_header("cookie", "a=12; b=345"),
            "a=<hidden, 2 characters>; b=<hidden, 3 characters>"
        );
        assert_eq!(
            mask_header("set-cookie", "vm_session=abcdef; Path=/; HttpOnly"),
            "vm_session=<hidden, 6 characters>; Path=/; HttpOnly"
        );
    }

    #[test]
    fn a_failure_message_of_an_answer_hides_cookies_and_parameters() {
        let answer = parse(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/?grant=topsecret\r\nSet-Cookie: s=cookievalue; Path=/\r\nContent-Length: 0\r\n\r\n",
        );
        let text = answer.describe();
        assert!(
            !text.contains("topsecret") && !text.contains("cookievalue"),
            "{text}"
        );
    }

    fn parse(raw: &str) -> Response {
        Response::parse_answer(raw.as_bytes(), "a test request".to_owned(), false).unwrap()
    }

    #[test]
    fn a_body_framed_by_content_length_is_read() {
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Vm-Request-Id: r1\r\n\r\nhello");
        assert_eq!(r.status, 200);
        assert_eq!(r.text(), "hello");
        assert_eq!(r.request_id(), "r1");
    }

    #[test]
    fn the_answer_to_head_has_no_body_whatever_its_length_says() {
        let r = Response::parse_answer(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n",
            "HEAD /".to_owned(),
            true,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert!(r.body().is_empty());
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
