//! One connection to a shared database, spoken to in Postgres's protocol
//! directly (messages from `postgres-protocol`, the crate tokio-postgres is
//! built on), because the executor needs what a general client hides:
//! results in text, as the server prints them; the extended protocol for
//! Application SQL, so that one string is one statement; a byte and row
//! count kept while the result arrives, with a message too large for what
//! is left of the byte limit refused from its header, before its body is
//! read; and the cancel key, to stop a statement from outside.
//!
//! Nothing here holds or reports the connection's host, port, database,
//! role or password: [`WireError`] is a kind, never a message.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::{Buf, BufMut, BytesMut};
use fallible_iterator::FallibleIterator;
use postgres_protocol::IsNull;
use postgres_protocol::authentication::md5_hash;
use postgres_protocol::authentication::sasl::{ChannelBinding, SCRAM_SHA_256, ScramSha256};
use postgres_protocol::message::backend::{
    DataRowBody, ErrorResponseBody, Header, Message, RowDescriptionBody,
};
use postgres_protocol::message::frontend;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};

use super::{Failure, Label, Level, Limit, Limits, Param, Rows, Target};

/// The largest message other than a row that is read. A larger one is a
/// protocol error and the connection is closed.
const MAX_MESSAGE: usize = 1024 * 1024;

/// How much is read from the socket at a time, at most.
const READ_CHUNK: usize = 64 * 1024;

/// What went wrong with a connection. A kind only: it carries nothing of
/// the connection, so it can be logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WireError {
    /// The network failed, or the server closed the connection.
    Io,
    /// Waiting took longer than its limit.
    TimedOut,
    /// The server sent what the protocol does not allow here.
    Protocol,
    /// The server refused the login: a wrong password, a role or database
    /// that does not exist, too many connections. The SQLSTATE's class.
    Refused,
    /// The server asked for a way of signing in this client does not have.
    Unsupported,
}

impl WireError {
    /// For the program's log.
    pub(super) fn describe(self) -> &'static str {
        match self {
            Self::Io => "the connection failed or was closed",
            Self::TimedOut => "the shared database did not answer in time",
            Self::Protocol => "the shared database sent something unexpected",
            Self::Refused => "the shared database refused the Resource's login",
            Self::Unsupported => {
                "the shared database asks for a way of signing in the platform does not support"
            }
        }
    }
}

fn io(_: std::io::Error) -> WireError {
    WireError::Io
}

fn protocol<E>(_: E) -> WireError {
    WireError::Protocol
}

/// A message read, or the header of one too large to read.
enum Incoming {
    Message(Message),
    /// Its body is being discarded as it arrives.
    Oversized {
        tag: u8,
    },
}

/// One operation: the Application's statement, its parameters, the level
/// of its transaction and its identity label.
pub(super) struct Operation<'a> {
    pub(super) level: Level,
    pub(super) sql: &'a str,
    pub(super) params: &'a [Param],
    pub(super) label: Label<'a>,
}

/// How one operation went on the connection.
pub(super) struct Run {
    /// The answer, or why there is none.
    pub(super) result: Result<Rows, Failure>,
    /// When the Application's statement was sent; `None` if it was not.
    pub(super) sent: Option<Instant>,
    /// When it ended.
    pub(super) ended: Instant,
    /// Whether the connection is clean and may serve another operation.
    pub(super) reusable: bool,
    /// The connection did not come back within the grace period after a
    /// cancel: its backend must be ended from another connection.
    pub(super) stuck: Option<i32>,
    /// The connection failed before the server answered anything: it was
    /// probably closed while idle, and the statement may be tried on a new
    /// one.
    pub(super) stale: bool,
}

pub(super) struct Connection {
    stream: TcpStream,
    /// Bytes read and not yet taken as a message. At most one message under
    /// its limit plus one [`READ_CHUNK`]: more is read only when the
    /// message at its front is incomplete.
    buffer: BytesMut,
    /// What is left of the body of an oversized message, to discard.
    skip: usize,
    /// Where the server is, for a cancel request.
    peer: SocketAddr,
    /// The cancel key.
    process_id: Option<i32>,
    secret_key: i32,
}

