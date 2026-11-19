//! Whom an Application is shared with, on the platform's API (CONTRACT.md,
//! "Contributors and sharing: On the platform's API", "The two lists"),
//! with the stand-in sign-in: the Application's object with both lists,
//! who may read it (`not-shared`), `POST /v1/applications/<name>/sharing`
//! (made whole or not at all, its refusals in their order, its bounds),
//! the log's line, the lists kept across a restart, and
//! `GET /v1/applications` for each person.
//!
//! Acceptance criteria: "After sharing with a person, a Group or everyone,
//! exactly those people can open it; after unsharing they cannot"; "The
//! tool's list of Applications for a person is exactly those they
//! contribute to or that are shared with them".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;

use blackbox::api::assert_json_content_type;
use blackbox::scenario::{ADMINISTRATORS, administrator};
use blackbox::sharing::{
    MAX_ENTRIES_A_REQUEST, MAX_SHARED_WITH, TRACED, add, add_remove, as_read,
    assert_application_members, assert_cannot_open, assert_entry, assert_error_email_entry,
    assert_error_entries, assert_opens, contributors_ok, create_unshared, email, entry_named,
    everyone, expected, group, keys, remove, share_ok, user,
};
use blackbox::{Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const APP: &str = "board";

fn vm() -> Vm {
    Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    )
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["finance"])
}

fn with_board() -> Vm {
    let vm = vm();
    create_unshared(&vm, &alice(), APP, TRACED);
    vm
}

fn sharing_records(vm: &Vm) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| r["event"] == "sharing-changed")
        .collect()
}

/// The Application's object as `reader` reads it, which must be `200`.
fn read(vm: &Vm, reader: &User) -> Value {
    let answer = vm.api_as(reader).get_application(APP);
    assert!(
        answer.status == 200,
        "expected {:?} to read {APP:?}, got\n{}",
        reader.id,
        answer.describe()
    );
    assert_json_content_type(&answer);
    let application = answer.json();
    assert_application_members(&application, &format!("{APP} as {:?} reads it", reader.id));
    application
}

#[test]
fn a_new_application_is_shared_with_nobody() {
    let vm = vm();
    let created = vm.api_as(&alice()).create_application(APP);
    created.expect_status(201);
    let application = created.json();
    assert_eq!(
        application["shared_with"],
        json!([]),
        "expected a new Application shared with nobody, in {application}"
    );
    assert_eq!(application["contributor"], true, "in {application}");
    assert_eq!(read(&vm, &alice())["shared_with"], json!([]));
}

#[test]
fn someone_who_cannot_open_it_reads_403_not_shared_after_not_found() {
    let vm = with_board();
    for reader in [bob(), carol(), User::new("ALICE")] {
        let answer = vm.api_as(&reader).get_application(APP);
        assert_api_error(&answer, 403, "not-shared");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            message.contains("contributor"),
            "expected the message to say to ask one of its Contributors, got {message:?}"
        );
        assert!(
            !message.contains("alice"),
            "expected the message to name no Contributor, got {message:?}"
        );
        assert_api_error(
            &vm.api_as(&reader).get_application("nothing-here"),
            404,
            "not-found",
        );
    }
}

#[test]
fn someone_it_is_shared_with_reads_who_maintains_it_but_not_whom_it_is_shared_with() {
    let vm = with_board();
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), group("finance")]),
    );
    for reader in [bob(), carol()] {
        let application = read(&vm, &reader);
        assert!(
            application.get("shared_with").is_some_and(Value::is_null),
            "expected `shared_with` null for {:?}, who is not a Contributor, got {application}",
            reader.id
        );
        assert_eq!(
            keys(&application["contributors"]),
            expected(&["user:alice"])
        );
        assert_eq!(application["contributor"], false, "in {application}");
        assert_eq!(application["name"], APP);
    }
}

#[test]
fn a_contributor_reads_whom_it_is_shared_with() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[group("finance")]));
    share_ok(&api, &vm, APP, &add(&[user("bob"), everyone()]));
    for reader in [alice(), carol()] {
        let application = read(&vm, &reader);
        assert_eq!(
            keys(&application["shared_with"]),
            expected(&["user:bob", "everyone"]),
            "{:?}, a Contributor, sees whom it is shared with",
            reader.id
        );
        assert_eq!(application["contributor"], true, "in {application}");
    }
}

#[test]
fn a_platform_administrator_reads_any_application_and_whom_it_is_shared_with() {
    let vm = with_board();
    let admin = administrator();
    let unshared = read(&vm, &admin);
    assert_eq!(unshared["shared_with"], json!([]), "in {unshared}");
    assert_eq!(unshared["contributor"], false, "in {unshared}");
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    let shared = read(&vm, &admin);
    assert_eq!(keys(&shared["shared_with"]), expected(&["user:bob"]));
}

