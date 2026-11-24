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
//! administrator is a member of the Group the OIDC connection in use names
//! (crate::oidc::connection), or with the stand-in of the Group named at
//! start (`--administrators-group`); without one, no one is.
//!
//! Connecting OIDC (`PUT /v1/oidc`, by a Platform administrator or with the
//! setup credential, [`Caller::Setup`]), `GET /v1/oidc`, `GET
//! /v1/installation` and `POST /v1/tool-sign-ins` (by anyone,
//! [`Caller::Anyone`]) are here too; the work of the two that change
//! anything is crate::oidc's. A body that registers
//! a Resource holds a password, so no message about it repeats anything
//! from it: its problems are told in fixed words.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Body;
use hyper::{Method, StatusCode};
use serde::de::{self, DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::app_logs::{self, Logs};
use crate::log;
use crate::oidc::Oidc;
use crate::oidc::connection::{self as oidc_connection, By};
use crate::oplog::{self, Ending, OperationLog};
use crate::origin::Public;
use crate::own_db::Migration;
use crate::platform_db::DbError;
use crate::platform_db::PlatformDatabase;
use crate::registry::{
    Access, AdminApplication, ApplicationRecord, Asked, Change, Changed, Created, DeclaredAccess,
    Disabled, Entry, List, Listing, MAX_CONTRIBUTORS, MAX_SHARED_WITH, NameProblem, Named,
    NewVersion, Registry, Resource, ResourceKind, Sent, Standing, VersionRecord, WentBack,
    check_name,
};
use crate::resources::{
    self, Connection, GrantWritten, Login, NewReach, NewResource, Registered, Resources,
    WRITE_ROW_WAIT, is_resource_name, level_of,
};
use crate::setup;
use crate::storage::{self, Storage};
use crate::structure;
use crate::tool_credentials::{Digest, MakeError, ToolCredentials};

/// Who a request is from.
#[derive(Clone, Copy)]
pub enum Caller<'a> {
    /// A User, signed in so.
    User(&'a User, SignedInWith),
    /// Someone with a setup credential, by its SHA-256: only `PUT
    /// /v1/oidc` takes it.
    Setup(Digest),
    /// Anyone: only `GET /v1/installation` and `POST /v1/tool-sign-ins`.
    Anyone,
}

/// How the installation signs people in, as the API needs it.
pub struct Signing {
    /// The stand-in sign-in is on.
    pub stand_in: bool,
    /// With the stand-in, the Group whose members are Platform
    /// administrators (`--administrators-group`).
    pub administrators: Option<Arc<str>>,
    /// OIDC, when the program has an installation's key and no stand-in.
    pub oidc: Option<Arc<Oidc>>,
    /// Where the setup credential and the connection are kept.
    pub database: Arc<PlatformDatabase>,
    /// How a browser reaches the installation.
    pub public: Public,
}
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

/// The largest body read of a request not signed in as a User (the setup
/// credential's, and anyone's).
pub const MAX_BODY_BYTES_NOT_SIGNED_IN: usize = 64 * 1024;

/// The longest a client may take to send a request's body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// An answer of the API: a status and a JSON body.
pub struct Answer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Answer {
    /// `value` as JSON, written straight from what it borrows.
    pub(crate) fn json(status: StatusCode, value: &impl Serialize) -> Self {
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

    pub(crate) fn error(status: StatusCode, reason: &str, message: &str) -> Self {
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
    /// `/v1/applications/<name>/contributors`
    Contributors(&'a str),
    /// `/v1/applications/<name>/sharing`
    Sharing(&'a str),
    /// `/v1/applications/<name>/disable` (`true`) and `/enable`
    Disable(&'a str, bool),
    /// `/v1/admin/applications`
    AdminApplications,
    /// `/v1/resources`
    Resources,
    /// `/v1/resources/<name>`
    Resource(&'a str),
    /// `/v1/resources/<name>/grants`
    Grants(&'a str),
    /// `/v1/resources/<name>/grants/<group>`, the Group as it was written
    /// in the path (percent-encoded), not decoded yet.
    GrantRevoke(&'a str, &'a str),
    /// `/v1/resources/<name>/structure`
    Structure(&'a str),
    /// `/v1/resources/<name>/connection`
    Connection(&'a str),
    /// `/v1/resources/<name>/location`
    Location(&'a str),
    /// `/v1/admin/resources`
    AdminResources,
    /// `/v1/admin/resources/<name>`
    AdminResource(&'a str),
    /// `/v1/admin/grants`
    AdminGrants,
    /// `/v1/outbound-hosts`
    OutboundHosts,
    /// `/v1/outbound-hosts/<entry>`, the entry as it was written in the
    /// path (percent-encoded), not decoded yet.
    OutboundEntry(&'a str),
    /// `/v1/operation-log`
    OperationLog,
    /// `/v1/me`
    Me,
    /// `/v1/tool-credentials`
    ToolCredentials,
    /// `/v1/tool-credentials/current`
    CurrentToolCredential,
    /// `/v1/oidc`
    Oidc,
    /// `/v1/installation`
    Installation,
    /// `/v1/tool-sign-ins`
    ToolSignIns,
}

impl Route<'_> {
    /// Only Platform administrators may use it, whatever the method.
    fn for_administrators(&self, method: &Method) -> bool {
        match self {
            Self::Resources => method != Method::GET,
            // One path, two kinds of request: `GET` is for every signed-in
            // User, and `PATCH` and `DELETE` are the administrators'.
            Self::Resource(_) => matches!(*method, Method::PATCH | Method::DELETE),
            Self::Grants(_)
            | Self::GrantRevoke(..)
            | Self::Connection(_)
            | Self::Location(_)
            | Self::AdminResources
            | Self::AdminResource(_)
            | Self::AdminGrants
            | Self::OperationLog
            | Self::Oidc
            | Self::Disable(..)
            | Self::AdminApplications
            | Self::OutboundEntry(_) => true,
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
            ["applications", name, "contributors"] => Some(Self::Contributors(name)),
            ["applications", name, "sharing"] => Some(Self::Sharing(name)),
            ["applications", name, "disable"] => Some(Self::Disable(name, true)),
            ["applications", name, "enable"] => Some(Self::Disable(name, false)),
            ["admin", "applications"] => Some(Self::AdminApplications),
            ["resources"] => Some(Self::Resources),
            ["resources", name] => Some(Self::Resource(name)),
            ["resources", name, "grants"] => Some(Self::Grants(name)),
            ["resources", name, "grants", group] => Some(Self::GrantRevoke(name, group)),
            ["resources", name, "structure"] => Some(Self::Structure(name)),
            ["resources", name, "connection"] => Some(Self::Connection(name)),
            ["resources", name, "location"] => Some(Self::Location(name)),
            ["admin", "resources"] => Some(Self::AdminResources),
            ["admin", "resources", name] => Some(Self::AdminResource(name)),
            ["admin", "grants"] => Some(Self::AdminGrants),
            ["outbound-hosts"] => Some(Self::OutboundHosts),
            ["outbound-hosts", entry] => Some(Self::OutboundEntry(entry)),
            ["operation-log"] => Some(Self::OperationLog),
            ["me"] => Some(Self::Me),
            ["tool-credentials"] => Some(Self::ToolCredentials),
            ["tool-credentials", "current"] => Some(Self::CurrentToolCredential),
            ["oidc"] => Some(Self::Oidc),
            ["installation"] => Some(Self::Installation),
            ["tool-sign-ins"] => Some(Self::ToolSignIns),
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
const VERSION_FORM: &str = r#"{ "bundle": "<the classic script>", "declared_access": { "resources": { "<name>": { "kind": "postgres" | "storage", "access": "structure|read|write" } } }, "git_commit": "<40 hex characters>", "migrations": [ { "name": "<1 to 200 letters, digits, _ and ->", "sql": "<the file drizzle-kit generated>" } ] (optional), "structures": { "<a declared Resource>": "sha256:<64 lower-case hex>" } (optional) }"#;

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
        match declared.get(&name) {
            None => {
                return Err("structures names a Resource the Version does not declare".into());
            }
            Some(resource) if resource.kind == ResourceKind::Storage => {
                return Err(
                    "structures names a Resource declared storage, which has no structure".into(),
                );
            }
            Some(_) => {}
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

pub(crate) fn read_version(body: &[u8]) -> Result<NewVersion, String> {
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
        if resource.kind == ResourceKind::Storage && matches!(resource.access, Access::Structure) {
            return Err(format!(
                "the Resource {name:?} is declared storage, whose access is read or write, not structure"
            ));
        }
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
const RESOURCE_FORM: &str = r#"{ "name": "<name>", "kind": "postgres" | "storage", "description": "<text>", "owner": "<text>", "contact": "<whom to ask for access>", "connection": { "host": "<host>", "port": <1 to 65535>, "database": "<database>", "read": { "role": "<role>", "password": "<password>" }, "write": { "role": "<role>", "password": "<password>" } (optional), "structure": { "role": "<role>", "password": "<password>" } (optional) } (for kind "postgres"; a Storage Resource has "location": { "backend": "directory", "directory": "<an absolute path beneath the storage root>" } instead of "connection") }"#;

/// A Resource to register, as the body asks.
enum ToRegister {
    Database(NewResource),
    /// A Storage Resource on `directory`, not yet checked.
    Storage {
        name: String,
        description: String,
        owner: String,
        contact: String,
        directory: String,
    },
}

/// `PUT /v1/resources/<name>/location`
const LOCATION_FORM: &str =
    r#"{ "backend": "directory", "directory": "<an absolute path beneath the storage root>" }"#;

/// The directory of a change of location. Its problems are fixed words that
/// never name what was sent.
fn read_location(body: &[u8]) -> Result<String, &'static str> {
    const LOCATION: &str = "the body is not an object { backend: \"directory\", directory: <a string> } with each key once";
    let object = Object::read(body).map_err(|_| LOCATION)?;
    let backend: String = object.fixed("backend", LOCATION)?;
    if backend != "directory" {
        return Err("backend is not \"directory\", the one backend there is");
    }
    object.fixed("directory", LOCATION)
}

/// Reads a Resource to register. Its problems are told in fixed words that
/// name a field, never anything the body holds.
fn read_resource(body: &[u8]) -> Result<ToRegister, &'static str> {
    const PORT: &str = "connection.port is missing or not a number from 1 to 65535";
    let object =
        Object::read(body).map_err(|_| "the body is not a JSON object with each key once")?;
    let kind: String = object.fixed("kind", "kind is missing or not a string")?;
    let name = object.fixed("name", "name is missing or not a string")?;
    let description: String =
        object.fixed("description", "description is missing or not a string")?;
    let owner: String = object.fixed("owner", "owner is missing or not a string")?;
    let contact: String = object.fixed("contact", "contact is missing or not a string")?;
    /// Bounds of a text of a Resource, as "Registering a database,
    /// checked: What the fields take" gives them.
    fn text_bounds(
        value: &str,
        min: usize,
        max: usize,
        line_ends: bool,
        field: &'static str,
    ) -> Result<(), &'static str> {
        const SIZE: &str = " is not within its bounds";
        const CONTROL: &str = " has a control character it may not have";
        if value.len() < min || value.len() > max || value.contains('\0') {
            return Err(match field {
                "description" => "description is 2,048 bytes at most, with no NUL",
                "owner" => "owner is 1 to 256 bytes, with no NUL",
                "contact" => "contact is 1 to 256 bytes, with no NUL",
                _ => SIZE,
            });
        }
        if value
            .chars()
            .any(|c| c.is_control() && !(line_ends && (c == '\n' || c == '\r')))
        {
            return Err(match field {
                "description" => "description has a control character other than a line end",
                "owner" => "owner has a control character",
                "contact" => "contact has a control character",
                _ => CONTROL,
            });
        }
        Ok(())
    }
    text_bounds(&description, 0, 2_048, true, "description")?;
    text_bounds(&owner, 1, 256, false, "owner")?;
    text_bounds(&contact, 1, 256, false, "contact")?;
    match kind.as_str() {
        "postgres" => {
            if object.0.contains_key("location") {
                return Err("a postgres Resource has connection and no location");
            }
        }
        "storage" => {
            if object.0.contains_key("connection") {
                return Err("a storage Resource has location and no connection");
            }
            const LOCATION: &str = "location is missing or not an object { backend: \"directory\", directory: <a string> } with each key once";
            let location = object.object("location").map_err(|_| LOCATION)?;
            let backend: String = location.fixed("backend", LOCATION)?;
            if backend != "directory" {
                return Err("location.backend is not \"directory\", the one backend there is");
            }
            let directory = location.fixed("directory", LOCATION)?;
            return Ok(ToRegister::Storage {
                name,
                description,
                owner,
                contact,
                directory,
            });
        }
        _ => return Err("kind is not \"postgres\" or \"storage\""),
    }
    let connection = object
        .object("connection")
        .map_err(|_| "connection is missing or not an object with each key once")?;
    let host: String = connection.fixed("host", "connection.host is missing or not a string")?;
    let port: u16 = connection.fixed("port", PORT)?;
    let database: String =
        connection.fixed("database", "connection.database is missing or not a string")?;
    const HOST: &str = "connection.host is 1 to 253 bytes, with no NUL";
    const DATABASE: &str = "connection.database is 1 to 63 bytes, with no NUL";
    const ROLE: &str = "a login's role is 1 to 63 bytes, with no NUL";
    const PASSWORD: &str = "a login's password is 1 to 1,024 bytes, with no NUL";
    let bounded = |value: &str, min: usize, max: usize, message: &'static str| {
        if value.len() < min || value.len() > max || value.contains('\0') {
            Err(message)
        } else {
            Ok(())
        }
    };
    let read = connection
        .object("read")
        .map_err(|_| "connection.read is missing or not an object with each key once")?;
    let role: String = read.fixed("role", "connection.read.role is missing or not a string")?;
    let password: String = read.fixed(
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
    bounded(&host, 1, 253, HOST)?;
    bounded(&database, 1, 63, DATABASE)?;
    bounded(&role, 1, 63, ROLE)?;
    bounded(&password, 1, 1_024, PASSWORD)?;
    for login in [write.as_ref(), structure.as_ref()].into_iter().flatten() {
        bounded(login.role(), 1, 63, ROLE)?;
        bounded(login.password(), 1, 1_024, PASSWORD)?;
    }
    if port == 0 {
        return Err(PORT);
    }
    Ok(ToRegister::Database(NewResource {
        name,
        description,
        owner,
        contact,
        reach: NewReach::Database(Connection::new(
            host,
            port,
            database,
            Login::new(role, password),
            write,
            structure,
        )),
    }))
}

/// `POST /v1/resources/<name>/grants`
const GRANT_FORM: &str = r#"{ "group": "<Group: 1 to 256 bytes, no control characters>", "access": "structure|read|write" }"#;

fn read_grant(body: &[u8]) -> Result<(String, authz::AccessLevel), String> {
    let object = Object::read(body)?;
    let group: String = object.required("group")?;
    let access: String = object.required("access")?;
    if !resources::is_group(&group) {
        return Err("group is not 1 to 256 bytes without control characters".into());
    }
    let level = level_of(&access)
        .ok_or_else(|| "access is not one of structure, read, write".to_owned())?;
    Ok((group, level))
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

/// `POST /v1/applications/<name>/contributors`
const CONTRIBUTORS_FORM: &str = r#"{ "add": [ { "user": "<user id>" } | { "group": "<Group>" } | { "email": "<address>" }, ... ], "remove": [ ... ] } (each optional, together 1 to 100 entries, no entry in both)"#;

/// `POST /v1/applications/<name>/sharing`
const SHARING_FORM: &str = r#"{ "add": [ { "user": "<user id>" } | { "group": "<Group>" } | { "everyone": true } | { "email": "<address>" }, ... ], "remove": [ ... ] } (each optional, together 1 to 100 entries, no entry in both)"#;

/// The most entries one change of a list holds, `add` and `remove`
/// together.
const MAX_ENTRIES_A_CHANGE: usize = 100;

/// The longest a user id, a Group or an email may be, in bytes.
pub(crate) const MAX_ENTRY_BYTES: usize = 256;

/// Whether `text` can be a user id, a Group or an email in an entry: 1 to
/// 256 bytes, no control character.
pub(crate) fn entry_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ENTRY_BYTES && !text.chars().any(char::is_control)
}

/// One entry of a change: exactly one of `user`, `group`, `everyone` (for
/// whom it is shared with only, and only `true`) and `email`; other keys
/// are ignored.
fn read_entry(value: &RawValue, list: List) -> Result<Asked, String> {
    let entry = Object::read_value(value)?;
    let text = |key: &str| -> Result<Option<String>, String> {
        match entry.0.get(key) {
            None => Ok(None),
            Some(value) => {
                let text: String = serde_json::from_str(value.get())
                    .map_err(|_| format!("{key} is not a string"))?;
                if entry_text(&text) {
                    Ok(Some(text))
                } else {
                    Err(format!(
                        "{key} is not 1 to {MAX_ENTRY_BYTES} bytes without control characters"
                    ))
                }
            }
        }
    };
    let user = text("user")?;
    let group = text("group")?;
    let email = text("email")?;
    let everyone = match entry.0.get("everyone") {
        None => false,
        Some(value) if value.get() == "true" => true,
        Some(_) => return Err("everyone is not true".into()),
    };
    if everyone && list == List::Contributors {
        return Err("everyone cannot be a Contributor: name a person or a Group".into());
    }
    match (user, group, email, everyone) {
        (Some(id), None, None, false) => Ok(Asked::Entry(Named::User(id))),
        (None, Some(group), None, false) => Ok(Asked::Entry(Named::Group(group))),
        (None, None, Some(email), false) => Ok(Asked::Email(email)),
        (None, None, None, true) => Ok(Asked::Entry(Named::Everyone)),
        (None, None, None, false) => {
            Err("an entry names nothing: give user, group, email or everyone".into())
        }
        _ => Err(
            "an entry names more than one thing: give one of user, group, email, everyone".into(),
        ),
    }
}

/// The body of a change of `list`: `add` and `remove`, each an array when
/// given, at least one given, 1 to [`MAX_ENTRIES_A_CHANGE`] entries in all,
/// none in both (an email compared letter case ignored).
fn read_change(body: &[u8], list: List) -> Result<Change, String> {
    let object = Object::read(body)?;
    let mut lists: [Vec<Asked>; 2] = [Vec::new(), Vec::new()];
    let mut given = false;
    for (key, into) in ["add", "remove"].into_iter().zip(&mut lists) {
        let Some(value) = object.0.get(key) else {
            continue;
        };
        given = true;
        let entries: Vec<&RawValue> = serde_json::from_str(value.get())
            .map_err(|_| format!("{key} is not an array of entries"))?;
        if entries.len() > MAX_ENTRIES_A_CHANGE {
            return Err(format!(
                "more than {MAX_ENTRIES_A_CHANGE} entries in one request"
            ));
        }
        for (index, entry) in entries.into_iter().enumerate() {
            into.push(read_entry(entry, list).map_err(|e| format!("{key}[{index}]: {e}"))?);
        }
    }
    let [add, remove] = lists;
    if !given {
        return Err("neither add nor remove is given".into());
    }
    let count = add.len().saturating_add(remove.len());
    if count == 0 {
        return Err("no entry is given".into());
    }
    if count > MAX_ENTRIES_A_CHANGE {
        return Err(format!(
            "more than {MAX_ENTRIES_A_CHANGE} entries in one request"
        ));
    }
    let same = |a: &Asked, b: &Asked| match (a, b) {
        (Asked::Email(a), Asked::Email(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => a == b,
    };
    if add.iter().any(|a| remove.iter().any(|r| same(a, r))) {
        return Err("the same entry is both in add and in remove".into());
    }
    Ok(Change { list, add, remove })
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

/// What a change of a list came to, for the API and the Dashboard alike.
pub(crate) enum ListChanged {
    NoSuchApplication,
    NotAContributor,
    /// The change could not be read: what was wrong.
    Malformed(String),
    /// Asked of the registry: made, or refused by its rules.
    Made(Changed),
}

pub struct Api {
    registry: Arc<Registry>,
    resources: Arc<Resources>,
    /// The one list of hosts Applications may call, from memory.
    outbound: Arc<crate::outbound::Outbound>,
    /// The storage root and the file threads, for registering a Storage
    /// Resource.
    storage: Arc<Storage>,
    log: Arc<OperationLog>,
    /// Applications' log entries, read here.
    logs: Arc<Logs>,
    credentials: Arc<ToolCredentials>,
    domain: Arc<str>,
    signing: Signing,
    /// The readings for Platform administrators running at once (`GET
    /// /v1/admin/applications`, `GET /v1/operation-log`): at most
    /// [`ADMIN_READINGS`]; one more is answered `503` at once.
    readings: tokio::sync::Semaphore,
    /// The checks of logins running now: at most [`CHECKS_AT_ONCE`].
    checks: tokio::sync::Semaphore,
}

/// At most this many checks of logins run at once in the program; a third
/// registration or replacement is `503 busy` at once.
pub const CHECKS_AT_ONCE: usize = 2;

/// Readings for Platform administrators at once in the program.
const ADMIN_READINGS: usize = 2;

/// The whole check of a registration's or a replacement's logins: 10
/// seconds (blackbox/CONTRACT.md, "Resources and Grants in full: Bounds").
const CHECK_WHOLE: std::time::Duration = std::time::Duration::from_secs(10);

/// The longest a write waits for the Resource's turn: the bound of a write
/// to the platform database.
const WRITE_TURN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// `409 too-many`: a 1,001st Resource.
fn too_many() -> Answer {
    Answer::error(
        StatusCode::CONFLICT,
        "too-many",
        "the installation holds the 1,000 Resources it may hold: remove one before registering another",
    )
}

/// `503 busy`: two checks are running already.
fn checks_busy() -> Answer {
    Answer::error(
        StatusCode::SERVICE_UNAVAILABLE,
        "busy",
        "two checks of a Resource's logins are running now: nothing was connected or stored; try again shortly",
    )
}

/// `422 login-failed`: every login that failed, in the order read, write,
/// structure, and nothing of what it holds.
fn login_failed(failures: &[resources::Failure]) -> Answer {
    let failed: Vec<Value> = failures
        .iter()
        .map(|failure| {
            json!({
                "login": failure.login.word(),
                "why": failure.why,
            })
        })
        .collect();
    let named: Vec<&str> = failures.iter().map(|f| f.login.word()).collect();
    let message = format!(
        "the {} login of the Resource cannot be used: nothing was stored. Look at the role and its password at the shared database, and at the host and port; then register the Resource again",
        named.join(", ")
    );
    Answer::json(
        StatusCode::UNPROCESSABLE_ENTITY,
        &json!({
            "error": { "reason": "login-failed", "message": message, "failed": failed },
        }),
    )
}

/// The answer of a registration: the Resource, and the warnings of its
/// check (always there).
#[derive(Serialize)]
struct RegisteredView<'a> {
    #[serde(flatten)]
    resource: ResourceView<'a>,
    warnings: &'a [Value],
}

/// The most Grants a Resource's administrator view shows.
const MAX_GRANTS_SHOWN: usize = 200;

/// The most Grants `GET /v1/resources/<name>/grants` answers.
const MAX_GRANTS_LISTED: usize = resources::MAX_GRANTS;

/// `503 unavailable`: another change of the Resource holds its turn; nothing
/// was sent.
fn busy_resource() -> Answer {
    Answer::error(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "another change of this Resource is running now: nothing was changed; try again shortly",
    )
}

/// `409 too-many`: a 1,001st Grant for a Group that held none.
fn too_many_grants() -> Answer {
    Answer::error(
        StatusCode::CONFLICT,
        "too-many",
        "the Resource holds the 1,000 Grants it may hold: revoke one that is no longer needed before giving another",
    )
}

/// The most one page of `GET /v1/admin/resources` may hold.
const MAX_ADMIN_LIST_BYTES: usize = 4 * 1024 * 1024;

const ADMIN_RESOURCES_FORM: &str = "?limit=<1 to 100>&after=<a Resource's name>";

const PATCH_FORM: &str =
    r#"{ "description"?: "<text>", "owner"?: "<text>", "contact"?: "<text>" } (at least one)"#;

/// `PATCH /v1/resources/<name>`: the fields given, each checked as a
/// registration's.
struct TextChange {
    description: Option<String>,
    owner: Option<String>,
    contact: Option<String>,
}

impl TextChange {
    fn fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.description.is_some() {
            fields.push("description");
        }
        if self.owner.is_some() {
            fields.push("owner");
        }
        if self.contact.is_some() {
            fields.push("contact");
        }
        fields
    }
}

const LOGINS_FORM: &str = r#"{ "host"?: "<host>", "port"?: <1 to 65535>, "database"?: "<database>", "read"?: { "role", "password" }, "write"?: { "role", "password" } | null, "structure"?: { "role", "password" } | null } (at least one)"#;

/// `PUT .../connection`: the members given, each checked as a
/// registration's.
#[derive(Default)]
struct LoginsChange {
    host: Option<String>,
    port: Option<u16>,
    database: Option<String>,
    read: Option<resources::Login>,
    write: Option<Option<resources::Login>>,
    structure: Option<Option<resources::Login>>,
    members: Vec<&'static str>,
}

fn read_logins_change(body: &[u8]) -> Result<LoginsChange, &'static str> {
    const HOST: &str = "host is 1 to 253 bytes, with no NUL";
    const DATABASE: &str = "database is 1 to 63 bytes, with no NUL";
    const ROLE: &str = "a login's role is 1 to 63 bytes, with no NUL";
    const PASSWORD: &str = "a login's password is 1 to 1,024 bytes, with no NUL";
    let object =
        Object::read(body).map_err(|_| "the body is not a JSON object with each key once")?;
    let bounded = |value: &str, min: usize, max: usize, message: &'static str| {
        if value.len() < min || value.len() > max || value.contains('\0') {
            Err(message)
        } else {
            Ok(())
        }
    };
    let login = |value: &RawValue| -> Result<resources::Login, &'static str> {
        let login = Object::read_value(value)
            .map_err(|_| "a login is not an object { role, password } with each key once")?;
        let role: String = login.fixed("role", "a login's role is missing or not a string")?;
        let password: String =
            login.fixed("password", "a login's password is missing or not a string")?;
        bounded(&role, 1, 63, ROLE)?;
        bounded(&password, 1, 1_024, PASSWORD)?;
        Ok(resources::Login::new(role, password))
    };
    let mut change = LoginsChange::default();
    if let Some(value) = object.0.get("host") {
        let host: String = serde_json::from_str(value.get()).map_err(|_| "host is not a string")?;
        bounded(&host, 1, 253, HOST)?;
        change.host = Some(host);
        change.members.push("host");
    }
    if let Some(value) = object.0.get("port") {
        let port: u16 = serde_json::from_str(value.get())
            .map_err(|_| "port is not a number from 1 to 65535")?;
        if port == 0 {
            return Err("port is not a number from 1 to 65535");
        }
        change.port = Some(port);
        change.members.push("port");
    }
    if let Some(value) = object.0.get("database") {
        let database: String =
            serde_json::from_str(value.get()).map_err(|_| "database is not a string")?;
        bounded(&database, 1, 63, DATABASE)?;
        change.database = Some(database);
        change.members.push("database");
    }
    if let Some(value) = object.0.get("read") {
        if value.get() == "null" {
            return Err("read cannot be removed: a Resource always has its read login");
        }
        change.read = Some(login(value)?);
        change.members.push("read");
    }
    for (member, slot) in [
        ("write", &mut change.write),
        ("structure", &mut change.structure),
    ] {
        let Some(value) = object.0.get(member) else {
            continue;
        };
        *slot = if value.get() == "null" {
            Some(None)
        } else {
            Some(Some(login(value)?))
        };
        change.members.push(member);
    }
    if change.members.is_empty() {
        return Err("at least one member of the connection must be given");
    }
    Ok(change)
}

fn read_text_change(body: &[u8]) -> Result<TextChange, &'static str> {
    let object =
        Object::read(body).map_err(|_| "the body is not a JSON object with each key once")?;
    let mut change = TextChange {
        description: None,
        owner: None,
        contact: None,
    };
    for (field, slot) in [
        ("description", &mut change.description),
        ("owner", &mut change.owner),
        ("contact", &mut change.contact),
    ] {
        let Some(value) = object.0.get(field) else {
            continue;
        };
        let text: String = serde_json::from_str(value.get()).map_err(|_| match field {
            "description" => "description is not a string",
            "owner" => "owner is not a string",
            _ => "contact is not a string",
        })?;
        let (min, max, line_ends) = match field {
            "description" => (0, 2_048, true),
            _ => (1, 256, false),
        };
        if text.len() < min || text.len() > max || text.contains('\0') {
            return Err(match field {
                "description" => "description is 2,048 bytes at most, with no NUL",
                "owner" => "owner is 1 to 256 bytes, with no NUL",
                _ => "contact is 1 to 256 bytes, with no NUL",
            });
        }
        if text
            .chars()
            .any(|c| c.is_control() && !(line_ends && (c == '\n' || c == '\r')))
        {
            return Err(match field {
                "description" => "description has a control character other than a line end",
                "owner" => "owner has a control character",
                _ => "contact has a control character",
            });
        }
        *slot = Some(text);
    }
    if change.description.is_none() && change.owner.is_none() && change.contact.is_none() {
        return Err("at least one of description, owner or contact is needed");
    }
    Ok(change)
}

/// `?after` and `?limit` of `GET /v1/admin/resources`.
fn resource_list_query(query: Option<&str>) -> Result<(Option<String>, usize), String> {
    let mut found = query_parameters(query, &["after", "limit"])
        .map_err(|name| format!("?{name} is given twice or is not percent-encoded"))?;
    let limit = match found.get("limit") {
        None => 100,
        Some(text) => plain_number(text)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| {
                "?limit must be a whole number from 1 to 100, written plainly".to_owned()
            })?,
    };
    let after = found.remove("after");
    if let Some(after) = &after
        && !is_resource_name(after)
    {
        return Err("?after must be a name a Resource could have: 1 to 63 lower-case letters, digits and underscores, starting with a letter".into());
    }
    Ok((after, limit))
}

#[derive(Serialize)]
struct RemovedDeclaredBy {
    application: String,
    version: u64,
    access: &'static str,
}

#[derive(Serialize)]
struct LocationChangedView<'a> {
    resource: &'a str,
    location: AdminLocation,
    changed: bool,
    files_moved: bool,
    message: &'a str,
}

#[derive(Serialize)]
struct RemoveView<'a> {
    name: &'a str,
    kind: &'static str,
    grants_removed: u64,
    declared_by: Vec<RemovedDeclaredBy>,
    declared_by_more: u64,
    directory_cleaned: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct AdminResourcesView {
    pub(crate) resources: Vec<AdminResourceView>,
    pub(crate) next: Option<String>,
}

/// One Resource's page: what `GET /v1/admin/resources/<name>` answers of
/// it, and every Grant `GET /v1/resources/<name>/grants` answers (up to
/// 1,000), for the Dashboard's page of it.
pub(crate) struct AdminResourcePage {
    pub name: String,
    pub kind: &'static str,
    pub description: String,
    pub owner: String,
    pub contact: String,
    pub created_by: String,
    pub created_at: String,
    pub usable: bool,
    pub problem: Option<&'static str>,
    pub logins: Option<resources::Logins>,
    pub location: Option<AdminLocation>,
    pub grants: Vec<resources::GrantEntry>,
    pub grants_more: usize,
}

impl AdminResourcePage {
    /// What `GET /v1/admin/resources/<name>` answers of it: the first 200
    /// Grants, and how many more.
    fn view(&self) -> AdminResourceView {
        let shown = self.grants.len().min(MAX_GRANTS_SHOWN);
        let entries = self.grants.get(..shown).unwrap_or(self.grants.as_slice());
        AdminResourceView {
            name: self.name.clone(),
            kind: self.kind,
            description: self.description.clone(),
            owner: self.owner.clone(),
            contact: self.contact.clone(),
            created_by: self.created_by.clone(),
            created_at: self.created_at.clone(),
            usable: self.usable,
            problem: self.problem,
            logins: self.logins,
            location: self.location.as_ref().map(|location| AdminLocation {
                backend: location.backend,
                directory: location.directory.clone(),
            }),
            grants: entries
                .iter()
                .map(|entry| AdminGrant {
                    group: entry.group.clone(),
                    access: resources::level_name(entry.level),
                    granted_by: entry.granted_by.as_deref().map(str::to_owned),
                    granted_at: entry.granted_at.as_deref().map(str::to_owned),
                })
                .collect(),
            grants_more: self.grants_more + (self.grants.len() - shown),
        }
    }
}

/// Why a Resource's page or the list of them answered nothing.
pub(crate) enum AdminResourceRead {
    Found(Box<AdminResourcePage>),
    NoSuchResource,
    /// The Grants on this Resource cannot be told now.
    Unknown(String),
    Failed(DbError),
}

#[derive(Serialize)]
pub(crate) struct AdminResourceView {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) description: String,
    pub(crate) owner: String,
    pub(crate) contact: String,
    pub(crate) created_by: String,
    pub(crate) created_at: String,
    pub(crate) usable: bool,
    pub(crate) problem: Option<&'static str>,
    pub(crate) logins: Option<resources::Logins>,
    pub(crate) location: Option<AdminLocation>,
    pub(crate) grants: Vec<AdminGrant>,
    pub(crate) grants_more: usize,
}

#[derive(Serialize)]
pub(crate) struct AdminLocation {
    pub(crate) backend: &'static str,
    pub(crate) directory: String,
}

#[derive(Serialize)]
pub(crate) struct AdminGrant {
    pub(crate) group: String,
    pub(crate) access: &'static str,
    pub(crate) granted_by: Option<String>,
    pub(crate) granted_at: Option<String>,
}

/// The answer of a replacement: the administrators' view and the warnings
/// of the check.
#[derive(Serialize)]
struct AdminChangedView {
    #[serde(flatten)]
    resource: AdminResourceView,
    warnings: Vec<Value>,
}

/// A Storage Resource's problem as "Storage: When the directory cannot be
/// used" names it in an administrator's answer.
fn directory_problem_word(problem: &'static str) -> &'static str {
    match problem {
        "missing" => "directory-missing",
        "link" => "directory-link",
        "outside-storage-root" => "outside-storage-root",
        "not-a-directory" => "not-a-directory",
        "not-marked" | "marked-for-another" => "directory-in-use",
        _ => "directory-unusable",
    }
}

/// The longest a request for Platform administrators reads or writes the
/// platform database (blackbox/CONTRACT.md, "What Platform administrators
/// see and stop: Bounds and costs").
const ADMIN_READ_TIME: Duration = Duration::from_secs(10);

/// The largest answer of `GET /v1/operation-log`: 4 MiB.
const MAX_LOG_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// The most Resources of an Application's Declared access the list of
/// every Application shows.
const MAX_DECLARED_LISTED: usize = 200;

/// The most Applications to a page of the list of every Application.
pub(crate) const MAX_ADMIN_PAGE: u8 = 100;

impl Api {
    /// Each registered Resource's kind, by name: for the pages that show a
    /// Resource's mark. From memory.
    pub(crate) fn resource_kinds(&self) -> BTreeMap<String, &'static str> {
        self.resources
            .list()
            .into_iter()
            .map(|resource| (resource.name.clone(), resource.kind.name()))
            .collect()
    }

    /// Every entry of the outbound list, in the byte order of `host`, as
    /// the Dashboard's page of it shows them: from memory, as the API
    /// answers them.
    pub(crate) fn outbound_entries(&self) -> Vec<crate::outbound::Kept> {
        self.outbound.entries()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the parts the API answers from, each named, made once at start"
    )]
    pub fn new(
        registry: Arc<Registry>,
        resources: Arc<Resources>,
        outbound: Arc<crate::outbound::Outbound>,
        storage: Arc<Storage>,
        log: Arc<OperationLog>,
        logs: Arc<Logs>,
        credentials: Arc<ToolCredentials>,
        domain: Arc<str>,
        signing: Signing,
    ) -> Self {
        Self {
            registry,
            resources,
            outbound,
            storage,
            log,
            logs,
            credentials,
            domain,
            signing,
            readings: tokio::sync::Semaphore::new(ADMIN_READINGS),
            checks: tokio::sync::Semaphore::new(CHECKS_AT_ONCE),
        }
    }

    /// The Group whose members are Platform administrators now: the
    /// connection's in use, else the stand-in's.
    fn administrators(&self) -> Option<Arc<str>> {
        match self.signing.oidc.as_ref().and_then(|o| o.connected()) {
            Some(connected) => Some(Arc::clone(&connected.administrators)),
            None => self.signing.administrators.as_ref().map(Arc::clone),
        }
    }

    pub(crate) fn is_administrator(&self, user: &User) -> bool {
        self.administrators()
            .is_some_and(|group| user.groups.iter().any(|g| *g == *group))
    }

    /// Answers one request of `caller`; `request` is its id.
    pub async fn handle<B>(
        &self,
        caller: Caller<'_>,
        request: &str,
        method: &Method,
        path: &str,
        query: Option<&str>,
        body: B,
    ) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let route = Route::of(path);
        // A setup credential is good for one thing only: nothing else is
        // read of a request made with it.
        if let Caller::Setup(_) = caller
            && !(matches!(route, Some(Route::Oidc)) && method == Method::PUT)
        {
            return oidc_connection::setup_refused();
        }
        // Someone not signed in sends a short body, or none. The requests of
        // "Resources and Grants in full" take 64 KiB whoever sends them,
        // which is checked before the administrators' (CONTRACT.md, "Who
        // may"), so it comes before the caller is known too.
        let signed_in = matches!(caller, Caller::User(..));
        let small = matches!(
            route,
            Some(
                Route::Resources
                    | Route::Resource(_)
                    | Route::Connection(_)
                    | Route::Location(_)
                    | Route::Grants(_)
                    | Route::GrantRevoke(..)
                    | Route::AdminGrants
                    | Route::AdminResources
                    | Route::AdminResource(_)
            )
        );
        let limit = if small || !signed_in {
            MAX_BODY_BYTES_NOT_SIGNED_IN
        } else {
            MAX_BODY_BYTES
        };
        let read = Limited::new(body, limit).collect();
        let body = match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(_)) if small || !signed_in => {
                return Answer::error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too-large",
                    "the body is larger than 64 KiB: send what this path takes",
                );
            }
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
        let Some(route) = route else {
            return Answer::not_found(
                "the API has no such path: its paths are /v1/me, /v1/installation, /v1/tool-credentials, /v1/tool-credentials/current, /v1/tool-sign-ins, /v1/oidc, /v1/applications, /v1/applications/<name>, /v1/applications/<name>/versions, /v1/applications/<name>/versions/<number>, /v1/applications/<name>/go-back, /v1/applications/<name>/logs, /v1/applications/<name>/contributors, /v1/applications/<name>/sharing, /v1/applications/<name>/disable, /v1/applications/<name>/enable, /v1/admin/applications, /v1/admin/resources, /v1/admin/resources/<name>, /v1/resources, /v1/resources/<name>, /v1/resources/<name>/grants, /v1/resources/<name>/grants/<group>, /v1/admin/grants, /v1/resources/<name>/structure, /v1/resources/<name>/connection, /v1/outbound-hosts, /v1/outbound-hosts/<entry> and /v1/operation-log",
            );
        };
        match route {
            Route::Installation => return self.installation(method),
            Route::ToolSignIns => return self.tool_sign_ins(method, &body).await,
            _ => {}
        }
        let (user, with) = match caller {
            Caller::User(user, with) => (user, with),
            Caller::Setup(digest) => return self.connect_with_setup(digest, &body).await,
            Caller::Anyone => return not_signed_in(),
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
            Route::Resource(name) => self.resource(user, method, name, &body).await,
            Route::Connection(name) => self.connection(user, method, name, &body).await,
            Route::Location(name) => self.location(user, method, name, &body).await,
            Route::AdminResources => Ok(self.admin_resources(method, query).await),
            Route::AdminResource(name) => self.admin_resource(method, name).await,
            Route::AdminGrants => Ok(self.admin_grants(method, query).await),
            Route::OutboundHosts => Ok(self.outbound_hosts(method)),
            Route::OutboundEntry(entry) => Ok(self.outbound_entry(user, method, entry).await),
            Route::Me => Ok(self.me(user, method)),
            Route::ToolCredentials => Ok(self.make_credential(user, with, method, &body).await),
            Route::CurrentToolCredential => self.end_credential(with, method).await,
            Route::Grants(name) => self.grants(user, method, name, &body).await,
            Route::GrantRevoke(name, group) => {
                Ok(self.revoke_grant(user, method, name, group, &body).await)
            }
            Route::Structure(name) => Ok(self.structure(user, request, method, name).await),
            Route::OperationLog => Ok(self.operation_log(method, query).await),
            Route::AdminApplications => Ok(self.every_application(method, query).await),
            Route::Disable(name, disable) => {
                Ok(self.disable(user, method, name, disable, &body).await)
            }
            Route::Applications => self.applications(user, method, &body).await,
            Route::Application(name) => self.application(user, method, name).await,
            Route::Versions(name) => self.versions(user, method, name, query, &body).await,
            Route::Version(name, number) => self.version(user, method, name, number).await,
            Route::GoBack(name) => self.go_back(user, method, name, &body).await,
            Route::Logs(name) => self.logs(user, method, name, query).await,
            Route::Contributors(name) => {
                self.change_list(user, method, name, List::Contributors, &body)
                    .await
            }
            Route::Sharing(name) => {
                self.change_list(user, method, name, List::SharedWith, &body)
                    .await
            }
            Route::Oidc => self.oidc(user, method, &body).await,
            Route::Installation | Route::ToolSignIns => Ok(not_signed_in()),
        };
        answer.unwrap_or_else(|error| Answer::unavailable(&error))
    }

    /// `GET /v1/outbound-hosts`, by anyone signed in: the list, in the byte
    /// order of `host`, from memory (blackbox/CONTRACT.md, "Outbound
    /// calls: The list").
    fn outbound_hosts(&self, method: &Method) -> Answer {
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let entries = self.outbound.entries();
        let hosts: Vec<OutboundHostView<'_>> = entries
            .iter()
            .map(|kept| OutboundHostView {
                host: kept.host(),
                added_by: kept.added_by(),
                added_at: kept.added_at(),
            })
            .collect();
        Answer::json(StatusCode::OK, &OutboundHostsView { hosts })
    }

    /// `PUT` and `DELETE /v1/outbound-hosts/<entry>`, by a Platform
    /// administrator (the caller checked before the entry is looked at):
    /// the entry read as the list reads it, then added or removed, in the
    /// platform database and in memory, within the bound of an
    /// administrators' request.
    async fn outbound_entry(&self, user: &User, method: &Method, written: &str) -> Answer {
        let add = if *method == Method::PUT {
            true
        } else if *method == Method::DELETE {
            false
        } else {
            return Answer::method_not_allowed(method, "PUT or DELETE");
        };
        let entry = match percent_decoded(written) {
            Some(text) => crate::outbound::Entry::parse(&text).map_err(|problem| {
                Answer::error(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    &crate::outbound::problem_words(problem),
                )
            }),
            None => Err(Answer::error(
                StatusCode::BAD_REQUEST,
                "malformed",
                "the entry is not percent-encoded as a path segment: encode it, such as api.example.com%3A8443",
            )),
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(refused) => return refused,
        };
        if add {
            match crate::outbound::Outbound::add(&self.outbound, &entry, &user.id).await {
                Ok(put) => {
                    if put.changed {
                        log::record(
                            "outbound-host-added",
                            json!({ "host": put.kept.host(), "by": &user.id }),
                        );
                    }
                    Answer::json(
                        StatusCode::OK,
                        &OutboundAddedView {
                            host: put.kept.host(),
                            added_by: put.kept.added_by(),
                            added_at: put.kept.added_at(),
                            changed: put.changed,
                        },
                    )
                }
                Err(crate::outbound::AddProblem::TooMany) => Answer::error(
                    StatusCode::CONFLICT,
                    "too-many",
                    &format!(
                        "the list holds {} hosts already, the most it takes: remove one with `vibe admin outbound remove <host>`, then add this one",
                        crate::outbound::MOST
                    ),
                ),
                Err(crate::outbound::AddProblem::Database(error)) => Answer::unavailable(&error),
            }
        } else {
            match crate::outbound::Outbound::remove(&self.outbound, &entry).await {
                Ok(removed) => {
                    if removed.changed {
                        log::record(
                            "outbound-host-removed",
                            json!({ "host": &removed.host, "by": &user.id }),
                        );
                    }
                    Answer::json(
                        StatusCode::OK,
                        &OutboundRemovedView {
                            host: &removed.host,
                            changed: removed.changed,
                        },
                    )
                }
                Err(error) => Answer::unavailable(&error),
            }
        }
    }

    /// Why OIDC cannot be connected now, if it cannot: the stand-in first,
    /// then no installation's key.
    fn cannot_connect(&self) -> Option<Answer> {
        if self.signing.stand_in {
            return Some(Answer::error(
                StatusCode::CONFLICT,
                "stand-in-sign-in",
                "this installation was started with --stand-in-sign-in, which cannot be on together with OIDC: start it without the stand-in to connect OIDC",
            ));
        }
        if self.signing.oidc.is_none() {
            return Some(Answer::error(
                StatusCode::CONFLICT,
                "no-secret-key",
                "this installation was started without an installation's key (VIBE_SECRET_KEY), so it can keep no client secret: start it with one, then connect OIDC",
            ));
        }
        None
    }

    /// `PUT /v1/oidc` with the setup credential: who asks first (`401` for
    /// one that is not the live one, whatever else is wrong), then as for a
    /// Platform administrator.
    async fn connect_with_setup(&self, digest: Digest, body: &[u8]) -> Answer {
        match setup::is_live(&self.signing.database, digest.bytes()).await {
            Ok(true) => {}
            Ok(false) => return oidc_connection::setup_refused(),
            Err(error) => return Answer::unavailable(&error),
        }
        if let Some(refused) = self.cannot_connect() {
            return refused;
        }
        match &self.signing.oidc {
            Some(oidc) => oidc.connect(body, By::Setup(digest.bytes())).await,
            None => oidc_connection::setup_refused(),
        }
    }

    /// `GET` and `PUT /v1/oidc`, by a Platform administrator.
    async fn oidc(&self, user: &User, method: &Method, body: &[u8]) -> Result<Answer, DbError> {
        if method == Method::GET {
            return Ok(match self.oidc_connection().await? {
                Some(stored) => Answer::json(StatusCode::OK, &stored.view()),
                None => Answer::not_found(
                    "OIDC is not connected: a Platform administrator connects it with `vibe admin connect-oidc`",
                ),
            });
        }
        if method != Method::PUT {
            return Ok(Answer::method_not_allowed(method, "GET or PUT"));
        }
        if let Some(refused) = self.cannot_connect() {
            return Ok(refused);
        }
        Ok(match &self.signing.oidc {
            Some(oidc) => oidc.connect(body, By::User(&user.id)).await,
            None => not_signed_in(),
        })
    }

    /// `GET /v1/installation`, by anyone: how to sign in.
    fn installation(&self, method: &Method) -> Answer {
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let (domain, sign_in, address) = self.installation_facts();
        Answer::json(
            StatusCode::OK,
            &json!({ "domain": domain, "sign_in": sign_in, "sign_in_address": address }),
        )
    }

    /// `POST /v1/tool-sign-ins`, by anyone.
    async fn tool_sign_ins(&self, method: &Method, body: &[u8]) -> Answer {
        if method != Method::POST {
            return Answer::method_not_allowed(method, "POST");
        }
        match &self.signing.oidc {
            Some(oidc) => oidc.take_tool_sign_in(body).await,
            None => Answer::error(
                StatusCode::CONFLICT,
                "sign-in-not-confirmed",
                "no sign-in of the tool has been confirmed: this installation does not sign in through the browser",
            ),
        }
    }

    async fn applications(
        &self,
        user: &User,
        method: &Method,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method == Method::GET {
            let listed = self.registry.listed_for(user, Listing::All).await?;
            let view = ListedApplicationsView {
                applications: listed
                    .iter()
                    .map(|a| ListedApplicationView {
                        name: &a.name,
                        description: a.description.as_deref(),
                        address: format!("{}.{}", a.name, self.domain),
                        contributor: a.contributor,
                        serving_version: a.serving_version,
                        disabled: a.disabled_since.is_some(),
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
                Answer::json(StatusCode::CREATED, &self.view(&application, user))
            }
            Created::NameTaken => Answer::error(
                StatusCode::CONFLICT,
                "name-taken",
                "an Application with this name exists: choose another name",
            ),
        })
    }

    /// `GET /v1/applications/<name>`: by whoever can open it, and by a
    /// Platform administrator (blackbox/CONTRACT.md, "Contributors and
    /// sharing: On the platform's API").
    async fn application(
        &self,
        user: &User,
        method: &Method,
        name: &str,
    ) -> Result<Answer, DbError> {
        let Some(application) = self.find(name).await? else {
            return Ok(no_application());
        };
        if method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        if !application.can_open(user) && !self.is_administrator(user) {
            return Ok(not_shared());
        }
        Ok(Answer::json(StatusCode::OK, &self.view(&application, user)))
    }

    /// What `user` may do with `name`; `None` for no such Application,
    /// also for a name no Application can have, without asking the
    /// database.
    async fn standing(&self, name: &str, user: &User) -> Result<Option<Standing>, DbError> {
        if check_name(name).is_err() {
            return Ok(None);
        }
        self.registry.standing(name, user).await
    }

    /// Whether `user` may read the Application's Versions: a Contributor,
    /// and a Platform administrator, as for its logs. Someone it is only
    /// shared with uses it and does not read how it is made.
    fn may_read(&self, standing: Standing, user: &User) -> bool {
        standing.contributor || self.is_administrator(user)
    }

    /// `POST /v1/applications/<name>/contributors` and `.../sharing`
    /// (blackbox/CONTRACT.md, "Contributors and sharing: On the platform's
    /// API"): by a Contributor. Refused, in order: `not-found`,
    /// `method-not-allowed`, `not-a-contributor`, `malformed`; then the
    /// change's own refusals ([`Api::list_changed`], which the Dashboard's
    /// changes go through too).
    async fn change_list(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        list: List,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(no_application());
        };
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "POST"));
        }
        let form = match list {
            List::Contributors => CONTRIBUTORS_FORM,
            List::SharedWith => SHARING_FORM,
        };
        let made = match self
            .list_changed(user, name, standing, read_change(body, list))
            .await?
        {
            ListChanged::NoSuchApplication => return Ok(no_application()),
            ListChanged::NotAContributor => return Ok(not_a_contributor_of_lists()),
            ListChanged::Malformed(problem) => return Ok(Answer::malformed(&problem, form)),
            ListChanged::Made(made) => made,
        };
        Ok(match made {
            Changed::Changed {
                application,
                added,
                removed,
            } => Answer::json(
                StatusCode::OK,
                &ChangedView {
                    application: self.view(&application, user),
                    added: added.iter().map(EntryView::of).collect(),
                    removed: removed.iter().map(EntryView::of).collect(),
                },
            ),
            Changed::SameInBoth(person) => Answer::malformed(
                &format!(
                    "the person {person:?} is both in add and in remove (once by user id, once by email)"
                ),
                form,
            ),
            Changed::NoSuchApplication => no_application(),
            Changed::NotAContributor => not_a_contributor_of_lists(),
            Changed::UnknownEmail(email) => Answer::json(
                StatusCode::UNPROCESSABLE_ENTITY,
                &json!({ "error": {
                    "reason": "unknown-email",
                    "message": format!(
                        "nobody who signed in here has the email {email:?}: name the person by their user id instead (`vibe whoami` shows it to them), or ask them to sign in once; nothing was changed"
                    ),
                    "entry": { "email": email },
                } }),
            ),
            Changed::AmbiguousEmail(email) => Answer::json(
                StatusCode::CONFLICT,
                &json!({ "error": {
                    "reason": "ambiguous-email",
                    "message": format!(
                        "more than one person who signed in here has the email {email:?}: name the one you mean by their user id (`vibe whoami` shows it to them); nothing was changed"
                    ),
                    "entry": { "email": email },
                } }),
            ),
            Changed::LastContributor(removed) => Answer::json(
                StatusCode::CONFLICT,
                &json!({ "error": {
                    "reason": "last-contributor",
                    "message": format!(
                        "removing {} would leave the Application with no Contributor: add another Contributor first (or in the same request); nothing was changed",
                        described(&removed)
                    ),
                    "entries": removed.iter().map(EntryView::of).collect::<Vec<_>>(),
                } }),
            ),
            Changed::TooMany(added) => {
                let (most, what) = match list {
                    List::Contributors => (MAX_CONTRIBUTORS, "Contributors"),
                    List::SharedWith => (MAX_SHARED_WITH, "entries of whom it is shared with"),
                };
                Answer::json(
                    StatusCode::CONFLICT,
                    &json!({ "error": {
                        "reason": "too-many",
                        "message": format!(
                            "adding {} would give the Application more than {most} {what}: remove some in the same request, or share with a Group in place of many people; nothing was changed",
                            described(&added)
                        ),
                        "entries": added.iter().map(EntryView::of).collect::<Vec<_>>(),
                    } }),
                )
            }
        })
    }

    /// A change of whom `name` is shared with, asked from the Dashboard
    /// (crate::dashboard): the API's own rules and refusals, in its order
    /// (`not-found`, `not-a-contributor`, `malformed`, then the change's).
    pub(crate) async fn change_sharing(
        &self,
        user: &User,
        name: &str,
        change: Result<Change, String>,
    ) -> Result<ListChanged, DbError> {
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(ListChanged::NoSuchApplication);
        };
        self.list_changed(user, name, standing, change).await
    }

    /// The part of a change of a list after the Application was found: by
    /// a Contributor, of a change that could be read; made by the registry
    /// ([`Registry::change`]), and recorded in the program's log when it
    /// changed something.
    async fn list_changed(
        &self,
        user: &User,
        name: &str,
        standing: Standing,
        change: Result<Change, String>,
    ) -> Result<ListChanged, DbError> {
        if !standing.contributor {
            return Ok(ListChanged::NotAContributor);
        }
        let change = match change {
            Ok(change) => change,
            Err(problem) => return Ok(ListChanged::Malformed(problem)),
        };
        let list = change.list;
        let made = self.registry.change(name, user, change).await?;
        if let Changed::Changed { added, removed, .. } = &made
            && (!added.is_empty() || !removed.is_empty())
        {
            let event = match list {
                List::Contributors => "contributors-changed",
                List::SharedWith => "sharing-changed",
            };
            log::record(
                event,
                json!({
                    "application": name,
                    "by": &user.id,
                    "added": added.iter().map(KeptView::of).collect::<Vec<_>>(),
                    "removed": removed.iter().map(KeptView::of).collect::<Vec<_>>(),
                }),
            );
        }
        Ok(ListChanged::Made(made))
    }

    async fn versions(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        query: Option<&str>,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(no_application());
        };
        if method == Method::GET {
            if !self.may_read(standing, user) {
                return Ok(not_a_reader_of_versions());
            }
            let Some(limit) = versions_limit(query) else {
                return Ok(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "the parameters are not the ones asked for: ?limit is a whole number from 1 to 1000, at most once",
                ));
            };
            let Some(versions) = self.registry.versions(name, limit).await? else {
                return Ok(no_application());
            };
            let versions = VersionsView {
                versions: versions.iter().map(VersionView::of).collect(),
            };
            return Ok(Answer::json(StatusCode::OK, &versions));
        }
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "GET or POST"));
        }
        if !standing.contributor {
            return Ok(not_a_contributor());
        }
        let version = match read_version(body) {
            Ok(version) => version,
            Err(problem) => return Ok(Answer::malformed(&problem, VERSION_FORM)),
        };
        Ok(
            match self.registry.send_version(name, version, user).await? {
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
                Sent::OwnDatabaseUnavailable { step, cause } => {
                    log::record(
                        "own-database-failed",
                        json!({ "application": name, "step": step.words, "code": cause.sqlstate() }),
                    );
                    Answer::error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "the platform could not prepare the Application's own database: nothing was applied; try again shortly",
                    )
                }
                Sent::OwnDatabaseNotServed => Answer::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "the Application's own database is not served now, so nothing of this Version was applied or recorded: the program was started without the installation's key, or the own database's passwords cannot be read. A Platform administrator starts the program with VIBE_SECRET_KEY (32 random bytes in base64); with another key, the own database gets new passwords at its next use, and the Version can be sent again",
                ),
                Sent::NoSecretKey => Answer::error(
                    StatusCode::CONFLICT,
                    "no-secret-key",
                    "a first Version makes the Application's own database, whose role passwords are sealed under the installation's key, and this program was started without it: start the program with VIBE_SECRET_KEY (32 random bytes in base64, as `openssl rand -base64 32` makes them), then send the Version again",
                ),
            },
        )
    }

    async fn version(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        number: Option<u64>,
    ) -> Result<Answer, DbError> {
        // Who may read is decided before anything is said of a Version,
        // so that its `not-found` tells nobody else how many there are:
        // to them `method-not-allowed`, then `not-a-contributor`. To a
        // reader, `not-found` comes first, as for any path.
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(no_application());
        };
        if !self.may_read(standing, user) {
            if method != Method::GET {
                return Ok(Answer::method_not_allowed(method, "GET"));
            }
            return Ok(not_a_reader_of_versions());
        }
        let version = match number {
            Some(number) => self.registry.version(name, number).await?,
            None => None,
        };
        let Some(version) = version else {
            return Ok(Answer::not_found(
                "no such Version: list the Application's Versions at /v1/applications/<name>/versions",
            ));
        };
        if method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        Ok(Answer::json(StatusCode::OK, &VersionView::of(&version)))
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
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(no_application());
        };
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "POST"));
        }
        if !standing.contributor {
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
        let Some(standing) = self.standing(name, user).await? else {
            return Ok(no_application());
        };
        if method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        if !standing.contributor && !self.is_administrator(user) {
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
            // Grants memory does not know now are read back, without
            // waiting; the User's access to a Resource is never guessed.
            let all = match self.known_resources().await {
                Ok(all) => all,
                Err(name) => return Ok(grants_unknown(&name)),
            };
            let groups = groups_of(user);
            let mut resources = Vec::with_capacity(all.len());
            for resource in &all {
                match ResourceRead::of(resource, &groups) {
                    Some(read) => resources.push(read),
                    None => return Ok(grants_unknown(&resource.name)),
                }
            }
            let resources = ResourcesView { resources };
            return Ok(Answer::json(StatusCode::OK, &resources));
        }
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "GET or POST"));
        }
        let new = match read_resource(body) {
            Ok(new) => new,
            Err(problem) => return Ok(Answer::malformed(problem, RESOURCE_FORM)),
        };
        let name = match &new {
            ToRegister::Database(new) => &new.name,
            ToRegister::Storage { name, .. } => name,
        };
        if !is_resource_name(name) {
            return Ok(Answer::error(
                StatusCode::BAD_REQUEST,
                "invalid-name",
                "a Resource's name is 1 to 63 lower-case letters, digits and underscores, starting with a letter",
            ));
        }
        let new = match new {
            ToRegister::Database(new) => {
                // A database Resource's logins are sealed under the
                // installation's key, which this program was started
                // without: nothing is registered (blackbox/CONTRACT.md,
                // "Resources and Grants in full: At start").
                if !self.resources.has_vault() {
                    return Ok(Answer::error(
                        StatusCode::CONFLICT,
                        "no-secret-key",
                        "a database Resource's logins are sealed under the installation's key, and this program was started without it: start the program with VIBE_SECRET_KEY (32 random bytes in base64, as `openssl rand -base64 32` makes them), then register the Resource again",
                    ));
                }
                // Name taken and too many, from memory, before any check
                // (CONTRACT.md, "Registering a database, checked: Order of
                // refusals").
                if self.resources.get(&new.name).is_some() {
                    return Ok(name_taken());
                }
                if self.resources.list().len() >= resources::MAX_RESOURCES {
                    return Ok(too_many());
                }
                new
            }
            ToRegister::Storage {
                name,
                description,
                owner,
                contact,
                directory,
            } => {
                return self
                    .register_storage(user, name, description, owner, contact, &directory)
                    .await;
            }
        };
        // Every login is connected before anything is stored, and no check
        // runs while another two do (CONTRACT.md, "Registering a database,
        // checked"; "Bounds").
        let Ok(_checking) = self.checks.try_acquire() else {
            return Ok(checks_busy());
        };
        let checked = match &new.reach {
            NewReach::Database(connection) => match tokio::time::timeout(
                CHECK_WHOLE,
                connection.check_logins(&connection.logins()),
            )
            .await
            {
                Ok(checked) => checked,
                Err(_) => resources::Checked {
                    failures: connection
                        .logins()
                        .into_iter()
                        .map(|login| resources::Failure {
                            login,
                            why: "timed-out",
                        })
                        .collect(),
                    warnings: Vec::new(),
                },
            },
            NewReach::Storage { .. } => resources::Checked::default(),
        };
        if !checked.failures.is_empty() {
            for failure in &checked.failures {
                log::record(
                    "resource-login-failed",
                    json!({
                        "resource": &new.name,
                        "login": failure.login.word(),
                        "why": failure.why,
                    }),
                );
            }
            return Ok(login_failed(&checked.failures));
        }
        let warnings: Vec<Value> = checked
            .warnings
            .iter()
            .map(resources::Warning::json)
            .collect();
        Ok(match self.resources.register(new, &user.id).await? {
            Registered::Registered(resource) => {
                log::record(
                    "resource-registered",
                    json!({
                        "resource": &resource.name,
                        "kind": resource.kind.name(),
                        "by": &user.id,
                    }),
                );
                for warning in &checked.warnings {
                    log::record(
                        "resource-login-warning",
                        json!({
                            "resource": &resource.name,
                            "login": warning.login.word(),
                            "reason": warning.reason,
                        }),
                    );
                }
                Answer::json(
                    StatusCode::CREATED,
                    &RegisteredView {
                        resource: ResourceView::of(&resource),
                        warnings: &warnings,
                    },
                )
            }
            Registered::NameTaken => name_taken(),
            Registered::TooMany => too_many(),
        })
    }

    /// `POST /v1/resources` of a Storage Resource (blackbox/CONTRACT.md,
    /// "Registering a Storage Resource"): the name, then the directory's
    /// checks in the table's order (crate::storage::Root), its marker and
    /// `.vibe-staging/` written, then the Resource registered. Nothing of
    /// the directory is in any answer.
    async fn register_storage(
        &self,
        user: &User,
        name: String,
        description: String,
        owner: String,
        contact: String,
        directory: &str,
    ) -> Result<Answer, DbError> {
        if self.resources.find(&name).await?.is_some() {
            return Ok(name_taken());
        }
        if self.resources.list().len() >= resources::MAX_RESOURCES {
            return Ok(too_many());
        }
        let Some(root) = &self.storage.root else {
            return Ok(storage_refused(storage::directory::Refused::NotConfigured));
        };
        // Read inside the checks, under the claims' lock: not a copy made
        // now, before another registration is in memory.
        let resources = Arc::clone(&self.resources);
        let prepared = match root
            .prepare(&self.storage.files, &name, directory, None, move || {
                resources.storage_taken()
            })
            .await
        {
            Ok(prepared) => prepared,
            Err(refused) => return Ok(storage_refused(refused)),
        };
        // Given back when this function ends, or when the request is given
        // up while the insert is on its way: no claim is held for it.
        let _claim = root.claim_guard(prepared.claim());
        let backend = storage::Directory::registered(
            &name,
            &prepared.directory,
            Arc::clone(&prepared.opened),
            Some(Arc::clone(root)),
            &self.storage.files,
        );
        // Copied: the name moves into the new Resource, and the error path
        // reads the Resource back by it. A few bytes, once a registration.
        let kept_name = name.clone();
        let new = NewResource {
            name,
            description,
            owner,
            contact,
            reach: NewReach::Storage {
                location: storage::Location::Directory {
                    directory: prepared.directory.to_string_lossy().into_owned(),
                },
                backend,
            },
        };
        match self.resources.register(new, &user.id).await {
            Ok(Registered::Registered(resource)) => {
                root.release(prepared.claim());
                Ok(Answer::json(
                    StatusCode::CREATED,
                    &RegisteredView {
                        resource: ResourceView::of(&resource),
                        warnings: &[],
                    },
                ))
            }
            Ok(Registered::NameTaken) => {
                root.undo(&self.storage.files, prepared).await;
                Ok(name_taken())
            }
            Ok(Registered::TooMany) => {
                root.undo(&self.storage.files, prepared).await;
                Ok(too_many())
            }
            Err(error) => {
                // The insert may still land. Read it back: the claim is
                // given back once the Resource is in memory, what was made
                // is undone once it is known not to be registered, and the
                // claim is kept (with the marker) when even that cannot be
                // known, so that no other registration takes the directory.
                match self.resources.find(&kept_name).await {
                    Ok(Some(found))
                        if found
                            .storage()
                            .is_some_and(|d| d.path() == prepared.directory.as_path()) =>
                    {
                        root.release(prepared.claim());
                    }
                    Ok(_) => root.undo(&self.storage.files, prepared).await,
                    Err(_) => {}
                }
                Err(error)
            }
        }
    }

    async fn resource(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let Some(resource) = self.resources.find(name).await? else {
            return Ok(no_resource());
        };
        match *method {
            Method::GET => {}
            Method::PATCH => return self.patch_resource(user, &resource, body).await,
            Method::DELETE => return self.remove_resource(user, &resource).await,
            _ => return Ok(Answer::method_not_allowed(method, "GET, PATCH or DELETE")),
        }
        match self
            .resources
            .read_back(
                &resource,
                tokio::time::Instant::now() + resources::READ_BACK_WAIT,
            )
            .await
        {
            resources::ReadBack::Known => {}
            resources::ReadBack::Removed => return Ok(no_resource()),
            resources::ReadBack::Unknown => return Ok(grants_unknown(name)),
        }
        Ok(match ResourceRead::of(&resource, &groups_of(user)) {
            Some(read) => Answer::json(StatusCode::OK, &read),
            None => grants_unknown(name),
        })
    }

    /// `DELETE /v1/resources/<name>` (blackbox/CONTRACT.md, "Resources and
    /// Grants in full: Removing a Resource"): one transaction that takes
    /// the row's lock first, its Grants counted and removed with it; the
    /// Applications that declare it, from memory; then memory lets it go,
    /// its pools close as they finish, and a Storage directory's marker and
    /// empty staging are removed (never a file).
    async fn remove_resource(
        &self,
        user: &User,
        resource: &resources::Resource,
    ) -> Result<Answer, DbError> {
        let mut declared: Vec<RemovedDeclaredBy> = Vec::new();
        let mut declaring: u64 = 0;
        for name in self.registry.serving_names() {
            if let crate::registry::Found::Known(kept) = self.registry.find(&name)
                && let Some(serving) = kept.serving()
                && let Some(declaration) = serving.declared.get(&resource.name)
            {
                declaring = declaring.saturating_add(1);
                // Only the first 100 are copied; the rest are counted.
                if declared.len() < 100 {
                    declared.push(RemovedDeclaredBy {
                        application: name.to_string(),
                        version: serving.version,
                        access: resources::level_name(declaration.level),
                    });
                }
            }
        }
        let declared_by_more =
            declaring.saturating_sub(u64::try_from(declared.len()).unwrap_or(u64::MAX));
        let Some(_turn) = resource.writing(WRITE_TURN_WAIT).await else {
            return Ok(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "another change of this Resource is running now: nothing was removed; try again shortly",
            ));
        };
        if !resource.grants_known() {
            match self
                .resources
                .read_back_locked(
                    resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await
            {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => return Ok(no_resource()),
                resources::ReadBack::Unknown => return Ok(grants_unknown(&resource.name)),
            }
        }
        // Recorded before the statement is sent: a request given up while
        // the removal is on its way owes the same record as one whose
        // answer is lost.
        resource.expect_removal(&user.id);
        let mut unsettled = resources::Unsettled::new(&self.resources, resource);
        let removed = match tokio::time::timeout(
            WRITE_ROW_WAIT,
            self.resources.remove(resource.id(), &resource.name),
        )
        .await
        {
            Ok(removed) => removed,
            Err(_) => Err(DbError::timed_out(
                "the removal of the Resource",
                WRITE_ROW_WAIT,
            )),
        };
        let grants_removed = match removed {
            Ok(resources::Removed::Removed { grants }) => grants,
            Ok(resources::Removed::NoSuchResource) => {
                // Certain: no row. Memory lets it go; the guard is disarmed
                // and no record is owed.
                resource.clear_removal();
                self.resources.forget_removed(resource);
                unsettled.settled();
                return Ok(no_resource());
            }
            // The guard forgets what memory knows: the commit may land. If
            // it does, a reading finds no row and owes the record.
            Err(error) => return Ok(Answer::unavailable(&error)),
        };
        resource.clear_removal();
        let held = self.resources.forget_removed(resource);
        unsettled.settled();
        // The record is written once the removal is in force, before the
        // cleaning: a request given up meanwhile still has it.
        log::record(
            "resource-removed",
            json!({ "resource": &resource.name, "by": &user.id }),
        );
        let directory_cleaned = match resource.kind {
            // Cleaned only while the directory is held (`forget_removed`):
            // without the hold it is left, and the answer says so.
            ResourceKind::Storage => Some(match (resource.storage(), &held) {
                (Some(directory), Some(_held)) => {
                    directory.clean(std::time::Duration::from_secs(5)).await
                }
                _ => false,
            }),
            ResourceKind::Postgres => None,
        };
        drop(held);
        Ok(Answer::json(
            StatusCode::OK,
            &RemoveView {
                name: &resource.name,
                kind: resource.kind.name(),
                grants_removed,
                declared_by: declared,
                declared_by_more,
                directory_cleaned,
            },
        ))
    }

    /// `PATCH /v1/resources/<name>` (blackbox/CONTRACT.md, "Resources and
    /// Grants in full: Changing a Resource's description, owner and
    /// contact"): at least one of the three, each as registration takes it,
    /// other members ignored; one transaction that takes the row's lock
    /// first; the answer is the administrators' view; the record
    /// `resource-changed`.
    async fn patch_resource(
        &self,
        user: &User,
        resource: &resources::Resource,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let change = match read_text_change(body) {
            Ok(change) => change,
            Err(problem) => return Ok(Answer::malformed(problem, PATCH_FORM)),
        };
        // One write of this Resource at a time, from before the row's
        // transaction to memory; a future given up here forgets what memory
        // knows (H1's guard). The check that the program knows it is made
        // under this turn.
        let Some(_turn) = resource.writing(WRITE_TURN_WAIT).await else {
            return Ok(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "another change of this Resource is running now: nothing was changed; try again shortly",
            ));
        };
        if !resource.grants_known() {
            match self
                .resources
                .read_back_locked(
                    resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await
            {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => return Ok(no_resource()),
                resources::ReadBack::Unknown => return Ok(grants_unknown(&resource.name)),
            }
        }
        let mut unsettled = resources::Unsettled::new(&self.resources, resource);
        let written = match tokio::time::timeout(
            WRITE_ROW_WAIT,
            self.resources.change_texts(
                resource.id(),
                &resource.name,
                change.description.as_deref(),
                change.owner.as_deref(),
                change.contact.as_deref(),
            ),
        )
        .await
        {
            Ok(written) => written,
            Err(_) => Err(DbError::timed_out(
                "the change of the Resource's row",
                WRITE_ROW_WAIT,
            )),
        };
        let (description, owner, contact) = match written {
            Ok(Some(texts)) => texts,
            Ok(None) => {
                // Certain: no row. Memory lets it go; the guard is disarmed.
                self.resources.forget_removed(resource);
                unsettled.settled();
                return Ok(no_resource());
            }
            // The guard forgets what memory knows: the commit may land
            // after this answer.
            Err(error) => return Ok(Answer::unavailable(&error)),
        };
        resource.set_texts(
            std::sync::Arc::from(description),
            std::sync::Arc::from(owner),
            std::sync::Arc::from(contact),
        );
        unsettled.settled();
        let fields: Vec<&str> = change.fields();
        log::record(
            "resource-changed",
            json!({ "resource": &resource.name, "fields": fields, "by": &user.id }),
        );
        // The answer is the view, as it is now; the Grants, if memory did
        // not know them, are read back within the request's bound, under
        // the turn this write still holds.
        match self
            .resources
            .read_back_locked(
                resource,
                tokio::time::Instant::now() + resources::READ_BACK_WAIT,
            )
            .await
        {
            resources::ReadBack::Known => {}
            resources::ReadBack::Removed => return Ok(no_resource()),
            resources::ReadBack::Unknown => return Ok(grants_unknown(&resource.name)),
        }
        Ok(match self.admin_resource_view(resource) {
            Some(view) => Answer::json(StatusCode::OK, &view),
            None => grants_unknown(&resource.name),
        })
    }

    /// `PUT /v1/resources/<name>/connection`: the checks on the logins
    /// given (and on every login when the address changes), then one
    /// transaction that takes the row's lock first, seals the new
    /// connection for the row and swaps the pools (blackbox/CONTRACT.md,
    /// "Resources and Grants in full: Replacing a Resource's logins").
    async fn replace_logins(
        &self,
        user: &User,
        resource: &resources::Resource,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let change = match read_logins_change(body) {
            Ok(change) => change,
            Err(problem) => return Ok(Answer::malformed(problem, LOGINS_FORM)),
        };
        if !self.resources.has_vault() {
            return Ok(Answer::error(
                StatusCode::CONFLICT,
                "no-secret-key",
                "a database Resource's logins are sealed under the installation's key, and this program was started without it: start the program with VIBE_SECRET_KEY, then replace its logins",
            ));
        }
        // A replacement of a Resource the program is still finding out reads
        // its row back first, within the contract's 1 second; when it still
        // does not know, nothing is written and it is asked again.
        if !resource.grants_known() {
            match self
                .resources
                .read_back(
                    resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await
            {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => return Ok(no_resource()),
                resources::ReadBack::Unknown => return Ok(grants_unknown(&resource.name)),
            }
        }
        let current = resource.connection();
        let new = match &current {
            Some(current) => resources::Connection::new(
                change
                    .host
                    .clone()
                    .unwrap_or_else(|| current.host().to_owned()),
                change.port.unwrap_or_else(|| current.port()),
                change
                    .database
                    .clone()
                    .unwrap_or_else(|| current.database().to_owned()),
                change
                    .read
                    .clone()
                    .unwrap_or_else(|| current.read_login().clone()),
                match &change.write {
                    Some(write) => write.clone(),
                    None => current.write_login().cloned(),
                },
                match &change.structure {
                    Some(structure) => structure.clone(),
                    None => current.structure_login().cloned(),
                },
            ),
            None => {
                // Not served: a whole connection and nothing less.
                let mut missing: Vec<&str> = Vec::new();
                if change.host.is_none() {
                    missing.push("host");
                }
                if change.port.is_none() {
                    missing.push("port");
                }
                if change.database.is_none() {
                    missing.push("database");
                }
                if change.read.is_none() {
                    missing.push("read");
                }
                if !missing.is_empty() {
                    return Ok(Answer::error(
                        StatusCode::CONFLICT,
                        "connection-incomplete",
                        &format!(
                            "the Resource is not served, so a whole connection is needed: {} {} missing. Give the host, the port, the database and the read login, and the write and structure logins it should have",
                            missing.join(", "),
                            if missing.len() == 1 { "is" } else { "are" },
                        ),
                    ));
                }
                resources::Connection::new(
                    change.host.clone().unwrap_or_default(),
                    change.port.unwrap_or_default(),
                    change.database.clone().unwrap_or_default(),
                    change
                        .read
                        .clone()
                        .unwrap_or_else(|| resources::Login::new(String::new(), String::new())),
                    change.write.clone().unwrap_or_default(),
                    change.structure.clone().unwrap_or_default(),
                )
            }
        };
        // The logins connected: the given ones, or every one when the
        // address changes (or when a whole connection is given).
        let address_changed =
            change.host.is_some() || change.port.is_some() || change.database.is_some();
        let only: Vec<resources::Which> = if current.is_none() || address_changed {
            new.logins()
        } else {
            let mut only = Vec::new();
            if change.read.is_some() {
                only.push(resources::Which::Read);
            }
            if change.write.is_some() && new.write_login().is_some() {
                only.push(resources::Which::Write);
            }
            if change.structure.is_some() && new.structure_login().is_some() {
                only.push(resources::Which::Structure);
            }
            only
        };
        let Ok(_checking) = self.checks.try_acquire() else {
            return Ok(checks_busy());
        };
        let checked = match tokio::time::timeout(CHECK_WHOLE, new.check_logins(&only)).await {
            Ok(checked) => checked,
            Err(_) => resources::Checked {
                failures: only
                    .iter()
                    .map(|login| resources::Failure {
                        login: *login,
                        why: "timed-out",
                    })
                    .collect(),
                warnings: Vec::new(),
            },
        };
        if !checked.failures.is_empty() {
            for failure in &checked.failures {
                log::record(
                    "resource-login-failed",
                    json!({
                        "resource": &resource.name,
                        "login": failure.login.word(),
                        "why": failure.why,
                    }),
                );
            }
            return Ok(login_failed(&checked.failures));
        }
        // The check holds no lock; the turn is taken after it, before the
        // row's transaction, and held until memory is what was written. The
        // check that the program knows the Resource is repeated under it.
        let Some(_turn) = resource.writing(WRITE_TURN_WAIT).await else {
            return Ok(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "another change of this Resource is running now: nothing was changed; try again shortly",
            ));
        };
        if !resource.grants_known() {
            // The logins were checked on what memory held before the
            // program found the Resource out: nothing is written, whichever
            // the reading found (a row gone is the write that finds no row).
            let reading = self
                .resources
                .read_back_locked(
                    resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await;
            if reading == resources::ReadBack::Removed {
                return Ok(no_resource());
            }
            return Ok(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the Resource's logins changed while they were being checked: nothing was changed; ask again",
            ));
        }
        let mut unsettled = resources::Unsettled::new(&self.resources, resource);
        let written = match tokio::time::timeout(
            WRITE_ROW_WAIT,
            self.resources.replace_connection(
                resource.id(),
                &resource.name,
                current.as_ref(),
                &new,
            ),
        )
        .await
        {
            Ok(written) => written,
            Err(_) => Err(DbError::timed_out(
                "the replacement of the Resource's logins",
                WRITE_ROW_WAIT,
            )),
        };
        match written {
            Ok(resources::Replaced::Replaced) => {}
            Ok(resources::Replaced::NoSuchResource) => {
                self.resources.forget_removed(resource);
                unsettled.settled();
                return Ok(no_resource());
            }
            Ok(resources::Replaced::ChangedMeanwhile) => {
                // Certain: another write's logins are the ones the row has;
                // nothing of this one was written.
                unsettled.settled();
                return Ok(Answer::error(
                    StatusCode::CONFLICT,
                    "changed-meanwhile",
                    "the Resource's logins changed between this replacement's check and its write: nothing was changed; ask again",
                ));
            }
            // The guard forgets what memory knows: the commit may land
            // after this answer.
            Err(error) => return Ok(Answer::unavailable(&error)),
        }
        self.resources.install_connection(resource, new);
        unsettled.settled();
        log::record(
            "resource-logins-replaced",
            json!({
                "resource": &resource.name,
                "members": change.members,
                "by": &user.id,
            }),
        );
        for warning in &checked.warnings {
            log::record(
                "resource-login-warning",
                json!({
                    "resource": &resource.name,
                    "login": warning.login.word(),
                    "reason": warning.reason,
                }),
            );
        }
        let warnings: Vec<Value> = checked
            .warnings
            .iter()
            .map(resources::Warning::json)
            .collect();
        match self
            .resources
            .read_back_locked(
                resource,
                tokio::time::Instant::now() + resources::READ_BACK_WAIT,
            )
            .await
        {
            resources::ReadBack::Known => {}
            resources::ReadBack::Removed => return Ok(no_resource()),
            resources::ReadBack::Unknown => return Ok(grants_unknown(&resource.name)),
        }
        Ok(match self.admin_resource_view(resource) {
            Some(view) => Answer::json(
                StatusCode::OK,
                &AdminChangedView {
                    resource: view,
                    warnings,
                },
            ),
            None => grants_unknown(&resource.name),
        })
    }

    /// `PUT /v1/resources/<name>/connection` (blackbox/CONTRACT.md,
    /// "Resources and Grants in full: Replacing a Resource's logins").
    async fn connection(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if *method != Method::PUT {
            return Ok(Answer::method_not_allowed(method, "PUT"));
        }
        let Some(resource) = self.resources.find(name).await? else {
            return Ok(no_resource());
        };
        if resource.kind == ResourceKind::Storage {
            return Ok(Answer::error(
                StatusCode::CONFLICT,
                "wrong-kind",
                "this is a Storage Resource: its place is changed with `vibe admin resource location`",
            ));
        }
        self.replace_logins(user, &resource, body).await
    }

    /// `PUT /v1/resources/<name>/location` (blackbox/CONTRACT.md,
    /// "Resources and Grants in full: A Storage Resource's location"): the
    /// registration's directory checks in the same order, save that the
    /// Resource's own directory is not another's; one transaction that
    /// takes the row's lock first; memory takes the new directory once the
    /// row is written; the old directory's marker and empty staging are
    /// removed, never a file.
    async fn location(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if *method != Method::PUT {
            return Ok(Answer::method_not_allowed(method, "PUT"));
        }
        let Some(resource) = self.resources.find(name).await? else {
            return Ok(no_resource());
        };
        if resource.kind != ResourceKind::Storage {
            return Ok(Answer::error(
                StatusCode::CONFLICT,
                "wrong-kind",
                "this is a database Resource, which has no location: its logins are replaced with PUT /v1/resources/<name>/connection",
            ));
        }
        let directory = match read_location(body) {
            Ok(directory) => directory,
            Err(problem) => return Ok(Answer::malformed(problem, LOCATION_FORM)),
        };
        let Some(root) = &self.storage.root else {
            return Ok(storage_refused(storage::directory::Refused::NotConfigured));
        };
        // One write of this Resource at a time; the check that the program
        // knows it is made under that turn, in the reading this write is.
        let Some(_turn) = resource.writing(WRITE_TURN_WAIT).await else {
            return Ok(Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "another change of this Resource is running now: nothing was changed; try again shortly",
            ));
        };
        if !resource.grants_known() {
            match self
                .resources
                .read_back_locked(
                    &resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await
            {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => return Ok(no_resource()),
                resources::ReadBack::Unknown => return Ok(grants_unknown(name)),
            }
        }
        let Some(current) = resource.storage() else {
            return Ok(no_resource());
        };
        let location_now = AdminLocation {
            backend: "directory",
            directory: current.path().to_string_lossy().into_owned(),
        };
        // The same directory again, as a registration takes it (one `/` at
        // its end dropped, and no more): nothing is written at all.
        let same = crate::storage::directory::segments_of(&directory).is_some_and(|asked| {
            crate::storage::directory::segments_of(&current.path().to_string_lossy())
                .is_some_and(|own| asked == own)
        });
        if same {
            if let Some(pending) = resource.take_location_change() {
                log::record(
                    "resource-location-changed",
                    json!({ "resource": name, "by": pending.by }),
                );
            }
            return Ok(Answer::json(
                StatusCode::OK,
                &LocationChangedView {
                    resource: name,
                    location: location_now,
                    changed: false,
                    files_moved: false,
                    message: "the Resource is already on this directory: nothing was written; its files stay where they are",
                },
            ));
        }
        let resources = Arc::clone(&self.resources);
        let own = current.path().to_path_buf();
        let prepared = match root
            .prepare(
                &self.storage.files,
                name,
                &directory,
                Some(current.path()),
                move || {
                    resources
                        .storage_taken()
                        .into_iter()
                        .filter(|taken| taken.path != own)
                        .collect()
                },
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(refused) => return Ok(storage_refused(refused)),
        };
        let new = storage::Location::Directory {
            directory: prepared.directory.to_string_lossy().into_owned(),
        };
        let new_path = prepared.directory.clone();
        let (made_marker, made_staging) = prepared.made();
        // Recorded before the statement is sent: a request its sender gives
        // up owes the same record and cleaning as one whose answer is lost.
        let ours = resource.expect_location_change_if_none_for(
            &user.id,
            new_path.clone(),
            made_marker,
            made_staging,
        );
        // Given back when this function ends, or when the request is given
        // up while the write is on its way.
        let _claim = root.claim_guard(prepared.claim());
        let mut unsettled = resources::Unsettled::new(&self.resources, &resource);
        let written = match tokio::time::timeout(
            WRITE_ROW_WAIT,
            self.resources
                .change_location(resource.id(), &resource.name, current.path(), &new),
        )
        .await
        {
            Ok(written) => written,
            Err(_) => Err(DbError::timed_out(
                "the change of the Resource's location",
                WRITE_ROW_WAIT,
            )),
        };
        match written {
            Ok(resources::LocationChanged::Changed) => {
                unsettled.settled();
                resource.clear_location_change();
                let backend = storage::Directory::registered(
                    name,
                    &prepared.directory,
                    Arc::clone(&prepared.opened),
                    self.storage.root.clone(),
                    &self.storage.files,
                );
                resource.install_location(backend);
                // The record is written before the cleaning: a request given
                // up meanwhile still has the record of the change in force.
                log::record(
                    "resource-location-changed",
                    json!({ "resource": name, "by": &user.id }),
                );
                current.clean(std::time::Duration::from_secs(5)).await;
                Ok(Answer::json(
                    StatusCode::OK,
                    &LocationChangedView {
                        resource: name,
                        location: AdminLocation {
                            backend: "directory",
                            directory: new_path.to_string_lossy().into_owned(),
                        },
                        changed: true,
                        files_moved: false,
                        message: "the files of the old directory stay where they are, and the new directory must hold the files the Resource is to have; nothing was moved",
                    },
                ))
            }
            Ok(resources::LocationChanged::Same) => {
                unsettled.settled();
                let backend = storage::Directory::registered(
                    name,
                    &prepared.directory,
                    Arc::clone(&prepared.opened),
                    self.storage.root.clone(),
                    &self.storage.files,
                );
                resource.install_location(backend);
                // Only a pending an earlier request left owes a record; this
                // request's own is cleared without one, as it wrote nothing.
                let pending = resource.take_location_change();
                if !ours && let Some(pending) = pending {
                    log::record(
                        "resource-location-changed",
                        json!({ "resource": name, "by": pending.by }),
                    );
                }
                current.clean(std::time::Duration::from_secs(5)).await;
                Ok(Answer::json(
                    StatusCode::OK,
                    &LocationChangedView {
                        resource: name,
                        location: AdminLocation {
                            backend: "directory",
                            directory: new_path.to_string_lossy().into_owned(),
                        },
                        changed: false,
                        files_moved: false,
                        message: "the Resource is already on this directory: nothing was changed; its files stay where they are",
                    },
                ))
            }
            Ok(resources::LocationChanged::ChangedMeanwhile) => {
                unsettled.settled();
                // Only this request's own: one an earlier request left is
                // settled by the reading that finds it out.
                if ours {
                    resource.clear_location_change();
                }
                root.undo(&self.storage.files, prepared).await;
                Ok(Answer::error(
                    StatusCode::CONFLICT,
                    "changed-meanwhile",
                    "the Resource's location changed between this change's finding and its write: nothing was changed; ask again",
                ))
            }
            Ok(resources::LocationChanged::NoSuchResource) => {
                unsettled.settled();
                resource.clear_location_change();
                root.undo(&self.storage.files, prepared).await;
                self.resources.forget_removed(&resource);
                Ok(no_resource())
            }
            Err(error) => {
                // The answer is lost: memory forgets, and the row is read
                // back to know whether the change landed. What the change
                // made in the new directory is undone when it did not; the
                // record and the old directory's cleaning happen in that
                // reading, before this answer.
                unsettled.settled();
                self.resources.write_failed(&resource);
                let state = self
                    .resources
                    .read_back_locked(
                        &resource,
                        tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                    )
                    .await;
                if state == resources::ReadBack::Removed {
                    // The row is gone: the new directory is not the
                    // Resource's; what the change made there goes.
                    root.undo(&self.storage.files, prepared).await;
                }
                Ok(Answer::unavailable(&error))
            }
        }
    }

    /// `GET /v1/admin/resources` (blackbox/CONTRACT.md, "Resources and
    /// Grants in full: Listing, for administrators").
    async fn admin_resources(&self, method: &Method, query: Option<&str>) -> Answer {
        if *method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let (after, limit) = match resource_list_query(query) {
            Ok(query) => query,
            Err(problem) => return Answer::malformed(&problem, ADMIN_RESOURCES_FORM),
        };
        match self.read_admin_resources(after.as_deref(), limit).await {
            Ok(read) => Answer::json(StatusCode::OK, &read),
            Err(AdminResourceRead::Unknown(name)) => grants_unknown(&name),
            Err(AdminResourceRead::Failed(error)) => Answer::unavailable(&error),
            Err(AdminResourceRead::NoSuchResource | AdminResourceRead::Found(_)) => {
                Answer::unavailable(&DbError::unexpected(
                    "a list of Resources is not one Resource",
                ))
            }
        }
    }

    /// One page of every Resource, as `GET /v1/admin/resources` answers it,
    /// from the same reading the Dashboard's list shows: Grants memory does
    /// not know are read back first, within the request's bound.
    pub(crate) async fn read_admin_resources(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<AdminResourcesView, AdminResourceRead> {
        let all: Vec<_> = self
            .resources
            .list()
            .into_iter()
            .filter(|r| after.is_none_or(|after| r.name.as_str() > after))
            .collect();
        let deadline = tokio::time::Instant::now() + resources::READ_BACK_WAIT;
        let mut remaining = all.into_iter().peekable();
        let mut views: Vec<AdminResourceView> = Vec::new();
        // The envelope's own bytes, and one comma between Resources.
        let budget = MAX_ADMIN_LIST_BYTES.saturating_sub(256);
        let mut used = 0usize;
        let mut stopped_early = false;
        while views.len() < limit {
            let Some(resource) = remaining.next() else {
                break;
            };
            match self.resources.read_back(&resource, deadline).await {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => continue,
                resources::ReadBack::Unknown => {
                    return Err(AdminResourceRead::Unknown(resource.name.clone()));
                }
            }
            let Some(view) = self.admin_resource_view(&resource) else {
                return Err(AdminResourceRead::Unknown(resource.name.clone()));
            };
            // Measured once here, and dropped from the end by that size.
            let size = serde_json::to_vec(&view).map_or(0, |body| body.len()) + 1;
            if !views.is_empty() && used.saturating_add(size) > budget {
                stopped_early = true;
                break;
            }
            used = used.saturating_add(size);
            views.push(view);
        }
        let next = if stopped_early || remaining.peek().is_some() {
            views.last().map(|view| view.name.clone())
        } else {
            None
        };
        Ok(AdminResourcesView {
            resources: views,
            next,
        })
    }

    /// `GET /v1/admin/resources/<name>`.
    async fn admin_resource(&self, method: &Method, name: &str) -> Result<Answer, DbError> {
        if *method != Method::GET {
            return Ok(Answer::method_not_allowed(method, "GET"));
        }
        Ok(match self.read_admin_resource(name).await {
            AdminResourceRead::Found(read) => Answer::json(StatusCode::OK, &read.view()),
            AdminResourceRead::NoSuchResource => no_resource(),
            AdminResourceRead::Unknown(name) => grants_unknown(&name),
            AdminResourceRead::Failed(error) => Answer::unavailable(&error),
        })
    }

    /// One Resource as `GET /v1/admin/resources/<name>` answers it and
    /// every Grant `GET /v1/resources/<name>/grants` answers, from the same
    /// readings the API makes; the Dashboard's page of it.
    pub(crate) async fn read_admin_resource(&self, name: &str) -> AdminResourceRead {
        let resource = match self.resources.find(name).await {
            Ok(Some(resource)) => resource,
            Ok(None) => return AdminResourceRead::NoSuchResource,
            Err(error) => return AdminResourceRead::Failed(error),
        };
        match self
            .resources
            .read_back(
                &resource,
                tokio::time::Instant::now() + resources::READ_BACK_WAIT,
            )
            .await
        {
            resources::ReadBack::Known => {}
            resources::ReadBack::Removed => return AdminResourceRead::NoSuchResource,
            resources::ReadBack::Unknown => {
                return AdminResourceRead::Unknown(resource.name.clone());
            }
        }
        // One reading of the Grants: the page shows the 1,000, and the
        // API's view it (when the API asks) takes the first 200 of them.
        let Some((grants, grants_more)) = resource.grants_page(MAX_GRANTS_LISTED) else {
            return AdminResourceRead::Unknown(resource.name.clone());
        };
        let (usable, problem) = match resource.kind {
            ResourceKind::Storage => {
                let problem = resource
                    .storage()
                    .and_then(|directory| directory.problem())
                    .map(directory_problem_word);
                (problem.is_none(), problem)
            }
            ResourceKind::Postgres => (resource.unserved().is_none(), resource.unserved()),
        };
        let location = resource.storage().map(|directory| AdminLocation {
            backend: "directory",
            directory: directory.path().to_string_lossy().into_owned(),
        });
        AdminResourceRead::Found(Box::new(AdminResourcePage {
            name: resource.name.clone(),
            kind: resource.kind.name(),
            description: resource.description().to_string(),
            owner: resource.owner().to_string(),
            contact: resource.contact().to_string(),
            created_by: resource.created_by.clone(),
            created_at: resource.created_at.clone(),
            usable,
            problem,
            logins: resource.logins(),
            location,
            grants,
            grants_more,
        }))
    }

    /// A Resource as a Platform administrator sees it; `None` while its
    /// Grants are not known.
    fn admin_resource_view(&self, resource: &resources::Resource) -> Option<AdminResourceView> {
        let (grants, grants_more) = resource.grants_page(MAX_GRANTS_SHOWN)?;
        Some(self.admin_resource_view_with(resource, &grants, grants_more))
    }

    /// A Resource as a Platform administrator sees it, from Grants already
    /// read: the first 200 of `entries`, and `grants_more` beyond them.
    fn admin_resource_view_with(
        &self,
        resource: &resources::Resource,
        entries: &[resources::GrantEntry],
        grants_more: usize,
    ) -> AdminResourceView {
        let (usable, problem) = match resource.kind {
            ResourceKind::Storage => {
                let problem = resource
                    .storage()
                    .and_then(|directory| directory.problem())
                    .map(directory_problem_word);
                (problem.is_none(), problem)
            }
            ResourceKind::Postgres => (resource.unserved().is_none(), resource.unserved()),
        };
        let location = resource.storage().map(|directory| AdminLocation {
            backend: "directory",
            directory: directory.path().to_string_lossy().into_owned(),
        });
        // Copied once per entry the view shows (at most 200): the view is
        // owned by its answer, the page by its own.
        let grants = entries
            .iter()
            .map(|entry| AdminGrant {
                group: entry.group.clone(),
                access: resources::level_name(entry.level),
                granted_by: entry.granted_by.as_deref().map(str::to_owned),
                granted_at: entry.granted_at.as_deref().map(str::to_owned),
            })
            .collect();
        AdminResourceView {
            name: resource.name.clone(),
            kind: resource.kind.name(),
            description: resource.description().to_string(),
            owner: resource.owner().to_string(),
            contact: resource.contact().to_string(),
            created_by: resource.created_by.clone(),
            created_at: resource.created_at.clone(),
            usable,
            problem,
            logins: resource.logins(),
            location,
            grants,
            grants_more,
        }
    }
    async fn grants(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        let Some(resource) = self.resources.find(name).await? else {
            return Ok(no_resource());
        };
        if method == Method::GET {
            match self
                .resources
                .read_back(
                    &resource,
                    tokio::time::Instant::now() + resources::READ_BACK_WAIT,
                )
                .await
            {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => return Ok(no_resource()),
                resources::ReadBack::Unknown => return Ok(grants_unknown(name)),
            }
            let Some((grants, grants_more)) = resource.grants_page(MAX_GRANTS_LISTED) else {
                return Ok(grants_unknown(name));
            };
            let view = GrantsView {
                grants: grants
                    .iter()
                    .map(|entry| GrantView {
                        group: &entry.group,
                        access: resources::level_name(entry.level),
                        granted_by: entry.granted_by.as_deref(),
                        granted_at: entry.granted_at.as_deref(),
                    })
                    .collect(),
                grants_more,
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
        if !resources::level_allowed(resource.kind, level) {
            return Ok(Answer::malformed(
                "access is not read or write: a Storage Resource has two levels, read and write",
                GRANT_FORM,
            ));
        }
        let granted = self
            .resources
            .write_grant(&resource, &group, Some(level), &user.id)
            .await?;
        Ok(match granted {
            GrantWritten::Written(change) => Answer::json(
                StatusCode::OK,
                &GrantSet {
                    resource: &resource.name,
                    group: &group,
                    access: resources::level_name(level),
                    changed: change.changed,
                    was: change.was.map(resources::level_name),
                },
            ),
            GrantWritten::TooMany => too_many_grants(),
            GrantWritten::NoSuchResource => no_resource(),
            GrantWritten::Unknown => grants_unknown(&resource.name),
            GrantWritten::Busy => busy_resource(),
        })
    }

    /// `DELETE /v1/resources/<name>/grants/<group>`
    /// (blackbox/CONTRACT.md, "Resources and Grants in full: Revoking a
    /// Grant"): the Group decoded after the path is split (so a `%2F` is
    /// one segment), `404` before it is looked at, then one transaction
    /// under the Resource's write turn that answers what it held.
    async fn revoke_grant(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        written: &str,
        _body: &[u8],
    ) -> Answer {
        if *method != Method::DELETE {
            return Answer::method_not_allowed(method, "DELETE");
        }
        let Some(resource) = (match self.resources.find(name).await {
            Ok(found) => found,
            Err(error) => return Answer::unavailable(&error),
        }) else {
            return no_resource();
        };
        let group = match percent_decoded(written) {
            Some(group) if resources::is_group(&group) => group,
            Some(_) => {
                return Answer::error(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "the Group in the path is not 1 to 256 bytes of UTF-8 without a control character: send the Group percent-encoded as one path segment",
                );
            }
            None => {
                return Answer::error(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "the Group in the path is not percent-encoded UTF-8: send it percent-encoded as one path segment",
                );
            }
        };
        let revoked = match self
            .resources
            .write_grant(&resource, &group, None, &user.id)
            .await
        {
            Ok(revoked) => revoked,
            Err(error) => return Answer::unavailable(&error),
        };
        match revoked {
            GrantWritten::Written(change) => Answer::json(
                StatusCode::OK,
                &GrantRevoked {
                    resource: &resource.name,
                    group: &group,
                    changed: change.changed,
                    was: change.was.map(resources::level_name),
                },
            ),
            GrantWritten::TooMany => too_many_grants(),
            GrantWritten::NoSuchResource => no_resource(),
            GrantWritten::Unknown => grants_unknown(&resource.name),
            GrantWritten::Busy => busy_resource(),
        }
    }

    /// `GET /v1/admin/grants?group=<group>` (blackbox/CONTRACT.md,
    /// "Resources and Grants in full: The Grants of a Group"): every Grant
    /// the Group holds, by Resource's name, from memory; a Resource whose
    /// Grants are not known is read back, and `503` while it still is not.
    async fn admin_grants(&self, method: &Method, query: Option<&str>) -> Answer {
        if *method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let named = match query_parameters(query, &["group"]) {
            Ok(found) => found,
            Err(_) => {
                return Answer::error(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "?group is given twice or is not percent-encoded: send one Group, percent-encoded",
                );
            }
        };
        let Some(group) = named.get("group") else {
            return Answer::error(
                StatusCode::BAD_REQUEST,
                "malformed",
                "?group is missing: give one Group, percent-encoded",
            );
        };
        if !resources::is_group(group) {
            return Answer::error(
                StatusCode::BAD_REQUEST,
                "malformed",
                "?group is not 1 to 256 bytes of UTF-8 without a control character: give one Group",
            );
        }
        let deadline = tokio::time::Instant::now() + resources::READ_BACK_WAIT;
        let mut grants = Vec::new();
        for resource in self.resources.list() {
            match self.resources.read_back(&resource, deadline).await {
                resources::ReadBack::Known => {}
                resources::ReadBack::Removed => continue,
                resources::ReadBack::Unknown => return grants_unknown(&resource.name),
            }
            let Some(entry) = resource.grant_named(group) else {
                return grants_unknown(&resource.name);
            };
            if let Some(entry) = entry {
                grants.push(GroupGrantView {
                    resource: resource.name.clone(),
                    kind: resource.kind.name(),
                    access: resources::level_name(entry.level),
                    granted_by: entry.granted_by.map(|who| who.to_string()),
                    granted_at: entry.granted_at.map(|when| when.to_string()),
                });
            }
        }
        Answer::json(StatusCode::OK, &GroupGrantsView { group, grants })
    }

    /// `GET /v1/resources/<name>/structure` (blackbox/CONTRACT.md, "Pulling
    /// a Resource's structure", "Pulling a Storage Resource, checking and
    /// deploying"): the Authorization decision with `structure` needed; the
    /// Operation log entry written before anything is sent to the shared
    /// database, and completed after; the pull (crate::structure) within its
    /// time and bytes. A Storage Resource answers what a pull needs of it:
    /// its name, kind, the User's Grant and whom to ask, and no structure.
    async fn structure(&self, user: &User, request: &str, method: &Method, name: &str) -> Answer {
        let resource = match self.resources.find(name).await {
            Ok(Some(resource)) => resource,
            Ok(None) => return no_resource(),
            Err(error) => return Answer::unavailable(&error),
        };
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        match self
            .resources
            .read_back(
                &resource,
                tokio::time::Instant::now() + resources::READ_BACK_WAIT,
            )
            .await
        {
            resources::ReadBack::Known => {}
            resources::ReadBack::Removed => return no_resource(),
            resources::ReadBack::Unknown => return grants_unknown(name),
        }
        let groups = groups_of(user);
        let decided = resource.with_grants(|grants| {
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
        let Some((decision, held)) = decided else {
            return grants_unknown(name);
        };
        if let authz::Decision::Refuse(_) = decision {
            if let Err(error) = self
                .log
                .refused_structure(request, &user.id, name, "no-grant")
                .await
            {
                return Answer::unavailable(&error);
            }
            let contact = resource.contact();
            return Answer::json(
                StatusCode::FORBIDDEN,
                &json!({ "error": {
                    "reason": "no-grant",
                    "message": format!(
                        "you hold no Grant on the Resource {name}: ask {contact} for one, and do not build against {name} until you have it"
                    ),
                    "resource": name,
                    "contact": contact.as_ref(),
                } }),
            );
        }
        // A Storage Resource has no structure to pull: the answer is the
        // Resource and the User's Grant, and nothing of a database
        // (blackbox/CONTRACT.md, "Pulling a Storage Resource, checking and
        // deploying"). Its entry is the pull's.
        if resource.kind == ResourceKind::Storage {
            let id = match self.log.begin_structure(request, &user.id, name).await {
                Ok(id) => id,
                Err(error) => return Answer::unavailable(&error),
            };
            // From the entry written to the answer, as a database's pull is
            // measured; there is no file work in it.
            let started = tokio::time::Instant::now();
            let answer = Answer::json(
                StatusCode::OK,
                &json!({
                    "resource": name,
                    "kind": "storage",
                    "your_access": held.map(resources::level_name),
                    "contact": resource.contact().as_ref(),
                }),
            );
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let ending = Ending {
                outcome: "allowed",
                error: None,
                rows: None,
                duration_ms: Some(duration_ms),
                began_nothing: false,
            };
            if let Err(error) = self.log.finish(id, &ending).await {
                log::record(
                    "operation-log-unwritten",
                    json!({ "resource": name, "error": error.to_string() }),
                );
            }
            return answer;
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
        let pulled = match resource.structure_and_roles() {
            Some((pool, read_role, write_role)) => {
                structure::pull(&pool, name, &read_role, write_role.as_deref()).await
            }
            // Not reached: every served database has both, and a Storage
            // Resource was answered above.
            None => Err(structure::Failed::Unavailable),
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let answer = pulled.and_then(|pulled| {
            let answer = Answer::json(
                StatusCode::OK,
                &StructureView {
                    resource: name,
                    kind: "postgres",
                    fingerprint: &pulled.fingerprint,
                    your_access: held.map(resources::level_name),
                    schema_ts: &pulled.schema_ts,
                    unsupported: &pulled.unsupported,
                    standin_sql: &pulled.standin_sql,
                    contact: &resource.contact(),
                    hidden: pulled.hidden,
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
                "a tool credential cannot make another: sign in again with `vibe sign-in`",
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

    /// `GET /v1/operation-log`, by a Platform administrator, with its
    /// filters (blackbox/CONTRACT.md, "What Platform administrators see and
    /// stop: Reading the Operation log").
    async fn operation_log(&self, method: &Method, query: Option<&str>) -> Answer {
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let asked = match LogQuery::read(query) {
            Ok(asked) => asked,
            Err(problem) => {
                return Answer::error(StatusCode::BAD_REQUEST, "malformed", &problem.message());
            }
        };
        match self.read_operation_log(&asked).await {
            Ok((entries, earlier)) => {
                Answer::json(StatusCode::OK, &EntriesView { entries, earlier })
            }
            Err(AdminRead::Busy) => readings_busy(),
            Err(AdminRead::TooLong(_)) => Answer::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the reading took too long (more than 10 seconds in the platform database) and was ended: narrow it, with since and until or another filter, and read again",
            ),
            Err(AdminRead::Failed(error)) => Answer::unavailable(&error),
        }
    }

    /// One reading of the Operation log for Platform administrators, as
    /// `GET /v1/operation-log` answers it and the Dashboard's page shows
    /// it: one of the [`ADMIN_READINGS`] at once, within
    /// [`ADMIN_READ_TIME`], at most 4 MiB written as the API writes it.
    /// The entries (oldest first) and `earlier`.
    pub(crate) async fn read_operation_log(
        &self,
        asked: &LogQuery,
    ) -> Result<(Vec<Value>, Option<i64>), AdminRead> {
        let Some(_permit) = self.reading() else {
            return Err(AdminRead::Busy);
        };
        let filter = asked.filter();
        // What the answer holds besides its entries: `{"entries":[],
        // "earlier":<id>}`, the id at most 20 digits.
        let envelope = 64;
        let max_bytes = MAX_LOG_ANSWER_BYTES - envelope;
        match self.log.filtered(&filter, max_bytes, ADMIN_READ_TIME).await {
            Ok((entries, more)) => {
                let earlier = if more {
                    entries
                        .first()
                        .and_then(|e| e.get("id"))
                        .and_then(Value::as_i64)
                } else {
                    None
                };
                Ok((entries, earlier))
            }
            Err(error) => {
                log::record(
                    "platform-database-failed",
                    json!({ "error": error.to_string() }),
                );
                if took_too_long(&error) {
                    Err(AdminRead::TooLong(error))
                } else {
                    Err(AdminRead::Failed(error))
                }
            }
        }
    }

    /// One of the [`ADMIN_READINGS`] readings for Platform administrators
    /// at once, if one is free.
    fn reading(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.readings.try_acquire().ok()
    }

    /// `GET /v1/admin/applications`, by a Platform administrator
    /// (blackbox/CONTRACT.md, "Every Application, on the platform's API").
    async fn every_application(&self, method: &Method, query: Option<&str>) -> Answer {
        if method != Method::GET {
            return Answer::method_not_allowed(method, "GET");
        }
        let (after, limit) = match admin_list_query(query) {
            Ok(asked) => asked,
            Err(problem) => {
                return Answer::error(StatusCode::BAD_REQUEST, "malformed", &problem);
            }
        };
        let read = match self
            .read_every_application(Which::After(after.as_deref()), limit)
            .await
        {
            Ok(read) => read,
            Err(AdminRead::Busy) => return readings_busy(),
            Err(AdminRead::TooLong(error) | AdminRead::Failed(error)) => {
                return Answer::unavailable(&error);
            }
        };
        let applications = read
            .rows
            .iter()
            .zip(read.declared.iter())
            .map(|(application, (declared_access, declared_more))| {
                self.admin_view(application, declared_access.as_ref(), *declared_more)
            })
            .collect();
        Answer::json(
            StatusCode::OK,
            &AdminApplicationsView {
                applications,
                next: read.next(),
            },
        )
    }

    /// One reading of every Application for Platform administrators, as
    /// `GET /v1/admin/applications` answers it and the Dashboard's pages
    /// show it (`which`: a page after a name, or the one Application of a
    /// name): one of the [`ADMIN_READINGS`] at once, within
    /// [`ADMIN_READ_TIME`], each Declared access cut ([`cut_declared`]).
    pub(crate) async fn read_every_application(
        &self,
        which: Which<'_>,
        limit: u8,
    ) -> Result<EveryApplication, AdminRead> {
        let Some(_permit) = self.reading() else {
            return Err(AdminRead::Busy);
        };
        let fetched = i64::from(limit).saturating_add(1);
        let (after, only) = match which {
            Which::After(after) => (after, None),
            Which::Named(name) => (None, Some(name)),
        };
        let mut rows = match self
            .registry
            .every_application(after, only, fetched, ADMIN_READ_TIME)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                log::record(
                    "platform-database-failed",
                    json!({ "error": error.to_string() }),
                );
                return Err(if took_too_long(&error) {
                    AdminRead::TooLong(error)
                } else {
                    AdminRead::Failed(error)
                });
            }
        };
        let limit = usize::from(limit);
        let more = rows.len() > limit;
        rows.truncate(limit);
        // Taken out of what was read, not copied.
        let declared: Vec<(Option<Value>, usize)> = rows
            .iter_mut()
            .map(|application| cut_declared(application.declared_access.take()))
            .collect();
        Ok(EveryApplication {
            rows,
            declared,
            more,
        })
    }

    /// An Application of the list of every Application, its Declared
    /// access already cut ([`cut_declared`]).
    fn admin_view<'a>(
        &self,
        application: &'a AdminApplication,
        declared_access: Option<&'a Value>,
        declared_more: usize,
    ) -> AdminApplicationView<'a> {
        AdminApplicationView {
            name: &application.name,
            description: application.description.as_deref(),
            address: format!("{}.{}", application.name, self.domain),
            created_by: &application.created_by,
            created_at: &application.created_at,
            serving_version: application.serving_version,
            latest_version: application.latest_version,
            declared_access,
            declared_more,
            contributors: application.contributors.iter().map(EntryView::of).collect(),
            shared_with: SharedCountsView {
                everyone: application.everyone,
                groups: application.groups,
                people: application.people,
            },
            disabled: application.disabled.as_ref().map(DisabledView::of),
            usage: UsageView {
                days: 30,
                from: &application.usage_from,
                requests: application.requests,
                users: application.users,
            },
        }
    }

    /// `POST /v1/applications/<name>/disable` (`disable`) and `/enable`, by
    /// a Platform administrator (blackbox/CONTRACT.md, "Disabling and
    /// enabling, on the platform's API"). Each read and the write within
    /// [`ADMIN_READ_TIME`].
    async fn disable(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        disable: bool,
        body: &[u8],
    ) -> Answer {
        if check_name(name).is_err() {
            return no_application();
        }
        let found =
            match tokio::time::timeout(ADMIN_READ_TIME, self.registry.application(name)).await {
                Ok(Ok(found)) => found,
                Ok(Err(error)) => return Answer::unavailable(&error),
                Err(_) => {
                    return Answer::unavailable(&DbError::timed_out(
                        "reading the Application",
                        ADMIN_READ_TIME,
                    ));
                }
            };
        if found.is_none() {
            return no_application();
        }
        if method != Method::POST {
            return Answer::method_not_allowed(method, "POST");
        }
        if !body.trim_ascii().is_empty()
            && let Err(problem) = Object::read(body)
        {
            return Answer::malformed(&problem, "nothing, or {}");
        }
        match self.switch(user, name, disable).await {
            Ok(Some((application, changed))) => Answer::json(
                StatusCode::OK,
                &SwitchedView {
                    application: self.view(&application, user),
                    changed,
                },
            ),
            // Gone meanwhile.
            Ok(None) => no_application(),
            Err(error) => Answer::unavailable(&error),
        }
    }

    /// Disables (`disable`) or enables the Application `name` by `user`,
    /// a Platform administrator, as `POST /v1/applications/<name>/disable`
    /// and `/enable` do and the Dashboard's forms do: within
    /// [`ADMIN_READ_TIME`]; `None` for no such Application (also, without
    /// asking the database, for a name none could have); else the
    /// Application as it is after and whether this changed it.
    pub(crate) async fn switch(
        &self,
        user: &User,
        name: &str,
        disable: bool,
    ) -> Result<Option<(ApplicationRecord, bool)>, DbError> {
        if check_name(name).is_err() {
            return Ok(None);
        }
        self.registry
            .set_disabled(name, disable, &user.id, ADMIN_READ_TIME)
            .await
    }

    /// What `GET /v1/installation` answers: the domain, how people sign in
    /// (`oidc`, `stand-in` or `none`), and with OIDC the sign-in address.
    pub(crate) fn installation_facts(&self) -> (&str, &'static str, Option<String>) {
        let connected = self.signing.oidc.as_ref().and_then(|o| o.connected());
        let (sign_in, address) = match connected {
            Some(_) => (
                "oidc",
                Some(
                    self.signing
                        .public
                        .origin(&format!("{}.{}", crate::oidc::AUTH_NAME, self.domain), None),
                ),
            ),
            None if self.signing.stand_in => ("stand-in", None),
            None => ("none", None),
        };
        (&self.domain, sign_in, address)
    }

    /// The OIDC connection as `GET /v1/oidc` reads it; `None` when OIDC is
    /// not connected.
    pub(crate) async fn oidc_connection(&self) -> Result<Option<oidc_connection::Stored>, DbError> {
        oidc_connection::read(&self.signing.database).await
    }

    /// Every Resource, by name, as `GET /v1/resources` reads them: the
    /// Grants memory does not know now read back first, all within one
    /// limit; `Err` with the name of the first Resource whose Grants still
    /// cannot be told (nothing is said of them on a guess).
    pub(crate) async fn known_resources(&self) -> Result<Vec<Arc<resources::Resource>>, String> {
        let all = self.resources.list();
        let deadline = tokio::time::Instant::now() + resources::READ_BACK_WAIT;
        let mut known = Vec::with_capacity(all.len());
        for resource in all {
            match self.resources.read_back(&resource, deadline).await {
                resources::ReadBack::Known => known.push(resource),
                resources::ReadBack::Removed => {}
                resources::ReadBack::Unknown => return Err(resource.name.clone()),
            }
        }
        Ok(known)
    }

    /// The Application `name`; `None` also for a name no Application can
    /// have, without asking the database.
    async fn find(&self, name: &str) -> Result<Option<ApplicationRecord>, DbError> {
        if check_name(name).is_err() {
            return Ok(None);
        }
        self.registry.application(name).await
    }

    /// The Application as `asker` is answered it: whom it is shared with
    /// only to a Contributor or a Platform administrator.
    fn view<'a>(&self, application: &'a ApplicationRecord, asker: &User) -> ApplicationView<'a> {
        let contributor = application.is_contributor(asker);
        let sees_sharing = contributor || self.is_administrator(asker);
        ApplicationView {
            name: &application.name,
            description: application.description.as_deref(),
            address: format!("{}.{}", application.name, self.domain),
            contributors: application.contributors.iter().map(EntryView::of).collect(),
            shared_with: sees_sharing
                .then(|| application.shared_with.iter().map(EntryView::of).collect()),
            contributor,
            serving_version: application.serving_version,
            latest_version: application.latest_version,
            created_by: &application.created_by,
            created_at: &application.created_at,
            disabled: application.disabled.as_ref().map(DisabledView::of),
        }
    }
}

/// An Application as the API answers it.
#[derive(Serialize)]
struct ApplicationView<'a> {
    name: &'a str,
    description: Option<&'a str>,
    address: String,
    contributors: Vec<EntryView<'a>>,
    /// `None` (null) to someone who is neither a Contributor nor a
    /// Platform administrator.
    shared_with: Option<Vec<EntryView<'a>>>,
    /// Whether the asker is a Contributor.
    contributor: bool,
    serving_version: Option<u64>,
    latest_version: Option<u64>,
    created_by: &'a str,
    created_at: &'a str,
    /// Null when it is not disabled.
    disabled: Option<DisabledView<'a>>,
}

/// The answer of a change of a list: the Application as it is after, and
/// the entries this request added and removed, as kept.
#[derive(Serialize)]
struct ChangedView<'a> {
    #[serde(flatten)]
    application: ApplicationView<'a>,
    added: Vec<EntryView<'a>>,
    removed: Vec<EntryView<'a>>,
}

/// What an entry names: `user`, `group` or `everyone: true`, one of them.
#[derive(Serialize)]
struct NamedView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    everyone: Option<bool>,
}

impl<'a> NamedView<'a> {
    fn of(named: &'a Named) -> Self {
        let (user, group, everyone) = match named {
            Named::User(id) => (Some(id.as_str()), None, None),
            Named::Group(group) => (None, Some(group.as_str()), None),
            Named::Everyone => (None, None, Some(true)),
        };
        Self {
            user,
            group,
            everyone,
        }
    }
}

/// An entry of a list as the API answers it: what it names; a person's
/// `name` and `email` (each null when not known); who added it and when.
#[derive(Serialize)]
struct EntryView<'a> {
    #[serde(flatten)]
    named: NamedView<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<Option<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<Option<&'a str>>,
    added_by: &'a str,
    added_at: &'a str,
}

