//! `vibe dev` in a browser: which requests it serves, and choosing the test
//! User (CONTRACT.md, "The local loop, kept running: the browser, test rows
//! and reset": "Which requests it serves", "Choosing the test User"; ticket
//! "Local loop in the browser, with test rows, reset and working offline":
//! opening the local address in a browser shows the choose-a-test-User
//! page, then the Application as that User; switching User changes what is
//! shown; the choose-a-test-User page exists only in local mode).
//!
//! The browser is the harness's (`blackbox/src/dev.rs`, `Browser`): it
//! connects to `127.0.0.1:<port>`, names the Application in `Host`, keeps
//! cookies by hand and follows no redirect. Whether the Application's code
//! ran is seen from what it writes: its `/notes` (`POST`) inserts a note
//! into its own database, and a note that is not there afterwards was never
//! written.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::time::Duration;

use blackbox::User;
use blackbox::deploy::{Deployed, commit_all, deploy, init_repository};
use blackbox::dev::{Browser, Dev, html_unescaped, percent_decode, percent_encode};
use blackbox::generate::{Generated, generate};
use blackbox::http::Response;
use blackbox::local::{
    APP, BUILDER, Board, DEVOPS, Local, OUTSIDER, dev_json, handler, local_project, write_dev_json,
};
use blackbox::project::NOTES_SCHEMA_TS;
use blackbox::sdk::TempProject;
use serde_json::{Value, json};

const NAME: &str = "browser-board";

/// A project declaring nothing whose own database has `notes`, migrated.
fn notes_project(local: &Local) -> TempProject {
    let project = local_project(&local.machine, NAME, &[]);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/index.ts", &handler(false, true));
    Generated::made(
        &generate(&local.machine, &project.dir, &["--name", "add_notes"]),
        &project.dir,
    );
    project
}

