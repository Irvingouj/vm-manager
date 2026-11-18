//! Sharing from the Dashboard (CONTRACT.md, "The Dashboard: Sharing from
//! it"), with the stand-in sign-in: the share form and each entry's
//! unshare form, `303` and the notice, the person shared with finding the
//! Application on their Dashboard and opening it, and neither after an
//! unshare; `nothing`; every refusal with its status, `data-vm-error` and
//! the way back, nothing changed; the `sharing-changed` record; a change
//! from another origin refused.
//!
//! Acceptance criterion: "A Contributor shares an Application with a
//! person, a Group or everyone from the Dashboard, and the change shows for
//! those people".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::dashboard::{
    Dashboard, MAX_FORM_BODY, assert_changed, assert_refusal_words, assert_refused, create, deploy,
    form_body, read_back, start,
};
use blackbox::sharing::{
    MAX_SHARED_WITH, add, assert_cannot_open, assert_opens, group, keys, share_ok, user,
};
use blackbox::{Response, User, Vm};
use serde_json::Value;

const APP: &str = "board";

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["finance"])
}

fn dave() -> User {
    User::new("dave")
}

/// `board`, made by alice with `fixtures/honest.js` as its Version, shared
/// with nobody.
fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, None);
    deploy(&vm, &alice(), APP, &[]);
    vm
}

fn shared_with(vm: &Vm) -> Vec<String> {
    keys(&read_back(vm, &alice(), APP)["shared_with"])
}

fn contributors(vm: &Vm) -> Vec<String> {
    keys(&read_back(vm, &alice(), APP)["contributors"])
}

fn strings(wanted: &[&str]) -> Vec<String> {
    wanted.iter().map(|k| (*k).to_owned()).collect()
}

fn sharing_records(vm: &Vm) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| r["event"] == "sharing-changed")
        .collect()
}

/// Fails unless `person` finds `board` on their Dashboard as `shared`.
fn assert_listed(vm: &Vm, person: &User, context: &str) {
    let page = Dashboard::new(vm, person).list();
    let found: Vec<_> = page
        .applications()
        .into_iter()
        .filter(|a| a.name == APP)
        .collect();
    assert!(
        matches!(found.as_slice(), [one] if one.relation.as_deref() == Some("shared")),
        "{context}: expected {:?} to find {APP:?} on their Dashboard, shared with them\n{}",
        person.id,
        page.describe()
    );
}

/// Fails unless `person` does not find `board` on their Dashboard.
fn assert_not_listed(vm: &Vm, person: &User, context: &str) {
    let page = Dashboard::new(vm, person).list();
    assert!(
        !page.names().iter().any(|n| n == APP),
        "{context}: expected {:?} not to find {APP:?} on their Dashboard\n{}",
        person.id,
        page.describe()
    );
}

#[test]
fn a_person_shared_with_finds_it_and_opens_it_and_after_an_unshare_neither() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    assert_not_listed(&vm, &bob(), "before the share");

    let answer = dashboard.share(APP, "user", "bob");
    assert_changed(
        &answer,
        &dashboard.url("/"),
        APP,
        "shared",
        "sharing with bob by his user id",
    );
    assert_eq!(
        shared_with(&vm),
        strings(&["user:bob"]),
        "the change made is the API's"
    );
    assert_eq!(
        contributors(&vm),
        strings(&["user:alice"]),
        "nobody is made a Contributor here"
    );
    assert_listed(&vm, &bob(), "after the share");
    assert_opens(&vm, &bob(), APP, "bob, at his next request after the share");
    let records = sharing_records(&vm);
    assert!(
        matches!(records.as_slice(), [r] if r["application"] == APP
            && r["by"] == "alice"
            && keys(&r["added"]) == strings(&["user:bob"])
            && keys(&r["removed"]).is_empty()),
        "expected one `sharing-changed` record by alice adding bob, got {records:?}"
    );

    // What the change redirects to says so.
    let notice = dashboard.page(&format!("/applications/{APP}?changed=shared"), 200, "after");
    assert_eq!(notice.notices(), strings(&["shared"]));

    let answer = dashboard.unshare(APP, "user", "bob");
    assert_changed(
        &answer,
        &dashboard.url("/"),
        APP,
        "unshared",
        "unsharing bob with his entry's form",
    );
    assert!(shared_with(&vm).is_empty(), "the entry removed");
    assert_not_listed(&vm, &bob(), "after the unshare");
    assert_cannot_open(
        &vm,
        &bob(),
        APP,
        "bob, at his next request after the unshare",
    );
    let records = sharing_records(&vm);
    assert!(
        records.len() == 2
            && keys(&records[1]["removed"]) == strings(&["user:bob"])
            && keys(&records[1]["added"]).is_empty(),
        "expected a second `sharing-changed` record removing bob, got {records:?}"
    );
}

