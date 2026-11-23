//! The Dashboard's page of one Application with a Storage Resource
//! (CONTRACT.md, "Storage: The tool and the Dashboard", third bullet;
//! ticket "Storage: a shared, named file Resource, on a mounted directory
//! now and S3 later"): each declared Resource's element gains
//! `data-vm-kind="postgres" | "storage"`, with `data-vm-declared`,
//! `data-vm-yours`, `data-vm-standing` and `data-vm-ask` as for a database
//! (`yours` is never `structure` for Storage), and the list's
//! `data-vm-attention` counts Storage Resources like any.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use blackbox::User;
use blackbox::dashboard::{
    Dashboard, assert_attention, assert_attention_attribute, create, stand_in_options,
};
use blackbox::sharing::{TRACED, add, share_ok, user};
use blackbox::storage::{FILES_CONTACT, MARKETING, Place, Storage};
use blackbox::storage_tool::kind_of;
use blackbox::vm::{Vm, fixture_bundle};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// A program for the Dashboard with [`MARKETING`] registered as a Storage
/// Resource and `devops` as a database Resource, `staff` granted `read` on
/// both.
fn start() -> Storage {
    let place = Place::new("storage-dashboard");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &stand_in_options());
    s.register(MARKETING);
    s.register_postgres("devops");
    s.grant(MARKETING, "staff", "read");
    s.grant("devops", "staff", "read");
    s
}

/// Creates `name` as this User and sends `fixtures/honest.js` as its
/// Version declaring these Resources as (name, kind, access level);
/// fails the test unless it is `201`.
fn deploy(vm: &Vm, who: &User, name: &str, declared: &[(&str, &str, &str)]) {
    create(vm, who, name, None);
    let answer = vm.api_as(who).send_version(
        name,
        &blackbox::storage::version_body_with_kinds(&fixture_bundle(TRACED), declared),
    );
    if answer.status != 201 {
        vm.fail(&format!(
            "expected `201` sending a Version of {name:?} declaring {declared:?}, got\n{}",
            answer.describe()
        ));
    }
}

#[test]
fn each_declared_resource_element_has_data_vm_kind_and_the_attributes_as_for_a_database() {
    let s = start();
    deploy(
        &s.vm,
        &alice(),
        "board",
        &[
            (MARKETING, "storage", "write"),
            ("devops", "postgres", "read"),
        ],
    );
    share_ok(&s.vm.api_as(&alice()), &s.vm, "board", &add(&[user("bob")]));

    // alice holds `read` on both: the Storage Resource declared at `write`
    // is lower, the database declared at `read` is enough.
    let page = Dashboard::new(&s.vm, &alice()).details("board");
    let marketing = page.resource(MARKETING);
    assert_eq!(
        kind_of(&page, MARKETING).as_deref(),
        Some("storage"),
        "expected the Storage Resource's element to gain `data-vm-kind=\"storage\"`\n{}",
        page.describe()
    );
    assert_eq!(
        marketing.declared.as_deref(),
        Some("write"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.yours.as_deref(),
        Some("read"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.standing.as_deref(),
        Some("lower"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.ask.as_deref(),
        Some(FILES_CONTACT),
        "expected `data-vm-ask` the Resource's contact for a standing too low\n{}",
        page.describe()
    );
    let devops = page.resource("devops");
    assert_eq!(
        kind_of(&page, "devops").as_deref(),
        Some("postgres"),
        "expected a database's element to gain `data-vm-kind=\"postgres\"`\n{}",
        page.describe()
    );
    assert_eq!(
        devops.declared.as_deref(),
        Some("read"),
        "{}",
        page.describe()
    );
    assert_eq!(devops.yours.as_deref(), Some("read"), "{}", page.describe());
    assert_eq!(
        devops.standing.as_deref(),
        Some("enough"),
        "{}",
        page.describe()
    );
    assert_eq!(
        devops.ask,
        None,
        "expected no `data-vm-ask` where the standing is enough\n{}",
        page.describe()
    );
    // `yours` is never `structure` for Storage.
    assert!(
        !page.response.text().contains("data-vm-yours=\"structure\""),
        "expected no `data-vm-yours` of `structure` on the page of an Application declaring a Storage Resource\n{}",
        page.describe()
    );

    // bob, who holds no Grant, sees his own standing.
    let page = Dashboard::new(&s.vm, &bob()).details("board");
    let marketing = page.resource(MARKETING);
    assert_eq!(kind_of(&page, MARKETING).as_deref(), Some("storage"));
    assert_eq!(
        marketing.declared.as_deref(),
        Some("write"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.yours.as_deref(),
        Some("none"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.standing.as_deref(),
        Some("none"),
        "{}",
        page.describe()
    );
    assert_eq!(
        marketing.ask.as_deref(),
        Some(FILES_CONTACT),
        "expected `data-vm-ask` the Resource's contact for one who holds nothing\n{}",
        page.describe()
    );
}

#[test]
fn the_lists_attention_counts_storage_resources_like_any() {
    let s = start();
    let vm = &s.vm;
    // `fine`: everything alice needs. `ghostly`: declares a Storage
    // Resource nothing of is registered.
    deploy(vm, &alice(), "fine", &[(MARKETING, "storage", "read")]);
    deploy(vm, &alice(), "ghostly", &[("ghost", "storage", "read")]);

    let dashboard = Dashboard::new(vm, &alice());
    for (name, wanted) in [("fine", "none"), ("ghostly", "not-registered")] {
        let context = format!("alice's list, {name}");
        assert_attention(&dashboard.list().listed(name), wanted, &context);
        assert_attention_attribute(
            &dashboard.details(name).listed(name),
            wanted,
            &format!("the page of {name} for alice"),
        );
    }
    // The not-registered Storage Resource's element says its kind and its
    // standing, as for a database.
    let ghostly = dashboard.details("ghostly");
    let ghost = ghostly.resource("ghost");
    assert_eq!(
        kind_of(&ghostly, "ghost").as_deref(),
        Some("storage"),
        "{}",
        ghostly.describe()
    );
    assert_eq!(
        ghost.declared.as_deref(),
        Some("read"),
        "{}",
        ghostly.describe()
    );
    assert_eq!(
        ghost.yours.as_deref(),
        Some("none"),
        "{}",
        ghostly.describe()
    );
    assert_eq!(
        ghost.standing.as_deref(),
        Some("not-registered"),
        "{}",
        ghostly.describe()
    );

    // A Storage Resource the person cannot use is the worst standing on
    // the list: `missing-access`, as for a database.
    deploy(vm, &alice(), "asks", &[(MARKETING, "storage", "write")]);
    let list = dashboard.list();
    assert_attention(&list.listed("asks"), "missing-access", "alice's list, asks");
    let bobs = Dashboard::new(vm, &bob()).list();
    assert!(
        bobs.names().is_empty(),
        "the harness's own check: nothing is shared with bob yet"
    );
    share_ok(&vm.api_as(&alice()), vm, "asks", &add(&[user("bob")]));
    let bobs = Dashboard::new(vm, &bob()).list();
    assert_attention(&bobs.listed("asks"), "missing-access", "bob's list, asks");
}
