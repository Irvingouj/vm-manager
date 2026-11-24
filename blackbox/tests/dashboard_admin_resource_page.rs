//! A Resource's page on the Dashboard (CONTRACT.md, "Resources and Grants in
//! full: The Dashboard: a Resource's page and its Grants"), with the
//! stand-in sign-in: `GET /admin/resources/<name>` shows what `GET
//! /v1/admin/resources/<name>` answers of the Resource and every Grant `GET
//! /v1/resources/<name>/grants` answers (the Group, its level, who set it
//! and when, a link to revoke it, "not recorded" for a Grant from before
//! this stage), the form that sets a Grant, one step away its owner,
//! contact, who registered it, its logins or its location and the link to
//! its Operation log, why it is not usable, never a credential, the notice
//! of `?changed=`, `404` for no such Resource and for a name none could
//! have, `HEAD`.
//!
//! What the forms do is in `dashboard_admin_grant_set.rs` and
//! `dashboard_admin_grant_revoke.rs`; who is refused in
//! `dashboard_admin_grants_refused.rs`; the `503` in
//! `dashboard_admin_grants_lost_answer.rs`.
//!
//! `data-vm-grants-more` on a Resource's page (a Resource that held more
//! than 1,000 Grants before stage three) cannot be made from outside
//! without rows by hand, and is not tested here; on the list it is, in
//! `dashboard_admin_resources.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin_pages::{
    OPERATION_LOG, RESOURCES, assert_sections, assert_title, changed_target, error_words,
    grant_form, grants_shown, grants_target, query_pairs, resource_target, revoke_target,
};
use blackbox::dashboard::{Dashboard, Page, administrator, stand_in_options};
use blackbox::resources::{
    admin_resource, admin_resources, body as registration, connection, fill_grants,
    forget_who_and_when, login_holding_nothing, named_login, not_served,
};
use blackbox::scenario::{Scenario, administrator as admin_user};
use blackbox::sealed::DEVOPS;
use blackbox::shared_database::token;
use blackbox::sharing::assert_own_page_headers;
use blackbox::storage::{FILE_WRITERS, FILES, MARKETING, Place, Storage, get, wanda};
use blackbox::{Response, VmOptions};
use serde_json::{Value, json};

const NAME: &str = "devops";

/// A program with the stand-in, [`NAME`] registered with a read, a write
/// and a structure login, `sre` given `write` and `staff` `read`.
fn world() -> Scenario {
    let s = Scenario::start_with(&stand_in_options());
    let structure = login_holding_nothing(&s.db);
    s.register_body(
        NAME,
        &registration(
            NAME,
            &connection(&s.db, &s.db.reader, Some(&s.db.writer), Some(&structure)),
        ),
    );
    s.grant(NAME, "sre", "write");
    s.grant(NAME, "staff", "read");
    s
}

/// A program with the stand-in and a storage root, [`MARKETING`] registered
/// and `designers` given `write`.
fn storage_world() -> Storage {
    let place = Place::new("dashboard-resource");
    let root = place.root();
    let s = Storage::start_on(
        place,
        Some(&root),
        &VmOptions::new().args_extra(&["--public-scheme", "http"]),
    );
    s.register(MARKETING);
    s.grant(MARKETING, "designers", "write");
    s
}

