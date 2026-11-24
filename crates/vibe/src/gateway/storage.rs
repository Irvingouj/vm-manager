//! The five Storage operations (blackbox/CONTRACT.md, "Storage: Through the
//! raw interface"), as the host runs them, in the contract's order:
//!
//! 1. the arguments are read ([`read`]): malformed is answered so, with
//!    nothing logged or touched. A put's `body` is one of the kinds the
//!    raw interface takes, as the platform's glue (crate::runner's
//!    `prelude.js`) hands it: a string, `null` (an empty file), or a
//!    marker naming a stream — one the host itself feeds (the request's
//!    own body, another file's) by its id, or one the glue pumps into
//!    (the code's own stream, bytes it held at once) with its length when
//!    known, or a note that the stream was locked, already read, or that
//!    the code may not have another stream of its own. A stream named is
//!    the put's from the call on: its receiver is taken inside the call,
//!    so the code can neither read it nor race the put for it. A list's
//!    `cursor` is opened with the key the platform database keeps
//!    (crate::storage::cursor);
//! 2. the Authorization decision, against Resources of kind `storage` only
//!    ([`PlatformGateway::decide_kind`]);
//! 3. in a task of its own (as every operation): a refusal's entry; then
//!    the key's rules (`invalid-key`, a whole `failed` entry, nothing
//!    touched), a put's body past the file bound before a byte is read
//!    (`limit` `bytes`, the same), a put that could not make the stream
//!    it pumps into (`unavailable`, the same); then the entry written
//!    `unfinished` (`log-unavailable` if it cannot be); then the
//!    backend's operation — head, get, delete and list within 10 seconds
//!    of the decision, given up if the Application is disabled meanwhile;
//!    a put part of its request, one write and the landing each within
//!    their own 10 seconds, given up (`request-ended`, or
//!    `application-disabled` at a disable) when the request ends — and
//!    the entry completed: a `get` when the file is open (the body is
//!    fed afterwards, a chunk a read job, its place among the bodies
//!    open at once held by the pump that feeds it), a `put` when it has
//!    landed or failed.
//!
//! The bounds of bodies open at once (16 a request, 512 the program)
//! are the request's [`Bodies`] (crate::runner::InFlight) and the
//! program's [`Open`] (crate::storage::Storage): an operation that opens
//! one waits up to [`WAIT`] for a place, then fails `unavailable`.
//!
//! The statement written in the entry is the operation's word and its key
//! (crate::storage::key::quoted); no entry holds a file's contents, content
//! type, size or the cursor.

use std::sync::Arc;
use std::time::Instant;

use authz::AccessLevel;
use bytes::Bytes;
use openworkers_runtime_v8::runtime::stream_manager::{StreamChunk, StreamId, StreamManager};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use super::Guard as CallGuard;
use super::transaction::ended_why;
use super::{
    APPLICATION_DISABLED, Decision, Dispatched, On, PlatformGateway, Refusal, Target, actor_name,
    error_to_wire, malformed, ok_to_wire,
};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::own_tls::rfc3339;
use crate::registry::ResourceKind;
use crate::runner::InFlight;
use crate::sent::OwnedTask;
use crate::storage::bodies::Place;
use crate::storage::cursor::Sealer;
use crate::storage::{
    BODY_FAILED, Backend, DEFAULT_PAGE, FILE_BYTES, Failure, Guard, Key, ListQuery, Metadata,
    OPERATION_TIME, Opened, PAGE, Page, Position, Read, Source, Storage, Until, is_content_type,
    key as keys,
};

/// One of the five operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Op {
    Head,
    Get,
    Put,
    Delete,
    List,
}