impl Connection {
    /// Connects and logs in, within `limit`.
    pub(super) async fn open(target: &Target, limit: Duration) -> Result<Self, WireError> {
        timeout(limit, Self::open_now(target))
            .await
            .map_err(|_| WireError::TimedOut)?
    }

    async fn open_now(target: &Target) -> Result<Self, WireError> {
        let stream = TcpStream::connect((target.host.as_str(), target.port))
            .await
            .map_err(io)?;
        stream.set_nodelay(true).map_err(io)?;
        let peer = stream.peer_addr().map_err(io)?;
        let mut connection = Self {
            stream,
            buffer: BytesMut::with_capacity(READ_CHUNK),
            skip: 0,
            peer,
            process_id: None,
            secret_key: 0,
        };
        let mut out = BytesMut::new();
        // The session as the database is set: only the encoding the program
        // reads, a name to recognise it by, and the string syntax the
        // statement scanner (crate::statement) uses. TimeZone and DateStyle
        // are left as the database sets them.
        frontend::startup_message(
            [
                ("user", target.role.as_str()),
                ("database", target.database.as_str()),
                ("client_encoding", "UTF8"),
                ("application_name", "vm"),
                ("standard_conforming_strings", "on"),
            ],
            &mut out,
        )
        .map_err(protocol)?;
        connection.send(&mut out).await?;
        let password = target.password.expose().as_bytes();
        let mut scram: Option<ScramSha256> = None;
        loop {
            let Incoming::Message(message) = connection.receive(0).await? else {
                return Err(WireError::Protocol);
            };
            match message {
                Message::AuthenticationOk
                | Message::ParameterStatus(_)
                | Message::NoticeResponse(_) => {}
                Message::AuthenticationCleartextPassword => {
                    frontend::password_message(password, &mut out).map_err(protocol)?;
                    connection.send(&mut out).await?;
                }
                Message::AuthenticationMd5Password(body) => {
                    let hash = md5_hash(target.role.as_bytes(), password, body.salt());
                    frontend::password_message(hash.as_bytes(), &mut out).map_err(protocol)?;
                    connection.send(&mut out).await?;
                }
                Message::AuthenticationSasl(body) => {
                    let offered = body
                        .mechanisms()
                        .any(|m| Ok(m == SCRAM_SHA_256))
                        .map_err(protocol)?;
                    if !offered {
                        return Err(WireError::Unsupported);
                    }
                    let exchange = ScramSha256::new(password, ChannelBinding::unsupported());
                    frontend::sasl_initial_response(SCRAM_SHA_256, exchange.message(), &mut out)
                        .map_err(protocol)?;
                    scram = Some(exchange);
                    connection.send(&mut out).await?;
                }
                Message::AuthenticationSaslContinue(body) => {
                    let exchange = scram.as_mut().ok_or(WireError::Protocol)?;
                    exchange.update(body.data()).map_err(protocol)?;
                    frontend::sasl_response(exchange.message(), &mut out).map_err(protocol)?;
                    connection.send(&mut out).await?;
                }
                Message::AuthenticationSaslFinal(body) => {
                    let exchange = scram.as_mut().ok_or(WireError::Protocol)?;
                    // Checks the server's signature: a server that does not
                    // know the password cannot pretend to.
                    exchange.finish(body.data()).map_err(protocol)?;
                }
                Message::BackendKeyData(body) => {
                    connection.process_id = Some(body.process_id());
                    connection.secret_key = body.secret_key();
                }
                Message::ReadyForQuery(_) => return Ok(connection),
                Message::ErrorResponse(_) => return Err(WireError::Refused),
                Message::AuthenticationGss
                | Message::AuthenticationKerberosV5
                | Message::AuthenticationScmCredential
                | Message::AuthenticationSspi
                | Message::AuthenticationGssContinue(_) => return Err(WireError::Unsupported),
                _ => return Err(WireError::Protocol),
            }
        }
    }

    /// Writes `out` whole and empties it.
    async fn send(&mut self, out: &mut BytesMut) -> Result<(), WireError> {
        self.stream.write_all(out).await.map_err(io)?;
        out.clear();
        Ok(())
    }

