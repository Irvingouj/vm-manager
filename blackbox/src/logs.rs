//! What the tests of "Versions: list them, go back to the previous one,
//! read logs" share (CONTRACT.md, "Versions, going back, and logs"):
//!
//! - `fixtures/prints.js` with its marker and the lines it prints while
//!   loading set ([`prints_bundle`]), and the bodies that make it print
//!   ([`printing`], [`Print`]);
//! - `POST /v1/applications/<name>/go-back` ([`go_back`]) and the fields of
//!   its answer ([`GONE_BACK_KEYS`]);
//! - `GET /v1/applications/<name>/logs` ([`logs`], [`page`]) with its
//!   parameters encoded ([`encode`]), the fields of an entry
//!   ([`ENTRY_KEYS`], [`assert_entry_shape`]), waiting for a request's entry
//!   to be readable ([`wait_for_entry`]), and reading everything after a
//!   cursor as following does ([`read_after`]);
//! - Versions with a commit of the test's choosing ([`commit_of`],
//!   [`version_body_with_commit`]).

use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::Api;
use crate::http::{Response, User};
use crate::vm::{Vm, fixture_bundle};

/// How long a test polls for a log entry before it fails. The contract
/// promises 2 seconds; the test waits longer so that a late entry is told
/// apart from one that never comes.
pub const ENTRY_WAIT: Duration = Duration::from_secs(10);

/// What the contract promises: an entry can be read within 2 seconds of its
/// response.
pub const ENTRY_PROMISED: Duration = Duration::from_secs(2);

/// How often a test asks again while it waits for an entry.
const POLL: Duration = Duration::from_millis(50);

/// The fields of an entry, sorted.
pub const ENTRY_KEYS: [&str; 12] = [
    "dropped_before",
    "id",
    "method",
    "path",
    "printed",
    "request",
    "status",
    "time",
    "truncated",
    "uncaught",
    "user",
    "version",
];

/// The fields of the answer to a go-back, sorted.
pub const GONE_BACK_KEYS: [&str; 4] = [
    "application",
    "migrations_not_undone",
    "serving_version",
    "was",
];

/// The bundle of `fixtures/prints.js` with its marker set and printing
/// `at_load` lines (`<marker> loading <i>`) each time its top level runs.
pub fn prints_bundle(marker: &str, at_load: usize) -> String {
    let template = fixture_bundle("prints.js");
    assert!(
        template.contains("%MARKER%") && template.contains("%AT_LOAD%"),
        "fixtures/prints.js has lost its %MARKER% or %AT_LOAD%"
    );
    template
        .replace("%MARKER%", marker)
        .replace("%AT_LOAD%", &at_load.to_string())
}

/// One item of what `fixtures/prints.js` is asked to print.
#[derive(Clone, Debug)]
pub struct Print {
    pub level: String,
    pub message: String,
    pub repeat: usize,
    pub times: usize,
}

impl Print {
    /// `message` once, with `console[level]`.
    pub fn new(level: &str, message: &str) -> Self {
        Self {
            level: level.to_owned(),
            message: message.to_owned(),
            repeat: 1,
            times: 1,
        }
    }

    /// The message made of `message` written `repeat` times.
    #[must_use]
    pub fn repeated(mut self, repeat: usize) -> Self {
        self.repeat = repeat;
        self
    }

    /// Printed `times` times.
    #[must_use]
    pub fn times(mut self, times: usize) -> Self {
        self.times = times;
        self
    }

    /// The message as printed.
    pub fn text(&self) -> String {
        self.message.repeat(self.repeat)
    }

    fn to_json(&self) -> Value {
        json!({ "level": self.level, "message": self.message, "repeat": self.repeat, "times": self.times })
    }
}

/// A record of `printed` as an entry holds it.
pub fn record(level: &str, message: &str, loading: bool) -> Value {
    json!({ "level": level, "message": message, "loading": loading })
}