impl<'a> EntryView<'a> {
    fn of(entry: &'a Entry) -> Self {
        let person = matches!(entry.named, Named::User(_));
        Self {
            named: NamedView::of(&entry.named),
            name: person.then_some(entry.name.as_deref()),
            email: person.then_some(entry.email.as_deref()),
            added_by: &entry.added_by,
            added_at: &entry.added_at,
        }
    }
}

/// An entry as kept, for the program's log: what it names, who added it
/// and when; nothing of the person's record.
#[derive(Serialize)]
struct KeptView<'a> {
    #[serde(flatten)]
    named: NamedView<'a>,
    added_by: &'a str,
    added_at: &'a str,
}

impl<'a> KeptView<'a> {
    fn of(entry: &'a Entry) -> Self {
        Self {
            named: NamedView::of(&entry.named),
            added_by: &entry.added_by,
            added_at: &entry.added_at,
        }
    }
}

/// Entries in a message: `the person <id>`, `the Group <Group>`,
/// `everyone`, each quoted as JSON quotes it, joined by commas.
fn described(entries: &[Entry]) -> String {
    let mut text = String::new();
    for (n, entry) in entries.iter().enumerate() {
        if n > 0 {
            text.push_str(", ");
        }
        match &entry.named {
            Named::User(id) => text.push_str(&format!("the person {id:?}")),
            Named::Group(group) => text.push_str(&format!("the Group {group:?}")),
            Named::Everyone => text.push_str("everyone"),
        }
    }
    text
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
    // Copied once per answer a request makes: a few bytes on the API's
    // path, which reads no database for it.
    description: String,
    owner: String,
    contact: String,
    created_by: &'a str,
    created_at: &'a str,
}

