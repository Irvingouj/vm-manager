//! What the tests of "Reading the Change history" share (CONTRACT.md,
//! "Reading the Change history"; ticket "Read a row's Change history through
//! the SDK"):
//!
//! - the fixture `fixtures/sdk/chronicle` installed beside the shared
//!   database it declares ([`chronicle`]), and the second Application
//!   `fixtures/sdk/chronicle-twin` installed in the same program
//!   ([`install_twin`]);
//! - a `history(...)` call made through the fixture's `/history` route with
//!   the key and the options given as JSON ([`HistoryCall`]), and the changes
//!   it answered ([`changes_of`]);
//! - what one change must say ([`Want`], [`assert_change`]), with values
//!   compared as the fixture described them (`describe.ts`);
//! - the one Operation log entry a call must have
//!   ([`assert_one_history_entry`]), and nothing sent when a call is refused
//!   by the SDK itself ([`assert_nothing_sent`]);
//! - the test Postgres's clock in milliseconds ([`epoch_ms`]), to put each
//!   change's `at` in the window of the test.

use serde_json::{Value, json};

use crate::http::{Response, User};
use crate::own_database::OwnDatabase;
use crate::scenario::{Expected, assert_entry, lines};
use crate::sdk::{SDK_REQUEST_TIMEOUT, Sdk, built, install};

/// The Application name of `fixtures/sdk/chronicle`.
pub const CHRONICLE: &str = "sdk-chronicle";

/// The Application name of `fixtures/sdk/chronicle-twin`.
pub const CHRONICLE_TWIN: &str = "sdk-chronicle-twin";

/// The migration of `chronicle` that changes rows of tables an earlier one
/// made.
pub const SEED_MIGRATION: &str = "0001_seed";

/// The program with `fixtures/sdk/chronicle` installed, and the shared
/// database it declares (`devops` and `ops`) registered and granted as for
/// `board`.
pub fn chronicle() -> Sdk {
    Sdk::with_shared("chronicle")
}

/// Installs `fixtures/sdk/chronicle-twin` in the program of `sdk`, and
/// returns its own database.
pub fn install_twin(sdk: &Sdk) -> OwnDatabase {
    install(sdk.vm(), &built("chronicle-twin"))
}

/// A `GET` of `path` on `application` as `user`.
pub fn get_on(sdk: &Sdk, application: &str, user: &User, path: &str) -> Response {
    sdk.vm()
        .request(application, path)
        .as_user(user)
        .timeout(SDK_REQUEST_TIMEOUT)
        .send()
}

/// As [`get_on`], failing the test unless the answer is `200`; returns the
/// response and its JSON body.
pub fn get_ok_on(sdk: &Sdk, application: &str, user: &User, path: &str) -> (Response, Value) {
    let response = get_on(sdk, application, user, path);
    if response.status != 200 {
        sdk.vm().fail(&format!(
            "expected `200` with JSON from {application:?} for GET {path} as {:?}, got\n{}",
            user.id,
            response.describe()
        ));
    }
    let body = response.json();
    (response, body)
}

/// A second User, without any Group.
pub fn bob() -> User {
    User::new("bob")
}

/// A third User, who changes nothing and reads.
pub fn carol() -> User {
    User::new("carol")
}

/// `text` percent-encoded for a query string: everything but the
/// unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// One `history(table, key, options)` call through `/history` of
/// `chronicle`.
#[derive(Clone, Debug)]
pub struct HistoryCall<'a> {
    /// `user` or `application`: which handle of the own database.
    pub actor: &'a str,
    /// `tasks`, `lines`, `events`, `archive`, `reshaped` or `recast`.
    pub table: &'a str,
    /// The key, as JSON; `None`: not passed.
    pub key: Option<Value>,
    /// Pass `undefined` as the key explicitly.
    pub undefined_key: bool,
    /// The options, as JSON; `None`: not passed.
    pub options: Option<Value>,
}

impl<'a> HistoryCall<'a> {
    /// `history(table)` as the User.
    pub fn of(table: &'a str) -> Self {
        Self {
            actor: "user",
            table,
            key: None,
            undefined_key: false,
            options: None,
        }
    }

    #[must_use]
    pub fn key(mut self, key: Value) -> Self {
        self.key = Some(key);
        self
    }