impl Op {
    pub(super) fn of(operation: &str) -> Option<Self> {
        Some(match operation {
            "storage.head" => Self::Head,
            "storage.get" => Self::Get,
            "storage.put" => Self::Put,
            "storage.delete" => Self::Delete,
            "storage.list" => Self::List,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Head => "storage.head",
            Self::Get => "storage.get",
            Self::Put => "storage.put",
            Self::Delete => "storage.delete",
            Self::List => "storage.list",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Get => "get",
            Self::Put => "put",
            Self::Delete => "delete",
            Self::List => "list",
        }
    }

    fn needed(self) -> AccessLevel {
        match self {
            Self::Head | Self::Get | Self::List => AccessLevel::Read,
            Self::Put | Self::Delete => AccessLevel::Write,
        }
    }

    /// Whether the operation opens a body: a place among those open at
    /// once is waited for, and `unavailable` when none is.
    fn opens_a_body(self) -> bool {
        matches!(self, Self::Get | Self::Put)
    }
}

/// A put's body as the raw interface's arguments carry it: what the
/// platform's glue (crate::runner's `prelude.js`) made of what the code
/// passed.
#[derive(Debug)]
pub(super) enum PutBody {
    /// `null`: an empty file.
    Empty,
    /// A string, written as UTF-8.
    Text(String),
    /// A stream the host itself feeds (the request's own body, another
    /// file's), named by its native id.
    Native(u64),
    /// A stream the glue pumps into (the code's own stream, or the bytes
    /// of an `ArrayBuffer` or view it held at once), named by the id of
    /// the stream it made; the body's length when the glue knew it.
    Pumped { id: u64, length: Option<u64> },
    /// No stream of the code's own was free to pump into: the put fails
    /// `unavailable` in its place in the order.
    Busy,
}

/// What an operation asks, its arguments read.
enum Asked {
    /// head, get, delete.
    Key(String),
    Put {
        key: String,
        body: PutBody,
        content_type: Option<String>,
    },
    List {
        prefix: String,
        delimiter: bool,
        limit: usize,
        after: Option<Position>,
    },
}

/// The arguments read.
struct ReadArgs {
    resource: String,
    actor: authz::Actor,
    asked: Asked,
}

const KEY: &str = "`key` must be the file's key, as a string";
const LIMIT: &str = "`limit` must be an integer from 1 to 1000 (100 when not given)";
const BODY_KINDS: &str = "`body` must be a ReadableStream, a string, an ArrayBuffer, an ArrayBufferView, or null (an empty file)";

/// A note the glue leaves in place of a body it could not hand over.
const LOCKED: &str = "`body` is a ReadableStream that is locked: a put takes the stream itself, not a stream another reader holds";
const ALREADY_READ: &str = "`body` is a ReadableStream that has already been read: a put takes a stream nobody has read from";

/// Reads the arguments of `op`, strictly; those it does not have are
/// ignored.
fn read_args(op: Op, args: Value, cursors: &Sealer) -> Result<ReadArgs, &'static str> {
    let Value::Object(mut args) = args else {
        return Err(
            "the arguments must be an object: { resource, as, key, body, content_type } or, for storage.list, { resource, as, prefix, delimiter, limit, cursor }",
        );
    };
    let resource = match args.remove("resource") {
        Some(Value::String(resource)) => resource,
        _ => return Err("`resource` must be the name of a Storage Resource, as a string"),
    };
    if args.contains_key("own") {
        return Err("`own` is not taken here: Storage is a Resource, named by `resource`");
    }
    let actor = match args.get("as") {
        None => authz::Actor::User,
        Some(Value::String(actor)) if actor == "user" => authz::Actor::User,
        Some(Value::String(actor)) if actor == "application" => authz::Actor::Application,
        Some(_) => return Err("`as` must be \"user\" (the default) or \"application\""),
    };
    let asked = match op {
        Op::Head | Op::Get | Op::Delete => Asked::Key(key_of(&mut args)?),
        Op::Put => {
            let key = key_of(&mut args)?;
            let body = match args.remove("body") {
                None => return Err("`body` must be given, or `null` for an empty file"),
                Some(Value::Null) => PutBody::Empty,
                Some(Value::String(text)) => PutBody::Text(text),
                Some(Value::Object(mut marker)) => {
                    // What the glue hands over in place of what JSON
                    // cannot carry: a stream by its id, or a note that
                    // the body was none of the kinds a put takes. A
                    // marker the code wrote itself is answered as the
                    // note it imitates, or refused below when it names
                    // no stream of this request.
                    match (marker.remove("$native"), marker.remove("$pumped")) {
                        (Some(Value::Number(id)), None) => match id.as_u64() {
                            Some(id) => PutBody::Native(id),
                            None => return Err(BODY_KINDS),
                        },
                        (None, Some(Value::Number(id))) => match id.as_u64() {
                            Some(id) => PutBody::Pumped {
                                id,
                                length: marker.remove("$length").and_then(|n| n.as_u64()),
                            },
                            None => return Err(BODY_KINDS),
                        },
                        _ => {
                            if marker.remove("$locked") == Some(Value::Bool(true))
                                && marker.is_empty()
                            {
                                return Err(LOCKED);
                            }
                            if marker.remove("$closed") == Some(Value::Bool(true))
                                && marker.is_empty()
                            {
                                return Err(ALREADY_READ);
                            }
                            if marker.remove("$busy") == Some(Value::Bool(true))
                                && marker.is_empty()
                            {
                                PutBody::Busy
                            } else {
                                return Err(BODY_KINDS);
                            }
                        }
                    }
                }
                Some(_) => return Err(BODY_KINDS),
            };
            let content_type = match args.remove("content_type") {
                None => None,
                Some(Value::String(given)) if is_content_type(&given) => Some(given),
                Some(_) => {
                    return Err(
                        "`content_type` must be 1 to 255 bytes of printable ASCII holding a /, or not given",
                    );
                }
            };
            Asked::Put {
                key,
                body,
                content_type,
            }
        }
        Op::List => {
            let prefix = match args.remove("prefix") {
                None => String::new(),
                Some(Value::String(prefix)) => prefix,
                Some(_) => return Err("`prefix` must be a string, or not given"),
            };
            let delimiter = match args.get("delimiter") {
                None => false,
                Some(Value::String(d)) if d == "/" => true,
                Some(_) => return Err("`delimiter` must be \"/\", or not given"),
            };
            let limit = match args.get("limit") {
                None => DEFAULT_PAGE,
                Some(Value::Number(n)) => match n.as_u64().and_then(|n| usize::try_from(n).ok()) {
                    Some(limit) if (1..=PAGE).contains(&limit) => limit,
                    _ => return Err(LIMIT),
                },
                Some(_) => return Err(LIMIT),
            };
            let after = match args.get("cursor") {
                None => None,
                Some(Value::String(cursor)) => Some(
                    cursors
                        .open(&resource, &prefix, delimiter, cursor)
                        .ok_or(
                            "`cursor` must be a cursor a list of this Resource with this prefix and delimiter answered",
                        )?,
                ),
                Some(_) => return Err("`cursor` must be a cursor a list answered, as a string"),
            };
            Asked::List {
                prefix,
                delimiter,
                limit,
                after,
            }
        }
    };
    Ok(ReadArgs {
        resource,
        actor,
        asked,
    })
}

