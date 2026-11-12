//! Resources and Grants on the platform's API (CONTRACT.md, "Resources and
//! Grants"; ticket "A User's read of a shared database is authorized and
//! logged": only a Platform administrator can register a Resource or write
//! a Grant). Whether the API ever returns a Resource's credentials is in
//! `credentials.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::api::{assert_json_content_type, assert_recent_rfc3339};
use blackbox::postgres;
use blackbox::scenario::{ADMINISTRATORS, CONTACT, Scenario, administrator, resource_body};
use blackbox::{User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice").in_groups(&["readers", "everyone"])
}

/// The keys of a JSON object, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

const PUBLIC_FACE: [&str; 7] = [
    "contact",
    "created_at",
    "created_by",
    "description",
    "kind",
    "name",
    "owner",
];

#[test]
fn an_administrator_registers_a_resource_and_every_signed_in_user_reads_its_public_face() {
    let s = Scenario::start();
    let body = resource_body("devops", &s.db, &s.db.reader);
    let created = s.admin().create_resource(&body);
    created.expect_status(201);
    assert_json_content_type(&created);
    let resource = created.json();
    assert_eq!(
        keys(&resource),
        PUBLIC_FACE,
        "expected the Resource answered as {{ name, kind, description, owner, contact, created_by, created_at }} and nothing else, got {resource}"
    );
    assert_eq!(resource["name"], "devops");
    assert_eq!(resource["kind"], "postgres");
    assert_eq!(resource["description"], body["description"]);
    assert_eq!(resource["owner"], body["owner"]);
    assert_eq!(resource["contact"], CONTACT);
    assert_eq!(resource["created_by"], administrator().id);
    assert_recent_rfc3339(&resource["created_at"], 120, "the Resource's created_at");

    for reader in [
        administrator(),
        alice(),
        User::new("bob"),
        User::new("ünïcode-user"),
    ] {
        let got = s.vm.api_as(&reader).get_resource("devops");
        got.expect_status(200);
        assert_json_content_type(&got);
        assert_eq!(
            got.json(),
            resource,
            "expected {:?} to read the same Resource as was answered when it was registered",
            reader.id
        );
    }
}

