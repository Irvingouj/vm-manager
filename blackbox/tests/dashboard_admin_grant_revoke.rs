//! Revoking a Grant from a Resource's page (CONTRACT.md, "Resources and
//! Grants in full: The Dashboard: a Resource's page and its Grants:
//! Revoking"), with the stand-in sign-in: the page that confirms a revoke
//! (its `<h1>`, what it says, one form with a hidden `group`, the way back,
//! no sections), a Group that holds none, `404` and `400`; the `POST` makes
//! the change `DELETE /v1/resources/<name>/grants/<group>` makes with the
//! same record, `303` to `?changed=revoked` or `nothing`; the Grant is gone
//! at the API; a User of that Group is refused `no-grant` through an
//! Application at once, with no renewal of their session.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::admin_pages::{
    api_grants, assert_changed_resource, assert_error_page, assert_no_sections, confirm_revoke,
    grants_shown, resource_target, revoke_page_target, revoke_target,
};
use blackbox::dashboard::{Dashboard, MAX_FORM_BODY, administrator, form_body, stand_in_options};
use blackbox::resources::{member_names, records_waited};
use blackbox::scenario::{RESOURCE, Scenario, administrator as admin_user, op, sql_on};
use serde_json::json;

const NAME: &str = "devops";

fn world() -> Scenario {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    s.register("other");
    s.grant(NAME, "sre", "write");
    s.grant(NAME, "staff", "read");
    s.grant("other", "sre", "read");
    s
}

#[test]
fn the_confirmation_names_the_group_and_the_resource_says_what_it_does_and_has_one_form() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&revoke_page_target(NAME, "sre"), 200, "the confirmation");
    let h1: Vec<String> = page.root().named("h1").iter().map(|h| h.text()).collect();
    assert_eq!(
        h1,
        [format!("Revoke sre on {NAME}?")],
        "its `<h1>`\n{}",
        page.describe()
    );
    let text = page.body_text();
    let lower = text.to_lowercase();
    for (words, what) in [
        ("write", "the level the Group holds"),
        (
            "unless another",
            "that it holds unless another of their Groups holds a Grant",
        ),
        ("deleted", "that nothing of the Resource is deleted"),
        ("restore", "that giving the Grant again restores it"),
    ] {
        assert!(
            lower.contains(words),
            "expected the words to say {what}; they say {text:?}"
        );
    }
    let forms = page.forms_of("revoke");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"revoke\"]`\n{}",
            page.describe()
        );
    };
    assert_eq!(form.method.as_deref(), Some("post"));
    assert_eq!(form.action.as_deref(), Some(revoke_target(NAME).as_str()));
    assert_eq!(
        form.hidden("group"),
        Some("sre"),
        "a hidden `group`: {form:?}"
    );
    assert_eq!(form.buttons.len(), 1, "one button: {form:?}");
    assert!(form.selects.is_empty(), "no other field: {form:?}");
    assert_eq!(page.forms().len(), 1);
    let root = page.root();
    let holders = root.with_attr_value("data-vm-confirm", NAME);
    assert_eq!(
        holders.len(),
        1,
        "one element `data-vm-confirm={NAME:?}`\n{}",
        page.describe()
    );
    assert_eq!(
        holders[0].attr("data-vm-group"),
        Some("sre"),
        "the element that holds it has `data-vm-group`"
    );
    let buttons: Vec<String> = holders[0]
        .with_attr_value("data-vm-form", "revoke")
        .iter()
        .flat_map(|f| f.named("button"))
        .map(|b| b.text())
        .collect();
    assert_eq!(
        buttons,
        ["Revoke sre"],
        "its one button, inside the element"
    );
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.path() == resource_target(NAME)),
        "expected a quiet link `a[data-vm-back]` to {}, got {back:?}",
        resource_target(NAME)
    );
    let back_text: Vec<String> = root
        .with_attr("data-vm-back")
        .iter()
        .map(|b| b.text())
        .collect();
    assert_eq!(back_text, ["Keep the Grant"], "the way back's words");
    assert_no_sections(&page, "the confirmation, a page of its own");
    assert_eq!(
        api_grants(&s.vm, NAME),
        [
            ("sre".to_owned(), "write".to_owned()),
            ("staff".to_owned(), "read".to_owned())
        ],
        "showing the page changes nothing"
    );
    // The level it names is the level that is held.
    let page = dashboard.page(&revoke_page_target(NAME, "staff"), 200, "staff");
    assert!(page.body_text().to_lowercase().contains("read"));
}

