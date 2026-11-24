//! What a Platform administrator sees of Resources (CONTRACT.md, "Resources
//! and Grants in full: Listing, for administrators"): `GET
//! /v1/admin/resources` and `GET /v1/admin/resources/<name>` with `usable`,
//! `problem`, `logins`, `location`, `grants` and `grants_more`; the pages;
//! every word of `problem`; never a credential, host, port, database or
//! role; `GET /v1/resources` for a User without the new members; the 1,001st
//! Resource; answered from memory with the platform database unreachable.
//!
//! Resources beyond a handful are rows inserted by SQL into the platform
//! database before the program starts (registering 1,000 one by one through
//! the API takes minutes); the program seals them at its start as it does
//! any clear row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::api::{assert_api_error, assert_recent_rfc3339};
use blackbox::lost_answer::HeldPostgres;
use blackbox::resources::{
    admin_resource, admin_resources, body, connection, found_in, named_login, not_served,
    reader_body, secrets_of,
};
use blackbox::scenario::{READERS, Scenario, WRITERS, administrator};
use blackbox::sealed::{DEVOPS, Sealed, Stored};
use blackbox::shared_database::{SharedDatabase, find_port, token};
use blackbox::storage::{FILES, MARKETING, Storage, get, wanda};
use blackbox::{Response, User, VmOptions};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// The names of a page, in order.
fn names(page: &Value) -> Vec<String> {
    page["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `resources` in {page}"))
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_owned())
        .collect()
}

fn keys(value: &Value) -> Vec<String> {
    let mut all: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    all.sort();
    all
}

#[test]
fn a_database_resource_is_listed_with_its_logins_grants_and_no_location() {
    let s = Scenario::start();
    let db = &s.db;
    let structure = blackbox::resources::login_holding_nothing(db);
    s.register_body(
        "devops",
        &body(
            "devops",
            &connection(db, &db.reader, Some(&db.writer), Some(&structure)),
        ),
    );
    s.register("plain");
    s.grant("devops", WRITERS, "write");
    s.grant("devops", "auditors", "read");
    s.grant("devops", "sre", "structure");
    let answer = admin_resources(&s.admin(), "");
    answer.expect_status(200);
    let page = answer.json();
    assert_eq!(
        keys(&page),
        vec!["next", "resources"],
        "{}",
        answer.describe()
    );
    assert_eq!(page["next"], Value::Null);
    assert_eq!(names(&page), vec!["devops", "plain"], "by name");
    let devops = &page["resources"][0];
    assert_eq!(
        keys(devops),
        vec![
            "contact",
            "created_at",
            "created_by",
            "description",
            "grants",
            "grants_more",
            "kind",
            "location",
            "logins",
            "name",
            "owner",
            "problem",
            "usable"
        ],
        "{devops}"
    );
    assert_eq!(devops["kind"], "postgres");
    assert_eq!(devops["usable"], json!(true));
    assert_eq!(devops["problem"], Value::Null);
    assert_eq!(
        devops["logins"],
        json!({ "read": true, "write": true, "structure": true })
    );
    assert_eq!(devops["location"], Value::Null);
    assert_eq!(
        blackbox::resources::without_who_and_when(&devops["grants"]),
        json!([
            { "group": "auditors", "access": "read" },
            { "group": "sre", "access": "structure" },
            { "group": WRITERS, "access": "write" },
        ]),
        "the Grants by Group, in byte order"
    );
    assert_eq!(devops["grants_more"], json!(0));
    assert_eq!(devops["created_by"], administrator().id);
    assert_recent_rfc3339(&devops["created_at"], 60, "created_at");
    assert_eq!(
        page["resources"][1]["logins"],
        json!({ "read": true, "write": false, "structure": false })
    );
    assert_eq!(page["resources"][1]["grants"], json!([]));
    // One Resource, not wrapped in a list.
    let one = admin_resource(&s.admin(), "devops");
    one.expect_status(200);
    assert_eq!(&one.json(), devops, "the one object is the one of the list");
    assert!(one.json().get("resources").is_none());
}

