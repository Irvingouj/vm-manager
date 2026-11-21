//! What the tests of Storage share (CONTRACT.md, "Storage"; ticket "Storage:
//! a shared, named file Resource, on a mounted directory now and S3
//! later"):
//!
//! - [`Place`], a temporary folder of the test's own holding the storage
//!   root (`root/`) and a place outside it (`outside/`), removed with
//!   everything in it when the test ends, also after it changed modes;
//! - [`Storage`], a program started with `--storage-root` on it (or
//!   without), the stand-in sign-in and the Platform administrators'
//!   Group, with helpers to register Storage Resources on directories of
//!   the root, give Grants, install fixture Applications whose Declared
//!   access names kinds, and call the five operations through the raw
//!   fixture `fixtures/operations.js`;
//! - checks on what the operations answer (a file's metadata, `invalid-key`
//!   with its rule, `not-reachable` with its reason) and on their
//!   Operation log entries;
//! - what the tests look at on the host's directory themselves: bytes,
//!   mode, link count, owner, the extended attribute
//!   `user.vibe.content-type`, the tree of a folder without following a
//!   link, and a FIFO planted;
//! - stopping the program with `SIGTERM` or `SIGKILL` and starting it
//!   again against the same platform database and storage root.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

use crate::api::{Api, SOME_GIT_COMMIT};
use crate::http::{Response, User};
use crate::program::run_to_exit;
use crate::scenario::{ADMINISTRATORS, Expected, administrator, assert_entry, lines};
use crate::shared_database::token;
use crate::vm::{INSTALLER, Vm, VmOptions, fixture_bundle};

/// The Storage Resource most tests register.
pub const MARKETING: &str = "marketing";

/// Whom to ask for access to a Storage Resource the harness registers.
pub const FILES_CONTACT: &str = "Mia Files, #marketing-files on chat";

/// The Group most tests grant `read` on [`MARKETING`].
pub const FILE_READERS: &str = "file-readers";

/// The Group most tests grant `write` on [`MARKETING`].
pub const FILE_WRITERS: &str = "file-writers";

/// The Application (`fixtures/operations.js`) that declares [`MARKETING`]
/// at `write` in [`Storage::ready`].
pub const FILES: &str = "files";

/// The Application (`fixtures/operations.js`) that declares [`MARKETING`]
/// at `read` in [`Storage::ready`].
pub const VIEWER: &str = "viewer";

/// The body bound: 8 MiB.
pub const BODY_BOUND: usize = 8 * 1024 * 1024;

/// The bound of a request to an Application: 8 MiB and 64 KiB.
pub const REQUEST_BODY_BOUND: usize = 8 * 1024 * 1024 + 64 * 1024;

/// How long a request with Storage operations may take.
pub const STORAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The extended attribute that holds a file's content type.
pub const CONTENT_TYPE_ATTRIBUTE: &str = "user.vibe.content-type";

/// A User whose Groups hold `write` on [`MARKETING`].
pub fn wanda() -> User {
    User::new("wanda").in_groups(&[FILE_WRITERS])
}

/// A User whose Groups hold `read` on [`MARKETING`].
pub fn rita() -> User {
    User::new("rita").in_groups(&[FILE_READERS])
}

/// A User whose Groups hold nothing.
pub fn nobody() -> User {
    User::new("nobody").in_groups(&["no-access"])
}

/// Standard base64 with padding.
pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// The bytes of standard base64 with padding; fails the test otherwise.
pub fn unb64(text: &str, context: &str) -> Vec<u8> {
    STANDARD.decode(text).unwrap_or_else(|e| {
        panic!("{context}: expected standard base64 with padding ({e}), got {text:?}")
    })
}

/// A temporary folder of the test's own: `root/` (the storage root, made)
/// and `outside/` (beside it, made). Removed with all it holds when
/// dropped, after giving every folder in it back its write permission.
pub struct Place {
    pub base: PathBuf,
}

impl Place {
    pub fn new(label: &str) -> Self {
        let base = std::env::temp_dir().join(format!("vm-storage-{label}-{}", token()));
        std::fs::create_dir_all(base.join("root"))
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", base.display()));
        std::fs::create_dir_all(base.join("outside"))
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", base.display()));
        Self { base }
    }

    /// The storage root.
    pub fn root(&self) -> PathBuf {
        self.base.join("root")
    }

    /// A folder beside the storage root, outside it.
    pub fn outside(&self) -> PathBuf {
        self.base.join("outside")
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        give_back_write(&self.base);
        drop(std::fs::remove_dir_all(&self.base));
    }
}

