//! One Application's page as a Platform administrator sees it, and as a
//! Contributor who is not one sees it (CONTRACT.md, "The Dashboard for
//! Platform administrators: One Application, as a Platform administrator
//! sees it"), with the stand-in sign-in: the Application's own page with
//! its usage (`data-vm-usage`), who disabled it (`data-vm-disabled-by`),
//! `a[data-vm-disable]` named "Disable <name>" when it is not disabled, one
//! `form[data-vm-form="enable"]` with one button "Enable <name>" when it is,
//! `a[data-vm-operation-log]`, and the notices `disabled`, `enabled` and
//! `nothing`; a Contributor sees none of these, and everything the page
//! shows anyone is shown as before.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{disable_ok, place_usage, usage_of};
use blackbox::admin_pages::{OPERATION_LOG, disable_target, enable_target, query_pairs};
use blackbox::dashboard::{Dashboard, Page, administrator, create, deploy, start};
use blackbox::sharing::{add, share_ok, user};
use blackbox::{User, Vm};

const APP: &str = "board";

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// `board`, alice's, a Version, shared with bob, used by two Users.
fn with_board() -> Vm {
    let vm = start();
    create(&vm, &alice(), APP, Some("The incident board"));
    deploy(&vm, &alice(), APP, &[]);
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    place_usage(&vm, APP, 0, "bob", 4);
    place_usage(&vm, APP, 1, "alice", 2);
    vm
}

fn admin_page(vm: &Vm, query: &str) -> Page {
    Dashboard::new(vm, &administrator()).page(
        &format!("/applications/{APP}{query}"),
        200,
        &format!("board's page{query} as a Platform administrator"),
    )
}

#[test]
fn a_platform_administrator_sees_its_usage_the_way_to_disable_it_and_its_operation_log() {
    let vm = with_board();
    let usage = usage_of(&vm, APP);
    let page = admin_page(&vm, "");
    let board = page.listed(APP);
    assert_eq!(
        board.relation.as_deref(),
        Some("none"),
        "not shared with the administrator: `data-vm-relation=\"none\"` as before\n{}",
        page.describe()
    );
    assert_eq!(
        page.contributor_keys(),
        ["user:alice"],
        "everything the page shows anyone, it shows them as before"
    );
    let root = page.root();
    let usages = root.with_attr("data-vm-usage");
    assert!(
        matches!(usages.as_slice(), [u] if u.attr("data-vm-requests") == Some(&usage["requests"].to_string())
            && u.attr("data-vm-users") == Some(&usage["users"].to_string())
            && u.text().to_lowercase().contains("in the last 30 days")),
        "expected one `data-vm-usage` with the API's usage {usage} and the words \"in the last 30 days\"\n{}",
        page.describe()
    );
    assert_eq!(
        (usage["requests"].as_u64(), usage["users"].as_u64()),
        (Some(6), Some(2)),
        "the harness's own check"
    );
    let disable = root.with_attr("data-vm-disable");
    assert!(
        matches!(disable.as_slice(), [a] if a.name() == "a"
            && a.attr("href").and_then(|h| page.url.join(h).ok()).is_some_and(|u| u.path() == disable_target(APP) && u.query().is_none())
            && a.accessible_name() == format!("Disable {APP}")),
        "expected one link `a[data-vm-disable]` to {} named \"Disable {APP}\"\n{}",
        disable_target(APP),
        page.describe()
    );
    assert!(
        page.forms_of("enable").is_empty() && page.forms_of("disable").is_empty(),
        "expected no enable form while it is not disabled, and no disable form here (it is a link to the confirmation)\n{}",
        page.describe()
    );
    let log = page
        .link("data-vm-operation-log")
        .unwrap_or_else(|| panic!("expected `a[data-vm-operation-log]`\n{}", page.describe()));
    assert_eq!(
        (log.path(), query_pairs(&log)),
        (
            OPERATION_LOG,
            vec![("application".to_owned(), APP.to_owned())]
        ),
        "expected the link to its Operation log"
    );
    assert_eq!(
        page.count("data-vm-disabled-by"),
        0,
        "not disabled: nobody disabled it"
    );
}