/// The notes' bodies as `GET /notes` answers them to a browser that has
/// chosen outsider.
fn note_bodies(dev: &Dev) -> Vec<String> {
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let response = browser.get("/notes");
    assert_eq!(response.status, 200, "{}", response.describe());
    response.json()["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["body"].as_str().unwrap().to_owned())
        .collect()
}

/// The `Location` of a `303`, which must be a path: it begins with one `/`
/// and is not an absolute address.
fn location_path(response: &Response) -> String {
    let location = response
        .header("location")
        .unwrap_or_else(|| panic!("expected a Location\n{}", response.describe()));
    assert!(
        location.starts_with('/') && !location.starts_with("//") && !location.contains("://"),
        "expected the Location a path, not an absolute address, got {location:?}\n{}",
        response.describe()
    );
    location.to_owned()
}

#[test]
fn its_own_name_with_or_without_the_port_is_the_applications() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    for host in [dev.host(), format!("{NAME}.localhost")] {
        let mut browser = Browser::new(dev.address(), &host);
        browser.set_cookie("vm_test_user", OUTSIDER);
        let response = browser.get("/me");
        assert_eq!(
            response.status,
            200,
            "Host {host}: expected the Application's answer\n{}",
            response.describe()
        );
        assert_eq!(
            response.json(),
            json!({ "id": OUTSIDER, "groups": [] }),
            "Host {host}"
        );
        response.request_id();
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn localhost_and_the_loopback_addresses_are_sent_to_the_same_path_at_its_own_name() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let port = dev.port;
    let target = "/notes?a=1&b=two%20words";
    for host in [
        format!("localhost:{port}"),
        "localhost".to_owned(),
        format!("127.0.0.1:{port}"),
        "127.0.0.1".to_owned(),
        format!("[::1]:{port}"),
        "[::1]".to_owned(),
    ] {
        let mut browser = Browser::new(dev.address(), &host);
        browser.set_cookie("vm_test_user", OUTSIDER);
        let response = browser.get(target);
        assert_eq!(
            response.status,
            307,
            "Host {host}: expected 307\n{}",
            response.describe()
        );
        assert_eq!(
            response.header("location"),
            Some(format!("http://{NAME}.localhost:{port}{target}").as_str()),
            "Host {host}: expected the same path and query at the Application's own name\n{}",
            response.describe()
        );
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn another_host_is_answered_421_with_the_address_and_the_code_does_not_run() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let port = dev.port;
    for host in [
        "evil.example".to_owned(),
        format!("evil.example:{port}"),
        format!("other-board.localhost:{port}"),
        format!("{NAME}.localhost.evil.example:{port}"),
        format!("x.{NAME}.localhost:{port}"),
    ] {
        let mut browser = Browser::new(dev.address(), &host);
        browser.set_cookie("vm_test_user", OUTSIDER);
        let request = browser
            .build("POST", "/notes")
            .body(format!("written through {host}"));
        let response = browser.send(&request);
        assert_eq!(
            response.status,
            421,
            "Host {host}: expected 421\n{}",
            response.describe()
        );
        assert!(
            response
                .text()
                .contains(&format!("{NAME}.localhost:{port}")),
            "Host {host}: expected the page to give the address\n{}",
            response.describe()
        );
    }
    assert_eq!(
        note_bodies(&dev),
        Vec::<String>::new(),
        "expected no note written: the Application's code never ran for another Host"
    );
    let operations = local
        .operation_log(&project.dir, &["--timeout", "60"])
        .ok_json()["entries"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        operations, 1,
        "expected the one read of note_bodies in the local Operation log, and nothing of the other Hosts"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn paths_under_vm_other_than_the_page_are_404_and_never_the_applications() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    for path in [
        "/_vm/",
        "/_vm/notes",
        "/_vm/test-user/more",
        "/_vm/control",
        "/_vm/anything?x=1",
    ] {
        for method in ["GET", "POST"] {
            let request = browser
                .build(method, path)
                .body(format!("written through {path}"));
            let response = browser.send(&request);
            assert_eq!(
                response.status,
                404,
                "{method} {path}: expected 404\n{}",
                response.describe()
            );
        }
    }
    assert_eq!(
        note_bodies(&dev),
        Vec::<String>::new(),
        "expected no note written by a path under /_vm/"
    );
    dev.stop("INT").expect_code(0);
}

/// `vibe.dev.json` with four Users, each in Groups whose Grants on `devops`
/// give one access, and a Grant on a Resource that is not pulled.
fn many_users() -> Value {
    json!({
        "users": [
            { "id": "user-ana", "displayName": "Ana <script>alert(\"x\")</script> & 'co'",
              "email": "ana@example.com", "groups": ["group-readers", "group-ops"] },
            { "id": "user-bo", "displayName": "Bo Auditor", "email": "bo@example.com",
              "groups": ["group-auditors"] },
            { "id": "user-cy", "groups": [] },
            { "id": "user-dee", "displayName": "Dee Writer", "groups": ["group-writers"] }
        ],
        "grants": [
            { "group": "group-readers", "resource": "devops", "access": "read" },
            { "group": "group-auditors", "resource": "devops", "access": "structure" },
            { "group": "group-writers", "resource": "devops", "access": "write" },
            { "group": "group-ops", "resource": "payroll", "access": "write" }
        ]
    })
}

/// The part of the page from the first mention of `id` to the first
/// mention of the next User (of `ids`) after it.
fn segment_of<'p>(page: &'p str, id: &str, ids: &[&str]) -> &'p str {
    let start = page
        .find(id)
        .unwrap_or_else(|| panic!("expected the page to name {id}\n{page}"));
    let end = ids
        .iter()
        .filter_map(|other| page.find(other))
        .filter(|at| *at > start)
        .min()
        .unwrap_or(page.len());
    page.get(start..end).unwrap_or(page)
}

#[test]
fn the_page_lists_every_test_user_with_their_groups_and_access_on_each_pulled_resource_escaped() {
    let board = Board::start("read", "read");
    write_dev_json(board.dir(), &many_users());
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    let page = browser.get("/_vm/test-user");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert_eq!(
        page.header("content-type")
            .map(str::to_lowercase)
            .as_deref(),
        Some("text/html; charset=utf-8"),
        "{}",
        page.describe()
    );
    assert!(
        page.header("cache-control")
            .is_some_and(|c| c.to_lowercase().contains("no-store")),
        "expected cache-control: no-store\n{}",
        page.describe()
    );
    let text = page.text();
    assert!(
        !text.contains("<script>alert(") && !text.contains("alert(\"x\")"),
        "expected the displayName escaped as HTML, its tag and its quotes\n{text}"
    );
    let plain = html_unescaped(&text);
    assert!(
        plain.contains("Ana <script>alert(\"x\")</script> & 'co'"),
        "expected the displayName on the page, escaped\n{text}"
    );
    let ids = ["user-ana", "user-bo", "user-cy", "user-dee"];
    for (id, email, groups, access) in [
        (
            "user-ana",
            Some("ana@example.com"),
            &["group-readers", "group-ops"][..],
            "read",
        ),
        (
            "user-bo",
            Some("bo@example.com"),
            &["group-auditors"][..],
            "structure",
        ),
        ("user-cy", None, &[][..], "none"),
        ("user-dee", None, &["group-writers"][..], "write"),
    ] {
        let part = html_unescaped(segment_of(&text, id, &ids));
        if let Some(email) = email {
            assert!(part.contains(email), "expected {id}'s email\n{part}");
        }
        for group in groups {
            assert!(
                part.contains(group),
                "expected {id}'s Group {group}\n{part}"
            );
        }
        assert!(
            part.contains(DEVOPS) || text.contains(DEVOPS),
            "expected the pulled Resource named\n{text}"
        );
        assert!(
            part.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == access),
            "expected {id}'s access on devops, {access}, beside them\n{part}"
        );
    }
    assert!(
        !html_unescaped(&text).contains("payroll"),
        "expected only pulled Resources listed\n{text}"
    );
    // Which one is chosen now shows.
    browser.choose("user-bo");
    let chosen = browser.get("/_vm/test-user");
    assert_eq!(chosen.status, 200);
    assert_ne!(
        chosen.text(),
        text,
        "expected the page to say which User is chosen now"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn choosing_sets_the_cookie_and_sends_the_browser_to_a_local_next_only() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    let origin = browser.origin();
    let response = browser.post_form("/_vm/test-user", &[("user", OUTSIDER)], Some(&origin));
    assert_eq!(response.status, 303, "{}", response.describe());
    assert_eq!(location_path(&response), "/", "expected / without next");
    let cookies = response.headers_named("set-cookie");
    let cookie = cookies
        .iter()
        .find(|c| c.starts_with("vm_test_user="))
        .unwrap_or_else(|| {
            panic!(
                "expected Set-Cookie: vm_test_user=...\n{}",
                response.describe()
            )
        });
    let attributes: Vec<String> = cookie.split(';').map(|a| a.trim().to_lowercase()).collect();
    assert_eq!(attributes[0], format!("vm_test_user={OUTSIDER}"));
    for wanted in ["path=/", "httponly", "samesite=lax"] {
        assert!(
            attributes.iter().any(|a| a == wanted),
            "expected the cookie's attribute {wanted}, got {cookie:?}"
        );
    }
    for (next, expected) in [
        ("/notes?a=1&b=2", "/notes?a=1&b=2"),
        ("//evil.example/", "/"),
        ("/\\evil.example/", "/"),
        ("https://evil.example/", "/"),
        ("http://evil.example/notes", "/"),
        ("notes", "/"),
    ] {
        let response = browser.post_form(
            "/_vm/test-user",
            &[("user", OUTSIDER), ("next", next)],
            Some(&origin),
        );
        assert_eq!(
            response.status,
            303,
            "next {next:?}\n{}",
            response.describe()
        );
        assert_eq!(
            location_path(&response),
            expected,
            "next {next:?}: expected the browser sent to {expected:?}\n{}",
            response.describe()
        );
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_user_not_in_the_file_is_400_with_the_page_and_another_sites_origin_is_403() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    let origin = browser.origin();
    let unknown = browser.post_form("/_vm/test-user", &[("user", "nobody-here")], Some(&origin));
    assert_eq!(unknown.status, 400, "{}", unknown.describe());
    assert!(
        unknown
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/html")),
        "expected the page again\n{}",
        unknown.describe()
    );
    assert!(
        unknown.text().contains(OUTSIDER),
        "expected the page again, listing the test Users\n{}",
        unknown.describe()
    );
    assert_eq!(
        browser.cookie("vm_test_user"),
        None,
        "expected no User chosen"
    );
    for other in [
        "http://evil.example".to_owned(),
        format!("http://other-board.localhost:{}", dev.port),
        "null".to_owned(),
    ] {
        let refused = browser.post_form("/_vm/test-user", &[("user", OUTSIDER)], Some(&other));
        assert_eq!(
            refused.status,
            403,
            "Origin {other}: expected 403\n{}",
            refused.describe()
        );
        assert_eq!(browser.cookie("vm_test_user"), None, "Origin {other}");
    }
    let without = browser.post_form("/_vm/test-user", &[("user", OUTSIDER)], None);
    assert_eq!(
        without.status,
        303,
        "expected a POST without Origin taken\n{}",
        without.describe()
    );
    assert_eq!(browser.cookie("vm_test_user").as_deref(), Some(OUTSIDER));
    let same = browser.post_form("/_vm/test-user", &[("user", OUTSIDER)], Some(&origin));
    assert_eq!(
        same.status,
        303,
        "expected its own Origin accepted\n{}",
        same.describe()
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn with_no_user_chosen_a_page_is_sent_to_the_choice_and_anything_else_is_401() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    let target = "/notes?a=1&b=two%20words";
    for method in ["GET", "HEAD"] {
        let request = browser.build(method, target);
        let response = browser.send(&request);
        assert_eq!(response.status, 303, "{method}\n{}", response.describe());
        let location = location_path(&response);
        assert_eq!(
            location, "/_vm/test-user?next=/notes%3Fa%3D1%26b%3Dtwo%2520words",
            "{method}: expected next with every byte but letters, digits, -._~ and / percent-encoded"
        );
        let next = location
            .strip_prefix("/_vm/test-user?next=")
            .unwrap_or_else(|| {
                panic!("{method}: expected /_vm/test-user?next=..., got {location}")
            });
        assert!(
            !next.contains('&') && !next.contains('?'),
            "{method}: expected next percent-encoded, got {next:?}"
        );
        assert_eq!(
            percent_decode(next, true).as_deref(),
            Some(target),
            "{method}: expected next to be the path and query"
        );
    }
    // Not a page: 401, text/plain, saying where to choose.
    let mut api = dev.browser();
    api.accept = "application/json".to_owned();
    for (method, body) in [("GET", ""), ("POST", "written with no User")] {
        let request = api.build(method, "/notes").body(body);
        let response = api.send(&request);
        assert_eq!(response.status, 401, "{method}\n{}", response.describe());
        assert!(
            response
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/plain")),
            "{method}\n{}",
            response.describe()
        );
        let text = response.text();
        assert!(
            text.contains("/_vm/test-user") && text.contains("vm_test_user"),
            "{method}: expected the answer to say where to choose a test User, or to send the cookie\n{text}"
        );
    }
    // A POST from a page is not a page either.
    let request = browser.build("POST", "/notes").body("written with no User");
    assert_eq!(browser.send(&request).status, 401);
    assert_eq!(
        note_bodies(&dev),
        Vec::<String>::new(),
        "expected the Application never given a request with no User"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_cookie_naming_a_user_removed_from_the_file_is_no_user() {
    let local = Local::new();
    let project = notes_project(&local);
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "carol", "groups": [] }));
    write_dev_json(&project.dir, &file);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose("carol");
    assert_eq!(browser.get("/me").json()["id"], json!("carol"));
    file["users"]
        .as_array_mut()
        .unwrap()
        .retain(|u| u["id"] != "carol");
    write_dev_json(&project.dir, &file);
    let response = browser.get("/me");
    assert_eq!(
        response.status,
        303,
        "expected a User no longer in the file to be no User\n{}",
        response.describe()
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_application_is_given_the_users_id_and_groups_as_the_file_has_them_now() {
    let local = Local::new();
    let project = notes_project(&local);
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "dana", "groups": ["ops"] }));
    write_dev_json(&project.dir, &file);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose("dana");
    assert_eq!(
        browser.get("/me").json(),
        json!({ "id": "dana", "groups": ["ops"] })
    );
    for user in file["users"].as_array_mut().unwrap() {
        if user["id"] == "dana" {
            user["groups"] = json!(["ops", "night-shift"]);
        }
    }
    write_dev_json(&project.dir, &file);
    assert_eq!(
        browser.get("/me").json(),
        json!({ "id": "dana", "groups": ["ops", "night-shift"] }),
        "expected the Groups as the file has them at the moment of the request"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn an_id_with_markup_quotes_a_percent_and_a_semicolon_is_escaped_on_the_page_and_percent_encoded_in_the_cookie()
 {
    let local = Local::new();
    let project = notes_project(&local);
    let id = "eve <i>\"x\"</i> 100%; é";
    // Every byte but letters, digits, `-`, `.`, `_` and `~` encoded.
    let encoded = "eve%20%3Ci%3E%22x%22%3C%2Fi%3E%20100%25%3B%20%C3%A9";
    assert_eq!(percent_encode(id), encoded, "the harness's own check");
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": id, "groups": [] }));
    write_dev_json(&project.dir, &file);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    let page = browser.get("/_vm/test-user").text();
    assert!(
        !page.contains("<i>\"x\"</i>"),
        "expected the id escaped as HTML\n{page}"
    );
    assert!(
        html_unescaped(&page).contains(id),
        "expected the id on the page\n{page}"
    );
    let chosen = browser.choose(id);
    let cookie = chosen
        .headers_named("set-cookie")
        .into_iter()
        .find(|c| c.starts_with("vm_test_user="))
        .unwrap_or_else(|| panic!("{}", chosen.describe()));
    let value = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("vm_test_user=");
    assert_eq!(
        value, encoded,
        "expected the id percent-encoded, every byte but letters, digits, -._~"
    );
    assert_eq!(browser.get("/me").json(), json!({ "id": id, "groups": [] }));
    // The same cookie written by hand, encoded by the harness.
    let mut by_hand = dev.browser();
    by_hand.set_cookie("vm_test_user", &percent_encode(id));
    assert_eq!(by_hand.get("/me").json()["id"], json!(id));
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_cookie_vm_test_user_is_taken_out_of_the_cookie_header_and_the_others_stay() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.set_cookie("first", "1");
    browser.choose(OUTSIDER);
    browser.set_cookie("second", "2");
    let echoed = browser.get("/echo").json();
    let cookie = echoed["headers"]["cookie"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the other cookies given to the code, got {echoed}"));
    assert!(
        cookie.contains("first=1") && cookie.contains("second=2"),
        "expected the other cookies kept, got {cookie:?}"
    );
    assert!(
        !cookie.contains("vm_test_user"),
        "expected vm_test_user taken out, got {cookie:?}"
    );
    let mut alone = dev.browser();
    alone.choose(OUTSIDER);
    let echoed = alone.get("/echo").json();
    assert!(
        echoed["headers"].get("cookie").is_none(),
        "expected a Cookie header left empty to be left out, got {echoed}"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn switching_user_changes_what_the_application_shows() {
    let board = Board::start("read", "read");
    fs::create_dir_all(board.dir().join("test-rows")).unwrap();
    board.project.write(
        "test-rows/devops.sql",
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n",
    );
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    let first = browser.get("/incidents");
    assert_eq!(
        first.status,
        303,
        "expected the choice first\n{}",
        first.describe()
    );
    let next = location_path(&first);
    let page = browser.get(&next);
    assert_eq!(page.status, 200, "{}", page.describe());
    let origin = browser.origin();
    let chosen = browser.post_form(
        "/_vm/test-user",
        &[("user", BUILDER), ("next", "/incidents")],
        Some(&origin),
    );
    assert_eq!(location_path(&chosen), "/incidents");
    let as_builder = browser.get("/incidents");
    assert_eq!(as_builder.status, 200, "{}", as_builder.describe());
    assert_eq!(
        as_builder.json(),
        json!({ "user": BUILDER, "incidents": [{ "id": 1, "title": "Printer on fire" }] })
    );
    browser.choose(OUTSIDER);
    let as_outsider = browser.get("/incidents");
    assert_eq!(
        as_outsider.status,
        403,
        "expected outsider refused\n{}",
        as_outsider.describe()
    );
    assert_eq!(as_outsider.json()["refusal"]["reason"], json!("no-grant"));
    browser.choose(BUILDER);
    assert_eq!(browser.get("/incidents").status, 200);
    dev.stop("INT").expect_code(0);
}

#[test]
fn on_an_installation_vm_test_user_is_a_path_like_any_other_given_to_the_code() {
    let board = Board::start("read", "read");
    init_repository(board.dir());
    commit_all(board.dir(), "A board");
    let deployed = Deployed::of(&deploy(&board.local.machine, board.dir(), &[]));
    let vm = &board.scenario.vm;
    // Shared with everyone by its Contributor: this test is about the
    // paths, not about who may open it ("Contributors and sharing").
    blackbox::sharing::share_with_everyone_as_deployer(vm, &deployed);
    for path in ["/_vm/test-user", "/_vm/control/x", "/_vm/anything"] {
        let response = vm.get_as(&User::new(OUTSIDER), APP, path);
        assert_eq!(
            response.status,
            200,
            "{path}: expected the deployed Application's own answer\n{}",
            response.describe()
        );
        assert_eq!(response.json(), json!({ "user": OUTSIDER }), "{path}");
    }
    let posted = vm
        .request(APP, "/_vm/test-user")
        .method("POST")
        .as_user(&User::new(OUTSIDER))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("user=builder")
        .send();
    assert_eq!(posted.status, 200, "{}", posted.describe());
    assert_eq!(
        posted.json(),
        json!({ "user": OUTSIDER }),
        "expected no choosing of a test User on an installation"
    );
    assert!(posted.header("set-cookie").is_none());
}

#[test]
fn only_requests_given_to_the_applications_code_have_a_request_line() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let port = dev.port;
    // No User chosen: a page sent to the choice, anything else 401.
    let mut browser = dev.browser();
    assert_eq!(browser.get("/notes").status, 303);
    let mut api = dev.browser();
    api.accept = "application/json".to_owned();
    assert_eq!(api.get("/notes").status, 401);
    // The page, and choosing.
    assert_eq!(browser.get("/_vm/test-user").status, 200);
    browser.choose(OUTSIDER);
    // A 307, a 421, a 404 under /_vm/.
    let mut loopback = Browser::new(dev.address(), &format!("127.0.0.1:{port}"));
    loopback.set_cookie("vm_test_user", OUTSIDER);
    assert_eq!(loopback.get("/notes").status, 307);
    let mut other = Browser::new(dev.address(), &format!("evil.example:{port}"));
    other.set_cookie("vm_test_user", OUTSIDER);
    assert_eq!(other.get("/notes").status, 421);
    assert_eq!(browser.get("/_vm/nothing-here").status, 404);
    // One the code is given.
    let served = browser.get("/notes");
    assert_eq!(served.status, 200, "{}", served.describe());
    let id = served.request_id();
    dev.wait_for_line(
        "the `request` line of /notes",
        Duration::from_secs(10),
        |l| l["event"] == "request" && l["request_id"] == id.as_str(),
    );
    let lines = dev.events("request");
    assert_eq!(
        lines.len(),
        1,
        "expected a `request` line only for the request the code was given, got {lines:#?}"
    );
    dev.stop("INT").expect_code(0);
}

/// Fails unless `response` carries the headers of every answer of the
/// local run's own.
fn assert_locked_down(response: &Response, what: &str) {
    assert!(
        response
            .header("cache-control")
            .is_some_and(|c| c.to_lowercase().contains("no-store")),
        "{what}: expected cache-control: no-store\n{}",
        response.describe()
    );
    assert_eq!(
        response
            .header("x-content-type-options")
            .map(str::to_lowercase)
            .as_deref(),
        Some("nosniff"),
        "{what}: expected x-content-type-options: nosniff\n{}",
        response.describe()
    );
    let policy = response
        .header("content-security-policy")
        .unwrap_or_else(|| {
            panic!(
                "{what}: expected a content-security-policy\n{}",
                response.describe()
            )
        });
    let directives: Vec<String> = policy
        .split(';')
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty())
        .collect();
    for wanted in [
        "default-src 'none'",
        "frame-ancestors 'none'",
        "base-uri 'none'",
        "form-action 'self'",
    ] {
        assert!(
            directives.iter().any(|d| d == wanted),
            "{what}: expected the directive {wanted:?} in {policy:?}"
        );
    }
}

#[test]
fn the_local_runs_own_answers_carry_no_store_nosniff_and_a_content_security_policy() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let port = dev.port;
    let mut browser = dev.browser();
    let to_choice = browser.get("/notes");
    assert_eq!(to_choice.status, 303);
    assert_locked_down(&to_choice, "the 303 to the choice");
    let mut api = dev.browser();
    api.accept = "application/json".to_owned();
    let unauthorized = api.get("/notes");
    assert_eq!(unauthorized.status, 401);
    assert_locked_down(&unauthorized, "the 401");
    let page = browser.get("/_vm/test-user");
    assert_eq!(page.status, 200);
    assert_locked_down(&page, "the page");
    let origin = browser.origin();
    let unknown = browser.post_form("/_vm/test-user", &[("user", "nobody-here")], Some(&origin));
    assert_eq!(unknown.status, 400);
    assert_locked_down(&unknown, "the 400");
    let refused = browser.post_form(
        "/_vm/test-user",
        &[("user", OUTSIDER)],
        Some("http://evil.example"),
    );
    assert_eq!(refused.status, 403);
    assert_locked_down(&refused, "the 403");
    let chosen = browser.choose(OUTSIDER);
    assert_locked_down(&chosen, "the 303 of choosing");
    let mut loopback = Browser::new(dev.address(), &format!("127.0.0.1:{port}"));
    let moved = loopback.get("/notes");
    assert_eq!(moved.status, 307);
    assert_locked_down(&moved, "the 307");
    let mut other = Browser::new(dev.address(), &format!("evil.example:{port}"));
    let misdirected = other.get("/notes");
    assert_eq!(misdirected.status, 421);
    assert_locked_down(&misdirected, "the 421");
    let missing = browser.get("/_vm/nothing-here");
    assert_eq!(missing.status, 404);
    assert_locked_down(&missing, "the 404 under /_vm/");
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_cookie_with_bytes_that_are_not_ascii_does_not_hide_the_chosen_user() {
    let local = Local::new();
    let project = notes_project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let one_line = dev
        .request("/echo")
        .header("Cookie", b"vm_test_user=outsider; name=Jos\xc3\xa9");
    let two_lines = dev
        .request("/echo")
        .header("Cookie", "vm_test_user=outsider")
        .header("Cookie", b"name=Jos\xc3\xa9");
    for (what, request) in [
        ("one Cookie line", one_line),
        ("two Cookie lines", two_lines),
    ] {
        let response = request.send();
        assert_eq!(
            response.status,
            200,
            "{what}: expected the request given to the Application as the chosen User\n{}",
            response.describe()
        );
        let echoed = response.json();
        let cookie = echoed["headers"]["cookie"].as_str().unwrap_or_else(|| {
            panic!("{what}: expected the other cookie given to the code, got {echoed}")
        });
        assert!(
            cookie.contains("name=Jos"),
            "{what}: expected the cookie `name` kept, got {cookie:?}"
        );
        assert!(
            !cookie.contains("vm_test_user"),
            "{what}: expected vm_test_user taken out, got {cookie:?}"
        );
    }
    dev.stop("INT").expect_code(0);
}
