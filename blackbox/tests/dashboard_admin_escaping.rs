//! Everything on the pages for Platform administrators that is not the
//! platform's own text is escaped as HTML (CONTRACT.md, "The Dashboard:
//! What it answers", which "The Dashboard for Platform administrators"
//! keeps on every page of its own, and its parts that name it: an entry's
//! statement, user id and reason; a Resource's description, owner, contact
//! and Groups; a filter's value; markup in an alias of a statement, since
//! a statement is recorded without its values): a description of `<script>alert(1)</script>`
//! is shown as those characters, never as markup. On a Resource's page and
//! the pages of its Grants ("Resources and Grants in full: The Dashboard: a
//! Resource's page and its Grants"): the page itself, the page that confirms
//! a revoke, the notice of `?changed=` and the field error of a form not
//! taken, each with a Group and a description made of markup, and a Group
//! kept in its field.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::admin_pages::{
    APPLICATIONS, OPERATION_LOG, RESOURCES, changed_target, disable_target, field_errors,
    grants_target, group_typed, log_target, resource_target, revoke_page_target, revoke_target,
};
use blackbox::dashboard::{
    Dashboard, Page, administrator, create, deploy, form_body, stand_in_options,
};
use blackbox::scenario::{READERS, RESOURCE};
use blackbox::sharing::{add, group, share_ok};
use blackbox::transactions::{APP, Target, Txs, outside};
use serde_json::json;

const HOSTILE_APP: &str = "hostile";
const DESCRIPTION: &str = "<script>alert(1)</script> & \"q\" 'a'";
const GROUP: &str = "<b>\"g\" & 'h'</b>";
const STATEMENT_MARK: &str = "<script>alert(2)</script>";
const OWNER: &str = "<em>Owners</em> & \"co\"";
const CONTACT: &str = "Ops <ops@example.test> & 'Q'";
const RESOURCE_DESCRIPTION: &str = "<img src=x onerror=alert(3)> database";

/// The raw markup that must never be on a page.
const RAW: [&str; 7] = [
    "<script>alert",
    "<i>o'neil",
    "<b>\"g\"",
    "\"co\"</i>",
    "<em>Owners",
    "<ops@example.test>",
    "<img src=x",
];

fn hostile_user() -> User {
    User::new("<i>o'neil & \"co\"</i>").in_groups(&[READERS, GROUP])
}