#[test]
fn a_group_with_awkward_characters_is_confirmed_and_revoked_by_its_exact_name() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for group in [
        "ops/team",
        "two words",
        "a&b=c",
        "say \"hi\"",
        "<b>x</b>",
        "Zoë ünï",
        "plus+sign",
        " sre",
        "100%",
    ] {
        s.grant(NAME, group, "read");
        let page = dashboard.page(&revoke_page_target(NAME, group), 200, group);
        let forms = page.forms_of("revoke");
        let [form] = forms.as_slice() else {
            panic!("{group:?}: expected one revoke form\n{}", page.describe());
        };
        assert_eq!(form.hidden("group"), Some(group), "{group:?}");
        let h1: Vec<String> = page.root().named("h1").iter().map(|h| h.text()).collect();
        let collapsed = format!("Revoke {group} on {NAME}?")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            h1,
            [collapsed],
            "{group:?}: the `<h1>`, white space collapsed"
        );
        assert_eq!(
            page.root().with_attr_value("data-vm-group", group).len(),
            1,
            "{group:?}: `data-vm-group` holds it exactly"
        );
        let html = page.response.text();
        assert!(
            !html.contains("<b>x</b>"),
            "{group:?}: escaped in the source"
        );
        let answer = confirm_revoke(&dashboard, NAME, group);
        assert_changed_resource(
            &answer,
            &dashboard.url("/"),
            NAME,
            "revoked",
            group,
            &format!("the Group {group:?}"),
        );
        assert!(
            !api_grants(&s.vm, NAME).iter().any(|(g, _)| g == group),
            "{group:?}: gone at the API"
        );
    }
    assert_eq!(
        api_grants(&s.vm, NAME).len(),
        2,
        "the others are as they were"
    );
    // The same name with the white space dropped is a Group of its own.
    s.grant(NAME, " lead", "read");
    s.grant(NAME, "lead", "write");
    assert_changed_resource(
        &confirm_revoke(&dashboard, NAME, " lead"),
        &dashboard.url("/"),
        NAME,
        "revoked",
        " lead",
        "a Group with a space before it",
    );
    assert!(
        api_grants(&s.vm, NAME).contains(&("lead".to_owned(), "write".to_owned())),
        "`lead` still holds its Grant"
    );
}

#[test]
fn a_group_that_holds_none_is_a_page_that_says_so_with_no_form() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for group in ["nobody", "SRE", "sre "] {
        let page = dashboard.page(&revoke_page_target(NAME, group), 200, group);
        assert!(
            page.forms().is_empty(),
            "{group:?}: no form\n{}",
            page.describe()
        );
        let back: Vec<String> = page
            .root()
            .with_attr("data-vm-back")
            .iter()
            .map(|b| b.text())
            .collect();
        assert!(
            back.len() == 1 && back[0] != "Keep the Grant",
            "{group:?}: there is no Grant to keep, got {back:?}"
        );
        let lower = page.body_text().to_lowercase();
        assert!(
            lower.contains("no grant")
                || lower.contains("holds none")
                || lower.contains("does not hold"),
            "{group:?}: expected the page to say that the Group holds no Grant there, got {lower:?}"
        );
    }
}