#[test]
fn a_group_and_everyone_are_shared_with_and_unshared_from_the_dashboard() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let base = dashboard.url("/");

    assert_changed(
        &dashboard.share(APP, "group", "finance"),
        &base,
        APP,
        "shared",
        "the Group finance",
    );
    assert_listed(&vm, &carol(), "carol, in finance");
    assert_opens(&vm, &carol(), APP, "carol, in finance");
    assert_not_listed(&vm, &dave(), "dave, in no Group");

    assert_changed(
        &dashboard.share(APP, "everyone", "whatever is typed"),
        &base,
        APP,
        "shared",
        "everyone (the value is not looked at)",
    );
    assert_eq!(shared_with(&vm), strings(&["group:finance", "everyone"]));
    assert_listed(&vm, &dave(), "dave, once it is shared with everyone");
    assert_opens(&vm, &dave(), APP, "dave, once it is shared with everyone");

    assert_changed(
        &dashboard.unshare(APP, "everyone", ""),
        &base,
        APP,
        "unshared",
        "unsharing everyone",
    );
    assert_not_listed(&vm, &dave(), "dave, after everyone is unshared");
    assert_cannot_open(&vm, &dave(), APP, "dave, after everyone is unshared");
    assert_listed(&vm, &carol(), "carol, still by her Group");

    assert_changed(
        &dashboard.unshare(APP, "group", "finance"),
        &base,
        APP,
        "unshared",
        "unsharing finance",
    );
    assert_not_listed(&vm, &carol(), "carol, after finance is unshared");
    assert!(shared_with(&vm).is_empty());
}

#[test]
fn a_change_that_changes_nothing_leads_to_the_notice_nothing() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let base = dashboard.url("/");
    let target = Dashboard::sharing_target(APP);
    assert_changed(
        &dashboard.share(APP, "user", "bob"),
        &base,
        APP,
        "shared",
        "the first share",
    );
    assert_changed(
        &dashboard.share(APP, "user", "bob"),
        &base,
        APP,
        "nothing",
        "bob again",
    );
    for (kind, value) in [
        ("group", "nobody-group"),
        ("user", "alice"),
        ("everyone", ""),
    ] {
        let answer = dashboard.post_form(
            &target,
            &form_body(&[("action", "unshare"), ("kind", kind), ("value", value)]),
        );
        assert_changed(
            &answer,
            &base,
            APP,
            "nothing",
            &format!("unsharing {kind} {value:?}, which it is not shared with"),
        );
    }
    assert_eq!(shared_with(&vm), strings(&["user:bob"]));
    assert_eq!(
        contributors(&vm),
        strings(&["user:alice"]),
        "a Contributor cannot take themselves off the Contributors here"
    );
    assert_eq!(
        sharing_records(&vm).len(),
        1,
        "expected a `sharing-changed` record only when something changed"
    );
}

#[test]
fn for_everyone_the_value_may_be_left_out() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let target = Dashboard::sharing_target(APP);
    assert_changed(
        &dashboard.post_form(
            &target,
            &form_body(&[("action", "share"), ("kind", "everyone")]),
        ),
        &dashboard.url("/"),
        APP,
        "shared",
        "everyone, with no `value` field",
    );
    assert_eq!(shared_with(&vm), strings(&["everyone"]));
    assert_changed(
        &dashboard.post_form(
            &target,
            &form_body(&[("action", "unshare"), ("kind", "everyone")]),
        ),
        &dashboard.url("/"),
        APP,
        "unshared",
        "unsharing everyone, with no `value` field",
    );
    assert!(shared_with(&vm).is_empty());
}

#[test]
fn surrounding_white_space_of_the_value_is_dropped() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    assert_changed(
        &dashboard.share(APP, "user", " \t bob  "),
        &dashboard.url("/"),
        APP,
        "shared",
        "bob with white space around",
    );
    assert_changed(
        &dashboard.share(APP, "group", "  finance "),
        &dashboard.url("/"),
        APP,
        "shared",
        "finance with white space around",
    );
    assert_eq!(shared_with(&vm), strings(&["user:bob", "group:finance"]));
}

