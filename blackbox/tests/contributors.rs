//! The Contributors of an Application (CONTRACT.md, "Contributors and
//! sharing: The two lists", "On the platform's API"), with the stand-in
//! sign-in: a Contributor adds and removes Contributors, people or Groups;
//! a Contributor by a Group deploys, goes back and reads logs; the last
//! Contributor cannot be removed; a Contributor may remove themselves while
//! one is left; at most 100.
//!
//! Acceptance criterion: "A Contributor adds and removes Contributors,
//! people or Groups; the last Contributor cannot be removed."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::{marked_bundle, version_body};
use blackbox::logs::{from_to, go_back, page};
use blackbox::scenario::{ADMINISTRATORS, administrator};
use blackbox::sharing::{
    MAX_CONTRIBUTORS, MAX_ENTRIES_A_REQUEST, TRACED, add, add_remove, assert_application_members,
    assert_entry, contributors_ok, create_unshared, entry_named, expected, group, keys, remove,
    user,
};
use blackbox::{User, Vm, VmOptions, assert_api_error};
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

fn read(vm: &Vm, reader: &User) -> Value {
    let application = vm.api_as(reader).application(APP);
    assert_application_members(&application, &format!("{APP} as {:?} reads it", reader.id));
    application
}

#[test]
fn the_creator_is_the_first_contributor_with_who_added_them_and_when() {
    let vm = vm();
    let created = vm.api_as(&alice()).create_application(APP);
    created.expect_status(201);
    let application = created.json();
    assert_eq!(
        keys(&application["contributors"]),
        expected(&["user:alice"])
    );
    assert_entry(
        entry_named(&application["contributors"], "user:alice"),
        "alice",
        None,
        None,
        "the creator, with the stand-in (no name, no email)",
    );
    assert_application_members(&application, "the answer to POST /v1/applications");
    assert_eq!(
        entry_named(&application["contributors"], "user:alice")["added_at"],
        application["created_at"],
        "the creator's entry was added when the Application was created, in {application}"
    );
    assert_eq!(application["contributor"], true);
    assert_eq!(
        read(&vm, &alice())["contributors"],
        application["contributors"],
        "the creator's entry is answered the same when read"
    );
}

#[test]
fn a_contributor_adds_people_and_groups_in_the_order_added_with_who_and_when() {
    let vm = with_board();
    let after = contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), group("finance")]),
    );
    assert_eq!(
        keys(&after["contributors"]),
        expected(&["user:alice", "user:bob", "group:finance"])
    );
    let carols = vm.api_as(&carol());
    let after = contributors_ok(&carols, &vm, APP, &add(&[group("ops"), user("dave")]));
    let list = &after["contributors"];
    assert_eq!(
        keys(list),
        expected(&[
            "user:alice",
            "user:bob",
            "group:finance",
            "group:ops",
            "user:dave"
        ])
    );
    assert_entry(entry_named(list, "user:bob"), "alice", None, None, "bob");
    assert_entry(
        entry_named(list, "group:finance"),
        "alice",
        None,
        None,
        "finance",
    );
    assert_entry(
        entry_named(list, "group:ops"),
        "carol",
        None,
        None,
        "ops, added by carol, a Contributor through finance",
    );
    assert_entry(entry_named(list, "user:dave"), "carol", None, None, "dave");
    assert_eq!(
        after,
        read(&vm, &carol()),
        "the answer is the Application as it is after"
    );
    // Added again, by someone else: each keeps its place, `added_by` and
    // `added_at`.
    let again = contributors_ok(&carols, &vm, APP, &add(&[user("bob"), user("alice")]));
    assert_eq!(again["contributors"], after["contributors"]);
    // Each of them is a Contributor now.
    for each in [
        bob(),
        carol(),
        User::new("oscar").in_groups(&["ops"]),
        User::new("dave"),
    ] {
        assert_eq!(
            read(&vm, &each)["contributor"],
            true,
            "{:?} is a Contributor",
            each.id
        );
    }
}

#[test]
fn a_contributor_removes_people_and_groups_and_they_can_no_longer_deploy() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[user("bob"), group("finance")]));
    vm.api_as(&bob())
        .send_bundle(APP, &marked_bundle("by-bob"))
        .expect_status(201);
    vm.api_as(&carol())
        .send_bundle(APP, &marked_bundle("by-carol"))
        .expect_status(201);
    let after = contributors_ok(&vm.api_as(&bob()), &vm, APP, &remove(&[group("finance")]));
    assert_eq!(
        keys(&after["contributors"]),
        expected(&["user:alice", "user:bob"])
    );
    assert_api_error(
        &vm.api_as(&carol())
            .send_bundle(APP, &marked_bundle("by-carol-again")),
        403,
        "not-a-contributor",
    );
    contributors_ok(&api, &vm, APP, &remove(&[user("bob")]));
    assert_api_error(
        &vm.api_as(&bob())
            .send_bundle(APP, &marked_bundle("by-bob-again")),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &vm.api_as(&bob())
            .change_contributors(APP, &add(&[user("bob")])),
        403,
        "not-a-contributor",
    );
}