#[test]
fn when_it_is_disabled_a_platform_administrator_sees_by_whom_and_one_enable_form() {
    let vm = with_board();
    let disabled = disable_ok(&vm, APP)["disabled"].clone();
    let since = disabled["since"].as_str().unwrap();
    let by = disabled["by"].as_str().unwrap();
    let page = admin_page(&vm, "");
    let root = page.root();
    let marked = root.with_attr_value("data-vm-disabled", since);
    assert!(
        matches!(marked.as_slice(), [e] if e.attr("data-vm-disabled-by") == Some(by) && e.text().contains(by)),
        "expected the element `data-vm-disabled={since:?}` to have `data-vm-disabled-by={by:?}` and that id in its text\n{}",
        page.describe()
    );
    let forms = page.forms_of("enable");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"enable\"]`, found {}\n{}",
            forms.len(),
            page.describe()
        );
    };
    assert_eq!(
        form.method.as_deref(),
        Some("post"),
        "the enable form posts"
    );
    assert_eq!(
        form.action.as_deref(),
        Some(enable_target(APP).as_str()),
        "the enable form's action"
    );
    assert!(
        form.inputs.is_empty() && form.selects.is_empty() && form.buttons.len() == 1,
        "expected one button and no other field in the enable form: {form:?}"
    );
    let buttons = root
        .with_attr_value("data-vm-form", "enable")
        .first()
        .map(|f| {
            f.named("button")
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(buttons, [format!("Enable {APP}")], "the button's text");
    assert_eq!(
        page.count("data-vm-disable"),
        0,
        "disabled: no `a[data-vm-disable]`\n{}",
        page.describe()
    );
    assert!(
        page.link("data-vm-operation-log").is_some(),
        "its Operation log, as ever"
    );
}

#[test]
fn the_notices_disabled_enabled_and_nothing_are_said_and_any_other_value_is_ignored() {
    let vm = with_board();
    for changed in ["disabled", "enabled", "nothing"] {
        let page = admin_page(&vm, &format!("?changed={changed}"));
        let notices: Vec<(String, String)> = page
            .root()
            .with_attr("data-vm-notice")
            .iter()
            .map(|n| {
                (
                    n.attr("data-vm-notice").unwrap_or_default().to_owned(),
                    n.text(),
                )
            })
            .collect();
        assert!(
            matches!(notices.as_slice(), [(v, t)] if v == changed && !t.is_empty()),
            "?changed={changed}: expected one `data-vm-notice={changed:?}` saying so, got {notices:?}"
        );
    }
    for changed in ["Disabled", "disable", "on", ""] {
        let page = admin_page(&vm, &format!("?changed={changed}"));
        assert!(
            page.notices().is_empty(),
            "?changed={changed:?}: expected any other value ignored, got {:?}",
            page.notices()
        );
    }
}

#[test]
fn a_contributor_who_is_not_a_platform_administrator_sees_none_of_it() {
    let vm = with_board();
    let disabled = disable_ok(&vm, APP)["disabled"].clone();
    let since = disabled["since"].as_str().unwrap();
    for person in [alice(), bob()] {
        let dashboard = Dashboard::new(&vm, &person);
        for query in ["", "?changed=disabled", "?changed=enabled"] {
            let context = format!("{}'s page of board{query}", person.id);
            let page = dashboard.page(&format!("/applications/{APP}{query}"), 200, &context);
            for absent in [
                "data-vm-usage",
                "data-vm-disabled-by",
                "data-vm-disable",
                "data-vm-operation-log",
                "data-vm-notice",
            ] {
                assert_eq!(
                    page.count(absent),
                    0,
                    "{context}: expected no `{absent}`\n{}",
                    page.describe()
                );
            }
            assert!(
                page.forms_of("enable").is_empty(),
                "{context}: no enable form\n{}",
                page.describe()
            );
            assert_eq!(
                page.root().with_attr_value("data-vm-disabled", since).len(),
                1,
                "{context}: that it is disabled and since when, as before\n{}",
                page.describe()
            );
            assert!(
                !page.response.text().contains(&administrator().id),
                "{context}: who disabled it is not shown to anyone but a Platform administrator\n{}",
                page.describe()
            );
        }
    }
    // `?changed=nothing` is the sharing's notice, as before, to its
    // Contributor.
    let page = Dashboard::new(&vm, &alice()).page(
        &format!("/applications/{APP}?changed=nothing"),
        200,
        "alice's page with ?changed=nothing",
    );
    assert_eq!(
        page.notices(),
        ["nothing"],
        "the notice of a change of sharing that changed nothing"
    );
}
