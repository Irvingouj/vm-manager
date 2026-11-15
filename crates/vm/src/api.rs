//! The platform's API, at `api.<domain>` (blackbox/CONTRACT.md, "The
//! platform's API"): HTTP in, calls on the Application registry, JSON out.
//!
//! Interface: [`Api::handle`], for a request whose caller Ingress has
//! already signed in. It reads the body (up to [`MAX_BODY_BYTES`]), finds
//! the route, and answers. Refusals are decided in the contract's order:
//! `too-large`, `not-an-administrator`, `not-found`, `method-not-allowed`,
//! `not-a-contributor`, `malformed`, then the rest. Adding an endpoint is a
//! [`Route`] and a handler.
//!
//! Tool credentials are made and ended here, and `GET /v1/me` says who is
//! signed in; whether a request was signed in with a tool credential comes
//! from Ingress ([`SignedInWith`]).
//!
//! Resources, Grants and the Operation log are here too. A Platform
//! administrator is a member of the Group named at start
//! (`--administrators-group`); without one, no one is. A body that registers
//! a Resource holds a password, so no message about it repeats anything
//! from it: its problems are told in fixed words.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::{Method, StatusCode};
use serde::de::{self, DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::app_logs::{self, Logs};
use crate::log;
use crate::oplog::{self, Ending, OperationLog};
use crate::own_db::Migration;
use crate::platform_db::DbError;
use crate::registry::{
    ApplicationRecord, Created, DeclaredAccess, NameProblem, NewVersion, Registry, Resource, Sent,
    VersionRecord, WentBack, check_name,
};
use crate::resources::{
    self, Connection, GrantWritten, Login, NewResource, Registered, Resources, is_resource_name,
    level_of,
};
use crate::structure;
use crate::tool_credentials::{Digest, MakeError, ToolCredentials};
use crate::user::User;

/// How the request's User was signed in.
#[derive(Clone, Copy, Debug)]
pub enum SignedInWith {
    /// With a tool credential, named by its digest.
    ToolCredential(Digest),
    /// Otherwise (for now: the stand-in).
    Otherwise,
}

/// The largest request body the API reads; past it the answer is `413`.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The longest a client may take to send a request's body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// An answer of the API: a status and a JSON body.
pub struct Answer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Answer {
    /// `value` as JSON, written straight from what it borrows.
    fn json(status: StatusCode, value: &impl Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self {
                status,
                body: Bytes::from(body),
            },
            // Only a map with keys that are not strings fails, and the
            // views below have none.
            Err(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: Bytes::from_static(
                    br#"{"error":{"reason":"internal","message":"the answer could not be written"}}"#,
                ),
            },
        }
    }

    fn error(status: StatusCode, reason: &str, message: &str) -> Self {
        Self::json(
            status,
            &json!({ "error": { "reason": reason, "message": message } }),
        )
    }

    fn not_found(message: &str) -> Self {
        Self::error(StatusCode::NOT_FOUND, "not-found", message)
    }

    fn method_not_allowed(method: &Method, allowed: &str) -> Self {
        Self::error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method-not-allowed",
            &format!("this path does not take {method}: use {allowed}"),
        )
    }

    fn malformed(problem: &str, form: &str) -> Self {
        Self::error(
            StatusCode::BAD_REQUEST,
            "malformed",
            &format!("the body is not the JSON asked for ({problem}): send {form}"),
        )
    }

    pub(crate) fn unavailable(error: &DbError) -> Self {
        log::record(
            "platform-database-failed",
            json!({ "error": error.to_string() }),
        );
        Self::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the platform could not reach its database: try again shortly",
        )
    }
}