    /// The next message. A row (`D`) whose body is longer than `row_limit`,
    /// or any other message longer than [`MAX_MESSAGE`], is not read: its
    /// header is reported and its body discarded as it arrives.
    async fn receive(&mut self, row_limit: usize) -> Result<Incoming, WireError> {
        loop {
            while self.skip > 0 {
                if self.buffer.is_empty() {
                    self.fill().await?;
                }
                let dropped = self.skip.min(self.buffer.len());
                self.buffer.advance(dropped);
                self.skip -= dropped;
            }
            if let Some(header) = Header::parse(&self.buffer).map_err(protocol)? {
                let length = usize::try_from(header.len()).map_err(protocol)?;
                let body = length.checked_sub(4).ok_or(WireError::Protocol)?;
                let limit = if header.tag() == b'D' {
                    row_limit
                } else {
                    MAX_MESSAGE
                };
                if body > limit {
                    // The tag and the length: five bytes, all here.
                    self.buffer.advance(5);
                    self.skip = body;
                    return Ok(Incoming::Oversized { tag: header.tag() });
                }
                if let Some(message) = Message::parse(&mut self.buffer).map_err(protocol)? {
                    return Ok(Incoming::Message(message));
                }
            }
            self.fill().await?;
        }
    }

    /// Reads what has arrived, up to [`READ_CHUNK`] more than is held.
    async fn fill(&mut self) -> Result<(), WireError> {
        self.buffer.reserve(READ_CHUNK);
        let mut chunk = (&mut self.stream).take(u64::try_from(READ_CHUNK).unwrap_or(u64::MAX));
        let read = chunk.read_buf(&mut self.buffer).await.map_err(io)?;
        if read == 0 {
            return Err(WireError::Io);
        }
        Ok(())
    }

    /// Runs one operation as one transaction (ADR 0013), all in one write
    /// ending in one Sync: `BEGIN READ ONLY` at [`Level::Read`] or `BEGIN
    /// READ WRITE` at [`Level::Write`]; the host's own first statement,
    /// which sets the identity label (its values as parameters); then the
    /// Application's statement over the extended protocol. If the host's
    /// part fails, the Application's statement is skipped. Then, when the
    /// statement ended on its own, the transaction is ended, `COMMIT` if
    /// the statement succeeded and `ROLLBACK` if it failed, together with
    /// `DISCARD ALL`; the connection is reusable only if both were answered
    /// without error and it is idle again. A `COMMIT` that fails turns the
    /// answer into that failure.
    ///
    /// The limits of `limits` are kept here: rows and bytes are counted as
    /// the result arrives; the statement has `limits.statement` from when
    /// it is sent. Past any of them the statement is cancelled, its
    /// transaction rolled back, and the connection is never reused (a
    /// cancel may still be on its way). If it does not come back within
    /// `limits.grace`, it is reported stuck. Nothing is committed unless
    /// the host sends `COMMIT`, so a connection dropped at any point rolls
    /// its transaction back.
    pub(super) async fn operate(&mut self, operation: &Operation<'_>, limits: &Limits) -> Run {
        let mut out = BytesMut::new();
        if encode_operation(operation, &mut out).is_err() {
            // The text holds a NUL; checked before, so not expected here.
            return self.ended(Err(Failure::Unavailable), None, false);
        }
        if self.send(&mut out).await.is_err() {
            return Run {
                stale: true,
                ..self.ended(Err(Failure::Unavailable), None, false)
            };
        }
        let sent = Instant::now();
        let deadline = sent + limits.statement;
        let mut result = Reading::new(limits);
        let mut answered = false;
        let status = loop {
            let row_limit = result.bytes_left();
            let incoming = match timeout_at(deadline, self.receive(row_limit)).await {
                Ok(Ok(incoming)) => incoming,
                Ok(Err(_)) => {
                    let failure = Err(Failure::Unavailable);
                    return Run {
                        stale: !answered,
                        ..self.ended(failure, if answered { Some(sent) } else { None }, false)
                    };
                }
                Err(_) => return self.stop(Limit::Time, sent, limits).await,
            };
            answered = true;
            match result.take(incoming, &mut out) {
                Step::Go => {}
                Step::Reply => {
                    if self.send(&mut out).await.is_err() {
                        return self.ended(Err(Failure::Unavailable), Some(sent), false);
                    }
                }
                Step::Ready(status) => break status,
                Step::Limit(limit) => return self.stop(limit, sent, limits).await,
                Step::Broken => {
                    return self.ended(Err(Failure::Unavailable), Some(sent), false);
                }
            }
        };
        let statement_ended = Instant::now();
        let result = result.finish();
        // The statement ran, or Postgres refused it: the connection is as
        // good as before, once its transaction is ended and its session
        // state discarded. Otherwise (the host's own part failed, or the
        // server is somewhere unexpected) it is closed, which rolls back.
        let ran_or_refused = matches!(result, Ok(_) | Err(Failure::Database { .. }));
        if !(matches!(status, b'T' | b'E' | b'I') && ran_or_refused) {
            return self.ended(result, Some(sent), false);
        }
        let commit = result.is_ok();
        let (ending, clean) = self.end_transaction(commit, limits.grace).await;
        let result = match (result, ending) {
            (Ok(_), Err(failure)) => Err(failure),
            (result, _) => result,
        };
        Run {
            ended: statement_ended,
            ..self.ended(result, Some(sent), clean)
        }
    }