fn api_resource(s: &Scenario, name: &str) -> Value {
    let answer = admin_resource(&s.admin(), name);
    answer.expect_status(200);
    answer.json()
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

fn title_of(page: &Page) -> Vec<String> {
    page.root()
        .named("title")
        .iter()
        .map(blackbox::page::Element::text)
        .collect()
}

#[test]
fn the_page_shows_what_the_api_answers_of_the_resource_and_every_grant() {
    let s = world();
    let api = api_resource(&s, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    assert_eq!(
        page.root()
            .named("h1")
            .iter()
            .map(blackbox::page::Element::text)
            .collect::<Vec<_>>(),
        [NAME],
        "its `<h1>` is the Resource's name\n{}",
        page.describe()
    );
    assert_eq!(
        title_of(&page),
        [format!("{NAME} — Dashboard")],
        "`<title>`: the name, then \" — Dashboard\""
    );
    assert_title(&page, "the Resource's page");
    assert_sections(&page, "resources", "the Resource's page");

    let holders = page.root().with_attr_value("data-vm-resource", NAME);
    assert_eq!(
        holders.len(),
        1,
        "one element `data-vm-resource={NAME:?}`\n{}",
        page.describe()
    );
    let holder = holders[0];
    assert_eq!(holder.attr("data-vm-kind"), Some("postgres"));
    let text = holder.text();
    assert!(
        text.contains(api["description"].as_str().unwrap()),
        "its description seen first, got {text:?}"
    );
    assert!(text.contains("Postgres"), "its kind in words, got {text:?}");
    assert_eq!(
        page.count("data-vm-unusable"),
        0,
        "a Resource that is served is not marked not usable"
    );

    let api_grants = api["grants"].as_array().unwrap();
    let shown = grants_shown(&page);
    assert_eq!(
        shown.len(),
        api_grants.len(),
        "every Grant, by Group\n{}",
        page.describe()
    );
    for (grant, api) in shown.iter().zip(api_grants) {
        let context = format!("the Grant of {:?}", grant.group);
        assert_eq!(grant.group, api["group"].as_str().unwrap(), "{context}");
        assert_eq!(
            grant.access.as_deref(),
            api["access"].as_str(),
            "{context}: `data-vm-access`"
        );
        assert_eq!(
            grant.by.as_deref(),
            api["granted_by"].as_str(),
            "{context}: `data-vm-granted-by`"
        );
        assert_eq!(
            grant.at.as_deref(),
            api["granted_at"].as_str(),
            "{context}: `data-vm-granted-at` as the API gives it"
        );
        assert_eq!(grant.by.as_deref(), Some(admin_user().id.as_str()));
        let minute: String = api["granted_at"]
            .as_str()
            .unwrap()
            .chars()
            .take(16)
            .collect();
        assert!(
            grant.text.contains(&grant.group)
                && grant.text.contains(grant.access.as_deref().unwrap())
                && grant.text.contains(&admin_user().id)
                && (grant.text.contains(&minute) || grant.text.contains(&minute.replace('T', " "))),
            "{context}: expected the Group, its level, who set it and when (UTC, to the minute: {minute}) in {:?}",
            grant.text
        );
        assert!(
            !grant.text.to_lowercase().contains("not recorded"),
            "{context}: a Grant with who and when does not say \"not recorded\""
        );
        assert_eq!(
            page.root()
                .with_attr_value("data-vm-grant", &grant.group)
                .iter()
                .filter(|g| g.has("data-vm-user"))
                .count(),
            0,
            "{context}: a Grant carries no `data-vm-user`"
        );
    }
    let groups: Vec<&str> = shown.iter().map(|g| g.group.as_str()).collect();
    assert_eq!(groups, ["sre", "staff"], "by Group");
    assert_eq!(page.count("data-vm-empty"), 0, "it has Grants");
    assert_eq!(page.count("data-vm-grants-more"), 0);
}

#[test]
fn each_grant_has_one_revoke_link_named_for_its_group_and_the_resource_is_not_said_again() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    let links = page.root().with_attr("data-vm-revoke");
    assert_eq!(
        links.len(),
        2,
        "one link for each Grant\n{}",
        page.describe()
    );
    for grant in grants_shown(&page) {
        let [(href, name)] = grant.revoke.as_slice() else {
            panic!(
                "expected one `a[data-vm-revoke]` in the Grant of {:?}\n{}",
                grant.group,
                page.describe()
            );
        };
        assert_eq!(href.path(), revoke_target(NAME));
        assert_eq!(
            query_pairs(href),
            vec![("group".to_owned(), grant.group.clone())],
            "the link leads to the confirmation of that Group"
        );
        assert_eq!(
            name,
            &format!("Revoke {}", grant.group),
            "its name, as a screen reader says it, is \"Revoke <Group>\""
        );
    }
    for link in links {
        assert_eq!(link.name(), "a", "a link, not a form");
    }
    let form = grant_form(&page);
    let button: Vec<String> = page
        .root()
        .with_attr_value("data-vm-form", "grant")
        .iter()
        .flat_map(|f| f.named("button"))
        .map(|b| b.text())
        .collect();
    assert_eq!(button, ["Set access"], "the form's one button: {form:?}");
}

#[test]
fn a_group_with_awkward_characters_is_percent_encoded_in_the_revoke_link_and_escaped_in_the_source()
{
    let s = world();
    let groups = [
        "ops/team",
        "two words",
        "a&b",
        "say \"hi\"",
        "<b>bold</b>",
        "Zoë ünï ログ",
        "plus+sign",
        "100%",
    ];
    for group in groups {
        s.grant(NAME, group, "read");
    }
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    let shown = grants_shown(&page);
    for group in groups {
        let found: Vec<_> = shown.iter().filter(|g| g.group == group).collect();
        let [grant] = found.as_slice() else {
            panic!(
                "expected one element `data-vm-grant={group:?}`, its attribute holding it exactly\n{}",
                page.describe()
            );
        };
        let [(href, _)] = grant.revoke.as_slice() else {
            panic!("expected one revoke link for {group:?}");
        };
        assert_eq!(href.path(), revoke_target(NAME), "{group:?}");
        assert_eq!(
            query_pairs(href),
            vec![("group".to_owned(), group.to_owned())],
            "{group:?}: the Group percent-encoded in the link's `href`: {href}"
        );
        let raw = page
            .root()
            .with_attr_value("data-vm-grant", group)
            .iter()
            .flat_map(|g| g.with_attr("data-vm-revoke"))
            .filter_map(|a| a.attr("href").map(str::to_owned))
            .next()
            .unwrap();
        assert!(
            raw.is_ascii() && !raw.contains([' ', '"', '<', '>']),
            "{group:?}: expected a tidy `href`, got {raw:?}"
        );
        // The link leads to the confirmation of that Group.
        let confirm = dashboard.page(
            &blackbox::browser::target_of(href),
            200,
            &format!("the link of {group:?}"),
        );
        let holders = confirm.root().with_attr_value("data-vm-group", group);
        assert_eq!(
            holders.len(),
            1,
            "{group:?}: the link leads to the confirmation of that Group\n{}",
            confirm.describe()
        );
    }
    let html = page.response.text();
    for raw in ["<b>bold</b>", "say \"hi\"</", "a&b<"] {
        assert!(
            !html.contains(raw),
            "expected the Groups escaped in the source, found {raw:?}"
        );
    }
    assert!(
        page.body_text().contains("<b>bold</b>"),
        "the markup is shown as those characters"
    );
}

#[test]
fn a_grant_from_before_this_stage_says_not_recorded_and_has_neither_attribute() {
    let s = world();
    forget_who_and_when(&s.vm);
    let Scenario { vm, db } = s;
    let s = Scenario {
        vm: blackbox::storage::terminate(vm).start(),
        db,
    };
    // One Grant set after the restart has both.
    s.grant(NAME, "newcomers", "structure");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    let shown = grants_shown(&page);
    let groups: Vec<&str> = shown.iter().map(|g| g.group.as_str()).collect();
    assert_eq!(groups, ["newcomers", "sre", "staff"], "{}", page.describe());
    for grant in &shown {
        let old = grant.group != "newcomers";
        assert_eq!(
            grant.by.is_none() && grant.at.is_none(),
            old,
            "{:?}: a Grant from before this stage has neither `data-vm-granted-by` nor `data-vm-granted-at`, one set after has both ({:?}, {:?})",
            grant.group,
            grant.by,
            grant.at
        );
        assert_eq!(
            grant.text.to_lowercase().contains("not recorded"),
            old,
            "{:?}: \"not recorded\" for a Grant from before this stage only, got {:?}",
            grant.group,
            grant.text
        );
        assert_eq!(
            grant.revoke.len(),
            1,
            "{:?}: an old Grant is revoked as any",
            grant.group
        );
    }
}

#[test]
fn with_no_grant_it_says_that_no_group_holds_one_so_nobodys_request_reaches_it() {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "no Grant");
    let empty = page.root().with_attr("data-vm-empty");
    assert_eq!(
        empty.len(),
        1,
        "one element `data-vm-empty`\n{}",
        page.describe()
    );
    let words = empty[0].text().to_lowercase();
    assert!(
        words.contains("no group") && words.contains("reaches") || words.contains("nobody"),
        "expected words that no Group holds a Grant, so nobody's request reaches the Resource; got {words:?}"
    );
    assert!(grants_shown(&page).is_empty());
    assert_eq!(
        page.forms_of("grant").len(),
        1,
        "the form is there to give the first"
    );
}