/// The paths the API has.
enum Route<'a> {
    /// `/v1/applications`
    Applications,
    /// `/v1/applications/<name>`
    Application(&'a str),
    /// `/v1/applications/<name>/versions`
    Versions(&'a str),
    /// `/v1/applications/<name>/versions/<number>`; `None` when the number
    /// cannot name a Version.
    Version(&'a str, Option<u64>),
    /// `/v1/applications/<name>/go-back`
    GoBack(&'a str),
    /// `/v1/applications/<name>/logs`
    Logs(&'a str),
    /// `/v1/resources`
    Resources,
    /// `/v1/resources/<name>`
    Resource(&'a str),
    /// `/v1/resources/<name>/grants`
    Grants(&'a str),
    /// `/v1/resources/<name>/structure`
    Structure(&'a str),
    /// `/v1/operation-log`
    OperationLog,
    /// `/v1/me`
    Me,
    /// `/v1/tool-credentials`
    ToolCredentials,
    /// `/v1/tool-credentials/current`
    CurrentToolCredential,
}

impl Route<'_> {
    /// Only Platform administrators may use it, whatever the method.
    fn for_administrators(&self, method: &Method) -> bool {
        match self {
            Self::Resources => method != Method::GET,
            Self::Grants(_) | Self::OperationLog => true,
            _ => false,
        }
    }
}

impl<'a> Route<'a> {
    /// The route of a path, or `None` for a path the API does not have.
    /// The path is matched as sent: nothing is decoded or normalised, so
    /// `..`, `%2F` or an empty segment never reach another route.
    fn of(path: &'a str) -> Option<Self> {
        let rest = path.strip_prefix("/v1/")?;
        let segments: Vec<&str> = rest.split('/').collect();
        match segments.as_slice() {
            ["applications"] => Some(Self::Applications),
            ["applications", name] => Some(Self::Application(name)),
            ["applications", name, "versions"] => Some(Self::Versions(name)),
            ["applications", name, "versions", number] => {
                Some(Self::Version(name, version_number(number)))
            }
            ["applications", name, "go-back"] => Some(Self::GoBack(name)),
            ["applications", name, "logs"] => Some(Self::Logs(name)),
            ["resources"] => Some(Self::Resources),
            ["resources", name] => Some(Self::Resource(name)),
            ["resources", name, "grants"] => Some(Self::Grants(name)),
            ["resources", name, "structure"] => Some(Self::Structure(name)),
            ["operation-log"] => Some(Self::OperationLog),
            ["me"] => Some(Self::Me),
            ["tool-credentials"] => Some(Self::ToolCredentials),
            ["tool-credentials", "current"] => Some(Self::CurrentToolCredential),
            _ => None,
        }
    }
}

/// A Version number as written in a path: digits, no sign, no leading
/// zero, above 0.
fn version_number(text: &str) -> Option<u64> {
    let canonical =
        !text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit());
    canonical.then(|| text.parse().ok()).flatten()
}

/// `POST /v1/applications`
struct CreateApplication {
    name: String,
    description: Option<String>,
}

const CREATE_FORM: &str = r#"{ "name": "<name>", "description": "<text, optional>" }"#;

impl CreateApplication {
    fn read(body: &[u8]) -> Result<Self, String> {
        let object = Object::read(body)?;
        Ok(Self {
            name: object.required("name")?,
            description: object.optional("description")?,
        })
    }
}

/// `POST /v1/applications/<name>/versions`
const VERSION_FORM: &str = r#"{ "bundle": "<the classic script>", "declared_access": { "resources": { "<name>": { "kind": "postgres", "access": "structure|read|write" } } }, "git_commit": "<40 hex characters>", "migrations": [ { "name": "<1 to 200 letters, digits, _ and ->", "sql": "<the file drizzle-kit generated>" } ] (optional), "structures": { "<a declared Resource>": "sha256:<64 lower-case hex>" } (optional) }"#;

/// The most characters a migration's name has.
const MAX_MIGRATION_NAME: usize = 200;

/// A Version's `migrations`: absent or `null`, none; otherwise an array of
/// `{ name, sql }`, each key once, names of 1 to 200 ASCII letters, digits,
/// `_` and `-`, unique in the list; no text holding NUL, which Postgres
/// cannot take.
fn read_migrations(object: &Object<'_>) -> Result<Vec<Migration>, String> {
    let Some(value) = object.0.get("migrations") else {
        return Ok(Vec::new());
    };
    if value.get() == "null" {
        return Ok(Vec::new());
    }
    let entries: Vec<&RawValue> =
        serde_json::from_str(value.get()).map_err(|_| "migrations is not an array".to_owned())?;
    let mut migrations: Vec<Migration> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let entry = Object::read_value(entry).map_err(|e| format!("migrations[{index}]: {e}"))?;
        let name: String = entry
            .required("name")
            .map_err(|e| format!("migrations[{index}]: {e}"))?;
        let sql: String = entry
            .required("sql")
            .map_err(|e| format!("migrations[{index}]: {e}"))?;
        let well_named = !name.is_empty()
            && name.len() <= MAX_MIGRATION_NAME
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !well_named {
            return Err(format!(
                "migrations[{index}].name is not 1 to 200 letters, digits, _ and -"
            ));
        }
        if sql.contains('\0') {
            return Err(format!("migrations[{index}].sql holds a NUL character"));
        }
        if migrations.iter().any(|m| m.name == name) {
            return Err(format!("migrations[{index}].name is given twice"));
        }
        migrations.push(Migration::new(name, sql));
    }
    Ok(migrations)
}

/// A Version's `structures`: absent or `null`, none; otherwise an object,
/// each key once, each key a Resource the Version declares and each value a
/// fingerprint.
fn read_structures(
    object: &Object<'_>,
    declared: &BTreeMap<String, Resource>,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = object.0.get("structures") else {
        return Ok(BTreeMap::new());
    };
    if value.get() == "null" {
        return Ok(BTreeMap::new());
    }
    let entries =
        Object::read_value(value).map_err(|e| format!("structures is not an object: {e}"))?;
    let mut structures = BTreeMap::new();
    for (name, value) in entries.0 {
        if !declared.contains_key(&name) {
            return Err("structures names a Resource the Version does not declare".into());
        }
        let fingerprint: String = serde_json::from_str(value.get())
            .map_err(|_| "a value of structures is not a string".to_owned())?;
        if !structure::is_fingerprint(&fingerprint) {
            return Err("a value of structures is not sha256: and 64 lower-case hex digits".into());
        }
        structures.insert(name, fingerprint);
    }
    Ok(structures)
}

fn read_version(body: &[u8]) -> Result<NewVersion, String> {
    let object = Object::read(body)?;
    let git_commit: String = object.required("git_commit")?;
    if git_commit.len() != 40 || !git_commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("git_commit is not 40 hex characters".into());
    }
    let declared = object.object("declared_access")?;
    let mut resources = BTreeMap::new();
    for (name, value) in declared.object("resources")?.0 {
        let resource =
            Object::read_value(value).map_err(|e| format!("the Resource {name:?}: {e}"))?;
        let resource = Resource {
            kind: resource.required("kind")?,
            access: resource.required("access")?,
        };
        resources.insert(name, resource);
    }
    let structures = read_structures(&object, &resources)?;
    Ok(NewVersion {
        bundle: object.required("bundle")?,
        declared_access: DeclaredAccess { resources },
        git_commit,
        migrations: read_migrations(&object)?,
        structures,
    })
}

/// `POST /v1/resources`
const RESOURCE_FORM: &str = r#"{ "name": "<name>", "kind": "postgres", "description": "<text>", "owner": "<text>", "contact": "<whom to ask for access>", "connection": { "host": "<host>", "port": <1 to 65535>, "database": "<database>", "read": { "role": "<role>", "password": "<password>" }, "write": { "role": "<role>", "password": "<password>" } (optional), "structure": { "role": "<role>", "password": "<password>" } (optional) } }"#;

/// Reads a Resource to register. Its problems are told in fixed words that
/// name a field, never anything the body holds.
fn read_resource(body: &[u8]) -> Result<NewResource, &'static str> {
    const PORT: &str = "connection.port is missing or not a number from 1 to 65535";
    let object =
        Object::read(body).map_err(|_| "the body is not a JSON object with each key once")?;
    let kind: String = object.fixed("kind", "kind is missing or not a string")?;
    let name = object.fixed("name", "name is missing or not a string")?;
    let description = object.fixed("description", "description is missing or not a string")?;
    let owner = object.fixed("owner", "owner is missing or not a string")?;
    let contact = object.fixed("contact", "contact is missing or not a string")?;
    let connection = object
        .object("connection")
        .map_err(|_| "connection is missing or not an object with each key once")?;
    let host = connection.fixed("host", "connection.host is missing or not a string")?;
    let port: u16 = connection.fixed("port", PORT)?;
    let database =
        connection.fixed("database", "connection.database is missing or not a string")?;
    let read = connection
        .object("read")
        .map_err(|_| "connection.read is missing or not an object with each key once")?;
    let role = read.fixed("role", "connection.read.role is missing or not a string")?;
    let password = read.fixed(
        "password",
        "connection.read.password is missing or not a string",
    )?;
    // Optional; `null` is the same as none.
    let write = match connection.0.get("write") {
        None => None,
        Some(value) if value.get() == "null" => None,
        Some(value) => {
            const WRITE: &str = "connection.write is not an object with each key once, holding role and password as strings";
            let write = Object::read_value(value).map_err(|_| WRITE)?;
            Some(Login::new(
                write.fixed("role", WRITE)?,
                write.fixed("password", WRITE)?,
            ))
        }
    };
    // Optional too; `null` is the same as none.
    let structure = match connection.0.get("structure") {
        None => None,
        Some(value) if value.get() == "null" => None,
        Some(value) => {
            const STRUCTURE: &str = "connection.structure is not an object with each key once, holding role and password as strings";
            let structure = Object::read_value(value).map_err(|_| STRUCTURE)?;
            Some(Login::new(
                structure.fixed("role", STRUCTURE)?,
                structure.fixed("password", STRUCTURE)?,
            ))
        }
    };
    if port == 0 {
        return Err(PORT);
    }
    if kind != "postgres" {
        return Err("kind is not \"postgres\"");
    }
    Ok(NewResource {
        name,
        description,
        owner,
        contact,
        connection: Connection::new(
            host,
            port,
            database,
            Login::new(role, password),
            write,
            structure,
        ),
    })
}

/// `POST /v1/resources/<name>/grants`
const GRANT_FORM: &str = r#"{ "group": "<Group: 1 to 256 bytes, no control characters>", "access": "structure|read|write" }"#;

fn read_grant(body: &[u8]) -> Result<(String, authz::AccessLevel), String> {
    let object = Object::read(body)?;
    let group: String = object.required("group")?;
    let access: String = object.required("access")?;
    if group.is_empty() || group.len() > 256 || group.chars().any(char::is_control) {
        return Err("group is not 1 to 256 bytes without control characters".into());
    }
    let level = level_of(&access)
        .ok_or_else(|| "access is not one of structure, read, write".to_owned())?;
    Ok((group, level))
}

/// `?limit=<n>` of `GET /v1/operation-log`: 1 to 1000, written plainly;
/// absent, 100. `None` for anything else. Other parameters are ignored.
fn log_limit(query: Option<&str>) -> Option<u16> {
    let mut limit = None;
    for pair in query.unwrap_or("").split('&') {
        let Some(value) = pair.strip_prefix("limit=") else {
            if pair == "limit" {
                return None;
            }
            continue;
        };
        if limit.is_some() {
            return None;
        }
        limit = Some(value);
    }
    let Some(text) = limit else {
        return Some(100);
    };
    let canonical = !text.is_empty()
        && !text.starts_with('0')
        && text.len() <= 4
        && text.bytes().all(|b| b.is_ascii_digit());
    let limit: u16 = canonical.then(|| text.parse().ok()).flatten()?;
    (1..=oplog::MAX_READ).contains(&limit).then_some(limit)
}

/// `POST /v1/applications/<name>/go-back`
const GO_BACK_FORM: &str =
    r#"{ "from": <the serving Version's number>, "to": <a Version below it, optional> }"#;

/// The body of a go-back: `from`, a positive integer, and `to`, a positive
/// integer below it or `from - 1` when absent or `null` (0 when `from` is
/// 1: there is none).
fn read_go_back(body: &[u8]) -> Result<(u64, u64), String> {
    let object = Object::read(body)?;
    let from: u64 = object.required("from")?;
    if from == 0 {
        return Err("from is not a positive integer".into());
    }
    match object.optional::<u64>("to")? {
        None => Ok((from, from - 1)),
        Some(to) if to == 0 || to >= from => Err(
            "to is not a positive integer below from: only an earlier Version can be gone back to"
                .into(),
        ),
        Some(to) => Ok((from, to)),
    }
}

/// The parameters of `GET /v1/applications/<name>/logs`: `limit` (1 to
/// 1000, written plainly; 100 without it), `after` (a cursor) and `request`,
/// each at most once and percent-decoded; others are ignored. `None` for
/// anything else.
fn logs_query(query: Option<&str>) -> Option<app_logs::Query> {
    let mut limit = None;
    let mut after = None;
    let mut request = None;
    for pair in query.unwrap_or("").split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let slot = match key {
            "limit" => &mut limit,
            "after" => &mut after,
            "request" => &mut request,
            _ => continue,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(percent_decoded(value)?);
    }
    let limit = match limit {
        None => 100,
        Some(text) => plain_limit(&text)?,
    };
    let after = match after {
        None => None,
        Some(text) => Some((app_logs::cursor(&text)?, text)),
    };
    Some(app_logs::Query {
        limit,
        after,
        request,
    })
}

/// The most Versions one answer holds.
const MAX_VERSIONS: u16 = 1000;

/// The `?limit` of a list of Versions: the newest 1 to [`MAX_VERSIONS`], by
/// default all of those; `None` when malformed or given twice. Other parameters are
/// ignored.
fn versions_limit(query: Option<&str>) -> Option<u16> {
    let mut limit = None;
    for pair in query.unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key != "limit" {
            continue;
        }
        if limit.is_some() {
            return None;
        }
        let given = plain_limit(&percent_decoded(value)?)?;
        limit = Some((given <= MAX_VERSIONS).then_some(given)?);
    }
    Some(limit.unwrap_or(MAX_VERSIONS))
}

/// A limit written plainly: digits, no sign, no leading zero, 1 to 1000.
fn plain_limit(text: &str) -> Option<u16> {
    let canonical = !text.is_empty()
        && !text.starts_with('0')
        && text.len() <= 4
        && text.bytes().all(|b| b.is_ascii_digit());
    let limit: u16 = canonical.then(|| text.parse().ok()).flatten()?;
    (1..=app_logs::MAX_READ).contains(&limit).then_some(limit)
}

/// A query value with its `%XX` decoded; `None` when an escape is broken
/// or the result is not UTF-8.
fn percent_decoded(text: &str) -> Option<String> {
    fn hex(byte: u8) -> Option<u8> {
        char::from(byte)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            let high = bytes.get(at + 1).copied().and_then(hex)?;
            let low = bytes.get(at + 2).copied().and_then(hex)?;
            decoded.push((high << 4) | low);
            at += 3;
        } else {
            decoded.push(byte);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// A JSON object read strictly: an object (never an array standing in for
/// one), each key at most once, and its values kept as text until asked
/// for by type. Keys the request does not have are ignored.
struct Object<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys;
        impl<'de> Visitor<'de> for Keys {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object<'de>, M::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if entries.contains_key(&key) {
                        return Err(de::Error::custom(format_args!(
                            "the key {key:?} is given twice"
                        )));
                    }
                    entries.insert(key, value);
                }
                Ok(Object(entries))
            }
        }
        deserializer.deserialize_map(Keys)
    }
}