/// The records of `printed` these make, printed by the request itself.
pub fn records_of(prints: &[Print]) -> Vec<Value> {
    prints
        .iter()
        .flat_map(|p| std::iter::repeat_n(record(&p.level, &p.text(), false), p.times))
        .collect()
}

/// The body that makes `fixtures/prints.js` print these, then throw
/// `throw` when given.
pub fn printing(prints: &[Print], throw: Option<&str>) -> Value {
    let mut body = json!({ "print": prints.iter().map(Print::to_json).collect::<Vec<_>>() });
    if let Some(message) = throw {
        body["throw"] = json!(message);
    }
    body
}

/// A `POST <path>` of this body to `application` as `user`.
pub fn post_to(vm: &Vm, user: &User, application: &str, path: &str, body: &Value) -> Response {
    vm.request(application, path)
        .as_user(user)
        .method("POST")
        .json(body)
        .send()
}

/// `POST /v1/applications/<application>/go-back` with this body.
pub fn go_back(api: &Api<'_>, application: &str, body: &Value) -> Response {
    api.post(&format!("/v1/applications/{application}/go-back"), body)
}

/// `{ "from": from }`, and `"to": to` when given.
pub fn from_to(from: u64, to: Option<u64>) -> Value {
    match to {
        Some(to) => json!({ "from": from, "to": to }),
        None => json!({ "from": from }),
    }
}

/// Text made safe for a URL's query: every byte but letters, digits and
/// `-._~` written `%XX`.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `GET /v1/applications/<application>/logs<query>`; `query` is empty or
/// begins with `?`.
pub fn logs(api: &Api<'_>, application: &str, query: &str) -> Response {
    api.get(&format!("/v1/applications/{application}/logs{query}"))
}

/// One answer of the logs: its entries, oldest first, and `next`.
#[derive(Clone, Debug)]
pub struct Page {
    pub entries: Vec<Value>,
    pub next: String,
    pub body: Value,
}

impl Page {
    /// The `request` of each entry (null for an entry printed outside a
    /// request).
    pub fn requests(&self) -> Vec<Option<String>> {
        self.entries
            .iter()
            .map(|e| e["request"].as_str().map(str::to_owned))
            .collect()
    }

    /// The `id` of each entry.
    pub fn ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e["id"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

/// [`logs`], which must answer `200` with exactly `{ entries, next }`,
/// each entry of the contract's shape and `next` a string.
pub fn page(api: &Api<'_>, application: &str, query: &str) -> Page {
    let answer = logs(api, application, query);
    assert!(
        answer.status == 200,
        "expected `200` reading the logs of {application:?} with {query:?}, got\n{}",
        answer.describe()
    );
    crate::api::assert_json_content_type(&answer);
    let body = answer.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["entries", "next"],
        "expected the logs to be exactly {{ entries, next }}, got {body}"
    );
    let entries = body["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `entries` an array, got {body}"))
        .clone();
    for entry in &entries {
        assert_entry_shape(entry);
    }
    let next = body["next"]
        .as_str()
        .unwrap_or_else(|| panic!("expected `next` a cursor (text), got {body}"))
        .to_owned();
    Page {
        entries,
        next,
        body,
    }
}