#[test]
fn a_body_of_4_kib_is_taken_and_a_longer_one_is_400_malformed() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let target = Dashboard::sharing_target(APP);
    // Spaces after the value (`+` in a form) are dropped with the rest of
    // the white space around it.
    let padded = |to: usize, value: &str| {
        let start = format!("action=share&kind=user&value={value}");
        format!("{start}{}", "+".repeat(to - start.len()))
    };
    let longer = padded(MAX_FORM_BODY + 1, "bob");
    assert_refused(
        dashboard.post_form(&target, &longer),
        &dashboard.url("/"),
        400,
        "malformed",
        APP,
        "a body of 4 KiB and one byte",
    );
    assert!(
        shared_with(&vm).is_empty(),
        "nothing is changed by a refusal"
    );
    let exact = padded(MAX_FORM_BODY, "bob");
    assert_eq!(exact.len(), MAX_FORM_BODY, "the harness's own count");
    assert_changed(
        &dashboard.post_form(&target, &exact),
        &dashboard.url("/"),
        APP,
        "shared",
        "a body of exactly 4 KiB",
    );
}

#[test]
fn a_form_that_is_not_such_a_form_is_400_malformed_and_changes_nothing() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let target = Dashboard::sharing_target(APP);
    let long = "x".repeat(257);
    let bodies: Vec<(String, String)> = vec![
        (
            "no action".into(),
            form_body(&[("kind", "user"), ("value", "bob")]),
        ),
        (
            "no kind".into(),
            form_body(&[("action", "share"), ("value", "bob")]),
        ),
        (
            "no value".into(),
            form_body(&[("action", "share"), ("kind", "user")]),
        ),
        (
            "action twice".into(),
            form_body(&[
                ("action", "share"),
                ("action", "share"),
                ("kind", "user"),
                ("value", "bob"),
            ]),
        ),
        (
            "kind twice".into(),
            form_body(&[
                ("action", "share"),
                ("kind", "user"),
                ("kind", "user"),
                ("value", "bob"),
            ]),
        ),
        (
            "value twice".into(),
            form_body(&[
                ("action", "share"),
                ("kind", "user"),
                ("value", "bob"),
                ("value", "bob"),
            ]),
        ),
        (
            "another action".into(),
            form_body(&[("action", "add"), ("kind", "user"), ("value", "bob")]),
        ),
        (
            "an action in capitals".into(),
            form_body(&[("action", "Share"), ("kind", "user"), ("value", "bob")]),
        ),
        (
            "another kind".into(),
            form_body(&[("action", "share"), ("kind", "team"), ("value", "bob")]),
        ),
        (
            "an empty user id".into(),
            form_body(&[("action", "share"), ("kind", "user"), ("value", "   ")]),
        ),
        (
            "an empty Group".into(),
            form_body(&[("action", "share"), ("kind", "group"), ("value", "")]),
        ),
        (
            "a user id of 257 bytes".into(),
            form_body(&[("action", "share"), ("kind", "user"), ("value", &long)]),
        ),
        (
            "a Group of 257 bytes".into(),
            form_body(&[("action", "share"), ("kind", "group"), ("value", &long)]),
        ),
        (
            "a user id with a control character".into(),
            form_body(&[("action", "share"), ("kind", "user"), ("value", "bo\u{1}b")]),
        ),
        ("not a form at all".into(), "%%%".to_owned()),
    ];
    for (what, body) in &bodies {
        assert_refused(
            dashboard.post_form(&target, body),
            &dashboard.url("/"),
            400,
            "malformed",
            APP,
            &format!("{what}: {body}"),
        );
    }
    for content_type in [
        "text/plain",
        "application/json",
        "multipart/form-data; boundary=x",
    ] {
        let request = dashboard
            .bare("POST", &target)
            .header("accept", "text/html")
            .header("origin", dashboard.origin())
            .header("sec-fetch-site", "same-origin")
            .header("content-type", content_type)
            .body("action=share&kind=user&value=bob");
        assert_refused(
            request.send(),
            &dashboard.url("/"),
            400,
            "malformed",
            APP,
            &format!("a body of {content_type}"),
        );
    }
    let none = dashboard
        .bare("POST", &target)
        .header("accept", "text/html")
        .header("origin", dashboard.origin())
        .body("action=share&kind=user&value=bob");
    assert_refused(
        none.send(),
        &dashboard.url("/"),
        400,
        "malformed",
        APP,
        "no content type",
    );
    assert!(shared_with(&vm).is_empty(), "a refusal changes nothing");
    assert!(
        sharing_records(&vm).is_empty(),
        "no `sharing-changed` record for a refusal"
    );
}