#[test]
fn the_form_sets_a_grant_with_a_visible_label_for_each_field_and_three_levels_for_a_database() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    let form = grant_form(&page);
    assert_eq!(form.method.as_deref(), Some("post"));
    assert_eq!(form.action.as_deref(), Some(grants_target(NAME).as_str()));
    assert!(
        form.has_text_input("group"),
        "a text input `group`: {form:?}"
    );
    assert_eq!(
        form.choices("access"),
        ["structure", "read", "write"],
        "a `select` `access` with the three levels of a database, in this order"
    );
    assert_eq!(form.buttons.len(), 1, "one button: {form:?}");
    assert_eq!(
        page.forms().len(),
        1,
        "no second form for changing a Grant; a revoke is a link\n{}",
        page.describe()
    );
    let root = page.root();
    let forms = root.with_attr_value("data-vm-form", "grant");
    let form_element = forms[0];
    let labels: Vec<String> = form_element
        .named("label")
        .iter()
        .map(blackbox::page::Element::text)
        .collect();
    assert!(
        labels.iter().any(|l| l.trim() == "Group") && labels.iter().any(|l| l.trim() == "Access"),
        "a visible label \"Group\" and one \"Access\", got {labels:?}"
    );
    let input = form_element
        .named("input")
        .into_iter()
        .find(|i| i.attr("name") == Some("group"))
        .unwrap();
    assert!(
        !input.has("maxlength"),
        "`maxlength` is not set: the bound is in bytes and the server says it"
    );
    let options: Vec<String> = form_element
        .named("option")
        .iter()
        .map(blackbox::page::Element::text)
        .collect();
    assert_eq!(
        options,
        ["Structure", "Read", "Write"],
        "the options' words"
    );
    let words = page.body_text().to_lowercase();
    assert!(
        words.contains("exactly") && words.contains("directory") && words.contains("at once"),
        "expected words under the form: a Group is written exactly as the company's directory names it, and a Grant holds at once in every Application; got {words:?}"
    );
    assert!(
        words.contains("changes it") || words.contains("another level"),
        "expected words that giving a Group that holds a Grant another level changes it; got {words:?}"
    );
}

