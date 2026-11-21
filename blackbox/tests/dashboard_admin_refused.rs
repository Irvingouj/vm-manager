//! Who is refused under `/admin` (CONTRACT.md, "The Dashboard for Platform
//! administrators: Who is refused"): everyone who is not a Platform
//! administrator gets `403` and the refusal page on every path that is
//! `/admin` or begins with `/admin/`, by every method; for one person the
//! body is the same byte for byte whatever the path, the parameters, the
//! body and whatever exists; it comes before `404`, `405`, `400`, `408` and
//! `413`; nothing is changed; a Contributor of the Application is refused as
//! anyone else; someone not signed in is answered as on every path of the
//! Dashboard; a form or a fetch from another origin is refused `403`
//! `text/plain` also for a Platform administrator, an `<iframe>` likewise;
//! with OIDC, a Platform administrator taken out of the Group is refused
//! from the next renewal on, a confirmation page loaded before and
//! submitted after included.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{disable_ok, logged};
use blackbox::admin_pages::{
    APPLICATIONS, INSTALLATION, OPERATION_LOG, RESOURCES, assert_admin_link, assert_no_admin_trace,
    assert_not_an_administrator, disable_target, enable_target, get_paths,
};
use blackbox::browser::describe_steps;
use blackbox::dashboard::{
    Dashboard, Page, administrator, create, deploy, oidc_page, oidc_post, oidc_sign_in, oidc_url,
    start,
};
use blackbox::oidc::Person;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, Installation, grace};
use blackbox::{Request, Response, User, Vm};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["finance", "devops"])
}

/// `board`, alice's, with a Version.
fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some("The incident board"));
    deploy(&vm, &alice(), APP, &[]);
    vm
}

fn disabled_now(vm: &Vm) -> serde_json::Value {
    vm.api_as(&alice()).application(APP)["disabled"].clone()
}

/// The paths of the refusal for `GET`: those of the table, and others.
fn refused_gets() -> Vec<String> {
    let mut paths = get_paths(APP);
    paths.extend(
        [
            "/admin/",
            "/admin/nope",
            "/admin/applications/nope",
            "/admin/applications/nope/disable",
            "/admin/applications/NOT%20A%20NAME/disable",
            "/admin/applications?after=Not_A_Name",
            "/admin/applications?after=board&x=%%%",
            "/admin/operation-log?outcome=nonsense&from=yesterday",
            "/admin/operation-log?application=board",
            "/admin/resources?x=1",
            "/admin/installation/more",
            "/admin/applications/board/enable",
        ]
        .map(str::to_owned),
    );
    paths
}

/// The `POST`s of the refusal: (target, body).
fn refused_posts() -> Vec<(String, String)> {
    vec![
        (disable_target(APP), String::new()),
        (enable_target(APP), String::new()),
        (disable_target(APP), "x=1&x=2".to_owned()),
        (enable_target(APP), "%%%".to_owned()),
        (disable_target("nope"), String::new()),
        (
            "/admin/applications/NOT%20A%20NAME/disable".to_owned(),
            String::new(),
        ),
        (disable_target(APP), format!("x={}", "a".repeat(8 * 1024))),
        (APPLICATIONS.to_owned(), "after=board".to_owned()),
        (OPERATION_LOG.to_owned(), String::new()),
        ("/admin".to_owned(), String::new()),
        ("/admin/nope".to_owned(), String::new()),
    ]
}

fn assert_same_body(
    first: &mut Option<(String, Vec<u8>)>,
    response: &Response,
    what: &str,
    who: &str,
) {
    match first {
        None => *first = Some((what.to_owned(), response.body().to_vec())),
        Some((was, body)) => assert!(
            body.as_slice() == response.body(),
            "{who}: expected the refusal of {what} to be byte for byte that of {was}\n{}",
            response.describe()
        ),
    }
}