fn key_of(args: &mut Map<String, Value>) -> Result<String, &'static str> {
    match args.remove("key") {
        Some(Value::String(key)) => Ok(key),
        _ => Err(KEY),
    }
}

/// The entry's statement: the word and the key, or the prefix and the
/// delimiter.
fn statement(op: Op, asked: &Asked) -> String {
    match asked {
        Asked::Key(key) | Asked::Put { key, .. } => {
            format!("{} {}", op.word(), keys::quoted(key))
        }
        Asked::List {
            prefix, delimiter, ..
        } => {
            let mut statement = format!("list {}", keys::quoted(prefix));
            if *delimiter {
                statement.push_str(" delimiter \"/\"");
            }
            statement
        }
    }
}

impl PlatformGateway {
    /// A Storage operation: read, decided and dispatched as every
    /// operation is. The call's guard travels into the work, with its
    /// arguments; a call answered at once here drops it, holding nothing.
    pub(super) fn storage(
        &self,
        op: Op,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: Value,
        held: Option<CallGuard>,
    ) -> Decision {
        let read = match read_args(op, args, &self.storage.cursors) {
            Ok(read) => read,
            // With no request in flight nothing is answered but that.
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => return malformed(problem),
        };
        // A stream the arguments name must be one of this request's — a
        // host-fed stream, or one the glue made — and a name that is not
        // is none of the kinds a put takes: `malformed`, before anything is
        // decided, as the order has it (the code reaching past the glue
        // cannot make the host write from a stream it does not have).
        if active && let Asked::Put { body, .. } = &read.asked {
            let streams = in_flight.streams.get();
            let known = match body {
                PutBody::Native(id) => streams.is_some_and(|streams| streams.is_host_fed(*id)),
                PutBody::Pumped { id, .. } => {
                    streams.is_some_and(|streams| streams.is_guest_stream(*id))
                }
                PutBody::Empty | PutBody::Text(_) | PutBody::Busy => true,
            };
            if !known {
                return malformed(BODY_KINDS);
            }
        }
        let decided = Instant::now();
        let (on, refusal) = self.decide_kind(
            op.needed(),
            read.actor,
            Target::Resource(&read.resource),
            ResourceKind::Storage,
            in_flight,
            active,
        );
        // A put holds its place among the bodies open at once from its
        // decision on, before its first byte ("Storage: Bounds"), and a
        // get takes its own in the order it is decided: taken here,
        // inside the code's call, so that the operations of one request
        // reach the registry in their own order; one that finds none free
        // waits in its task (up to [`WAIT`], ended by the request's end).
        let pre_taken = match (refusal.is_none(), op.opens_a_body()) {
            (true, true) => in_flight.bodies.try_guard(&self.storage.open),
            _ => None,
        };
        // A stream the glue named is the put's from here on, inside the
        // code's call, so the code can neither read it nor race the put
        // for it. Taken only when the put is allowed (a refused operation
        // touches nothing: the stream is dropped, and with it the code's
        // body — the glue had it locked or taken, so the code cannot read
        // it either way).
        let mut taken: Option<mpsc::Receiver<StreamChunk>> = None;
        if refusal.is_none()
            && let Asked::Put { body, .. } = &read.asked
        {
            let streams = in_flight.streams.get();
            match body {
                PutBody::Empty | PutBody::Text(_) | PutBody::Busy => {}
                PutBody::Native(id) => match streams.and_then(|streams| streams.take_receiver(*id))
                {
                    Some(chunks) => taken = Some(chunks),
                    // Another put of this request took it first.
                    None => {
                        return malformed(
                            "`body` is a ReadableStream that was already given to another put of this request",
                        );
                    }
                },
                PutBody::Pumped { id, .. } => {
                    match streams.and_then(|streams| streams.take_receiver(*id)) {
                        Some(chunks) => taken = Some(chunks),
                        None => {
                            return malformed(
                                "`body` is a ReadableStream that was already given to another put of this request",
                            );
                        }
                    }
                }
            }
        }
        let work = StorageWork {
            op,
            in_flight: Arc::clone(in_flight),
            active,
            actor: read.actor,
            target: on,
            statement: statement(op, &read.asked),
            refusal,
            asked: Some(read.asked),
            taken,
            pre_taken,
            _held: held,
            log: Arc::clone(&self.log),
            storage: Arc::clone(&self.storage),
            until: Until::new(decided + OPERATION_TIME),
        };
        if !active {
            return self.refused_unawaited_with(work, StorageWork::run);
        }
        let refused = work.refusal.as_ref().map(|r| r.reason);
        self.dispatch(work, refused, StorageWork::run)
    }
}