    fn ended(&self, result: Result<Rows, Failure>, sent: Option<Instant>, reusable: bool) -> Run {
        Run {
            result,
            sent,
            ended: Instant::now(),
            reusable,
            stuck: None,
            stale: false,
        }
    }

    /// `COMMIT` (or `ROLLBACK`) and `DISCARD ALL`, sent together, within
    /// `limit`. Answers how the transaction ended (a `COMMIT` Postgres
    /// refused is its failure; no answer is unavailable), and whether the
    /// connection is shown clean: both answered, neither with an error, and
    /// idle outside any transaction after each. `DISCARD ALL` ends what a
    /// session can hold: settings and the search path, temporary tables,
    /// prepared statements, cursors (`WITH HOLD` too), session advisory
    /// locks, `LISTEN`, and any change of role.
    async fn end_transaction(
        &mut self,
        commit: bool,
        limit: Duration,
    ) -> (Result<(), Failure>, bool) {
        let mut out = BytesMut::new();
        let end = if commit { "COMMIT" } else { "ROLLBACK" };
        if frontend::query(end, &mut out).is_err()
            || frontend::query("DISCARD ALL", &mut out).is_err()
        {
            return (Err(Failure::Unavailable), false);
        }
        let mut ending: Result<(), Failure> = Err(Failure::Unavailable);
        let mut clean = true;
        let exchange = async {
            self.send(&mut out).await?;
            let mut failed: Option<Failure> = None;
            let mut ready = 0;
            while ready < 2 {
                match self.receive(MAX_MESSAGE).await? {
                    Incoming::Message(Message::ReadyForQuery(body)) => {
                        if ready == 0 {
                            ending = failed.take().map_or(Ok(()), Err);
                        }
                        ready += 1;
                        clean &= body.status() == b'I';
                    }
                    Incoming::Message(Message::ErrorResponse(body)) => {
                        if ready == 0 {
                            failed = Some(database_error(&body));
                        }
                        clean = false;
                    }
                    Incoming::Oversized { .. } => clean = false,
                    Incoming::Message(_) => {}
                }
            }
            Ok::<(), WireError>(())
        };
        let answered = matches!(timeout(limit, exchange).await, Ok(Ok(())));
        (ending, answered && clean)
    }

    /// Past a limit: cancels the statement and waits up to `limits.grace`
    /// for the connection to come back, discarding what arrives.
    async fn stop(&mut self, limit: Limit, sent: Instant, limits: &Limits) -> Run {
        let ended = Instant::now();
        let grace = ended + limits.grace;
        let cancelled = match self.process_id {
            Some(process_id) => timeout_at(grace, cancel(self.peer, process_id, self.secret_key))
                .await
                .is_ok_and(|sent| sent.is_ok()),
            None => false,
        };
        let back = cancelled
            && timeout_at(grace, self.drain())
                .await
                .is_ok_and(|r| r.is_ok());
        if back {
            // Its transaction rolled back now, rather than when the
            // connection is closed, so that what it holds is let go at
            // once. Not committed either way.
            let rolled_back = timeout_at(grace, self.simple("ROLLBACK")).await;
            if !matches!(rolled_back, Ok(Ok(true))) {
                return Run {
                    result: Err(Failure::Limit(limit)),
                    sent: Some(sent),
                    ended,
                    reusable: false,
                    stuck: self.process_id,
                    stale: false,
                };
            }
        }
        Run {
            result: Err(Failure::Limit(limit)),
            sent: Some(sent),
            ended,
            reusable: false,
            stuck: if back { None } else { self.process_id },
            stale: false,
        }
    }

