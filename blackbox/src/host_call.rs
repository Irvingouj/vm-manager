//! What the tests of "A host call's arguments are bounded" share
//! (CONTRACT.md, the section of that name): a program with the Application
//! [`APP`] (`fixtures/host-call-bounds.js`, with an own database, so that a
//! call can wait with `select pg_sleep(..)` and no Grant), the calls the
//! fixture makes (see its header for what a call and its `spec` are), and
//! checks of what it answers.

use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{Response, User};
use crate::own_database::{Own, first_tables, version_body_with_migrations};
use crate::vm::fixture_bundle;

/// The Application every test calls through.
pub const APP: &str = "caller";

/// The fixture it runs.
pub const FIXTURE: &str = "host-call-bounds.js";

/// 1 MiB.
pub const MIB: usize = 1024 * 1024;

/// One call: 8 MiB.
pub const CALL_BOUND: usize = 8 * MIB;

/// One request: 16 MiB not yet answered.
pub const REQUEST_BOUND: usize = 16 * MIB;

/// One request: 1,024 calls not yet answered.
pub const REQUEST_CALLS: usize = 1024;

/// The program: 256 MiB not yet answered.
pub const PROGRAM_BOUND: usize = 256 * MIB;

/// How long a request of these tests may take.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// The program, [`APP`] installed.
pub struct Calls {
    pub own: Own,
}

impl Calls {
    pub fn start() -> Self {
        let own = Own::start();
        own.create(APP);
        let body = version_body_with_migrations(&fixture_bundle(FIXTURE), &[], &[first_tables()]);
        let sent = own.send_body(APP, &body);
        if sent.status != 201 {
            own.vm.fail(&format!(
                "expected `201` sending {FIXTURE} as a Version of {APP:?}, got\n{}",
                sent.describe()
            ));
        }
        Self { own }
    }

    /// As [`Calls::start`], and these Applications besides [`APP`], each
    /// the same fixture with an own database of its own (the program's
    /// shares are an Application's: a test of them needs several).
    pub fn start_with(more: &[&str]) -> Self {
        let h = Self::start();
        for name in more {
            h.own.create(name);
            let body =
                version_body_with_migrations(&fixture_bundle(FIXTURE), &[], &[first_tables()]);
            let sent = h.own.send_body(name, &body);
            if sent.status != 201 {
                h.own.vm.fail(&format!(
                    "expected `201` sending {FIXTURE} as a Version of {name:?}, got\n{}",
                    sent.describe()
                ));
            }
        }
        h
    }

    pub fn user() -> User {
        User::new("alice")
    }

    /// `POST <path>` on the Application `application` as `user`, whatever
    /// the answer.
    pub fn send_to(&self, application: &str, user: &User, path: &str, body: &Value) -> Response {
        self.own
            .vm
            .request(application, path)
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(TIMEOUT)
            .send()
    }