impl<'a> ResourceView<'a> {
    fn of(resource: &'a resources::Resource) -> Self {
        Self {
            name: &resource.name,
            kind: resource.kind.name(),
            description: resource.description().to_string(),
            owner: resource.owner().to_string(),
            contact: resource.contact().to_string(),
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
    /// `None` while memory does not know the Resource's Grants.
    fn of(resource: &'a resources::Resource, groups: &[&str]) -> Option<Self> {
        let held = resource.with_grants(|grants| authz::held(groups, grants))?;
        Some(Self {
            face: ResourceView::of(resource),
            your_access: held.map(resources::level_name),
        })
    }
}

/// `409` `name-taken`.
fn name_taken() -> Answer {
    Answer::error(
        StatusCode::CONFLICT,
        "name-taken",
        "a Resource with this name is registered: choose another name",
    )
}

/// A Storage registration refused by its directory's checks.
fn storage_refused(refused: storage::directory::Refused) -> Answer {
    use storage::directory::Refused;
    let status = match refused {
        Refused::InUse => StatusCode::CONFLICT,
        Refused::Busy => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    Answer::error(status, refused.reason(), refused.message())
}

/// `503` `unavailable`: the Grants on the Resource `name` are not known now
/// (crate::resources), and nothing is said on a guess.
fn grants_unknown(name: &str) -> Answer {
    Answer::error(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        &format!(
            "the platform is finding out the Grants on the Resource {name} after a change of them whose answer was lost: try again shortly"
        ),
    )
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
    /// Whether a Platform administrator has disabled it.
    disabled: bool,
}

#[derive(Serialize)]
struct ListedApplicationsView<'a> {
    applications: Vec<ListedApplicationView<'a>>,
}

#[derive(Serialize)]
struct GrantView<'a> {
    group: &'a str,
    access: &'static str,
    granted_by: Option<&'a str>,
    granted_at: Option<&'a str>,
}

#[derive(Serialize)]
struct GrantsView<'a> {
    grants: Vec<GrantView<'a>>,
    grants_more: usize,
}

