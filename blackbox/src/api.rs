//! A client for the platform's API (CONTRACT.md, "The platform's API"), as
//! a named User, and checks on what it answers.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::http::{Request, Response, User};
use crate::vm::Vm;

/// The `git_commit` the harness sends when a test does not care which.
pub const SOME_GIT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The body of a Version: this bundle, no resources in the Declared access,
/// and [`SOME_GIT_COMMIT`].
pub fn version_body(bundle: &str) -> Value {
    json!({
        "bundle": bundle,
        "declared_access": { "resources": {} },
        "git_commit": SOME_GIT_COMMIT,
    })
}

/// The body of a Version: this bundle, declaring these Resources as
/// (name, access level), all of kind `postgres`, and [`SOME_GIT_COMMIT`].
pub fn version_body_declaring(bundle: &str, declared: &[(&str, &str)]) -> Value {
    let resources: serde_json::Map<String, Value> = declared
        .iter()
        .map(|(name, access)| {
            (
                (*name).to_owned(),
                json!({ "kind": "postgres", "access": access }),
            )
        })
        .collect();
    json!({
        "bundle": bundle,
        "declared_access": { "resources": resources },
        "git_commit": SOME_GIT_COMMIT,
    })
}

/// The bundle of `fixtures/which-version.js` with its marker set: an
/// Application that answers which bundle it is (`marker`) and what the host
/// says (`whoami`, which names the Version).
pub fn marked_bundle(marker: &str) -> String {
    let template = crate::vm::fixture_bundle("which-version.js");
    assert!(
        template.contains("%MARKER%"),
        "fixtures/which-version.js has lost its %MARKER%"
    );
    template.replace("%MARKER%", marker)
}

/// The SHA-256 of `bytes`, in lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The platform's API as one User.
pub struct Api<'a> {
    vm: &'a Vm,
    user: User,
}

impl<'a> Api<'a> {
    pub fn new(vm: &'a Vm, user: &User) -> Self {
        Self {
            vm,
            user: user.clone(),
        }
    }

    /// A request to the API, signed in as this User. `GET` until changed.
    pub fn request(&self, target: &str) -> Request {
        self.vm.api_request(target).as_user(&self.user)
    }

    /// `POST /v1/applications` with `{ "name": name }`.
    pub fn create_application(&self, name: &str) -> Response {
        self.create_application_with(&json!({ "name": name }))
    }

    /// `POST /v1/applications` with this body.
    pub fn create_application_with(&self, body: &Value) -> Response {
        self.post("/v1/applications", body)
    }

    /// `POST /v1/applications/<application>/versions` with this body.
    pub fn send_version(&self, application: &str, body: &Value) -> Response {
        self.post(&format!("/v1/applications/{application}/versions"), body)
    }

    /// Sends `bundle` as a Version of `application` (see [`version_body`]).
    pub fn send_bundle(&self, application: &str, bundle: &str) -> Response {
        self.send_version(application, &version_body(bundle))
    }

    /// `GET /v1/applications/<name>`.
    pub fn get_application(&self, name: &str) -> Response {
        self.get(&format!("/v1/applications/{name}"))
    }

    /// `GET /v1/applications/<application>/versions/<number>`.
    pub fn get_version(&self, application: &str, number: u64) -> Response {
        self.get(&format!("/v1/applications/{application}/versions/{number}"))
    }

    /// `GET /v1/applications/<application>/versions`.
    pub fn list_versions(&self, application: &str) -> Response {
        self.get(&format!("/v1/applications/{application}/versions"))
    }

    /// `POST /v1/resources` with this body.
    pub fn create_resource(&self, body: &Value) -> Response {
        self.post("/v1/resources", body)
    }

    /// `GET /v1/resources/<name>`.
    pub fn get_resource(&self, name: &str) -> Response {
        self.get(&format!("/v1/resources/{name}"))
    }

    /// `GET /v1/resources`.
    pub fn list_resources(&self) -> Response {
        self.get("/v1/resources")
    }