    /// Reads and discards until the server is ready again.
    async fn drain(&mut self) -> Result<(), WireError> {
        loop {
            if let Incoming::Message(Message::ReadyForQuery(_)) = self.receive(MAX_MESSAGE).await? {
                return Ok(());
            }
        }
    }

    /// Runs one statement of the host's own over the simple protocol and
    /// waits for its end; whether it succeeded.
    pub(super) async fn simple(&mut self, sql: &str) -> Result<bool, WireError> {
        let mut out = BytesMut::new();
        frontend::query(sql, &mut out).map_err(protocol)?;
        self.send(&mut out).await?;
        let mut succeeded = true;
        loop {
            match self.receive(MAX_MESSAGE).await? {
                Incoming::Message(Message::ReadyForQuery(_)) => return Ok(succeeded),
                Incoming::Message(Message::ErrorResponse(_)) => succeeded = false,
                Incoming::Message(_) | Incoming::Oversized { .. } => {}
            }
        }
    }
}

/// Sends a cancel request for `process_id` to the server at `peer`, on a
/// connection of its own, and waits for the server to close it.
async fn cancel(peer: SocketAddr, process_id: i32, secret_key: i32) -> Result<(), WireError> {
    let mut stream = TcpStream::connect(peer).await.map_err(io)?;
    let mut out = BytesMut::new();
    frontend::cancel_request(process_id, secret_key, &mut out);
    stream.write_all(&out).await.map_err(io)?;
    let mut nothing = [0_u8; 1];
    stream.read(&mut nothing).await.map_err(io)?;
    Ok(())
}

/// The messages of one operation, ending in one Sync.
fn encode_operation(operation: &Operation<'_>, out: &mut BytesMut) -> Result<(), WireError> {
    let begin = match operation.level {
        Level::Read => "BEGIN READ ONLY",
        Level::Write => "BEGIN READ WRITE",
    };
    let label = operation.label;
    let host: [(&str, &[&str]); 2] = [
        (begin, &[]),
        (
            HOST_FIRST_STATEMENT,
            &[label.user, label.application, label.request],
        ),
    ];
    for (sql, values) in host {
        frontend::parse("", sql, values.iter().map(|_| 0), out).map_err(protocol)?;
        frontend::bind(
            "",
            "",
            std::iter::empty(),
            values.iter(),
            |value, buf| {
                buf.put_slice(value.as_bytes());
                Ok(IsNull::No)
            },
            std::iter::empty(),
            out,
        )
        .map_err(protocol)?;
        frontend::execute("", 0, out).map_err(protocol)?;
    }
    // Types left for the server to infer (0), every parameter and every
    // result column in text (no format codes means text).
    frontend::parse("", operation.sql, operation.params.iter().map(|_| 0), out)
        .map_err(protocol)?;
    frontend::bind(
        "",
        "",
        std::iter::empty(),
        operation.params,
        |param, buf| match param {
            Param::Null => Ok(IsNull::Yes),
            Param::Text(text) => {
                buf.put_slice(text.as_bytes());
                Ok(IsNull::No)
            }
        },
        std::iter::empty(),
        out,
    )
    .map_err(protocol)?;
    frontend::describe(b'P', "", out).map_err(protocol)?;
    frontend::execute("", 0, out).map_err(protocol)?;
    frontend::sync(out);
    Ok(())
}

/// The host's own first statement in the transaction, before Application
/// SQL: it sets the identity label as transaction-local settings, gone
/// when the transaction ends (ADR 0013: on a shared database, with nothing
/// installed, a label Application SQL could overwrite). Its values are
/// parameters, never part of the text. Once a statement has run, a
/// read-only transaction can no longer be made read-write (`SET
/// TRANSACTION READ WRITE` fails with 25001).
const HOST_FIRST_STATEMENT: &str = "SELECT set_config('vm.user', $1, true), \
     set_config('vm.application', $2, true), set_config('vm.request', $3, true)";