#[test]
fn sharing_keeps_each_entry_in_the_order_added_with_who_added_it_and_when() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let after = share_ok(&api, &vm, APP, &add(&[group("finance"), user("bob")]));
    assert_eq!(
        keys(&after["shared_with"]),
        expected(&["group:finance", "user:bob"])
    );
    contributors_ok(&api, &vm, APP, &add(&[user("carol")]));
    let carols = vm.api_as(&User::new("carol"));
    let after = share_ok(&carols, &vm, APP, &add(&[everyone(), user("dave")]));
    let shared = &after["shared_with"];
    assert_eq!(
        keys(shared),
        expected(&["group:finance", "user:bob", "everyone", "user:dave"]),
        "in the order they were added"
    );
    assert_entry(
        entry_named(shared, "group:finance"),
        "alice",
        None,
        None,
        "finance",
    );
    assert_entry(entry_named(shared, "user:bob"), "alice", None, None, "bob");
    assert_entry(
        entry_named(shared, "everyone"),
        "carol",
        None,
        None,
        "everyone",
    );
    assert_entry(
        entry_named(shared, "user:dave"),
        "carol",
        None,
        None,
        "dave",
    );
    assert_eq!(
        as_read(&after),
        read(&vm, &alice()),
        "the answer is the Application as it is after"
    );
}

#[test]
fn a_change_answers_the_application_as_it_is_after() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let answer = api.change_sharing(APP, &add(&[user("bob")]));
    answer.expect_status(200);
    assert_json_content_type(&answer);
    let body = answer.json();
    for field in [
        "name",
        "description",
        "address",
        "contributors",
        "shared_with",
        "contributor",
        "serving_version",
        "created_by",
        "created_at",
    ] {
        assert!(
            body.get(field).is_some(),
            "expected `{field}` in the Application answered, got {body}"
        );
    }
    assert_eq!(as_read(&body), read(&vm, &alice()));
    let removed = share_ok(&api, &vm, APP, &remove(&[user("bob")]));
    assert_eq!(removed["shared_with"], json!([]));
    assert_eq!(as_read(&removed), read(&vm, &alice()));
}

#[test]
fn adding_what_is_there_and_removing_what_is_not_change_nothing_and_are_not_refused() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[user("carol")]));
    let first = share_ok(&api, &vm, APP, &add(&[user("bob"), group("finance")]));
    let carols = vm.api_as(&User::new("carol"));
    let again = share_ok(&carols, &vm, APP, &add(&[user("bob"), group("finance")]));
    assert_eq!(
        again["shared_with"], first["shared_with"],
        "adding entries that are there changes nothing: not the order, not who added them, not when"
    );
    for key in ["user:bob", "group:finance"] {
        assert_eq!(
            entry_named(&again["shared_with"], key)["added_by"],
            "alice",
            "{key}, added again by carol, keeps alice as `added_by`"
        );
    }
    // Added again after another entry: it keeps its place.
    share_ok(&api, &vm, APP, &add(&[user("erin")]));
    let kept = share_ok(&carols, &vm, APP, &add(&[user("erin"), user("bob")]));
    assert_eq!(
        keys(&kept["shared_with"]),
        expected(&["user:bob", "group:finance", "user:erin"]),
        "an entry added again keeps its place"
    );
    assert_eq!(
        entry_named(&kept["shared_with"], "user:bob"),
        entry_named(&first["shared_with"], "user:bob"),
        "bob's entry, added again, is as it was kept the first time"
    );
    share_ok(&api, &vm, APP, &remove(&[user("erin")]));
    let not_there = share_ok(
        &carols,
        &vm,
        APP,
        &remove(&[user("zed"), group("nobody-group"), everyone()]),
    );
    assert_eq!(not_there["shared_with"], first["shared_with"]);
    let twice = share_ok(&api, &vm, APP, &add(&[user("dave"), user("dave")]));
    assert_eq!(
        keys(&twice["shared_with"]),
        expected(&["user:bob", "group:finance", "user:dave"]),
        "an entry given twice is kept once"
    );
    // Nothing changed by the requests in between: no line for them. The
    // line of adding dave, the last change, is written after the lines of
    // every request before it; once it is in, all of theirs are.
    vm.wait_for_record("the `sharing-changed` record of adding dave", |r| {
        r["event"] == "sharing-changed" && r["added"].to_string().contains("dave")
    });
    let lines: Vec<Value> = sharing_records(&vm);
    let changed: Vec<(Vec<String>, Vec<String>)> = lines
        .iter()
        .map(|l| (keys(&l["added"]), keys(&l["removed"])))
        .collect();
    assert_eq!(
        changed,
        vec![
            (expected(&["user:bob", "group:finance"]), Vec::new()),
            (expected(&["user:erin"]), Vec::new()),
            (Vec::new(), expected(&["user:erin"])),
            (expected(&["user:dave"]), Vec::new()),
        ],
        "expected a `sharing-changed` line only when something changed (bob and finance added, erin added, erin removed, dave added), got {lines:?}"
    );
}