/// A Storage operation, decided, to be logged and run.
struct StorageWork {
    op: Op,
    in_flight: Arc<InFlight>,
    active: bool,
    actor: authz::Actor,
    target: On,
    statement: String,
    refusal: Option<Refusal>,
    /// Taken by [`StorageWork::run`].
    asked: Option<Asked>,
    /// A put's body as a stream: the receiver taken in the code's call.
    taken: Option<mpsc::Receiver<StreamChunk>>,
    /// The place among the bodies open at once, taken at the decision
    /// when one was free then.
    pre_taken: Option<Guard>,
    /// The call's place among what its request and the program hold of
    /// calls not yet answered: read by no one, only held — given back
    /// with this work, here or in the task that runs the operation.
    _held: Option<CallGuard>,
    log: Arc<OperationLog>,
    storage: Arc<Storage>,
    /// 10 seconds from the decision: head, get, delete and list.
    until: Until,
}

impl Dispatched for StorageWork {
    fn refusal(&self) -> Option<&Refusal> {
        self.refusal.as_ref()
    }

    fn on(&self) -> &On {
        &self.target
    }

    fn busy(&self) -> Value {
        log::record(
            "operations-busy",
            json!({
                "application": &*self.in_flight.application,
                "resource": self.resource_name(),
                "own_database": false,
            }),
        );
        match &self.refusal {
            Some(refusal) => refusal.to_wire(),
            None => unavailable(),
        }
    }
}

/// What a backend's operation answered, by operation (a put is answered
/// in its own right, in [`StorageWork::run_put`]).
enum Done {
    Head(Option<Metadata>),
    Get(Option<Opened>),
    Delete,
    /// The page, and its cursor sealed.
    List(Page, Option<String>),
}

/// A put, its key taken and its body as the interface takes it.
struct PutWork {
    key: Key,
    source: Source,
    content_type: Option<String>,
}

/// What an operation asks once its key is taken.
enum Checked {
    Head(Key),
    Get(Key),
    Delete(Key),
    Put(PutWork),
    List(ListQuery),
}

fn unavailable() -> Value {
    error_to_wire(json!({ "kind": "unavailable" }))
}

fn limit(which: &str) -> Value {
    error_to_wire(json!({ "kind": "limit", "limit": which }))
}

fn body_failed() -> Value {
    error_to_wire(json!({ "kind": "body-failed" }))
}

/// A file's metadata as every answer gives it.
fn metadata_json(metadata: &Metadata) -> Map<String, Value> {
    let mut object = Map::with_capacity(5);
    object.insert("key".into(), metadata.key.as_str().into());
    object.insert("size".into(), metadata.size.into());
    object.insert("content_type".into(), metadata.content_type.as_str().into());
    object.insert("last_modified".into(), rfc3339(metadata.modified).into());
    object
}

impl StorageWork {
    fn resource_name(&self) -> Option<&str> {
        match &self.target {
            On::Own => None,
            On::Resource { name, .. } => Some(name),
        }
    }