/// What to do after a message.
enum Step {
    Go,
    /// A message was written to `out` and must be sent.
    Reply,
    /// ReadyForQuery, with the transaction status.
    Ready(u8),
    Limit(Limit),
    Broken,
}

/// The result of the Application's statement as it arrives.
struct Reading {
    /// Statements of the host's own still to complete.
    host_left: u8,
    columns: Option<Vec<(String, u32)>>,
    rows: Vec<Value>,
    /// The count in the statement's command tag.
    affected: Option<u64>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    outcome: Option<Result<(), Failure>>,
}

impl Reading {
    fn new(limits: &Limits) -> Self {
        Self {
            host_left: 2,
            columns: None,
            rows: Vec::new(),
            affected: None,
            bytes: 0,
            max_rows: limits.max_rows,
            max_bytes: limits.max_bytes,
            outcome: None,
        }
    }

    /// The longest row body that still fits the byte limit.
    fn bytes_left(&self) -> usize {
        self.max_bytes.saturating_sub(self.bytes)
    }

    fn take(&mut self, incoming: Incoming, out: &mut BytesMut) -> Step {
        let message = match incoming {
            Incoming::Message(message) => message,
            Incoming::Oversized { tag: b'D' } if self.host_left == 0 => {
                return Step::Limit(Limit::Bytes);
            }
            Incoming::Oversized { .. } => return Step::Broken,
        };
        match message {
            Message::ParseComplete
            | Message::BindComplete
            | Message::NoData
            | Message::ParameterDescription(_)
            | Message::ParameterStatus(_)
            | Message::NoticeResponse(_)
            | Message::NotificationResponse(_)
            | Message::CopyDone
            | Message::CopyOutResponse(_) => Step::Go,
            Message::CommandComplete(body) => {
                if self.host_left > 0 {
                    self.host_left -= 1;
                } else if self.outcome.is_none() {
                    self.affected = body.tag().ok().and_then(count_of);
                    self.outcome = Some(Ok(()));
                }
                Step::Go
            }
            Message::EmptyQueryResponse => {
                if self.host_left > 0 {
                    self.host_left -= 1;
                } else if self.outcome.is_none() {
                    self.outcome = Some(Ok(()));
                }
                Step::Go
            }
            Message::PortalSuspended => {
                self.outcome.get_or_insert(Ok(()));
                Step::Go
            }
            Message::RowDescription(body) => match columns_of(&body) {
                Ok(columns) => {
                    self.columns = Some(columns);
                    Step::Go
                }
                Err(_) => Step::Broken,
            },
            Message::DataRow(body) if self.host_left == 0 => self.row(&body),
            Message::DataRow(_) => Step::Go,
            Message::CopyData(body) => {
                // COPY ... TO STDOUT: nothing to answer, but it counts.
                self.bytes = self.bytes.saturating_add(body.data().len());
                if self.bytes > self.max_bytes {
                    Step::Limit(Limit::Bytes)
                } else {
                    Step::Go
                }
            }
            Message::CopyInResponse(_) => {
                // COPY ... FROM STDIN: the program has nothing to send.
                match frontend::copy_fail("COPY from the client is not available", out) {
                    Ok(()) => Step::Reply,
                    Err(_) => Step::Broken,
                }
            }
            Message::ErrorResponse(body) => {
                if self.host_left > 0 {
                    // The host's own part failed: not the Application's
                    // statement, which was skipped.
                    self.outcome = Some(Err(Failure::Unavailable));
                } else {
                    self.outcome = Some(Err(database_error(&body)));
                }
                Step::Go
            }
            Message::ReadyForQuery(body) => Step::Ready(body.status()),
            _ => Step::Broken,
        }
    }