fn give_back_write(dir: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    drop(std::fs::set_permissions(
        dir,
        std::fs::Permissions::from_mode(0o755),
    ));
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            give_back_write(&entry.path());
        }
    }
}

/// The body that registers a Storage Resource `name` on `directory`.
pub fn storage_body(name: &str, directory: &str) -> Value {
    json!({
        "name": name,
        "kind": "storage",
        "description": format!("The {name} team's files"),
        "owner": "The Marketing team",
        "contact": FILES_CONTACT,
        "location": { "backend": "directory", "directory": directory },
    })
}

/// The body that registers a Resource of kind `postgres` named `name`, on
/// a database that is never reached (registering does not connect).
pub fn postgres_body(name: &str) -> Value {
    json!({
        "name": name,
        "kind": "postgres",
        "description": format!("The {name} database"),
        "owner": "The DevOps team",
        "contact": "Dana Ops",
        "connection": {
            "host": "127.0.0.1", "port": 1, "database": "nowhere",
            "read": { "role": "nobody_reads", "password": "never-used" }
        },
    })
}

/// The body of a Version: this bundle, declaring these Resources as (name,
/// kind, access level).
pub fn version_body_with_kinds(bundle: &str, declared: &[(&str, &str, &str)]) -> Value {
    let resources: Map<String, Value> = declared
        .iter()
        .map(|(name, kind, access)| {
            (
                (*name).to_owned(),
                json!({ "kind": kind, "access": access }),
            )
        })
        .collect();
    json!({
        "bundle": bundle,
        "declared_access": { "resources": resources },
        "git_commit": SOME_GIT_COMMIT,
    })
}

/// One call for `fixtures/operations.js`.
pub fn op(operation: &str, arguments: &Value) -> Value {
    json!({ "operation": operation, "arguments": arguments })
}

/// `storage.head` of `key` on `resource`.
pub fn head(resource: &str, key: &str) -> Value {
    op("storage.head", &json!({ "resource": resource, "key": key }))
}

/// `storage.get` of `key` on `resource`.
pub fn get(resource: &str, key: &str) -> Value {
    op("storage.get", &json!({ "resource": resource, "key": key }))
}

/// `storage.delete` of `key` on `resource`.
pub fn delete(resource: &str, key: &str) -> Value {
    op(
        "storage.delete",
        &json!({ "resource": resource, "key": key }),
    )
}

/// `storage.put` of `bytes` at `key` on `resource`, with this content type
/// when given.
pub fn put(resource: &str, key: &str, bytes: &[u8], content_type: Option<&str>) -> Value {
    let mut args = json!({ "resource": resource, "key": key, "body": b64(bytes) });
    if let Some(content_type) = content_type {
        args["content_type"] = json!(content_type);
    }
    op("storage.put", &args)
}

/// `storage.list` on `resource` with these arguments beside `resource`.
pub fn list(resource: &str, more: &Value) -> Value {
    let mut args = more.clone();
    args["resource"] = json!(resource);
    op("storage.list", &args)
}

/// A program started with `--storage-root` on a [`Place`] of its own (or
/// without), the stand-in sign-in and [`ADMINISTRATORS`].
pub struct Storage {
    pub vm: Vm,
    pub place: Place,
}

impl Storage {
    /// The program with `--storage-root <place>/root`; nothing registered.
    pub fn start() -> Self {
        let place = Place::new("s");
        let root = place.root();
        Self::start_on(place, Some(&root), &VmOptions::new())
    }

