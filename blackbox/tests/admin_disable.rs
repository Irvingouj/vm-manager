//! Disabling and enabling, on the platform's API, and what a disabled
//! Application answers (CONTRACT.md, "What Platform administrators see and
//! stop": "Disabling and enabling, on the platform's API", "What a disabled
//! Application answers", "What still works on a disabled Application"):
//! the answers with `changed`, nothing changed by doing it twice, `disabled`
//! on every object of the API, kept across a restart, the program's log
//! records, the refusals and their order; a disabled Application answers
//! `503` (a page, or one plain sentence), after the `403` of someone it is
//! not shared with and before the `404` of no Version, with no
//! `retry-after`, and none of its code runs; deploying, going back, sharing
//! and reading go on; only a Platform administrator enables, and enabling
//! serves the Version that serves now.
//!
//! Acceptance criterion: "A disabled Application stops serving at once ...;
//! its Users get a message that it is disabled; enabling restores it."

use std::thread;

use blackbox::admin::{
    self, administrator, assert_disabled_member, assert_disabled_page, assert_disabled_plain,
    assert_disabled_record, disable, disable_ok, enable, enable_ok, logged, other_administrator,
};
use blackbox::api::{assert_json_content_type, assert_served_by, marked_bundle, version_body};
use blackbox::browser::NAVIGATION_ACCEPT;
use blackbox::logs::{ENTRY_WAIT, from_to, go_back, logs};
use blackbox::sessions::hrefs;
use blackbox::sharing::{
    TRACED, add, as_read, assert_not_shared_plain, contributors_ok, create_unshared, group,
    share_ok, user,
};
use blackbox::{Response, User, Vm, assert_api_error};
use serde_json::{Value, json};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol")
}

/// A program with `board` made by alice (`fixtures/honest.js`), shared
/// with bob.
fn with_board() -> Vm {
    let vm = admin::start();
    create_unshared(&vm, &alice(), APP, TRACED);
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    vm
}

/// `GET /hello` of `board` as this User, not a page.
fn open(vm: &Vm, user: &User) -> Response {
    vm.request(APP, "/hello")
        .as_user(user)
        .header("accept", "application/json")
        .send()
}

fn opens(vm: &Vm, user: &User) -> bool {
    let r = open(vm, user);
    r.status == 200 && r.text().contains(&format!("Hello, {}!", user.id))
}

/// The Application as this User reads it.
fn read(vm: &Vm, user: &User) -> Value {
    vm.api_as(user).application(APP)
}

#[test]
fn disable_and_enable_answer_the_application_and_whether_they_changed_it() {
    let vm = with_board();
    let first = disable(&vm, &administrator(), APP);
    first.expect_status(200);
    assert_json_content_type(&first);
    let answer = first.json();
    assert_eq!(answer["changed"], true, "{answer}");
    let since = assert_disabled_member(&answer["disabled"], &administrator().id, "the disable");
    let mut as_object = answer;
    as_object.as_object_mut().unwrap().remove("changed");
    assert_eq!(
        as_object,
        read(&vm, &administrator()),
        "expected the Application's object as GET answers it to that administrator, and `changed`"
    );

    // Again, by another administrator: nothing changes, not refused.
    let again = disable(&vm, &other_administrator(), APP);
    again.expect_status(200);
    let again = again.json();
    assert_eq!(again["changed"], false, "{again}");
    assert_eq!(
        again["disabled"],
        json!({ "since": since, "by": administrator().id }),
        "disabling a disabled Application keeps `since` and `by`"
    );

    let enabled = enable(&vm, &administrator(), APP);
    enabled.expect_status(200);
    let enabled = enabled.json();
    assert_eq!(enabled["changed"], true, "{enabled}");
    assert!(enabled["disabled"].is_null(), "{enabled}");
    let mut as_object = enabled;
    as_object.as_object_mut().unwrap().remove("changed");
    assert_eq!(as_object, read(&vm, &administrator()));
    let again = enable(&vm, &administrator(), APP).json();
    assert_eq!(
        again["changed"], false,
        "enabling one that is not disabled: {again}"
    );
    assert!(again["disabled"].is_null());

    // The program's log: one record for each change, none when nothing
    // changed.
    let disabled = logged(&vm, "application-disabled");
    let enabled = logged(&vm, "application-enabled");
    assert_eq!(disabled.len(), 1, "{disabled:?}");
    assert_eq!(enabled.len(), 1, "{enabled:?}");
    assert_eq!(disabled[0]["application"], APP);
    assert_eq!(disabled[0]["by"], administrator().id);
    assert_eq!(enabled[0]["application"], APP);
    assert_eq!(enabled[0]["by"], administrator().id);
}