    fn row(&mut self, body: &DataRowBody) -> Step {
        let Some(columns) = &self.columns else {
            return Step::Broken;
        };
        if self.rows.len() >= self.max_rows {
            return Step::Limit(Limit::Rows);
        }
        self.bytes = self.bytes.saturating_add(body.buffer().len());
        if self.bytes > self.max_bytes {
            return Step::Limit(Limit::Bytes);
        }
        let mut values = Vec::with_capacity(columns.len());
        let mut ranges = body.ranges();
        for (_, oid) in columns {
            let value = match ranges.next() {
                Ok(Some(Some(range))) => match body.buffer().get(range) {
                    Some(text) => decode(*oid, text),
                    None => return Step::Broken,
                },
                Ok(Some(None)) => Value::Null,
                Ok(None) | Err(_) => return Step::Broken,
            };
            values.push(value);
        }
        self.rows.push(Value::Array(values));
        Step::Go
    }

    fn finish(self) -> Result<Rows, Failure> {
        match self.outcome {
            Some(Ok(())) => Ok(Rows {
                columns: self
                    .columns
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect(),
                rows: self.rows,
                affected: self.affected,
            }),
            Some(Err(failure)) => Err(failure),
            None => Err(Failure::Unavailable),
        }
    }
}

/// The count a command tag ends with: `INSERT 0 3`, `UPDATE 2`, `SELECT 5`.
fn count_of(tag: &str) -> Option<u64> {
    tag.rsplit(' ').next()?.parse().ok()
}

fn columns_of(body: &RowDescriptionBody) -> Result<Vec<(String, u32)>, std::io::Error> {
    body.fields()
        .map(|field| Ok((field.name().to_owned(), field.type_oid())))
        .collect()
}

/// Postgres's SQLSTATE and primary message. Its detail, hint and the rest
/// are not kept: they can name roles and values.
fn database_error(body: &ErrorResponseBody) -> Failure {
    let mut code = String::new();
    let mut message = String::new();
    let mut fields = body.fields();
    while let Ok(Some(field)) = fields.next() {
        match field.type_() {
            b'C' => code = String::from_utf8_lossy(field.value_bytes()).into_owned(),
            b'M' => message = String::from_utf8_lossy(field.value_bytes()).into_owned(),
            _ => {}
        }
    }
    Failure::Database { code, message }
}

/// A value as Postgres printed it, for Application code
/// (blackbox/CONTRACT.md, "Operations on a shared database"): `bool` as a
/// boolean; `int2`, `int4`, `float4` and `float8` as numbers; `json` and
/// `jsonb` parsed; anything else, or a value that does not read as its type
/// says (a float that is not finite), as the text itself.
fn decode(oid: u32, text: &[u8]) -> Value {
    const BOOL: u32 = 16;
    const INT2: u32 = 21;
    const INT4: u32 = 23;
    const JSON: u32 = 114;
    const FLOAT4: u32 = 700;
    const FLOAT8: u32 = 701;
    const JSONB: u32 = 3802;
    let text = String::from_utf8_lossy(text);
    let typed = match oid {
        BOOL => match &*text {
            "t" => Some(Value::Bool(true)),
            "f" => Some(Value::Bool(false)),
            _ => None,
        },
        INT2 | INT4 => text.parse::<i64>().ok().map(Value::from),
        FLOAT4 | FLOAT8 => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        JSON | JSONB => serde_json::from_str(&text).ok(),
        _ => None,
    };
    typed.unwrap_or_else(|| Value::String(text.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_is_read_from_the_command_tag() {
        assert_eq!(count_of("INSERT 0 3"), Some(3));
        assert_eq!(count_of("UPDATE 12"), Some(12));
        assert_eq!(count_of("DELETE 0"), Some(0));
        assert_eq!(count_of("SELECT 10000"), Some(10_000));
        assert_eq!(count_of("COMMIT"), None);
    }

    #[test]
    fn values_are_decoded_as_the_contract_says() {
        assert_eq!(decode(16, b"t"), Value::Bool(true));
        assert_eq!(decode(21, b"-12"), serde_json::json!(-12));
        assert_eq!(decode(701, b"-0.25"), serde_json::json!(-0.25));
        assert_eq!(decode(701, b"NaN"), serde_json::json!("NaN"));
        assert_eq!(
            decode(20, b"9007199254740993"),
            serde_json::json!("9007199254740993")
        );
        assert_eq!(decode(3802, br#"{"a": 1}"#), serde_json::json!({ "a": 1 }));
        assert_eq!(
            decode(25, "h\u{e9}".as_bytes()),
            serde_json::json!("h\u{e9}")
        );
    }
}
