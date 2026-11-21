//! The five Storage operations (blackbox/CONTRACT.md, "Storage: Through the
//! raw interface"), as the host runs them, in the contract's order:
//!
//! 1. the arguments are read ([`read`]): malformed is answered so, with
//!    nothing logged or touched; a put's `body` is looked at, not decoded,
//!    and a text longer than a body of the bound encodes to is not looked
//!    at (it is past the bound); a list's `cursor` is opened with the key
//!    the platform database keeps (crate::storage::cursor);
//! 2. the Authorization decision, against Resources of kind `storage` only
//!    ([`PlatformGateway::decide_kind`]);
//! 3. in a task of its own (as every operation): a refusal's entry; then
//!    the key's rules (`invalid-key`, a whole `failed` entry, nothing
//!    touched), a put's body past the bound (`limit` `bytes`, the same);
//!    then the entry written `unfinished` (`log-unavailable` if it cannot
//!    be); then, for a put, room among the bodies held at once; then the
//!    backend's operation, within 10 seconds of the decision, given up if
//!    the Application is disabled meanwhile; and the entry completed.
//!
//! The statement written in the entry is the operation's word and its key
//! (crate::storage::key::quoted); no entry holds a file's contents, content
//! type, size or the cursor.

use std::sync::Arc;
use std::time::Instant;

use authz::AccessLevel;
use serde_json::{Map, Value, json};

use super::{
    APPLICATION_DISABLED, Decision, Dispatched, On, PlatformGateway, Refusal, Target, actor_name,
    error_to_wire, malformed, ok_to_wire,
};
use crate::log;
use crate::oplog::{Ending, Entry, OperationLog};
use crate::own_tls::rfc3339;
use crate::registry::ResourceKind;
use crate::runner::InFlight;
use crate::storage::cursor::Sealer;
use crate::storage::{
    BODY_BYTES, Backend, Body, BodyText, DEFAULT_PAGE, Failure, Got, Key, ListQuery, Metadata,
    OPERATION_TIME, PAGE, Page, Position, Storage, Until, is_content_type, key as keys,
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
}