#[test]
fn every_text_of_an_application_an_entry_and_a_resource_is_escaped_on_every_page() {
    let t = Txs::start_with(&stand_in_options());
    let s = &t.scenario;
    let vm = t.vm();
    let h = hostile_user();
    // An Application of the hostile User's, described and shared with a
    // hostile Group.
    create(vm, &h, HOSTILE_APP, Some(DESCRIPTION));
    deploy(vm, &h, HOSTILE_APP, &[]);
    share_ok(&vm.api_as(&h), vm, HOSTILE_APP, &add(&[group(GROUP)]));
    // An entry by the hostile User whose statement holds markup in an
    // alias (a statement is recorded without its values: the markup is in
    // the statement's own text, not in a literal).
    t.run(
        &h,
        APP,
        &[outside(
            "query",
            Target::Shared(RESOURCE),
            &format!("select 1 as \"{STATEMENT_MARK}\""),
        )],
    );
    // A Resource whose texts hold markup, granted to the hostile Group.
    let registered = json!({
        "name": "hostile_res",
        "kind": "postgres",
        "description": RESOURCE_DESCRIPTION,
        "owner": OWNER,
        "contact": CONTACT,
        "connection": s.db.connection(&s.db.reader),
    });
    let body = s.register_body("hostile_res", &registered);
    assert_eq!(body["name"], "hostile_res", "the harness's own check");
    s.grant("hostile_res", GROUP, "read");

    let dashboard = Dashboard::new(vm, &administrator());
    let pages = [
        (APPLICATIONS.to_owned(), vec![DESCRIPTION, h.id.as_str()]),
        (
            format!("/applications/{HOSTILE_APP}"),
            vec![DESCRIPTION, h.id.as_str(), GROUP],
        ),
        (disable_target(HOSTILE_APP), vec![]),
        (
            OPERATION_LOG.to_owned(),
            vec![h.id.as_str(), STATEMENT_MARK],
        ),
        (
            log_target(&[("user", h.id.as_str())]),
            vec![h.id.as_str(), STATEMENT_MARK],
        ),
        (
            log_target(&[("user", "<i>nobody</i> & 'x'")]),
            vec!["<i>nobody</i> & 'x'"],
        ),
        (
            RESOURCES.to_owned(),
            vec![RESOURCE_DESCRIPTION, OWNER, CONTACT, GROUP],
        ),
        (
            resource_target("hostile_res"),
            vec![RESOURCE_DESCRIPTION, OWNER, CONTACT, GROUP],
        ),
        (
            revoke_page_target("hostile_res", GROUP),
            vec![GROUP, "hostile_res"],
        ),
        (
            changed_target("hostile_res", "granted", GROUP),
            vec![GROUP, RESOURCE_DESCRIPTION],
        ),
        (changed_target("hostile_res", "revoked", GROUP), vec![GROUP]),
        (
            revoke_page_target("hostile_res", "<i>nobody</i> & 'x'"),
            vec!["<i>nobody</i> & 'x'"],
        ),
    ];
    for (target, shown) in pages {
        let page = dashboard.page(&target, 200, &target);
        let html = page.response.text();
        for raw in RAW.iter().chain(["<i>nobody</i>"].iter()) {
            assert!(
                !html.contains(raw),
                "{target}: expected everything that is not the platform's own text escaped as HTML, but the page holds {raw:?}\n{html}"
            );
        }
        let text = page.body_text();
        for value in shown {
            assert!(
                text.contains(value),
                "{target}: expected {value:?} shown as those characters\n{}",
                page.describe()
            );
        }
    }
    // A Grant's Group in its attribute, exactly.
    let page = dashboard.page(RESOURCES, 200, "the Resources");
    let grants = page.root().with_attr_value("data-vm-grant", GROUP);
    assert_eq!(
        grants.len(),
        1,
        "the hostile Group's Grant, its attribute holding it exactly\n{}",
        page.describe()
    );
    // The same on the Resource's page, with its link to revoke it and the
    // confirmation's hidden field and attributes.
    let page = dashboard.page(&resource_target("hostile_res"), 200, "the Resource's page");
    assert_eq!(
        page.root().with_attr_value("data-vm-grant", GROUP).len(),
        1,
        "the hostile Group's Grant on the Resource's page, its attribute holding it exactly\n{}",
        page.describe()
    );
    let confirm = dashboard.page(
        &revoke_page_target("hostile_res", GROUP),
        200,
        "the confirmation",
    );
    assert_eq!(
        confirm.root().with_attr_value("data-vm-group", GROUP).len(),
        1,
        "`data-vm-group` holds the Group exactly\n{}",
        confirm.describe()
    );
    let forms = confirm.forms_of("revoke");
    assert_eq!(forms.len(), 1);
    assert_eq!(
        forms[0].hidden("group"),
        Some(GROUP),
        "the hidden field holds the Group exactly"
    );
    // A form not taken keeps what was typed, escaped, and its error is
    // plain text.
    let answer = dashboard.post_form(
        &grants_target("hostile_res"),
        &form_body(&[("group", GROUP), ("access", STATEMENT_MARK)]),
    );
    assert_eq!(answer.status, 400, "{}", answer.describe());
    let refused = Page::read(
        answer,
        dashboard.url(&grants_target("hostile_res")),
        "a field error",
    );
    let html = refused.response.text();
    for raw in RAW.iter().chain([STATEMENT_MARK].iter()) {
        assert!(
            !html.contains(raw),
            "the field error's page: expected everything typed escaped, but it holds {raw:?}\n{html}"
        );
    }
    assert_eq!(
        group_typed(&refused).as_deref(),
        Some(GROUP),
        "the Group in its field, exactly\n{}",
        refused.describe()
    );
    assert!(
        !field_errors(&refused).is_empty(),
        "an error beside the field\n{}",
        refused.describe()
    );
    assert!(
        refused.body_text().contains(GROUP) || group_typed(&refused).is_some(),
        "the Group is shown as those characters"
    );
    // A Group given in the body of a revoke that is not taken is not echoed
    // as markup either.
    let answer = dashboard.post_form(
        &revoke_target("hostile_res"),
        &form_body(&[("group", "<script>alert(2)</script>"), ("group", GROUP)]),
    );
    assert_eq!(answer.status, 400, "{}", answer.describe());
    let html = answer.text();
    assert!(!html.contains("<script>alert"), "{html}");
}
