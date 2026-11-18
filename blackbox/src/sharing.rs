//! What the tests of Contributors and sharing share (CONTRACT.md,
//! "Contributors and sharing"): the entries of the two lists and the
//! bodies that change them, an Application made the way a test of who can
//! open it needs (not shared with anyone), the `403` for someone it is not
//! shared with, as a page and as plain text, and the tool's four commands.

use serde_json::{Value, json};

use crate::api::{Api, assert_recent_rfc3339, version_body};
use crate::http::{Response, User};
use crate::tool::{Machine, Run};
use crate::vm::{Vm, fixture_bundle};

/// The fixture the tests of opening use: it prints a line for every request
/// its code is given, so a request that ran its code leaves a `printed`
/// record carrying the request's id (and a log entry).
pub const TRACED: &str = "honest.js";

/// The most entries of a request that changes a list.
pub const MAX_ENTRIES_A_REQUEST: usize = 100;
/// The most Contributors an Application has.
pub const MAX_CONTRIBUTORS: usize = 100;
/// The most entries of whom an Application is shared with.
pub const MAX_SHARED_WITH: usize = 1000;

/// `{ "user": <id> }`.
pub fn user(id: &str) -> Value {
    json!({ "user": id })
}

/// `{ "group": <Group> }`.
pub fn group(name: &str) -> Value {
    json!({ "group": name })
}

/// `{ "everyone": true }`.
pub fn everyone() -> Value {
    json!({ "everyone": true })
}

/// `{ "email": <address> }`.
pub fn email(address: &str) -> Value {
    json!({ "email": address })
}

/// `{ "add": [ ... ] }`.
pub fn add(entries: &[Value]) -> Value {
    json!({ "add": entries })
}

/// `{ "remove": [ ... ] }`.
pub fn remove(entries: &[Value]) -> Value {
    json!({ "remove": entries })
}

/// `{ "add": [ ... ], "remove": [ ... ] }`.
pub fn add_remove(added: &[Value], removed: &[Value]) -> Value {
    json!({ "add": added, "remove": removed })
}

/// What an entry names, as one string: `user:<id>`, `group:<Group>` or
/// `everyone`; `None` when it is none of the three (or more than one).
pub fn entry_key(entry: &Value) -> Option<String> {
    let user = entry.get("user").and_then(Value::as_str);
    let group = entry.get("group").and_then(Value::as_str);
    let everyone = entry.get("everyone").and_then(Value::as_bool) == Some(true);
    match (user, group, everyone) {
        (Some(id), None, false) => Some(format!("user:{id}")),
        (None, Some(name), false) => Some(format!("group:{name}")),
        (None, None, true) => Some("everyone".to_owned()),
        _ => None,
    }
}

/// What each entry of a list names (see [`entry_key`]), in order; fails
/// unless `list` is an array of entries.
pub fn keys(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap_or_else(|| panic!("expected a list of entries, got {list}"))
        .iter()
        .map(|e| entry_key(e).unwrap_or_else(|| panic!("expected an entry, got {e} in {list}")))
        .collect()
}

/// The keys as [`keys`] gives them, from short names: `["user:bob",
/// "everyone"]`.
pub fn expected(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| (*k).to_owned()).collect()
}

/// The entry of `list` that names `key`; fails unless there is exactly one.
pub fn entry_named<'a>(list: &'a Value, key: &str) -> &'a Value {
    let found: Vec<&Value> = list
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of entries, got {list}"))
        .iter()
        .filter(|e| entry_key(e).as_deref() == Some(key))
        .collect();
    match found.as_slice() {
        [one] => one,
        _ => panic!("expected exactly one entry {key} in {list}"),
    }
}

