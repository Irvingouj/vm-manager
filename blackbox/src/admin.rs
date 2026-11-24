//! What the tests of "What Platform administrators see and stop" share
//! (CONTRACT.md, that section): a program with the stand-in sign-in and a
//! Platform administrators' Group, the list of every Application read a
//! page at a time, disabling and enabling, what a disabled Application
//! answers (a page, or one plain sentence), usage counted and placed on
//! past days in `application_usage` as the Postgres administrator, the
//! Operation log read with its filters and paged by `before`, and the
//! fixture that holds a request in its code as long as it is asked
//! (`fixtures/waits.js`).

use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::{assert_api_error, assert_json_content_type, assert_recent_rfc3339};
use crate::http::{Response, User};
use crate::logs::encode;
use crate::records::event;
use crate::sql::Sql;
use crate::vm::{Vm, VmOptions};

/// The list of every Application.
pub const LIST: &str = "/v1/admin/applications";

/// The Operation log.
pub const OPERATION_LOG: &str = "/v1/operation-log";

/// The fixture that holds its request in its code: `GET /?ms=<n>` prints
/// `waits: began <path>`, waits, prints `waits: done`, calls `whoami` and
/// answers. Any path; `/now` answers at once (it waits 0 ms).
pub const WAITS: &str = "waits.js";

/// The members of each Application of [`LIST`], sorted.
pub const LISTED_KEYS: [&str; 13] = [
    "address",
    "contributors",
    "created_at",
    "created_by",
    "declared_access",
    "declared_more",
    "description",
    "disabled",
    "latest_version",
    "name",
    "serving_version",
    "shared_with",
    "usage",
];

/// [`LISTED_KEYS`] as a list.
pub fn listed_keys() -> Vec<&'static str> {
    LISTED_KEYS.to_vec()
}

/// How long the contract gives a counted request to show in `usage`.
pub const USAGE_PROMISED: Duration = Duration::from_secs(15);

/// How long a test waits for usage before it fails: the promise and some
/// slack for a loaded machine, so that a late count is told apart from one
/// that never comes.
pub const USAGE_WAIT: Duration = Duration::from_secs(25);

/// What the contract gives a disable to answer the requests in flight.
pub const IN_FLIGHT_PROMISED: Duration = Duration::from_secs(1);

/// What the tests allow it on a loaded machine (the gate runs many
/// programs at once): the promise and one second more.
pub const IN_FLIGHT_ALLOWED: Duration = Duration::from_secs(2);

/// The options of every program of these tests: the stand-in, the
/// administrators' Group, and `--public-scheme http` (the Dashboard's
/// address is then `http://dashboard.<domain>:<port>`).
pub fn options() -> VmOptions {
    crate::dashboard::stand_in_options()
}

/// A program with [`options`].
pub fn start() -> Vm {
    Vm::start(&options())
}

/// A Platform administrator: `it-admin`, in the administrators' Group.
pub fn administrator() -> User {
    crate::scenario::administrator()
}

/// Another Platform administrator.
pub fn other_administrator() -> User {
    User::new("it-admin-2").in_groups(&[crate::scenario::ADMINISTRATORS])
}

/// `POST /v1/applications/<name>/disable` with `{}`.
pub fn disable(vm: &Vm, user: &User, name: &str) -> Response {
    vm.api_as(user)
        .post(&format!("/v1/applications/{name}/disable"), &json!({}))
}

/// `POST /v1/applications/<name>/enable` with `{}`.
pub fn enable(vm: &Vm, user: &User, name: &str) -> Response {
    vm.api_as(user)
        .post(&format!("/v1/applications/{name}/enable"), &json!({}))
}