#[derive(Serialize)]
struct GrantSet<'a> {
    resource: &'a str,
    group: &'a str,
    access: &'static str,
    changed: bool,
    was: Option<&'static str>,
}

#[derive(Serialize)]
struct GrantRevoked<'a> {
    resource: &'a str,
    group: &'a str,
    changed: bool,
    was: Option<&'static str>,
}

/// One Grant in `GET /v1/admin/grants`.
#[derive(Serialize)]
struct GroupGrantView {
    resource: String,
    kind: &'static str,
    access: &'static str,
    granted_by: Option<String>,
    granted_at: Option<String>,
}

#[derive(Serialize)]
struct GroupGrantsView<'a> {
    group: &'a str,
    grants: Vec<GroupGrantView>,
}

/// `GET /v1/resources/<name>/structure`.
#[derive(Serialize)]
struct StructureView<'a> {
    resource: &'a str,
    kind: &'static str,
    fingerprint: &'a str,
    your_access: Option<&'static str>,
    schema_ts: &'a str,
    unsupported: &'a [structure::Unsupported],
    standin_sql: &'a [String],
    contact: &'a str,
    hidden: u64,
}

#[derive(Serialize)]
struct EntriesView {
    entries: Vec<Value>,
    /// The `id` of the first entry answered when an older one matches.
    earlier: Option<i64>,
}