/// Fails unless `entry` has exactly the contract's fields, each of its type:
/// `id` text, `time` RFC 3339, `request`, `user`, `method`, `path` text or
/// null, `version` a number, `status` a number or null, `printed` a list of
/// `{ level, message }`, `uncaught` a list of text, `truncated` a boolean,
/// `dropped_before` a number.
pub fn assert_entry_shape(entry: &Value) {
    let mut keys: Vec<&str> = entry
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys, ENTRY_KEYS,
        "expected an entry to have exactly the contract's fields, got {entry}"
    );
    assert!(entry["id"].is_string(), "expected `id` text in {entry}");
    assert!(
        entry["time"]
            .as_str()
            .and_then(crate::api::parse_rfc3339)
            .is_some(),
        "expected `time` an RFC 3339 time in {entry}"
    );
    for field in ["request", "user", "method", "path"] {
        assert!(
            entry[field].is_string() || entry[field].is_null(),
            "expected `{field}` text or null in {entry}"
        );
    }
    assert!(
        entry["status"].is_u64() || entry["status"].is_null(),
        "expected `status` a number or null in {entry}"
    );
    assert!(
        entry["version"].is_u64(),
        "expected `version` a number in {entry}"
    );
    let printed = entry["printed"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `printed` a list in {entry}"));
    for record in printed {
        let mut inner: Vec<&str> = record
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        inner.sort_unstable();
        assert_eq!(
            inner,
            ["level", "loading", "message"],
            "expected each of `printed` to be {{ level, message, loading }} in {entry}"
        );
        assert!(
            record["level"].is_string()
                && record["message"].is_string()
                && record["loading"].is_boolean(),
            "expected `level` and `message` text and `loading` a boolean in {entry}"
        );
    }
    let uncaught = entry["uncaught"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `uncaught` a list in {entry}"));
    assert!(
        uncaught.iter().all(Value::is_string),
        "expected each of `uncaught` text in {entry}"
    );
    assert!(
        entry["truncated"].is_boolean(),
        "expected `truncated` a boolean in {entry}"
    );
    assert!(
        entry["dropped_before"].is_u64(),
        "expected `dropped_before` a number in {entry}"
    );
}

/// Polls `?request=<id>` until the entry of that request can be read, at
/// most [`ENTRY_WAIT`]; returns it and how long after the call it was
/// first read. Fails, saying what it waited for, when it never comes.
pub fn wait_for_entry(api: &Api<'_>, application: &str, request: &str) -> (Value, Duration) {
    let began = Instant::now();
    loop {
        let found = page(api, application, &format!("?request={}", encode(request)));
        if let Some(entry) = found.entries.first() {
            assert_eq!(
                found.entries.len(),
                1,
                "expected only the entry of request {request:?} with ?request=, got {:?}",
                found.entries
            );
            return (entry.clone(), began.elapsed());
        }
        assert!(
            began.elapsed() < ENTRY_WAIT,
            "expected the log entry of request {request:?} of {application:?} to be readable (the contract promises within {ENTRY_PROMISED:?} of its response); waited {ENTRY_WAIT:?} and it never came"
        );
        thread::sleep(POLL);
    }
}

/// Waits for the entry of each of these requests, in turn.
pub fn wait_for_entries(api: &Api<'_>, application: &str, responses: &[&Response]) -> Vec<Value> {
    responses
        .iter()
        .map(|r| wait_for_entry(api, application, &r.request_id()).0)
        .collect()
}

/// Every entry after `after`, read as following does: `?after=<next>` again
/// and again, `limit` at a time, until an answer has no entry. Returns the
/// entries and the last `next`.
pub fn read_after(
    api: &Api<'_>,
    application: &str,
    after: &str,
    limit: usize,
) -> (Vec<Value>, String) {
    let mut cursor = after.to_owned();
    let mut all = Vec::new();
    for _ in 0..100_000 {
        let got = page(
            api,
            application,
            &format!("?after={}&limit={limit}", encode(&cursor)),
        );
        if got.entries.is_empty() {
            assert_eq!(
                got.next, cursor,
                "expected `next` to be `after` as given when no entry is answered"
            );
            return (all, cursor);
        }
        all.extend(got.entries);
        cursor = got.next;
    }
    panic!("the harness's own bound: more than 100,000 pages of logs");
}

/// A `git_commit` of 40 hex digits told apart from the others by any of
/// its first digits: `n`'s hex digit, 40 times (`n` from 1 to 15).
pub fn commit_of(n: u64) -> String {
    assert!(
        (1..16).contains(&n),
        "the harness's own bound: commit_of(1..=15)"
    );
    format!("{n:x}").repeat(40)
}

/// The body of a Version: this bundle, nothing declared, this commit.
pub fn version_body_with_commit(bundle: &str, commit: &str) -> Value {
    json!({
        "bundle": bundle,
        "declared_access": { "resources": {} },
        "git_commit": commit,
    })
}