/// Disables `name` as [`administrator`], failing the test with the
/// program's log unless it is `200`; returns the answer.
pub fn disable_ok(vm: &Vm, name: &str) -> Value {
    let answer = disable(vm, &administrator(), name);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` disabling {name:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// Enables `name` as [`administrator`], failing unless it is `200`.
pub fn enable_ok(vm: &Vm, name: &str) -> Value {
    let answer = enable(vm, &administrator(), name);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` enabling {name:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// Fails unless `disabled` is `{ "since": <a recent RFC 3339 time>, "by":
/// by }` and nothing else; returns `since`.
pub fn assert_disabled_member(disabled: &Value, by: &str, context: &str) -> String {
    let mut keys: Vec<&str> = disabled
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["by", "since"],
        "{context}: expected `disabled` to be {{ since, by }}, got {disabled}"
    );
    assert_eq!(
        disabled["by"].as_str(),
        Some(by),
        "{context}: expected `disabled.by` to be the Platform administrator who disabled it, got {disabled}"
    );
    assert_recent_rfc3339(
        &disabled["since"],
        300,
        &format!("{context}: disabled.since"),
    );
    disabled["since"].as_str().unwrap_or_default().to_owned()
}

/// `GET /v1/admin/applications<query>` as this User; `query` is empty or
/// begins with `?`.
pub fn list(vm: &Vm, user: &User, query: &str) -> Response {
    vm.api_as(user).get(&format!("{LIST}{query}"))
}

/// One page of [`LIST`] as [`administrator`], which must be `200`, JSON,
/// exactly `{ applications, next }`, each Application with exactly the
/// members of [`listed_keys`].
pub fn list_page(vm: &Vm, query: &str) -> Value {
    let answer = list(vm, &administrator(), query);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` reading {LIST}{query} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    assert_json_content_type(&answer);
    let body = answer.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["applications", "next"],
        "expected exactly {{ applications, next }} from {LIST}{query}, got {body}"
    );
    assert!(
        body["next"].is_null() || body["next"].is_string(),
        "expected `next` a name or null, got {body}"
    );
    let applications = body["applications"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `applications` an array, got {body}"));
    for application in applications {
        let mut keys: Vec<&str> = application
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            listed_keys(),
            "expected each Application of {LIST} to have exactly the contract's members, got {application}"
        );
    }
    body
}

/// Every Application of [`LIST`], read with `?limit=<limit>` and then
/// `after=<next>` until `next` is null; also the pages, as names.
pub fn every_application(vm: &Vm, limit: usize) -> (Vec<Value>, Vec<Vec<String>>) {
    let mut all = Vec::new();
    let mut pages = Vec::new();
    let mut query = format!("?limit={limit}");
    for _ in 0..10_000 {
        let page = list_page(vm, &query);
        let applications = page["applications"].as_array().cloned().unwrap_or_default();
        pages.push(names(&applications));
        all.extend(applications);
        match page["next"].as_str() {
            Some(next) => query = format!("?limit={limit}&after={}", encode(next)),
            None => return (all, pages),
        }
    }
    panic!("the harness's own bound: more than 10,000 pages of {LIST}");
}

/// The `name` of each Application.
pub fn names(applications: &[Value]) -> Vec<String> {
    applications
        .iter()
        .map(|a| a["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The Application `name` as [`LIST`] has it; fails unless it is there
/// exactly once.
pub fn listed(vm: &Vm, name: &str) -> Value {
    let (all, _) = every_application(vm, 100);
    let found: Vec<&Value> = all.iter().filter(|a| a["name"] == name).collect();
    match found.as_slice() {
        [one] => (*one).clone(),
        _ => vm.fail(&format!(
            "expected {name:?} exactly once in {LIST}, found it {} times",
            found.len()
        )),
    }
}

/// The `usage` of `name` in [`LIST`].
pub fn usage_of(vm: &Vm, name: &str) -> Value {
    listed(vm, name)["usage"].clone()
}

/// Polls [`usage_of`] until `requests` and `users` are these, at most
/// [`USAGE_WAIT`]; returns the usage and how long it took. Fails, with the
/// last usage read, when it never comes.
pub fn wait_for_usage(vm: &Vm, name: &str, requests: u64, users: u64) -> (Value, Duration) {
    let began = Instant::now();
    loop {
        let usage = usage_of(vm, name);
        if usage["requests"].as_u64() == Some(requests) && usage["users"].as_u64() == Some(users) {
            return (usage, began.elapsed());
        }
        if began.elapsed() > USAGE_WAIT {
            vm.fail(&format!(
                "expected the usage of {name:?} to be {requests} requests by {users} Users within {USAGE_WAIT:?} (the contract promises {USAGE_PROMISED:?}), it is {usage}"
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// The harness's superuser on this program's platform database.
pub fn platform_sql(vm: &Vm) -> Sql {
    Sql::connect(vm.platform().url()).unwrap_or_else(|e| {
        panic!(
            "the harness cannot reach the platform database {:?}: {e}",
            vm.platform()
        )
    })
}

/// Writes `text` into `column` (`description`, `owner` or `contact`) of the
/// Resource `name`'s row at the platform database, as the harness's
/// superuser, as a Resource registered before the bounds on a Resource's
/// text would hold it. The program serves Resources from memory: stop it
/// and start it again on the same platform database to have it hold the row.
pub fn set_resource_text_by_hand(vm: &Vm, name: &str, column: &str, text: &str) {
    assert!(
        matches!(column, "description" | "owner" | "contact"),
        "the harness's own check: {column:?} is not a text column of a Resource"
    );
    let hex: String = text.bytes().map(|b| format!("{b:02x}")).collect();
    let platform = platform_sql(vm);
    platform.batch(&format!(
        "UPDATE resources SET {column} = convert_from(decode('{hex}', 'hex'), 'UTF8') WHERE name = '{name}'"
    ));
    assert_eq!(
        platform.value(&format!(
            "SELECT count(*)::text FROM resources WHERE name = '{name}' AND {column} = convert_from(decode('{hex}', 'hex'), 'UTF8')"
        )),
        Some("1".to_owned()),
        "the harness's own check: the row of {name:?} holds the text written by hand"
    );
}

/// The UTC day `days_ago` days before today's, as `YYYY-MM-DD`, as the
/// platform database's clock says it.
pub fn utc_day(sql: &Sql, days_ago: u32) -> String {
    sql.value(&format!(
        "SELECT ((now() AT TIME ZONE 'UTC')::date - {days_ago})::text"
    ))
    .unwrap_or_else(|| panic!("the harness's own check: no date from the platform database"))
}

/// Places `requests` counted requests of `user` on `application`, on the
/// UTC day `days_ago` days before today, in `application_usage`, as the
/// Postgres administrator (CONTRACT.md: "the tests read and write this
/// table").
pub fn place_usage(vm: &Vm, application: &str, days_ago: u32, user: &str, requests: u64) {
    let sql = platform_sql(vm);
    let day = utc_day(&sql, days_ago);
    sql.batch(&format!(
        "INSERT INTO application_usage (application, day, user_id, requests) VALUES ({}, DATE {}, {}, {requests})",
        literal(application),
        literal(&day),
        literal(user)
    ));
}

/// The rows of `application_usage` for `application` as (day, user,
/// requests), by day and user.
pub fn usage_rows(vm: &Vm, application: &str) -> Vec<(String, String, u64)> {
    platform_sql(vm)
        .batch(&format!(
            "SELECT day::text, user_id, requests::text FROM application_usage WHERE application = {} ORDER BY 1, 2",
            literal(application)
        ))
        .into_iter()
        .map(|row| {
            let mut cells = row.into_iter().map(Option::unwrap_or_default);
            (
                cells.next().unwrap_or_default(),
                cells.next().unwrap_or_default(),
                cells.next().unwrap_or_default().parse().unwrap_or_default(),
            )
        })
        .collect()
}

/// A SQL string literal.
pub fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The one sentence a disabled Application answers to a request that is
/// not a page.
pub fn disabled_sentence(application: &str) -> String {
    format!("The Application {application} has been disabled by a Platform administrator.")
}

/// Fails unless the answer is a disabled Application's to a request that
/// is not a page: `503`, `text/plain`, 1 to 200 bytes, exactly the one
/// sentence and a line end, no `retry-after`.
pub fn assert_disabled_plain(response: &Response, application: &str, context: &str) {
    assert!(
        response.status == 503,
        "{context}: expected the `503` of a disabled Application, got\n{}",
        response.describe()
    );
    assert!(
        response
            .header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
        "{context}: expected `text/plain`, got\n{}",
        response.describe()
    );
    let text = response.text();
    assert!(
        (1..=200).contains(&response.body().len()),
        "{context}: expected 1 to 200 bytes, got {}\n{}",
        response.body().len(),
        response.describe()
    );
    assert!(
        text.ends_with('\n')
            && text.trim_end_matches(['\r', '\n']) == disabled_sentence(application),
        "{context}: expected exactly {:?} and a line end, got {text:?}",
        disabled_sentence(application)
    );
    assert_no_retry_after(response, context);
}

/// Fails if the answer has `retry-after`.
pub fn assert_no_retry_after(response: &Response, context: &str) {
    assert!(
        response.header("retry-after").is_none(),
        "{context}: a disabled Application's `503` does not say to try again shortly: expected no `retry-after`, got\n{}",
        response.describe()
    );
}

/// Fails unless the `request` record of this answer names the Application,
/// `version` null, `status` 503 and the User, and no code of the
/// Application ran for it.
pub fn assert_disabled_record(
    vm: &Vm,
    response: &Response,
    application: &str,
    user: &str,
    context: &str,
) {
    let records = vm.records_of(response);
    let record = &records.request;
    assert_eq!(
        record["application"].as_str(),
        Some(application),
        "{context}: expected the `request` record to name the Application, got {record}"
    );
    assert!(
        record.get("version").is_some_and(Value::is_null),
        "{context}: expected `version` null in the `request` record, got {record}"
    );
    assert_eq!(
        record["status"].as_u64(),
        Some(503),
        "{context}: expected `status` 503 in the `request` record, got {record}"
    );
    assert_eq!(
        crate::records::user_of(record),
        Some(user),
        "{context}: expected the `request` record to name the User, got {record}"
    );
    records.assert_no_application_code_ran();
}

/// Fails unless the answer is a disabled Application's page: `503`, the
/// headers of every page of the platform's own, an `<h1>` saying it is
/// disabled, the Application named, a Platform administrator and IT
/// named, whom the person is signed in as (`shown`), an element
/// `data-vm-disabled="<application>"`, none of `hidden`, no
/// `retry-after`.
pub fn assert_disabled_page(
    response: &Response,
    application: &str,
    shown: &[&str],
    hidden: &[&str],
    context: &str,
) {
    assert!(
        response.status == 503,
        "{context}: expected the `503` page of a disabled Application, got\n{}",
        response.describe()
    );
    crate::sharing::assert_own_page_headers(response, context);
    assert_no_retry_after(response, context);
    let html = response.text();
    let document = crate::page::Document::parse(&html);
    let root = document.root();
    let h1: Vec<String> = root
        .named("h1")
        .iter()
        .map(crate::page::Element::text)
        .collect();
    assert!(
        h1.len() == 1 && h1[0].to_lowercase().contains("disabled"),
        "{context}: expected one `<h1>` saying the Application is disabled, got {h1:?}\n{html}"
    );
    let marked = root.with_attr_value("data-vm-disabled", application);
    assert_eq!(
        marked.len(),
        1,
        "{context}: expected one element with data-vm-disabled=\"{application}\"\n{html}"
    );
    let text = root.text();
    let lower = text.to_lowercase();
    assert!(
        text.contains(application),
        "{context}: expected the page to name the Application\n{html}"
    );
    assert!(
        lower.contains("platform administrator"),
        "{context}: expected the page to say that a Platform administrator stopped it\n{html}"
    );
    assert!(
        text.contains("IT"),
        "{context}: expected the page to say to ask the company's IT\n{html}"
    );
    for value in shown {
        assert!(
            text.contains(value),
            "{context}: expected the page to say whom the person is signed in as ({value:?})\n{html}"
        );
    }
    for value in hidden {
        assert!(
            !html.contains(value),
            "{context}: expected the page to hold nothing of {value:?} (no administrator, no since, no by)\n{html}"
        );
    }
}

/// `GET /v1/operation-log<query>` as this User; `query` is empty or begins
/// with `?`.
pub fn operation_log(vm: &Vm, user: &User, query: &str) -> Response {
    vm.api_as(user).get(&format!("{OPERATION_LOG}{query}"))
}

/// One page of the Operation log as [`administrator`], which must be
/// `200`, exactly `{ entries, earlier }`, `earlier` an id or null.
pub fn log_page(vm: &Vm, query: &str) -> Value {
    let answer = operation_log(vm, &administrator(), query);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` reading the Operation log with {query:?}, got status {}\n{}",
            answer.status,
            answer.text().chars().take(2000).collect::<String>()
        ));
    }
    let body = answer.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["earlier", "entries"],
        "expected exactly {{ entries, earlier }} reading the Operation log with {query:?}"
    );
    assert!(
        body["earlier"].is_null() || body["earlier"].as_u64().is_some(),
        "expected `earlier` an id or null with {query:?}, got {}",
        body["earlier"]
    );
    body
}