/// The answer of `GET /v1/outbound-hosts`.
#[derive(Serialize)]
struct OutboundHostsView<'a> {
    hosts: Vec<OutboundHostView<'a>>,
}

/// One entry of `GET /v1/outbound-hosts`.
#[derive(Serialize)]
struct OutboundHostView<'a> {
    host: &'a str,
    added_by: &'a str,
    added_at: &'a str,
}

/// The answer of a `PUT /v1/outbound-hosts/<entry>`.
#[derive(Serialize)]
struct OutboundAddedView<'a> {
    host: &'a str,
    added_by: &'a str,
    added_at: &'a str,
    changed: bool,
}

/// The answer of a `DELETE /v1/outbound-hosts/<entry>`.
#[derive(Serialize)]
struct OutboundRemovedView<'a> {
    host: &'a str,
    changed: bool,
}

/// A disable in force, as the API answers it.
#[derive(Serialize)]
struct DisabledView<'a> {
    since: &'a str,
    by: &'a str,
}

impl<'a> DisabledView<'a> {
    fn of(disabled: &'a Disabled) -> Self {
        Self {
            since: &disabled.since,
            by: &disabled.by,
        }
    }
}

/// The answer of a disable or an enable: the Application as it is after,
/// and whether this request changed it.
#[derive(Serialize)]
struct SwitchedView<'a> {
    #[serde(flatten)]
    application: ApplicationView<'a>,
    changed: bool,
}