    /// The program on `place`, with `--storage-root <root>` when given,
    /// from these options.
    pub fn start_on(place: Place, root: Option<&Path>, options: &VmOptions) -> Self {
        let mut options = options
            .clone()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS);
        if let Some(root) = root {
            options = options.args_extra(&["--storage-root", &root.to_string_lossy()]);
        }
        Self {
            vm: Vm::start(&options),
            place,
        }
    }

    /// The usual setting: [`MARKETING`] registered on `root/marketing`,
    /// [`FILE_READERS`] granted `read` and [`FILE_WRITERS`] `write`, the
    /// Application [`FILES`] (`fixtures/operations.js`) declaring it at
    /// `write` and [`VIEWER`] (the same fixture) at `read`.
    pub fn ready() -> Self {
        let s = Self::start();
        s.register(MARKETING);
        s.grant(MARKETING, FILE_READERS, "read");
        s.grant(MARKETING, FILE_WRITERS, "write");
        s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
        s.install(VIEWER, "operations.js", &[(MARKETING, "storage", "read")]);
        s
    }

    /// The storage root.
    pub fn root(&self) -> PathBuf {
        self.place.root()
    }

    /// The directory of a Resource registered with [`Storage::register`].
    pub fn dir(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    /// The platform's API as the Platform administrator.
    pub fn admin(&self) -> Api<'_> {
        self.vm.api_as(&administrator())
    }

    /// Makes `root/<name>` and registers the Storage Resource `name` on
    /// it, failing the test unless it is `201`.
    pub fn register(&self, name: &str) -> Value {
        let dir = self.dir(name);
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        self.register_on(name, &dir)
    }

    /// Registers the Storage Resource `name` on `dir`, which must be
    /// usable, failing the test unless it is `201`.
    pub fn register_on(&self, name: &str, dir: &Path) -> Value {
        let answer = self.try_register(name, &dir.to_string_lossy());
        if answer.status != 201 {
            self.vm.fail(&format!(
                "expected `201` registering the Storage Resource {name:?} as a Platform administrator, got\n{}",
                answer.describe()
            ));
        }
        answer.json()
    }

    /// `POST /v1/resources` registering the Storage Resource `name` on
    /// `directory`, as the Platform administrator.
    pub fn try_register(&self, name: &str, directory: &str) -> Response {
        self.admin().create_resource(&storage_body(name, directory))
    }

    /// Registers a Resource of kind `postgres` (never reached), failing the
    /// test unless it is `201`.
    pub fn register_postgres(&self, name: &str) {
        let answer = self.admin().create_resource(&postgres_body(name));
        if answer.status != 201 {
            self.vm.fail(&format!(
                "expected `201` registering the database Resource {name:?}, got\n{}",
                answer.describe()
            ));
        }
    }

    /// Gives `group` a Grant at `access` on `resource`, failing the test
    /// unless it is `200`.
    pub fn grant(&self, resource: &str, group: &str, access: &str) {
        let answer = self.admin().grant(resource, group, access);
        if answer.status != 200 {
            self.vm.fail(&format!(
                "expected `200` granting {group:?} {access:?} on {resource:?}, got\n{}",
                answer.describe()
            ));
        }
    }

    /// Creates the Application `name` as [`INSTALLER`], shares it with
    /// everyone, and sends the fixture `file` as its Version declaring
    /// these Resources as (name, kind, access).
    pub fn install(&self, name: &str, file: &str, declared: &[(&str, &str, &str)]) {
        let api = self.vm.api_as(&User::new(INSTALLER));
        let created = api.create_application(name);
        if created.status != 201 {
            self.vm.fail(&format!(
                "expected `201` creating the Application {name:?}, got\n{}",
                created.describe()
            ));
        }
        api.share_with_everyone(name);
        let sent = api.send_version(
            name,
            &version_body_with_kinds(&fixture_bundle(file), declared),
        );
        if sent.status != 201 {
            self.vm.fail(&format!(
                "expected `201` sending {file} as a Version of {name:?} declaring {declared:?}, got\n{}",
                sent.describe()
            ));
        }
    }

    /// `POST <path>` on the Application as this User with this JSON body,
    /// waiting up to [`STORAGE_REQUEST_TIMEOUT`]; fails unless `200` with
    /// JSON.
    pub fn post(
        &self,
        user: &User,
        application: &str,
        path: &str,
        body: &Value,
    ) -> (Response, Value) {
        let response = self
            .vm
            .request(application, path)
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(STORAGE_REQUEST_TIMEOUT)
            .send();
        if response.status != 200 {
            self.vm.fail(&format!(
                "expected `200` from {application:?} for POST {path} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        (response, json)
    }

    /// These calls through `fixtures/operations.js`, one after the other;
    /// the request's answer and the host's answers.
    pub fn calls(&self, user: &User, application: &str, calls: &[Value]) -> (Response, Vec<Value>) {
        let (response, json) = self.post(user, application, "/", &json!({ "calls": calls }));
        let answers = json["answers"].as_array().cloned().unwrap_or_else(|| {
            self.vm.fail(&format!(
                "expected {{ answers: [...] }} from {application:?}, got {json}"
            ))
        });
        (response, answers)
    }

    /// One call through `fixtures/operations.js`.
    pub fn one(&self, user: &User, application: &str, call: &Value) -> (Response, Value) {
        let (response, answers) = self.calls(user, application, std::slice::from_ref(call));
        (response, answers.into_iter().next().unwrap_or(Value::Null))
    }

    /// One call as [`wanda`] through [`FILES`]; the host's answer.
    pub fn as_writer(&self, call: &Value) -> Value {
        self.one(&wanda(), FILES, call).1
    }

    /// Puts `bytes` at `key` of [`MARKETING`] as [`wanda`], failing the test
    /// unless it is `{ ok }`; the metadata answered.
    pub fn put_ok(&self, key: &str, bytes: &[u8], content_type: Option<&str>) -> Value {
        let answer = self.as_writer(&put(MARKETING, key, bytes, content_type));
        if !answer["ok"].is_object() {
            self.vm.fail(&format!(
                "expected the put of {key:?} to be answered {{ ok: <metadata> }}, got {answer}"
            ));
        }
        answer["ok"].clone()
    }

    /// Every page of a list of [`MARKETING`] with these arguments, `limit`
    /// a page, as [`wanda`]: each page's `{ files, folders, cursor }`.
    pub fn every_page(&self, more: &Value, limit: u64) -> Vec<Value> {
        let mut pages = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut args = more.clone();
            args["limit"] = json!(limit);
            if let Some(c) = &cursor {
                args["cursor"] = json!(c);
            }
            let answer = self.as_writer(&list(MARKETING, &args));
            let page = answer["ok"].clone();
            if !page.is_object() {
                self.vm.fail(&format!(
                    "expected a page {{ ok: {{ files, folders, cursor }} }}, got {answer}"
                ));
            }
            cursor = page["cursor"].as_str().map(str::to_owned);
            pages.push(page);
            if cursor.is_none() || pages.len() > 2000 {
                return pages;
            }
        }
    }

    /// Every entry of the Operation log, read by the administrator.
    pub fn log(&self) -> Vec<Value> {
        self.admin().operation_log_entries()
    }

    /// The Operation log entries of the request this answers.
    pub fn entries_of(&self, response: &Response) -> Vec<Value> {
        let id = response.request_id();
        self.log()
            .into_iter()
            .filter(|e| e["request"].as_str() == Some(id.as_str()))
            .collect()
    }

    /// The entries of the request this answers, waiting up to 5 seconds
    /// until there are `count` of them and none is `unfinished`.
    pub fn entries_eventually(&self, response: &Response, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let entries = self.entries_of(response);
            let done =
                entries.len() >= count && entries.iter().all(|e| e["outcome"] != "unfinished");
            if done || Instant::now() > deadline {
                return entries;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The one entry of the request this answers.
    pub fn entry_of(&self, response: &Response) -> Value {
        let entries = self.entries_eventually(response, 1);
        match entries.as_slice() {
            [entry] => entry.clone(),
            _ => self.vm.fail(&format!(
                "expected exactly one Operation log entry for request {}, got {}:\n{}",
                response.request_id(),
                entries.len(),
                lines(&entries)
            )),
        }
    }
}

/// What a Storage operation's entry must say, from [`Expected`] with the
/// operation and statement set: the entry also has `own_database` false,
/// `actor` `user` and `transaction` null.
#[expect(
    clippy::needless_pass_by_value,
    reason = "callers write `Expected { .. }` in place; taking it by value reads as the struct update it is"
)]
pub fn storage_expected<'a>(
    base: Expected<'a>,
    operation: &'a str,
    statement: &'a str,
) -> Expected<'a> {
    Expected {
        operation,
        statement: Some(statement),
        actor: Some("user"),
        own_database: Some(false),
        ..base
    }
}