/// Fails unless this entry of an Application's lists says it was added by
/// `by`, at a time within five minutes of now, and, for a person, has
/// `name` and `email` as given.
pub fn assert_entry(
    entry: &Value,
    by: &str,
    name: Option<&str>,
    email: Option<&str>,
    context: &str,
) {
    assert_eq!(
        entry["added_by"].as_str(),
        Some(by),
        "{context}: expected `added_by` {by:?} in {entry}"
    );
    assert_recent_rfc3339(&entry["added_at"], 300, &format!("{context}: added_at"));
    if entry.get("user").is_some() {
        assert_eq!(
            entry.get("name"),
            Some(&name.map_or(Value::Null, |n| json!(n))),
            "{context}: expected the person's `name` {name:?} in {entry}"
        );
        assert_eq!(
            entry.get("email"),
            Some(&email.map_or(Value::Null, |e| json!(e))),
            "{context}: expected the person's `email` {email:?} in {entry}"
        );
    }
}

/// Creates `name` as `creator` and sends `fixtures/<file>` as its first
/// Version, sharing it with nobody, failing the test with the program's log
/// unless both are `201`.
pub fn create_unshared(vm: &Vm, creator: &User, name: &str, file: &str) {
    let api = vm.api_as(creator);
    let created = api.create_application(name);
    if created.status != 201 {
        vm.fail(&format!(
            "expected `201` creating {name:?} as {:?}, got\n{}",
            creator.id,
            created.describe()
        ));
    }
    let sent = api.send_version(name, &version_body(&fixture_bundle(file)));
    if sent.status != 201 {
        vm.fail(&format!(
            "expected `201` sending {file} as the first Version of {name:?}, got\n{}",
            sent.describe()
        ));
    }
}

/// Changes whom `application` is shared with as `api`'s User, failing the
/// test with the program's log unless it is `200`; returns the Application
/// as it is after.
pub fn share_ok(api: &Api<'_>, vm: &Vm, application: &str, body: &Value) -> Value {
    let answer = api.change_sharing(application, body);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` changing whom {application:?} is shared with ({body}), got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// Changes the Contributors of `application` as `api`'s User, failing the
/// test with the program's log unless it is `200`; returns the Application
/// as it is after.
pub fn contributors_ok(api: &Api<'_>, vm: &Vm, application: &str, body: &Value) -> Value {
    let answer = api.change_contributors(application, body);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` changing the Contributors of {application:?} ({body}), got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// A `GET /hello` of the Application as this User (not a page).
pub fn open(vm: &Vm, user: &User, application: &str) -> Response {
    vm.request(application, "/hello")
        .as_user(user)
        .header("accept", "application/json")
        .send()
}

/// Fails unless this User opens the Application (`fixtures/honest.js`):
/// `200`, its code given the request as them.
pub fn assert_opens(vm: &Vm, user: &User, application: &str, context: &str) {
    let response = open(vm, user, application);
    assert!(
        response.status == 200 && response.text().contains(&format!("Hello, {}!", user.id)),
        "{context}: expected {:?} to open {application:?} (200 from its code), got\n{}",
        user.id,
        response.describe()
    );
}

/// Fails unless this User is refused at the Application's address with the
/// plain-text `403` of someone it is not shared with (see
/// [`assert_not_shared_plain`]).
pub fn assert_cannot_open(vm: &Vm, user: &User, application: &str, context: &str) {
    let response = open(vm, user, application);
    assert_not_shared_plain(vm, &response, application, &user.id, context);
}

/// Fails unless the answer is the `403` of an Application not shared with
/// the User, for a request that is not a page: `text/plain`, 1 to 200
/// bytes, naming the Application; and its `request` record has the
/// Application's name, `version` null, `status` 403, the User; and no
/// code of the Application ran.
pub fn assert_not_shared_plain(
    vm: &Vm,
    response: &Response,
    application: &str,
    user: &str,
    context: &str,
) {
    assert!(
        response.status == 403,
        "{context}: expected `403` for {user:?} at {application:?}, which is not shared with them, got\n{}",
        response.describe()
    );
    assert!(
        response
            .header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
        "{context}: expected the `403` to be `text/plain`, got\n{}",
        response.describe()
    );
    let length = response.body().len();
    assert!(
        (1..=200).contains(&length),
        "{context}: expected a body of 1 to 200 bytes, got {length}\n{}",
        response.describe()
    );
    let text = response.text().to_ascii_lowercase();
    assert!(
        text.contains("shared"),
        "{context}: expected the sentence to say the Application is not shared with them, got {:?}",
        response.text()
    );
    assert_not_shared_record(vm, response, application, user, context);
}

/// Fails unless the `request` record of this answer has the Application's
/// name, `version` null, `status` 403 and the User, and no code of the
/// Application ran for it.
pub fn assert_not_shared_record(
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
        Some(403),
        "{context}: expected `status` 403 in the `request` record, got {record}"
    );
    assert_eq!(
        record["user"].as_str(),
        Some(user),
        "{context}: expected the `request` record to name the User, got {record}"
    );
    records.assert_no_application_code_ran();
}

