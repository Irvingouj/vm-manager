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

/// Fails unless this entry has exactly the members an entry is answered
/// with wherever it is answered (CONTRACT.md, "Contributors and sharing:
/// On the platform's API"): what it names (`user`, `group` or `everyone`),
/// for a person `name` and `email` (each text or null), and `added_by`
/// (text) and `added_at` (an RFC 3339 time).
pub fn assert_entry_members(entry: &Value, context: &str) {
    let key =
        entry_key(entry).unwrap_or_else(|| panic!("{context}: expected an entry, got {entry}"));
    let mut members: Vec<&str> = entry
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    members.sort_unstable();
    let want: &[&str] = if key.starts_with("user:") {
        &["added_at", "added_by", "email", "name", "user"]
    } else if key.starts_with("group:") {
        &["added_at", "added_by", "group"]
    } else {
        &["added_at", "added_by", "everyone"]
    };
    assert_eq!(
        members, want,
        "{context}: expected exactly the members of an entry, got {entry}"
    );
    for member in ["name", "email"] {
        if let Some(value) = entry.get(member) {
            assert!(
                value.is_null() || value.is_string(),
                "{context}: expected `{member}` text or null in {entry}"
            );
        }
    }
    assert!(
        entry["added_by"].as_str().is_some_and(|b| !b.is_empty()),
        "{context}: expected `added_by` a user id in {entry}"
    );
    let at = entry["added_at"].as_str().unwrap_or_default();
    assert!(
        crate::api::parse_rfc3339(at).is_some(),
        "{context}: expected `added_at` an RFC 3339 time in {entry}"
    );
}

/// [`assert_entry_members`] for each entry of a list, which must be an
/// array.
pub fn assert_list_members(list: &Value, context: &str) {
    let items = list
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected a list of entries, got {list}"));
    for entry in items {
        assert_entry_members(entry, context);
    }
}

/// Fails unless both lists of an Application's object have entries with
/// their members (`shared_with` may be null: the asker is no Contributor).
pub fn assert_application_members(application: &Value, context: &str) {
    assert_list_members(
        &application["contributors"],
        &format!("{context}: contributors"),
    );
    if !application["shared_with"].is_null() {
        assert_list_members(
            &application["shared_with"],
            &format!("{context}: shared_with"),
        );
    }
}

/// Fails unless the answer to a change of either list (`200`) is the
/// Application with both lists' entries as [`assert_application_members`]
/// wants them, and `added` and `removed`, each a list of entries with their
/// members (CONTRACT.md, "Contributors and sharing: On the platform's API").
pub fn assert_change_answer_members(answer: &Value, context: &str) {
    assert_application_members(answer, context);
    for member in ["added", "removed"] {
        assert!(
            answer[member].is_array(),
            "{context}: expected `{member}`, a list of entries, in the answer to a change, got {answer}"
        );
        assert_list_members(&answer[member], &format!("{context}: {member}"));
    }
}

/// The Application of the answer to a change, without `added` and
/// `removed`: what a read of it answers.
pub fn as_read(answer: &Value) -> Value {
    let mut application = answer.clone();
    if let Some(object) = application.as_object_mut() {
        object.remove("added");
        object.remove("removed");
    }
    application
}

/// Fails unless the refusal has `error.entries`, the entries named by
/// `wanted` (see [`entry_key`]) in that order, each with the members of an
/// entry anywhere (CONTRACT.md: `last-contributor` has those it would have
/// removed, `too-many` those it would have added).
pub fn assert_error_entries(answer: &Response, wanted: &[&str], context: &str) {
    let body = answer.json();
    let entries = &body["error"]["entries"];
    assert!(
        entries.is_array(),
        "{context}: expected `error.entries`, a list of entries, got\n{}",
        answer.describe()
    );
    assert_list_members(entries, &format!("{context}: error.entries"));
    assert_eq!(
        keys(entries),
        expected(wanted),
        "{context}: expected `error.entries` to be the entries the refusal is about\n{}",
        answer.describe()
    );
}