impl<'a> Object<'a> {
    fn read(json: &'a [u8]) -> Result<Self, String> {
        serde_json::from_slice(json).map_err(|e| e.to_string())
    }

    fn read_value(value: &'a RawValue) -> Result<Self, String> {
        serde_json::from_str(value.get()).map_err(|e| e.to_string())
    }

    fn required<T: DeserializeOwned>(&self, key: &str) -> Result<T, String> {
        let value = self.0.get(key).ok_or_else(|| format!("{key} is missing"))?;
        serde_json::from_str(value.get()).map_err(|e| format!("{key}: {e}"))
    }

    /// Absent and `null` are both `None`.
    fn optional<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        match self.0.get(key) {
            None => Ok(None),
            Some(value) => serde_json::from_str(value.get()).map_err(|e| format!("{key}: {e}")),
        }
    }

    /// Like [`Object::required`], with `problem` as the only message.
    fn fixed<T: DeserializeOwned>(
        &self,
        key: &str,
        problem: &'static str,
    ) -> Result<T, &'static str> {
        self.required(key).map_err(|_| problem)
    }

    fn object(&self, key: &str) -> Result<Object<'a>, String> {
        let value = self.0.get(key).ok_or_else(|| format!("{key} is missing"))?;
        Self::read_value(value).map_err(|e| format!("{key}: {e}"))
    }
}