#[test]
fn the_same_entry_in_add_and_in_remove_is_400_malformed_and_changes_nothing() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    share_ok(&api, &vm, APP, &add(&[user("bob")]));
    let before = read(&vm, &alice());
    for (added, removed) in [
        (vec![user("carol")], vec![user("carol")]),
        (vec![user("bob")], vec![user("bob")]),
        (vec![group("finance"), user("dave")], vec![group("finance")]),
        (vec![everyone()], vec![everyone()]),
    ] {
        assert_api_error(
            &api.change_sharing(APP, &add_remove(&added, &removed)),
            400,
            "malformed",
        );
    }
    assert_api_error(
        &api.change_contributors(APP, &add_remove(&[user("carol")], &[user("carol")])),
        400,
        "malformed",
    );
    assert_eq!(
        read(&vm, &alice()),
        before,
        "a refused change changes nothing"
    );
}

/// Bodies that are not `{ "add": [...], "remove": [...] }` of 1 to 100
/// entries in all.
fn malformed_bodies() -> Vec<(String, Vec<u8>)> {
    let long = "u".repeat(257);
    let values = [
        json!({}),
        json!({ "add": [] }),
        json!({ "remove": [] }),
        json!({ "add": [], "remove": [] }),
        json!({ "add": null }),
        json!({ "add": "bob" }),
        json!({ "add": { "user": "bob" } }),
        json!({ "add": [ "bob" ] }),
        json!({ "add": [ {} ] }),
        json!({ "add": [ { "user": "" } ] }),
        json!({ "add": [ { "user": long } ] }),
        json!({ "add": [ { "user": "bo\u{7}b" } ] }),
        json!({ "add": [ { "user": "bo\nb" } ] }),
        json!({ "add": [ { "user": 7 } ] }),
        json!({ "add": [ { "group": "" } ] }),
        json!({ "add": [ { "group": long } ] }),
        json!({ "add": [ { "group": "fin\u{0}ance" } ] }),
        json!({ "add": [ { "everyone": false } ] }),
        json!({ "add": [ { "everyone": "true" } ] }),
        json!({ "add": [ { "user": "bob", "group": "finance" } ] }),
        json!({ "add": [ { "user": "bob", "everyone": true } ] }),
        json!({ "add": [ { "team": "finance" } ] }),
        json!({ "add": [ { "email": 3 } ] }),
        json!({ "remove": [ { "user": null } ] }),
        json!([ { "user": "bob" } ]),
        json!("add"),
        json!({ "other": [ { "user": "bob" } ] }),
        json!({ "add": [ { "everyone": 1 } ] }),
        json!({ "add": [ { "everyone": null } ] }),
        json!({ "add": [ { "user": "bob", "email": "bob@example.test" } ] }),
        json!({ "add": [ { "group": "finance", "email": "bob@example.test" } ] }),
        json!({ "add": [ { "added_by": "alice" } ] }),
        json!({ "remove": [ {} ] }),
    ];
    let mut bodies: Vec<(String, Vec<u8>)> = values
        .iter()
        .map(|v| (v.to_string(), v.to_string().into_bytes()))
        .collect();
    let too_many: Vec<Value> = (0..=MAX_ENTRIES_A_REQUEST)
        .map(|n| user(&format!("p{n}")))
        .collect();
    bodies.push((
        "101 entries in add".to_owned(),
        add(&too_many).to_string().into_bytes(),
    ));
    let added: Vec<Value> = (0..51).map(|n| user(&format!("q{n}"))).collect();
    let removed: Vec<Value> = (0..50).map(|n| group(&format!("g{n}"))).collect();
    bodies.push((
        "51 in add and 50 in remove".to_owned(),
        add_remove(&added, &removed).to_string().into_bytes(),
    ));
    bodies.push(("not JSON".to_owned(), b"{ not json".to_vec()));
    bodies.push((
        "a key twice".to_owned(),
        br#"{ "add": [ { "user": "bob" } ], "add": [ { "user": "carol" } ] }"#.to_vec(),
    ));
    bodies.push(("empty".to_owned(), Vec::new()));
    bodies
}

#[test]
fn malformed_bodies_are_400_malformed_and_change_nothing() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let before = read(&vm, &alice());
    for path in ["sharing", "contributors"] {
        for (what, body) in malformed_bodies() {
            let answer = api.post_raw(&format!("/v1/applications/{APP}/{path}"), &body);
            assert!(
                answer.status == 400,
                "expected `400 malformed` for {what} on {path}, got\n{}",
                answer.describe()
            );
            assert_api_error(&answer, 400, "malformed");
        }
    }
    // `everyone` is no Contributor.
    for body in [
        add(&[everyone()]),
        add(&[user("bob"), everyone()]),
        remove(&[everyone()]),
    ] {
        assert_api_error(&api.change_contributors(APP, &body), 400, "malformed");
    }
    assert_eq!(read(&vm, &alice()), before);
}