/// Fails unless `entry` says what `want` says ([`assert_entry`]) and has
/// `transaction` null.
pub fn assert_storage_entry(entry: &Value, want: &Expected<'_>, context: &str) {
    assert_entry(entry, want, context);
    assert!(
        entry.get("transaction").is_none_or(Value::is_null),
        "{context}: expected `transaction` null for a Storage operation, got the entry {entry}"
    );
}

/// The statement of an operation on a key: `<word> "<key as JSON>"`.
pub fn statement_of(word: &str, key: &str) -> String {
    format!("{word} {}", json_key(key))
}

/// Whether `c` is a character the key rule `character` names (other than
/// `\`): a control, U+0000 to U+001F or U+007F to U+009F, or a
/// bidirectional mark.
pub fn is_named_character(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// `key` as the Operation log writes it: a JSON string in which every
/// character the rule `character` names is `\uXXXX` (lower-case hex, as
/// JSON writes the controls it must escape), `"` is `\"` and `\` is `\\`.
pub fn json_key(key: &str) -> String {
    let mut out = String::from("\"");
    for c in key.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if is_named_character(c) => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `text` is RFC 3339 in UTC with whole seconds:
/// `YYYY-MM-DDTHH:MM:SSZ`.
pub fn is_whole_seconds_utc(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// Fails unless `metadata` is exactly `{ key, size, content_type,
/// last_modified }` with these values, `last_modified` RFC 3339 in UTC
/// with whole seconds.
pub fn assert_metadata(
    metadata: &Value,
    key: &str,
    size: usize,
    content_type: &str,
    context: &str,
) {
    let mut keys: Vec<&str> = metadata
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["content_type", "key", "last_modified", "size"],
        "{context}: expected a file's metadata {{ key, size, content_type, last_modified }} and nothing else, got {metadata}"
    );
    assert_eq!(
        metadata["key"],
        json!(key),
        "{context}: expected the key {key:?}, got {metadata}"
    );
    assert_eq!(
        metadata["size"].as_u64(),
        u64::try_from(size).ok(),
        "{context}: expected the size {size}, got {metadata}"
    );
    assert_eq!(
        metadata["content_type"],
        json!(content_type),
        "{context}: expected the content type {content_type:?}, got {metadata}"
    );
    let time = metadata["last_modified"].as_str().unwrap_or_default();
    assert!(
        is_whole_seconds_utc(time),
        "{context}: expected `last_modified` as RFC 3339 in UTC with whole seconds (2026-11-21T10:15:30Z), got {metadata}"
    );
}

/// Fails unless `answer` is `{ ok: <metadata> }` as [`assert_metadata`].
pub fn assert_ok_metadata(
    answer: &Value,
    key: &str,
    size: usize,
    content_type: &str,
    context: &str,
) {
    assert!(
        answer["ok"].is_object()
            && answer.get("error").is_none()
            && answer.get("refused").is_none(),
        "{context}: expected {{ ok: <metadata> }}, got {answer}"
    );
    assert_metadata(&answer["ok"], key, size, content_type, context);
}

/// Fails unless `answer` is a get's `{ ok: { ...metadata, body } }` holding
/// exactly `bytes` (base64 with padding) with this content type.
pub fn assert_got(answer: &Value, key: &str, bytes: &[u8], content_type: &str, context: &str) {
    let ok = &answer["ok"];
    assert!(
        ok.is_object(),
        "{context}: expected {{ ok: {{ ...metadata, body }} }}, got {answer}"
    );
    let body = ok["body"]
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected `body`, the bytes in base64, got {answer}"));
    assert_eq!(
        body,
        b64(bytes),
        "{context}: expected the body to be the bytes put, in standard base64 with padding"
    );
    let mut metadata = ok.clone();
    if let Some(o) = metadata.as_object_mut() {
        o.remove("body");
    }
    assert_metadata(&metadata, key, bytes.len(), content_type, context);
}

/// Fails unless `answer` is `{ ok: null }`.
pub fn assert_ok_null(answer: &Value, context: &str) {
    assert!(
        answer.get("ok").is_some_and(Value::is_null) && answer.get("error").is_none(),
        "{context}: expected {{ ok: null }} (no file has the key), got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "invalid-key", rule, message } }`.
pub fn assert_invalid_key(answer: &Value, rule: &str, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("invalid-key"),
        "{context}: expected {{ error: {{ kind: \"invalid-key\", rule: {rule:?} }} }}, got {answer}"
    );
    assert_eq!(
        answer["error"]["rule"].as_str(),
        Some(rule),
        "{context}: expected the rule {rule:?}, got {answer}"
    );
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{context}: expected a message saying what the key breaks, got {answer}"
    );
}

/// Fails unless `answer` is `{ error: { kind: "not-reachable", reason } }`.
pub fn assert_not_reachable(answer: &Value, reason: &str, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("not-reachable"),
        "{context}: expected {{ error: {{ kind: \"not-reachable\", reason: {reason:?} }} }}, got {answer}"
    );
    assert_eq!(
        answer["error"]["reason"].as_str(),
        Some(reason),
        "{context}: expected the reason {reason:?}, got {answer}"
    );
}

/// The keys and folders of a list answer `{ ok: { files, folders } }`, in
/// the order answered: files as their key, folders as they are.
pub fn listed_names(answer: &Value) -> (Vec<String>, Vec<String>) {
    let files = answer["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| f["key"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let folders = answer["folders"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| f.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    (files, folders)
}

/// The mode bits (permissions) of `path`, not following a link.
pub fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .mode()
        & 0o7777
}

/// Sets the permission bits of `path`.
pub fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("cannot chmod {}: {e}", path.display()));
}