#[test]
fn the_list_of_resources_is_by_name_and_visible_to_every_signed_in_user() {
    let s = Scenario::start();
    for name in ["zeta_db", "alpha", "devops"] {
        s.register(name);
    }
    for reader in [administrator(), alice(), User::new("bob")] {
        let listed = s.vm.api_as(&reader).list_resources();
        listed.expect_status(200);
        let body = listed.json();
        let names: Vec<&str> = body["resources"]
            .as_array()
            .unwrap_or_else(|| panic!("expected {{ \"resources\": [ ... ] }}, got {body}"))
            .iter()
            .map(|r| r["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            ["alpha", "devops", "zeta_db"],
            "expected every Resource, by name, got {body}"
        );
        for r in body["resources"].as_array().unwrap() {
            assert_eq!(
                keys(r),
                PUBLIC_FACE,
                "expected each listed Resource to show its public face only, got {r}"
            );
            assert_eq!(
                *r,
                s.vm.api_as(&reader)
                    .get_resource(r["name"].as_str().unwrap())
                    .json()
            );
        }
    }
}

#[test]
fn a_resource_that_does_not_exist_is_not_found() {
    let s = Scenario::start();
    assert_api_error(
        &s.vm.api_as(&alice()).get_resource("ghost"),
        404,
        "not-found",
    );
    assert_api_error(&s.admin().get_resource("ghost"), 404, "not-found");
    let listed = s.vm.api_as(&alice()).list_resources();
    listed.expect_status(200);
    assert_eq!(listed.json(), json!({ "resources": [] }));
}

#[test]
fn reading_resources_needs_a_signed_in_user() {
    let s = Scenario::start();
    s.register("devops");
    for target in ["/v1/resources", "/v1/resources/devops"] {
        let answer = s.vm.api_request(target).send();
        assert_eq!(
            answer.status,
            401,
            "expected `401` for GET {target} not signed in, got\n{}",
            answer.describe()
        );
    }
}

#[test]
fn only_an_administrator_may_register_a_resource() {
    let s = Scenario::start();
    let body = resource_body("devops", &s.db, &s.db.reader);
    // Not in the Group; in a Group that differs in case, by a look-alike
    // letter, or by a prefix.
    for user in [
        alice(),
        User::new("bob"),
        User::new("carol").in_groups(&["Platform-Admins"]),
        User::new("dave").in_groups(&["platform-аdmins"]),
        User::new("erin").in_groups(&["platform-admin", "platform-admins-x"]),
        // An administrator's id is not what makes one.
        User::new(&administrator().id),
    ] {
        assert_api_error(
            &s.vm.api_as(&user).create_resource(&body),
            403,
            "not-an-administrator",
        );
        // Before anything about the body.
        assert_api_error(
            &s.vm.api_as(&user).post_raw("/v1/resources", "{ not json"),
            403,
            "not-an-administrator",
        );
    }
    let anonymous =
        s.vm.api_request("/v1/resources")
            .method("POST")
            .json(&body)
            .send();
    assert_eq!(
        anonymous.status,
        401,
        "not signed in: {}",
        anonymous.describe()
    );
    assert_api_error(&s.admin().get_resource("devops"), 404, "not-found");
    // A User in the Group among others is one.
    let among_others = User::new("frank").in_groups(&["everyone", ADMINISTRATORS]);
    s.vm.api_as(&among_others)
        .create_resource(&body)
        .expect_status(201);
}

#[test]
fn with_no_administrators_group_nobody_is_an_administrator() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let db = blackbox::shared_database::SharedDatabase::create();
    let body = resource_body("devops", &db, &db.reader);
    for user in [
        administrator(),
        User::new("admin").in_groups(&["admin", "admins", "administrators", ""]),
    ] {
        let api = vm.api_as(&user);
        assert_api_error(&api.create_resource(&body), 403, "not-an-administrator");
        assert_api_error(
            &api.grant("devops", "readers", "read"),
            403,
            "not-an-administrator",
        );
        assert_api_error(&api.list_grants("devops"), 403, "not-an-administrator");
        assert_api_error(&api.operation_log(None), 403, "not-an-administrator");
    }
}

#[test]
fn only_an_administrator_may_write_or_read_grants_even_of_a_resource_that_does_not_exist() {
    let s = Scenario::start();
    s.register("devops");
    for user in [alice(), User::new("carol").in_groups(&["Platform-Admins"])] {
        let api = s.vm.api_as(&user);
        for resource in ["devops", "ghost"] {
            assert_api_error(
                &api.grant(resource, "readers", "read"),
                403,
                "not-an-administrator",
            );
            assert_api_error(&api.list_grants(resource), 403, "not-an-administrator");
            assert_api_error(
                &api.post_raw(&format!("/v1/resources/{resource}/grants"), "{ not json"),
                403,
                "not-an-administrator",
            );
        }
    }
    // Nothing was granted by those attempts.
    let grants = s.admin().list_grants("devops");
    grants.expect_status(200);
    assert_eq!(grants.json(), json!({ "grants": [] }));
    // For the administrator, a Resource that does not exist is not found.
    assert_api_error(
        &s.admin().grant("ghost", "readers", "read"),
        404,
        "not-found",
    );
    assert_api_error(&s.admin().list_grants("ghost"), 404, "not-found");
}