#[test]
fn a_storage_resource_is_listed_with_its_location_and_no_logins() {
    let s = Storage::start();
    s.register(MARKETING);
    s.register_postgres("devops");
    s.grant(MARKETING, "designers", "write");
    let answer = admin_resources(&s.admin(), "");
    answer.expect_status(200);
    let page = answer.json();
    assert_eq!(names(&page), vec!["devops", "marketing"]);
    let storage = &page["resources"][1];
    assert_eq!(storage["kind"], "storage");
    assert_eq!(storage["usable"], json!(true), "{storage}");
    assert_eq!(storage["problem"], Value::Null);
    assert_eq!(storage["logins"], Value::Null);
    assert_eq!(
        storage["location"],
        json!({ "backend": "directory", "directory": s.dir(MARKETING).to_string_lossy() }),
        "{storage}"
    );
    assert_eq!(
        blackbox::resources::without_who_and_when(&storage["grants"]),
        json!([{ "group": "designers", "access": "write" }])
    );
    let database = &page["resources"][0];
    assert_eq!(database["location"], Value::Null);
    assert_eq!(
        database["logins"],
        json!({ "read": true, "write": false, "structure": false })
    );
    let one = admin_resource(&s.admin(), MARKETING);
    one.expect_status(200);
    assert_eq!(&one.json(), storage);
}

#[test]
fn a_storage_resource_whose_directory_cannot_be_used_is_not_usable_and_says_why() {
    let s = Storage::ready();
    std::fs::remove_dir_all(s.dir(MARKETING)).unwrap();
    // The program finds out as it uses the directory.
    let _ = s.one(&wanda(), FILES, &get(MARKETING, "anything"));
    let answer = admin_resource(&s.admin(), MARKETING);
    answer.expect_status(200);
    let resource = answer.json();
    assert_eq!(resource["usable"], json!(false), "{resource}");
    let problem = resource["problem"].as_str().unwrap_or_else(|| {
        panic!("expected a problem word for a directory that is gone: {resource}")
    });
    assert!(
        problem.starts_with("directory-"),
        "expected one of Storage's words for a directory that cannot be used, got {problem:?}"
    );
    assert_eq!(resource["logins"], Value::Null);
    assert_eq!(resource["location"]["backend"], "directory", "{resource}");
}