/// The `id` of each entry.
pub fn ids(entries: &[Value]) -> Vec<u64> {
    entries
        .iter()
        .map(|e| {
            e["id"]
                .as_u64()
                .unwrap_or_else(|| panic!("expected each entry's `id` a number, got {e}"))
        })
        .collect()
}

/// Every entry matching `filters` (`a=b&c=d`, or empty), read with
/// `limit` and then `before=<earlier>` until `earlier` is null; the pages
/// as ids, newest page first. Each page must be oldest first, and
/// `earlier` the id of its first entry.
pub fn every_entry(vm: &Vm, filters: &str, limit: usize) -> (Vec<Value>, Vec<Vec<u64>>) {
    let join = if filters.is_empty() { "" } else { "&" };
    let mut query = format!("?{filters}{join}limit={limit}");
    let mut all: Vec<Value> = Vec::new();
    let mut pages = Vec::new();
    for _ in 0..100_000 {
        let page = log_page(vm, &query);
        let entries = page["entries"].as_array().cloned().unwrap_or_default();
        let page_ids = ids(&entries);
        assert!(
            page_ids.windows(2).all(|w| w[0] < w[1]),
            "expected each page oldest first with {query:?}, got ids {page_ids:?}"
        );
        match page["earlier"].as_u64() {
            Some(earlier) => {
                assert_eq!(
                    Some(&earlier),
                    page_ids.first(),
                    "expected `earlier` to be the id of the first entry answered with {query:?}"
                );
                pages.push(page_ids);
                let mut older = entries;
                older.extend(all);
                all = older;
                query = format!("?{filters}{join}limit={limit}&before={earlier}");
            }
            None => {
                pages.push(page_ids);
                let mut older = entries;
                older.extend(all);
                return (older, pages);
            }
        }
    }
    panic!("the harness's own bound: more than 100,000 pages of the Operation log");
}