#[test]
fn everyone_who_is_not_a_platform_administrator_is_refused_on_every_path_and_method_with_one_body()
{
    let vm = with_board();
    let base = Dashboard::new(&vm, &alice()).url("/");
    for person in [alice(), bob(), carol()] {
        let who = person.id.clone();
        let dashboard = Dashboard::new(&vm, &person);
        let mut first = None;
        for path in refused_gets() {
            let what = format!("GET {path}");
            let page =
                assert_not_an_administrator(dashboard.get(&path), &base, &format!("{who}: {what}"));
            assert_same_body(&mut first, &page.response, &what, &who);
            let (id, _) = page.user();
            assert_eq!(
                id, who,
                "{who}: {what}: the header is the person's, as on every page"
            );
            let text = page.response.text();
            for named in [
                administrator().id.as_str(),
                ADMINISTRATORS,
                "staff",
                "finance",
                "devops",
            ] {
                assert!(
                    !text.contains(named),
                    "{who}: {what}: the refusal names no Platform administrator and no Group, found {named:?}"
                );
            }
            let head = dashboard.navigation("HEAD", &path).send();
            assert!(
                head.status == 403 && head.body().is_empty(),
                "{who}: HEAD {path}: expected `403` without a body\n{}",
                head.describe()
            );
        }
        for (target, body) in refused_posts() {
            let what = format!("POST {target} with a body of {} bytes", body.len());
            let answer = dashboard.post_form(&target, &body);
            let page = assert_not_an_administrator(answer, &base, &format!("{who}: {what}"));
            assert_same_body(&mut first, &page.response, &what, &who);
        }
        for (method, target) in [
            ("PUT", disable_target(APP)),
            ("DELETE", APPLICATIONS.to_owned()),
            ("PATCH", RESOURCES.to_owned()),
            ("DELETE", "/admin/nope".to_owned()),
            ("POST", INSTALLATION.to_owned()),
        ] {
            let what = format!("{method} {target}");
            let answer = dashboard.form_request(&target, "").method(method).send();
            let page = assert_not_an_administrator(answer, &base, &format!("{who}: {what}"));
            assert_same_body(&mut first, &page.response, &what, &who);
        }
    }
    assert!(
        disabled_now(&vm).is_null(),
        "nothing is changed by a refused request"
    );
    assert!(logged(&vm, "application-disabled").is_empty());
    assert!(logged(&vm, "application-enabled").is_empty());
}

#[test]
fn the_refusal_comes_before_a_body_is_waited_for_or_its_length_looked_at() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &bob());
    let base = dashboard.url("/");
    for (what, declared) in [
        ("a body that never comes whole (else `408`)", 64),
        (
            "a body declared over 16 MiB (else `413`)",
            16 * 1024 * 1024 + 1,
        ),
    ] {
        let request = dashboard
            .form_request(&disable_target(APP), "x=1")
            .declare_length(declared)
            .timeout(Duration::from_secs(30));
        let started = Instant::now();
        let answer = request
            .try_send()
            .unwrap_or_else(|e| panic!("{what}: expected the `403` refusal page, no answer ({e})"));
        let waited = started.elapsed();
        assert_not_an_administrator(answer, &base, what);
        assert!(
            waited < Duration::from_secs(5),
            "{what}: expected the refusal without waiting for the body, it came after {waited:?}"
        );
    }
    assert!(disabled_now(&vm).is_null(), "nothing is changed");
}

#[test]
fn a_contributor_is_refused_on_its_disable_and_enable_and_nothing_changes() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let base = dashboard.url("/");
    assert_not_an_administrator(
        dashboard.get(&disable_target(APP)),
        &base,
        "alice's confirmation page",
    );
    assert_not_an_administrator(
        dashboard.post_form(&disable_target(APP), ""),
        &base,
        "alice disabling her own Application",
    );
    assert!(disabled_now(&vm).is_null(), "still not disabled");
    let before = disable_ok(&vm, APP)["disabled"].clone();
    assert_not_an_administrator(
        dashboard.post_form(&enable_target(APP), ""),
        &base,
        "alice enabling her own Application",
    );
    assert_eq!(disabled_now(&vm), before, "still disabled, as it was");
    assert!(logged(&vm, "application-enabled").is_empty());
    assert_eq!(
        logged(&vm, "application-disabled").len(),
        1,
        "only the administrator's"
    );
}