    fn entry(&self) -> Entry<'_> {
        Entry {
            request: self.active.then_some(&*self.in_flight.request),
            user: self.active.then_some(self.in_flight.user.id.as_str()),
            application: &self.in_flight.application,
            version: self.in_flight.version,
            resource: self.resource_name(),
            actor: actor_name(self.actor),
            operation: self.op.name(),
            statement: Some(&self.statement),
            transaction: None,
            host: None,
        }
    }

    /// An allowed operation that ends before the directory is touched: its
    /// whole entry, `failed` with `error`, and `answer`.
    async fn failed_unsent(&self, error: &str, answer: Value) -> Value {
        match self.log.failed(&self.entry(), error).await {
            Ok(()) => answer,
            Err(failure) => {
                self.unlogged(&failure);
                Refusal::log_unavailable(self.resource_name()).to_wire()
            }
        }
    }

    fn unlogged(&self, error: &crate::platform_db::DbError) {
        log::record(
            "operation-log-failed",
            json!({
                "application": &*self.in_flight.application,
                "resource": self.resource_name(),
                "own_database": false,
                "error": error.to_string(),
            }),
        );
    }

    async fn finish(&self, id: i64, ending: &Ending<'_>) {
        if let Err(error) = self.log.finish(id, ending).await {
            self.unlogged(&error);
        }
    }

    async fn run(mut self) -> Value {
        if let Some(refusal) = &self.refusal {
            if let Err(error) = self.log.refused(&self.entry(), refusal.reason).await {
                self.unlogged(&error);
            }
            return refusal.to_wire();
        }
        let resource = match &self.target {
            On::Resource {
                resource: Some(resource),
                ..
            } => Arc::clone(resource),
            // Allowed means registered as Storage: not reached.
            On::Resource { resource: None, .. } | On::Own => {
                return self.failed_unsent("unavailable", unavailable()).await;
            }
        };
        let Some(asked) = self.asked.take() else {
            return self.failed_unsent("unavailable", unavailable()).await;
        };
        // The key's rules, then a put's bound and its stream: before the
        // entry is begun, with nothing touched.
        let checked = match self.check(asked) {
            Ok(checked) => checked,
            Err((error, answer)) => return self.failed_unsent(error, answer).await,
        };
        let id = match self.log.begin(&self.entry()).await {
            Ok(id) => id,
            Err(error) => {
                self.unlogged(&error);
                return Refusal::log_unavailable(self.resource_name()).to_wire();
            }
        };
        // Handed to the directory: `duration_ms` runs from here, the
        // waits for a place and for a file thread included.
        let started = tokio::time::Instant::now();
        let elapsed = || Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        // An operation that opens a body holds the place it took at its
        // decision, or waits for one among those open at once, up to
        // [`WAIT`], then fails `unavailable` and touches nothing — and is
        // given up at once when its request ends or its Application is
        // disabled meanwhile.
        let guard = match self.op.opens_a_body() {
            false => None,
            true => match self.pre_taken.take() {
                Some(guard) => Some(Arc::new(guard)),
                None => match self.in_flight.bodies.guard(&self.storage.open).await {
                    Place::Made(guard) => Some(Arc::new(guard)),
                    Place::Ended => {
                        let error = ended_why(&self.in_flight);
                        self.finish(id, &failed_ending(error, elapsed())).await;
                        return if self.in_flight.stop.is_stopped() {
                            Refusal::application_disabled().to_wire()
                        } else {
                            unavailable()
                        };
                    }
                    Place::Waited => {
                        self.finish(id, &failed_ending("unavailable", elapsed()))
                            .await;
                        return unavailable();
                    }
                },
            },
        };
        match checked {
            Checked::Put(put) => self.run_put(resource, put, id, elapsed, guard).await,
            checked => {
                let Some(backend) = resource.storage() else {
                    self.finish(id, &failed_ending("unavailable", elapsed()))
                        .await;
                    return unavailable();
                };
                self.run_open(&backend, checked, id, elapsed, guard).await
            }
        }
    }

    /// The key's rules and a put's bound: `Err` is a failure before
    /// anything is touched, with its entry's `error` and its answer.
    fn check(&mut self, asked: Asked) -> Result<Checked, (&'static str, Value)> {
        let invalid = |rule: keys::Rule| {
            (
                "invalid-key",
                error_to_wire(json!({
                    "kind": "invalid-key",
                    "rule": rule.name(),
                    "message": rule.message(),
                })),
            )
        };
        match (self.op, asked) {
            (op, Asked::Key(key)) => match Key::new(key) {
                Ok(key) => Ok(match op {
                    Op::Get => Checked::Get(key),
                    Op::Delete => Checked::Delete(key),
                    _ => Checked::Head(key),
                }),
                Err(rule) => Err(invalid(rule)),
            },
            (
                _,
                Asked::Put {
                    key,
                    body,
                    content_type,
                },
            ) => match Key::new(key) {
                Err(rule) => Err(invalid(rule)),
                Ok(key) => {
                    // A put whose body was none of the kinds the glue
                    // could hand over, and one that could not make the
                    // stream it pumps into.
                    let (known, source) = match body {
                        PutBody::Busy => return Err(("unavailable", unavailable())),
                        PutBody::Empty => (None, Source::Whole(Bytes::new())),
                        PutBody::Text(text) => {
                            let known = u64::try_from(text.len()).unwrap_or(u64::MAX);
                            (Some(known), Source::Whole(Bytes::from(text)))
                        }
                        PutBody::Native(id) => {
                            // The request's own body when it stated a
                            // `content-length` is the one known length a
                            // native stream can have; a file's is never
                            // past the bound (the get that opened it
                            // refused to).
                            let known = self.request_body_length(id);
                            let Some(chunks) = self.taken.take() else {
                                return Err(("unavailable", unavailable()));
                            };
                            (
                                known,
                                Source::Stream {
                                    chunks,
                                    length: None,
                                },
                            )
                        }
                        PutBody::Pumped { id: _, length } => {
                            let Some(chunks) = self.taken.take() else {
                                return Err(("unavailable", unavailable()));
                            };
                            (length, Source::Stream { chunks, length })
                        }
                    };
                    // The file bound, before a byte is read, for a body
                    // whose length is known.
                    if known.is_some_and(|known| known > FILE_BYTES) {
                        return Err(("bytes", limit("bytes")));
                    }
                    Ok(Checked::Put(PutWork {
                        key,
                        source,
                        content_type,
                    }))
                }
            },
            (
                _,
                Asked::List {
                    prefix,
                    delimiter,
                    limit,
                    after,
                },
            ) => match keys::check_prefix(&prefix) {
                Ok(()) => Ok(Checked::List(ListQuery {
                    prefix,
                    delimiter,
                    after,
                    limit,
                })),
                Err(rule) => Err(invalid(rule)),
            },
        }
    }

    /// The length of the request's own body when it stated one, when
    /// `id` is that body's stream; a file's body has none that the put
    /// must check (the get that opened it already held the bound).
    fn request_body_length(&self, id: u64) -> Option<u64> {
        if self
            .in_flight
            .streams
            .get()
            .is_some_and(|streams| streams.request_body() == Some(id))
        {
            self.in_flight.content_length
        } else {
            None
        }
    }

    /// Head, get, delete or list: the backend's operation within 10
    /// seconds of the decision, given up if the Application is disabled
    /// meanwhile. A get's entry ends when the file is open; its body is
    /// fed afterwards.
    async fn run_open(
        self,
        backend: &dyn Backend,
        checked: Checked,
        id: i64,
        elapsed: impl Fn() -> Option<u64>,
        guard: Option<Arc<Guard>>,
    ) -> Value {
        let deadline = tokio::time::Instant::from_std(self.until.deadline());
        let stop = Arc::clone(&self.in_flight.stop);
        let active = self.active;
        let until = self.until.clone();
        let stopped = async move {
            if active {
                stop.stopped().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let call = self.call(backend, checked, until);
        let ran = tokio::select! {
            biased;
            () = stopped => None,
            done = tokio::time::timeout_at(deadline, call) => Some(done.unwrap_or(Err(Failure::Time))),
        };
        let duration_ms = elapsed();
        let Some(done) = ran else {
            // Disabled meanwhile: given up.
            self.until.give_up();
            self.finish(id, &failed_ending(APPLICATION_DISABLED, duration_ms))
                .await;
            return Refusal::application_disabled().to_wire();
        };
        match done {
            Ok(Done::Get(Some(opened))) => {
                // The file is open: the entry ends here, however much of
                // the body is read afterwards. The body is a stream the
                // host feeds, a chunk a read job on the file threads, its
                // place among the bodies open at once held by the pump
                // that feeds it — given back when it was read to its end
                // or cancelled, at the request's end, or, for a body
                // given back unread, when its sending ends.
                let Opened { metadata, reader } = opened;
                let ended = Ending {
                    outcome: "allowed",
                    error: None,
                    rows: Some(1),
                    duration_ms,
                    began_nothing: false,
                };
                self.finish(id, &ended).await;
                // A get always opened a body and so always holds a place;
                // one without is answered `unavailable` rather than with a
                // body nobody holds.
                let Some(guard) = guard else {
                    return unavailable();
                };
                match self.answer_of_open_body(metadata, reader, guard).await {
                    Some(answer) => answer,
                    // No context to feed a stream through, which cannot
                    // happen while a request is in flight.
                    None => unavailable(),
                }
            }
            Ok(done) => {
                let (rows, answer) = Self::answer(done);
                let ending = Ending {
                    outcome: "allowed",
                    error: None,
                    rows,
                    duration_ms,
                    began_nothing: false,
                };
                self.finish(id, &ending).await;
                answer
            }
            Err(failure) => {
                let (error, answer) = failed(failure);
                self.finish(id, &failed_ending(error, duration_ms)).await;
                answer
            }
        }
    }

    /// A get's answer: the file's metadata and its body as the stream the
    /// host feeds, and the pump that feeds it, owned by the request's
    /// registry of open bodies (a body given back unread is the
    /// response's: the runner takes the pump over to send it). `None`
    /// only with no context to feed a stream through, which cannot
    /// happen while a request is in flight.
    async fn answer_of_open_body(
        &self,
        metadata: Metadata,
        reader: std::sync::Arc<dyn crate::storage::Reader>,
        guard: Arc<Guard>,
    ) -> Option<Value> {
        let streams = self.in_flight.streams.get()?;
        let id = streams.create_stream("storage get".to_owned());
        streams.mark_host_fed(id);
        let pump = body_pump(
            reader,
            Arc::clone(streams),
            id,
            guard,
            Arc::clone(&self.in_flight.bodies),
        );
        // The size with the pump: the `content-length` the platform states
        // for a body it sends itself.
        self.in_flight.bodies.hold(id, pump, Some(metadata.size));
        let mut object = metadata_json(&metadata);
        // The glue turns the marker into the ReadableStream it names.
        object.insert("body".into(), json!({ "$stream": id }));
        Some(ok_to_wire(Value::Object(object)))
    }

    /// A put: part of its request. The backend's put runs in a task of its
    /// own, so that giving it up (the request's end, a disable) lets the
    /// put end its own body — staging removed, nothing landed — instead of
    /// being cut off mid-flight; the entry is completed `failed` with
    /// `request-ended` (or `application-disabled`) at once, and the task
    /// is reaped after its own end, its answer landing nowhere.
    async fn run_put(
        self,
        resource: Arc<crate::resources::Resource>,
        put: PutWork,
        id: i64,
        elapsed: impl Fn() -> Option<u64>,
        guard: Option<Arc<Guard>>,
    ) -> Value {
        let given_up = self.in_flight.bodies.given_up();
        let (answer_tx, answer_rx) = oneshot::channel();
        // The put's place is the task's too: a put given up still holds its
        // staging file until it has ended its body, and it counts among the
        // files open at once until then.
        let place = guard.as_ref().map(Arc::clone);
        let task = tokio::spawn(async move {
            let Some(backend) = resource.storage() else {
                return;
            };
            // The put's own end removes its staging file on every failure
            // (crate::storage::directory), so this task needs no answer
            // waited for to clean up after it.
            let PutWork {
                key,
                source,
                content_type,
            } = put;
            let answer = backend.put(key, source, content_type, given_up).await;
            // Given back before the answer goes: the code's next operation
            // finds the place free.
            drop(place);
            drop(answer_tx.send(answer));
        });
        // The request's registry owns the task from here on, so that it is
        // ended in every case however this work ends — including this
        // future being dropped (the program stopping, the task cancelled):
        // the registry gives a put whose request ended its bound to remove
        // its staging file and ends it after.
        let Some(put_id) = self
            .in_flight
            .bodies
            .own_put(crate::sent::OwnedTask::new(task))
        else {
            // The request was over before the put started: it never does.
            let error = ended_why(&self.in_flight);
            self.finish(id, &failed_ending(error, elapsed())).await;
            drop(guard);
            return unavailable();
        };
        let ended = self.in_flight.bodies.ended();
        let mut answer_rx = answer_rx;
        let answer = tokio::select! {
            biased;
            () = ended => None,
            answer = &mut answer_rx => Some(answer),
        };
        // A put that had landed and answered when the end came is not given
        // up: its file is there, and its entry says so. (The end and the
        // answer can both be ready in one poll, and the end is looked at
        // first.) Any other answer already there is the put given up.
        let answer = match answer {
            Some(answer) => Some(answer),
            None => match answer_rx.try_recv() {
                Ok(Ok(metadata)) => Some(Ok(Ok(metadata))),
                _ => None,
            },
        };
        let duration_ms = elapsed();
        let Some(answer) = answer else {
            // The request ended (its own time, or a disable): the put is
            // given up, exactly as a statement still running then is
            // cancelled. Nothing lands, nothing is left in staging: the
            // task sees `given_up`, ends its body and removes its staging
            // file (or, if it cannot within the bound, is ended by the
            // registry and what is left is what the Resource's next load
            // clears).
            let error = ended_why(&self.in_flight);
            self.finish(id, &failed_ending(error, duration_ms)).await;
            drop(guard);
            return unavailable();
        };
        // The task ended (its answer is here): the registry lets it go.
        self.in_flight.bodies.release_put(put_id);
        drop(guard);
        match answer {
            Ok(Ok(metadata)) => {
                let ending = Ending {
                    outcome: "allowed",
                    error: None,
                    rows: Some(1),
                    duration_ms,
                    began_nothing: false,
                };
                self.finish(id, &ending).await;
                ok_to_wire(Value::Object(metadata_json(&metadata)))
            }
            // The backend's `Stopped` is the put given up: its body saw
            // the request's end (or the Application's disable) and ended
            // without landing. That answer can be ready in the same poll
            // in which the end is set, before the `ended` arm above is
            // seen, though the request is over; the entry says what ended
            // it, whichever arm of the select came first.
            Ok(Err(Failure::Stopped)) => {
                let error = ended_why(&self.in_flight);
                self.finish(id, &failed_ending(error, duration_ms)).await;
                unavailable()
            }
            Ok(Err(failure)) => {
                let (error, answer) = failed(failure);
                self.finish(id, &failed_ending(error, duration_ms)).await;
                answer
            }
            // The put's task ended without its answer: aborted at the
            // request's end before it could send (its owner ends it),
            // which is the put given up. Any other way of ending
            // without an answer is its own code's, which cannot.
            Err(_) => {
                let error = if self.in_flight.bodies.is_ended() {
                    ended_why(&self.in_flight)
                } else {
                    "unavailable"
                };
                self.finish(id, &failed_ending(error, duration_ms)).await;
                unavailable()
            }
        }
    }

    /// The backend's operation (head, get, delete, list).
    async fn call(
        &self,
        backend: &dyn Backend,
        checked: Checked,
        until: Until,
    ) -> Result<Done, Failure> {
        match checked {
            Checked::Head(key) => backend.head(key, until).await.map(Done::Head),
            Checked::Get(key) => backend.get(key, until).await.map(Done::Get),
            Checked::Delete(key) => backend.delete(key, until).await.map(|()| Done::Delete),
            Checked::List(query) => {
                let prefix = query.prefix.clone();
                let delimiter = query.delimiter;
                let page = backend.list(query, until).await?;
                // Sealed for this Resource, prefix and delimiter.
                let cursor = page.next.as_ref().map(|next| {
                    self.storage.cursors.seal(
                        self.resource_name().unwrap_or_default(),
                        &prefix,
                        delimiter,
                        next,
                    )
                });
                Ok(Done::List(page, cursor))
            }
            // A put is run by [`StorageWork::run_put`].
            Checked::Put { .. } => Err(Failure::Unavailable),
        }
    }

    /// The answer and the entry's `rows`, for an operation that opens no
    /// body (a get that answered a file is answered in
    /// [`StorageWork::run_open`], its body a stream).
    fn answer(done: Done) -> (Option<u64>, Value) {
        match done {
            Done::Head(None) | Done::Get(None) => (Some(0), ok_to_wire(Value::Null)),
            Done::Head(Some(metadata)) => {
                (Some(1), ok_to_wire(Value::Object(metadata_json(&metadata))))
            }
            Done::Get(Some(_)) => {
                // Not reached: a get that answered a file is answered in
                // [`StorageWork::run_open`], with its body as a stream.
                (Some(1), unavailable())
            }
            Done::Delete => (None, ok_to_wire(json!({}))),
            Done::List(page, cursor) => {
                let count = page.files.len() + page.folders.len();
                let files: Vec<Value> = page
                    .files
                    .iter()
                    .map(|m| Value::Object(metadata_json(m)))
                    .collect();
                (
                    u64::try_from(count).ok(),
                    ok_to_wire(json!({
                        "files": files,
                        "folders": page.folders,
                        "cursor": cursor,
                    })),
                )
            }
        }
    }
}

/// Feeds a get's opened file into the stream the code reads: a chunk a
/// read job on the file threads, the channel bounded by the runtime, so
/// the file is read only as fast as the code (or the response sending it)
/// takes its bytes. Owned by the request's registry of open bodies (or,
/// for a body given back unread, by the response that sends it, once the
/// runner takes it over); the place the body holds among those open at
/// once is given back with this task's end, and so is its entry in the
/// registry and the runtime's own record of the stream — a body that has
/// ended leaves nothing for its id.
fn body_pump(
    reader: std::sync::Arc<dyn crate::storage::Reader>,
    streams: Arc<StreamManager>,
    id: StreamId,
    guard: Arc<Guard>,
    bodies: Arc<crate::storage::bodies::Bodies>,
) -> OwnedTask {
    OwnedTask::new(tokio::spawn(async move {
        loop {
            let chunk = match reader.read().await {
                // The file's exact end.
                Ok(Read::End) => {
                    drop(streams.write_chunk(id, StreamChunk::Done).await);
                    break;
                }
                Ok(Read::Chunk(bytes)) => bytes,
                // The file was found shorter while it was read, or could
                // not be read: the stream fails, never ended as if whole.
                Ok(Read::Failed) | Err(_) => {
                    drop(
                        streams
                            .write_chunk(id, StreamChunk::Error(BODY_FAILED.to_owned()))
                            .await,
                    );
                    break;
                }
            };
            // Waits for room when the channel is full (its owner ends it
            // when the request does, or the response when its sending
            // does; no timeout of its own is needed or wanted).
            if streams
                .write_chunk(id, StreamChunk::Data(chunk))
                .await
                .is_err()
            {
                // The stream was closed: cancelled, or the request over.
                break;
            }
        }
        // The body is whole in the stream. Its place is given back when
        // it was read to its end — the stream's channel, all of it, free
        // again — or at once when the stream is closed here (cancelled,
        // or the request over): the wait on every place of the channel
        // ends either way. A body being sent empties as the client (or
        // the response sending it) takes it, so its place ends with the
        // sending (blackbox/CONTRACT.md, "Storage: Bounds").
        if let Some(sender) = streams.sender(id) {
            drop(sender.reserve_many(sender.max_capacity()).await);
        }
        // The stream is over for good: its sender, its end, its marks and
        // its metadata go (a request that gets a thousand small files
        // holds nothing in the runtime for the thousand it finished), and
        // its entry goes from the request's registry.
        streams.close_stream(id);
        bodies.release(id);
        // The place the body holds is given back with this task's end.
        drop(guard);
    }))
}

fn failed_ending(error: &str, duration_ms: Option<u64>) -> Ending<'_> {
    Ending {
        outcome: "failed",
        error: Some(error),
        rows: None,
        duration_ms,
        began_nothing: false,
    }
}

/// A failure's entry `error` and its answer.
fn failed(failure: Failure) -> (&'static str, Value) {
    match failure {
        Failure::Bytes => ("bytes", limit("bytes")),
        Failure::Time => ("time", limit("time")),
        Failure::BodyFailed => ("body-failed", body_failed()),
        Failure::KeyConflict => (
            "key-conflict",
            error_to_wire(json!({
                "kind": "key-conflict",
                "message": "a file and a folder of the same name cannot both exist: a file is a folder of this key, or this key is a folder of other files",
            })),
        ),
        Failure::NotReachable(why) => (
            "not-reachable",
            error_to_wire(json!({ "kind": "not-reachable", "reason": why.name() })),
        ),
        Failure::Full => (
            "full",
            error_to_wire(json!({
                "kind": "full",
                "message": "the disk or the quota of the Storage Resource is full: ask the Resource's contact to make room",
            })),
        ),
        Failure::Unavailable | Failure::Stopped => ("unavailable", unavailable()),
    }
}