#[test]
fn the_body_is_empty_or_any_json_object() {
    let vm = with_board();
    let api = vm.api_as(&administrator());
    let target = format!("/v1/applications/{APP}/disable");
    let empty = api.request(&target).method("POST").send();
    empty.expect_status(200);
    for body in [r#"{}"#, r#"{"reason":"ignored","x":[1,2]}"#] {
        api.post_raw(&target, body).expect_status(200);
    }
    for body in ["[]", "\"x\"", "1", "null", "not json", "{"] {
        assert_api_error(&api.post_raw(&target, body), 400, "malformed");
        assert_api_error(
            &api.post_raw(&format!("/v1/applications/{APP}/enable"), body),
            400,
            "malformed",
        );
    }
    // A refused enable changed nothing.
    assert!(!read(&vm, &administrator())["disabled"].is_null());
}

#[test]
fn disabled_is_on_every_object_of_the_api_the_same_to_everyone() {
    let vm = with_board();
    let answer = disable_ok(&vm, APP);
    let disabled = answer["disabled"].clone();
    for user in [alice(), bob(), administrator()] {
        assert_eq!(
            read(&vm, &user)["disabled"],
            disabled,
            "GET /v1/applications/{APP} as {:?}",
            user.id
        );
    }
    let created = vm.api_as(&alice()).create_application("fresh");
    created.expect_status(201);
    assert!(
        created.json().get("disabled").is_some_and(Value::is_null),
        "POST /v1/applications answers `disabled` null\n{}",
        created.describe()
    );
    let api = vm.api_as(&alice());
    let shared = share_ok(&api, &vm, APP, &add(&[user("carol")]));
    assert_eq!(
        shared["disabled"], disabled,
        "the answer to a change of sharing"
    );
    let contributed = contributors_ok(&api, &vm, APP, &add(&[group("board-team")]));
    assert_eq!(
        contributed["disabled"], disabled,
        "the answer to a change of the Contributors"
    );
    assert_eq!(as_read(&contributed), read(&vm, &alice()));
    // The User's own list: a boolean.
    let list = api.list_applications().json();
    let flags: Vec<(String, Value)> = list["applications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_owned(),
                a["disabled"].clone(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        vec![
            (APP.to_owned(), json!(true)),
            ("fresh".to_owned(), json!(false))
        ],
        "GET /v1/applications: `disabled` a boolean on each"
    );
}

#[test]
fn a_disabled_application_answers_one_plain_sentence_and_runs_no_code() {
    let vm = with_board();
    assert!(opens(&vm, &bob()));
    disable_ok(&vm, APP);
    let mut answered = Vec::new();
    for (method, path) in [
        ("GET", "/hello"),
        ("POST", "/hello"),
        ("PUT", "/anything/else?x=1"),
        ("DELETE", "/"),
        ("GET", "/_vm/session"),
    ] {
        for user in [bob(), alice()] {
            let response = vm
                .request(APP, path)
                .as_user(&user)
                .method(method)
                .header("accept", "application/json")
                .body(if method == "GET" || method == "DELETE" {
                    ""
                } else {
                    "{}"
                })
                .send();
            let context = format!("{method} {path} as {}", user.id);
            assert_disabled_plain(&response, APP, &context);
            assert_disabled_record(&vm, &response, APP, &user.id, &context);
            answered.push(response);
        }
    }
    // No entry in the logs and nothing counted: its code was given none.
    std::thread::sleep(ENTRY_WAIT / 4);
    let read_logs = logs(&vm.api_as(&alice()), APP, "?limit=1000");
    read_logs.expect_status(200);
    let entries = read_logs.json()["entries"].as_array().unwrap().clone();
    for response in &answered {
        let id = response.request_id();
        assert!(
            !entries.iter().any(|e| e["request"] == id.as_str()),
            "a request answered disabled has no entry in the logs, but {id} has one"
        );
    }
}

#[test]
fn a_disabled_application_answers_a_page_to_a_person_at_a_browser() {
    let vm = admin::start();
    let odd = User::new("<b>bob</b> & \"co\"");
    create_unshared(&vm, &alice(), APP, TRACED);
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[user("bob"), user(&odd.id)]),
    );
    let answer = disable_ok(&vm, APP);
    let since = answer["disabled"]["since"].as_str().unwrap().to_owned();
    for accept in [
        "text/html",
        NAVIGATION_ACCEPT,
        "application/json, text/html;q=0.1",
    ] {
        let response = vm
            .request(APP, "/some/page?x=1")
            .as_user(&bob())
            .header("accept", accept)
            .send();
        let context = format!("Accept {accept:?}");
        assert_disabled_page(
            &response,
            APP,
            &["bob"],
            &[&administrator().id, &since, "alice"],
            &context,
        );
        let html = response.text();
        assert!(
            hrefs(&html).is_empty() && !html.to_ascii_lowercase().contains("<a"),
            "{context}: expected no link at all with the stand-in, which has no sign-out\n{html}"
        );
        assert_disabled_record(&vm, &response, APP, "bob", &context);
    }
    // A `HEAD` that asks for a page: the page's answer without a body.
    let head = vm
        .request(APP, "/")
        .as_user(&bob())
        .method("HEAD")
        .header("accept", "text/html")
        .send();
    assert_eq!(head.status, 503, "{}", head.describe());
    assert!(head.body().is_empty());
    assert!(head.header("retry-after").is_none(), "{}", head.describe());
    assert!(
        head.header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/html")),
        "{}",
        head.describe()
    );
    // A `POST` that asks for a page is not a page.
    let post = vm
        .request(APP, "/")
        .as_user(&bob())
        .method("POST")
        .header("accept", "text/html")
        .body("x")
        .send();
    assert_disabled_plain(&post, APP, "POST with Accept text/html");
    // Everything shown of the person is escaped.
    let page = vm
        .request(APP, "/")
        .as_user(&odd)
        .header("accept", "text/html")
        .send();
    assert_eq!(page.status, 503, "{}", page.describe());
    let html = page.text();
    assert!(
        !html.contains("<b>bob</b>") && html.contains("&lt;b&gt;bob&lt;/b&gt;"),
        "expected the User id escaped as HTML\n{html}"
    );
}