#[test]
fn a_path_that_only_begins_with_admin_is_not_refused_it_is_404() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &bob());
    for path in ["/adminx", "/administration", "/admins/applications"] {
        let answer = dashboard.get(path);
        assert_eq!(
            answer.status,
            404,
            "{path}: not `/admin` nor under `/admin/`: the Dashboard's `404`, not the refusal\n{}",
            answer.describe()
        );
        let page = Page::read(answer, dashboard.url(path), path);
        assert!(
            page.errors()
                .iter()
                .all(|(r, _)| r != "not-an-administrator"),
            "{path}: not the refusal"
        );
    }
}

#[test]
fn a_form_or_a_fetch_from_another_origin_or_an_iframe_is_refused_also_for_an_administrator() {
    let vm = with_board();
    let port = vm.address().port();
    let board_origin = format!("http://board.vm.test:{port}");
    for person in [administrator(), alice()] {
        let who = person.id.clone();
        let dashboard = Dashboard::new(&vm, &person);
        for target in [disable_target(APP), enable_target(APP)] {
            for (site, origin) in [
                ("same-site", board_origin.as_str()),
                ("cross-site", "https://elsewhere.example"),
            ] {
                let answer = dashboard
                    .bare("POST", &target)
                    .header("accept", "text/html")
                    .header("origin", origin)
                    .header("sec-fetch-site", site)
                    .header("sec-fetch-mode", "navigate")
                    .header("sec-fetch-dest", "document")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body("")
                    .send();
                assert_plain_403(
                    &answer,
                    &format!("{who}: a form of {origin} posted to {target}"),
                );
            }
        }
        for path in [
            APPLICATIONS.to_owned(),
            disable_target(APP),
            OPERATION_LOG.to_owned(),
        ] {
            let framed = dashboard
                .bare("GET", &path)
                .header("accept", "text/html")
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "iframe")
                .send();
            assert_plain_403(
                &framed,
                &format!("{who}: {path} in an <iframe> of an Application's page"),
            );
        }
    }
    assert!(disabled_now(&vm).is_null(), "nothing is changed");
    assert!(logged(&vm, "application-disabled").is_empty());
    // A page the administrator is answered forbids being framed.
    let page = Dashboard::new(&vm, &administrator()).get(APPLICATIONS);
    let policy = page.header("content-security-policy").unwrap_or_default();
    assert!(
        policy.contains("frame-ancestors 'none'"),
        "expected `frame-ancestors 'none'` on an administrator's page, got {policy:?}\n{}",
        page.describe()
    );
}

fn assert_plain_403(answer: &Response, context: &str) {
    assert!(
        answer.status == 403
            && answer
                .header("content-type")
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
        "{context}: expected the origin rule's `403`, `text/plain`\n{}",
        answer.describe()
    );
}

#[test]
fn with_the_stand_in_and_no_headers_it_is_401_and_nothing_changes() {
    let vm = with_board();
    let host = format!("dashboard.vm.test:{}", vm.address().port());
    for (method, target) in [
        ("GET", disable_target(APP)),
        ("HEAD", RESOURCES.to_owned()),
        ("POST", disable_target(APP)),
    ] {
        let answer = Request::new(vm.address(), &target, &host)
            .method(method)
            .header("accept", "text/html")
            .header("content-type", "application/x-www-form-urlencoded")
            .body("")
            .send();
        assert_eq!(
            answer.status,
            401,
            "{method} {target} not signed in\n{}",
            answer.describe()
        );
    }
    assert!(disabled_now(&vm).is_null(), "nothing is changed");
}

fn ada_admin() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

