//! What the Dashboard answers under `/admin` (CONTRACT.md, "The Dashboard
//! for Platform administrators: What it answers"), with the stand-in
//! sign-in: each path of the table with its status; another method `405`
//! with `allow`; any other path under `/admin` `404`, a page; `HEAD` as
//! `GET` without a body; every answer a page of the platform's own with its
//! headers and `form-action 'self'`, its `<title>` the page's name then
//! ` — Dashboard`; the order: sign-in first, then the origin rule, then
//! whether the person is a Platform administrator; `503` with `retry-after`
//! while the platform database cannot be reached (the Resources and the
//! installation too), and the pages again once it can.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::admin_pages::{
    APPLICATIONS, INSTALLATION, OPERATION_LOG, OUTBOUND, RESOURCES, assert_title, disable_target,
    enable_target, get_paths,
};
use blackbox::dashboard::{
    Dashboard, Page, administrator, create, deploy, stand_in_options, start,
};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::sharing::assert_own_page_headers;
use blackbox::{Request, Response, User, Vm};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice")
}

fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some("The incident board"));
    deploy(&vm, &alice(), APP, &[]);
    vm
}

fn allowed(response: &Response) -> Vec<String> {
    let mut methods: Vec<String> = response
        .header("allow")
        .unwrap_or_default()
        .split(',')
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .collect();
    methods.sort();
    methods
}

#[test]
fn each_path_of_the_table_answers_its_status_with_a_page_and_its_title() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for (target, status) in [
        (APPLICATIONS.to_owned(), 200),
        (format!("{APPLICATIONS}?after={APP}"), 200),
        (disable_target(APP), 200),
        (OPERATION_LOG.to_owned(), 200),
        (RESOURCES.to_owned(), 200),
        (OUTBOUND.to_owned(), 200),
        (INSTALLATION.to_owned(), 200),
    ] {
        let page = dashboard.page(&target, status, &target);
        assert_title(&page, &target);
        assert_eq!(
            page.root().named("h1").len(),
            1,
            "{target}: expected one `<h1>`\n{}",
            page.describe()
        );
    }
}

#[test]
fn another_method_on_a_path_of_the_table_is_405_with_allow() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let get_head = vec!["GET", "HEAD"];
    let mut cases: Vec<(&str, String, Vec<&str>)> = Vec::new();
    for path in [
        "/admin",
        APPLICATIONS,
        OPERATION_LOG,
        RESOURCES,
        OUTBOUND,
        INSTALLATION,
    ] {
        for method in ["POST", "DELETE", "PUT"] {
            cases.push((method, path.to_owned(), get_head.clone()));
        }
    }
    for method in ["DELETE", "PUT", "PATCH"] {
        cases.push((method, disable_target(APP), vec!["GET", "HEAD", "POST"]));
    }
    for method in ["GET", "DELETE", "PUT"] {
        cases.push((method, enable_target(APP), vec!["POST"]));
    }
    for (method, target, wanted) in cases {
        let response = if method == "POST" {
            dashboard.post_form(&target, "")
        } else {
            dashboard.navigation(method, &target).send()
        };
        let context = format!("{method} {target}");
        assert_eq!(
            response.status,
            405,
            "{context}: expected `405`\n{}",
            response.describe()
        );
        let mut wanted: Vec<String> = wanted.iter().map(|m| (*m).to_owned()).collect();
        wanted.sort();
        assert_eq!(
            allowed(&response),
            wanted,
            "{context}: expected `allow` to name exactly these methods\n{}",
            response.describe()
        );
        let page = Page::read(response, dashboard.url(&target), &context);
        assert_eq!(
            page.root().named("h1").len(),
            1,
            "{context}: a page with one `<h1>`"
        );
    }
}

