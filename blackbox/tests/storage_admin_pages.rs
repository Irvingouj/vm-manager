//! The Platform administrator's pages with a Storage Resource (CONTRACT.md,
//! "Storage: The tool and the Dashboard", last bullet; ticket "Storage: a
//! shared, named file Resource, on a mounted directory now and S3 later"):
//! wherever a Resource is shown, its element (`data-vm-resource="<name>"`)
//! has `data-vm-kind="postgres" | "storage"` for what it is registered as,
//! and a Storage Resource is never said or marked to be a database: the
//! text of its element does not have the word `database`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use blackbox::User;
use blackbox::admin_pages::{APPLICATIONS, OPERATION_LOG, RESOURCES};
use blackbox::dashboard::{Dashboard, Page, administrator, create, stand_in_options};
use blackbox::page::Element;
use blackbox::sharing::TRACED;
use blackbox::storage::{MARKETING, Place, Storage};
use blackbox::vm::fixture_bundle;

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// A program for the Dashboard with [`MARKETING`] a Storage Resource and
/// `devops` a database Resource, and `board` (alice's) declaring both.
fn world() -> Storage {
    let place = Place::new("storage-admin-pages");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &stand_in_options());
    s.register(MARKETING);
    s.register_postgres("devops");
    s.grant(MARKETING, "staff", "read");
    s.grant("devops", "staff", "read");
    create(&s.vm, &alice(), "board", None);
    let sent = s.vm.api_as(&alice()).send_version(
        "board",
        &blackbox::storage::version_body_with_kinds(
            &fixture_bundle(TRACED),
            &[
                (MARKETING, "storage", "write"),
                ("devops", "postgres", "read"),
            ],
        ),
    );
    sent.expect_status(201);
    s
}

fn element<'a>(page: &'a Page, name: &str) -> Element<'a> {
    let found = page.root().with_attr_value("data-vm-resource", name);
    match found.as_slice() {
        [one] => *one,
        _ => panic!(
            "expected one element `data-vm-resource={name:?}`, found {}\n{}",
            found.len(),
            page.describe()
        ),
    }
}

/// Fails unless the element of `name` on this page says its kind, and the
/// Storage one does not say `database` in its text or as a screen reader
/// says it.
fn assert_kinds(page: &Page, context: &str) {
    let storage = element(page, MARKETING);
    assert_eq!(
        storage.attr("data-vm-kind"),
        Some("storage"),
        "{context}: expected the Storage Resource's element to say `data-vm-kind=\"storage\"`\n{}",
        page.describe()
    );
    let devops = element(page, "devops");
    assert_eq!(
        devops.attr("data-vm-kind"),
        Some("postgres"),
        "{context}: expected the database Resource's element to say `data-vm-kind=\"postgres\"`\n{}",
        page.describe()
    );
    for (what, text) in [("text", storage.text()), ("spoken text", storage.spoken())] {
        assert!(
            !text.to_lowercase().contains("database") && !text.to_lowercase().contains("postgres"),
            "{context}: expected neither `database` nor `Postgres` in the {what} of the Storage Resource's element, got {text:?}\n{}",
            page.describe()
        );
    }
}

#[test]
fn the_resources_page_says_each_resources_kind_and_a_storage_one_is_never_a_database() {
    let s = world();
    let page = Dashboard::new(&s.vm, &administrator()).page(RESOURCES, 200, "the Resources");
    assert_kinds(&page, "the Resources");
    // "where a Resource's kind is said in words, a Storage Resource's is
    // `Storage`".
    let text = element(&page, MARKETING).text();
    assert!(
        text.contains("Storage"),
        "expected the Storage Resource's kind said in words as `Storage`, got {text:?}\n{}",
        page.describe()
    );
}

#[test]
fn an_applications_page_as_a_platform_administrator_says_each_declared_resources_kind() {
    let s = world();
    let page = Dashboard::new(&s.vm, &administrator()).details("board");
    assert_kinds(&page, "board's page as a Platform administrator");
}

#[test]
fn the_list_of_every_application_never_says_a_storage_resource_is_a_database() {
    let s = world();
    let page = Dashboard::new(&s.vm, &administrator()).page(APPLICATIONS, 200, "every Application");
    let found = page
        .root()
        .with_attr_value("data-vm-declared-resource", MARKETING);
    assert_eq!(
        found.len(),
        1,
        "expected one element `data-vm-declared-resource={MARKETING:?}`\n{}",
        page.describe()
    );
    for text in [found[0].text(), found[0].spoken()] {
        assert!(
            !text.to_lowercase().contains("database") && !text.to_lowercase().contains("postgres"),
            "expected neither `database` nor `Postgres` in the Storage Resource's element, got {text:?}\n{}",
            page.describe()
        );
    }
}

#[test]
fn a_row_of_the_operation_log_naming_a_storage_resource_never_says_database_or_postgres() {
    let s = world();
    // A pull of the Storage Resource writes an entry of the Operation log
    // that names it.
    blackbox::structure::pull(&s.vm.api_as(&alice()), MARKETING).expect_status(200);
    let page =
        Dashboard::new(&s.vm, &administrator()).page(OPERATION_LOG, 200, "the Operation log");
    let rows: Vec<Element<'_>> = page
        .root()
        .with_attr("data-vm-entry")
        .into_iter()
        .filter(|e| e.text().contains(MARKETING))
        .collect();
    assert!(
        !rows.is_empty(),
        "expected a row naming the Resource\n{}",
        page.describe()
    );
    for row in rows {
        for text in [row.text(), row.spoken()] {
            assert!(
                !text.to_lowercase().contains("database")
                    && !text.to_lowercase().contains("postgres"),
                "expected neither `database` nor `Postgres` in the row of a Storage Resource, got {text:?}\n{}",
                page.describe()
            );
        }
    }
}

#[test]
fn an_applications_page_names_a_declared_storage_resource_without_calling_it_a_database() {
    let s = world();
    let page = Dashboard::new(&s.vm, &administrator()).details("board");
    // Wherever the page names the Resource, in an element of its own or
    // not: no element that names it and holds `database` or `Postgres`.
    for element in page.root().descendants() {
        let own: Vec<_> = element
            .attributes()
            .iter()
            .filter(|(_, v)| v == MARKETING)
            .collect();
        if own.is_empty() {
            continue;
        }
        for text in [element.text(), element.spoken()] {
            assert!(
                !text.to_lowercase().contains("database")
                    && !text.to_lowercase().contains("postgres"),
                "expected no `database` or `Postgres` in an element naming the Storage Resource, got {text:?}\n{}",
                page.describe()
            );
        }
    }
}