pub struct Api {
    registry: Arc<Registry>,
    resources: Arc<Resources>,
    log: Arc<OperationLog>,
    /// Applications' log entries, read here.
    logs: Arc<Logs>,
    credentials: Arc<ToolCredentials>,
    domain: Arc<str>,
    /// The Group whose members are Platform administrators, if any.
    administrators: Option<Arc<str>>,
}

impl Api {
    pub fn new(
        registry: Arc<Registry>,
        resources: Arc<Resources>,
        log: Arc<OperationLog>,
        logs: Arc<Logs>,
        credentials: Arc<ToolCredentials>,
        domain: Arc<str>,
        administrators: Option<Arc<str>>,
    ) -> Self {
        Self {
            registry,
            resources,
            log,
            logs,
            credentials,
            domain,
            administrators,
        }
    }

    fn is_administrator(&self, user: &User) -> bool {
        self.administrators
            .as_deref()
            .is_some_and(|group| user.groups.iter().any(|g| g == group))
    }

    /// Answers one request of a signed-in `user`, signed in `with` a tool
    /// credential or otherwise; `request` is its id.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of one request, each named"
    )]
    pub async fn handle(
        &self,
        user: &User,
        with: SignedInWith,
        request: &str,
        method: &Method,
        path: &str,
        query: Option<&str>,
        body: Incoming,
    ) -> Answer {
        let read = Limited::new(body, MAX_BODY_BYTES).collect();
        let body = match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(_)) => {
                return Answer::error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too-large",
                    "the body is larger than 16 MiB: send a smaller bundle",
                );
            }
            Err(_) => {
                return Answer::error(
                    StatusCode::REQUEST_TIMEOUT,
                    "timed-out",
                    "the body did not arrive in time: send it again",
                );
            }
        };
        let Some(route) = Route::of(path) else {
            return Answer::not_found(
                "the API has no such path: its paths are /v1/me, /v1/tool-credentials, /v1/tool-credentials/current, /v1/applications, /v1/applications/<name>, /v1/applications/<name>/versions, /v1/applications/<name>/versions/<number>, /v1/applications/<name>/go-back, /v1/applications/<name>/logs, /v1/resources, /v1/resources/<name>, /v1/resources/<name>/grants, /v1/resources/<name>/structure and /v1/operation-log",
            );
        };
        if route.for_administrators(method) && !self.is_administrator(user) {
            return Answer::error(
                StatusCode::FORBIDDEN,
                "not-an-administrator",
                "only Platform administrators may do this: ask one",
            );
        }
        let answer = match route {
            Route::Resources => self.resources_route(user, method, &body).await,
            Route::Resource(name) => Ok(self.resource(user, method, name)),
            Route::Me => Ok(self.me(user, method)),
            Route::ToolCredentials => Ok(self.make_credential(user, with, method, &body).await),
            Route::CurrentToolCredential => self.end_credential(with, method).await,
            Route::Grants(name) => self.grants(method, name, &body).await,
            Route::Structure(name) => Ok(self.structure(user, request, method, name).await),
            Route::OperationLog => self.operation_log(method, query).await,
            Route::Applications => self.applications(user, method, &body).await,
            Route::Application(name) => self.application(method, name).await,
            Route::Versions(name) => self.versions(user, method, name, query, &body).await,
            Route::Version(name, number) => self.version(method, name, number).await,
            Route::GoBack(name) => self.go_back(user, method, name, &body).await,
            Route::Logs(name) => self.logs(user, method, name, query).await,
        };
        answer.unwrap_or_else(|error| Answer::unavailable(&error))
    }

    async fn applications(
        &self,
        user: &User,
        method: &Method,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method == Method::GET {
            let listed = self.registry.contributed_to(&user.id).await?;
            let view = ListedApplicationsView {
                applications: listed
                    .iter()
                    .map(|a| ListedApplicationView {
                        name: &a.name,
                        description: a.description.as_deref(),
                        address: format!("{}.{}", a.name, self.domain),
                        contributor: true,
                        serving_version: a.serving_version,
                    })
                    .collect(),
            };
            return Ok(Answer::json(StatusCode::OK, &view));
        }
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "GET or POST"));
        }
        let request = match CreateApplication::read(body) {
            Ok(request) => request,
            Err(problem) => return Ok(Answer::malformed(&problem, CREATE_FORM)),
        };
        match check_name(&request.name) {
            Ok(()) => {}
            Err(NameProblem::Invalid) => {
                return Ok(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "invalid-name",
                    "an Application's name is its subdomain: use 1 to 63 lower-case letters, digits and hyphens, not starting or ending with a hyphen, and not starting with xn--",
                ));
            }
            Err(NameProblem::Reserved) => {
                return Ok(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "reserved-name",
                    "this name is one of the platform's own (api, auth, dashboard, git, admin, www): choose another",
                ));
            }
        }
        let created = self
            .registry
            .create_application(&request.name, request.description.as_deref(), &user.id)
            .await?;
        Ok(match created {
            Created::Created(application) => {
                Answer::json(StatusCode::CREATED, &self.view(&application))
            }
            Created::NameTaken => Answer::error(
                StatusCode::CONFLICT,
                "name-taken",
                "an Application with this name exists: choose another name",
            ),
        })
    }

    async fn application(&self, method: &Method, name: &str) -> Result<Answer, DbError> {
        let Some(application) = self.find(name).await? else {
            return Ok(no_application());
        };
        Ok(if method == Method::GET {
            Answer::json(StatusCode::OK, &self.view(&application))
        } else {
            Answer::method_not_allowed(method, "GET")
        })
    }

    async fn versions(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        query: Option<&str>,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method == Method::GET {
            let Some(limit) = versions_limit(query) else {
                // Malformed; but an Application that is not there is that
                // first.
                return Ok(match self.find(name).await? {
                    Some(_) => Answer::error(
                        StatusCode::BAD_REQUEST,
                        "malformed",
                        "the parameters are not the ones asked for: ?limit is a whole number from 1 to 1000, at most once",
                    ),
                    None => no_application(),
                });
            };
            let versions = if check_name(name).is_ok() {
                self.registry.versions(name, limit).await?
            } else {
                None
            };
            let Some(versions) = versions else {
                return Ok(no_application());
            };
            let versions = VersionsView {
                versions: versions.iter().map(VersionView::of).collect(),
            };
            return Ok(Answer::json(StatusCode::OK, &versions));
        }
        if method != Method::POST {
            return Ok(match self.find(name).await? {
                Some(_) => Answer::method_not_allowed(method, "GET or POST"),
                None => no_application(),
            });
        }
        let contributor = if check_name(name).is_ok() {
            self.registry.is_contributor(name, &user.id).await?
        } else {
            None
        };
        match contributor {
            None => return Ok(no_application()),
            Some(false) => return Ok(not_a_contributor()),
            Some(true) => {}
        }
        let version = match read_version(body) {
            Ok(version) => version,
            Err(problem) => return Ok(Answer::malformed(&problem, VERSION_FORM)),
        };
        Ok(
            match self.registry.send_version(name, version, &user.id).await? {
                Sent::Sent(version) => {
                    Answer::json(StatusCode::CREATED, &VersionView::of(&version))
                }
                Sent::NoSuchApplication => no_application(),
                Sent::NotAContributor => not_a_contributor(),
                Sent::Diverged => Answer::error(
                    StatusCode::CONFLICT,
                    "migrations-diverged",
                    "the Version's migrations do not begin with exactly the previous Version's (the same names and texts, in the same order): send the Application's whole history of migrations, with new ones only at the end",
                ),
                Sent::MigrationFailed {
                    migration,
                    code,
                    message,
                    serving,
                } => Answer::json(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &json!({ "error": {
                        "reason": "migration-failed",
                        "message": format!("the migration {migration:?} failed in the database ({code}): nothing was applied{}; correct it and send the Version again", still_serves(serving)),
                        "migration": migration,
                        "code": code,
                        "database_message": message,
                    } }),
                ),
                Sent::MigrationRefused {
                    migration,
                    rule,
                    message,
                    serving,
                } => Answer::json(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &json!({ "error": {
                        "reason": "migration-refused",
                        "rule": rule,
                        "message": format!("the migration {migration:?} breaks the platform's rule {rule}: {message}. Nothing was applied{}", still_serves(serving)),
                        "migration": migration,
                    } }),
                ),
                Sent::Busy => Answer::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "too many Versions are being applied at once on the platform: nothing of this one was applied; send it again shortly",
                ),
                Sent::OwnDatabaseUnavailable { step, code } => {
                    log::record(
                        "own-database-failed",
                        json!({ "application": name, "step": step, "code": code }),
                    );
                    Answer::error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "the platform could not prepare the Application's own database: nothing was applied; try again shortly",
                    )
                }
            },
        )
    }

    async fn version(
        &self,
        method: &Method,
        name: &str,
        number: Option<u64>,
    ) -> Result<Answer, DbError> {
        let version = match number {
            Some(number) if check_name(name).is_ok() => self.registry.version(name, number).await?,
            _ => None,
        };
        let Some(version) = version else {
            return Ok(Answer::not_found(
                "no such Version: list the Application's Versions at /v1/applications/<name>/versions",
            ));
        };
        Ok(if method == Method::GET {
            Answer::json(StatusCode::OK, &VersionView::of(&version))
        } else {
            Answer::method_not_allowed(method, "GET")
        })
    }

    /// `POST /v1/applications/<name>/go-back` (blackbox/CONTRACT.md,
    /// "Going back, on the platform's API"): by a Contributor only; a
    /// Platform administrator who is not one is refused as anyone else.
    async fn go_back(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let contributor = if check_name(name).is_ok() {
            self.registry.is_contributor(name, &user.id).await?
        } else {
            None
        };
        let Some(contributor) = contributor else {
            return Ok(no_application());
        };
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "POST"));
        }
        if !contributor {
            return Ok(Answer::error(
                StatusCode::FORBIDDEN,
                "not-a-contributor",
                "you are not a Contributor of this Application, and only its Contributors may change which Version serves: ask one of them",
            ));
        }
        let (from, to) = match read_go_back(body) {
            Ok(asked) => asked,
            Err(problem) => return Ok(Answer::malformed(&problem, GO_BACK_FORM)),
        };
        Ok(match self.registry.go_back(name, from, to).await? {
            WentBack::WentBack {
                migrations_not_undone,
            } => {
                log::record(
                    "went-back",
                    json!({ "application": name, "user": &user.id, "from": from, "to": to }),
                );
                Answer::json(
                    StatusCode::OK,
                    &json!({
                        "application": name,
                        "serving_version": to,
                        "was": from,
                        "migrations_not_undone": migrations_not_undone,
                    }),
                )
            }
            WentBack::ServingChanged(serving) => Answer::error(
                StatusCode::CONFLICT,
                "serving-changed",
                &match serving {
                    Some(serving) => format!(
                        "Version {from} is not the serving Version: Version {serving} serves now (someone deployed or went back meanwhile). Look at the Application's Versions, then go back from Version {serving} if that is still what you want; nothing was changed"
                    ),
                    None => "the Application has no Version, so none serves and there is nothing to go back from; nothing was changed".to_owned(),
                },
            ),
            WentBack::NoPreviousVersion => Answer::error(
                StatusCode::CONFLICT,
                "no-previous-version",
                &format!(
                    "Version {from} serves and it is the Application's first: there is no Version before it, so nothing to go back to; nothing was changed"
                ),
            ),
        })
    }

    /// `GET /v1/applications/<name>/logs` (blackbox/CONTRACT.md, "Logs, on
    /// the platform's API"): by a Contributor or a Platform administrator.
    async fn logs(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        query: Option<&str>,
    ) -> Result<Answer, DbError> {
        let contributor = if check_name(name).is_ok() {
            self.registry.is_contributor(name, &user.id).await?
        } else {
            None
        };
        let Some(contributor) = contributor else {
            return Ok(no_application());
        };
        if method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        if !contributor && !self.is_administrator(user) {
            return Ok(Answer::error(
                StatusCode::FORBIDDEN,
                "not-a-contributor",
                "you are not a Contributor of this Application, and only its Contributors and Platform administrators may read its logs: ask one of its Contributors",
            ));
        }
        let Some(query) = logs_query(query) else {
            return Ok(Answer::error(
                StatusCode::BAD_REQUEST,
                "malformed",
                "the parameters are not the ones asked for: ?limit is a whole number from 1 to 1000, ?after a cursor the API gave (an entry's id, or next), ?request a request's x-vm-request-id; each at most once",
            ));
        };
        let page = self.logs.read(name, &query).await?;
        Ok(Answer::json(StatusCode::OK, &page))
    }

    async fn resources_route(
        &self,
        user: &User,
        method: &Method,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method == Method::GET {
            let all = self.resources.list();
            let resources = ResourcesView {
                resources: {
                    let groups = groups_of(user);
                    all.iter().map(|r| ResourceRead::of(r, &groups)).collect()
                },
            };
            return Ok(Answer::json(StatusCode::OK, &resources));
        }
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "GET or POST"));
        }
        let new = match read_resource(body) {
            Ok(new) => new,
            Err(problem) => return Ok(Answer::malformed(problem, RESOURCE_FORM)),
        };
        if !is_resource_name(&new.name) {
            return Ok(Answer::error(
                StatusCode::BAD_REQUEST,
                "invalid-name",
                "a Resource's name is 1 to 63 lower-case letters, digits and underscores, starting with a letter",
            ));
        }
        Ok(match self.resources.register(new, &user.id).await? {
            Registered::Registered(resource) => {
                Answer::json(StatusCode::CREATED, &ResourceView::of(&resource))
            }
            Registered::NameTaken => Answer::error(
                StatusCode::CONFLICT,
                "name-taken",
                "a Resource with this name is registered: choose another name",
            ),
        })
    }

    fn resource(&self, user: &User, method: &Method, name: &str) -> Answer {
        let Some(resource) = self.resources.get(name) else {
            return no_resource();
        };
        if method == Method::GET {
            Answer::json(
                StatusCode::OK,
                &ResourceRead::of(&resource, &groups_of(user)),
            )
        } else {
            Answer::method_not_allowed(method, "GET")
        }
    }

    async fn grants(&self, method: &Method, name: &str, body: &[u8]) -> Result<Answer, DbError> {
        let Some(resource) = self.resources.get(name) else {
            return Ok(no_resource());
        };
        if method == Method::GET {
            let grants = resource.grants();
            let view = GrantsView {
                grants: grants
                    .iter()
                    .map(|(group, level)| GrantView {
                        group,
                        access: resources::level_name(*level),
                    })
                    .collect(),
            };
            return Ok(Answer::json(StatusCode::OK, &view));
        }
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "GET or POST"));
        }
        let (group, level) = match read_grant(body) {
            Ok(grant) => grant,
            Err(problem) => return Ok(Answer::malformed(&problem, GRANT_FORM)),
        };
        Ok(match self.resources.grant(name, &group, level).await? {
            GrantWritten::Written => Answer::json(
                StatusCode::OK,
                &GrantSet {
                    resource: &resource.name,
                    group: &group,
                    access: resources::level_name(level),
                },
            ),
            GrantWritten::NoSuchResource => no_resource(),
        })
    }

    /// `GET /v1/resources/<name>/structure` (blackbox/CONTRACT.md, "Pulling
    /// a Resource's structure"): the Authorization decision with
    /// `structure` needed; the Operation log entry written before anything
    /// is sent to the shared database, and completed after; the pull
    /// (crate::structure) within its time and bytes.
    async fn structure(&self, user: &User, request: &str, method: &Method, name: &str) -> Answer {
        let Some(resource) = self.resources.get(name) else {
            return no_resource();
        };
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let groups = groups_of(user);
        let (decision, held) = resource.with_grants(|grants| {
            let decision = authz::decide(&authz::Question {
                request_in_flight: true,
                actor: authz::Actor::User,
                own_database: false,
                groups: &groups,
                // No Application declares a pull: the Builder asks for the
                // structure, which is what the pull needs.
                declared: Some(authz::AccessLevel::Structure),
                registered: true,
                grants,
                needed: authz::AccessLevel::Structure,
            });
            (decision, authz::held(&groups, grants))
        });
        if let authz::Decision::Refuse(_) = decision {
            if let Err(error) = self
                .log
                .refused_structure(request, &user.id, name, "no-grant")
                .await
            {
                return Answer::unavailable(&error);
            }
            return Answer::json(
                StatusCode::FORBIDDEN,
                &json!({ "error": {
                    "reason": "no-grant",
                    "message": format!(
                        "you hold no Grant on the Resource {name}: ask {} for one, and do not build against {name} until you have it",
                        resource.contact
                    ),
                    "resource": name,
                    "contact": &resource.contact,
                } }),
            );
        }
        // Taken before the entry is written and anything is sent; held to the
        // end of this function, or given back as its future is dropped.
        let pulling = resource.pulling();
        let id = match self.log.begin_structure(request, &user.id, name).await {
            Ok(id) => id,
            Err(error) => return Answer::unavailable(&error),
        };
        let Some(_pulling) = pulling else {
            let ending = Ending {
                outcome: "failed",
                error: Some("busy"),
                rows: None,
                duration_ms: None,
                began_nothing: false,
            };
            if let Err(error) = self.log.finish(id, &ending).await {
                log::record(
                    "operation-log-unwritten",
                    json!({ "resource": name, "error": error.to_string() }),
                );
            }
            return Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "structure-unavailable",
                &format!(
                    "{} pulls of the structure of {name} are running now: try again shortly",
                    resources::PULLS_AT_ONCE
                ),
            );
        };
        let started = tokio::time::Instant::now();
        let pulled = structure::pull(resource.structure_pool(), name).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let answer = pulled.and_then(|pulled| {
            let answer = Answer::json(
                StatusCode::OK,
                &StructureView {
                    resource: name,
                    fingerprint: &pulled.fingerprint,
                    your_access: held.map(resources::level_name),
                    schema_ts: &pulled.schema_ts,
                    unsupported: &pulled.unsupported,
                },
            );
            if answer.body.len() > structure::MAX_BYTES {
                Err(structure::Failed::TooLarge)
            } else {
                Ok(answer)
            }
        });
        let ending = match &answer {
            Ok(_) => Ending {
                outcome: "allowed",
                error: None,
                rows: None,
                duration_ms: Some(duration_ms),
                began_nothing: false,
            },
            Err(failed) => Ending {
                outcome: "failed",
                error: Some(failed.error()),
                rows: None,
                duration_ms: (*failed != structure::Failed::Unavailable).then_some(duration_ms),
                began_nothing: false,
            },
        };
        if let Err(error) = self.log.finish(id, &ending).await {
            log::record(
                "operation-log-unwritten",
                json!({ "resource": name, "error": error.to_string() }),
            );
        }
        answer.unwrap_or_else(|failed| match failed {
            structure::Failed::TooLarge => Answer::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "structure-too-large",
                &format!(
                    "the structure of {name} is larger than the 4 MiB a pull answers: ask its owner whether part of it can be kept in another database, or another Resource registered for the part you build against"
                ),
            ),
            structure::Failed::Time => Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "structure-unavailable",
                &format!(
                    "reading the structure of {name} took longer than the 10 seconds a pull has: try again shortly; if it goes on, tell its owner"
                ),
            ),
            structure::Failed::Unavailable => Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "structure-unavailable",
                &format!(
                    "the shared database of {name} cannot be reached or does not take the platform's login now: try again shortly; if it goes on, tell its owner"
                ),
            ),
        })
    }

    /// `GET /v1/me`: who is signed in.
    fn me(&self, user: &User, method: &Method) -> Answer {
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        Answer::json(
            StatusCode::OK,
            &MeView {
                user: &user.id,
                groups: &user.groups,
                platform_administrator: self.is_administrator(user),
            },
        )
    }

    /// `POST /v1/tool-credentials`: a tool credential for a User signed in
    /// otherwise. The answer is the only one that ever holds a credential.
    async fn make_credential(
        &self,
        user: &User,
        with: SignedInWith,
        method: &Method,
        body: &[u8],
    ) -> Answer {
        if method != Method::POST {
            return Answer::method_not_allowed(method, "POST");
        }
        if let SignedInWith::ToolCredential(_) = with {
            return Answer::error(
                StatusCode::FORBIDDEN,
                "not-with-a-tool-credential",
                "a tool credential cannot make another: sign in again with `vm sign-in`",
            );
        }
        if !body.iter().all(u8::is_ascii_whitespace)
            && let Err(problem) = Object::read(body)
        {
            return Answer::malformed(&problem, "an empty body, or {}");
        }
        let made = match self.credentials.make(user).await {
            Ok(made) => made,
            Err(MakeError::Database(error)) => return Answer::unavailable(&error),
            Err(MakeError::Randomness) => {
                log::record("randomness-failed", json!({ "for": "a tool credential" }));
                return Answer::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "the platform could not make a credential: try again shortly",
                );
            }
        };
        Answer::json(
            StatusCode::CREATED,
            &MadeView {
                credential: made.credential.expose(),
                user: &user.id,
                groups: &user.groups,
                platform_administrator: self.is_administrator(user),
                expires_at: &made.expires_at,
            },
        )
    }

    /// `DELETE /v1/tool-credentials/current`: ends the credential the
    /// request was signed in with.
    async fn end_credential(&self, with: SignedInWith, method: &Method) -> Result<Answer, DbError> {
        if method != Method::DELETE {
            return Ok(Answer::method_not_allowed(method, "DELETE"));
        }
        let SignedInWith::ToolCredential(digest) = with else {
            return Ok(Answer::not_found(
                "this request was not signed in with a tool credential, so there is none to end: send it with the credential in `Authorization: Bearer`",
            ));
        };
        self.credentials.end(digest).await?;
        Ok(Answer {
            status: StatusCode::NO_CONTENT,
            body: Bytes::new(),
        })
    }

    async fn operation_log(&self, method: &Method, query: Option<&str>) -> Result<Answer, DbError> {
        if method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        let Some(limit) = log_limit(query) else {
            return Ok(Answer::error(
                StatusCode::BAD_REQUEST,
                "malformed",
                "?limit must be a whole number from 1 to 1000",
            ));
        };
        let entries = self.log.newest(limit).await?;
        Ok(Answer::json(StatusCode::OK, &EntriesView { entries }))
    }

    /// The Application `name`; `None` also for a name no Application can
    /// have, without asking the database.
    async fn find(&self, name: &str) -> Result<Option<ApplicationRecord>, DbError> {
        if check_name(name).is_err() {
            return Ok(None);
        }
        self.registry.application(name).await
    }

    fn view<'a>(&self, application: &'a ApplicationRecord) -> ApplicationView<'a> {
        ApplicationView {
            name: &application.name,
            description: application.description.as_deref(),
            address: format!("{}.{}", application.name, self.domain),
            contributors: application
                .contributors
                .iter()
                .map(|user| ContributorView { user })
                .collect(),
            serving_version: application.serving_version,
            latest_version: application.latest_version,
            created_by: &application.created_by,
            created_at: &application.created_at,
        }
    }
}