#[test]
fn grants_are_set_replaced_and_listed_by_group() {
    let s = Scenario::start();
    s.register("devops");
    let admin = s.admin();
    let set = admin.grant("devops", "readers", "read");
    set.expect_status(200);
    assert_json_content_type(&set);
    assert_eq!(
        set.json(),
        json!({ "resource": "devops", "group": "readers", "access": "read" }),
        "expected the Grant answered as {{ resource, group, access }}"
    );
    admin.grant("devops", "writers", "write").expect_status(200);
    admin
        .grant("devops", "auditors", "structure")
        .expect_status(200);
    let listed = admin.list_grants("devops");
    listed.expect_status(200);
    assert_eq!(
        listed.json(),
        json!({ "grants": [
            { "group": "auditors", "access": "structure" },
            { "group": "readers", "access": "read" },
            { "group": "writers", "access": "write" },
        ] }),
        "expected the Grants by Group"
    );
    // A later Grant for the same Group replaces the level.
    let replaced = admin.grant("devops", "readers", "structure");
    replaced.expect_status(200);
    assert_eq!(
        replaced.json(),
        json!({ "resource": "devops", "group": "readers", "access": "structure" })
    );
    admin.grant("devops", "writers", "read").expect_status(200);
    assert_eq!(
        admin.list_grants("devops").json(),
        json!({ "grants": [
            { "group": "auditors", "access": "structure" },
            { "group": "readers", "access": "structure" },
            { "group": "writers", "access": "read" },
        ] }),
        "expected each Group once, at its latest level"
    );
}

#[test]
fn groups_in_grants_are_kept_exactly() {
    let s = Scenario::start();
    s.register("devops");
    let admin = s.admin();
    let long = "g".repeat(256);
    let groups = [
        "readers",
        "Readers",
        "rеaders",
        "ünïcode group",
        long.as_str(),
    ];
    for group in groups {
        let answer = admin.grant("devops", group, "read");
        answer.expect_status(200);
        assert_eq!(
            answer.json()["group"],
            group,
            "expected the Group kept exactly as given"
        );
    }
    let listed = admin.list_grants("devops").json();
    let mut got: Vec<&str> = listed["grants"]
        .as_array()
        .unwrap_or_else(|| panic!("expected {{ grants: [...] }}, got {listed}"))
        .iter()
        .map(|g| g["group"].as_str().unwrap_or_default())
        .collect();
    got.sort_unstable();
    let mut want = groups.to_vec();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "expected five different Groups, compared exactly, got {listed}"
    );
}

#[test]
fn a_grant_needs_a_group_of_1_to_256_bytes_without_control_characters_and_a_known_level() {
    let s = Scenario::start();
    s.register("devops");
    let admin = s.admin();
    let too_long = "g".repeat(257);
    for group in [
        "",
        too_long.as_str(),
        "bell\u{7}",
        "new\nline",
        "tab\there",
        "nul\u{0}",
    ] {
        let answer = admin.grant("devops", group, "read");
        assert_eq!(
            answer.status,
            400,
            "expected `400` for the Group {group:?}, got\n{}",
            answer.describe()
        );
    }
    for access in ["admin", "READ", "", "owner"] {
        let answer = admin.grant("devops", "readers", access);
        assert_eq!(
            answer.status,
            400,
            "expected `400` for the Access level {access:?}, got\n{}",
            answer.describe()
        );
    }
    for body in [
        json!({ "group": "readers" }),
        json!({ "access": "read" }),
        json!({ "group": 7, "access": "read" }),
        json!({ "group": "readers", "access": ["read"] }),
    ] {
        assert_api_error(
            &admin.post("/v1/resources/devops/grants", &body),
            400,
            "malformed",
        );
    }
    assert_eq!(
        admin.list_grants("devops").json(),
        json!({ "grants": [] }),
        "expected none of those to set a Grant"
    );
}

#[test]
fn valid_resource_names_are_accepted() {
    let s = Scenario::start();
    let longest = format!("a{}", "b".repeat(62));
    for name in [
        "a",
        "devops",
        "x9_",
        "a_1_b",
        "incidents2026",
        longest.as_str(),
    ] {
        let created = s
            .admin()
            .create_resource(&resource_body(name, &s.db, &s.db.reader));
        created.expect_status(201);
        assert_eq!(created.json()["name"], name);
        s.vm.api_as(&alice()).get_resource(name).expect_status(200);
    }
}