/// Microseconds since the epoch of an RFC 3339 time with up to six digits
/// of fraction.
pub fn micros(text: &str) -> Option<i128> {
    let seconds = crate::api::parse_rfc3339(text)?;
    let fraction = text
        .get(19..)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .unwrap_or_default();
    if fraction.len() > 6 {
        return None;
    }
    let padded = format!("{fraction:0<6}");
    Some(i128::from(seconds) * 1_000_000 + padded.parse::<i128>().ok()?)
}

/// The `event` records of the program's log with this name.
pub fn logged(vm: &Vm, wanted: &str) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| event(r) == wanted)
        .collect()
}

/// How many `request` records the program's log has: what tells whether a
/// command asked the installation anything.
pub fn request_records(vm: &Vm) -> usize {
    logged(vm, "request").len()
}

/// Waits until the program's log has a `printed` record of request `id`
/// whose message holds `text`, at most `within`.
pub fn wait_for_printed(vm: &Vm, id: &str, text: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let found = vm.records().into_iter().any(|r| {
            event(&r) == "printed"
                && r["request"].as_str() == Some(id)
                && r["message"].as_str().is_some_and(|m| m.contains(text))
        });
        if found {
            return;
        }
        if Instant::now() >= deadline {
            vm.fail(&format!(
                "expected a `printed` record of request {id:?} holding {text:?} within {within:?}"
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Fails unless the answer is the API's refusal `403`
/// `not-an-administrator`.
pub fn assert_not_an_administrator(response: &Response) {
    assert_api_error(response, 403, "not-an-administrator");
}