#[test]
fn at_most_two_hundred_grants_are_shown_and_the_rest_counted() {
    let s = Scenario::start();
    s.register("devops");
    for n in 0..201 {
        s.grant("devops", &format!("group{n:03}"), "read");
    }
    let one = admin_resource(&s.admin(), "devops").json();
    let grants = one["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 200, "{}", one["grants_more"]);
    assert_eq!(one["grants_more"], json!(1));
    assert_eq!(grants[0]["group"], "group000");
    assert_eq!(grants[199]["group"], "group199", "the first 200 by Group");
    let listed = admin_resources(&s.admin(), "").json();
    assert_eq!(
        listed["resources"][0]["grants"].as_array().unwrap().len(),
        200
    );
    assert_eq!(listed["resources"][0]["grants_more"], json!(1));
}

#[test]
fn pages_are_by_name_with_limit_after_and_next() {
    let s = Scenario::start();
    for name in ["delta", "alpha", "echo", "charlie", "bravo"] {
        s.register(name);
    }
    let api = s.admin();
    let all = admin_resources(&api, "").json();
    assert_eq!(
        names(&all),
        vec!["alpha", "bravo", "charlie", "delta", "echo"]
    );
    assert_eq!(all["next"], Value::Null);
    let first = admin_resources(&api, "?limit=2").json();
    assert_eq!(names(&first), vec!["alpha", "bravo"]);
    assert_eq!(
        first["next"], "bravo",
        "the name of the last Resource answered"
    );
    let second = admin_resources(&api, "?limit=2&after=bravo").json();
    assert_eq!(names(&second), vec!["charlie", "delta"]);
    assert_eq!(second["next"], "delta");
    let third = admin_resources(&api, "?after=delta&limit=2").json();
    assert_eq!(names(&third), vec!["echo"]);
    assert_eq!(third["next"], Value::Null, "nothing comes after the last");
    // A limit that takes all of them and no more has no `next`.
    let exact = admin_resources(&api, "?limit=5").json();
    assert_eq!(exact["next"], Value::Null);
    // `after` need not be a Resource's now.
    assert_eq!(
        names(&admin_resources(&api, "?after=b").json()),
        vec!["bravo", "charlie", "delta", "echo"]
    );
    let none = admin_resources(&api, "?after=zzz").json();
    assert_eq!(none, json!({ "resources": [], "next": null }));
    // Other parameters are ignored; limits at their edges.
    assert_eq!(
        names(&admin_resources(&api, "?colour=red&limit=1").json()),
        vec!["alpha"]
    );
    admin_resources(&api, "?limit=100").expect_status(200);
    admin_resources(&api, "?limit=1").expect_status(200);
    for bad in [
        "?limit=0",
        "?limit=101",
        "?limit=-1",
        "?limit=abc",
        "?limit=",
        "?limit=1.5",
        "?limit=1&limit=2",
        "?after=Not_A_Name",
        "?after=",
        "?after=a&after=b",
        "?after=Alpha",
    ] {
        let answer = admin_resources(&api, bad);
        assert_api_error(&answer, 400, "malformed");
    }
}

#[test]
fn a_hundred_to_a_page_and_a_thousand_resources_at_most() {
    // A platform database the program made, then 999 clear rows by SQL.
    let platform = {
        let s = Sealed::start(&VmOptions::new());
        s.platform().clone()
    };
    let admin = Stored::of(&platform).admin();
    let connection = json!({
        "host": "bulk.invalid", "port": 5432, "database": "bulk",
        "read": { "role": "bulk_reader", "password": format!("pw-bulk-{}", token()) },
        "write": null, "structure": null,
    })
    .to_string();
    admin.batch(&format!(
        "INSERT INTO resources (name, kind, description, owner, contact, connection, created_by)
         SELECT 'bulk_' || g, 'postgres', 'bulk', 'bulk', 'bulk',
                convert_to('{connection}', 'UTF8'), 'harness'
           FROM generate_series(1, 999) g"
    ));
    let s = Sealed::start_on(&platform, SharedDatabase::create(), &VmOptions::new());
    let api = s.admin();
    // The pages: 100 each, in byte order, none twice, `next` until the end.
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let query = after
            .as_deref()
            .map_or(String::new(), |a| format!("?after={a}"));
        let page = admin_resources(&api, &query).json();
        let found = names(&page);
        assert!(found.len() <= 100, "100 to a page, got {}", found.len());
        pages += 1;
        seen.extend(found);
        match page["next"].as_str() {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
        assert!(pages < 20, "the pages never end");
    }
    assert_eq!(seen.len(), 999);
    assert_eq!(pages, 10, "999 Resources are ten pages of at most 100");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(seen, sorted, "by name in byte order, each once");
    // The 1,000th is registered; the 1,001st is `too-many`, and a name that
    // is taken is `name-taken` even then.
    let one_more = s.admin().create_resource(&reader_body("one_more", &s.db));
    one_more.expect_status(201);
    let too_many = s.admin().create_resource(&reader_body("two_more", &s.db));
    assert_api_error(&too_many, 409, "too-many");
    let taken = s.admin().create_resource(&reader_body("one_more", &s.db));
    assert_api_error(&taken, 409, "name-taken");
    let taken_bulk = s.admin().create_resource(&reader_body("bulk_1", &s.db));
    assert_api_error(&taken_bulk, 409, "name-taken");
    s.admin().get_resource("two_more").expect_status(404);
    // The list still has 1,000.
    let mut total = 0;
    let mut after: Option<String> = None;
    loop {
        let query = after
            .as_deref()
            .map_or(String::new(), |a| format!("?after={a}"));
        let page = admin_resources(&s.admin(), &query).json();
        total += names(&page).len();
        match page["next"].as_str() {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(total, 1000);
}

#[test]
fn a_resource_that_is_not_served_says_why_in_the_list() {
    for problem in ["no-secret-key", "does-not-open", "not-sealed"] {
        let s = not_served(problem);
        let started = Instant::now();
        let one = loop {
            let answer = admin_resource(&s.admin(), DEVOPS);
            if answer.status == 200 {
                break answer.json();
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{problem}: expected the Resource to be listed once its Grants are known, got {}",
                answer.describe()
            );
            std::thread::sleep(Duration::from_millis(500));
        };
        assert_eq!(one["usable"], json!(false), "{problem}: {one}");
        assert_eq!(one["problem"], problem, "{one}");
        assert_eq!(
            one["logins"],
            Value::Null,
            "{problem}: what is sealed cannot be read"
        );
        assert_eq!(one["location"], Value::Null);
        assert_eq!(one["name"], DEVOPS);
        assert_eq!(
            blackbox::resources::without_who_and_when(&one["grants"]),
            json!([{ "group": READERS, "access": "read" }])
        );
        let listed = admin_resources(&s.admin(), "").json();
        assert_eq!(
            listed["resources"][0], one,
            "{problem}: the list and the one agree"
        );
    }
}

#[test]
fn the_list_has_no_credential_host_port_database_or_role_and_no_location_but_for_administrators() {
    let s = Scenario::start();
    let db = &s.db;
    let read = named_login(
        db,
        &format!("role_marker_read_{}", token()),
        &format!("password-marker-read-{}", token()),
    );
    let write = named_login(
        db,
        &format!("role_marker_write_{}", token()),
        &format!("password-marker-write-{}", token()),
    );
    s.register_body(
        "devops",
        &body("devops", &connection(db, &read, Some(&write), None)),
    );
    s.grant("devops", READERS, "read");
    let secrets = secrets_of(db, &[&read, &write]);
    let mut answers: Vec<Response> = vec![
        admin_resources(&s.admin(), ""),
        admin_resource(&s.admin(), "devops"),
        admin_resource(&s.admin(), "ghost"),
        admin_resources(&s.admin(), "?limit=0"),
        s.admin().list_resources(),
        s.admin().get_resource("devops"),
        s.admin().list_grants("devops"),
        s.vm.api_as(&rita()).list_resources(),
        s.vm.api_as(&rita()).get_resource("devops"),
        admin_resource(&s.vm.api_as(&rita()), "devops"),
    ];
    answers.push(admin_resources(&s.vm.api_as(&rita()), ""));
    // The harness's own check that the administrators' answers are there to
    // search: the list and the one are `200`, the ghost `404`.
    answers[0].expect_status(200);
    answers[1].expect_status(200);
    answers[2].expect_status(404);
    assert!(answers[1].describe().contains("devops"));
    for answer in &answers {
        let text = answer.describe();
        assert!(
            found_in(&secrets, &text).is_empty(),
            "expected no part of the connection in the answer to {}, found {:?}",
            answer.request(),
            found_in(&secrets, &text)
        );
        assert!(
            !find_port(&text, db.port()),
            "no port in the answer to {}",
            answer.request()
        );
    }
    for line in s.vm.lines() {
        assert!(
            found_in(&secrets, &line.raw).is_empty(),
            "no part of the connection in the log"
        );
    }
}

#[test]
fn the_directory_is_in_the_administrators_answers_and_nowhere_else() {
    let s = Storage::start();
    s.register(MARKETING);
    s.grant(MARKETING, "designers", "write");
    let directory = s.dir(MARKETING).to_string_lossy().into_owned();
    let admin = s.admin();
    let shown = [
        admin_resources(&admin, ""),
        admin_resource(&admin, MARKETING),
    ];
    for answer in &shown {
        assert!(
            answer.describe().contains(&directory),
            "the administrators see the location"
        );
    }
    let nowhere = [
        admin.list_resources(),
        admin.get_resource(MARKETING),
        admin.list_grants(MARKETING),
        s.vm.api_as(&rita()).list_resources(),
        s.vm.api_as(&rita()).get_resource(MARKETING),
        admin_resource(&s.vm.api_as(&rita()), MARKETING),
        admin_resources(&s.vm.api_as(&rita()), ""),
    ];
    for answer in &nowhere {
        assert!(
            !answer.describe().contains(&directory),
            "expected no directory in the answer to {}",
            answer.request()
        );
    }
    for line in s.vm.lines() {
        assert!(
            !line.raw.contains(&directory),
            "no directory in the program's log: {}",
            line.raw
        );
    }
}

#[test]
fn what_every_user_sees_of_a_resource_has_none_of_the_new_members() {
    let s = Scenario::start();
    s.register_writable("devops");
    s.grant("devops", READERS, "read");
    let new = [
        "logins",
        "location",
        "usable",
        "problem",
        "grants",
        "grants_more",
    ];
    for user in [rita(), administrator()] {
        let api = s.vm.api_as(&user);
        let list = api.list_resources().json();
        let one = api.get_resource("devops").json();
        let listed = list["resources"]
            .as_array()
            .unwrap_or_else(|| panic!("a list of Resources: {list}"));
        for resource in listed.iter().chain([&one]) {
            for member in new {
                assert!(
                    resource.get(member).is_none(),
                    "{}: expected `{member}` in no answer of `GET /v1/resources`, got {resource}",
                    user.id
                );
            }
        }
    }
}

#[test]
fn nothing_is_found_that_is_not_there_and_the_refusals_come_in_order() {
    let s = Scenario::start();
    s.register("devops");
    let api = s.admin();
    assert_api_error(&admin_resource(&api, "ghost"), 404, "not-found");
    let odd = admin_resource(&api, "Not_A_Name");
    assert_api_error(&odd, 404, "not-found");
    // Only GET.
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let answer = blackbox::resources::call(&api, method, "/v1/admin/resources", None);
        assert_api_error(&answer, 405, "method-not-allowed");
        let answer = blackbox::resources::call(&api, method, "/v1/admin/resources/devops", None);
        assert_api_error(&answer, 405, "method-not-allowed");
    }
}

#[test]
fn it_is_answered_from_memory_within_a_second_with_the_platform_database_unreachable() {
    let held = HeldPostgres::start();
    let platform = held.platform("vm_platform_admin_list_memory");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    s.register("devops");
    s.grant("devops", READERS, "read");
    // Nothing connects to the platform database any more, and what is
    // connected is ended.
    let server = held.superuser();
    server.batch("ALTER DATABASE vm_platform_admin_list_memory WITH ALLOW_CONNECTIONS false");
    server.batch(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
          WHERE datname = 'vm_platform_admin_list_memory'",
    );
    std::thread::sleep(Duration::from_millis(500));
    for path in ["", "/devops"] {
        let began = Instant::now();
        let answer = blackbox::resources::call(
            &s.admin(),
            "GET",
            &format!("/v1/admin/resources{path}"),
            None,
        );
        let took = began.elapsed();
        answer.expect_status(200);
        assert!(
            took < Duration::from_secs(1),
            "expected the answer from memory within 1 second, it took {took:?}"
        );
        assert!(answer.describe().contains("devops"));
    }
    server.batch("ALTER DATABASE vm_platform_admin_list_memory WITH ALLOW_CONNECTIONS true");
}