#[test]
fn not_found_and_not_a_contributor_are_refused_as_the_api_refuses_them() {
    let vm = with_board();
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    let share_dave = form_body(&[("action", "share"), ("kind", "user"), ("value", "dave")]);

    let alices = Dashboard::new(&vm, &alice());
    let page = assert_refused(
        alices.post_form(&Dashboard::sharing_target("nope"), &share_dave),
        &alices.url("/"),
        404,
        "not-found",
        "nope",
        "an Application of no name it has",
    );
    assert_refusal_words(
        &page,
        &[&["nope"]],
        "not-found names the Application's name",
    );

    for (person, why) in [
        (bob(), "someone it is shared with"),
        (dave(), "someone who cannot open it"),
    ] {
        let dashboard = Dashboard::new(&vm, &person);
        let page = assert_refused(
            dashboard.post_form(&Dashboard::sharing_target(APP), &share_dave),
            &dashboard.url("/"),
            403,
            "not-a-contributor",
            APP,
            why,
        );
        assert_refusal_words(
            &page,
            &[&["Contributor", "contributor"]],
            &format!(
                "{why}: says only the Application's Contributors change whom it is shared with"
            ),
        );
        assert!(
            !page.response.text().contains("alice"),
            "{why}: expected the refusal to name no Contributor\n{}",
            page.describe()
        );
        // Not even a body that is not such a form tells them more.
        assert_refused(
            dashboard.post_form(&Dashboard::sharing_target(APP), "kind=team"),
            &dashboard.url("/"),
            403,
            "not-a-contributor",
            APP,
            &format!("{why}, with a malformed form"),
        );
    }
    assert_eq!(
        shared_with(&vm),
        strings(&["user:bob"]),
        "nothing is changed"
    );
}

#[test]
fn an_email_nobody_signed_in_with_is_422_unknown_email() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let address = "bob@example.test";
    let page = assert_refused(
        dashboard.share(APP, "email", address),
        &dashboard.url("/"),
        422,
        "unknown-email",
        APP,
        "an email with the stand-in, which keeps no record of people",
    );
    assert_refusal_words(
        &page,
        &[&[address], &["user id", "user ID"]],
        "unknown-email names the address as given and says to use the person's user id",
    );
    assert!(shared_with(&vm).is_empty());
}

#[test]
fn one_more_than_1000_entries_is_409_too_many() {
    let vm = with_board();
    let api = vm.api_as(&alice());
    let ids: Vec<String> = (0..MAX_SHARED_WITH)
        .map(|i| format!("person-{i:04}"))
        .collect();
    for chunk in ids.chunks(100) {
        let entries: Vec<Value> = chunk.iter().map(|id| user(id)).collect();
        share_ok(&api, &vm, APP, &add(&entries));
    }
    let dashboard = Dashboard::new(&vm, &alice());
    let answer = dashboard.post_form(
        &Dashboard::sharing_target(APP),
        &form_body(&[
            ("action", "share"),
            ("kind", "group"),
            ("value", "one-more"),
        ]),
    );
    let page = assert_refused(
        answer,
        &dashboard.url("/"),
        409,
        "too-many",
        APP,
        "a 1001st entry",
    );
    assert_refusal_words(
        &page,
        &[&["1,000", "1000"]],
        "too-many names the bound of 1,000",
    );
    assert_eq!(
        shared_with(&vm).len(),
        MAX_SHARED_WITH,
        "nothing is changed"
    );
}

