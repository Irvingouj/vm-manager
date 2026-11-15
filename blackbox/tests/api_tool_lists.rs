//! What the tool lists, on the platform's API (CONTRACT.md, "The tool:
//! Listing Resources", "Listing Applications"; ticket "The tool signs in,
//! and lists Resources and Applications"): each Resource gains
//! `your_access`, the highest Access level the User holds through their
//! Groups; `GET /v1/applications` lists the Applications the User is a
//! Contributor of, by name.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_json_content_type;
use blackbox::scenario::{Scenario, administrator};
use blackbox::tool::bearer;
use blackbox::{DOMAIN, User, Vm, VmOptions};
use serde_json::{Value, json};

/// The keys of a JSON object, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

const RESOURCE_KEYS: [&str; 8] = [
    "contact",
    "created_at",
    "created_by",
    "description",
    "kind",
    "name",
    "owner",
    "your_access",
];

const APPLICATION_KEYS: [&str; 5] = [
    "address",
    "contributor",
    "description",
    "name",
    "serving_version",
];

/// `alpha` granted to `readers` at read and `writers` at write; `beta` to
/// `analysts` at structure and `readers` at structure; `gamma` to nobody.
fn three_resources() -> Scenario {
    let s = Scenario::start();
    for name in ["gamma", "alpha", "beta"] {
        s.register(name);
    }
    s.grant("alpha", "readers", "read");
    s.grant("alpha", "writers", "write");
    s.grant("beta", "analysts", "structure");
    s.grant("beta", "readers", "structure");
    s
}

/// `your_access` of each listed Resource, by name, failing unless the list
/// is by name and each Resource is its public face and `your_access`.
fn access_listed(listed: &Value) -> Vec<(String, Value)> {
    let resources = listed["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("expected {{ \"resources\": [ ... ] }}, got {listed}"));
    for r in resources {
        assert_eq!(
            keys(r),
            RESOURCE_KEYS,
            "expected each Resource's public face and `your_access`, got {r}"
        );
    }
    resources
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_owned(),
                r["your_access"].clone(),
            )
        })
        .collect()
}

#[test]
fn each_resource_says_the_highest_access_the_user_holds_through_their_groups() {
    let s = three_resources();
    let cases = [
        (
            User::new("wren").in_groups(&["readers", "writers"]),
            [json!("write"), json!("structure"), Value::Null],
        ),
        (
            User::new("rita").in_groups(&["readers"]),
            [json!("read"), json!("structure"), Value::Null],
        ),
        (
            User::new("ana").in_groups(&["analysts", "everyone"]),
            [Value::Null, json!("structure"), Value::Null],
        ),
        (User::new("nobody"), [Value::Null, Value::Null, Value::Null]),
        (administrator(), [Value::Null, Value::Null, Value::Null]),
        // A Group's name is compared exactly.
        (
            User::new("case").in_groups(&["Readers", "WRITERS", "reader"]),
            [Value::Null, Value::Null, Value::Null],
        ),
    ];
    for (user, want) in cases {
        let api = s.vm.api_as(&user);
        let listed = api.list_resources();
        listed.expect_status(200);
        assert_json_content_type(&listed);
        let got = access_listed(&listed.json());
        let expected: Vec<(String, Value)> = ["alpha", "beta", "gamma"]
            .iter()
            .map(|n| (*n).to_owned())
            .zip(want.iter().cloned())
            .collect();
        assert_eq!(
            got, expected,
            "expected every Resource by name with {:?}'s highest Access level (Groups {:?}), got {got:?}",
            user.id, user.groups
        );
        for (name, access) in &expected {
            let one = api.get_resource(name);
            one.expect_status(200);
            let one = one.json();
            assert_eq!(keys(&one), RESOURCE_KEYS, "{one}");
            assert_eq!(
                &one["your_access"], access,
                "GET /v1/resources/{name} as {:?}: {one}",
                user.id
            );
        }
        // The same through a tool credential made for them.
        let (credential, _) = api.tool_credential();
        let through = bearer(&s.vm, "/v1/resources", &credential).send();
        through.expect_status(200);
        assert_eq!(
            access_listed(&through.json()),
            expected,
            "through {:?}'s tool credential",
            user.id
        );
    }
}

#[test]
fn a_grant_raised_or_lowered_shows_at_once() {
    let s = three_resources();
    let rita = User::new("rita").in_groups(&["readers"]);
    s.grant("alpha", "readers", "structure");
    s.grant("gamma", "readers", "write");
    let got = access_listed(&s.vm.api_as(&rita).list_resources().json());
    assert_eq!(
        got,
        vec![
            ("alpha".to_owned(), json!("structure")),
            ("beta".to_owned(), json!("structure")),
            ("gamma".to_owned(), json!("write")),
        ]
    );
}

#[test]
fn get_applications_lists_those_the_user_contributes_to_by_name() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let alice = User::new("alice").in_groups(&["staff"]);
    let bob = User::new("bob").in_groups(&["staff"]);
    let as_alice = vm.api_as(&alice);
    as_alice
        .create_application_with(
            &json!({ "name": "zeta-board", "description": "Where incidents are triaged" }),
        )
        .expect_status(201);
    as_alice.create_application("alpha-tool").expect_status(201);
    as_alice
        .send_bundle("zeta-board", "globalThis.default = {};")
        .expect_status(201);
    as_alice
        .send_bundle("zeta-board", "globalThis.default = {};")
        .expect_status(201);
    vm.api_as(&bob)
        .create_application("bobs-app")
        .expect_status(201);

    let listed = as_alice.list_applications();
    listed.expect_status(200);
    assert_json_content_type(&listed);
    let body = listed.json();
    assert_eq!(
        body,
        json!({ "applications": [
            { "name": "alpha-tool", "description": null, "address": format!("alpha-tool.{DOMAIN}"),
              "contributor": true, "serving_version": null },
            { "name": "zeta-board", "description": "Where incidents are triaged",
              "address": format!("zeta-board.{DOMAIN}"), "contributor": true, "serving_version": 2 },
        ] }),
        "expected alice's Applications, by name, and not bob's"
    );
    for a in body["applications"].as_array().unwrap() {
        assert_eq!(keys(a), APPLICATION_KEYS, "{a}");
    }
    assert_eq!(
        vm.api_as(&bob).list_applications().json(),
        json!({ "applications": [
            { "name": "bobs-app", "description": null, "address": format!("bobs-app.{DOMAIN}"),
              "contributor": true, "serving_version": null },
        ] })
    );
    // Sharing a Group with a Contributor is not being one.
    assert_eq!(
        vm.api_as(&User::new("carol").in_groups(&["staff"]))
            .list_applications()
            .json(),
        json!({ "applications": [] })
    );
    let (credential, _) = as_alice.tool_credential();
    assert_eq!(
        bearer(&vm, "/v1/applications", &credential).send().json(),
        body,
        "the same through alice's tool credential"
    );
    let anonymous = vm.api_request("/v1/applications").send();
    assert_eq!(anonymous.status, 401, "{}", anonymous.describe());
}