#[test]
fn a_hundred_entries_in_one_request_are_taken() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let hundred: Vec<Value> = (0..MAX_ENTRIES_A_REQUEST)
        .map(|n| user(&format!("p{n:03}")))
        .collect();
    let after = share_ok(&api, &vm, APP, &add(&hundred));
    assert_eq!(after["shared_with"].as_array().map(Vec::len), Some(100));
    let mixed = add_remove(
        &(0..50).map(|n| group(&format!("g{n}"))).collect::<Vec<_>>(),
        &(0..50)
            .map(|n| user(&format!("p{n:03}")))
            .collect::<Vec<_>>(),
    );
    let after = share_ok(&api, &vm, APP, &mixed);
    assert_eq!(after["shared_with"].as_array().map(Vec::len), Some(100));
}

#[test]
fn ids_and_groups_of_256_bytes_any_letters_are_taken_and_compared_exactly() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let long_user = "é".repeat(128);
    let long_group = "g".repeat(256);
    let after = share_ok(
        &api,
        &vm,
        APP,
        &add(&[user(&long_user), group(&long_group), user("zoë 数据")]),
    );
    assert_eq!(
        keys(&after["shared_with"]),
        vec![
            format!("user:{long_user}"),
            format!("group:{long_group}"),
            "user:zoë 数据".to_owned()
        ]
    );
    assert_opens(&vm, &User::new(&long_user), APP, "a 256-byte id");
    assert_opens(
        &vm,
        &User::new("x").in_groups(&[long_group.as_str()]),
        APP,
        "a 256-byte Group",
    );
    assert_opens(&vm, &User::new("zoë 数据"), APP, "an id with any letters");
}

#[test]
fn a_person_or_group_need_not_exist_to_be_shared_with() {
    let vm = with_board();
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("never-signed-in"), group("no-such-group-yet")]),
    );
    assert_opens(
        &vm,
        &User::new("never-signed-in"),
        APP,
        "their first sign-in",
    );
    assert_opens(
        &vm,
        &User::new("x").in_groups(&["no-such-group-yet"]),
        APP,
        "the Group's first member",
    );
}

#[test]
fn refusals_come_in_their_order_not_found_method_not_allowed_not_a_contributor_malformed() {
    let vm = with_board();
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    for path in ["sharing", "contributors"] {
        // not-found first, whatever the body and the method.
        for asker in [alice(), bob()] {
            let api = vm.api_as(&asker);
            assert_api_error(
                &api.post_raw(
                    &format!("/v1/applications/nothing-here/{path}"),
                    "{ not json",
                ),
                404,
                "not-found",
            );
            assert_api_error(
                &api.get(&format!("/v1/applications/nothing-here/{path}")),
                404,
                "not-found",
            );
        }
        // method-not-allowed before not-a-contributor.
        for method in ["GET", "PUT", "DELETE", "PATCH"] {
            for asker in [alice(), bob(), carol()] {
                let answer = vm
                    .api_as(&asker)
                    .request(&format!("/v1/applications/{APP}/{path}"))
                    .method(method)
                    .send();
                assert_api_error(&answer, 405, "method-not-allowed");
            }
        }
        // not-a-contributor before malformed, also for someone it is
        // shared with, and for a Platform administrator.
        for asker in [bob(), carol(), administrator()] {
            let api = vm.api_as(&asker);
            assert_api_error(
                &api.post_raw(&format!("/v1/applications/{APP}/{path}"), "{ not json"),
                403,
                "not-a-contributor",
            );
            assert_api_error(
                &api.post(
                    &format!("/v1/applications/{APP}/{path}"),
                    &add(&[user(&asker.id)]),
                ),
                403,
                "not-a-contributor",
            );
        }
        assert_api_error(
            &vm.api_as(&alice())
                .post_raw(&format!("/v1/applications/{APP}/{path}"), "{ not json"),
            400,
            "malformed",
        );
    }
}

#[test]
fn a_platform_administrator_who_is_not_a_contributor_cannot_change_either_list() {
    let vm = with_board();
    let admin = vm.api_as(&administrator());
    let before = read(&vm, &alice());
    assert_api_error(
        &admin.change_sharing(APP, &add(&[everyone()])),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &admin.change_contributors(APP, &add(&[user(&administrator().id)])),
        403,
        "not-a-contributor",
    );
    assert_eq!(read(&vm, &alice()), before);
    assert_cannot_open(
        &vm,
        &administrator(),
        APP,
        "the refused sharing gave nothing",
    );
}