#[test]
fn no_such_resource_is_404_and_a_group_missing_twice_or_none_is_400_malformed() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for target in [
        format!("{}?group=sre", revoke_target("nope")),
        revoke_target("nope"),
        format!("{}?group=sre", revoke_target("Not_A_Name")),
    ] {
        let page = dashboard.page(&target, 404, &target);
        let errors = page.errors();
        assert!(
            matches!(errors.as_slice(), [(r, _)] if r == "not-found"),
            "{target}: `data-vm-error=\"not-found\"`, got {errors:?}"
        );
    }
    let long = "a".repeat(257);
    for (what, target) in [
        ("no group", revoke_target(NAME)),
        (
            "a group given twice",
            format!("{}?group=a&group=b", revoke_target(NAME)),
        ),
        ("an empty group", format!("{}?group=", revoke_target(NAME))),
        (
            "a group of 257 bytes",
            format!("{}?group={long}", revoke_target(NAME)),
        ),
        (
            "a control character",
            format!("{}?group=a%07b", revoke_target(NAME)),
        ),
        (
            "bytes that are not UTF-8",
            format!("{}?group=%FF%FE", revoke_target(NAME)),
        ),
    ] {
        let answer = dashboard.get(&target);
        let page = assert_error_page(
            answer,
            &dashboard.url("/"),
            400,
            "malformed",
            &resource_target(NAME),
            what,
        );
        assert!(page.forms().is_empty(), "{what}: no form to confirm");
    }
}

#[test]
fn confirming_revokes_303_to_the_page_with_revoked_and_the_grant_is_gone_at_the_api() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let answer = confirm_revoke(&dashboard, NAME, "sre");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "revoked",
        "sre",
        "the revoke confirmed",
    );
    assert_eq!(
        api_grants(&s.vm, NAME),
        [("staff".to_owned(), "read".to_owned())],
        "gone at the API, the other Group's Grant as it was"
    );
    assert_eq!(
        api_grants(&s.vm, "other"),
        [("sre".to_owned(), "read".to_owned())],
        "that Group's Grant on another Resource as it was"
    );
    let page = dashboard.page(
        &blackbox::admin_pages::changed_target(NAME, "revoked", "sre"),
        200,
        "after",
    );
    assert_eq!(page.notices(), ["revoked"]);
    let shown: Vec<String> = grants_shown(&page).into_iter().map(|g| g.group).collect();
    assert_eq!(shown, ["staff"], "the Grant is no longer on the page");
}

#[test]
fn revoking_a_grant_the_group_does_not_hold_is_nothing_and_logs_nothing() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    assert_changed_resource(
        &confirm_revoke(&dashboard, NAME, "sre"),
        &dashboard.url("/"),
        NAME,
        "revoked",
        "sre",
        "first",
    );
    // Posted again, as the confirmation of an earlier page would be.
    let answer = dashboard.post_form(&revoke_target(NAME), &form_body(&[("group", "sre")]));
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "nothing",
        "sre",
        "a second revoke",
    );
    let answer = dashboard.post_form(&revoke_target(NAME), &form_body(&[("group", "never-held")]));
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        NAME,
        "nothing",
        "never-held",
        "a Group that held none",
    );
    assert_eq!(
        records_waited(&s.vm, "grant-revoked", 1).len(),
        1,
        "one change, two that changed nothing"
    );
    let page = dashboard.page(
        &blackbox::admin_pages::changed_target(NAME, "nothing", "sre"),
        200,
        "nothing",
    );
    assert_eq!(page.notices(), ["nothing"]);
}

#[test]
fn the_log_has_the_grant_revoked_record_the_apis_revoke_makes_with_the_same_by() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    blackbox::resources::revoke(&s.admin(), NAME, "staff").expect_status(200);
    confirm_revoke(&dashboard, NAME, "sre");
    let records = records_waited(&s.vm, "grant-revoked", 2);
    assert_eq!(records.len(), 2, "{records:?}");
    let api = records.iter().find(|r| r["group"] == "staff").unwrap();
    let page = records.iter().find(|r| r["group"] == "sre").unwrap();
    assert_eq!(
        member_names(page),
        member_names(api),
        "the same members as the API's record: {page}"
    );
    assert_eq!(page["event"], "grant-revoked");
    assert_eq!(page["resource"], NAME);
    assert_eq!(page["was"], "write");
    assert_eq!(page["by"], json!(admin_user().id), "{page}");
    assert_eq!(api["by"], page["by"]);
}

