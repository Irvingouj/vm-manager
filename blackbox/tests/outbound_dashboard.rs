//! The Dashboard's page of the list (CONTRACT.md, "Outbound calls: The
//! Dashboard"; "The Dashboard for Platform administrators": who may, the
//! way in, what it answers, who is refused): `GET /admin/outbound` shows
//! what `GET /v1/outbound-hosts` answers, one element
//! `data-vm-outbound-host="<host>"` an entry with who added it and when,
//! an empty state that says nothing can be called and gives the command,
//! and no form; it keeps the rules of every page of the Dashboard, has its
//! way in from the administrators' navigation, is refused to everyone who
//! is not a Platform administrator as every page under `/admin` is; the
//! Operation log page shows a `fetch` entry's host (`→ <host>`) and takes
//! `host` among its filters.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::other_administrator;
use blackbox::admin_pages::{
    APPLICATIONS, INSTALLATION, OPERATION_LOG, OUTBOUND, RESOURCES, SECTIONS, assert_admin_link,
    assert_not_an_administrator, assert_sections, assert_title, entries, field_value, log_target,
};
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options};
use blackbox::outbound_surface::{add, alice, bob, delete, entries as listed, put, start};
use blackbox::sharing::{TRACED, assert_own_page_headers};
use blackbox::{Request, User, Vm};

fn page_of(vm: &Vm) -> Page {
    Dashboard::new(vm, &administrator()).page(OUTBOUND, 200, "the outbound list")
}

fn rows(page: &Page) -> Vec<(String, String)> {
    page.root()
        .with_attr("data-vm-outbound-host")
        .iter()
        .map(|e| {
            (
                e.attr("data-vm-outbound-host").unwrap().to_owned(),
                e.text(),
            )
        })
        .collect()
}

#[test]
fn with_no_entry_it_says_nothing_can_be_called_and_gives_the_command() {
    let vm = start();
    let page = page_of(&vm);
    assert!(rows(&page).is_empty(), "no row: {:?}", rows(&page));
    let text = page.body_text();
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    assert!(
        lower.contains("nothing") || lower.contains("no host"),
        "expected words saying that nothing can be called yet\n{text}"
    );
    assert!(
        flat.contains("vibe admin outbound add"),
        "expected the command `vibe admin outbound add <host>` in the words\n{text}"
    );
}

#[test]
fn each_entry_is_one_row_with_who_added_it_and_when_in_the_apis_order() {
    let vm = start();
    for entry in [
        "c.example.com",
        "*.example.org:8443",
        "a.example.com",
        "203.0.113.7",
        "[2001:db8::7]:8443",
        "bücher.example",
    ] {
        add(&vm, entry);
    }
    // A second administrator adds one.
    let other = other_administrator();
    put(&vm, &other, "by-other.example.net").expect_status(200);
    let api = listed(&vm);
    assert_eq!(api.len(), 7, "the harness's own check");
    let page = page_of(&vm);
    let shown = rows(&page);
    let api_hosts: Vec<&str> = api.iter().map(|e| e["host"].as_str().unwrap()).collect();
    let shown_hosts: Vec<&str> = shown.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(
        shown_hosts,
        api_hosts,
        "expected one row an entry, in the order the API answers them\n{}",
        page.describe()
    );
    for (entry, (host, text)) in api.iter().zip(&shown) {
        let by = entry["added_by"].as_str().unwrap();
        let date: String = entry["added_at"]
            .as_str()
            .unwrap()
            .chars()
            .take(10)
            .collect();
        assert!(
            text.contains(host.as_str()),
            "expected the row to say its host {host:?}, got {text:?}"
        );
        assert!(
            text.contains(by),
            "expected the row of {host:?} to say who added it ({by}), got {text:?}"
        );
        assert!(
            text.contains(&date),
            "expected the row of {host:?} to say when it was added (the day {date}), got {text:?}"
        );
    }
}