/// The value of the extended attribute `name` of `path` (not following a
/// link), or `None` when it has none.
pub fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buffer = vec![0_u8; 4096];
    match rustix::fs::lgetxattr(path, name, &mut buffer[..]) {
        Ok(n) => {
            buffer.truncate(n);
            Some(buffer)
        }
        Err(rustix::io::Errno::NODATA) => None,
        Err(e) => panic!(
            "cannot read the attribute {name} of {}: {e}",
            path.display()
        ),
    }
}

/// Sets the extended attribute `name` of `path` to `value`.
pub fn set_xattr(path: &Path, name: &str, value: &[u8]) {
    rustix::fs::setxattr(path, name, value, rustix::fs::XattrFlags::empty())
        .unwrap_or_else(|e| panic!("cannot set the attribute {name} of {}: {e}", path.display()));
}

/// Makes a FIFO at `path`.
pub fn mkfifo(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap_or_else(|e| panic!("cannot make the FIFO {}: {e}", path.display()));
}

/// Writes `bytes` to `path`, making its folders: a file placed by hand.
pub fn place(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// Every entry under `dir`, not following links, as `<relative path>` plus
/// `/` for a folder, ` -> <target>` for a link, ` |` for a FIFO, sorted;
/// with each file's size and an empty list when `dir` is not there.
pub fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

fn walk(base: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            let target = std::fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default();
            found.push(format!("{relative} -> {target}"));
        } else if kind.is_dir() {
            found.push(format!("{relative}/"));
            walk(base, &path, found);
        } else if kind.is_file() {
            found.push(format!("{relative} ({} bytes)", meta.len()));
        } else {
            found.push(format!("{relative} |"));
        }
    }
}