#[test]
fn one_step_away_the_owner_contact_who_registered_it_its_logins_and_its_operation_log() {
    let s = world();
    let api = api_resource(&s, NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the Resource's page");
    let root = page.root();
    let details = root.named("details");
    let [details] = details.as_slice() else {
        panic!("expected one `<details>`\n{}", page.describe());
    };
    let text_of = |attribute: &str| -> Vec<String> {
        details
            .with_attr(attribute)
            .iter()
            .map(blackbox::page::Element::text)
            .collect()
    };
    assert_eq!(text_of("data-vm-owner"), [api["owner"].as_str().unwrap()]);
    assert_eq!(
        text_of("data-vm-contact"),
        [api["contact"].as_str().unwrap()]
    );
    let text = details.text();
    assert!(
        text.contains(api["created_by"].as_str().unwrap())
            && text.contains(
                &api["created_at"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .take(10)
                    .collect::<String>()
            ),
        "who registered it and when, got {text:?}"
    );
    let mut logins: Vec<String> = details
        .with_attr("data-vm-login")
        .iter()
        .map(|e| e.attr("data-vm-login").unwrap_or_default().to_owned())
        .collect();
    logins.sort();
    assert_eq!(
        logins,
        ["read", "structure", "write"],
        "an element `data-vm-login` for each login it has, in the `<details>`"
    );
    assert_eq!(
        root.with_attr("data-vm-login").len(),
        3,
        "the logins are one step away only"
    );
    let log = details.with_attr("data-vm-operation-log");
    let href = log
        .first()
        .and_then(|a| a.attr("href"))
        .and_then(|h| page.url.join(h).ok());
    assert!(
        href.as_ref().is_some_and(|u| u.path() == OPERATION_LOG
            && query_pairs(u) == vec![("resource".to_owned(), NAME.to_owned())]),
        "expected `a[data-vm-operation-log]` to {OPERATION_LOG}?resource={NAME}, got {href:?}"
    );
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.path() == RESOURCES && b.query().is_none()),
        "expected a quiet link `a[data-vm-back]` to {RESOURCES}, got {back:?}"
    );
}