    /// As [`Calls::post_as`], on `application`.
    pub fn post_to(
        &self,
        application: &str,
        user: &User,
        path: &str,
        body: &Value,
    ) -> (Response, Value) {
        let response = self.send_to(application, user, path, body);
        if response.status != 200 {
            self.own.vm.fail(&format!(
                "expected `200` from {application:?} {path} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        (response, json)
    }

    /// As [`Calls::seq`], on `application`.
    pub fn seq_on(&self, application: &str, calls: &[Value]) -> (Response, Vec<Value>) {
        let (response, body) = self.post_to(
            application,
            &Self::user(),
            "/seq",
            &json!({ "calls": calls }),
        );
        let results = self.results(&body, "results");
        (response, results)
    }

    /// `POST <path>` with this body as `user`, whatever the answer.
    pub fn send(&self, user: &User, path: &str, body: &Value) -> Response {
        self.own
            .vm
            .request(APP, path)
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(TIMEOUT)
            .send()
    }

    /// `POST <path>` as alice; fails unless `200` with JSON. Returns the
    /// response and its body.
    pub fn post(&self, path: &str, body: &Value) -> (Response, Value) {
        self.post_as(&Self::user(), path, body)
    }

    pub fn post_as(&self, user: &User, path: &str, body: &Value) -> (Response, Value) {
        let response = self.send(user, path, body);
        if response.status != 200 {
            self.own.vm.fail(&format!(
                "expected `200` from {path} as {:?}, got\n{}",
                user.id,
                response.describe()
            ));
        }
        let json = response.json();
        (response, json)
    }

    /// `/seq` of these calls: each call's result, in order.
    pub fn seq(&self, calls: &[Value]) -> (Response, Vec<Value>) {
        let (response, body) = self.post("/seq", &json!({ "calls": calls }));
        let results = self.results(&body, "results");
        (response, results)
    }

    /// The array `key` of the fixture's answer.
    pub fn results(&self, body: &Value, key: &str) -> Vec<Value> {
        body[key].as_array().cloned().unwrap_or_else(|| {
            self.own.vm.fail(&format!(
                "expected {{ {key}: [...] }} from the fixture, got {}",
                short(body)
            ))
        })
    }

    /// The number of entries of the Operation log (the newest 1,000).
    pub fn log_len(&self) -> usize {
        self.own.log().len()
    }

    /// The `host-call` records of this request that were refused, with
    /// their reasons.
    pub fn refused_records(&self, response: &Response) -> Vec<Value> {
        self.own
            .vm
            .records_of(response)
            .host_calls()
            .into_iter()
            .filter(|r| r["outcome"] == "refused")
            .cloned()
            .collect()
    }
}

/// A body cut to a length a failure message can hold.
pub fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 1500 {
        format!("{}...", text.chars().take(1500).collect::<String>())
    } else {
        text
    }
}

/// This call, its arguments not measured by the fixture (it would stringify
/// them again: the many calls of the memory tests would use more CPU than
/// a request may).
pub fn unmeasured(mut call: Value) -> Value {
    call["noMeasure"] = json!(true);
    call
}

/// A call: `op` with arguments built from `spec`.
pub fn call(op: &str, spec: Value) -> Value {
    let mut call = serde_json::Map::new();
    call.insert("op".to_owned(), json!(op));
    call.insert("spec".to_owned(), spec);
    Value::Object(call)
}

/// This call made `n` times.
pub fn times(mut call: Value, n: usize) -> Value {
    call["times"] = json!(n);
    call
}

/// A call of `op` whose arguments are `base` plus a string making their
/// text exactly `bytes` as UTF-8, in the key `path` (or `params`).
pub fn padded(op: &str, base: &Value, path: &str, bytes: usize, multibyte: bool) -> Value {
    call(
        op,
        json!({ "pad": { "bytes": bytes, "base": base, "path": path, "multibyte": multibyte } }),
    )
}

/// A call of `op` with `{ pad }` of exactly `bytes`.
pub fn sized(op: &str, bytes: usize) -> Value {
    padded(op, &json!({}), "pad", bytes, false)
}

/// A `query` on the own database with the statement `sql` and one parameter
/// of the code's own making the text exactly `bytes` long.
pub fn query_of(sql: &str, bytes: usize, multibyte: bool) -> Value {
    padded(
        "query",
        &json!({ "own": true, "as": "application", "sql": sql }),
        "params",
        bytes,
        multibyte,
    )
}

/// A query that waits `seconds` in the database, its arguments `bytes`.
pub fn sleeper(seconds: u32, bytes: usize) -> Value {
    query_of(
        &format!("select pg_sleep({seconds}) as s, length($1::text) as n"),
        bytes,
        false,
    )
}

/// A query that answers at once, its arguments `bytes`.
pub fn quick(bytes: usize) -> Value {
    query_of("select length($1::text) as n", bytes, false)
}

/// A call of an operation the host does not know.
pub fn unknown(spec: Value) -> Value {
    call("nonesuch", spec)
}

/// `{ plain: value }`.
pub fn plain(value: &Value) -> Value {
    json!({ "plain": value })
}

/// A `{ error: { kind: "limit", limit: <limit> } }`.
pub fn assert_limit(result: &Value, limit: &str, context: &str) {
    let answer = &result["answer"];
    assert_eq!(
        (
            answer["error"]["kind"].as_str(),
            answer["error"]["limit"].as_str()
        ),
        (Some("limit"), Some(limit)),
        "{context}: expected {{ error: {{ kind: \"limit\", limit: \"{limit}\" }} }}, got {}",
        short(result)
    );
}

/// `{ error: { kind: "unavailable" } }`.
pub fn assert_unavailable(result: &Value, context: &str) {
    let answer = &result["answer"];
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("unavailable"),
        "{context}: expected {{ error: {{ kind: \"unavailable\" }} }}, got {}",
        short(result)
    );
}

/// The call was taken: it is not a `limit` and not `unavailable` from the
/// bounds, and it was answered as its operation answers: `{ ok }` for a
/// query or `whoami`, a refusal `unknown-operation` for `nonesuch`.
pub fn assert_taken(result: &Value, op: &str, context: &str) {
    let answer = &result["answer"];
    if op == "nonesuch" {
        assert_eq!(
            answer["refused"]["refusal"]["reason"].as_str(),
            Some("unknown-operation"),
            "{context}: expected the call taken and then refused `unknown-operation`, got {}",
            short(result)
        );
    } else {
        assert!(
            answer["ok"].is_object(),
            "{context}: expected the call taken and answered `{{ ok: ... }}`, got {}",
            short(result)
        );
    }
}

/// Fails unless the fixture measured this call's arguments as `bytes`.
pub fn assert_measured(result: &Value, bytes: usize, context: &str) {
    assert_eq!(
        result["bytes"].as_u64(),
        u64::try_from(bytes).ok(),
        "{context}: the fixture was to make arguments of exactly {bytes} UTF-8 bytes, it measured {}",
        short(result)
    );
}

/// `at` of a result, in ms since `started`.
pub fn at(result: &Value, started: &Value) -> i64 {
    result["at"].as_i64().unwrap_or(i64::MAX) - started.as_i64().unwrap_or(0)
}