    /// `POST /v1/resources/<resource>/grants` with `{ group, access }`.
    pub fn grant(&self, resource: &str, group: &str, access: &str) -> Response {
        self.post(
            &format!("/v1/resources/{resource}/grants"),
            &json!({ "group": group, "access": access }),
        )
    }

    /// `GET /v1/resources/<resource>/grants`.
    pub fn list_grants(&self, resource: &str) -> Response {
        self.get(&format!("/v1/resources/{resource}/grants"))
    }

    /// `GET /v1/operation-log`, with `?limit=<limit>` when given.
    pub fn operation_log(&self, limit: Option<&str>) -> Response {
        match limit {
            Some(limit) => self.get(&format!("/v1/operation-log?limit={limit}")),
            None => self.get("/v1/operation-log"),
        }
    }

    /// The Operation log's entries, which this User must be allowed to read
    /// (`200`, `{ "entries": [ ... ] }`), oldest first, at most 1000.
    pub fn operation_log_entries(&self) -> Vec<Value> {
        let answer = self.operation_log(Some("1000"));
        answer.expect_status(200);
        let body = answer.json();
        body["entries"].as_array().cloned().unwrap_or_else(|| {
            panic!("expected {{ \"entries\": [ ... ] }} from the Operation log, got {body}")
        })
    }

    /// Creates `name` and sends `bundle` as its first Version declaring
    /// these Resources as (name, access level), failing the test unless both
    /// are `201`. Returns the Version's record.
    pub fn create_declaring(&self, name: &str, bundle: &str, declared: &[(&str, &str)]) -> Value {
        self.create_application(name).expect_status(201);
        self.send_version(name, &version_body_declaring(bundle, declared))
            .expect_status(201)
            .json()
    }

    /// A `GET` of any target on the API.
    pub fn get(&self, target: &str) -> Response {
        self.request(target).send()
    }

    /// A `POST` of a JSON value to any target on the API.
    pub fn post(&self, target: &str, body: &Value) -> Response {
        self.request(target).method("POST").json(body).send()
    }

    /// A `POST` of these exact bytes, as `application/json`, to any target.
    pub fn post_raw(&self, target: &str, body: impl AsRef<[u8]>) -> Response {
        self.request(target)
            .method("POST")
            .header("content-type", "application/json")
            .body(body)
            .send()
    }

    /// Creates `name` and sends `bundle` as its first Version, failing the
    /// test unless both are `201`. Returns the Version's record.
    pub fn create_with_bundle(&self, name: &str, bundle: &str) -> Value {
        self.create_application(name).expect_status(201);
        self.send_bundle(name, bundle).expect_status(201).json()
    }

    /// The Application's record, which must be there (`200`).
    pub fn application(&self, name: &str) -> Value {
        self.get_application(name).expect_status(200).json()
    }

    /// The list of the Application's Versions, which must be there (`200`).
    pub fn versions(&self, application: &str) -> Vec<Value> {
        let answer = self.list_versions(application);
        answer.expect_status(200);
        let body = answer.json();
        body["versions"].as_array().cloned().unwrap_or_else(|| {
            panic!(
                "expected {{ \"versions\": [ ... ] }} listing the Versions of {application:?}, got {body}"
            )
        })
    }
}

/// Fails unless `response` is a refusal of the API: this status, a JSON
/// body `{ "error": { "reason": <reason>, "message": <a sentence> } }`.
pub fn assert_api_error(response: &Response, status: u16, reason: &str) {
    assert!(
        response.status == status,
        "expected `{status}` with reason {reason:?} for {}, got\n{}",
        response.request(),
        response.describe()
    );
    let body: Value = serde_json::from_slice(response.body()).unwrap_or_else(|e| {
        panic!(
            "expected the `{status}` for {} to have a JSON body {{ \"error\": ... }} ({e}), got\n{}",
            response.request(),
            response.describe()
        )
    });
    assert_eq!(
        body["error"]["reason"].as_str(),
        Some(reason),
        "expected the error's reason {reason:?} for {}, got {body}",
        response.request()
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "expected the error to carry a non-empty `message` a Coding agent can act on, for {}, got {body}",
        response.request()
    );
    assert_json_content_type(response);
}