/// An Application as the API answers it.
#[derive(Serialize)]
struct ApplicationView<'a> {
    name: &'a str,
    description: Option<&'a str>,
    address: String,
    contributors: Vec<ContributorView<'a>>,
    serving_version: Option<u64>,
    latest_version: Option<u64>,
    created_by: &'a str,
    created_at: &'a str,
}

#[derive(Serialize)]
struct ContributorView<'a> {
    user: &'a str,
}

/// A Version as the API answers it.
#[derive(Serialize)]
struct VersionView<'a> {
    application: &'a str,
    number: u64,
    declared_access: &'a Value,
    git_commit: &'a str,
    bundle_sha256: &'a str,
    migrations: &'a Value,
    structures: &'a Value,
    created_by: &'a str,
    created_at: &'a str,
}

impl<'a> VersionView<'a> {
    fn of(version: &'a VersionRecord) -> Self {
        Self {
            application: &version.application,
            number: version.number,
            declared_access: &version.declared_access,
            git_commit: &version.git_commit,
            bundle_sha256: &version.bundle_sha256,
            migrations: &version.migrations,
            structures: &version.structures,
            created_by: &version.created_by,
            created_at: &version.created_at,
        }
    }
}

#[derive(Serialize)]
struct VersionsView<'a> {
    versions: Vec<VersionView<'a>>,
}