#[test]
fn what_the_page_shows_is_what_the_api_answers_now() {
    let vm = start();
    add(&vm, "api.example.com");
    assert_eq!(rows(&page_of(&vm)).len(), 1);
    add(&vm, "other.example.com");
    assert_eq!(rows(&page_of(&vm)).len(), 2);
    delete(&vm, &administrator(), "api.example.com").expect_status(200);
    let shown = rows(&page_of(&vm));
    assert_eq!(
        shown.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
        ["other.example.com"],
        "a removal shows at once"
    );
}

#[test]
fn the_page_is_read_only_and_keeps_the_rules_of_every_page_of_the_dashboard() {
    let vm = start();
    add(&vm, "api.example.com");
    let dashboard = Dashboard::new(&vm, &administrator());
    let answer = dashboard.get(OUTBOUND);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_own_page_headers(&answer, "the outbound list");
    let page = Page::read(answer, dashboard.url(OUTBOUND), "the outbound list");
    assert_title(&page, "the outbound list");
    assert_admin_link(&page, "the outbound list");
    assert!(
        page.root().named("form").is_empty()
            && page.root().named("button").is_empty()
            && page.root().named("input").is_empty(),
        "expected no form, button or field: the page is read-only in this ticket\n{}",
        page.describe()
    );
    assert_eq!(
        page.count("data-vm-admin-sections"),
        1,
        "expected the administrators' navigation under the header\n{}",
        page.describe()
    );
    // `HEAD` is answered as `GET`, without a body.
    let head = dashboard.navigation("HEAD", OUTBOUND).send();
    assert!(
        head.status == 200 && head.body().is_empty(),
        "expected `200` without a body for HEAD\n{}",
        head.describe()
    );
    // Another method: `405`, a page, with `allow`.
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let answer = dashboard.form_request(OUTBOUND, "").method(method).send();
        assert_eq!(
            answer.status,
            405,
            "{method} {OUTBOUND}: expected `405`\n{}",
            answer.describe()
        );
        let allow = answer.header("allow").unwrap_or_default().to_uppercase();
        assert!(
            allow.contains("GET") && allow.contains("HEAD") && !allow.contains("POST"),
            "{method}: expected `allow: GET, HEAD`, got {allow:?}"
        );
    }
    assert_eq!(listed(&vm).len(), 1, "nothing was changed by them");
}

#[test]
fn the_fifth_section_of_the_navigation_is_outbound_after_resources_before_installation() {
    let vm = start();
    let dashboard = Dashboard::new(&vm, &administrator());
    for path in [
        APPLICATIONS,
        OPERATION_LOG,
        RESOURCES,
        OUTBOUND,
        INSTALLATION,
    ] {
        let page = dashboard.page(path, 200, path);
        let current = SECTIONS
            .iter()
            .find(|(_, p)| *p == path)
            .map(|(s, _)| *s)
            .unwrap();
        assert_sections(&page, current, path);
        let navs = page.root().with_attr("data-vm-admin-sections");
        let order: Vec<String> = navs[0]
            .with_attr("data-vm-section")
            .iter()
            .map(|a| a.attr("data-vm-section").unwrap().to_owned())
            .collect();
        assert_eq!(
            order,
            [
                "applications",
                "operation-log",
                "resources",
                "outbound",
                "installation"
            ],
            "{path}: the five sections in order\n{}",
            page.describe()
        );
        let outbound = navs[0].with_attr_value("data-vm-section", "outbound");
        assert_eq!(
            outbound[0].accessible_name(),
            "Outbound",
            "{path}: the link's name\n{}",
            page.describe()
        );
    }
}

