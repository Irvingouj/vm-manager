//! Who is refused on a Resource's page and its forms (CONTRACT.md, "Resources
//! and Grants in full: The Dashboard: a Resource's page and its Grants" and
//! "The Dashboard for Platform administrators: Who is refused"), with the
//! stand-in sign-in: for someone who is not a Platform administrator (a
//! Contributor, the Resource's owner, a User that only carries the
//! administrator's id), each of the five requests (the list, the Resource's
//! page, the setting, the page that confirms a revoke, the revoke) is the
//! refusal page, byte for byte the same body whatever the path, the
//! parameters and the body, before `404`, `405`, `400`, `408` and `413`;
//! nothing is changed and no record is logged. Someone not signed in is
//! answered as on every page. From another origin the two `POST`s (and a
//! `GET` that is not a navigation) are `403` `text/plain`, also for a
//! Platform administrator, and change nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin_pages::{
    RESOURCES, api_grants, assert_not_an_administrator, grants_target, resource_target,
    revoke_page_target, revoke_target,
};
use blackbox::dashboard::{Dashboard, administrator, form_body, stand_in_options};
use blackbox::records::event;
use blackbox::resources::records_waited;
use blackbox::scenario::{ADMINISTRATORS, Scenario};
use blackbox::{Request, Response, User};

const NAME: &str = "devops";

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn mallory() -> User {
    User::new("mallory").in_groups(&["Platform-Admins", "platform-admins-x", "devops"])
}

/// Everyone who is not a Platform administrator that the tests try: a
/// Contributor, someone in a Group that is not the administrators', someone
/// who has the administrator's id and none of their Groups, and the
/// Resource's owner as it is written in its registration.
fn not_administrators() -> Vec<User> {
    vec![
        alice(),
        mallory(),
        User::new(&administrator().id),
        User::new("The DevOps team"),
    ]
}

fn world() -> Scenario {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    s.grant(NAME, "sre", "write");
    s
}

fn records_of_grants(s: &Scenario) -> usize {
    ["grant-set", "grant-revoked", "grant-unsettled"]
        .iter()
        .map(|e| records_waited(&s.vm, e, 0).len())
        .sum()
}

/// The `GET`s of the refusal, with every name and parameter the pages have
/// and some they have not.
fn refused_gets() -> Vec<String> {
    vec![
        RESOURCES.to_owned(),
        format!("{RESOURCES}?after=Not_A_Name"),
        format!("{RESOURCES}?after=devops&x=%%%"),
        resource_target(NAME),
        format!("{}?changed=granted&group=sre", resource_target(NAME)),
        format!("{}?changed=%%%&group=%FF", resource_target(NAME)),
        resource_target("nope"),
        resource_target("Not_A_Name"),
        resource_target("a%20b"),
        resource_target("%00"),
        revoke_page_target(NAME, "sre"),
        revoke_page_target(NAME, "nobody"),
        revoke_target(NAME),
        format!("{}?group=a&group=b", revoke_target(NAME)),
        format!("{}?group=", revoke_target(NAME)),
        revoke_page_target("nope", "sre"),
        revoke_target("Not_A_Name"),
        grants_target(NAME),
        format!("{}/nope", resource_target(NAME)),
        format!("{}/grants/sre", resource_target(NAME)),
    ]
}