#[test]
fn a_contributor_by_a_group_deploys_goes_back_and_reads_logs() {
    let vm = with_board();
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("finance")]));
    let carols = vm.api_as(&carol());
    let sent = carols.send_bundle(APP, &marked_bundle("carols"));
    sent.expect_status(201);
    assert_eq!(sent.json()["number"], 2, "{}", sent.describe());
    assert_eq!(sent.json()["created_by"], "carol");
    let served = vm.get_as(&carol(), APP, "/");
    assert_eq!(served.status, 200, "{}", served.describe());
    let gone_back = go_back(&carols, APP, &from_to(2, None));
    gone_back.expect_status(200);
    assert_eq!(
        gone_back.json()["serving_version"],
        1,
        "{}",
        gone_back.describe()
    );
    let opened = vm.get_as(&carol(), APP, "/hello");
    assert_eq!(opened.status, 200, "{}", opened.describe());
    let entry = blackbox::logs::wait_for_entry(&carols, APP, &opened.request_id()).0;
    assert_eq!(entry["user"], "carol", "{entry}");
    let logs = page(&carols, APP, "?limit=10");
    assert!(!logs.entries.is_empty(), "carol reads the logs");
    carols.versions(APP);
    // Someone it is shared with, who is no Contributor, does none of it.
    let bobs = vm.api_as(&bob());
    blackbox::sharing::share_ok(&carols, &vm, APP, &add(&[user("bob")]));
    assert_api_error(
        &bobs.send_bundle(APP, &marked_bundle("bobs")),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &go_back(&bobs, APP, &from_to(1, None)),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &blackbox::logs::logs(&bobs, APP, ""),
        403,
        "not-a-contributor",
    );
}

#[test]
fn a_contributor_by_a_group_stops_being_one_when_their_groups_no_longer_hold_it() {
    let vm = with_board();
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("finance")]));
    let still = User::new("carol").in_groups(&["finance"]);
    vm.api_as(&still)
        .send_bundle(APP, &marked_bundle("in-finance"))
        .expect_status(201);
    // The stand-in states the Groups at each request: the same person
    // without the Group is no Contributor.
    let left = User::new("carol").in_groups(&["staff"]);
    assert_api_error(
        &vm.api_as(&left).send_bundle(APP, &marked_bundle("left")),
        403,
        "not-a-contributor",
    );
    assert_eq!(
        read(&vm, &alice())["contributors"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn the_last_contributor_cannot_be_removed() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let before = read(&vm, &alice());
    for body in [
        remove(&[user("alice")]),
        add_remove(&[], &[user("alice")]),
        remove(&[user("alice"), user("nobody")]),
    ] {
        let answer = api.change_contributors(APP, &body);
        assert_api_error(&answer, 409, "last-contributor");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains("alice"),
            "expected the refusal to name the entry it is about, got {message:?}"
        );
    }
    // Two Contributors removed at once, leaving none.
    contributors_ok(&api, &vm, APP, &add(&[group("finance")]));
    let both = api.change_contributors(APP, &remove(&[user("alice"), group("finance")]));
    assert_api_error(&both, 409, "last-contributor");
    let after = read(&vm, &alice());
    assert_eq!(
        keys(&after["contributors"]),
        expected(&["user:alice", "group:finance"]),
        "a refused change changes nothing"
    );
    assert_eq!(after["shared_with"], before["shared_with"]);
    // Replacing the last one in the same request leaves one: taken.
    contributors_ok(&api, &vm, APP, &remove(&[group("finance")]));
    let replaced = contributors_ok(
        &api,
        &vm,
        APP,
        &add_remove(&[user("bob")], &[user("alice")]),
    );
    assert_eq!(keys(&replaced["contributors"]), expected(&["user:bob"]));
}

#[test]
fn a_contributor_may_remove_themselves_while_one_is_left() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[user("bob")]));
    let after = contributors_ok(&api, &vm, APP, &remove(&[user("alice")]));
    assert_eq!(keys(&after["contributors"]), expected(&["user:bob"]));
    assert_eq!(
        after["contributor"], false,
        "the answer is as the asker sees it after: alice is no longer a Contributor"
    );
    assert_api_error(
        &api.change_contributors(APP, &add(&[user("alice")])),
        403,
        "not-a-contributor",
    );
    assert_api_error(
        &vm.api_as(&bob())
            .change_contributors(APP, &remove(&[user("bob")])),
        409,
        "last-contributor",
    );
}