#[test]
fn a_database_with_only_a_read_login_has_one_login_element() {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "a Resource with a read login");
    let logins: Vec<String> = page
        .root()
        .with_attr("data-vm-login")
        .iter()
        .map(|e| e.attr("data-vm-login").unwrap_or_default().to_owned())
        .collect();
    assert_eq!(logins, ["read"], "{}", page.describe());
}

#[test]
fn a_storage_resource_has_its_location_two_levels_and_no_login() {
    let s = storage_world();
    let api = admin_resource(&s.admin(), MARKETING);
    api.expect_status(200);
    let api = api.json();
    let directory = api["location"]["directory"].as_str().unwrap().to_owned();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(MARKETING), 200, "a Storage Resource");
    let holders = page.root().with_attr_value("data-vm-resource", MARKETING);
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].attr("data-vm-kind"), Some("storage"));
    let locations: Vec<String> = page
        .root()
        .with_attr("data-vm-location")
        .iter()
        .map(blackbox::page::Element::text)
        .collect();
    assert_eq!(
        locations,
        std::slice::from_ref(&directory),
        "the directory in an element `data-vm-location`, escaped\n{}",
        page.describe()
    );
    assert_eq!(
        page.count("data-vm-login"),
        0,
        "a Storage Resource has no login"
    );
    let form = grant_form(&page);
    assert_eq!(
        form.choices("access"),
        ["read", "write"],
        "two levels for a Storage Resource"
    );
    assert!(
        page.body_text().to_lowercase().contains("storage"),
        "its kind in words"
    );
    // The list shows no directory; this page and the API's answer do.
    let list = dashboard.page(RESOURCES, 200, "the list");
    assert!(
        !list.response.text().contains(&directory) && list.count("data-vm-location") == 0,
        "the list does not show a Storage Resource's location\n{}",
        list.describe()
    );
}

#[test]
fn a_resource_that_is_not_usable_says_in_words_what_is_wrong_and_what_puts_it_right() {
    for (problem, words) in [
        ("no-secret-key", vec!["key"]),
        ("does-not-open", vec!["vibe admin resource logins", DEVOPS]),
        ("not-sealed", vec!["vibe admin resource logins", DEVOPS]),
    ] {
        let s = not_served(problem);
        let dashboard = Dashboard::new(&s.vm, &administrator());
        let target = resource_target(DEVOPS);
        let began = Instant::now();
        let page = loop {
            let answer = dashboard.get(&target);
            if answer.status == 200 {
                break Page::read(answer, dashboard.url(&target), problem);
            }
            assert!(
                began.elapsed() < Duration::from_secs(20),
                "{problem}: expected the page once the Grants are known, got\n{}",
                answer.describe()
            );
            std::thread::sleep(Duration::from_millis(500));
        };
        let marks = page.root().with_attr("data-vm-unusable");
        assert_eq!(marks.len(), 1, "{problem}: one mark\n{}", page.describe());
        assert_eq!(marks[0].attr("data-vm-unusable"), Some(problem));
        let said = marks[0].text().to_lowercase();
        for word in words {
            assert!(
                said.contains(&word.to_lowercase()),
                "{problem}: expected the words to hold {word:?}, got {said:?}"
            );
        }
        assert!(
            marks[0].text().len() > 20,
            "{problem}: words for a person, not the bare word: {said:?}"
        );
        // Its Grants are shown all the same.
        assert!(
            !grants_shown(&page).is_empty(),
            "{problem}: the Grants are the program's, shown"
        );
    }
}

#[test]
fn a_storage_resource_whose_directory_is_gone_names_the_directory_as_the_problem() {
    let s = storage_world();
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    std::fs::remove_dir_all(s.dir(MARKETING)).unwrap();
    // The program finds out as it uses the directory.
    let _ = s.one(&wanda(), FILES, &get(MARKETING, "anything"));
    let api = admin_resource(&s.admin(), MARKETING).json();
    assert_eq!(
        api["usable"],
        json!(false),
        "the harness's own check: {api}"
    );
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(MARKETING), 200, "a lost directory");
    let marks = page.root().with_attr("data-vm-unusable");
    assert_eq!(marks.len(), 1, "{}", page.describe());
    assert_eq!(
        marks[0].attr("data-vm-unusable"),
        api["problem"].as_str(),
        "the problem the API says"
    );
    assert!(
        marks[0].text().to_lowercase().contains("directory"),
        "a Storage problem says what is wrong with the directory, got {:?}",
        marks[0].text()
    );
}

