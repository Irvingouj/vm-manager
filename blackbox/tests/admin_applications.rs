//! Every Application, on the platform's API (CONTRACT.md, "What Platform
//! administrators see and stop: Every Application, on the platform's
//! API"): `GET /v1/admin/applications` holds every Application of the
//! installation exactly once (one never shared, one with no Version, one
//! disabled), by name in byte order, a page at a time with `limit` and
//! `after`, each with its Declared access, its Contributors, whom it is
//! shared with as counts, its serving and latest Version, whether it is
//! disabled, and its usage; malformed parameters and other methods are
//! refused.
//!
//! Acceptance criterion: "The list holds every Application, including ones
//! never shared, with Declared access, Contributors, serving Version, and
//! requests and Users over a recent period."

use blackbox::admin::{
    self, LIST, administrator, disable_ok, every_application, list, list_page, listed, names,
};
use blackbox::api::{assert_recent_rfc3339, marked_bundle, version_body_declaring};
use blackbox::logs::{from_to, go_back};
use blackbox::sharing::{add, contributors_ok, everyone, group, share_ok, user};
use blackbox::{DOMAIN, User, Vm, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// Creates `name` as alice, shared with nobody; with a first Version
/// (`fixtures/honest.js`) when `version` is true.
fn create(vm: &Vm, name: &str, version: bool) {
    let api = vm.api_as(&alice());
    api.create_application(name).expect_status(201);
    if version {
        api.send_bundle(name, &fixture_bundle("honest.js"))
            .expect_status(201);
    }
}

#[test]
fn every_application_is_listed_once_by_name_shared_or_not_with_a_version_or_not_disabled_or_not() {
    let vm = admin::start();
    // Made out of order; byte order puts digits before letters and `-`
    // before digits.
    for (name, version) in [
        ("never-shared", true),
        ("no-version", false),
        ("9lives", true),
        ("a-b", true),
        ("a1", true),
        ("shared", true),
        ("stopped", true),
    ] {
        create(&vm, name, version);
    }
    let api = vm.api_as(&alice());
    api.share_with_everyone("shared");
    share_ok(&api, &vm, "stopped", &add(&[group("staff")]));
    disable_ok(&vm, "stopped");

    let page = list_page(&vm, "");
    let applications = page["applications"].as_array().unwrap().clone();
    assert_eq!(
        names(&applications),
        [
            "9lives",
            "a-b",
            "a1",
            "never-shared",
            "no-version",
            "shared",
            "stopped"
        ],
        "expected every Application once, by name in byte order"
    );
    assert!(page["next"].is_null(), "{page}");
    let stopped = &applications[6];
    admin::assert_disabled_member(&stopped["disabled"], &administrator().id, "stopped");
    for a in &applications[..6] {
        assert!(a["disabled"].is_null(), "{a}");
    }
    let no_version = &applications[4];
    assert!(
        no_version["serving_version"].is_null()
            && no_version["latest_version"].is_null()
            && no_version["declared_access"].is_null(),
        "an Application with no Version: no Version and no Declared access, got {no_version}"
    );
    assert_eq!(no_version["declared_more"], 0, "{no_version}");
    let never = &applications[3];
    assert_eq!(
        never["shared_with"],
        json!({ "everyone": false, "groups": 0, "people": 0 }),
        "{never}"
    );

    // An administrator's own list is unchanged: theirs, as anyone's.
    let own = vm.api_as(&administrator()).list_applications();
    own.expect_status(200);
    let own_names: Vec<String> = own.json()["applications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        own_names,
        ["shared"],
        "the administrator's own GET /v1/applications: only what is shared with them"
    );
}

#[test]
fn each_member_says_what_the_contract_says() {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_application_with(
        &json!({ "name": "board", "description": "Where incidents are triaged" }),
    )
    .expect_status(201);
    api.send_version(
        "board",
        &version_body_declaring(&marked_bundle("v1"), &[("devops", "read")]),
    )
    .expect_status(201);
    api.send_version(
        "board",
        &version_body_declaring(&marked_bundle("v2"), &[("devops", "write"), ("hr", "read")]),
    )
    .expect_status(201);
    go_back(&api, "board", &from_to(2, Some(1))).expect_status(200);
    contributors_ok(
        &api,
        &vm,
        "board",
        &add(&[group("board-team"), user("bob")]),
    );
    share_ok(
        &api,
        &vm,
        "board",
        &add(&[everyone(), group("finance"), user("carol"), user("dave")]),
    );

    let board = listed(&vm, "board");
    assert_eq!(board["name"], "board");
    assert_eq!(board["description"], "Where incidents are triaged");
    assert_eq!(board["address"], format!("board.{DOMAIN}"));
    assert_eq!(board["created_by"], "alice");
    assert_recent_rfc3339(&board["created_at"], 300, "created_at");
    assert_eq!(board["serving_version"], 1, "{board}");
    assert_eq!(board["latest_version"], 2, "{board}");
    assert_eq!(
        board["declared_access"],
        json!({ "resources": { "devops": { "kind": "postgres", "access": "read" } } }),
        "expected the serving Version's Declared access (Version 1's, after going back), got {board}"
    );
    assert_eq!(board["declared_more"], 0);
    let read = vm.api_as(&administrator()).application("board");
    assert_eq!(
        board["contributors"], read["contributors"],
        "expected the Contributors as the API answers them anywhere, in the order of the list"
    );
    blackbox::sharing::assert_list_members(&board["contributors"], "contributors");
    assert_eq!(
        board["shared_with"],
        json!({ "everyone": true, "groups": 1, "people": 2 }),
        "expected whom it is shared with as counts"
    );
    assert!(board["disabled"].is_null(), "{board}");
    let usage = &board["usage"];
    let sql = admin::platform_sql(&vm);
    assert_eq!(
        usage,
        &json!({ "days": 30, "from": admin::utc_day(&sql, 29), "requests": 0, "users": 0 }),
        "an Application nobody used: 0 requests by 0 Users over the last 30 UTC days"
    );
    // `serving_version` and `latest_version` are those of the object.
    assert_eq!(board["serving_version"], read["serving_version"]);
    assert_eq!(board["latest_version"], read["latest_version"]);
}

#[test]
fn at_most_200_declared_resources_are_listed_and_declared_more_counts_the_rest() {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_application("wide").expect_status(201);
    let names: Vec<String> = (0..203).rev().map(|i| format!("r{i:03}")).collect();
    let declared: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "read")).collect();
    api.send_version(
        "wide",
        &version_body_declaring(&fixture_bundle("honest.js"), &declared),
    )
    .expect_status(201);
    let wide = listed(&vm, "wide");
    let listed_names: Vec<&String> = wide["declared_access"]["resources"]
        .as_object()
        .unwrap_or_else(|| panic!("expected declared_access.resources, got {wide}"))
        .keys()
        .collect();
    let mut want: Vec<String> = (0..200).map(|i| format!("r{i:03}")).collect();
    want.sort();
    let mut got: Vec<String> = listed_names.into_iter().cloned().collect();
    got.sort();
    assert_eq!(got, want, "expected the first 200 Resources by name");
    assert_eq!(wide["declared_more"], 3, "{}", wide["declared_more"]);
    // The whole is the Version's, which an administrator may read.
    let whole = vm.api_as(&administrator()).get_version("wide", 1);
    whole.expect_status(200);
}