/// A Resource as every signed-in User sees it.
#[derive(Serialize)]
struct ResourceView<'a> {
    name: &'a str,
    kind: &'static str,
    description: &'a str,
    owner: &'a str,
    contact: &'a str,
    created_by: &'a str,
    created_at: &'a str,
}

impl<'a> ResourceView<'a> {
    fn of(resource: &'a resources::Resource) -> Self {
        Self {
            name: &resource.name,
            kind: "postgres",
            description: &resource.description,
            owner: &resource.owner,
            contact: &resource.contact,
            created_by: &resource.created_by,
            created_at: &resource.created_at,
        }
    }
}

/// A Resource as a signed-in User reads it: its public face and the
/// highest Access level they hold on it through their Groups.
#[derive(Serialize)]
struct ResourceRead<'a> {
    #[serde(flatten)]
    face: ResourceView<'a>,
    your_access: Option<&'static str>,
}

impl<'a> ResourceRead<'a> {
    /// `groups`: the reader's, as [`groups_of`] gives them, once a request.
    fn of(resource: &'a resources::Resource, groups: &[&str]) -> Self {
        let held = resource.with_grants(|grants| authz::held(groups, grants));
        Self {
            face: ResourceView::of(resource),
            your_access: held.map(resources::level_name),
        }
    }
}