/// Fails unless `response` was answered by the bundle `marked_bundle(marker)`
/// as the Version `version` of `application`: its body says so, its
/// `whoami` names that Version, and so does its `request` record.
pub fn assert_served_by(
    vm: &Vm,
    response: &Response,
    application: &str,
    marker: &str,
    version: u64,
) {
    assert!(
        response.status == 200,
        "expected `200` from the Version {version} of {application:?} (marker {marker:?}) for {}, got\n{}",
        response.request(),
        response.describe()
    );
    let body = response.json();
    assert_eq!(
        body["marker"].as_str(),
        Some(marker),
        "expected the request to be served by the bundle marked {marker:?} (Version {version}), got {body}"
    );
    assert_eq!(
        body["whoami"]["ok"]["version"].as_u64(),
        Some(version),
        "expected whoami to name the Version {version}, got {body}"
    );
    assert_eq!(
        body["whoami"]["ok"]["application"].as_str(),
        Some(application),
        "expected whoami to name the Application {application:?}, got {body}"
    );
    let record = vm.records_of(response).request;
    assert_eq!(
        record["version"].as_u64(),
        Some(version),
        "expected the `request` record to name the Version {version}, got {record}"
    );
    assert_eq!(
        record["application"].as_str(),
        Some(application),
        "expected the `request` record to name the Application {application:?}, got {record}"
    );
}

/// Fails unless the answer says it is JSON.
pub fn assert_json_content_type(response: &Response) {
    assert!(
        response
            .header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("application/json")),
        "expected the API's answer to {} to be `application/json`, got\n{}",
        response.request(),
        response.describe()
    );
}

/// Fails unless `value` is an RFC 3339 date and time within `slack_secs`
/// of now.
pub fn assert_recent_rfc3339(value: &Value, slack_secs: i64, context: &str) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected an RFC 3339 time as a string, got {value}"));
    let seconds = parse_rfc3339(text)
        .unwrap_or_else(|| panic!("{context}: expected an RFC 3339 time, got {text:?}"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    assert!(
        (seconds - now).abs() <= slack_secs,
        "{context}: expected a time within {slack_secs} s of now, got {text:?}"
    );
}

/// Seconds since the Unix epoch of an RFC 3339 `date-time`
/// (`2026-11-12T10:20:30.123Z`, `...+01:00`), or `None` if it is not one.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, len: usize| -> Option<i64> {
        let digits = bytes.get(from..from + len)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    };
    let at = |i: usize| bytes.get(i).copied();
    let (year, month, day) = (number(0, 4)?, number(5, 2)?, number(8, 2)?);
    let (hour, minute, second) = (number(11, 2)?, number(14, 2)?, number(17, 2)?);
    if at(4) != Some(b'-')
        || at(7) != Some(b'-')
        || !matches!(at(10), Some(b'T' | b't'))
        || at(13) != Some(b':')
        || at(16) != Some(b':')
    {
        return None;
    }
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut i = 19;
    if at(i) == Some(b'.') {
        i += 1;
        let start = i;
        while at(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let offset = match at(i)? {
        b'Z' | b'z' if i + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == bytes.len() && at(i + 3) == Some(b':') => {
            let minutes = number(i + 1, 2)? * 60 + number(i + 4, 2)?;
            if sign == b'+' { minutes } else { -minutes }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_times_are_read() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-11-12T10:00:00.123456Z"),
            Some(1_794_477_600)
        );
        assert_eq!(
            parse_rfc3339("2026-11-12T11:00:00+01:00"),
            Some(1_794_477_600)
        );
        assert_eq!(parse_rfc3339("2026-11-12 10:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-11-12T10:00:00"), None);
        assert_eq!(parse_rfc3339("yesterday"), None);
    }

    #[test]
    fn sha256_is_lower_case_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
