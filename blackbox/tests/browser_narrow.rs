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
    APPLICATIONS, INSTALLATION, OPERATION_LOG, RESOURCES, SECTIONS, SITE_RESOURCE, disable_target,
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

/// Ada, a Platform administrator, signed in at the Dashboard of a site of
/// her own, in a browser held to [`NARROW`].
fn admin_at_320() -> (Site, Firefox) {
    let admin = Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS]);
    let site = Site::start(&[admin, grace()]);
    let mut browser = site.browser(&[]);
    browser.narrow(NARROW);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    (site, browser)
}

/// Each section's name as the contract gives it.
const SECTION_NAMES: [(&str, &str); 4] = [
    ("applications", "Applications"),
    ("operation-log", "Operation log"),
    ("resources", "Resources"),
    ("installation", "Installation"),
];

#[test]
fn at_320_css_pixels_the_four_sections_are_on_one_row_named_and_the_pages_own_shows_its_word() {
    firefox::required();
    let (site, mut browser) = admin_at_320();
    for (section, path) in SECTIONS {
        let at = format!("{path} at {NARROW} CSS pixels");
        browser.navigate(&site.dashboard(path));
        let measured = browser.script(
            "const nav = document.querySelector('nav[data-vm-admin-sections]');
if (!nav) { return null; }
const r = nav.getBoundingClientRect();
const links = Array.from(nav.querySelectorAll('a[data-vm-section]')).map((a) => {
  const b = a.getBoundingClientRect();
  // The rendered width of the text of the link (not of a mark): a word
  // hidden off screen or clipped to a pixel has none to speak of.
  let text = 0;
  const walker = document.createTreeWalker(a, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n.textContent.trim() === '') { continue; }
    const range = document.createRange();
    range.selectNodeContents(n);
    const t = range.getBoundingClientRect();
    let clip = n.parentElement;
    let clipped = false;
    for (let e = clip; e && e !== a.parentElement; e = e.parentElement) {
      const s = getComputedStyle(e);
      if (s.display === 'none' || s.visibility !== 'visible' || (s.clip && s.clip !== 'auto') || s.clipPath !== 'none') { clipped = true; }
      const eb = e.getBoundingClientRect();
      if ((s.overflow === 'hidden' || s.overflowX === 'hidden') && eb.width < 2) { clipped = true; }
    }
    if (!clipped) { text = Math.max(text, t.width); }
  }
  return { section: a.getAttribute('data-vm-section'), top: b.top, bottom: b.bottom, current: a.getAttribute('aria-current'), text };
});
return { height: r.height, links };",
            &[],
        );
        assert!(
            !measured.is_null(),
            "{at}: expected `nav[data-vm-admin-sections]`\n{}",
            browser.describe()
        );
        let height = measured["height"].as_f64().unwrap_or(f64::MAX);
        assert!(
            height <= 64.0,
            "{at}: the sections' `nav` is {height} CSS pixels high, at most 64"
        );
        let links = measured["links"].as_array().cloned().unwrap_or_default();
        assert_eq!(links.len(), 4, "{at}: four links: {links:?}");
        let tops: Vec<f64> = links
            .iter()
            .map(|l| l["top"].as_f64().unwrap_or(0.0))
            .collect();
        let bottoms: Vec<f64> = links
            .iter()
            .map(|l| l["bottom"].as_f64().unwrap_or(0.0))
            .collect();
        let lowest_top = tops.iter().copied().fold(f64::MIN, f64::max);
        let highest_bottom = bottoms.iter().copied().fold(f64::MAX, f64::min);
        assert!(
            lowest_top < highest_bottom,
            "{at}: the four links on one row (each link's box shares a line with every other): {links:?}"
        );
        for link in &links {
            let this = link["section"].as_str().unwrap_or("?");
            if this == section {
                assert_eq!(
                    link["current"], "page",
                    "{at}: the page's own link is `aria-current=\"page\"`"
                );
                assert!(
                    link["text"].as_f64().unwrap_or(0.0) > 8.0,
                    "{at}: the page's own link shows its word: {link}"
                );
            }
        }
        // Each link's name as a screen reader says it.
        for (name_of, name) in SECTION_NAMES {
            let found = browser.find_all(&format!(
                "nav[data-vm-admin-sections] a[data-vm-section=\"{name_of}\"]"
            ));
            assert_eq!(found.len(), 1, "{at}: one link of the section {name_of}");
            let label = browser.must(
                "WebDriver:GetComputedLabel",
                &serde_json::json!({ "id": found[0].0 }),
            );
            assert_eq!(
                label["value"]
                    .as_str()
                    .or_else(|| label.as_str())
                    .map(str::trim),
                Some(name),
                "{at}: the accessible name of the section {name_of}"
            );
        }
    }
}

#[test]
fn at_320_css_pixels_no_field_of_the_operation_logs_filters_is_wider_than_its_form_or_overlaps_another()
 {
    firefox::required();
    let (site, mut browser) = admin_at_320();
    browser.navigate(&site.dashboard(OPERATION_LOG));
    let measured = browser.script(
        "const form = document.querySelector('form[data-vm-form=\"operation-log\"]');
if (!form) { return null; }
const f = form.getBoundingClientRect();
const boxes = Array.from(form.querySelectorAll('input:not([type=\"hidden\" i]), select, button, textarea')).map((el) => {
  const b = el.getBoundingClientRect();
  return { el: el.getAttribute('name') || el.localName, left: b.left, right: b.right, top: b.top, bottom: b.bottom };
});
return { form: { left: f.left, right: f.right }, boxes };",
        &[],
    );
    assert!(
        !measured.is_null(),
        "expected the filter form\n{}",
        browser.describe()
    );
    let n = |v: &serde_json::Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let form = &measured["form"];
    let boxes = measured["boxes"].as_array().cloned().unwrap_or_default();
    assert!(boxes.len() >= 7, "the six fields and the button: {boxes:?}");
    let mut failures = Vec::new();
    for b in &boxes {
        if n(b, "left") < n(form, "left") - 0.5 || n(b, "right") > n(form, "right") + 0.5 {
            failures.push(format!("{} is wider than its form: {b} in {form}", b["el"]));
        }
    }
    for (i, a) in boxes.iter().enumerate() {
        for b in boxes.iter().skip(i + 1) {
            let x = n(a, "right").min(n(b, "right")) - n(a, "left").max(n(b, "left"));
            let y = n(a, "bottom").min(n(b, "bottom")) - n(a, "top").max(n(b, "top"));
            if x > 0.5 && y > 0.5 {
                failures.push(format!("{} overlaps {}: {a} and {b}", a["el"], b["el"]));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "at {NARROW} CSS pixels, the Operation log's filter form:\n{}",
        failures.join("\n")
    );
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