#[test]
fn an_entry_with_the_characters_of_html_is_unshared_by_its_own_form() {
    let vm = with_board();
    let hostile_group = "<b>\"night\" & 'shift'</b>";
    let hostile_user = "o'neil & \"co\" <x>";
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        APP,
        &add(&[group(hostile_group), user(hostile_user)]),
    );
    let dashboard = Dashboard::new(&vm, &alice());
    let page = dashboard.details(APP);
    assert_eq!(
        page.shared_keys(),
        vec![
            format!("group:{hostile_group}"),
            format!("user:{hostile_user}")
        ],
        "expected `data-vm-id` to hold each exactly, escaped as HTML\n{}",
        page.describe()
    );
    assert_changed(
        &dashboard.unshare(APP, "group", hostile_group),
        &dashboard.url("/"),
        APP,
        "unshared",
        "the Group with the characters of HTML",
    );
    assert_changed(
        &dashboard.unshare(APP, "user", hostile_user),
        &dashboard.url("/"),
        APP,
        "unshared",
        "the user id with the characters of HTML",
    );
    assert!(
        shared_with(&vm).is_empty(),
        "both removed by their own forms"
    );
}

/// Fails unless the answer is the `403` of a request from another origin:
/// `text/plain`.
fn assert_other_origin(response: &Response, context: &str) {
    assert!(
        response.status == 403
            && response
                .header("content-type")
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/plain")),
        "{context}: expected `403`, `text/plain`, for a request from another origin\n{}",
        response.describe()
    );
}

#[test]
fn a_change_from_another_origin_is_refused_and_changes_nothing() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let target = Dashboard::sharing_target(APP);
    let body = form_body(&[("action", "share"), ("kind", "everyone"), ("value", "")]);
    let port = vm.address().port();
    let board_origin = format!("http://board.vm.test:{port}");
    let cases = [
        (
            "a form on an Application's page",
            dashboard
                .bare("POST", &target)
                .header("origin", &board_origin)
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(&body),
        ),
        (
            "a form on another site's page",
            dashboard
                .bare("POST", &target)
                .header("origin", "http://evil.example")
                .header("sec-fetch-site", "cross-site")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(&body),
        ),
        (
            "Origin of the Dashboard over https, the installation being http",
            dashboard
                .bare("POST", &target)
                .header("origin", "https://dashboard.vm.test")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(&body),
        ),
        (
            "no Origin and Sec-Fetch-Site: cross-site",
            dashboard
                .bare("POST", &target)
                .header("sec-fetch-site", "cross-site")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(&body),
        ),
        (
            "Origin: null",
            dashboard
                .bare("POST", &target)
                .header("origin", "null")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(&body),
        ),
    ];
    for (what, request) in cases {
        assert_other_origin(&request.send(), what);
    }
    assert!(shared_with(&vm).is_empty(), "nothing was changed");
    assert!(
        sharing_records(&vm).is_empty(),
        "no `sharing-changed` record"
    );

    // Nor can another page read the Dashboard.
    for (what, request) in [
        (
            "a fetch from an Application's page",
            dashboard
                .bare("GET", "/")
                .header("origin", &board_origin)
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "cors")
                .header("sec-fetch-dest", "empty"),
        ),
        (
            "the Dashboard in a frame of an Application's page",
            dashboard
                .bare("GET", "/")
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "iframe"),
        ),
    ] {
        assert_other_origin(&request.send(), what);
    }
    // A link followed from another site is a navigation of the whole page.
    let followed = dashboard
        .bare("GET", "/")
        .header("accept", "text/html")
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document")
        .send();
    assert_eq!(
        followed.status,
        200,
        "expected a navigation of the whole page from another site to be answered\n{}",
        followed.describe()
    );
    // The same form from the Dashboard's own page is taken.
    assert_changed(
        &dashboard.post_form(&target, &body),
        &dashboard.url("/"),
        APP,
        "shared",
        "the same form from the Dashboard's own page",
    );
    assert_eq!(shared_with(&vm), strings(&["everyone"]));
}

#[test]
fn not_signed_in_a_change_is_401() {
    let vm = with_board();
    let dashboard = Dashboard::new(&vm, &alice());
    let answer = blackbox::Request::new(
        vm.address(),
        &Dashboard::sharing_target(APP),
        &format!("dashboard.vm.test:{}", vm.address().port()),
    )
    .method("POST")
    .header("origin", dashboard.origin())
    .header("content-type", "application/x-www-form-urlencoded")
    .body(form_body(&[
        ("action", "share"),
        ("kind", "everyone"),
        ("value", ""),
    ]))
    .send();
    assert!(
        answer.status == 401
            && answer
                .header("content-type")
                .is_some_and(|t| t.starts_with("text/plain")),
        "expected the `401` of \"Not signed in\"\n{}",
        answer.describe()
    );
    assert!(shared_with(&vm).is_empty());
}