#[test]
fn an_email_nobody_signed_in_with_is_422_unknown_email_and_changes_nothing() {
    // With the stand-in the platform knows nobody's email.
    let vm = with_board();
    let api = vm.api_as(&alice());
    let before = read(&vm, &alice());
    for body in [
        add(&[email("bob@example.test")]),
        add(&[user("bob"), email("bob@example.test")]),
        add_remove(&[group("finance")], &[email("bob@example.test")]),
    ] {
        let answer = api.change_sharing(APP, &body);
        assert_api_error(&answer, 422, "unknown-email");
        assert_error_email_entry(&answer, "bob@example.test", &format!("{body}"));
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains("bob@example.test"),
            "expected the refusal to name the entry it is about, got {message:?}"
        );
        assert!(
            message.contains("vibe whoami") && message.to_ascii_lowercase().contains("sign in"),
            "expected the message to say to use their user id, which `vibe whoami` shows, or to ask them to sign in once, got {message:?}"
        );
    }
    assert_eq!(
        read(&vm, &alice()),
        before,
        "a change with one refused entry changes nothing, not even its good entries"
    );
    assert_cannot_open(&vm, &bob(), APP, "bob, whose entry came with a refused one");
}

#[test]
fn too_many_entries_is_409_too_many_and_changes_nothing() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    for round in 0..(MAX_SHARED_WITH / MAX_ENTRIES_A_REQUEST) {
        let batch: Vec<Value> = (0..MAX_ENTRIES_A_REQUEST)
            .map(|n| user(&format!("p{round}-{n}")))
            .collect();
        share_ok(&api, &vm, APP, &add(&batch));
    }
    let full = read(&vm, &alice());
    assert_eq!(
        full["shared_with"].as_array().map(Vec::len),
        Some(MAX_SHARED_WITH)
    );
    for (body, names, would_add) in [
        (
            add(&[user("one-more")]),
            vec!["one-more"],
            vec!["user:one-more"],
        ),
        (add(&[everyone()]), vec!["everyone"], vec!["everyone"]),
        (
            add_remove(&[user("x1"), user("x2")], &[user("p0-0")]),
            vec!["x1", "x2"],
            vec!["user:x1", "user:x2"],
        ),
        // Already there, so not something it would have added.
        (
            add(&[user("p0-1"), user("one-more")]),
            vec!["one-more"],
            vec!["user:one-more"],
        ),
    ] {
        let answer = api.change_sharing(APP, &body);
        assert_api_error(&answer, 409, "too-many");
        assert_error_entries(&answer, &would_add, &format!("too-many for {body}"));
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            names.iter().any(|n| message.contains(n)),
            "expected the refusal to name the entry it is about ({names:?}), got {message:?}"
        );
    }
    assert_eq!(
        read(&vm, &alice()),
        full,
        "a refused change changes nothing"
    );
    // Taking one out and putting one in does not pass the bound.
    let swapped = share_ok(
        &api,
        &vm,
        APP,
        &add_remove(&[user("one-more")], &[user("p0-0")]),
    );
    assert_eq!(
        swapped["shared_with"].as_array().map(Vec::len),
        Some(MAX_SHARED_WITH)
    );
}

#[test]
fn the_log_has_a_line_for_each_change_with_the_entries_as_kept() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    share_ok(
        &api,
        &vm,
        APP,
        &add(&[user("bob"), group("finance"), everyone()]),
    );
    let line = vm.wait_for_record("a `sharing-changed` record", |r| {
        r["event"] == "sharing-changed"
    });
    assert_eq!(line["application"], APP, "{line}");
    assert_eq!(line["by"], "alice", "{line}");
    assert_eq!(
        keys(&line["added"]),
        expected(&["user:bob", "group:finance", "everyone"]),
        "{line}"
    );
    assert_eq!(line["removed"], json!([]), "{line}");
    share_ok(
        &api,
        &vm,
        APP,
        &add_remove(&[user("dave")], &[everyone(), user("zed")]),
    );
    vm.wait_for_record("the `sharing-changed` record of adding dave", |r| {
        r["event"] == "sharing-changed" && r["added"].to_string().contains("dave")
    });
    let lines = sharing_records(&vm);
    let last = lines.last().unwrap();
    assert_eq!(keys(&last["added"]), expected(&["user:dave"]), "{last}");
    assert_eq!(
        keys(&last["removed"]),
        expected(&["everyone"]),
        "only what was removed, not what was not there: {last}"
    );
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn both_lists_are_kept_across_a_restart() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[group("finance"), user("dave")]));
    share_ok(
        &api,
        &vm,
        APP,
        &add(&[user("bob"), group("ops"), everyone()]),
    );
    share_ok(&api, &vm, APP, &remove(&[everyone()]));
    let before = read(&vm, &alice());
    let vm = vm.restart();
    assert_eq!(
        read(&vm, &alice()),
        before,
        "the Contributors and whom it is shared with, with who added each and when, after a restart"
    );
    assert_opens(&vm, &bob(), APP, "bob, shared with before the restart");
    assert_opens(&vm, &carol(), APP, "carol, a Contributor by finance");
    assert_opens(
        &vm,
        &User::new("oscar").in_groups(&["ops"]),
        APP,
        "a member of ops",
    );
    assert_cannot_open(
        &vm,
        &User::new("zed"),
        APP,
        "everyone was unshared before the restart",
    );
}