#[test]
fn a_contributor_by_a_group_may_remove_their_group_while_one_is_left() {
    let vm = with_board();
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[group("finance")]));
    let after = contributors_ok(&vm.api_as(&carol()), &vm, APP, &remove(&[group("finance")]));
    assert_eq!(keys(&after["contributors"]), expected(&["user:alice"]));
}

#[test]
fn a_platform_administrator_who_is_not_a_contributor_cannot_change_the_contributors() {
    let vm = with_board();
    let admin = vm.api_as(&administrator());
    for body in [
        add(&[user(&administrator().id)]),
        add(&[group(ADMINISTRATORS)]),
        remove(&[user("alice")]),
    ] {
        assert_api_error(
            &admin.change_contributors(APP, &body),
            403,
            "not-a-contributor",
        );
    }
    assert_eq!(
        keys(&read(&vm, &alice())["contributors"]),
        expected(&["user:alice"])
    );
    assert_api_error(
        &admin.send_bundle(APP, &marked_bundle("admins")),
        403,
        "not-a-contributor",
    );
}

#[test]
fn at_most_a_hundred_contributors() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let first: Vec<Value> = (0..MAX_ENTRIES_A_REQUEST - 1)
        .map(|n| user(&format!("c{n:02}")))
        .collect();
    let full = contributors_ok(&api, &vm, APP, &add(&first));
    assert_eq!(
        full["contributors"].as_array().map(Vec::len),
        Some(MAX_CONTRIBUTORS)
    );
    for (body, name) in [
        (add(&[user("one-more")]), "one-more"),
        (add(&[group("finance")]), "finance"),
        (add_remove(&[user("x1"), user("x2")], &[user("c00")]), "x"),
    ] {
        let answer = api.change_contributors(APP, &body);
        assert_api_error(&answer, 409, "too-many");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains(name),
            "expected the refusal to name the entry it is about ({name:?}), got {message:?}"
        );
    }
    assert_eq!(
        read(&vm, &alice()),
        full,
        "a refused change changes nothing"
    );
    let swapped = contributors_ok(
        &api,
        &vm,
        APP,
        &add_remove(&[user("one-more")], &[user("c00")]),
    );
    assert_eq!(
        swapped["contributors"].as_array().map(Vec::len),
        Some(MAX_CONTRIBUTORS)
    );
}

#[test]
fn an_email_nobody_signed_in_with_is_422_unknown_email_for_the_contributors_too() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let answer = api.change_contributors(
        APP,
        &add(&[user("bob"), blackbox::sharing::email("bob@example.test")]),
    );
    assert_api_error(&answer, 422, "unknown-email");
    assert_eq!(
        keys(&read(&vm, &alice())["contributors"]),
        expected(&["user:alice"])
    );
}

#[test]
fn the_log_has_a_line_for_each_change_of_the_contributors() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    contributors_ok(&api, &vm, APP, &add(&[user("bob"), group("finance")]));
    let line = vm.wait_for_record("a `contributors-changed` record", |r| {
        r["event"] == "contributors-changed"
    });
    assert_eq!(line["application"], APP, "{line}");
    assert_eq!(line["by"], "alice", "{line}");
    assert_eq!(
        keys(&line["added"]),
        expected(&["user:bob", "group:finance"])
    );
    assert_eq!(line["removed"], json!([]), "{line}");
    contributors_ok(&api, &vm, APP, &add(&[user("bob")]));
    contributors_ok(
        &vm.api_as(&bob()),
        &vm,
        APP,
        &remove(&[group("finance"), user("zed")]),
    );
    let removal = vm.wait_for_record("the `contributors-changed` record of the removal", |r| {
        r["event"] == "contributors-changed" && r["by"] == "bob"
    });
    assert_eq!(removal["added"], json!([]), "{removal}");
    assert_eq!(
        keys(&removal["removed"]),
        expected(&["group:finance"]),
        "{removal}"
    );
    let lines: Vec<Value> = vm
        .records()
        .into_iter()
        .filter(|r| r["event"] == "contributors-changed")
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "no line for the change that changed nothing: {lines:?}"
    );
}

#[test]
fn a_version_sent_by_a_contributor_added_later_serves() {
    let vm = with_board();
    contributors_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    let sent = vm
        .api_as(&bob())
        .send_version(APP, &version_body(&marked_bundle("bobs")));
    sent.expect_status(201);
    blackbox::api::assert_served_by(&vm, &vm.get_as(&bob(), APP, "/"), APP, "bobs", 2);
}