/// One Application of Platform administrators' list of every Application.
#[derive(Serialize)]
struct AdminApplicationView<'a> {
    name: &'a str,
    description: Option<&'a str>,
    address: String,
    created_by: &'a str,
    created_at: &'a str,
    serving_version: Option<u64>,
    latest_version: Option<u64>,
    /// At most [`MAX_DECLARED_LISTED`] Resources, by name.
    declared_access: Option<&'a Value>,
    declared_more: usize,
    contributors: Vec<EntryView<'a>>,
    shared_with: SharedCountsView,
    disabled: Option<DisabledView<'a>>,
    usage: UsageView<'a>,
}

#[derive(Serialize)]
struct SharedCountsView {
    everyone: bool,
    groups: i64,
    people: i64,
}

#[derive(Serialize)]
struct UsageView<'a> {
    days: u8,
    from: &'a str,
    requests: i64,
    users: i64,
}

#[derive(Serialize)]
struct AdminApplicationsView<'a> {
    applications: Vec<AdminApplicationView<'a>>,
    next: Option<&'a str>,
}

/// A Declared access as stored, cut to its first [`MAX_DECLARED_LISTED`]
/// Resources by name (byte order, as the map keeps them), and how many more
/// it declares.
fn cut_declared(stored: Option<Value>) -> (Option<Value>, usize) {
    let mut more = 0;
    let cut = stored.map(|mut stored| {
        if let Some(Value::Object(resources)) = stored.get_mut("resources")
            && resources.len() > MAX_DECLARED_LISTED
        {
            more = resources.len() - MAX_DECLARED_LISTED;
            let kept: serde_json::Map<String, Value> = std::mem::take(resources)
                .into_iter()
                .take(MAX_DECLARED_LISTED)
                .collect();
            *resources = kept;
        }
        stored
    });
    (cut, more)
}