#[test]
fn only_who_can_open_it_learns_that_it_is_disabled_and_no_version_comes_after() {
    let vm = admin::start();
    create_unshared(&vm, &alice(), APP, TRACED);
    vm.api_as(&alice())
        .create_application("empty")
        .expect_status(201);
    disable_ok(&vm, APP);
    disable_ok(&vm, "empty");
    // Someone it is not shared with: the `403`, as before.
    let response = open(&vm, &carol());
    assert_not_shared_plain(&vm, &response, APP, "carol", "carol, not shared with");
    // Not signed in: the `401`.
    let anonymous = vm.request(APP, "/hello").send();
    assert_eq!(anonymous.status, 401, "{}", anonymous.describe());
    // Its Contributor: disabled.
    assert_disabled_plain(&open(&vm, &alice()), APP, "alice, its Contributor");
    // No Version: disabled comes before the `404`.
    let empty = vm
        .request("empty", "/")
        .as_user(&alice())
        .header("accept", "application/json")
        .send();
    assert_disabled_plain(&empty, "empty", "an Application with no Version");
    // A name no Application has: `404` as ever.
    let nothing = vm.get_as(&alice(), "nothing-here", "/");
    assert_eq!(nothing.status, 404, "{}", nothing.describe());
}

#[test]
fn enabling_serves_by_the_version_that_serves_now_and_deploying_going_back_and_sharing_go_on() {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_with_bundle(APP, &marked_bundle("v1"));
    assert_served_by(&vm, &vm.get_as(&bob(), APP, "/"), APP, "v1", 1);
    disable_ok(&vm, APP);

    // A Version sent while disabled becomes the serving Version, and is
    // not served.
    let sent = api.send_version(APP, &version_body(&marked_bundle("v2")));
    sent.expect_status(201);
    assert_eq!(read(&vm, &alice())["serving_version"], 2);
    assert_disabled_plain(
        &vm.request(APP, "/")
            .as_user(&bob())
            .header("accept", "application/json")
            .send(),
        APP,
        "after a Version is sent while disabled",
    );
    // Sharing, the Contributors and reading go on.
    share_ok(&api, &vm, APP, &add(&[user("carol")]));
    contributors_ok(&api, &vm, APP, &add(&[user("dave")]));
    api.list_versions(APP).expect_status(200);
    logs(&api, APP, "").expect_status(200);
    api.get_application(APP).expect_status(200);
    // Only a Platform administrator enables: not a Contributor, not a
    // deploy, not a go-back.
    assert_api_error(&enable(&vm, &alice(), APP), 403, "not-an-administrator");
    assert!(!read(&vm, &alice())["disabled"].is_null());

    let enabled = enable_ok(&vm, APP);
    assert_eq!(enabled["serving_version"], 2, "{enabled}");
    assert_served_by(&vm, &vm.get_as(&bob(), APP, "/"), APP, "v2", 2);

    // Gone back while disabled: Version 1 serves once enabled.
    disable_ok(&vm, APP);
    go_back(&api, APP, &from_to(2, None)).expect_status(200);
    assert_disabled_plain(
        &vm.request(APP, "/")
            .as_user(&bob())
            .header("accept", "application/json")
            .send(),
        APP,
        "after a go-back while disabled",
    );
    assert!(
        !read(&vm, &alice())["disabled"].is_null(),
        "a go-back does not enable"
    );
    enable_ok(&vm, APP);
    assert_served_by(&vm, &vm.get_as(&bob(), APP, "/"), APP, "v1", 1);
    assert_served_by(&vm, &vm.get_as(&carol(), APP, "/"), APP, "v1", 1);
}