/// Fails unless the refusal has `error.entry`, exactly the `{ "email": ... }`
/// as given.
pub fn assert_error_email_entry(answer: &Response, address: &str, context: &str) {
    let body = answer.json();
    assert_eq!(
        body["error"]["entry"],
        json!({ "email": address }),
        "{context}: expected `error.entry` to be the entry as given\n{}",
        answer.describe()
    );
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
    let after = answer.json();
    // `added` and `removed` are checked by the tests of the answer, not
    // here, so that a test about something else is not stopped by them.
    assert_application_members(&after, "the answer to a change of sharing");
    for member in ["added", "removed"] {
        if after.get(member).is_some_and(Value::is_array) {
            assert_list_members(&after[member], &format!("the answer to a change: {member}"));
        }
    }
    after
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
    let after = answer.json();
    // `added` and `removed` are checked by the tests of the answer, not
    // here, so that a test about something else is not stopped by them.
    assert_application_members(&after, "the answer to a change of the Contributors");
    for member in ["added", "removed"] {
        if after.get(member).is_some_and(Value::is_array) {
            assert_list_members(&after[member], &format!("the answer to a change: {member}"));
        }
    }
    after
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
    assert!(
        response.text().contains(application),
        "{context}: expected the sentence to name the Application {application:?}, got {:?}",
        response.text()
    );
    let text = response.text().to_ascii_lowercase();
    assert!(
        text.contains("not shared"),
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
    assert_own_page_headers(response, context);
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

/// Fails unless the answer has the headers of every page of the
/// platform's own: `text/html; charset=utf-8`, `cache-control: no-store`,
/// `x-content-type-options: nosniff`, `referrer-policy: same-origin` (it
/// was `no-referrer` until the ticket "The Dashboard"), and
/// a `content-security-policy` with `default-src 'none'` and
/// `frame-ancestors 'none'`.
pub fn assert_own_page_headers(response: &Response, context: &str) {
    let header = |name: &str| {
        response
            .header(name)
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    assert_eq!(
        header("content-type").replace(' ', ""),
        "text/html;charset=utf-8",
        "{context}: expected `text/html; charset=utf-8`, got\n{}",
        response.describe()
    );
    assert_eq!(
        header("referrer-policy").trim(),
        "same-origin",
        "{context}: expected `referrer-policy: same-origin`, as every page of the platform's own from the ticket \"The Dashboard\" on, got\n{}",
        response.describe()
    );
    for (name, wanted) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'"),
        ("content-security-policy", "frame-ancestors 'none'"),
    ] {
        assert!(
            header(name).contains(wanted),
            "{context}: expected `{name}` with {wanted:?}, got\n{}",
            response.describe()
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
/// removed, visible_to: { contributors, shared_with } }`, with
/// `visible_to.shared_with` a list or null (a Contributor who has just taken
/// themselves off); returns it.
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
    let shared_with = &body["visible_to"]["shared_with"];
    let lists: Vec<&Value> = [
        Some(&body["added"]),
        Some(&body["removed"]),
        Some(&body["visible_to"]["contributors"]),
        (!shared_with.is_null()).then_some(shared_with),
    ]
    .into_iter()
    .flatten()
    .collect();
    for list in lists {
        assert_list_members(list, &run.describe());
        assert!(
            list.as_array()
                .is_some_and(|items| items.iter().all(|e| entry_key(e).is_some())),
            "expected each list to hold entries, got {list}\n{}",
            run.describe()
        );
    }
    body
}

/// Shares an Application `vm deploy` just made with everyone, as its first
/// Contributor (the one `visible_to.contributors` names), failing the test
/// unless it is `200`: for tests of a deployed Application that are not
/// about who can open it.
pub fn share_with_everyone_as_deployer(vm: &Vm, deployed: &crate::deploy::Deployed) {
    let deployer = deployed
        .contributors
        .first()
        .and_then(|c| c["user"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "expected the deploy's visible_to to name its Contributor\n{}",
                deployed.run.describe()
            )
        });
    vm.api_as(&User::new(deployer))
        .share_with_everyone(&deployed.application);
}