#[test]
fn invalid_resource_names_are_refused() {
    let s = Scenario::start();
    let too_long = format!("a{}", "b".repeat(63));
    for name in [
        "",
        "Devops",
        "DEVOPS",
        "1devops",
        "_devops",
        "dev-ops",
        "dev ops",
        " devops",
        "devops ",
        "dev.ops",
        "dévops",
        "devops\n",
        too_long.as_str(),
    ] {
        assert_api_error(
            &s.admin()
                .create_resource(&resource_body(name, &s.db, &s.db.reader)),
            400,
            "invalid-name",
        );
    }
    let listed = s.admin().list_resources().json();
    assert_eq!(listed, json!({ "resources": [] }), "nothing was registered");
}

#[test]
fn a_taken_name_is_refused() {
    let s = Scenario::start();
    s.register("devops");
    let mut again = resource_body("devops", &s.db, &s.db.careless);
    again["description"] = json!("another");
    assert_api_error(&s.admin().create_resource(&again), 409, "name-taken");
    assert_ne!(
        s.admin().get_resource("devops").json()["description"],
        "another",
        "expected the first registration to stand"
    );
}

#[test]
fn every_required_field_is_required_and_typed() {
    let s = Scenario::start();
    let full = resource_body("devops", &s.db, &s.db.reader);
    let mut bodies = Vec::new();
    for field in [
        "name",
        "kind",
        "description",
        "owner",
        "contact",
        "connection",
    ] {
        let mut body = full.clone();
        body.as_object_mut().unwrap().remove(field);
        bodies.push((format!("without `{field}`"), body));
    }
    for field in ["host", "port", "database", "read"] {
        let mut body = full.clone();
        body["connection"].as_object_mut().unwrap().remove(field);
        bodies.push((format!("without `connection.{field}`"), body));
    }
    for field in ["role", "password"] {
        let mut body = full.clone();
        body["connection"]["read"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        bodies.push((format!("without `connection.read.{field}`"), body));
    }
    for (what, value) in [
        ("description", json!(7)),
        ("owner", json!(["x"])),
        ("contact", json!({ "who": "x" })),
        ("connection", json!("postgres://somewhere")),
    ] {
        let mut body = full.clone();
        body[what] = value;
        bodies.push((format!("with `{what}` of the wrong type"), body));
    }
    let mut body = full.clone();
    body["connection"]["port"] = json!("5432");
    bodies.push(("with the port as a string".to_owned(), body));
    for (what, body) in bodies {
        let answer = s.admin().create_resource(&body);
        assert_eq!(
            answer.status,
            400,
            "expected `400` for a Resource {what}, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    let mut other_kind = full;
    other_kind["kind"] = json!("mysql");
    let answer = s.admin().create_resource(&other_kind);
    assert_eq!(
        answer.status,
        400,
        "expected `400` for a Resource of kind \"mysql\", got\n{}",
        answer.describe()
    );
    assert_api_error(&s.admin().get_resource("devops"), 404, "not-found");
}

#[test]
fn registering_does_not_connect_to_the_database() {
    let s = Scenario::start();
    // Nothing listens on this port, and the database and login do not exist.
    let port = postgres::free_port().unwrap();
    let body = json!({
        "name": "unreachable",
        "kind": "postgres",
        "description": "Not there yet",
        "owner": "Nobody",
        "contact": "Nobody",
        "connection": {
            "host": "127.0.0.1",
            "port": port,
            "database": "no_such_database",
            "read": { "role": "no_such_role", "password": "no such password" },
        },
    });
    let began = Instant::now();
    s.admin().create_resource(&body).expect_status(201);
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "expected registering to answer at once, without trying the database; it took {:?}",
        began.elapsed()
    );
    s.vm.api_as(&alice())
        .get_resource("unreachable")
        .expect_status(200);
}