    #[must_use]
    pub fn undefined_key(mut self) -> Self {
        self.undefined_key = true;
        self
    }

    #[must_use]
    pub fn options(mut self, options: Value) -> Self {
        self.options = Some(options);
        self
    }

    #[must_use]
    pub fn as_actor(mut self, actor: &'a str) -> Self {
        self.actor = actor;
        self
    }

    /// The path of `/history` that makes this call.
    pub fn path(&self) -> String {
        let mut path = format!("/history?as={}&table={}", self.actor, self.table);
        if let Some(key) = &self.key {
            path.push_str(&format!("&key={}", encode(&key.to_string())));
        }
        if self.undefined_key {
            path.push_str("&undefinedKey=1");
        }
        if let Some(options) = &self.options {
            path.push_str(&format!("&options={}", encode(&options.to_string())));
        }
        path
    }

    /// Makes the call as `user`; the response and the fixture's answer.
    pub fn send(&self, sdk: &Sdk, user: &User) -> (Response, Value) {
        sdk.get_ok(user, &self.path())
    }

    /// Makes the call as `user` and fails the test unless it answered
    /// changes; returns the response and the changes.
    pub fn changes(&self, sdk: &Sdk, user: &User, context: &str) -> (Response, Vec<Value>) {
        let (response, body) = self.send(sdk, user);
        (
            response,
            changes_of(&body, &format!("{context} ({self:?})")),
        )
    }
}

/// The changes a `/history` answer holds; fails the test with what was
/// caught when the call threw.
pub fn changes_of(body: &Value, context: &str) -> Vec<Value> {
    match body["changes"].as_array() {
        Some(changes) => {
            assert_eq!(
                body["isArray"], true,
                "{context}: history resolves to an array"
            );
            changes.clone()
        }
        None => panic!(
            "{context}: expected history(...) to resolve to the changes, but it answered {body}"
        ),
    }
}

/// The `caught` of a `/history` (or another route's) answer; fails the test
/// when the call did not throw.
pub fn caught_of<'v>(body: &'v Value, context: &str) -> &'v Value {
    let caught = &body["caught"];
    assert!(
        caught.is_object(),
        "{context}: expected the call to throw, but it answered {body}"
    );
    caught
}

/// The fixture's description of a string.
fn described_string(value: &Value) -> Option<&str> {
    (value["type"] == "string")
        .then(|| value["value"].as_str())
        .flatten()
}

/// What a change must say. `old` and `new` are descriptions (`describe.ts`),
/// or [`crate::sdk::d::null`].
#[derive(Clone, Debug)]
pub struct Want<'a> {
    pub kind: &'a str,
    pub old: Value,
    pub new: Value,
    /// `None`: `changed` is null.
    pub changed: Option<Vec<&'a str>>,
    pub user: &'a str,
    pub application: &'a str,
    pub actor: &'a str,
    /// `None`: `request` is null.
    pub request: Option<String>,
    /// `None`: `migration` is null.
    pub migration: Option<&'a str>,
}

impl<'a> Want<'a> {
    fn base(kind: &'a str, user: &'a str, actor: &'a str, request: &str) -> Self {
        Self {
            kind,
            old: crate::sdk::d::null(),
            new: crate::sdk::d::null(),
            changed: None,
            user,
            application: CHRONICLE,
            actor,
            request: Some(request.to_owned()),
            migration: None,
        }
    }

    /// An insert of the row `new`, by `user` acting as `actor` in `request`.
    pub fn insert(user: &'a str, actor: &'a str, request: &str, new: &Value) -> Self {
        Self {
            new: new.clone(),
            ..Self::base("insert", user, actor, request)
        }
    }

