//! The Resources and their Grants on the Dashboard, read only (CONTRACT.md,
//! "The Dashboard for Platform administrators: Resources and Grants, read
//! only"), with the stand-in sign-in: every Resource `GET /v1/resources`
//! answers, by name, each one element with `data-vm-kind`, its description
//! and its Grants as `GET /v1/resources/<name>/grants` answers them (each
//! `data-vm-grant` with `data-vm-access`), words when no Group holds one;
//! one step away its owner, whom to ask, its kind, who registered it and
//! when, and the link to its Operation log; nothing of its connection in
//! the answer, headers and body; no form, and words saying where Grants are
//! given; words when none is registered; `your_access` not shown; the `503`
//! while the Grants on a Resource cannot be told.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;

use blackbox::shared_database::{SharedDatabase, token};

use blackbox::admin_pages::{OPERATION_LOG, RESOURCES, query_pairs};
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options, start};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, quickly};
use blackbox::page::Element;
use blackbox::scenario::Scenario;
use blackbox::{Response, assert_api_error};
use serde_json::{Value, json};

/// A body registering `name` over `db` whose connection holds only
/// recognisable strings: roles named after the marker, passwords the same
/// (registering connects, so they are real roles of the shared database).
fn body(db: &SharedDatabase, name: &str) -> Value {
    let read = blackbox::resources::named_login(
        db,
        &format!("role_marker_read_{name}_{}", token()),
        &format!("password-marker-read-{name}-{}", token()),
    );
    let write = blackbox::resources::named_login(
        db,
        &format!("role_marker_write_{name}_{}", token()),
        &format!("password-marker-write-{name}-{}", token()),
    );
    json!({
        "name": name,
        "kind": "postgres",
        "description": format!("The {name} database of the incident team"),
        "owner": format!("The {name} owners"),
        "contact": format!("ask-{name}@example.test"),
        "connection": blackbox::resources::connection(db, &read, Some(&write), None),
    })
}

/// The markers of a body's connection: its database, roles and passwords,
/// and the host when it is unmistakable.
fn markers(body: &Value) -> Vec<String> {
    let c = &body["connection"];
    let mut all = vec![
        c["database"].as_str().unwrap().to_owned(),
        c["read"]["role"].as_str().unwrap().to_owned(),
        c["read"]["password"].as_str().unwrap().to_owned(),
        c["write"]["role"].as_str().unwrap().to_owned(),
        c["write"]["password"].as_str().unwrap().to_owned(),
    ];
    if c["host"].as_str() != Some("127.0.0.1") {
        all.push(c["host"].as_str().unwrap().to_owned());
    }
    all
}

/// Three Resources (registered out of order): `alpha` with Grants to ops
/// (write) and staff (read), `beta` with none, `gamma` with finance
/// (structure).
fn world() -> Scenario {
    world_with_markers().0
}

/// [`world`], and the markers of each Resource's connection.
fn world_with_markers() -> (Scenario, Vec<(&'static str, Vec<String>)>) {
    let s = Scenario::start_with(&stand_in_options());
    let mut all = Vec::new();
    for name in ["gamma", "alpha", "beta"] {
        let registered = body(&s.db, name);
        s.register_body(name, &registered);
        all.push((name, markers(&registered)));
    }
    s.grant("alpha", "staff", "read");
    s.grant("alpha", "ops", "write");
    s.grant("gamma", "finance", "structure");
    (s, all)
}

fn whole(response: &Response) -> String {
    let mut text: String = response
        .headers()
        .iter()
        .map(|(n, v)| format!("{n}: {v}\n"))
        .collect();
    text.push_str(&response.text());
    text
}

fn element<'a>(page: &'a Page, name: &str) -> Element<'a> {
    let found = page.root().with_attr_value("data-vm-resource", name);
    match found.as_slice() {
        [one] => *one,
        _ => panic!(
            "expected one element `data-vm-resource={name:?}`, found {}\n{}",
            found.len(),
            page.describe()
        ),
    }
}