/// The `POST`s of the refusal: (target, body).
fn refused_posts() -> Vec<(String, String)> {
    let valid = form_body(&[("group", "sre"), ("access", "read")]);
    let revoke = form_body(&[("group", "sre")]);
    vec![
        (grants_target(NAME), valid.clone()),
        (grants_target(NAME), String::new()),
        (grants_target(NAME), "%%%".to_owned()),
        (
            grants_target(NAME),
            "group=a&group=b&access=read".to_owned(),
        ),
        (
            grants_target(NAME),
            format!("{valid}&pad={}", "a".repeat(8 * 1024)),
        ),
        (grants_target("nope"), valid.clone()),
        (grants_target("Not_A_Name"), valid),
        (revoke_target(NAME), revoke.clone()),
        (revoke_target(NAME), String::new()),
        (revoke_target(NAME), "group=sre&group=staff".to_owned()),
        (
            revoke_target(NAME),
            format!("{revoke}&pad={}", "a".repeat(8 * 1024)),
        ),
        (revoke_target("nope"), revoke.clone()),
        (revoke_target("Not_A_Name"), revoke),
        (RESOURCES.to_owned(), "after=devops".to_owned()),
        (resource_target(NAME), String::new()),
        (format!("{}/nope", resource_target(NAME)), String::new()),
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
fn everyone_who_is_not_a_platform_administrator_is_refused_on_each_of_the_five_requests_with_one_body()
 {
    let s = world();
    let base = Dashboard::new(&s.vm, &alice()).url("/");
    let held = api_grants(&s.vm, NAME);
    let records = records_of_grants(&s);
    assert_eq!(
        records, 1,
        "the harness's own count: the setup's one record"
    );
    for person in not_administrators() {
        let who = person.id.clone();
        let dashboard = Dashboard::new(&s.vm, &person);
        let mut first = None;
        for path in refused_gets() {
            let what = format!("GET {path}");
            let page =
                assert_not_an_administrator(dashboard.get(&path), &base, &format!("{who}: {what}"));
            assert_same_body(&mut first, &page.response, &what, &who);
            let (id, _) = page.user();
            assert_eq!(id, who, "{who}: {what}: the header is the person's");
            let text = page.response.text();
            for named in [NAME, "sre", ADMINISTRATORS, "staff"] {
                assert!(
                    !text.contains(named),
                    "{who}: {what}: the refusal names no Resource and no Group, found {named:?}"
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
            ("PUT", grants_target(NAME)),
            ("DELETE", grants_target(NAME)),
            ("PUT", revoke_target(NAME)),
            ("DELETE", revoke_target(NAME)),
            ("PATCH", resource_target(NAME)),
            ("DELETE", resource_target(NAME)),
            ("PUT", RESOURCES.to_owned()),
        ] {
            let what = format!("{method} {target}");
            let answer = dashboard.form_request(&target, "").method(method).send();
            let page = assert_not_an_administrator(answer, &base, &format!("{who}: {what}"));
            assert_same_body(&mut first, &page.response, &what, &who);
        }
    }
    assert_eq!(api_grants(&s.vm, NAME), held, "nothing is changed");
    assert_eq!(
        records_of_grants(&s),
        records,
        "no record of a Grant is logged for a refused request"
    );
}

#[test]
fn the_refusal_comes_before_a_body_is_waited_for_or_its_length_looked_at() {
    let s = world();
    for person in [alice(), mallory()] {
        let dashboard = Dashboard::new(&s.vm, &person);
        let base = dashboard.url("/");
        for target in [grants_target(NAME), revoke_target(NAME)] {
            for (what, declared) in [
                ("a body that never comes whole (else `408`)", 64),
                (
                    "a body declared over 16 MiB (else `413`)",
                    16 * 1024 * 1024 + 1,
                ),
            ] {
                let request = dashboard
                    .form_request(&target, "group=sre&access=read")
                    .declare_length(declared)
                    .timeout(Duration::from_secs(30));
                let started = Instant::now();
                let answer = request.try_send().unwrap_or_else(|e| {
                    panic!("{what}: expected the `403` refusal page, no answer ({e})")
                });
                let waited = started.elapsed();
                assert_not_an_administrator(answer, &base, &format!("{target}: {what}"));
                assert!(
                    waited < Duration::from_secs(5),
                    "{target}: {what}: expected the refusal without waiting for the body, it came after {waited:?}"
                );
            }
        }
    }
    assert_eq!(
        api_grants(&s.vm, NAME),
        [("sre".to_owned(), "write".to_owned())],
        "nothing is changed"
    );
}

#[test]
fn a_contributor_and_the_resources_owner_cannot_give_or_revoke_a_grant() {
    let s = world();
    for person in [alice(), User::new("The DevOps team")] {
        let dashboard = Dashboard::new(&s.vm, &person);
        let base = dashboard.url("/");
        assert_not_an_administrator(
            dashboard.get(&resource_target(NAME)),
            &base,
            "the Resource's page",
        );
        assert_not_an_administrator(
            dashboard.post_form(&grants_target(NAME), "group=mine&access=write"),
            &base,
            "giving a Group access",
        );
        assert_not_an_administrator(
            dashboard.get(&revoke_page_target(NAME, "sre")),
            &base,
            "the confirmation of a revoke",
        );
        assert_not_an_administrator(
            dashboard.post_form(&revoke_target(NAME), "group=sre"),
            &base,
            "a revoke",
        );
    }
    assert_eq!(
        api_grants(&s.vm, NAME),
        [("sre".to_owned(), "write".to_owned())],
        "nothing is changed"
    );
    assert_eq!(
        records_waited(&s.vm, "grant-set", 0).len(),
        1,
        "no record but the setup's"
    );
    assert!(records_waited(&s.vm, "grant-revoked", 0).is_empty());
}

#[test]
fn someone_not_signed_in_is_401_on_each_of_the_five_requests_and_nothing_changes() {
    let s = world();
    let host = format!("dashboard.vm.test:{}", s.vm.address().port());
    for (method, target) in [
        ("GET", RESOURCES.to_owned()),
        ("HEAD", resource_target(NAME)),
        ("GET", resource_target(NAME)),
        ("GET", resource_target("nope")),
        ("POST", grants_target(NAME)),
        ("GET", revoke_page_target(NAME, "sre")),
        ("POST", revoke_target(NAME)),
        ("DELETE", revoke_target(NAME)),
    ] {
        let answer = Request::new(s.vm.address(), &target, &host)
            .method(method)
            .header("accept", "text/html")
            .header("content-type", "application/x-www-form-urlencoded")
            .body("group=sre")
            .send();
        assert_eq!(
            answer.status,
            401,
            "{method} {target} not signed in\n{}",
            answer.describe()
        );
    }
    assert_eq!(
        api_grants(&s.vm, NAME),
        [("sre".to_owned(), "write".to_owned())]
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
fn a_form_or_a_fetch_from_another_origin_or_an_iframe_is_refused_also_for_an_administrator() {
    let s = world();
    let port = s.vm.address().port();
    let board_origin = format!("http://board.vm.test:{port}");
    for person in [administrator(), alice()] {
        let who = person.id.clone();
        let dashboard = Dashboard::new(&s.vm, &person);
        for (target, body) in [
            (grants_target(NAME), "group=elsewhere&access=write"),
            (revoke_target(NAME), "group=sre"),
        ] {
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
                    .body(body)
                    .send();
                assert_plain_403(
                    &answer,
                    &format!("{who}: a form of {origin} posted to {target}"),
                );
            }
        }
        for path in [
            resource_target(NAME),
            revoke_page_target(NAME, "sre"),
            RESOURCES.to_owned(),
        ] {
            let fetched = dashboard
                .bare("GET", &path)
                .header("origin", &board_origin)
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "cors")
                .header("sec-fetch-dest", "empty")
                .send();
            assert_plain_403(
                &fetched,
                &format!("{who}: a fetch of {path} from a page of an Application"),
            );
            let framed = dashboard
                .bare("GET", &path)
                .header("accept", "text/html")
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "iframe")
                .send();
            assert_plain_403(&framed, &format!("{who}: {path} in an <iframe>"));
        }
    }
    assert_eq!(
        api_grants(&s.vm, NAME),
        [("sre".to_owned(), "write".to_owned())],
        "nothing is changed"
    );
    assert_eq!(records_waited(&s.vm, "grant-set", 0).len(), 1);
    assert!(records_waited(&s.vm, "grant-revoked", 0).is_empty());
    let records: Vec<String> =
        s.vm.records()
            .iter()
            .map(|r| event(r).to_owned())
            .filter(|e| e == "grant-unsettled")
            .collect();
    assert!(records.is_empty());
}