#[test]
fn limit_and_after_page_through_every_application_once() {
    let vm = admin::start();
    for name in ["e", "c", "a", "g", "b", "f", "d"] {
        create(&vm, name, false);
    }
    let (all, pages) = every_application(&vm, 3);
    assert_eq!(names(&all), ["a", "b", "c", "d", "e", "f", "g"]);
    assert_eq!(
        pages,
        vec![
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            vec!["d".to_owned(), "e".to_owned(), "f".to_owned()],
            vec!["g".to_owned()],
        ]
    );
    let first = list_page(&vm, "?limit=3");
    assert_eq!(first["next"], "c", "`next` is the last name answered");
    // `next` is null when nothing comes after the last one answered, also
    // when the page is full.
    let last_full = list_page(&vm, "?limit=3&after=d");
    assert_eq!(
        names(last_full["applications"].as_array().unwrap()),
        ["e", "f", "g"]
    );
    assert!(last_full["next"].is_null(), "{last_full}");
    // `after` need not be a name an Application has now.
    let between = list_page(&vm, "?after=c-not-there");
    assert_eq!(
        names(between["applications"].as_array().unwrap()),
        ["d", "e", "f", "g"]
    );
    let past = list_page(&vm, "?after=zzz");
    assert_eq!(past, json!({ "applications": [], "next": null }));

    // Made meanwhile, after the cursor: in the next reading's answer.
    let page = list_page(&vm, "?limit=2");
    assert_eq!(page["next"], "b");
    create(&vm, "bb", false);
    create(&vm, "0early", false);
    let next = list_page(&vm, "?limit=2&after=b");
    assert_eq!(
        names(next["applications"].as_array().unwrap()),
        ["bb", "c"],
        "an Application made after the cursor is in the next reading"
    );
    let (all, _) = every_application(&vm, 4);
    assert_eq!(
        names(&all),
        ["0early", "a", "b", "bb", "c", "d", "e", "f", "g"]
    );
}