/// Fails unless the answer is the platform's own page for an Application
/// not shared with the person: `403`, `text/html; charset=utf-8`,
/// `cache-control: no-store`, naming the Application and whom they are
/// signed in as (`shown`: their id, and their name when known), saying to
/// ask one of its Contributors, and holding none of `hidden` (its
/// Contributors, its description).
pub fn assert_not_shared_page(
    response: &Response,
    application: &str,
    shown: &[&str],
    hidden: &[&str],
    context: &str,
) {
    assert!(
        response.status == 403,
        "{context}: expected the `403` page for {application:?}, got\n{}",
        response.describe()
    );
    let content_type = response
        .header("content-type")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(' ', "");
    assert_eq!(
        content_type,
        "text/html;charset=utf-8",
        "{context}: expected `text/html; charset=utf-8`, got\n{}",
        response.describe()
    );
    assert!(
        response
            .header("cache-control")
            .is_some_and(|c| c.to_ascii_lowercase().contains("no-store")),
        "{context}: expected `cache-control: no-store`, got\n{}",
        response.describe()
    );
    let html = response.text();
    let lower = html.to_ascii_lowercase();
    assert!(
        html.contains(application),
        "{context}: expected the page to name the Application {application:?}\n{html}"
    );
    assert!(
        lower.contains("shared"),
        "{context}: expected the page to say the Application is not shared with them\n{html}"
    );
    assert!(
        lower.contains("contributor"),
        "{context}: expected the page to say to ask one of the Application's Contributors\n{html}"
    );
    for value in shown {
        assert!(
            html.contains(value),
            "{context}: expected the page to name whom they are signed in as ({value:?})\n{html}"
        );
    }
    for value in hidden {
        assert!(
            !html.contains(value),
            "{context}: expected the page to name no Contributor and nothing else of the Application, but it holds {value:?}\n{html}"
        );
    }
}

/// `vm <args...> --json` on this machine.
pub fn tool_json(machine: &Machine, args: &[&str]) -> Run {
    let mut all = args.to_vec();
    all.push("--json");
    machine.run(&all)
}

/// Fails unless what one of the four commands that change a list printed
/// (`--json`, exit code `0`) is exactly `{ application, address, added,
/// removed, visible_to: { contributors, shared_with } }`; returns it.
pub fn assert_change_printed(run: &Run, application: &str) -> Value {
    let body = run.ok_json();
    let mut fields: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["added", "address", "application", "removed", "visible_to"],
        "expected exactly {{ application, address, added, removed, visible_to }}\n{}",
        run.describe()
    );
    assert_eq!(body["application"], application, "{}", run.describe());
    assert_eq!(
        body["address"],
        format!("{application}.{}", crate::vm::DOMAIN),
        "{}",
        run.describe()
    );
    let mut visible: Vec<&str> = body["visible_to"]
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    visible.sort_unstable();
    assert_eq!(
        visible,
        ["contributors", "shared_with"],
        "expected `visible_to` to be {{ contributors, shared_with }}\n{}",
        run.describe()
    );
    for list in [
        &body["added"],
        &body["removed"],
        &body["visible_to"]["contributors"],
        &body["visible_to"]["shared_with"],
    ] {
        assert!(
            list.as_array()
                .is_some_and(|items| items.iter().all(|e| entry_key(e).is_some())),
            "expected each list to hold entries, got {list}\n{}",
            run.describe()
        );
    }
    body
}
