//! No page is wider than a narrow window, in a real browser (the
//! installation's pages over HTTPS, the local run's in plain HTTP) (CONTRACT.md,
//! "A real browser in the gate: What it walks", 8; "The Dashboard": "A page
//! is never wider than its window, down to a phone's 320 CSS pixels").
//!
//! Acceptance criteria: "A page is not wider than a window of 320 CSS
//! pixels (the check made by hand for "The Dashboard" becomes a test)".
//!
//! Each page is first held to a window 320 CSS pixels wide as the page
//! itself sees it (`window.innerWidth`), then its document must be no
//! wider (`document.documentElement.scrollWidth`). That this check fails
//! for a page that is too wide is the harness's own test
//! (`browser_harness.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin_pages::{
    APPLICATIONS, INSTALLATION, OPERATION_LOG, RESOURCES, SITE_RESOURCE, disable_target,
    prepare_site,
};
use blackbox::dev::Dev;
use blackbox::firefox::{self, Firefox, Options, no_wider_than};
use blackbox::local::{Local, local_project};
use blackbox::oidc::Person;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, ada, grace};
use blackbox::tool::Machine;
use blackbox::tool_sign_ins::{OPEN_TIMEOUT, tool_address_in};
use blackbox::walks::{Site, choose_at_provider, on_site, share_by_form, sign_in_to_dashboard};

/// The width of a phone's window, in CSS pixels.
const NARROW: u32 = 320;

/// An Application's name as long as a name may be (63 characters), with
/// no place to break.
fn longest_name() -> String {
    "incidentboard".repeat(5).chars().take(63).collect()
}

/// A description holding one long address with no place to break.
fn long_description() -> String {
    format!(
        "Runbook: https://wiki.example.test/{}",
        "operations-handbook-chapter".repeat(6)
    )
}

fn assert_narrow(browser: &mut Firefox, page: &str) {
    if let Err(problem) = no_wider_than(browser, u64::from(NARROW)) {
        let shown = browser.describe();
        panic!("{page}: {problem}\n{shown}");
    }
}