/// The User's Groups as the Authorization decision takes them: borrowed,
/// not copied.
fn groups_of(user: &User) -> Vec<&str> {
    user.groups.iter().map(String::as_str).collect()
}

#[derive(Serialize)]
struct ResourcesView<'a> {
    resources: Vec<ResourceRead<'a>>,
}

/// `GET /v1/me`.
#[derive(Serialize)]
struct MeView<'a> {
    user: &'a str,
    groups: &'a [String],
    platform_administrator: bool,
}

/// The answer of `POST /v1/tool-credentials`.
#[derive(Serialize)]
struct MadeView<'a> {
    credential: &'a str,
    user: &'a str,
    groups: &'a [String],
    platform_administrator: bool,
    expires_at: &'a str,
}

/// An Application in `GET /v1/applications`.
#[derive(Serialize)]
struct ListedApplicationView<'a> {
    name: &'a str,
    description: Option<&'a str>,
    address: String,
    contributor: bool,
    serving_version: Option<u64>,
}

#[derive(Serialize)]
struct ListedApplicationsView<'a> {
    applications: Vec<ListedApplicationView<'a>>,
}

#[derive(Serialize)]
struct GrantView<'a> {
    group: &'a str,
    access: &'static str,
}

#[derive(Serialize)]
struct GrantsView<'a> {
    grants: Vec<GrantView<'a>>,
}