/// Seconds since the epoch of the time of last modification of `path`.
pub fn mtime(path: &Path) -> i64 {
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .mtime()
}

/// The user id of this test process.
pub fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Bytes holding every value 0 to 255, then `marker`: a body with NUL and
/// bytes that are not UTF-8.
pub fn binary_with(marker: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..=255_u8).collect();
    bytes.extend_from_slice(marker.as_bytes());
    bytes
}

/// What the program was started with, once it has been stopped: to start
/// it again against the same platform database and storage root.
pub struct Stopped {
    options: VmOptions,
}

impl Stopped {
    /// Starts the program again with the same options and platform
    /// database.
    pub fn start(&self) -> Vm {
        Vm::start(&self.options)
    }
}

/// Stops the program with `SIGTERM` and waits up to 15 seconds for it to
/// end (then it is killed, as when it is dropped).
pub fn terminate(vm: Vm) -> Stopped {
    let options = vm.options().clone().platform(vm.platform());
    let pid = vm.pid();
    let mut command = Command::new("kill");
    command.arg("-TERM").arg(pid.to_string());
    drop(run_to_exit(command, Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && alive(pid) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(vm);
    Stopped { options }
}

/// Ends the program with `SIGKILL` at once.
pub fn kill(vm: Vm) -> Stopped {
    let options = vm.options().clone().platform(vm.platform());
    drop(vm);
    Stopped { options }
}

/// Whether the process is there and not a zombie.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit(')')
                .next()
                .map(|rest| !rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Fails unless the host's `host-call` records of the request this
/// answers name exactly these operations, in order.
pub fn assert_host_calls(vm: &Vm, response: &Response, operations: &[&str], context: &str) {
    let records = vm.records_of(response);
    let named: Vec<String> = records
        .host_calls()
        .iter()
        .map(|r| r["operation"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        named, operations,
        "{context}: expected the request's `host-call` records to name {operations:?}, got {named:?}"
    );
}

/// Fails if any answer or program log line holds `needle`.
pub fn assert_nowhere(vm: &Vm, texts: &[String], needle: &str, what: &str, context: &str) {
    for text in texts {
        assert!(
            !text.contains(needle),
            "{context}: expected no answer to hold {what}, but one does: {text}"
        );
    }
    for line in vm.lines() {
        assert!(
            !line.raw.contains(needle),
            "{context}: expected no record of the program's log to hold {what}, but one does: {}",
            line.raw
        );
    }
}

/// Waits up to `within` for `condition`, checking every 100 ms.
pub fn eventually(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