#[test]
fn the_platforms_pages_are_no_wider_than_a_window_320_css_pixels_wide() {
    firefox::required();
    let site = Site::start(&[ada(), grace()]);
    let long = longest_name();
    site.application_of(ADA, &long, Some(&long_description()));
    site.application_of(ADA, "board", Some("The incident board"));
    site.application_of(GRACE, "private", None);
    // Grace signs in once, so that Ada can share with her by email.
    let mut browser = site.browser(&[long.as_str(), "board", "private"]);
    sign_in_to_dashboard(&mut browser, &site, GRACE);
    drop(browser);

    let mut browser = site.browser(&[long.as_str(), "board", "private"]);
    browser.narrow(NARROW);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    assert!(
        browser
            .find(&format!("[data-vm-application=\"{long}\"]"))
            .is_some(),
        "expected the list to hold the Application of the longest name\n{}",
        browser.describe()
    );
    assert_narrow(&mut browser, "the Dashboard's list with Applications in it");

    browser.navigate(&site.dashboard(&format!("/applications/{long}")));
    share_by_form(&mut browser, "email", "grace@example.test");
    assert!(
        browser.find("form[data-vm-form=\"unshare\"]").is_some(),
        "expected the Contributor's page with an unshare form\n{}",
        browser.describe()
    );
    assert_narrow(
        &mut browser,
        "one Application's page as a Contributor sees it",
    );

    share_by_form(
        &mut browser,
        "email",
        "nobody-with-a-rather-long-address-here@example.test",
    );
    assert!(
        browser.find("[data-vm-error]").is_some(),
        "expected the refusal's page\n{}",
        browser.describe()
    );
    assert_narrow(&mut browser, "the Dashboard's page of a refusal");

    browser.navigate(&site.application("private", "/"));
    assert_eq!(browser.status(), Some(403), "{}", browser.describe());
    assert_narrow(
        &mut browser,
        "the page of an Application not shared with the person",
    );

    browser.navigate(&site.auth("/sign-out"));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    assert_narrow(&mut browser, "the sign-out page");

    let machine = Machine::new();
    let mut args = vec!["sign-in".to_owned()];
    args.extend(site.place());
    args.extend(["--no-browser".to_owned(), "--json".to_owned()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ca_name, ca_value) = site.ca_env();
    let running = machine.spawn_into_files(&machine.cwd(), &refs, &[(ca_name, ca_value.as_str())]);
    let started = std::time::Instant::now();
    let address = loop {
        if let Some(address) = tool_address_in(&running.stderr_so_far(), &site.origin("auth")) {
            break address;
        }
        assert!(started.elapsed() < OPEN_TIMEOUT, "no address from the tool");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    browser.navigate(address.as_str());
    choose_at_provider(&mut browser, site.provider(), ADA);
    browser.wait_for_page("the page of the tool's sign-in", |u| {
        on_site(u, &site, "auth")
    });
    assert!(
        browser.find("form input[name=\"code\"]").is_some(),
        "expected the tool's sign-in page asking for the check code\n{}",
        browser.describe()
    );
    assert_narrow(&mut browser, "the tool's sign-in page");
    drop(running);
}

/// A Group's name as long as the harness makes one, with no place to break.
fn long_group() -> String {
    "incident-responders-of-the-north-atlantic-region".repeat(2)
}

#[test]
fn the_pages_of_platform_administrators_are_no_wider_than_a_window_320_css_pixels_wide() {
    firefox::required();
    let admin = Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS]);
    let site = Site::start(&[admin, grace()]);
    let long = longest_name();
    let credential = site.application_of(ADA, &long, Some(&long_description()));
    let prepared = prepare_site(&site, ADA, GRACE);
    let post = |target: &str, body: &serde_json::Value| {
        blackbox::tool::bearer(&site.at.vm, target, &credential)
            .method("POST")
            .json(body)
            .send()
    };
    post(
        &format!("/v1/applications/{long}/disable"),
        &serde_json::json!({}),
    )
    .expect_status(200);
    post(
        &format!("/v1/resources/{SITE_RESOURCE}/grants"),
        &serde_json::json!({ "group": long_group(), "access": "write" }),
    )
    .expect_status(200);

    let mut browser = site.browser(&[long.as_str()]);
    browser.narrow(NARROW);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    for (target, css, page) in [
        (
            APPLICATIONS.to_owned(),
            format!("[data-vm-application=\"{long}\"]"),
            "every Application",
        ),
        (
            format!("/applications/{long}"),
            "form[data-vm-form=\"enable\"]".to_owned(),
            "the page of the longest name as a Platform administrator sees it",
        ),
        (
            disable_target(&long),
            "[data-vm-disabled]".to_owned(),
            "the page that confirms a disable, of one disabled already",
        ),
        (
            disable_target("board-not-there"),
            "[data-vm-error]".to_owned(),
            "the page of a disable of a name no Application has",
        ),
        (
            format!("{OPERATION_LOG}?outcome=nonsense&user={}", long_group()),
            "[data-vm-field-error]".to_owned(),
            "the Operation log with a malformed filter",
        ),
        (
            format!("{OPERATION_LOG}?user={}", long_group()),
            "[data-vm-empty]".to_owned(),
            "the Operation log with nothing matching a long filter",
        ),
        (
            INSTALLATION.to_owned(),
            "[data-vm-issuer]".to_owned(),
            "the installation",
        ),
        (
            RESOURCES.to_owned(),
            format!("[data-vm-grant=\"{}\"]", long_group()),
            "the Resources with a long Group",
        ),
    ] {
        browser.navigate(&site.dashboard(&target));
        assert!(
            browser.find(&css).is_some(),
            "{page}: expected `{css}`\n{}",
            browser.describe()
        );
        assert_narrow(&mut browser, page);
    }
    browser.navigate(&site.dashboard(OPERATION_LOG));
    let summaries = browser.find_all("[data-vm-entry] details summary");
    assert!(
        !summaries.is_empty(),
        "expected entries with a `<details>`\n{}",
        browser.describe()
    );
    for summary in &summaries {
        browser.click(summary);
    }
    assert_narrow(
        &mut browser,
        "the Operation log with every `<details>` opened",
    );
    drop(browser);

    let mut browser = site.browser(&[long.as_str()]);
    browser.narrow(NARROW);
    sign_in_to_dashboard(&mut browser, &site, GRACE);
    browser.navigate(&site.dashboard(APPLICATIONS));
    assert_eq!(browser.status(), Some(403), "{}", browser.describe());
    assert_narrow(
        &mut browser,
        "the refusal page of someone who is not a Platform administrator",
    );
    drop(prepared);
}

#[test]
fn the_local_runs_test_user_page_is_no_wider_than_a_window_320_css_pixels_wide() {
    firefox::required();
    let local = Local::new();
    let name = longest_name();
    let project = local_project(&local.machine, &name, &[]);
    let mut file = blackbox::local::dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "someone-with-a-long-id-".repeat(4),
            "displayName": "A display name long enough to need breaking somewhere",
            "email": format!("{}@example.test", "a-very-long-mailbox".repeat(3)),
            "groups": ["a-group-with-a-long-name-and-no-spaces-in-it-at-all"]
        }));
    blackbox::local::write_dev_json(&project.dir, &file);
    let dev = Dev::start(&local, &project.dir, &name);
    let mut browser = Firefox::start_with(&Options::default());
    browser.narrow(NARROW);
    browser.navigate(&format!(
        "http://{name}.localhost:{}/_vm/test-user",
        dev.port
    ));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    assert_narrow(&mut browser, "the local run's test-User page");
    drop(browser);
    dev.stop("INT").expect_code(0);
}