/// Why a reading for Platform administrators answered nothing.
pub(crate) enum AdminRead {
    /// [`ADMIN_READINGS`] readings run already: answered at once.
    Busy,
    /// It passed its [`ADMIN_READ_TIME`] in the platform database and was
    /// ended.
    TooLong(DbError),
    /// The platform database could not be read.
    Failed(DbError),
}

/// Which Applications a reading of every Application reads.
#[derive(Clone, Copy)]
pub(crate) enum Which<'a> {
    /// A page: those whose name comes after this one (from the first
    /// without it).
    After(Option<&'a str>),
    /// The one Application of this name.
    Named(&'a str),
}

/// A page of every Application, as read: its Applications by name, each
/// one's Declared access cut and how many more it declares, and whether
/// more Applications come after them.
pub(crate) struct EveryApplication {
    pub rows: Vec<AdminApplication>,
    pub declared: Vec<(Option<Value>, usize)>,
    pub more: bool,
}

impl EveryApplication {
    /// The API's `next`: the last Application's name when more come after
    /// it.
    pub fn next(&self) -> Option<&str> {
        if self.more {
            self.rows.last().map(|a| a.name.as_str())
        } else {
            None
        }
    }
}

/// `503`: [`ADMIN_READINGS`] readings for Platform administrators run.
fn readings_busy() -> Answer {
    Answer::error(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "readings for Platform administrators are running: try again shortly",
    )
}

/// Whether a reading failed because it passed its time.
fn took_too_long(error: &DbError) -> bool {
    use tokio_postgres::error::SqlState;
    error.is_timeout()
        || matches!(
            error.code(),
            Some(&SqlState::QUERY_CANCELED | &SqlState::LOCK_NOT_AVAILABLE)
        )
}

/// A query's parameters, each value percent-decoded (`+` read as a space,
/// as a form writes one), only those named in `known`; `Err` with the name
/// of one given twice or whose value cannot be decoded. A parameter written
/// without `=` has the empty value.
pub(crate) fn query_parameters<'k>(
    query: Option<&str>,
    known: &[&'k str],
) -> Result<BTreeMap<&'k str, String>, &'k str> {
    let mut found = BTreeMap::new();
    for pair in query.unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(name) = known.iter().find(|k| **k == key) else {
            continue;
        };
        let spaced = value.replace('+', " ");
        let Some(decoded) = percent_decoded(&spaced) else {
            return Err(name);
        };
        if found.insert(*name, decoded).is_some() {
            return Err(name);
        }
    }
    Ok(found)
}