#[test]
fn everyone_who_is_not_a_platform_administrator_is_refused_as_on_every_page_under_admin() {
    let vm = start();
    add(&vm, "api.example.com");
    let carol = User::new("carol").in_groups(&["finance"]);
    vm.api_as(&alice())
        .create_application("board")
        .expect_status(201);
    for person in [alice(), bob(), carol] {
        let who = person.id.clone();
        let dashboard = Dashboard::new(&vm, &person);
        let base = dashboard.url("/");
        let page = assert_not_an_administrator(
            dashboard.get(OUTBOUND),
            &base,
            &format!("{who}: GET {OUTBOUND}"),
        );
        let nothing_there = assert_not_an_administrator(
            dashboard.get("/admin/nope"),
            &base,
            &format!("{who}: GET /admin/nope"),
        );
        assert_eq!(
            page.response.body(),
            nothing_there.response.body(),
            "{who}: the refusal of {OUTBOUND} is byte for byte that of any path under /admin"
        );
        assert!(
            !page.response.text().contains("api.example.com"),
            "{who}: the refusal tells nothing of the list"
        );
        for method in ["POST", "PUT", "DELETE"] {
            let answer = dashboard.form_request(OUTBOUND, "").method(method).send();
            assert_not_an_administrator(answer, &base, &format!("{who}: {method} {OUTBOUND}"));
        }
        let head = dashboard.navigation("HEAD", OUTBOUND).send();
        assert!(
            head.status == 403 && head.body().is_empty(),
            "{who}: HEAD {OUTBOUND}: expected `403` without a body\n{}",
            head.describe()
        );
    }
    assert_eq!(listed(&vm).len(), 1, "nothing changed");
}

#[test]
fn someone_with_no_session_is_401_and_the_page_holds_no_entry() {
    let vm = start();
    add(&vm, "api.example.com");
    let host = format!("dashboard.vm.test:{}", vm.address().port());
    let answer = Request::new(vm.address(), OUTBOUND, &host)
        .header("accept", "text/html")
        .send();
    assert_eq!(answer.status, 401, "{}", answer.describe());
    assert!(!answer.text().contains("api.example.com"));
}

#[test]
fn the_operation_log_page_shows_a_fetchs_host_and_takes_host_among_its_filters() {
    let vm = Vm::start(&stand_in_options().application("board", TRACED));
    // Nothing is listed: a call is refused, and has its entry with its host
    // all the same.
    let calls = [
        "https://api.example.com/v1/secret-path?token=abc123",
        "https://other.example.org/x",
    ];
    for to in calls {
        let answer = vm
            .request(
                "board",
                &format!("/fetch?to={}", blackbox::logs::encode(to)),
            )
            .as_user(&bob())
            .header("accept", "application/json")
            .send();
        answer.expect_status(200);
    }
    let dashboard = Dashboard::new(&vm, &administrator());
    let page = dashboard.page(OPERATION_LOG, 200, "the Operation log");
    let shown = entries(&page);
    let with_api: Vec<_> = shown
        .iter()
        .filter(|s| s.text.contains("→ api.example.com"))
        .collect();
    assert_eq!(
        with_api.len(),
        1,
        "expected one entry saying `→ api.example.com`, found {} among {} entries\n{}",
        with_api.len(),
        shown.len(),
        page.describe()
    );
    assert!(
        shown.iter().any(|s| s.text.contains("→ other.example.org")),
        "expected the other call's entry to say `→ other.example.org`\n{}",
        page.describe()
    );
    let html = page.response.text();
    for secret in ["secret-path", "abc123", "token="] {
        assert!(
            !html.contains(secret),
            "expected nothing of the path or the query ({secret:?}) on the page"
        );
    }
    // The filter.
    let narrowed = dashboard.page(
        &log_target(&[("host", "api.example.com")]),
        200,
        "the Operation log, host=api.example.com",
    );
    let shown = entries(&narrowed);
    assert_eq!(
        shown.len(),
        1,
        "expected exactly the entry of the call to api.example.com\n{}",
        narrowed.describe()
    );
    assert!(shown[0].text.contains("→ api.example.com"));
    assert_eq!(
        field_value(&narrowed, "host").as_deref(),
        Some("api.example.com"),
        "expected the filter form to hold the host asked for\n{}",
        narrowed.describe()
    );
    let none = dashboard.page(
        &log_target(&[("host", "nobody.example.com")]),
        200,
        "the Operation log, a host nothing was called at",
    );
    assert!(
        entries(&none).is_empty(),
        "expected no entry for a host nothing was called at"
    );
}
