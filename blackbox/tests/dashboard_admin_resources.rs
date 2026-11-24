//! The list of Resources and their Grants on the Dashboard (CONTRACT.md,
//! "The Dashboard for Platform administrators: Resources and Grants, read
//! only", as "Resources and Grants in full: The Dashboard: a Resource's
//! page and its Grants" changes it), with the stand-in sign-in: one page of
//! what `GET /v1/admin/resources` answers, by name (100 to a page, `after`,
//! `a[data-vm-next]` and `a[data-vm-first]`, a refused `after` is `400`),
//! each one element with `data-vm-kind`, its name the link
//! `a[data-vm-details]` to its page, its description and its Grants (the
//! first 200, `data-vm-grants-more` for the rest), words when no Group
//! holds one; a Resource that is not usable marked "Not usable" with
//! `data-vm-unusable`; one step away its owner, whom to ask, its kind, who
//! registered it and when, and the link to its Operation log; nothing of its
//! connection in the answer, headers and body; no form, and words saying
//! where a Group's access is given and how a Resource is registered; words
//! when none is registered; `your_access` not shown; the `503` while the
//! Grants on a Resource cannot be told.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::shared_database::{SharedDatabase, token};

use blackbox::admin_pages::{OPERATION_LOG, RESOURCES, query_pairs};
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options, start};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, quickly};
use blackbox::page::Element;
use blackbox::resources::{fill_grants, not_served};
use blackbox::scenario::Scenario;
use blackbox::storage::{FILE_WRITERS, FILES, MARKETING, Place, Storage, get, storage_body, wanda};
use blackbox::{Response, VmOptions, assert_api_error};
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
        let links = e.with_attr("data-vm-details");
        let [link] = links.as_slice() else {
            panic!(
                "{context}: expected one `a[data-vm-details]`, its name\n{}",
                page.describe()
            );
        };
        assert_eq!(link.name(), "a", "{context}: a link");
        assert_eq!(link.text(), name, "{context}: the link is its name");
        assert_eq!(
            link.attr("href")
                .and_then(|h| page.url.join(h).ok())
                .map(|u| (u.path().to_owned(), u.query().map(str::to_owned))),
            Some((format!("/admin/resources/{name}"), None)),
            "{context}: `a[data-vm-details]` leads to its page"
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
    assert_eq!(
        body.matches("vibe admin resource register").count(),
        1,
        "expected words, once, that a Resource is registered with the tool (`vibe admin resource register`); got {body:?}"
    );
    assert!(
        body.contains("resource's page") && body.contains("revoke"),
        "expected words, once, that a Group's access is given, changed and revoked on a Resource's page; got {body:?}"
    );
    assert!(
        !body.contains("platform's api"),
        "the words about the platform's API are struck; got {body:?}"
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
        text.contains("no resource")
            && text.contains("platform administrator")
            && text.contains("vibe admin resource register"),
        "expected words that none is registered yet and that a Platform administrator registers one with `vibe admin resource register`; got {text:?}"
    );
    assert!(page.forms().is_empty(), "no form");
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

/// A program with the stand-in and a storage root, and the Dashboard's
/// address with `http`.
fn storage_world() -> Storage {
    let place = Place::new("dashboard-list");
    let root = place.root();
    Storage::start_on(
        place,
        Some(&root),
        &VmOptions::new().args_extra(&["--public-scheme", "http"]),
    )
}

/// Registers `count` Storage Resources `r000`, `r001`, ... from a few
/// connections at once, within two minutes (else "the harness's own
/// check").
fn register_many(s: &Storage, count: usize) {
    const CONNECTIONS: usize = 8;
    let deadline = Instant::now() + Duration::from_secs(120);
    let next = AtomicUsize::new(0);
    let failures: Vec<String> = thread::scope(|scope| {
        let workers: Vec<_> = (0..CONNECTIONS)
            .map(|_| {
                scope.spawn(|| {
                    let api = s.vm.api_as(&administrator());
                    loop {
                        let n = next.fetch_add(1, Ordering::SeqCst);
                        if n >= count {
                            return None;
                        }
                        if Instant::now() >= deadline {
                            return Some(format!(
                                "the harness's own check: expected {count} Resources registered within two minutes, the time is out"
                            ));
                        }
                        let name = format!("r{n:03}");
                        let directory = s.dir(&name);
                        if let Err(e) = std::fs::create_dir_all(&directory) {
                            return Some(format!("cannot create {}: {e}", directory.display()));
                        }
                        let answer =
                            api.create_resource(&storage_body(&name, &directory.to_string_lossy()));
                        if answer.status != 201 {
                            return Some(format!(
                                "expected `201` registering {name:?}, got\n{}",
                                answer.describe()
                            ));
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .filter_map(|w| {
                w.join()
                    .unwrap_or_else(|_| Some("a worker failed".to_owned()))
            })
            .collect()
    });
    if let Some(first) = failures.first() {
        s.vm.fail(first);
    }
}

fn listed_names(page: &Page) -> Vec<String> {
    page.root()
        .with_attr("data-vm-resource")
        .iter()
        .map(|e| e.attr("data-vm-resource").unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn a_page_holds_a_hundred_resources_by_name_and_the_links_go_forward_and_back_to_the_first() {
    let s = storage_world();
    register_many(&s, 101);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let api = blackbox::resources::admin_resources(&s.admin(), "").json();
    assert_eq!(
        api["resources"].as_array().map(Vec::len),
        Some(100),
        "the harness's own check: the API's first page"
    );
    let page = dashboard.page(RESOURCES, 200, "the first page");
    let names = listed_names(&page);
    let expected: Vec<String> = (0..100).map(|n| format!("r{n:03}")).collect();
    assert_eq!(names, expected, "the first 100, by name");
    assert_eq!(api["next"], "r099", "the harness's own check");
    let next = page.link("data-vm-next");
    assert_eq!(
        next.as_ref().map(|u| (u.path().to_owned(), query_pairs(u))),
        Some((
            RESOURCES.to_owned(),
            vec![("after".to_owned(), "r099".to_owned())]
        )),
        "`a[data-vm-next]` to the page after the last one shown\n{}",
        page.describe()
    );
    assert!(
        page.link("data-vm-first").is_none(),
        "no link to the first page on the first page"
    );
    // Other parameters are ignored: `limit` is the API's, not the page's.
    let page = dashboard.page(&format!("{RESOURCES}?limit=3&x=1"), 200, "limit ignored");
    assert_eq!(listed_names(&page).len(), 100);
    // Following the link.
    let next = dashboard.page(
        &blackbox::browser::target_of(
            &dashboard
                .page(RESOURCES, 200, "again")
                .link("data-vm-next")
                .unwrap(),
        ),
        200,
        "the second page",
    );
    assert_eq!(listed_names(&next), ["r100"], "the one after");
    assert!(
        next.link("data-vm-next").is_none(),
        "no next after the last"
    );
    let first = next.link("data-vm-first");
    assert_eq!(
        first
            .as_ref()
            .map(|u| (u.path().to_owned(), u.query().map(str::to_owned))),
        Some((RESOURCES.to_owned(), None)),
        "`a[data-vm-first]` to the list without `after`"
    );
    // A name that no Resource has now, but could, is a place in the list.
    let beyond = dashboard.page(&format!("{RESOURCES}?after=zzz"), 200, "beyond the end");
    assert!(listed_names(&beyond).is_empty());
    assert!(beyond.link("data-vm-first").is_some());
    // Each of the hundred is its own link to its page.
    let page = dashboard.page(RESOURCES, 200, "the first page");
    assert_eq!(page.count("data-vm-details"), 100);
}

#[test]
fn an_after_the_api_would_refuse_is_400_malformed_with_a_link_to_the_first_page() {
    let s = storage_world();
    s.register(MARKETING);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for target in [
        format!("{RESOURCES}?after=Not_A_Name"),
        format!("{RESOURCES}?after="),
        format!("{RESOURCES}?after=a&after=b"),
        format!("{RESOURCES}?after=-x"),
        format!("{RESOURCES}?after=a%20b"),
        format!("{RESOURCES}?after={}", "n".repeat(64)),
    ] {
        let answer = dashboard.get(&target);
        assert_eq!(answer.status, 400, "{target}\n{}", answer.describe());
        let page = Page::read(answer, dashboard.url(&target), &target);
        let errors = page.errors();
        assert!(
            matches!(errors.as_slice(), [(r, w)] if r == "malformed" && !w.is_empty() && !w.contains("/v1/")),
            "{target}: expected `data-vm-error=\"malformed\"` in words for a person, got {errors:?}"
        );
        let first = page.link("data-vm-first");
        assert_eq!(
            first
                .as_ref()
                .map(|u| (u.path().to_owned(), u.query().map(str::to_owned))),
            Some((RESOURCES.to_owned(), None)),
            "{target}: `a[data-vm-first]` to the list"
        );
        assert_eq!(
            page.count("data-vm-resource"),
            0,
            "{target}: no Resource on the page of a refusal"
        );
    }
}

#[test]
fn a_resource_that_is_not_usable_is_marked_not_usable_with_its_problem_and_a_usable_one_is_not() {
    let s = storage_world();
    s.register(MARKETING);
    s.register("fine");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    std::fs::remove_dir_all(s.dir(MARKETING)).unwrap();
    // The program finds out as it uses the directory.
    let _ = s.one(&wanda(), FILES, &get(MARKETING, "anything"));
    let api = blackbox::resources::admin_resource(&s.admin(), MARKETING).json();
    assert_eq!(
        api["usable"],
        json!(false),
        "the harness's own check: {api}"
    );
    let page = Dashboard::new(&s.vm, &administrator()).page(RESOURCES, 200, "the list");
    let lost = element(&page, MARKETING);
    assert_eq!(
        lost.attr("data-vm-unusable"),
        api["problem"].as_str(),
        "`data-vm-unusable` says the problem the API says"
    );
    assert!(
        lost.text().contains("Not usable"),
        "the mark \"Not usable\" beside its name, got {:?}",
        lost.text()
    );
    let fine = element(&page, "fine");
    assert!(!fine.has("data-vm-unusable"), "a usable Resource has none");
    assert!(!fine.text().contains("Not usable"));
}

#[test]
fn a_database_resource_that_is_not_served_is_marked_not_usable_for_each_problem() {
    for problem in ["no-secret-key", "does-not-open", "not-sealed"] {
        let s = not_served(problem);
        let dashboard = Dashboard::new(&s.vm, &administrator());
        let began = Instant::now();
        let page = loop {
            let answer = dashboard.get(RESOURCES);
            if answer.status == 200 {
                break Page::read(answer, dashboard.url(RESOURCES), problem);
            }
            assert!(
                began.elapsed() < Duration::from_secs(20),
                "{problem}: expected the list once the Grants are known, got\n{}",
                answer.describe()
            );
            thread::sleep(Duration::from_millis(500));
        };
        let one = element(&page, "devops");
        assert_eq!(one.attr("data-vm-unusable"), Some(problem), "{problem}");
        assert!(
            one.text().contains("Not usable"),
            "{problem}: the mark \"Not usable\", got {:?}",
            one.text()
        );
        assert_eq!(
            page.count("data-vm-grant"),
            1,
            "{problem}: its Grants are shown all the same"
        );
    }
}

#[test]
fn the_first_200_grants_are_shown_and_the_rest_counted_on_the_list_with_the_page_having_them_all() {
    let s = Scenario::start_with(&stand_in_options());
    s.register("many");
    s.register("few");
    fill_grants(&s.vm, "many", 0, 230, "read");
    fill_grants(&s.vm, "few", 0, 200, "read");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(RESOURCES, 200, "the list");
    let many = element(&page, "many");
    let shown: Vec<String> = many
        .with_attr("data-vm-grant")
        .iter()
        .map(|g| g.attr("data-vm-grant").unwrap_or_default().to_owned())
        .collect();
    let expected: Vec<String> = (0..200).map(|n| format!("group{n:04}")).collect();
    assert_eq!(shown, expected, "the first 200, by Group");
    let more = many.with_attr("data-vm-grants-more");
    assert_eq!(
        more.iter()
            .map(|e| e.attr("data-vm-grants-more").unwrap_or_default().to_owned())
            .collect::<Vec<_>>(),
        ["30"],
        "an element `data-vm-grants-more` says how many more\n{}",
        page.describe()
    );
    assert!(
        more[0].text().to_lowercase().contains("page"),
        "and that the Resource's page has them all, got {:?}",
        more[0].text()
    );
    let few = element(&page, "few");
    assert_eq!(few.with_attr("data-vm-grant").len(), 200);
    assert!(
        few.with_attr("data-vm-grants-more").is_empty(),
        "exactly 200 Grants are all of them"
    );
    let one = dashboard.page("/admin/resources/many", 200, "the Resource's page");
    assert_eq!(
        one.count("data-vm-grant"),
        230,
        "its page has every one of them"
    );
}