/// The `GET /v1/applications` of this User: (name, contributor) by name.
fn listed(vm: &Vm, reader: &User) -> Vec<(String, bool)> {
    let answer: Response = vm.api_as(reader).list_applications();
    answer.expect_status(200);
    let body = answer.json();
    body["applications"]
        .as_array()
        .unwrap_or_else(|| panic!("expected {{ applications: [...] }}, got {body}"))
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap_or_default().to_owned(),
                a["contributor"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("expected `contributor` a boolean in {a}")),
            )
        })
        .collect()
}

fn pairs(items: &[(&str, bool)]) -> Vec<(String, bool)> {
    items.iter().map(|(n, c)| ((*n).to_owned(), *c)).collect()
}

#[test]
fn the_list_of_applications_is_exactly_those_contributed_to_and_those_shared_each_once() {
    let vm = vm();
    let api = vm.api_as(&alice());
    for name in [
        "a-mine",
        "b-by-person",
        "c-by-group",
        "d-everyone",
        "e-both",
        "f-hidden",
    ] {
        create_unshared(&vm, &alice(), name, TRACED);
    }
    api.create_application("g-no-version").expect_status(201);
    share_ok(&api, &vm, "b-by-person", &add(&[user("bob")]));
    share_ok(&api, &vm, "c-by-group", &add(&[group("finance")]));
    share_ok(&api, &vm, "d-everyone", &add(&[everyone()]));
    // e-both: bob is a Contributor by his id and through a Group, and it
    // is shared with him three ways: listed once, as a Contributor.
    contributors_ok(&api, &vm, "e-both", &add(&[user("bob"), group("finance")]));
    share_ok(
        &api,
        &vm,
        "e-both",
        &add(&[user("bob"), group("finance"), everyone()]),
    );
    share_ok(&api, &vm, "g-no-version", &add(&[user("bob")]));
    let bob_in_finance = User::new("bob").in_groups(&["finance"]);
    assert_eq!(
        listed(&vm, &bob_in_finance),
        pairs(&[
            ("b-by-person", false),
            ("c-by-group", false),
            ("d-everyone", false),
            ("e-both", true),
            ("g-no-version", false),
        ])
    );
    assert_eq!(
        listed(&vm, &carol()),
        pairs(&[
            ("c-by-group", false),
            ("d-everyone", false),
            ("e-both", true)
        ])
    );
    assert_eq!(
        listed(&vm, &User::new("zed")),
        pairs(&[("d-everyone", false), ("e-both", false)])
    );
    assert_eq!(
        listed(&vm, &alice()),
        pairs(&[
            ("a-mine", true),
            ("b-by-person", true),
            ("c-by-group", true),
            ("d-everyone", true),
            ("e-both", true),
            ("f-hidden", true),
            ("g-no-version", true),
        ])
    );
    // A Platform administrator lists what they contribute to or can open,
    // like anyone.
    assert_eq!(
        listed(&vm, &administrator()),
        pairs(&[("d-everyone", false), ("e-both", false)])
    );
    // Unshared: gone from the list at once.
    share_ok(&api, &vm, "d-everyone", &remove(&[everyone()]));
    assert_eq!(listed(&vm, &User::new("zed")), pairs(&[("e-both", false)]));
}

#[test]
fn a_change_answers_what_it_added_and_removed_as_kept_in_the_order_added() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    for (path, first, second) in [
        ("sharing", group("finance"), everyone()),
        ("contributors", group("finance"), group("ops")),
    ] {
        let change = |body: &Value| {
            if path == "sharing" {
                share_ok(&api, &vm, APP, body)
            } else {
                contributors_ok(&api, &vm, APP, body)
            }
        };
        let list = if path == "sharing" {
            "shared_with"
        } else {
            "contributors"
        };
        // Added in the order the request gives them, as kept.
        let made = change(&add(&[user("bob"), first.clone()]));
        assert_eq!(
            keys(&made["added"]),
            expected(&["user:bob", "group:finance"]),
            "{path}: `added` in the order added: {made}"
        );
        assert_eq!(made["removed"], json!([]), "{path}: {made}");
        for entry in made["added"].as_array().unwrap() {
            let key = blackbox::sharing::entry_key(entry).unwrap();
            assert_eq!(
                entry,
                entry_named(&made[list], &key),
                "{path}: each added entry as it is kept"
            );
            assert_entry(entry, "alice", None, None, &format!("{path}: added {key}"));
        }
        // Some there already, some new: only the new ones are `added`.
        let more = change(&add(&[first.clone(), second.clone(), user("bob")]));
        assert_eq!(
            more["added"],
            json!([entry_named(
                &more[list],
                &blackbox::sharing::entry_key(&second).unwrap()
            )]),
            "{path}: only what this request added: {more}"
        );
        assert_eq!(more["removed"], json!([]), "{path}: {more}");
        // Everything so already: both empty.
        let nothing = change(&add_remove(&[user("bob")], &[user("never-there")]));
        assert_eq!(nothing["added"], json!([]), "{path}: {nothing}");
        assert_eq!(nothing["removed"], json!([]), "{path}: {nothing}");
        // Removed: each as it was kept, in the order they were added, not
        // the order of the request; one not there is not in it.
        let kept_bob = entry_named(&more[list], "user:bob").clone();
        let kept_first = entry_named(&more[list], "group:finance").clone();
        let removed = change(&add_remove(
            &[user("dave")],
            &[first.clone(), user("zed"), user("bob")],
        ));
        assert_eq!(
            removed["removed"],
            json!([kept_bob, kept_first]),
            "{path}: `removed` holds the entries as they were kept, in the order they were added: {removed}"
        );
        assert_eq!(
            keys(&removed["added"]),
            expected(&["user:dave"]),
            "{path}: {removed}"
        );
        assert_eq!(
            as_read(&removed),
            read(&vm, &alice()),
            "{path}: beside `added` and `removed`, the Application as it is after"
        );
    }
}