#[test]
fn a_disabled_application_stays_disabled_across_a_restart() {
    let vm = with_board();
    let answer = disable_ok(&vm, APP);
    let disabled = answer["disabled"].clone();
    let vm = vm.restart();
    assert_eq!(
        read(&vm, &alice())["disabled"],
        disabled,
        "kept with its since and by"
    );
    assert_disabled_plain(&open(&vm, &bob()), APP, "after a restart");
    let again = disable(&vm, &other_administrator(), APP).json();
    assert_eq!(again["changed"], false);
    assert_eq!(again["disabled"], disabled);
    enable_ok(&vm, APP);
    assert!(opens(&vm, &bob()));
    let vm = vm.restart();
    assert!(opens(&vm, &bob()), "enabled is kept too");
    assert!(read(&vm, &alice())["disabled"].is_null());
}

#[test]
fn refusals_come_in_the_contracts_order_and_change_nothing() {
    let vm = with_board();
    let admin = vm.api_as(&administrator());
    for name in ["nothing-here", "Not_A_Name", "-x"] {
        for action in ["disable", "enable"] {
            let target = format!("/v1/applications/{name}/{action}");
            assert_api_error(&admin.post(&target, &json!({})), 404, "not-found");
            // not-found before malformed.
            assert_api_error(&admin.post_raw(&target, "[1]"), 404, "not-found");
        }
    }
    for action in ["disable", "enable"] {
        let target = format!("/v1/applications/{APP}/{action}");
        for method in ["GET", "PUT", "DELETE", "PATCH"] {
            let answer = admin
                .request(&target)
                .method(method)
                .json(&json!({}))
                .send();
            assert_api_error(&answer, 405, "method-not-allowed");
        }
        // too-large comes first.
        let large = admin
            .request(&target)
            .method("POST")
            .header("content-type", "application/json")
            .body(vec![b' '; 16 * 1024 * 1024 + 1])
            .send();
        assert_api_error(&large, 413, "too-large");
    }
    assert!(read(&vm, &alice())["disabled"].is_null(), "nothing changed");
    assert!(logged(&vm, "application-disabled").is_empty());
    assert!(logged(&vm, "application-enabled").is_empty());
    assert!(opens(&vm, &bob()));
}

#[test]
fn two_disables_at_once_change_it_once_and_by_names_that_caller() {
    for round in 0..3 {
        let vm = with_board();
        let (a, b) = thread::scope(|scope| {
            let a = scope.spawn(|| disable(&vm, &administrator(), APP));
            let b = scope.spawn(|| disable(&vm, &other_administrator(), APP));
            (a.join().unwrap(), b.join().unwrap())
        });
        a.expect_status(200);
        b.expect_status(200);
        let (a, b) = (a.json(), b.json());
        let changed: Vec<bool> = [&a, &b]
            .iter()
            .map(|x| x["changed"].as_bool().unwrap())
            .collect();
        assert_eq!(
            changed.iter().filter(|c| **c).count(),
            1,
            "round {round}: exactly one of two disables at once changed it: {a} {b}"
        );
        let winner = if changed[0] {
            administrator()
        } else {
            other_administrator()
        };
        assert_eq!(
            a["disabled"]["by"],
            winner.id.as_str(),
            "round {round}: {a}"
        );
        assert_eq!(
            b["disabled"]["by"],
            winner.id.as_str(),
            "round {round}: {b}"
        );
        assert_eq!(a["disabled"], b["disabled"]);
        assert_eq!(logged(&vm, "application-disabled").len(), 1);
    }
}