#[test]
fn every_resource_with_its_grants_as_the_api_answers_them() {
    let s = world();
    let api = s.admin().list_resources().json();
    let page = Dashboard::new(&s.vm, &administrator()).page(RESOURCES, 200, "the Resources");
    let names: Vec<String> = page
        .root()
        .with_attr("data-vm-resource")
        .iter()
        .map(|e| e.attr("data-vm-resource").unwrap().to_owned())
        .collect();
    let api_names: Vec<String> = api["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        api_names,
        ["alpha", "beta", "gamma"],
        "the harness's own check"
    );
    assert_eq!(
        names,
        api_names,
        "every Resource, by name\n{}",
        page.describe()
    );
    for resource in api["resources"].as_array().unwrap() {
        let name = resource["name"].as_str().unwrap();
        let context = format!("the Resource {name}");
        let e = element(&page, name);
        assert_eq!(
            e.attr("data-vm-kind"),
            Some("postgres"),
            "{context}: data-vm-kind"
        );
        let grants: Vec<(String, String)> = e
            .with_attr("data-vm-grant")
            .iter()
            .map(|g| {
                (
                    g.attr("data-vm-grant").unwrap_or_default().to_owned(),
                    g.attr("data-vm-access").unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let api_grants: Vec<(String, String)> = s.admin().list_grants(name).json()["grants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                (
                    g["group"].as_str().unwrap().to_owned(),
                    g["access"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            grants, api_grants,
            "{context}: its Grants as the API answers them, by Group"
        );
        for g in e.with_attr("data-vm-grant") {
            let group = g.attr("data-vm-grant").unwrap();
            let access = g.attr("data-vm-access").unwrap();
            assert!(
                g.text().contains(group) && g.text().contains(access),
                "{context}: the Grant shows the Group and its level, got {:?}",
                g.text()
            );
            assert!(
                !g.has("data-vm-user"),
                "{context}: a Grant carries no `data-vm-user`"
            );
        }
        let details = e.named("details");
        let [details] = details.as_slice() else {
            panic!("{context}: expected one `<details>`\n{}", page.describe());
        };
        let first = e.text().replacen(&details.text(), "", 1);
        assert!(first.contains(name), "{context}: its name seen first");
        assert!(
            first.contains(resource["description"].as_str().unwrap()),
            "{context}: its description seen first, got {first:?}"
        );
        if grants.is_empty() {
            let lower = first.to_lowercase();
            assert!(
                lower.contains("no group"),
                "{context}: expected words that no Group holds a Grant, so nobody's request reaches it; got {first:?}"
            );
        }
        let owner: Vec<String> = details
            .with_attr("data-vm-owner")
            .iter()
            .map(Element::text)
            .collect();
        assert_eq!(
            owner,
            [resource["owner"].as_str().unwrap()],
            "{context}: its owner, one step away"
        );
        let contact: Vec<String> = details
            .with_attr("data-vm-contact")
            .iter()
            .map(Element::text)
            .collect();
        assert_eq!(
            contact,
            [resource["contact"].as_str().unwrap()],
            "{context}: whom to ask, one step away"
        );
        let text = details.text();
        assert!(
            text.contains("Postgres"),
            "{context}: its kind in words, got {text:?}"
        );
        assert!(
            text.contains(resource["created_by"].as_str().unwrap())
                && text.contains(
                    &resource["created_at"]
                        .as_str()
                        .unwrap()
                        .chars()
                        .take(10)
                        .collect::<String>()
                ),
            "{context}: who registered it and when, got {text:?}"
        );
        let links = details.with_attr("data-vm-operation-log");
        let href = links
            .first()
            .and_then(|a| a.attr("href"))
            .and_then(|h| page.url.join(h).ok());
        assert!(
            href.as_ref().is_some_and(|u| u.path() == OPERATION_LOG
                && query_pairs(u) == vec![("resource".to_owned(), name.to_owned())]),
            "{context}: expected `a[data-vm-operation-log]` to {OPERATION_LOG}?resource={name}, got {href:?}"
        );
    }
}

#[test]
fn nothing_of_a_connection_no_form_and_no_your_access() {
    let (s, all_markers) = world_with_markers();
    // The administrator holds a Grant through a Group of theirs: still not
    // shown.
    s.grant("beta", blackbox::scenario::ADMINISTRATORS, "write");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let answer = dashboard.get(RESOURCES);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    let text = whole(&answer);
    for (name, found) in &all_markers {
        for marker in found {
            assert!(
                !text.contains(marker.as_str()),
                "expected nothing of {name}'s connection in the answer, found {marker:?}"
            );
        }
    }
    let page = Page::read(answer, dashboard.url(RESOURCES), "the Resources");
    assert!(
        page.forms().is_empty(),
        "no form on the Resources\n{}",
        page.describe()
    );
    let body = page.body_text().to_lowercase();
    assert!(
        body.contains("platform's api"),
        "expected words, once, that Grants are given and Resources registered on the platform's API for now; got {body:?}"
    );
    assert_eq!(page.count("data-vm-yours"), 0, "`your_access` is not shown");
    assert!(!body.contains("your access"), "`your_access` is not shown");
}

#[test]
fn with_no_resource_it_says_none_is_registered_and_who_registers_one() {
    let vm = start();
    let page = Dashboard::new(&vm, &administrator()).page(RESOURCES, 200, "no Resource");
    assert_eq!(page.count("data-vm-resource"), 0);
    let text = page.body_text().to_lowercase();
    assert!(
        text.contains("no resource") && text.contains("platform administrator"),
        "expected words that none is registered yet and that a Platform administrator registers one on the platform's API; got {text:?}"
    );
}

#[test]
fn while_the_grants_on_a_resource_cannot_be_told_the_page_is_503_naming_it() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_dashboard_lost_grants");
    let s = Scenario::start_with(&stand_in_options().platform(&platform));
    s.register_body("crewdb", &body(&s.db, "crewdb"));
    s.grant("crewdb", "crew", "read");
    let vm = &s.vm;
    let dashboard = Dashboard::new(vm, &administrator());
    assert_eq!(
        s.admin().list_grants("crewdb").status,
        200,
        "the harness's own check: before"
    );
    postgres.hold();
    let lower = s
        .admin()
        .request("/v1/resources/crewdb/grants")
        .method("POST")
        .json(&json!({ "group": "crew", "access": "structure" }))
        .timeout(LOST_WRITE_TIMEOUT);
    let lowered = thread::scope(|scope| {
        let lowered = scope.spawn(move || lower.try_send());
        postgres.wait_for_waiting(1, "the Grant lowered");
        lowered.join().unwrap()
    });
    let lowered = lowered.unwrap_or_else(|e| panic!("expected `503` for the Grant lowered ({e})"));
    assert_api_error(&lowered, 503, "unavailable");
    assert_api_error(&s.admin().list_resources(), 503, "unavailable");
    let answer = quickly(
        vm,
        &dashboard.navigation("GET", RESOURCES),
        "the Resources meanwhile",
    );
    assert!(
        answer.status == 503 && answer.header("retry-after").is_some(),
        "expected `503` with `retry-after` while the Grants on crewdb cannot be told\n{}",
        answer.describe()
    );
    let page = Page::read(answer, dashboard.url(RESOURCES), "meanwhile");
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, w)] if r == "unavailable" && w.contains("crewdb") && w.to_lowercase().contains("again")),
        "expected `data-vm-error=\"unavailable\"` naming crewdb and saying to try again shortly, got {errors:?}"
    );
    assert_eq!(page.count("data-vm-grant"), 0, "no Grant shown on a guess");
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let page = dashboard.page(RESOURCES, 200, "after");
    let grants: Vec<String> = page
        .root()
        .with_attr("data-vm-grant")
        .iter()
        .map(|g| {
            format!(
                "{}:{}",
                g.attr("data-vm-grant").unwrap(),
                g.attr("data-vm-access").unwrap()
            )
        })
        .collect();
    assert_eq!(
        grants,
        ["crew:structure"],
        "the Grant the database has once settled"
    );
}
