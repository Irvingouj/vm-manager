//! Reading the program's log records (CONTRACT.md, "What the log records
//! per request").

use serde_json::Value;

/// The `event` of a record.
pub fn event(record: &Value) -> &str {
    record["event"].as_str().unwrap_or_default()
}

/// The `request` of a record: the request id, or `None` when null or absent.
pub fn request_of(record: &Value) -> Option<&str> {
    record["request"].as_str()
}

/// The User id in a record's `user` field, or `None` when null or absent.
///
/// CONTRACT.md does not say whether `user` is the User's id or the whole
/// `{ id, groups }`; both are read here as the id.
pub fn user_of(record: &Value) -> Option<&str> {
    match &record["user"] {
        Value::String(id) => Some(id),
        Value::Object(user) => user.get("id").and_then(Value::as_str),
        _ => None,
    }
}

/// The `application` of a record, or `None` when null or absent.
pub fn application_of(record: &Value) -> Option<&str> {
    record["application"].as_str()
}

/// Whether Application code wrote this record: a `printed`, `uncaught` or
/// `host-call` record.
pub fn is_from_application_code(record: &Value) -> bool {
    matches!(event(record), "printed" | "uncaught" | "host-call")
}

/// Records, one per line, for failure messages.
pub fn describe(records: &[&Value]) -> String {
    let mut text = String::new();
    for record in records {
        text.push_str("  ");
        text.push_str(&record.to_string());
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("  (none)\n");
    }
    text
}

/// The records that carry one request's id, in log order, its `request`
/// record last among them when the program keeps CONTRACT.md's order.
#[derive(Clone, Debug)]
pub struct RequestRecords {
    pub id: String,
    /// The `request` record of this request.
    pub request: Value,
    /// Every record carrying this request's id, in log order, the `request`
    /// record included.
    pub all: Vec<Value>,
}

impl RequestRecords {
    pub fn of_event(&self, wanted: &str) -> Vec<&Value> {
        self.all.iter().filter(|r| event(r) == wanted).collect()
    }

    pub fn host_calls(&self) -> Vec<&Value> {
        self.of_event("host-call")
    }

    pub fn printed(&self) -> Vec<&Value> {
        self.of_event("printed")
    }

    pub fn uncaught(&self) -> Vec<&Value> {
        self.of_event("uncaught")
    }

    /// CONTRACT.md: "No Application code ran" for a request means no
    /// `printed`, `uncaught` or `host-call` record carries its id.
    pub fn assert_no_application_code_ran(&self) {
        let from_code: Vec<&Value> = self
            .all
            .iter()
            .filter(|r| is_from_application_code(r))
            .collect();
        assert!(
            from_code.is_empty(),
            "expected no Application code to run for request {}, but these records carry its id:\n{}",
            self.id,
            describe(&from_code)
        );
    }

    /// Fails unless every `host-call` record of this request names this
    /// User and this Application.
    pub fn assert_host_saw_only(&self, user: &str, application: &str) {
        let wrong: Vec<&Value> = self
            .host_calls()
            .into_iter()
            .filter(|r| user_of(r) != Some(user) || application_of(r) != Some(application))
            .collect();
        assert!(
            wrong.is_empty(),
            "expected every host call of request {} to be attributed to User {user:?} in Application {application:?}, but these were not:\n{}",
            self.id,
            describe(&wrong)
        );
    }
}
