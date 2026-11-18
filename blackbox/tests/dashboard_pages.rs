//! What every answer of the Dashboard keeps (CONTRACT.md, "The Dashboard:
//! What it answers" and "Signing in to it"), with the stand-in sign-in:
//! pages of the platform's own with its headers and `form-action 'self'`,
//! no script, whom the person is signed in as, everything shown escaped as
//! HTML, `404` and `405` with `allow`, `HEAD`, `503` with `retry-after`
//! while the platform database cannot be reached, the `request` record.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::dashboard::{
    Dashboard, Page, assert_refused, create, deploy, form_body, stand_in_options, start,
};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::sharing::{add, assert_own_page_headers, contributors_ok, group, share_ok};
use blackbox::{Request, User, Vm};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice")
}

fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some("The incident board"));
    deploy(&vm, &alice(), APP, &[]);
    create(&vm, &User::new("bob"), "bobs", None);
    vm
}

/// Fails unless the page says it is alice's: `data-vm-user="alice"`, and,
/// with the stand-in, no link to sign out.
fn assert_alices(page: &Page, context: &str) {
    let (id, text) = page.user();
    assert_eq!(
        id,
        "alice",
        "{context}: expected `data-vm-user=\"alice\"`\n{}",
        page.describe()
    );
    assert!(
        text.contains("alice"),
        "{context}: expected the element `data-vm-user` to say whom they are signed in as, got {text:?}"
    );
    assert!(
        !page.hrefs().iter().any(|h| h.contains("sign-out")),
        "{context}: expected no link to sign out with the stand-in\n{}",
        page.describe()
    );
}

#[test]
fn every_answer_is_a_page_of_the_platforms_own_that_says_whom_they_are_signed_in_as() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let cases: Vec<(&str, u16, blackbox::Response)> = vec![
        ("the list", 200, dashboard.get("/")),
        ("a page past the last", 200, dashboard.get("/?page=2")),
        ("a page that is no number", 400, dashboard.get("/?page=x")),
        (
            "an Application's page",
            200,
            dashboard.get("/applications/board"),
        ),
        (
            "with a notice",
            200,
            dashboard.get("/applications/board?changed=shared"),
        ),
        (
            "an Application not shared with her",
            403,
            dashboard.get("/applications/bobs"),
        ),
        (
            "an Application of no name it has",
            404,
            dashboard.get("/applications/nope"),
        ),
        ("a path the Dashboard has not", 404, dashboard.get("/nope")),
        (
            "another method",
            405,
            dashboard.navigation("DELETE", "/").send(),
        ),
        (
            "a refused change",
            404,
            dashboard.post_form(
                "/applications/nope/sharing",
                "action=share&kind=user&value=bob",
            ),
        ),
    ];
    for (what, status, response) in cases {
        assert_eq!(
            response.status,
            status,
            "{what}: expected `{status}`\n{}",
            response.describe()
        );
        let page = Page::read(response, dashboard.url("/"), what);
        assert_alices(&page, what);
    }
}

#[test]
fn other_paths_are_404() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    for target in [
        "/nope",
        "/applications",
        "/applications/",
        "/applications/board/",
        "/applications/board/extra",
        "/applications/board/sharing/x",
        "/favicon.ico",
        "/v1/applications",
        "/_vm/nothing",
    ] {
        let page = dashboard.page(
            target,
            404,
            &format!("{target}: a path the Dashboard has not"),
        );
        assert_alices(&page, target);
    }
}

fn allowed(response: &blackbox::Response) -> Vec<String> {
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
fn another_method_on_a_path_it_has_is_405_with_allow() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    for (method, target, wanted) in [
        ("POST", "/", vec!["GET", "HEAD"]),
        ("DELETE", "/", vec!["GET", "HEAD"]),
        ("PUT", "/applications/board", vec!["GET", "HEAD"]),
        ("POST", "/applications/board", vec!["GET", "HEAD"]),
        ("GET", "/applications/board/sharing", vec!["POST"]),
        ("DELETE", "/applications/board/sharing", vec!["POST"]),
    ] {
        let response = if method == "POST" {
            dashboard.post_form(target, &form_body(&[("action", "share")]))
        } else {
            dashboard.navigation(method, target).send()
        };
        let context = format!("{method} {target}");
        assert_eq!(
            response.status,
            405,
            "{context}: expected `405`\n{}",
            response.describe()
        );
        let allow = allowed(&response);
        for m in &wanted {
            assert!(
                allow.iter().any(|a| a == m),
                "{context}: expected `allow` to name {m}, got {allow:?}\n{}",
                response.describe()
            );
        }
        assert!(
            !allow.iter().any(|a| a == method),
            "{context}: expected `allow` not to name {method}, got {allow:?}"
        );
        Page::read(response, dashboard.url(target), &context);
    }
}

#[test]
fn head_is_answered_as_get_without_a_body() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    for (target, status) in [
        ("/", 200),
        ("/applications/board", 200),
        ("/applications/bobs", 403),
        ("/applications/nope", 404),
    ] {
        let get = dashboard.get(target);
        let head = dashboard.navigation("HEAD", target).send();
        assert_eq!(
            (get.status, head.status),
            (status, status),
            "HEAD {target}: expected the status of GET, {status}\nGET:\n{}\nHEAD:\n{}",
            get.describe(),
            head.describe()
        );
        assert!(head.body().is_empty(), "HEAD {target}: expected no body");
        assert_own_page_headers(&head, &format!("HEAD {target}"));
    }
}

