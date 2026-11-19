//! Sharing and unsharing by the Dashboard's forms, and a refusal in words,
//! walked in real browsers over HTTPS (CONTRACT.md, "A real browser in the gate: What
//! it walks", 3 and 4; "The Dashboard: One Application", "Sharing from
//! it").
//!
//! Acceptance criteria: "In a real browser, as tests of the gate: ...
//! sharing and unsharing by the Dashboard's forms; a refusal shown in
//! words".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::firefox::{self, Firefox};
use blackbox::sessions::{ADA, GRACE, ada, grace};
use blackbox::walks::{
    Site, click_to_another_page, on_site, page_json, share_by_form, sign_in_to_dashboard,
    unshare_by_form, wait_for_element,
};

const APP: &str = "board";

fn site() -> Site {
    Site::start(&[ada(), grace()])
}

/// The `data-vm-shared` entries of the Application's page shown, as
/// `kind:id`.
fn shown_shared(browser: &mut Firefox) -> Vec<String> {
    let entries = browser.find_all("[data-vm-shared]");
    entries
        .iter()
        .map(|e| {
            let kind = browser.attribute(e, "data-vm-shared").unwrap_or_default();
            match browser.attribute(e, "data-vm-id") {
                Some(id) => format!("{kind}:{id}"),
                None => kind,
            }
        })
        .collect()
}

/// The notice of the page shown (`data-vm-notice`), if any.
fn notice(browser: &mut Firefox) -> Option<String> {
    let found = browser.find("[data-vm-notice]")?;
    browser.attribute(&found, "data-vm-notice")
}

#[test]
fn a_contributor_shares_by_the_form_the_other_person_opens_it_and_after_unsharing_is_refused() {
    firefox::required();
    let site = site();
    let credential = site.application_of(ADA, APP, Some("The incident board"));

    // Grace has signed in here once, so the platform knows her email.
    let mut grace_browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut grace_browser, &site, GRACE);
    assert!(
        grace_browser
            .find(&format!("[data-vm-application=\"{APP}\"]"))
            .is_none(),
        "expected Grace's Dashboard not to list it before it is shared\n{}",
        grace_browser.describe()
    );

    let mut ada_browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut ada_browser, &site, ADA);
    ada_browser.navigate(&site.dashboard(&format!("/applications/{APP}")));
    assert_eq!(
        shown_shared(&mut ada_browser),
        Vec::<String>::new(),
        "expected the page to show it shared with nobody yet"
    );
    share_by_form(&mut ada_browser, "email", "grace@example.test");
    assert_eq!(
        ada_browser.status(),
        Some(200),
        "expected the share taken and the browser sent to the page again\n{}",
        ada_browser.describe()
    );
    assert_eq!(
        notice(&mut ada_browser).as_deref(),
        Some("shared"),
        "expected the page the change led to to say it was shared\n{}",
        ada_browser.describe()
    );
    assert_eq!(
        shown_shared(&mut ada_browser),
        vec![format!("user:{GRACE}")],
        "expected the page to show it shared with Grace\n{}",
        ada_browser.describe()
    );
    assert_eq!(
        site.shared_with(&credential, APP),
        [format!("user:{GRACE}")]
    );

    grace_browser.navigate(&site.dashboard("/"));
    let listed = wait_for_element(
        &mut grace_browser,
        &format!("[data-vm-application=\"{APP}\"][data-vm-relation=\"shared\"]"),
    );
    let link = grace_browser.find_within(&listed, "a[data-vm-open]");
    assert_eq!(link.len(), 1, "{}", grace_browser.describe());
    click_to_another_page(&mut grace_browser, &link[0]);
    let url = grace_browser.url();
    assert!(on_site(&url, &site, APP), "{url}");
    assert_eq!(
        page_json(&mut grace_browser)["user"]["id"],
        GRACE,
        "expected Grace to open the Application shared with her\n{}",
        grace_browser.describe()
    );

    ada_browser.navigate(&site.dashboard(&format!("/applications/{APP}")));
    unshare_by_form(&mut ada_browser, "user", GRACE);
    assert_eq!(
        notice(&mut ada_browser).as_deref(),
        Some("unshared"),
        "expected the page the change led to to say it was unshared\n{}",
        ada_browser.describe()
    );
    assert_eq!(
        shown_shared(&mut ada_browser),
        Vec::<String>::new(),
        "expected the page no longer to show Grace\n{}",
        ada_browser.describe()
    );
    assert_eq!(site.shared_with(&credential, APP), Vec::<String>::new());

    grace_browser.navigate(&site.application(APP, "/"));
    assert_eq!(
        grace_browser.status(),
        Some(403),
        "expected Grace refused the Application after it was unshared\n{}",
        grace_browser.describe()
    );
    let said = grace_browser.text();
    assert!(
        said.contains(APP) && said.to_lowercase().contains("not shared"),
        "expected the refusal to say the Application is not shared with her\n{}",
        grace_browser.describe()
    );
    grace_browser.navigate(&site.dashboard("/"));
    assert!(
        grace_browser
            .find(&format!("[data-vm-application=\"{APP}\"]"))
            .is_none(),
        "expected Grace's Dashboard no longer to list it\n{}",
        grace_browser.describe()
    );
}

#[test]
fn sharing_with_an_unknown_email_is_refused_on_the_page_in_words_and_nothing_is_shared() {
    firefox::required();
    let site = site();
    let credential = site.application_of(ADA, APP, None);
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    browser.navigate(&site.dashboard(&format!("/applications/{APP}")));
    let unknown = "nobody-here@example.test";
    share_by_form(&mut browser, "email", unknown);
    assert_eq!(
        browser.status(),
        Some(422),
        "expected the refusal's status\n{}",
        browser.describe()
    );
    let error = browser.the("[data-vm-error]");
    assert_eq!(
        browser.attribute(&error, "data-vm-error").as_deref(),
        Some("unknown-email"),
        "{}",
        browser.describe()
    );
    let words = browser.text_of(&error);
    assert!(
        words.contains(unknown),
        "expected the refusal's words to name the address as given, got {words:?}"
    );
    let lower = words.to_lowercase();
    assert!(
        lower.contains("user id") && lower.contains("sign in"),
        "expected the words to say to use the person's user id or ask them to sign in once, got {words:?}"
    );
    let back = browser.the("a[data-vm-back]");
    click_to_another_page(&mut browser, &back);
    assert_eq!(
        shown_shared(&mut browser),
        Vec::<String>::new(),
        "expected nothing shared\n{}",
        browser.describe()
    );
    assert_eq!(site.shared_with(&credential, APP), Vec::<String>::new());
}