#[derive(Serialize)]
struct GrantSet<'a> {
    resource: &'a str,
    group: &'a str,
    access: &'static str,
}

/// `GET /v1/resources/<name>/structure`.
#[derive(Serialize)]
struct StructureView<'a> {
    resource: &'a str,
    fingerprint: &'a str,
    your_access: Option<&'static str>,
    schema_ts: &'a str,
    unsupported: &'a [structure::Unsupported],
}

#[derive(Serialize)]
struct EntriesView {
    entries: Vec<Value>,
}

fn no_resource() -> Answer {
    Answer::not_found("no such Resource: list them at /v1/resources")
}

/// What a refused Version's message says of the one serving: that it
/// still serves when one does, nothing when none does.
fn still_serves(serving: Option<u64>) -> String {
    serving
        .map(|n| format!(" and Version {n} still serves"))
        .unwrap_or_default()
}

fn no_application() -> Answer {
    Answer::not_found("no such Application: create it with POST /v1/applications")
}

fn not_a_contributor() -> Answer {
    Answer::error(
        StatusCode::FORBIDDEN,
        "not-a-contributor",
        "you are not a Contributor of this Application: ask one of its Contributors to send the Version, or to add you",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_numbers_in_paths_are_canonical_decimals_above_zero() {
        assert_eq!(version_number("1"), Some(1));
        assert_eq!(version_number("42"), Some(42));
        for text in [
            "",
            "0",
            "01",
            "-1",
            "+1",
            "1.5",
            "1e0",
            "%31",
            "18446744073709551616",
        ] {
            assert_eq!(version_number(text), None, "{text}");
        }
    }

    #[test]
    fn bodies_are_objects_with_each_key_once() {
        assert!(CreateApplication::read(br#"{"name":"a","other":[1]}"#).is_ok());
        for body in [
            &br#"["a"]"#[..],
            br#"{"name":"a","name":"b"}"#,
            br#"{"name":5}"#,
            br#"{}"#,
            b"{\"name\":\"\xff\"}",
        ] {
            assert!(
                CreateApplication::read(body).is_err(),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        let good = br#"{"bundle":"x","declared_access":{"resources":{"r":{"kind":"postgres","access":"read"}}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#;
        assert!(read_version(good).is_ok());
        for body in [
            r#"{"bundle":"x","declared_access":[{"resources":{}}],"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{"r":["postgres","read"]}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{"r":{"kind":"postgres","access":"read"},"r":{"kind":"postgres","access":"write"}}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{}},"git_commit":"0123456789abcdef0123456789abcdef0123456"}"#,
        ] {
            assert!(read_version(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn operation_log_limits_are_plain_numbers_from_1_to_1000() {
        assert_eq!(log_limit(None), Some(100));
        assert_eq!(log_limit(Some("")), Some(100));
        assert_eq!(log_limit(Some("limit=1")), Some(1));
        assert_eq!(log_limit(Some("limit=1000")), Some(1000));
        for query in [
            "limit=0",
            "limit=1001",
            "limit=-1",
            "limit=abc",
            "limit=",
            "limit",
            "limit=01",
            "limit=5&limit=6",
        ] {
            assert_eq!(log_limit(Some(query)), None, "{query}");
        }
    }

    #[test]
    fn a_resource_body_problem_never_repeats_the_body() {
        let body =
            br#"{"name":"devops","kind":"postgres","description":"d","owner":"o","contact":"c",
            "connection":{"host":"secret-host","port":"secret-port","database":"secret-db",
            "read":{"role":"secret-role","password":"secret-password"}}}"#;
        let problem = read_resource(body).err().unwrap_or_default();
        assert!(
            problem.contains("port") && !problem.contains("secret"),
            "{problem}"
        );
        let problem = read_resource(br#""secret-password""#)
            .err()
            .unwrap_or_default();
        assert!(!problem.contains("secret"), "{problem}");
        let body =
            br#"{"name":"devops","kind":"postgres","description":"d","owner":"o","contact":"c",
            "connection":{"host":"h","port":5432,"database":"d",
            "read":{"role":"r","password":"p"},"write":{"role":"secret-role","password":7}}}"#;
        let problem = read_resource(body).err().unwrap_or_default();
        assert!(
            problem.contains("connection.write") && !problem.contains("secret"),
            "{problem}"
        );
    }

    #[test]
    fn paths_are_matched_as_sent() {
        assert!(matches!(
            Route::of("/v1/applications"),
            Some(Route::Applications)
        ));
        assert!(matches!(
            Route::of("/v1/applications/a/versions"),
            Some(Route::Versions("a"))
        ));
        for path in [
            "/",
            "/v1",
            "/v1/",
            "/v2/applications",
            "/V1/applications",
            "/v1/applications/a/b",
            "/v1/applications/a/versions/1/x",
        ] {
            assert!(Route::of(path).is_none(), "{path}");
        }
    }
}