/// `?after` and `?limit` of `GET /v1/admin/applications`.
pub(crate) fn admin_list_query(query: Option<&str>) -> Result<(Option<String>, u8), String> {
    let mut found = query_parameters(query, &["after", "limit"])
        .map_err(|name| format!("?{name} is given twice or is not percent-encoded"))?;
    let limit = match found.get("limit") {
        None => MAX_ADMIN_PAGE,
        Some(text) => plain_number(text)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=MAX_ADMIN_PAGE).contains(n))
            .ok_or_else(|| {
                "?limit must be a whole number from 1 to 100, written plainly".to_owned()
            })?,
    };
    let after = found.remove("after");
    if let Some(after) = &after
        && matches!(check_name(after), Err(NameProblem::Invalid))
    {
        return Err("?after must be a name an Application could have: 1 to 63 lower-case letters, digits and hyphens".into());
    }
    Ok((after, limit))
}

/// A whole number written plainly: digits, no sign, no leading zero, above
/// 0, at most 18 digits.
pub(crate) fn plain_number(text: &str) -> Option<i64> {
    let canonical = !text.is_empty()
        && !text.starts_with('0')
        && text.len() <= 18
        && text.bytes().all(|b| b.is_ascii_digit());
    canonical.then(|| text.parse().ok()).flatten()
}

/// Whether `text` is RFC 3339 with a date, a time, up to six digits of a
/// second's fraction, and an offset (`Z` or `±hh:mm`), each part in its
/// range; answers it as microseconds since 1970 (for comparing `since` with
/// `until`).
pub(crate) fn rfc3339_micros(text: &str) -> Option<i64> {
    fn digits(text: &str) -> Option<i64> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    }
    let date = text.get(..10)?;
    let rest = text.get(10..)?;
    let (year, month, day) = (
        digits(date.get(..4)?)?,
        digits(date.get(5..7)?)?,
        digits(date.get(8..10)?)?,
    );
    if date.get(4..5)? != "-" || date.get(7..8)? != "-" {
        return None;
    }
    let rest = rest.strip_prefix(['T', 't'])?;
    let (hour, minute, second) = (
        digits(rest.get(..2)?)?,
        digits(rest.get(3..5)?)?,
        digits(rest.get(6..8)?)?,
    );
    if rest.get(2..3)? != ":" || rest.get(5..6)? != ":" {
        return None;
    }
    let mut rest = rest.get(8..)?;
    let mut micros = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 || length > 6 {
            return None;
        }
        let padded = format!("{:0<6}", fraction.get(..length)?);
        micros = digits(&padded)?;
        rest = fraction.get(length..)?;
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.get(3..4)? != ":" {
                return None;
            }
            let (h, m) = (digits(rest.get(1..3)?)?, digits(rest.get(4..6)?)?);
            if h > 23 || m > 59 {
                return None;
            }
            sign * (h * 60 + m)
        }
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if year < 1 || day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days since 1970-01-01 (the civil-from-days inverse).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * m + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset * 60;
    Some(seconds * 1_000_000 + micros)
}

/// The filters of `GET /v1/operation-log`, read and checked.
#[derive(Debug, Default)]
pub(crate) struct LogQuery {
    pub user: Option<String>,
    pub application: Option<String>,
    pub resource: Option<String>,
    /// The host of a `fetch`, as the list writes it.
    pub host: Option<String>,
    pub outcome: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub before: Option<i64>,
    pub limit: i64,
}

/// The filters of `GET /v1/operation-log`, as named in a query.
pub(crate) const LOG_FILTERS: [&str; 8] = [
    "user",
    "application",
    "resource",
    "host",
    "outcome",
    "since",
    "until",
    "before",
];

/// What is wrong with the filters of a reading of the Operation log.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogProblem {
    /// A parameter given twice, or whose value is not percent-encoded.
    Repeated(&'static str),
    /// A value not taken as the contract's table says (empty included).
    Value(&'static str),
    /// `since` not before `until`.
    Order,
}

impl LogProblem {
    /// The API's message, naming the parameter.
    pub fn message(&self) -> String {
        match self {
            Self::Repeated(name) => format!("?{name} is given twice or is not percent-encoded"),
            Self::Order => "?since must be before ?until".into(),
            Self::Value("user") => {
                "?user must be a user id of 1 to 256 bytes without control characters".into()
            }
            Self::Value("application") => "?application must be a name an Application could have: 1 to 63 lower-case letters, digits and hyphens".into(),
            Self::Value("resource") => "?resource must be a name a Resource could have".into(),
            Self::Value("host") => format!(
                "?host must be a call's host with its port when it is not 443, as the list writes one, 1 to {} bytes, a name or an address",
                crate::outbound::MAX_FILTER_BYTES
            ),
            Self::Value("outcome") => "?outcome must be allowed, refused, failed or unfinished".into(),
            Self::Value(time @ ("since" | "until")) => format!(
                "?{time} must be RFC 3339 with a date, a time and an offset (Z or ±hh:mm, + written %2B), up to microseconds"
            ),
            Self::Value("before") => "?before must be a positive whole number, written plainly".into(),
            Self::Value(_) => "?limit must be a whole number from 1 to 1000, written plainly".into(),
        }
    }
}

impl LogQuery {
    /// Every parameter's problem is told naming it.
    fn read(query: Option<&str>) -> Result<Self, LogProblem> {
        let mut names = LOG_FILTERS.to_vec();
        names.push("limit");
        let mut found = query_parameters(query, &names).map_err(LogProblem::Repeated)?;
        let limit = found.remove("limit");
        let mut asked = Self::of_filters(found)?;
        if let Some(limit) = limit {
            asked.limit = plain_number(&limit)
                .filter(|n| (1..=i64::from(oplog::MAX_READ)).contains(n))
                .ok_or(LogProblem::Value("limit"))?;
        }
        Ok(asked)
    }

    /// The filters of [`LOG_FILTERS`] given, each decoded once, checked as
    /// the API checks them, with the API's `limit` of 100. Shared by the
    /// API and the Dashboard's page of the Operation log, which gives what
    /// its form sent here.
    pub(crate) fn of_filters(mut found: BTreeMap<&str, String>) -> Result<Self, LogProblem> {
        let mut asked = Self {
            limit: 100,
            ..Self::default()
        };
        if let Some(user) = found.remove("user") {
            if user.is_empty() || user.len() > MAX_ENTRY_BYTES || user.chars().any(char::is_control)
            {
                return Err(LogProblem::Value("user"));
            }
            asked.user = Some(user);
        }
        if let Some(application) = found.remove("application") {
            if matches!(check_name(&application), Err(NameProblem::Invalid)) {
                return Err(LogProblem::Value("application"));
            }
            asked.application = Some(application);
        }
        if let Some(resource) = found.remove("resource") {
            if !is_resource_name(&resource) {
                return Err(LogProblem::Value("resource"));
            }
            asked.resource = Some(resource);
        }
        if let Some(host) = found.remove("host") {
            // An entry whose `host` is exactly this text: whatever a call's
            // host with its port could be, 1 to 260 bytes. (A host that is
            // no entry matches nothing, and is no fault.)
            let usable = !host.is_empty() && host.len() <= crate::outbound::MAX_FILTER_BYTES;
            if !usable {
                return Err(LogProblem::Value("host"));
            }
            asked.host = Some(host);
        }
        if let Some(outcome) = found.remove("outcome") {
            if !matches!(
                outcome.as_str(),
                "allowed" | "refused" | "failed" | "unfinished"
            ) {
                return Err(LogProblem::Value("outcome"));
            }
            asked.outcome = Some(outcome);
        }
        let mut times = [None, None];
        for (at, name) in [(0, "since"), (1, "until")] {
            if let Some(time) = found.remove(name) {
                let Some(micros) = rfc3339_micros(&time) else {
                    return Err(LogProblem::Value(name));
                };
                if let Some(slot) = times.get_mut(at) {
                    *slot = Some(micros);
                }
                if at == 0 {
                    asked.since = Some(time);
                } else {
                    asked.until = Some(time);
                }
            }
        }
        if let [Some(since), Some(until)] = times
            && since >= until
        {
            return Err(LogProblem::Order);
        }
        if let Some(before) = found.remove("before") {
            let Some(before) = plain_number(&before) else {
                return Err(LogProblem::Value("before"));
            };
            asked.before = Some(before);
        }
        Ok(asked)
    }

    fn filter(&self) -> oplog::Filter<'_> {
        oplog::Filter {
            user: self.user.as_deref(),
            application: self.application.as_deref(),
            resource: self.resource.as_deref(),
            host: self.host.as_deref(),
            outcome: self.outcome.as_deref(),
            since: self.since.as_deref(),
            until: self.until.as_deref(),
            before: self.before,
            limit: self.limit,
        }
    }
}

/// `401`: not signed in, or with a credential that signs nobody in.
fn not_signed_in() -> Answer {
    Answer::error(
        StatusCode::UNAUTHORIZED,
        "not-signed-in",
        "not signed in: sign in with `vibe sign-in`, then ask again",
    )
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

/// `403` `not-shared`: the asker can neither open the Application nor is
/// a Platform administrator. Names no Contributor.
fn not_shared() -> Answer {
    Answer::error(
        StatusCode::FORBIDDEN,
        "not-shared",
        "this Application is not shared with you: ask one of its Contributors to share it with you",
    )
}

/// `403` `not-a-contributor` on a list of the Application's.
fn not_a_contributor_of_lists() -> Answer {
    Answer::error(
        StatusCode::FORBIDDEN,
        "not-a-contributor",
        "you are not a Contributor of this Application, and only its Contributors may change its Contributors and whom it is shared with: ask one of them",
    )
}

/// `403` `not-a-contributor` on reading Versions.
fn not_a_reader_of_versions() -> Answer {
    Answer::error(
        StatusCode::FORBIDDEN,
        "not-a-contributor",
        "you are not a Contributor of this Application, and only its Contributors (and Platform administrators) read its Versions: ask one of them",
    )
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
        let limit = |query: Option<&str>| LogQuery::read(query).ok().map(|asked| asked.limit);
        assert_eq!(limit(None), Some(100));
        assert_eq!(limit(Some("")), Some(100));
        assert_eq!(limit(Some("limit=1")), Some(1));
        assert_eq!(limit(Some("limit=1000")), Some(1000));
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
            assert_eq!(limit(Some(query)), None, "{query}");
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