/// What an operation asks, its arguments read.
enum Asked {
    /// head, get, delete.
    Key(String),
    Put {
        key: String,
        body: BodyText,
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
struct Read {
    resource: String,
    actor: authz::Actor,
    asked: Asked,
}

const KEY: &str = "`key` must be the file's key, as a string";
const LIMIT: &str = "`limit` must be an integer from 1 to 1000 (100 when not given)";

/// Reads the arguments of `op`, strictly; those it does not have are
/// ignored. Moved, not copied: a put's body is 11 MB of text at most.
fn read(op: Op, args: Value, cursors: &Sealer) -> Result<Read, &'static str> {
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
                Some(Value::String(text)) => Body::read(text)?,
                _ => {
                    return Err(
                        "`body` must be the bytes in standard base64 (with or without = padding), as a string",
                    );
                }
            };
            let content_type = match args.remove("content_type") {
                None | Some(Value::Null) => None,
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
                None | Some(Value::Null) => String::new(),
                Some(Value::String(prefix)) => prefix,
                Some(_) => return Err("`prefix` must be a string, or not given"),
            };
            let delimiter = match args.get("delimiter") {
                None | Some(Value::Null) => false,
                Some(Value::String(d)) if d == "/" => true,
                Some(_) => return Err("`delimiter` must be \"/\", or not given"),
            };
            let limit = match args.get("limit") {
                None | Some(Value::Null) => DEFAULT_PAGE,
                Some(Value::Number(n)) => match n.as_u64().and_then(|n| usize::try_from(n).ok()) {
                    Some(limit) if (1..=PAGE).contains(&limit) => limit,
                    _ => return Err(LIMIT),
                },
                Some(_) => return Err(LIMIT),
            };
            let after = match args.get("cursor") {
                None | Some(Value::Null) => None,
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
    Ok(Read {
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
    /// operation is.
    pub(super) fn storage(
        &self,
        op: Op,
        in_flight: &Arc<InFlight>,
        active: bool,
        args: Value,
    ) -> Decision {
        let read = match read(op, args, &self.storage.cursors) {
            Ok(read) => read,
            // With no request in flight nothing is answered but that.
            Err(_) if !active => return Decision::refused(&Refusal::no_request()),
            Err(problem) => return malformed(problem),
        };
        let decided = Instant::now();
        let (on, refusal) = self.decide_kind(
            op.needed(),
            read.actor,
            Target::Resource(&read.resource),
            ResourceKind::Storage,
            in_flight,
            active,
        );
        let work = StorageWork {
            op,
            in_flight: Arc::clone(in_flight),
            active,
            actor: read.actor,
            target: on,
            statement: statement(op, &read.asked),
            refusal,
            asked: Some(read.asked),
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
    log: Arc<OperationLog>,
    storage: Arc<Storage>,
    /// 10 seconds from the decision.
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

/// What a backend's operation answered, by operation.
enum Done {
    Head(Option<Metadata>),
    Get(Option<Got>),
    Put(Metadata),
    Delete,
    /// The page, and its cursor sealed.
    List(Page, Option<String>),
}

/// What an operation asks once its key is taken.
enum Checked {
    Head(Key),
    Get(Key),
    Delete(Key),
    Put {
        key: Key,
        body: Body,
        content_type: Option<String>,
    },
    List(ListQuery),
}

fn unavailable() -> Value {
    error_to_wire(json!({ "kind": "unavailable" }))
}

fn limit(which: &str) -> Value {
    error_to_wire(json!({ "kind": "limit", "limit": which }))
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
            statement: &self.statement,
            transaction: None,
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
        let Some(backend) = resource.storage() else {
            return self.failed_unsent("unavailable", unavailable()).await;
        };
        let Some(asked) = self.asked.take() else {
            return self.failed_unsent("unavailable", unavailable()).await;
        };
        // The key's rules, then a put's bound: before the entry is begun,
        // with nothing touched.
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
        // Room for a put's body among the bodies held at once (a get takes
        // its own once it knows the file's size).
        let _room = match &checked {
            Checked::Put { body, .. } => match self.storage.bodies.take(body.len()).await {
                Some(room) => Some(room),
                None => {
                    log::record(
                        "storage-busy",
                        json!({ "problem": "no room for the body among those held at once" }),
                    );
                    let ending = failed_ending("unavailable", None);
                    self.finish(id, &ending).await;
                    return unavailable();
                }
            },
            _ => None,
        };
        let started = tokio::time::Instant::now();
        let deadline = tokio::time::Instant::from_std(self.until.deadline());
        let stop = Arc::clone(&self.in_flight.stop);
        let active = self.active;
        let stopped = async move {
            if active {
                stop.stopped().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let ran = tokio::select! {
            biased;
            () = stopped => None,
            done = tokio::time::timeout_at(deadline, self.call(backend, checked)) => {
                Some(done.unwrap_or(Err(Failure::Time)))
            }
        };
        let duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let Some(done) = ran else {
            // Disabled meanwhile: given up before it lands.
            self.until.give_up();
            self.finish(id, &failed_ending(APPLICATION_DISABLED, None))
                .await;
            return Refusal::application_disabled().to_wire();
        };
        match done {
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

    /// The key's rules and a put's bound: `Err` is a failure before
    /// anything is touched, with its entry's `error` and its answer.
    fn check(&self, asked: Asked) -> Result<Checked, (&'static str, Value)> {
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
                Ok(key) => match body {
                    BodyText::Within(body) if body.len() <= BODY_BYTES => Ok(Checked::Put {
                        key,
                        body,
                        content_type,
                    }),
                    BodyText::Within(_) | BodyText::Past => Err(("bytes", limit("bytes"))),
                },
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

    /// The backend's operation.
    async fn call(&self, backend: &dyn Backend, checked: Checked) -> Result<Done, Failure> {
        let until = self.until.clone();
        match checked {
            Checked::Head(key) => backend.head(key, until).await.map(Done::Head),
            Checked::Get(key) => backend
                .get(key, Arc::clone(&self.storage.bodies), until)
                .await
                .map(Done::Get),
            Checked::Put {
                key,
                body,
                content_type,
            } => backend
                .put(key, body, content_type, until)
                .await
                .map(Done::Put),
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
        }
    }

    /// The answer and the entry's `rows`.
    fn answer(done: Done) -> (Option<u64>, Value) {
        match done {
            Done::Head(None) | Done::Get(None) => (Some(0), ok_to_wire(Value::Null)),
            Done::Head(Some(metadata)) => {
                (Some(1), ok_to_wire(Value::Object(metadata_json(&metadata))))
            }
            Done::Get(Some(got)) => {
                let Got { metadata, body, .. } = got;
                let mut object = metadata_json(&metadata);
                // Moved in: the body's text is not copied.
                object.insert("body".into(), Value::String(body));
                (Some(1), ok_to_wire(Value::Object(object)))
            }
            Done::Put(metadata) => (Some(1), ok_to_wire(Value::Object(metadata_json(&metadata)))),
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