#[test]
fn without_limit_a_page_holds_100_and_100_is_the_most() {
    let vm = admin::start();
    let made: Vec<String> = (0..101).map(|i| format!("app-{i:03}")).collect();
    for name in &made {
        create(&vm, name, false);
    }
    let page = list_page(&vm, "");
    assert_eq!(page["applications"].as_array().unwrap().len(), 100);
    assert_eq!(page["next"], "app-099");
    let rest = list_page(&vm, "?after=app-099");
    assert_eq!(names(rest["applications"].as_array().unwrap()), ["app-100"]);
    assert!(rest["next"].is_null());
    assert_eq!(
        list_page(&vm, "?limit=100")["applications"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    assert_api_error(&list(&vm, &administrator(), "?limit=101"), 400, "malformed");
    let (all, _) = every_application(&vm, 100);
    assert_eq!(names(&all), made);
}

#[test]
fn malformed_parameters_are_refused_and_other_methods_are_not_allowed() {
    let vm = admin::start();
    create(&vm, "board", true);
    let long = "a".repeat(64);
    for query in [
        "?limit=0".to_owned(),
        "?limit=101".to_owned(),
        "?limit=-1".to_owned(),
        "?limit=%2B5".to_owned(),
        "?limit=05".to_owned(),
        "?limit=abc".to_owned(),
        "?limit=".to_owned(),
        "?limit=1.5".to_owned(),
        "?limit=%201".to_owned(),
        "?limit=5&limit=5".to_owned(),
        "?after=Board".to_owned(),
        "?after=-board".to_owned(),
        "?after=board-".to_owned(),
        "?after=bo_ard".to_owned(),
        "?after=".to_owned(),
        format!("?after={long}"),
        "?after=b&after=c".to_owned(),
    ] {
        assert_api_error(&list(&vm, &administrator(), &query), 400, "malformed");
    }
    // A 63-character name is one an Application could have.
    let longest = "a".repeat(63);
    assert_eq!(
        list_page(&vm, &format!("?after={longest}"))["applications"],
        json!([])
    );
    // Other parameters are ignored.
    let ignored = list_page(&vm, "?page=2&sort=desc&limit=5");
    assert_eq!(
        names(ignored["applications"].as_array().unwrap()),
        ["board"]
    );
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let answer = vm
            .api_as(&administrator())
            .request(LIST)
            .method(method)
            .json(&json!({}))
            .send();
        assert_api_error(&answer, 405, "method-not-allowed");
    }
}

#[test]
fn the_list_is_read_from_the_platform_database_and_survives_a_restart() {
    let vm = admin::start();
    create(&vm, "kept", true);
    disable_ok(&vm, "kept");
    let before: Value = listed(&vm, "kept");
    let vm = vm.restart();
    let after = listed(&vm, "kept");
    assert_eq!(
        before, after,
        "expected the same Application, disabled as it was, after a restart"
    );
}