/// What a reader of Versions is asked for, in order: the Versions,
/// Version 1 (which exists), Version 99 (which does not), two `<number>`s
/// that are no number, and the Versions with a `?limit` that is good, bad
/// and given twice.
const VERSION_TARGETS: [&str; 8] = [
    "/v1/applications/board/versions",
    "/v1/applications/board/versions/1",
    "/v1/applications/board/versions/99",
    "/v1/applications/board/versions/abc",
    "/v1/applications/board/versions/1.5",
    "/v1/applications/board/versions?limit=1",
    "/v1/applications/board/versions?limit=abc",
    "/v1/applications/board/versions?limit=1&limit=2",
];

#[test]
fn versions_are_read_by_a_contributor_and_a_platform_administrator_and_by_nobody_else() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[group("finance")]));
    share_ok(&api, &vm, APP, &add(&[user("bob"), group("ops")]));
    // A Contributor by id, a Contributor by a Group, and a Platform
    // administrator who is not a Contributor.
    for reader in [alice(), carol(), administrator()] {
        let reads = vm.api_as(&reader);
        let versions = reads.list_versions(APP);
        assert_eq!(
            versions.status,
            200,
            "{:?} reads the Versions\n{}",
            reader.id,
            versions.describe()
        );
        assert_eq!(
            versions.json()["versions"].as_array().map(Vec::len),
            Some(1),
            "{}",
            versions.describe()
        );
        let one = reads.get_version(APP, 1);
        assert_eq!(one.status, 200, "{:?}\n{}", reader.id, one.describe());
        assert_api_error(&reads.get_version(APP, 99), 404, "not-found");
        // A `<number>` that is no number: `not-found`, as before this
        // ticket (api_errors).
        for target in [
            "/v1/applications/board/versions/abc",
            "/v1/applications/board/versions/1.5",
        ] {
            assert_api_error(&reads.get(target), 404, "not-found");
        }
        assert_api_error(
            &reads.get(&format!("/v1/applications/{APP}/versions?limit=abc")),
            400,
            "malformed",
        );
    }
    // Someone it is shared with (by id, by a Group), and people who cannot
    // open it at all: `not-a-contributor`, the same answer for a Version
    // that exists and one that does not, whatever the `?limit`.
    for reader in [
        bob(),
        User::new("oscar").in_groups(&["ops"]),
        User::new("zed"),
        User::new("ALICE").in_groups(&["staff"]),
    ] {
        let reads = vm.api_as(&reader);
        let answers: Vec<Response> = VERSION_TARGETS.iter().map(|t| reads.get(t)).collect();
        for answer in &answers {
            assert_api_error(answer, 403, "not-a-contributor");
        }
        for other in &answers[2..5] {
            assert_eq!(
                answers[1].json(),
                other.json(),
                "{:?}: Version 1, which exists, Version 99, which does not, and a `<number>` that is no number get the same answer ({})",
                reader.id,
                other.request()
            );
        }
        // The `not-found` of the Application comes first ...
        for target in [
            "/v1/applications/nothing-here/versions",
            "/v1/applications/nothing-here/versions/1",
            "/v1/applications/nothing-here/versions?limit=abc",
        ] {
            assert_api_error(&reads.get(target), 404, "not-found");
        }
        // ... then `method-not-allowed`.
        for (method, target) in [
            ("PUT", "/v1/applications/board/versions"),
            ("DELETE", "/v1/applications/board/versions"),
            ("PUT", "/v1/applications/board/versions/1"),
            ("DELETE", "/v1/applications/board/versions/99"),
            ("POST", "/v1/applications/board/versions/1"),
        ] {
            let answer = reads.request(target).method(method).send();
            assert_api_error(&answer, 405, "method-not-allowed");
        }
    }
    // A Contributor removed stops reading them at the next request.
    contributors_ok(&api, &vm, APP, &remove(&[group("finance")]));
    assert_api_error(
        &vm.api_as(&carol()).list_versions(APP),
        403,
        "not-a-contributor",
    );
}