    /// An update from `old` to `new`, these properties differing.
    pub fn update(
        user: &'a str,
        actor: &'a str,
        request: &str,
        old: &Value,
        new: &Value,
        changed: &[&'a str],
    ) -> Self {
        Self {
            old: old.clone(),
            new: new.clone(),
            changed: Some(changed.to_vec()),
            ..Self::base("update", user, actor, request)
        }
    }

    /// A delete of the row `old`.
    pub fn delete(user: &'a str, actor: &'a str, request: &str, old: &Value) -> Self {
        Self {
            old: old.clone(),
            ..Self::base("delete", user, actor, request)
        }
    }

    /// The same change made by `migration`, of a Version `contributor`
    /// sent: actor `migration`, no request.
    #[must_use]
    pub fn by_migration(mut self, contributor: &'a str, migration: &'a str) -> Self {
        self.user = contributor;
        self.actor = "migration";
        self.request = None;
        self.migration = Some(migration);
        self
    }

    #[must_use]
    pub fn in_application(mut self, application: &'a str) -> Self {
        self.application = application;
        self
    }
}

/// The description of a string, or of null.
fn string_or_null(value: Option<&str>) -> Value {
    value.map_or_else(crate::sdk::d::null, crate::sdk::d::string)
}

/// Fails unless `change` (as `/history` described it) says what `want`
/// says, property by property as CONTRACT.md's table "A change" gives them:
/// `id` a string of decimal digits, `at` a `Date`, and the rest equal.
pub fn assert_change(change: &Value, want: &Want<'_>, context: &str) {
    use crate::sdk::d;
    let mut wrong = Vec::new();
    let mut check = |property: &str, expected: Value| {
        if change.get(property) != Some(&expected) {
            wrong.push(format!(
                "  `{property}`: expected {expected}, got {}",
                change
                    .get(property)
                    .map_or("(missing)".to_owned(), Value::to_string)
            ));
        }
    };
    check("kind", d::string(want.kind));
    check("old", want.old.clone());
    check("new", want.new.clone());
    check(
        "changed",
        want.changed.as_ref().map_or_else(d::null, |names| {
            d::array(&names.iter().map(|n| d::string(n)).collect::<Vec<_>>())
        }),
    );
    check("userId", d::string(want.user));
    check("application", d::string(want.application));
    check("actor", d::string(want.actor));
    check("request", string_or_null(want.request.as_deref()));
    check("migration", string_or_null(want.migration));
    if !described_string(&change["id"])
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        wrong.push(format!(
            "  `id`: expected a string of decimal digits, got {}",
            change["id"]
        ));
    }
    if change["at"]["type"] != "date" || !change["atMs"].is_i64() {
        wrong.push(format!("  `at`: expected a Date, got {}", change["at"]));
    }
    assert!(
        wrong.is_empty(),
        "{context}: the change is not the one expected:\n{}\nthe change: {change}",
        wrong.join("\n")
    );
}

/// Fails unless `changes` are exactly `wants`, in this order (newest
/// first).
pub fn assert_changes(changes: &[Value], wants: &[Want<'_>], context: &str) {
    assert_eq!(
        changes.len(),
        wants.len(),
        "{context}: expected {} changes, newest first, got {}:\n{}",
        wants.len(),
        changes.len(),
        lines(changes)
    );
    for (index, (change, want)) in changes.iter().zip(wants).enumerate() {
        assert_change(change, want, &format!("{context}, change {index}"));
    }
    assert_ids_decrease(changes, context);
}

/// A change's `id`, as a number.
pub fn id_of(change: &Value) -> u128 {
    described_string(&change["id"])
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "expected a change's `id` as decimal digits, got {}",
                change["id"]
            )
        })
}

/// A change's `id`, as the text it was answered as.
pub fn id_text(change: &Value) -> String {
    described_string(&change["id"])
        .unwrap_or_else(|| panic!("expected a change's `id` as a string, got {}", change["id"]))
        .to_owned()
}

/// Fails unless the changes' ids decrease strictly: newest first.
pub fn assert_ids_decrease(changes: &[Value], context: &str) {
    let ids: Vec<u128> = changes.iter().map(id_of).collect();
    assert!(
        ids.windows(2).all(|w| w[0] > w[1]),
        "{context}: expected the changes newest first, their ids decreasing, got {ids:?}"
    );
}

/// The test Postgres's clock, in milliseconds since the epoch.
pub fn epoch_ms(db: &OwnDatabase) -> i64 {
    db.superuser()
        .value("SELECT floor(extract(epoch from clock_timestamp()) * 1000)::bigint::text")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("cannot read the clock of the test Postgres"))
}