#[test]
fn any_other_path_under_admin_is_404_a_page() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    for target in [
        "/admin/",
        "/admin/nope",
        "/admin/applications/",
        "/admin/applications/board",
        "/admin/applications/board/",
        "/admin/applications/board/disable/x",
        "/admin/applications/board/delete",
        "/admin/resources/devops",
        "/admin/installation/x",
        "/admin/operation-log/1",
        "/admin/admin",
        "/admin//applications",
    ] {
        let page = dashboard.page(
            target,
            404,
            &format!("{target}: a path under /admin it has not"),
        );
        assert_eq!(
            page.root().named("h1").len(),
            1,
            "{target}: a page with one `<h1>`\n{}",
            page.describe()
        );
    }
}

#[test]
fn head_is_answered_as_get_without_a_body() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let mut targets: Vec<(String, u16)> = get_paths(APP)
        .into_iter()
        .map(|p| {
            let status = if p == "/admin" { 303 } else { 200 };
            (p, status)
        })
        .collect();
    targets.extend([
        ("/admin/nope".to_owned(), 404),
        (disable_target("nope"), 404),
        (format!("{APPLICATIONS}?after=Not_A_Name"), 400),
        (format!("{OPERATION_LOG}?outcome=maybe"), 400),
    ]);
    for (target, status) in targets {
        let get = dashboard.get(&target);
        let head = dashboard.navigation("HEAD", &target).send();
        assert_eq!(
            head.status,
            get.status,
            "HEAD {target}: expected the status of GET\nGET:\n{}\nHEAD:\n{}",
            get.describe(),
            head.describe()
        );
        assert_eq!(
            get.status,
            status,
            "GET {target}: expected `{status}`\n{}",
            get.describe()
        );
        assert!(
            head.body().is_empty(),
            "HEAD {target}: expected no body\n{}",
            head.describe()
        );
        if get.status != 303 {
            assert_own_page_headers(&head, &format!("HEAD {target}"));
        }
    }
}

#[test]
fn every_answer_has_the_headers_and_the_policy_of_the_dashboards_pages() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &administrator());
    let answers = vec![
        (APPLICATIONS.to_owned(), 200, dashboard.get(APPLICATIONS)),
        (
            disable_target(APP),
            200,
            dashboard.get(&disable_target(APP)),
        ),
        (OPERATION_LOG.to_owned(), 200, dashboard.get(OPERATION_LOG)),
        (RESOURCES.to_owned(), 200, dashboard.get(RESOURCES)),
        (OUTBOUND.to_owned(), 200, dashboard.get(OUTBOUND)),
        (INSTALLATION.to_owned(), 200, dashboard.get(INSTALLATION)),
        ("/admin/nope".to_owned(), 404, dashboard.get("/admin/nope")),
        (
            "a refused disable".to_owned(),
            404,
            dashboard.post_form(&disable_target("nope"), ""),
        ),
    ];
    for (what, status, response) in answers {
        assert_eq!(
            response.status,
            status,
            "{what}: expected `{status}`\n{}",
            response.describe()
        );
        // `Page::read` checks the headers, `form-action 'self'`, no script
        // and the viewport.
        let page = Page::read(response, dashboard.url("/"), &what);
        let policy = page
            .response
            .header("content-security-policy")
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            policy.contains("frame-ancestors 'none'") && policy.contains("default-src 'none'"),
            "{what}: expected `default-src 'none'` and `frame-ancestors 'none'` in the policy, got {policy:?}"
        );
        let (id, _) = page.user();
        assert_eq!(id, administrator().id, "{what}: whom they are signed in as");
    }
}