/// Whether this User may open the Application whose lists are these (as
/// a Contributor reads them): a Contributor by id or by a Group, or shared
/// with by id, by a Group or with everyone.
fn may_open(application: &Value, id: &str, groups: &[&str]) -> bool {
    let names = |list: &Value| {
        keys(list).into_iter().any(|key| {
            key == format!("user:{id}")
                || key == "everyone"
                || groups.iter().any(|g| key == format!("group:{g}"))
        })
    };
    names(&application["contributors"]) || names(&application["shared_with"])
}

/// Everyone the changes at once name, with their Groups.
const INVOLVED: [(&str, &[&str]); 11] = [
    ("alice", &["staff"]),
    ("bob", &[]),
    ("carol", &["finance"]),
    ("dave", &[]),
    ("erin", &[]),
    ("frank", &[]),
    ("gina", &[]),
    ("hank", &[]),
    ("oscar", &["ops"]),
    ("ian", &["eng"]),
    ("quinn", &["qa"]),
];

/// Fails unless what the Application's address answers each person agrees
/// with the lists `application` holds: opened when they hold them, the
/// `403` otherwise; and a stranger in no Group is let in only through
/// everyone.
fn assert_opening_agrees(vm: &Vm, application: &Value, context: &str) {
    let stranger: (&str, &[&str]) = ("zed", &[]);
    for (id, groups) in INVOLVED.iter().chain([&stranger]) {
        let person = User::new(id).in_groups(groups);
        let ctx = format!(
            "{context}: {id} {groups:?}, with contributors {} and shared_with {}",
            application["contributors"], application["shared_with"]
        );
        if may_open(application, id, groups) {
            assert_opens(vm, &person, APP, &ctx);
        } else {
            assert_cannot_open(vm, &person, APP, &ctx);
        }
    }
}

#[test]
fn changes_made_at_once_leave_the_address_deciding_by_the_lists_the_api_answers() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let changes: Vec<(&str, Value)> = vec![
        // Different people and Groups at once.
        ("sharing", add(&[user("bob")])),
        ("sharing", add(&[group("finance")])),
        ("sharing", add_remove(&[user("dave")], &[group("ops")])),
        ("sharing", remove(&[user("erin")])),
        ("contributors", add(&[user("gina")])),
        // The same entry added and removed at once.
        ("sharing", add(&[user("frank")])),
        ("sharing", remove(&[user("frank")])),
        ("sharing", add(&[group("eng")])),
        ("sharing", remove(&[group("eng")])),
        ("sharing", add(&[everyone()])),
        ("sharing", remove(&[everyone()])),
        ("contributors", add(&[user("hank")])),
        ("contributors", remove(&[user("hank")])),
        ("contributors", add(&[group("qa")])),
        ("contributors", remove(&[group("qa")])),
    ];
    let mut answered: Vec<(usize, Value, Value)> = Vec::new();
    for round in 0..3 {
        // The same lists before each round.
        share_ok(
            &api,
            &vm,
            APP,
            &add_remove(
                &[group("ops"), user("erin"), group("eng")],
                &[
                    user("bob"),
                    group("finance"),
                    user("dave"),
                    user("frank"),
                    everyone(),
                ],
            ),
        );
        contributors_ok(
            &api,
            &vm,
            APP,
            &remove(&[user("gina"), user("hank"), group("qa")]),
        );
        let start = Barrier::new(changes.len());
        let answers: Vec<(Value, Response)> = thread::scope(|scope| {
            let handles: Vec<_> = changes
                .iter()
                .map(|(path, body)| {
                    let (vm, start) = (&vm, &start);
                    scope.spawn(move || {
                        let request = vm
                            .api_as(&alice())
                            .request(&format!("/v1/applications/{APP}/{path}"))
                            .method("POST")
                            .json(body);
                        start.wait();
                        (body.clone(), request.send())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (body, answer) in &answers {
            assert_eq!(
                answer.status,
                200,
                "round {round}: {body}\n{}",
                answer.describe()
            );
            answered.push((round, body.clone(), answer.json()));
        }
        let lists = read(&vm, &alice());
        assert_opening_agrees(&vm, &lists, &format!("round {round}, after the changes"));
    }
    let lists = read(&vm, &alice());
    let vm = vm.restart();
    let after = read(&vm, &alice());
    assert_eq!(after, lists, "the lists after a restart");
    assert_opening_agrees(&vm, &after, "after a restart");
    // What each answer says it added and removed (checked last, so that a
    // failure here does not hide whether the address agrees).
    for (round, body, answer) in &answered {
        blackbox::sharing::assert_change_answer_members(answer, &format!("round {round}"));
        // Someone else's change at the same moment is never in them.
        for (member, asked) in [("added", &body["add"]), ("removed", &body["remove"])] {
            let asked: Vec<String> = if asked.is_null() {
                Vec::new()
            } else {
                keys(asked)
            };
            for key in keys(&answer[member]) {
                assert!(
                    asked.contains(&key),
                    "round {round}: `{member}` of {body} holds {key}, which it did not ask for: {answer}"
                );
            }
        }
    }
}