/// Fails unless every change's `at` is within `[from, to]` (milliseconds
/// since the epoch, one millisecond of slack each way), and the changes'
/// times do not increase (newest first).
pub fn assert_at_within(changes: &[Value], from: i64, to: i64, context: &str) {
    let times: Vec<i64> = changes
        .iter()
        .map(|c| {
            c["atMs"]
                .as_i64()
                .unwrap_or_else(|| panic!("{context}: a change without a Date `at`: {c}"))
        })
        .collect();
    for (change, at) in changes.iter().zip(&times) {
        assert!(
            (from - 1..=to + 1).contains(at),
            "{context}: expected `at` within the test's window [{from}, {to}] ms, got {at}: {change}"
        );
    }
    assert!(
        times.windows(2).all(|w| w[0] >= w[1]),
        "{context}: expected the changes' times not to increase, newest first, got {times:?}"
    );
}

/// Fails unless the request `response` answers made exactly one host
/// operation, logged as one Operation log entry: `query` on the own
/// database (`resource` null) as `actor`, for `user` in [`CHRONICLE`],
/// `allowed`, with `rows` the number of changes answered, and a `statement`
/// that reads from `vm.history` and holds none of `values`.
pub fn assert_one_history_entry(
    sdk: &Sdk,
    response: &Response,
    user: &User,
    actor: &str,
    rows: usize,
    values: &[&str],
    context: &str,
) -> Value {
    let entries = sdk.entries_of(response);
    assert_eq!(
        entries.len(),
        1,
        "{context}: expected exactly one Operation log entry for the history(...) call, got:\n{}",
        lines(&entries)
    );
    let entry = &entries[0];
    let request = response.request_id();
    let expected = Expected {
        rows: Some(u64::try_from(rows).unwrap_or(u64::MAX)),
        ..Expected::own(&request, &user.id, CHRONICLE, actor, "query")
    };
    assert_entry(entry, &expected, context);
    let statement = entry["statement"].as_str().unwrap_or_default();
    let unquoted = statement.replace('"', "").to_ascii_lowercase();
    assert!(
        unquoted.contains("vm.history"),
        "{context}: expected the entry's `statement` to read from vm.history, got {statement:?}"
    );
    for value in values {
        assert!(
            !statement.contains(value),
            "{context}: the entry's `statement` holds the value {value:?}: {statement:?}"
        );
    }
    let calls: Vec<Value> = sdk
        .vm()
        .records_of(response)
        .host_calls()
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "{context}: expected exactly one host call for the history(...) call, got:\n{}",
        lines(&calls)
    );
    assert_eq!(
        calls[0]["operation"], "query",
        "{context}: the history(...) call is a `query`: {}",
        calls[0]
    );
    entry.clone()
}

/// Fails unless the request `response` answers sent nothing to the host:
/// no `host-call` record and no Operation log entry carry its id.
pub fn assert_nothing_sent(sdk: &Sdk, response: &Response, context: &str) {
    let calls: Vec<Value> = sdk
        .vm()
        .records_of(response)
        .host_calls()
        .into_iter()
        .cloned()
        .collect();
    assert!(
        calls.is_empty(),
        "{context}: expected nothing sent to the host, but it saw:\n{}",
        lines(&calls)
    );
    let entries = sdk.entries_of(response);
    assert!(
        entries.is_empty(),
        "{context}: expected no Operation log entry, got:\n{}",
        lines(&entries)
    );
}

/// Fails unless `caught` (as `describeError` describes it) is a
/// `TypeError` whose message holds each of `names`.
pub fn assert_type_error(caught: &Value, names: &[&str], context: &str) {
    assert_eq!(
        caught["isTypeError"], true,
        "{context}: expected a TypeError, got {caught}"
    );
    let message = caught["message"].as_str().unwrap_or_default();
    for name in names {
        assert!(
            message.contains(name),
            "{context}: expected the TypeError's message to name {name:?}, got {message:?}"
        );
    }
}

/// The row a write route of `chronicle` answered (`row`, described).
pub fn row_of(body: &Value) -> Value {
    let row = &body["row"];
    assert!(
        row.is_object(),
        "expected a write route to answer the row it read back, got {body}"
    );
    row.clone()
}

/// The description of a row object as the fixture gives it, with these
/// properties.
pub fn described_row(entries: &[(&str, Value)]) -> Value {
    crate::sdk::d::object(entries)
}

/// `{ "id": <n> }`, a key of `tasks`.
pub fn task_key(id: i64) -> Value {
    json!({ "id": id })
}