#[test]
fn with_oidc_someone_not_signed_in_signs_in_and_comes_back_to_the_refusal_or_is_401() {
    let at = Installation::start_with(&[("honest", "honest.js")], &[ada_admin(), grace()], &[]);
    let mut browser = at.browser_as(GRACE);
    let address = oidc_url(&at, APPLICATIONS);
    // Anything but a page: `401`, `text/plain`.
    for method in ["GET", "POST"] {
        let request = browser
            .request(method, &address)
            .header("accept", "application/json");
        let answer = browser.send(&address, &request);
        assert!(
            answer.status == 401
                && answer
                    .header("content-type")
                    .is_some_and(|t| t.starts_with("text/plain")),
            "{method} {APPLICATIONS} without a session, not a page: expected `401` `text/plain`\n{}",
            answer.describe()
        );
    }
    let steps = browser.navigate(&address);
    assert!(
        steps.first().is_some_and(|s| s.response.status == 302),
        "a page without a session: expected `302` to sign in first\n{}",
        describe_steps(&steps)
    );
    let last = steps.last().unwrap();
    assert!(
        last.url == address,
        "expected to come back to the address asked for once signed in\n{}",
        describe_steps(&steps)
    );
    let page = assert_not_an_administrator(
        last.response.clone(),
        &address,
        "Grace back at every Application",
    );
    assert_no_admin_trace(&page, "Grace");
    // Ada, a Platform administrator, comes back to the page itself.
    let mut browser = at.browser_as(ADA);
    let steps = browser.navigate(&address);
    let last = steps.last().unwrap();
    assert!(
        last.url == address && last.response.status == 200,
        "Ada: expected to come back to every Application, `200`, once signed in\n{}",
        describe_steps(&steps)
    );
}

#[test]
fn with_oidc_an_administrator_taken_out_of_the_group_is_refused_from_the_next_renewal_on() {
    let renewal = Duration::from_secs(3);
    let at = Installation::start_with(
        &[("honest", "honest.js")],
        &[ada_admin(), grace()],
        &["--session-renewal", "3"],
    );
    let mut browser = at.browser_as(ADA);
    let (_, list) = oidc_sign_in(&at, &mut browser);
    let signed_in = Instant::now();
    assert_admin_link(&list, "Ada, a Platform administrator");
    let confirm = oidc_page(
        &at,
        &mut browser,
        &disable_target("honest"),
        200,
        "the confirmation, before",
    );
    let forms = confirm.forms_of("disable");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one disable form on the confirmation page\n{}",
            confirm.describe()
        );
    };
    let action = confirm
        .url
        .join(form.action.as_deref().unwrap_or_default())
        .unwrap();
    let body = form.body(&[]);
    at.provider.set_groups(ADA, &["finance"]);
    assert!(
        signed_in.elapsed() < renewal,
        "the test was too slow ({:?})",
        signed_in.elapsed()
    );
    let until = signed_in + renewal + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));

    let answer = oidc_post(&mut browser, &confirm, &action, &body);
    assert_not_an_administrator(
        answer,
        &action,
        "the confirmation submitted after the renewal",
    );
    let credential = at.tool_credential_for(GRACE);
    let honest = blackbox::tool::bearer(&at.vm, "/v1/applications/honest", &credential).send();
    assert!(
        honest.status == 200 && honest.json()["disabled"].is_null(),
        "expected honest still not disabled\n{}",
        honest.describe()
    );
    let mut grace_browser = at.browser_as(GRACE);
    at.sign_in(&mut grace_browser, "honest", "/");
    let seen = at.get_json(&mut grace_browser, "honest", "/");
    assert_eq!(seen.status, 200, "honest still serves\n{}", seen.describe());

    let home = oidc_page(&at, &mut browser, "/", 200, "the list after the renewal");
    assert_no_admin_trace(&home, "taken out of the Group, after the renewal");
    for path in [
        APPLICATIONS.to_owned(),
        RESOURCES.to_owned(),
        disable_target("honest"),
    ] {
        let step = browser.open(&oidc_url(&at, &path));
        assert_not_an_administrator(
            step.response,
            &oidc_url(&at, &path),
            &format!("{path} after the renewal"),
        );
    }
    let own = oidc_page(
        &at,
        &mut browser,
        "/applications/honest",
        200,
        "honest's page after the renewal",
    );
    for absent in [
        "data-vm-usage",
        "data-vm-disable",
        "data-vm-operation-log",
        "data-vm-disabled-by",
    ] {
        assert_eq!(
            own.count(absent),
            0,
            "honest's page after the renewal: no `{absent}`"
        );
    }
}