#[test]
fn a_resource_page_holds_never_a_credential_in_its_text_its_source_or_its_headers() {
    let s = Scenario::start_with(&stand_in_options());
    let read = named_login(
        &s.db,
        &format!("role_marker_read_{}", token()),
        &format!("password-marker-read-{}", token()),
    );
    let write = named_login(
        &s.db,
        &format!("role_marker_write_{}", token()),
        &format!("password-marker-write-{}", token()),
    );
    let registered = registration(NAME, &connection(&s.db, &read, Some(&write), None));
    s.register_body(NAME, &registered);
    s.grant(NAME, "sre", "write");
    let c = &registered["connection"];
    let mut markers = vec![
        c["database"].as_str().unwrap().to_owned(),
        c["read"]["role"].as_str().unwrap().to_owned(),
        c["read"]["password"].as_str().unwrap().to_owned(),
        c["write"]["role"].as_str().unwrap().to_owned(),
        c["write"]["password"].as_str().unwrap().to_owned(),
    ];
    if c["host"].as_str() != Some("127.0.0.1") {
        markers.push(c["host"].as_str().unwrap().to_owned());
    }
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for target in [
        resource_target(NAME),
        changed_target(NAME, "granted", "sre"),
        blackbox::admin_pages::revoke_page_target(NAME, "sre"),
    ] {
        let answer = dashboard.get(&target);
        assert_eq!(answer.status, 200, "{target}\n{}", answer.describe());
        let text = whole(&answer);
        for marker in &markers {
            assert!(
                !text.contains(marker.as_str()),
                "{target}: expected nothing of the connection in the answer, found {marker:?}"
            );
        }
        let port = c["port"].to_string();
        let lower = text.to_lowercase();
        assert!(
            !lower.contains(&format!("port {port}")) && !lower.contains(&format!(":{port}")),
            "{target}: expected no port of the connection"
        );
    }
}

#[test]
fn a_resource_holding_more_than_200_grants_shows_every_one_with_no_more_to_say() {
    let s = Scenario::start_with(&stand_in_options());
    s.register(NAME);
    fill_grants(&s.vm, NAME, 0, 201, "read");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "201 Grants");
    let shown = grants_shown(&page);
    assert_eq!(
        shown.len(),
        201,
        "the page shows every Grant (up to 1,000), the list's first 200 only"
    );
    let groups: Vec<&str> = shown.iter().map(|g| g.group.as_str()).collect();
    let mut sorted = groups.clone();
    sorted.sort_unstable();
    assert_eq!(groups, sorted, "by Group");
    assert_eq!(page.count("data-vm-grants-more"), 0);
}

#[test]
fn the_notice_for_each_of_the_four_values_names_the_group_and_says_what_happened() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for (changed, words) in [
        ("granted", vec!["given", "granted", "access"]),
        ("changed", vec!["changed"]),
        ("revoked", vec!["revoked", "taken away", "removed"]),
        ("nothing", vec!["nothing", "no change", "already"]),
    ] {
        let page = dashboard.page(
            &changed_target(NAME, changed, "sre"),
            200,
            &format!("changed={changed}"),
        );
        assert_eq!(page.notices(), [changed], "{}", page.describe());
        let notice = page.root().with_attr("data-vm-notice");
        let text = notice[0].text();
        assert!(
            text.contains("sre"),
            "{changed}: the notice names the Group, got {text:?}"
        );
        let lower = text.to_lowercase();
        assert!(
            words.iter().any(|w| lower.contains(w)),
            "{changed}: expected the notice to say what happened ({words:?}), got {text:?}"
        );
        assert!(
            !grants_shown(&page).is_empty(),
            "{changed}: the list of Grants under it says what holds now"
        );
    }
    // A Group that holds none now is named all the same.
    let page = dashboard.page(
        &changed_target(NAME, "revoked", "gone-group"),
        200,
        "revoked",
    );
    assert_eq!(page.notices(), ["revoked"]);
    assert!(
        page.root().with_attr("data-vm-notice")[0]
            .text()
            .contains("gone-group")
    );
}