#[test]
fn a_users_operation_through_an_application_is_no_grant_right_after_with_no_renewal() {
    let s = Scenario::start_with(&stand_in_options().args_extra(&["--session-renewal", "86400"]));
    s.register(RESOURCE);
    s.grant(RESOURCE, "crew", "read");
    s.install("board", "operations.js", &[(RESOURCE, "read")]);
    let carl = User::new("carl").in_groups(&["crew"]);
    let call = op("query", &sql_on(RESOURCE, "select 1 as one"));
    let (_, before) = s.one(&carl, "board", &call);
    assert!(
        before["ok"].is_object(),
        "the harness's own check: carl reads before the revoke: {before}"
    );
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let answer = confirm_revoke(&dashboard, RESOURCE, "crew");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        RESOURCE,
        "revoked",
        "crew",
        "the revoke",
    );
    let (_, after) = s.one(&carl, "board", &call);
    assert_eq!(
        after["refused"]["refusal"]["reason"], "no-grant",
        "the operation made after the `303` is refused, with no renewal of carl's session: {after}"
    );
    // Giving it again restores it.
    let answer = blackbox::admin_pages::set_access(&dashboard, RESOURCE, "crew", "read");
    assert_changed_resource(
        &answer,
        &dashboard.url("/"),
        RESOURCE,
        "granted",
        "crew",
        "giving it again",
    );
    let (_, again) = s.one(&carl, "board", &call);
    assert!(again["ok"].is_object(), "restored: {again}");
}

#[test]
fn a_group_missing_given_twice_or_none_in_the_post_is_400_malformed_and_nothing_changes() {
    let s = world();
    let before = api_grants(&s.vm, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let long = "a".repeat(257);
    for (what, body) in [
        ("no field", String::new()),
        ("another field", "x=sre".to_owned()),
        ("a group given twice", "group=sre&group=staff".to_owned()),
        ("an empty group", "group=".to_owned()),
        ("a group of 257 bytes", format!("group={long}")),
        ("a control character", "group=a%07b".to_owned()),
    ] {
        let answer = dashboard.post_form(&revoke_target(NAME), &body);
        assert_error_page(
            answer,
            &dashboard.url("/"),
            400,
            "malformed",
            &resource_target(NAME),
            what,
        );
        assert_eq!(api_grants(&s.vm, NAME), before, "{what}: nothing changed");
    }
    let padded = format!("group=sre&pad={}", "a".repeat(MAX_FORM_BODY));
    let answer = dashboard.post_form(&revoke_target(NAME), &padded);
    assert_error_page(
        answer,
        &dashboard.url("/"),
        400,
        "malformed",
        &resource_target(NAME),
        "a body over 4 KiB",
    );
    assert_eq!(api_grants(&s.vm, NAME), before, "nothing changed");
}

#[test]
fn a_body_not_whole_within_10_seconds_is_408_timed_out_and_changes_nothing() {
    let s = world();
    let before = api_grants(&s.vm, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let request = dashboard
        .form_request(&revoke_target(NAME), "group=sre")
        .declare_length(64)
        .timeout(Duration::from_secs(30));
    let began = Instant::now();
    let answer = request.send();
    let waited = began.elapsed();
    assert_error_page(
        answer,
        &dashboard.url("/"),
        408,
        "timed-out",
        &resource_target(NAME),
        "a body that does not come whole",
    );
    assert!(
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(20),
        "expected about 10 s, it answered after {waited:?}"
    );
    assert_eq!(api_grants(&s.vm, NAME), before, "nothing changed");
}

#[test]
fn the_post_to_a_resource_that_is_not_there_is_404_and_changes_nothing() {
    let s = world();
    let before = api_grants(&s.vm, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for name in ["nope", "Not_A_Name"] {
        let answer = dashboard.post_form(&revoke_target(name), "group=sre");
        let page = assert_error_page(
            answer,
            &dashboard.url("/"),
            404,
            "not-found",
            "/admin/resources",
            &format!("a revoke on {name:?}"),
        );
        assert!(blackbox::dashboard::error_text(&page).contains(name));
    }
    assert_eq!(api_grants(&s.vm, NAME), before, "nothing changed");
}