#[test]
fn everything_shown_is_escaped_as_html() {
    let vm = start();
    let hostile_user = User::new("<i>o'neil & \"co\"</i>");
    let description = "<script>alert(1)</script> & \"q\" 'a'";
    let hostile_group = "<b>\"g\" & 'h'</b>";
    create(&vm, &hostile_user, APP, Some(description));
    deploy(&vm, &hostile_user, APP, &[]);
    contributors_ok(
        &vm.api_as(&hostile_user),
        &vm,
        APP,
        &add(&[group(hostile_group)]),
    );
    share_ok(
        &vm.api_as(&hostile_user),
        &vm,
        APP,
        &add(&[group(hostile_group)]),
    );
    let dashboard = Dashboard::new(&vm, &hostile_user);
    for page in [dashboard.list(), dashboard.details(APP)] {
        let (id, text) = page.user();
        assert_eq!(
            id,
            hostile_user.id,
            "expected `data-vm-user` to hold the id exactly\n{}",
            page.describe()
        );
        assert!(
            text.contains(&hostile_user.id),
            "expected the id shown as those characters, got {text:?}"
        );
        let board = page.listed(APP);
        assert_eq!(
            board.description(),
            Some(description),
            "expected the description shown as those characters\n{}",
            page.describe()
        );
        assert_eq!(
            board.contributor_keys(),
            vec![
                format!("user:{}", hostile_user.id),
                format!("group:{hostile_group}")
            ],
            "expected each Contributor's `data-vm-id` to hold it exactly\n{}",
            page.describe()
        );
        for shown in &board.contributors {
            let id = shown.id.clone().unwrap_or_default();
            assert!(
                shown.text.contains(&id),
                "expected {id:?} shown as those characters, got {:?}",
                shown.text
            );
        }
        let html = page.response.text();
        for raw in ["<script>alert", "<i>o'neil", "<b>\"g\"", "\"co\"</i>"] {
            assert!(
                !html.contains(raw),
                "expected everything that is not the platform's own text escaped as HTML, but the page holds {raw:?}\n{html}"
            );
        }
    }
    let details = dashboard.details(APP);
    assert_eq!(
        details.shared_keys(),
        vec![format!("group:{hostile_group}")]
    );
}

#[test]
fn with_the_platform_database_stopped_a_page_is_503_with_retry_after() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_dashboard_stopped");
    let vm = Vm::start(&stand_in_options().platform(&platform));
    create(&vm, &alice(), APP, None);
    deploy(&vm, &alice(), APP, &[]);
    let dashboard = Dashboard::new(&vm, &alice());
    dashboard.list();
    // The form of a change, as the browser holds it before the stop.
    let change = dashboard
        .form_request(
            &Dashboard::sharing_target(APP),
            &form_body(&[("action", "share"), ("kind", "user"), ("value", "bob")]),
        )
        .timeout(Duration::from_secs(40));
    postgres.stop();
    for target in ["/", "/applications/board"] {
        let response = dashboard
            .navigation("GET", target)
            .timeout(Duration::from_secs(40))
            .send();
        assert!(
            response.status == 503 && response.header("retry-after").is_some(),
            "{target}: expected `503` with `retry-after` while the platform database cannot be reached\n{}",
            response.describe()
        );
        let page = Page::read(response, dashboard.url(target), target);
        assert!(
            page.text().to_lowercase().contains("again"),
            "{target}: expected a page saying to try again shortly\n{}",
            page.describe()
        );
    }
    // A change: `503` with `retry-after` and `data-vm-error="unavailable"`.
    assert_refused(
        change.send(),
        &dashboard.url("/"),
        503,
        "unavailable",
        APP,
        "a change of sharing while the platform database cannot be reached",
    );
}

#[test]
fn a_request_to_the_dashboard_has_a_request_record_with_no_application() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    for (target, path, status) in [
        ("/", "/", 200),
        (
            "/applications/board?changed=shared",
            "/applications/board",
            200,
        ),
        ("/nope", "/nope", 404),
    ] {
        let response = dashboard.get(target);
        assert_eq!(response.status, status, "{target}\n{}", response.describe());
        let record = vm.records_of(&response).request;
        for (field, wanted) in [
            ("application", serde_json::Value::Null),
            ("version", serde_json::Value::Null),
            ("user", serde_json::json!("alice")),
            ("method", serde_json::json!("GET")),
            ("path", serde_json::json!(path)),
            ("status", serde_json::json!(status)),
        ] {
            assert_eq!(
                record[field], wanted,
                "{target}: expected `{field}` {wanted} in the `request` record, got {record}"
            );
        }
    }
}

#[test]
fn not_signed_in_is_401_as_everywhere() {
    let vm = with_board();
    for (method, target) in [
        ("GET", "/"),
        ("GET", "/applications/board"),
        ("POST", "/applications/board/sharing"),
    ] {
        let response = Request::new(
            vm.address(),
            target,
            &format!("dashboard.vm.test:{}", vm.address().port()),
        )
        .method(method)
        .header("accept", "text/html")
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
}