#[test]
fn sign_in_comes_first_then_the_origin_rule_then_who_may() {
    let vm = with_board();
    let host = format!("dashboard.vm.test:{}", vm.address().port());
    // Not signed in: `401`, whatever the path under /admin and whether it
    // exists.
    for (method, target) in [
        ("GET", "/admin"),
        ("GET", APPLICATIONS),
        ("GET", "/admin/nope"),
        ("GET", "/admin/applications?after=Not_A_Name"),
        ("POST", "/admin/applications/board/disable"),
        ("POST", "/admin/applications/nope/enable"),
        ("DELETE", OPERATION_LOG),
    ] {
        let response = Request::new(vm.address(), target, &host)
            .method(method)
            .header("accept", "text/html")
            .header("content-type", "application/x-www-form-urlencoded")
            .send();
        assert!(
            response.status == 401
                && response
                    .header("content-type")
                    .is_some_and(|t| t.starts_with("text/plain")),
            "{method} {target}: expected the `401` of \"Not signed in\" with the stand-in and no stand-in headers\n{}",
            response.describe()
        );
    }
    // Signed in, from another origin: the origin rule's `403`, `text/plain`,
    // before whether the person is a Platform administrator (the same to
    // both) and before `404`.
    let board_origin = format!("http://board.vm.test:{}", vm.address().port());
    for person in [administrator(), alice()] {
        let dashboard = Dashboard::new(&vm, &person);
        for target in [APPLICATIONS, "/admin/nope"] {
            let response = dashboard
                .bare("GET", target)
                .header("origin", &board_origin)
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "cors")
                .header("sec-fetch-dest", "empty")
                .send();
            assert!(
                response.status == 403
                    && response
                        .header("content-type")
                        .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
                "a fetch of {target} from an Application's page as {}: expected the origin rule's `403`, `text/plain`\n{}",
                person.id,
                response.describe()
            );
        }
    }
}

#[test]
fn with_the_platform_database_stopped_an_administrators_page_is_503_with_retry_after() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_admin_pages_stopped");
    let vm = Vm::start(&stand_in_options().platform(&platform));
    create(&vm, &alice(), APP, None);
    let dashboard = Dashboard::new(&vm, &administrator());
    dashboard.page(APPLICATIONS, 200, "before the stop");
    let disable = dashboard
        .form_request(&disable_target(APP), "")
        .timeout(Duration::from_secs(40));
    // Everything written so far on disk first: the harness's Postgres
    // runs without fsync and is stopped as a crash, which would otherwise
    // lose what it had not yet written (`application_usage` was lost so
    // once: "relation does not exist" after the restart).
    blackbox::admin::platform_sql(&vm).batch("CHECKPOINT");
    postgres.stop();
    for target in [
        APPLICATIONS,
        OPERATION_LOG,
        RESOURCES,
        OUTBOUND,
        INSTALLATION,
    ] {
        let target = target.to_owned();
        let response = dashboard
            .navigation("GET", &target)
            .timeout(Duration::from_secs(40))
            .send();
        assert!(
            response.status == 503 && response.header("retry-after").is_some(),
            "{target}: expected `503` with `retry-after` while the platform database cannot be reached\n{}",
            response.describe()
        );
        let page = Page::read(response, dashboard.url(&target), &target);
        assert!(
            page.text().to_lowercase().contains("again"),
            "{target}: expected a page saying to try again shortly\n{}",
            page.describe()
        );
    }
    let answer = disable.send();
    let page = blackbox::dashboard::assert_refused(
        answer,
        &dashboard.url("/"),
        503,
        "unavailable",
        APP,
        "a disable while the platform database cannot be reached",
    );
    let words = blackbox::dashboard::error_text(&page).to_lowercase();
    assert!(
        words.contains("whether"),
        "expected the words of a disable's `503` to say it cannot be told whether it was made, got {words:?}"
    );
    // Once the platform database can be reached again, the pages answer
    // again: the Resources and the installation too, which show what the
    // program holds in memory but read the database all the same
    // (`10e00ff`).
    postgres
        .start_again()
        .unwrap_or_else(|e| panic!("the harness could not start its Postgres again: {e}"));
    for target in [
        RESOURCES,
        INSTALLATION,
        OUTBOUND,
        APPLICATIONS,
        OPERATION_LOG,
    ] {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let response = dashboard
                .navigation("GET", target)
                .timeout(Duration::from_secs(40))
                .send();
            if response.status == 200 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{target}: expected `200` again within 30 s of the platform database coming back\n{}",
                response.describe()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
