//! Changing a Resource's description, owner and contact (CONTRACT.md,
//! "Resources and Grants in full: Changing a Resource's description, owner
//! and contact"): `PATCH /v1/resources/<name>` with each field and several,
//! at least one required, other members ignored, the bounds of
//! registration, `404`; a `no-grant` refusal names the new contact at once;
//! it works on a Resource that is not served and on a Storage Resource; the
//! log record `resource-changed`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_api_error;
use blackbox::resources::{admin_resource, call, not_served, patch_resource};
use blackbox::scenario::{CONTACT, RESOURCE, Scenario, administrator, sql_on};
use blackbox::sealed::DEVOPS;
use blackbox::storage::{MARKETING, Storage};
use blackbox::{Response, User};
use serde_json::{Value, json};

fn admin_view(s: &Scenario, name: &str) -> Value {
    let answer = admin_resource(&s.admin(), name);
    answer.expect_status(200);
    answer.json()
}

fn changed_records(s: &Scenario, expected: usize) -> Vec<Value> {
    blackbox::resources::records_waited(&s.vm, "resource-changed", expected)
}

fn sorted_fields(record: &Value) -> Vec<String> {
    let mut fields: Vec<String> = record["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_owned())
        .collect();
    fields.sort();
    fields
}

#[test]
fn each_field_and_several_can_be_changed_and_the_answer_is_the_administrators_view() {
    let s = Scenario::ready();
    let api = s.admin();
    let before = admin_view(&s, RESOURCE);
    for (field, value) in [
        ("description", "The new description"),
        ("owner", "The Platform team"),
        ("contact", "Sam Ops, #ops on chat"),
    ] {
        let answer = patch_resource(&api, RESOURCE, &json!({ field: value }));
        answer.expect_status(200);
        let view = answer.json();
        assert_eq!(view[field], value, "{}", answer.describe());
        assert_eq!(
            view,
            admin_view(&s, RESOURCE),
            "the answer is the administrators' view"
        );
        // Nothing else changed.
        for other in [
            "name",
            "kind",
            "created_by",
            "created_at",
            "grants",
            "logins",
            "usable",
        ] {
            assert_eq!(view[other], before[other], "{field} changed `{other}`");
        }
    }
    let answer = patch_resource(
        &api,
        RESOURCE,
        &json!({ "description": "d2", "owner": "o2", "contact": "c2" }),
    );
    answer.expect_status(200);
    let view = answer.json();
    assert_eq!(
        (
            view["description"].as_str(),
            view["owner"].as_str(),
            view["contact"].as_str()
        ),
        (Some("d2"), Some("o2"), Some("c2"))
    );
    // Every User sees all three at once, in the list and by name.
    let rita = s.vm.api_as(&User::new("rita"));
    let one = rita.get_resource(RESOURCE).json();
    assert_eq!(
        (
            one["description"].as_str(),
            one["owner"].as_str(),
            one["contact"].as_str()
        ),
        (Some("d2"), Some("o2"), Some("c2"))
    );
    let listed = rita.list_resources().json();
    let found = listed["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == RESOURCE)
        .unwrap()
        .clone();
    assert_eq!(found["contact"], "c2");
    // Asking again is safe; a line end is allowed in a description.
    patch_resource(&api, RESOURCE, &json!({ "description": "d2" })).expect_status(200);
    patch_resource(&api, RESOURCE, &json!({ "description": "two\nlines" })).expect_status(200);
    // The records: one for each change, naming the fields and never values.
    let records = changed_records(&s, 6);
    assert_eq!(records.len(), 6, "{records:?}");
    assert_eq!(sorted_fields(&records[0]), vec!["description"]);
    assert_eq!(sorted_fields(&records[1]), vec!["owner"]);
    assert_eq!(sorted_fields(&records[2]), vec!["contact"]);
    assert_eq!(
        sorted_fields(&records[3]),
        vec!["contact", "description", "owner"]
    );
    for record in &records {
        assert_eq!(record["resource"], RESOURCE);
        assert_eq!(record["by"], administrator().id);
        let text = record.to_string();
        for value in ["The new description", "Platform team", "c2", "d2", "o2"] {
            assert!(!text.contains(value), "no value in the record: {text}");
        }
    }
}

#[test]
fn at_least_one_of_the_three_is_needed_and_the_other_members_are_ignored() {
    let s = Scenario::ready();
    let api = s.admin();
    let before = admin_view(&s, RESOURCE);
    for (what, body) in [
        ("an empty object", json!({})),
        ("only a name", json!({ "name": "renamed" })),
        ("only a kind", json!({ "kind": "storage" })),
        (
            "only members it does not have",
            json!({ "connection": {}, "grants": [] }),
        ),
        ("an array", json!(["contact"])),
        ("text", json!("contact")),
    ] {
        let answer = patch_resource(&api, RESOURCE, &body);
        assert_api_error(&answer, 400, "malformed");
        let _ = what;
    }
    let answer = patch_resource(
        &api,
        RESOURCE,
        &json!({
            "contact": "Only this", "name": "renamed", "kind": "storage",
            "usable": false, "logins": { "read": false }, "created_by": "someone",
        }),
    );
    answer.expect_status(200);
    let view = admin_view(&s, RESOURCE);
    assert_eq!(view["contact"], "Only this");
    assert_eq!(view["name"], RESOURCE);
    assert_eq!(view["kind"], "postgres");
    assert_eq!(view["created_by"], before["created_by"]);
    assert_eq!(view["logins"], before["logins"]);
    s.admin().get_resource("renamed").expect_status(404);
    let records = changed_records(&s, 1);
    assert_eq!(records.len(), 1, "a refusal is not a change: {records:?}");
    assert_eq!(sorted_fields(&records[0]), vec!["contact"]);
}

#[test]
fn the_fields_take_what_registration_takes() {
    let s = Scenario::ready();
    let api = s.admin();
    let edge_ok = [
        json!({ "description": "d".repeat(2048) }),
        json!({ "description": "" }),
        json!({ "owner": "o".repeat(256) }),
        json!({ "owner": "o" }),
        json!({ "contact": "c".repeat(256) }),
        json!({ "contact": "c" }),
    ];
    for body in &edge_ok {
        patch_resource(&api, RESOURCE, body).expect_status(200);
    }
    let refused = [
        ("description", json!({ "description": "d".repeat(2049) })),
        ("description", json!({ "description": "é".repeat(1025) })),
        ("description", json!({ "description": "a\u{0}b" })),
        ("description", json!({ "description": "a\u{7}b" })),
        ("description", json!({ "description": 7 })),
        ("owner", json!({ "owner": "" })),
        ("owner", json!({ "owner": "o".repeat(257) })),
        ("owner", json!({ "owner": "a\u{0}b" })),
        ("owner", json!({ "owner": ["x"] })),
        ("contact", json!({ "contact": "" })),
        ("contact", json!({ "contact": "c".repeat(257) })),
        ("contact", json!({ "contact": "a\u{0}b" })),
        ("contact", json!({ "contact": { "who": "x" } })),
        ("contact", json!({ "contact": "é".repeat(129) })),
    ];
    let before = admin_view(&s, RESOURCE);
    for (field, body) in refused {
        let answer = patch_resource(&api, RESOURCE, &body);
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            message.contains(field),
            "expected the message to name `{field}`, got {message:?}"
        );
        assert!(!message.contains(&"d".repeat(100)), "never what was sent");
    }
    // A good field beside a bad one changes nothing.
    let answer = patch_resource(&api, RESOURCE, &json!({ "contact": "fine", "owner": "" }));
    assert_api_error(&answer, 400, "malformed");
    assert_eq!(
        admin_view(&s, RESOURCE),
        before,
        "a refused change changes nothing"
    );
    let answer = api
        .request(&format!("/v1/resources/{RESOURCE}"))
        .method("PATCH")
        .header("content-type", "application/json")
        .body("{not json")
        .send();
    assert_api_error(&answer, 400, "malformed");
    let mut huge = json!({ "contact": "x" }).to_string();
    huge.push_str(&" ".repeat(65537));
    let answer = api
        .request(&format!("/v1/resources/{RESOURCE}"))
        .method("PATCH")
        .header("content-type", "application/json")
        .body(huge)
        .send();
    assert_api_error(&answer, 413, "too-large");
}

#[test]
fn a_resource_that_is_not_there_is_404() {
    let s = Scenario::ready();
    // The path exists for a Resource that is there.
    patch_resource(&s.admin(), RESOURCE, &json!({ "contact": "x" })).expect_status(200);
    let answer = patch_resource(&s.admin(), "ghost", &json!({ "contact": "x" }));
    assert_api_error(&answer, 404, "not-found");
    assert_eq!(
        changed_records(&s, 1).len(),
        1,
        "only the change that was made"
    );
}

#[test]
fn a_no_grant_refusal_names_the_new_contact_at_once() {
    let s = Scenario::ready();
    let nobody = User::new("nobody");
    let ask = || {
        s.query(&nobody, "board", &sql_on(RESOURCE, "select 1 as one"))
            .1
    };
    let refusal = ask();
    assert_eq!(
        refusal["refused"]["refusal"]["reason"], "no-grant",
        "{refusal}"
    );
    assert_eq!(
        refusal["refused"]["refusal"]["contact"], CONTACT,
        "{refusal}"
    );
    let new_contact = "Sam Ops, #new-access on chat";
    patch_resource(&s.admin(), RESOURCE, &json!({ "contact": new_contact })).expect_status(200);
    let refusal = ask();
    assert_eq!(
        refusal["refused"]["refusal"]["reason"], "no-grant",
        "{refusal}"
    );
    assert_eq!(
        refusal["refused"]["refusal"]["contact"], new_contact,
        "the very next refusal names the new contact: {refusal}"
    );
    let fix = refusal["refused"]["fix"].as_str().unwrap_or_default();
    assert!(
        fix.contains(new_contact) && !fix.contains(CONTACT),
        "{fix:?}"
    );
}

#[test]
fn it_works_on_a_resource_that_is_not_served() {
    for problem in ["no-secret-key", "does-not-open", "not-sealed"] {
        let s = not_served(problem);
        let answer = patch_resource(
            &s.admin(),
            DEVOPS,
            &json!({ "contact": "Kim, #help", "owner": "Ops" }),
        );
        answer.expect_status(200);
        let view = answer.json();
        assert_eq!(
            view["contact"],
            "Kim, #help",
            "{problem}: {}",
            answer.describe()
        );
        assert_eq!(view["usable"], json!(false), "{problem}: still not served");
        assert_eq!(view["problem"], problem);
        assert_eq!(
            s.vm.api_as(&User::new("rita")).get_resource(DEVOPS).json()["contact"],
            "Kim, #help"
        );
    }
}

#[test]
fn it_works_on_a_storage_resource_and_the_new_contact_is_named() {
    let s = Storage::ready();
    let answer = call(
        &s.admin(),
        "PATCH",
        &format!("/v1/resources/{MARKETING}"),
        Some(&json!({ "description": "Campaign files", "contact": "Pat Files" })),
    );
    answer.expect_status(200);
    let view: Value = answer.json();
    assert_eq!(view["kind"], "storage");
    assert_eq!(view["description"], "Campaign files");
    assert_eq!(view["location"]["backend"], "directory", "{view}");
    let one: Response =
        s.vm.api_as(&blackbox::storage::rita())
            .get_resource(MARKETING);
    assert_eq!(one.json()["contact"], "Pat Files");
    // A refusal for someone without a Grant names it.
    let (_, answer) = s.one(
        &blackbox::storage::nobody(),
        blackbox::storage::FILES,
        &blackbox::storage::get(MARKETING, "x"),
    );
    let text = answer.to_string();
    assert!(
        text.contains("Pat Files"),
        "the refusal names the new contact: {text}"
    );
}