#[test]
fn another_value_or_no_group_or_one_that_is_none_has_no_notice() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let too_long = "a".repeat(257);
    let targets = [
        format!("{}?changed=bogus&group=sre", resource_target(NAME)),
        format!("{}?changed=Granted&group=sre", resource_target(NAME)),
        format!("{}?changed=&group=sre", resource_target(NAME)),
        format!("{}?changed=granted", resource_target(NAME)),
        format!("{}?changed=granted&group=", resource_target(NAME)),
        format!("{}?group=sre", resource_target(NAME)),
        format!("{}?changed=granted&group={too_long}", resource_target(NAME)),
        format!("{}?changed=granted&group=a%07b", resource_target(NAME)),
        format!("{}?changed=granted&group=%FF%FE", resource_target(NAME)),
        format!("{}?changed=revoked&group=", resource_target(NAME)),
    ];
    for target in targets {
        let page = dashboard.page(&target, 200, &target);
        assert!(
            page.notices().is_empty(),
            "{target}: expected no notice\n{}",
            page.describe()
        );
        assert_eq!(
            grants_shown(&page).len(),
            2,
            "{target}: the page is the Resource's all the same"
        );
    }
}

#[test]
fn no_such_resource_or_a_name_none_could_have_is_404_naming_it_with_a_way_back_to_the_list() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for (target_name, shown) in [
        ("nope", Some("nope")),
        ("Not_A_Name", Some("Not_A_Name")),
        ("-x", Some("-x")),
        ("x.y", Some("x.y")),
        ("UPPER", Some("UPPER")),
        ("a%20b", None),
        ("caf%C3%A9", None),
        ("%00", None),
    ] {
        let target = resource_target(target_name);
        let page = dashboard.page(&target, 404, &target);
        let errors = page.errors();
        assert!(
            matches!(errors.as_slice(), [(r, w)] if r == "not-found" && !w.contains("/v1/") && shown.is_none_or(|n| w.contains(n))),
            "{target}: expected one `data-vm-error=\"not-found\"` naming it, got {errors:?}"
        );
        let back = page.link("data-vm-back");
        assert!(
            back.as_ref()
                .is_some_and(|b| b.path() == RESOURCES && b.query().is_none()),
            "{target}: expected `a[data-vm-back]` to {RESOURCES}, got {back:?}"
        );
    }
    let long = "n".repeat(64);
    let page = dashboard.page(&resource_target(&long), 404, "a name of 64 characters");
    assert_eq!(page.errors().len(), 1);
    // A name that only differs in letter case is another name.
    let page = dashboard.page(&resource_target("Devops"), 404, "Devops");
    assert_eq!(page.errors().len(), 1);
    assert!(!error_words(&page).is_empty());
}

#[test]
fn head_is_answered_as_get_without_a_body_and_with_the_headers_of_a_page() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &administrator());
    for (target, status) in [
        (resource_target(NAME), 200),
        (changed_target(NAME, "granted", "sre"), 200),
        (blackbox::admin_pages::revoke_page_target(NAME, "sre"), 200),
        (resource_target("nope"), 404),
    ] {
        let get = dashboard.get(&target);
        let head = dashboard.navigation("HEAD", &target).send();
        assert_eq!(get.status, status, "GET {target}\n{}", get.describe());
        assert_eq!(head.status, status, "HEAD {target}\n{}", head.describe());
        assert!(head.body().is_empty(), "HEAD {target}: no body");
        assert_own_page_headers(&head, &format!("HEAD {target}"));
    }
}

#[test]
fn the_list_and_the_page_agree_on_the_resources_grants() {
    let s = world();
    let list = admin_resources(&s.admin(), "").json();
    let one = api_resource(&s, NAME);
    assert_eq!(list["resources"][0], one, "the harness's own check");
    let dashboard = Dashboard::new(&s.vm, &administrator());
    let page = dashboard.page(&resource_target(NAME), 200, "the page");
    let on_list = dashboard.page(RESOURCES, 200, "the list");
    let from_page: Vec<(String, Option<String>)> = grants_shown(&page)
        .into_iter()
        .map(|g| (g.group, g.access))
        .collect();
    let from_list: Vec<(String, Option<String>)> = grants_shown(&on_list)
        .into_iter()
        .map(|g| (g.group, g.access))
        .collect();
    assert_eq!(from_page, from_list);
}
