//! How the platform's own pages look, measured in a real browser set to
//! light and again in one set to dark (CONTRACT.md, "How the pages look",
//! and "How it is tested" there).
//!
//! Acceptance criteria (ticket "One theme for every page of the platform's
//! own"): one theme on every page of the platform's own, nothing loaded;
//! light and dark as the browser prefers; every text readable (WCAG 2's
//! contrast); what can be pressed large enough, and its focus seen; nothing
//! moving for a reader who asks for less motion; no page wider than 320
//! CSS pixels, light and dark; the tool's own page on the loopback address
//! dressed too.
//!
//! **One walk, many tests.** The pages are reached once: one installation
//! over HTTPS (Ada with two Applications, one shared with Grace) and one
//! local run (`vibe dev`), then one browser set to light and one set to
//! dark, each visiting the six pages in turn: the Dashboard's list with
//! Applications in it, one Application's page as a Contributor sees it
//! (its sharing form on it), a page of refusal, the sign-out page, the
//! tool's sign-in page, and the local run's test-User page; then, Ada
//! being a Platform administrator (CONTRACT.md, "The Dashboard for
//! Platform administrators: How the pages look, and the browser's walks"),
//! every Application with a disabled one and one not deployed on it, one
//! Application's page as she sees it with its enable form, the page that
//! confirms a disable, the Operation log with entries of three outcomes
//! (allowed, refused, failed: an unfinished entry is not made here) and a
//! `<details>` opened, the Operation log with a malformed filter, the
//! installation with OIDC connected, the Resources with Grants and a
//! Resource that is not usable; a Resource's page with Grants (one of them
//! "not recorded") and its form, the same page with a field error, the page
//! with a notice, and the page that confirms a revoke (CONTRACT.md,
//! "Resources and Grants in full: The Dashboard: a Resource's page and its
//! Grants": the installation is made, then ended and started again, so that
//! a Grant has neither who set it nor when and a Storage Resource's
//! directory is gone, [`blackbox::admin_pages::start_aged`]); and, in a
//! browser of Grace's, the refusal page of someone who is not a Platform
//! administrator. On each it
//! presses Tab through the page, reads the computed styles and boxes
//! ([`blackbox::look::measure`]) and holds the window to 320 CSS pixels.
//! Each promise is then a test of its own reading those measurements (the
//! first test to ask runs the walk; the others wait for it), and each
//! failure says the page, the scheme, the element and the number measured.
//! The policies are read over HTTP with the harness's small browser, and
//! the tool's own page as the tool serves it during a real `vibe sign-in`.
//!
//! That a browser set to dark tells pages so, that the Tab key brings a
//! focus matching `:focus-visible`, and that reduced motion reaches the
//! page are the harness's own tests (`browser_harness.rs`); the walk also
//! checks on every page that the scheme the page sees is the one set.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use blackbox::admin_pages::{
    APPLICATIONS as ADMIN_APPLICATIONS, Aged, INSTALLATION, OLD_GROUP, OLD_RESOURCE, OPERATION_LOG,
    PreparedSite, RESOURCES, SITE_DISABLED, SITE_RESOURCE, changed_target, disable_target,
    grant_form, prepare_site, resource_target, revoke_page_target, start_aged,
};
use blackbox::browser::url;
use blackbox::dashboard::{Page, oidc_post};
use blackbox::dev::Dev;
use blackbox::firefox::{self, Firefox, Options, Scheme};
use blackbox::local::{Local, local_project};
use blackbox::look::{
    Colour, FocusWalk, backdrop, contrast, measure, narrowed, pictures_in, policy_problems,
    walk_focus,
};
use blackbox::oidc::Person;
use blackbox::page::Document;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, grace};
use blackbox::tool::Machine;
use blackbox::tool_sign_ins::{BrowserCommand, Challenge, Decision, OPEN_TIMEOUT};
use blackbox::walks::{
    Site, choose_at_provider, click_to_another_page, names_of, on_site, share_by_form,
    sign_in_to_dashboard, submit_button_of,
};
use serde_json::{Value, json};

/// The width of a phone's window, in CSS pixels.
const NARROW: u32 = 320;

/// The window every other measure is taken in.
const WIDE: (u32, u32) = (1280, 900);

/// Ada's Applications.
const APPLICATIONS: [&str; 2] = ["board", "notes"];

/// The Application of the local run.
const LOCAL_APPLICATION: &str = "lookboard";

/// An address the platform does not know: sharing with it is refused.
const NOBODY: &str = "nobody-the-platform-knows@example.test";

const LIST: &str = "the Dashboard's list with Applications in it";
const DETAILS: &str = "one Application's page as a Contributor sees it";
const REFUSAL: &str = "a page of refusal";
const SIGN_OUT: &str = "the sign-out page";
const TOOL_SIGN_IN: &str = "the tool's sign-in page";
const TEST_USER: &str = "the local run's test-User page";
const ADMIN_LIST: &str = "every Application, a disabled one and one not deployed among them";
const ADMIN_DETAILS: &str =
    "one Application's page as a Platform administrator sees it, with its enable form";
const CONFIRM: &str = "the page that confirms a disable";
const LOG: &str = "the Operation log with entries of each outcome, a <details> opened";
const LOG_MALFORMED: &str = "the Operation log with a malformed filter";
const INSTALLATION_PAGE: &str = "the installation with OIDC connected";
const RESOURCES_PAGE: &str = "the Resources with Grants, one of them not usable";
const RESOURCE_PAGE: &str = "a Resource's page with Grants, one not recorded, and its form";
const RESOURCE_ERROR: &str = "a Resource's page with a field error";
const RESOURCE_NOTICE: &str = "a Resource's page with a notice";
const REVOKE_CONFIRM: &str = "the page that confirms a revoke";
const NOT_ADMIN: &str = "the refusal page of someone who is not a Platform administrator";

/// The pages, in the order each browser visits them.
const PAGES: [&str; 18] = [
    LIST,
    DETAILS,
    REFUSAL,
    ADMIN_LIST,
    ADMIN_DETAILS,
    CONFIRM,
    LOG,
    LOG_MALFORMED,
    INSTALLATION_PAGE,
    RESOURCES_PAGE,
    RESOURCE_PAGE,
    RESOURCE_ERROR,
    RESOURCE_NOTICE,
    REVOKE_CONFIRM,
    SIGN_OUT,
    TOOL_SIGN_IN,
    TEST_USER,
    NOT_ADMIN,
];

/// Ada, in `finance` and a Platform administrator.
fn ada() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

const SCHEMES: [Scheme; 2] = [Scheme::Light, Scheme::Dark];

/// The smallest text anywhere, and of what a person reads to act.
const MIN_TEXT: f64 = 12.0;
const MIN_ACTING_TEXT: f64 = 16.0;

/// One page as one browser saw it.
struct Look {
    scheme: Scheme,
    page: &'static str,
    url: String,
    measured: Value,
    focus: FocusWalk,
    narrow: Result<(), String>,
    /// The page's markup as the browser holds it.
    html: String,
}

impl Look {
    fn at(&self) -> String {
        format!("{} ({}, {})", self.page, self.scheme, self.url)
    }

    fn canvas(&self) -> Colour {
        Colour::of(&self.measured["canvas"]).unwrap_or(match self.scheme {
            Scheme::Light => Colour::WHITE,
            Scheme::Dark => Colour::BLACK,
        })
    }
}

/// Everything the walk saw.
struct Walk {
    looks: Vec<Look>,
    /// What stopped a part of the walk before it was done.
    broken: Vec<String>,
    /// Each page's `content-security-policy`, read over HTTP.
    policies: Vec<(&'static str, Option<String>)>,
    /// The tool's own page: its policy and its HTML.
    tool_page: Result<(Option<String>, String), String>,
}

/// Runs `f`; its panic as an `Err` with the panic's words.
fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "a panic without words".to_owned())
    })
}

/// The walk, run by the first test that asks for it.
fn walk() -> &'static Walk {
    static WALK: OnceLock<Walk> = OnceLock::new();
    WALK.get_or_init(|| {
        let began = Instant::now();
        let walk = run_walk();
        eprintln!(
            "the walk of the pages took {} s ({} pages measured)",
            began.elapsed().as_secs(),
            walk.looks.len()
        );
        walk
    })
}

fn run_walk() -> Walk {
    firefox::required();
    let mut walk = Walk {
        looks: Vec::new(),
        broken: Vec::new(),
        policies: Vec::new(),
        tool_page: Err("not read".to_owned()),
    };
    let (site, _prepared, _aged) = match caught(|| {
        let (site, aged): (Site, Aged) = start_aged(&[ada(), grace()], ADA);
        site.application_of(ADA, APPLICATIONS[0], Some("The incident board"));
        site.application_of(ADA, APPLICATIONS[1], None);
        // Grace signs in once, so that Ada can share with her by email.
        let mut browser = site.at.browser_as(GRACE);
        let landed = browser.navigate(&url(&site.dashboard("/")));
        assert_eq!(
            landed.last().map(|s| s.response.status),
            Some(200),
            "expected Grace on the Dashboard"
        );
        let prepared: PreparedSite = prepare_site(&site, ADA, GRACE);
        (site, prepared, aged)
    }) {
        Ok(site) => site,
        Err(e) => {
            walk.broken.push(format!("starting the installation: {e}"));
            return walk;
        }
    };
    let local = match caught(Local::new) {
        Ok(local) => local,
        Err(e) => {
            walk.broken.push(format!("making the local machine: {e}"));
            return walk;
        }
    };
    let dev = match caught(|| {
        let project = local_project(&local.machine, LOCAL_APPLICATION, &[]);
        let dev = Dev::start(&local, &project.dir, LOCAL_APPLICATION);
        (project, dev)
    }) {
        Ok(started) => started,
        Err(e) => {
            walk.broken.push(format!("starting the local run: {e}"));
            return walk;
        }
    };
    match caught(|| policies(&site, &dev.1)) {
        Ok(policies) => {
            for (page, policy) in &policies {
                eprintln!("the policy of {page}: {policy:?}");
            }
            walk.policies = policies;
        }
        Err(e) => walk
            .broken
            .push(format!("reading the policies over HTTP: {e}")),
    }
    walk.tool_page = caught(|| tool_page(&site));
    for scheme in SCHEMES {
        let mut looks = Vec::new();
        if let Err(e) = caught(|| walk_in(scheme, &site, &dev.1, &mut looks)) {
            walk.broken.push(format!(
                "the walk in a browser set to {scheme} stopped after {} page(s): {e}",
                looks.len()
            ));
        }
        walk.looks.extend(looks);
    }
    let (project, dev) = dev;
    drop(dev.stop("INT"));
    drop(project);
    walk
}

/// Ada's way through the pages in a browser set to `scheme`.
fn walk_in(scheme: Scheme, site: &Site, dev: &Dev<'_>, looks: &mut Vec<Look>) {
    let mut browser = Firefox::start_with(
        &Options::reaching(&names_of(&APPLICATIONS))
            .trusting(site.authority.der().as_ref())
            .in_scheme(scheme)
            .reducing_motion(),
    );
    browser.set_window(WIDE.0, WIDE.1);

    sign_in_to_dashboard(&mut browser, site, ADA);
    expect_on_page(&mut browser, "[data-vm-application=\"board\"]", LIST);
    looks.push(look(&mut browser, scheme, LIST));

    browser.navigate(&site.dashboard("/applications/board"));
    expect_on_page(&mut browser, "form[data-vm-form=\"share\"]", DETAILS);
    expect_on_page(&mut browser, "form[data-vm-form=\"unshare\"]", DETAILS);
    looks.push(look(&mut browser, scheme, DETAILS));

    share_by_form(&mut browser, "email", NOBODY);
    expect_on_page(&mut browser, "[data-vm-error]", REFUSAL);
    looks.push(look(&mut browser, scheme, REFUSAL));

    walk_admin_pages(&mut browser, scheme, site, looks);

    browser.navigate(&site.auth("/sign-out"));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    expect_on_page(&mut browser, "form", SIGN_OUT);
    looks.push(look(&mut browser, scheme, SIGN_OUT));

    browser.navigate(&tool_sign_in_address(site));
    choose_at_provider(&mut browser, site.provider(), ADA);
    browser.wait_for_page(TOOL_SIGN_IN, |u| on_site(u, site, "auth"));
    expect_on_page(&mut browser, "form input[name=\"code\"]", TOOL_SIGN_IN);
    looks.push(look(&mut browser, scheme, TOOL_SIGN_IN));

    browser.navigate(&format!(
        "http://{LOCAL_APPLICATION}.localhost:{}/_vm/test-user",
        dev.port
    ));
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    expect_on_page(&mut browser, "form", TEST_USER);
    looks.push(look(&mut browser, scheme, TEST_USER));
    drop(browser);

    let mut browser = Firefox::start_with(
        &Options::reaching(&names_of(&APPLICATIONS))
            .trusting(site.authority.der().as_ref())
            .in_scheme(scheme)
            .reducing_motion(),
    );
    browser.set_window(WIDE.0, WIDE.1);
    sign_in_to_dashboard(&mut browser, site, GRACE);
    browser.navigate(&site.dashboard(ADMIN_APPLICATIONS));
    expect_on_page(
        &mut browser,
        "[data-vm-error=\"not-an-administrator\"]",
        NOT_ADMIN,
    );
    looks.push(look(&mut browser, scheme, NOT_ADMIN));
}

/// Ada's way through the pages of Platform administrators.
fn walk_admin_pages(browser: &mut Firefox, scheme: Scheme, site: &Site, looks: &mut Vec<Look>) {
    browser.navigate(&site.dashboard(ADMIN_APPLICATIONS));
    expect_on_page(
        browser,
        &format!("[data-vm-application=\"{SITE_DISABLED}\"][data-vm-disabled]"),
        ADMIN_LIST,
    );
    looks.push(look(browser, scheme, ADMIN_LIST));

    browser.navigate(&site.dashboard(&format!("/applications/{SITE_DISABLED}")));
    expect_on_page(browser, "form[data-vm-form=\"enable\"]", ADMIN_DETAILS);
    looks.push(look(browser, scheme, ADMIN_DETAILS));

    browser.navigate(&site.dashboard(&disable_target(APPLICATIONS[0])));
    expect_on_page(browser, "form[data-vm-form=\"disable\"]", CONFIRM);
    looks.push(look(browser, scheme, CONFIRM));

    browser.navigate(&site.dashboard(OPERATION_LOG));
    for outcome in ["allowed", "refused", "failed"] {
        expect_on_page(browser, &format!("[data-vm-outcome=\"{outcome}\"]"), LOG);
    }
    // One `<details>` opened without giving anything the focus: the
    // keyboard walk must begin as on a page just loaded (a click on its
    // summary would leave the browser's starting point of the next Tab
    // there).
    browser.script(
        "document.querySelector('[data-vm-entry] details').open = true; return null;",
        &[],
    );
    expect_on_page(browser, "[data-vm-entry] details[open]", LOG);
    looks.push(look(browser, scheme, LOG));

    browser.navigate(&site.dashboard(&format!("{OPERATION_LOG}?outcome=nonsense")));
    expect_on_page(browser, "[data-vm-field-error=\"outcome\"]", LOG_MALFORMED);
    looks.push(look(browser, scheme, LOG_MALFORMED));

    browser.navigate(&site.dashboard(INSTALLATION));
    expect_on_page(browser, "[data-vm-issuer]", INSTALLATION_PAGE);
    looks.push(look(browser, scheme, INSTALLATION_PAGE));

    browser.navigate(&site.dashboard(RESOURCES));
    expect_on_page(
        browser,
        &format!("[data-vm-resource=\"{SITE_RESOURCE}\"] [data-vm-grant]"),
        RESOURCES_PAGE,
    );
    expect_on_page(
        browser,
        "[data-vm-resource][data-vm-unusable]",
        RESOURCES_PAGE,
    );
    looks.push(look(browser, scheme, RESOURCES_PAGE));

    browser.navigate(&site.dashboard(&resource_target(OLD_RESOURCE)));
    expect_on_page(browser, "form[data-vm-form=\"grant\"]", RESOURCE_PAGE);
    expect_on_page(
        browser,
        &format!("[data-vm-grant=\"{OLD_GROUP}\"]"),
        RESOURCE_PAGE,
    );
    assert!(
        browser.text().to_lowercase().contains("not recorded"),
        "expected {RESOURCE_PAGE}: a Grant that says \"not recorded\"\n{}",
        browser.describe()
    );
    looks.push(look(browser, scheme, RESOURCE_PAGE));

    // A field error: a Group of 257 bytes typed (the script sets the field's
    // value at once), the form sent by its button.
    let group_field = "form[data-vm-form=\"grant\"] input[name=\"group\"]";
    let set = browser.script(
        "const f = document.querySelector(arguments[0]); f.value = arguments[1]; return f.value.length;",
        &[json!(group_field), json!("g".repeat(257))],
    );
    assert_eq!(
        set.as_u64(),
        Some(257),
        "the harness's own check: the field holds the Group typed"
    );
    let button = submit_button_of(browser, "form[data-vm-form=\"grant\"]");
    click_to_another_page(browser, &button);
    expect_on_page(browser, "[data-vm-field-error=\"group\"]", RESOURCE_ERROR);
    looks.push(look(browser, scheme, RESOURCE_ERROR));

    browser.navigate(&site.dashboard(&changed_target(OLD_RESOURCE, "granted", OLD_GROUP)));
    expect_on_page(browser, "[data-vm-notice=\"granted\"]", RESOURCE_NOTICE);
    looks.push(look(browser, scheme, RESOURCE_NOTICE));

    browser.navigate(&site.dashboard(&revoke_page_target(OLD_RESOURCE, OLD_GROUP)));
    expect_on_page(browser, "form[data-vm-form=\"revoke\"]", REVOKE_CONFIRM);
    looks.push(look(browser, scheme, REVOKE_CONFIRM));
}

/// The tool's sign-in page without a port (it asks for the check code), as
/// a tool run with `--no-browser` makes it: a fresh challenge.
fn tool_sign_in_address(site: &Site) -> String {
    let challenge = Challenge::fresh();
    site.auth(&format!(
        "/tool?challenge={}&machine=harness-machine",
        challenge.challenge
    ))
}

fn expect_on_page(browser: &mut Firefox, css: &str, page: &str) {
    assert!(
        browser.find(css).is_some(),
        "expected {page}, with `{css}` on it\n{}",
        browser.describe()
    );
}

/// Measures the page shown: Tab through it, its styles and boxes, then its
/// width at [`NARROW`] (and the window back to [`WIDE`]).
fn look(browser: &mut Firefox, scheme: Scheme, page: &'static str) -> Look {
    // The address without its query: a callback's code is not shown.
    let url = url::Url::parse(&browser.url()).map_or_else(
        |_| "?".to_owned(),
        |mut u| {
            u.set_query(None);
            u.to_string()
        },
    );
    let focus = walk_focus(browser);
    browser.script(
        "if (document.activeElement) { document.activeElement.blur(); } return null;",
        &[],
    );
    let measured = measure(browser);
    let html = browser.source();
    let narrow = narrowed(browser, NARROW);
    browser.set_window(WIDE.0, WIDE.1);
    eprintln!(
        "measured {page} ({scheme}): {} texts, {} fields, {} buttons and fields, {} links, {} forms; Tab reached {} of {} links, buttons and fields",
        items(&measured["texts"]).len(),
        items(&measured["fields"]).len(),
        items(&measured["pressables"]).len(),
        items(&measured["links"]).len(),
        items(&measured["forms"]).len(),
        focus.steps.len(),
        focus.wanted.len()
    );
    Look {
        scheme,
        page,
        url,
        measured,
        focus,
        narrow,
        html,
    }
}

fn policy_of(response: &blackbox::Response) -> Option<String> {
    response
        .header("content-security-policy")
        .map(str::to_owned)
}

/// Each page's `content-security-policy`, read over HTTP with the
/// harness's small browser as Ada (Grace is shared with on the way, so
/// that the Application's page has its unshare form, as in the walk).
fn policies(site: &Site, dev: &Dev<'_>) -> Vec<(&'static str, Option<String>)> {
    let mut found = Vec::new();
    let mut browser = site.at.browser_as(ADA);
    let list = url(&site.dashboard("/"));
    let steps = browser.navigate(&list);
    let landed = steps.last().unwrap();
    assert!(
        landed.response.status == 200 && landed.url == list,
        "expected Ada on the Dashboard's list\n{}",
        blackbox::browser::describe_steps(&steps)
    );
    found.push((LIST, policy_of(&landed.response)));

    let details_url = url(&site.dashboard("/applications/board"));
    let details = Page::read(
        browser.open(&details_url).response,
        details_url.clone(),
        DETAILS,
    );
    let (action, body) = details.share_submission("email", "grace@example.test");
    let shared = oidc_post(&mut browser, &details, &action, &body);
    assert!(
        shared.status < 400,
        "expected the Application shared with Grace\n{}",
        shared.describe()
    );
    let details = Page::read(browser.open(&details_url).response, details_url, DETAILS);
    found.push((DETAILS, policy_of(&details.response)));

    let (action, body) = details.share_submission("email", NOBODY);
    let refused = oidc_post(&mut browser, &details, &action, &body);
    assert!(
        refused.text().contains("data-vm-error"),
        "expected a page of refusal\n{}",
        refused.describe()
    );
    found.push((REFUSAL, policy_of(&refused)));

    for (page, target) in [
        (ADMIN_LIST, ADMIN_APPLICATIONS.to_owned()),
        (ADMIN_DETAILS, format!("/applications/{SITE_DISABLED}")),
        (CONFIRM, disable_target(APPLICATIONS[0])),
        (LOG, OPERATION_LOG.to_owned()),
        (LOG_MALFORMED, format!("{OPERATION_LOG}?outcome=nonsense")),
        (INSTALLATION_PAGE, INSTALLATION.to_owned()),
        (RESOURCES_PAGE, RESOURCES.to_owned()),
        (RESOURCE_PAGE, resource_target(OLD_RESOURCE)),
        (
            RESOURCE_NOTICE,
            changed_target(OLD_RESOURCE, "granted", OLD_GROUP),
        ),
        (REVOKE_CONFIRM, revoke_page_target(OLD_RESOURCE, OLD_GROUP)),
    ] {
        let answer = browser.open(&url(&site.dashboard(&target))).response;
        found.push((page, policy_of(&answer)));
    }
    let resource_url = url(&site.dashboard(&resource_target(OLD_RESOURCE)));
    let resource = Page::read(
        browser.open(&resource_url).response,
        resource_url,
        RESOURCE_PAGE,
    );
    let form = grant_form(&resource);
    let action = resource.action_of(&form);
    let not_taken = oidc_post(
        &mut browser,
        &resource,
        &action,
        &form.body(&[("group", &"g".repeat(257)), ("access", "read")]),
    );
    assert_eq!(
        not_taken.status,
        400,
        "expected a field error\n{}",
        not_taken.describe()
    );
    found.push((RESOURCE_ERROR, policy_of(&not_taken)));
    let mut grace_browser = site.at.browser_as(GRACE);
    let steps = grace_browser.navigate(&url(&site.dashboard(ADMIN_APPLICATIONS)));
    found.push((NOT_ADMIN, policy_of(&steps.last().unwrap().response)));

    let sign_out = browser.open(&url(&site.auth("/sign-out")));
    assert_eq!(
        sign_out.response.status,
        200,
        "{}",
        sign_out.response.describe()
    );
    found.push((SIGN_OUT, policy_of(&sign_out.response)));

    let steps = site
        .at
        .walk_tool_to_page(&mut browser, &url(&tool_sign_in_address(site)));
    found.push((TOOL_SIGN_IN, policy_of(&steps.last().unwrap().response)));

    let test_user = dev.browser().get("/_vm/test-user");
    assert_eq!(test_user.status, 200, "{}", test_user.describe());
    found.push((TEST_USER, policy_of(&test_user)));
    found
}

/// The page the tool itself serves on the loopback address once the
/// sign-in is confirmed, as a real `vibe sign-in` (with a browser on the
/// machine: `BROWSER`) serves it: its policy and its HTML.
fn tool_page(site: &Site) -> (Option<String>, String) {
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (browser_name, browser_value) = opener.env();
    let (ca_name, ca_value) = site.ca_env();
    let mut args = vec!["sign-in".to_owned()];
    args.extend(site.place());
    args.push("--json".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let running = machine.spawn_into_files(
        &machine.cwd(),
        &refs,
        &[
            (ca_name, ca_value.as_str()),
            (browser_name, browser_value.as_str()),
        ],
    );
    let opened = opener
        .wait_opened(OPEN_TIMEOUT)
        .unwrap_or_else(|| panic!("expected `vibe sign-in` to hand an address to BROWSER"));
    let mut browser = site.at.browser_as(ADA);
    let steps = site.at.walk_tool_to_page(&mut browser, &url(&opened));
    let page = steps.last().unwrap();
    let confirmed = site
        .at
        .answer_tool_page(&mut browser, page, Decision::Confirm, None);
    let at_tool = browser.follow(&confirmed);
    assert!(
        at_tool.url.host_str() == Some("127.0.0.1") && at_tool.response.status == 200,
        "expected the tool's own page on the loopback address, got {} {}",
        at_tool.response.status,
        blackbox::http::mask_values(at_tool.url.as_str())
    );
    let run = running.wait(Duration::from_secs(60));
    assert_eq!(
        run.code,
        Some(0),
        "expected `vibe sign-in` to end signed in"
    );
    (policy_of(&at_tool.response), at_tool.response.text())
}

/// The looks, with what is wrong with the walk itself as failures first: a
/// part that stopped, a page not measured, a page that saw another scheme
/// than its browser was set to.
fn looks(failures: &mut Vec<String>) -> &'static [Look] {
    let walk = walk();
    failures.extend(walk.broken.iter().map(|b| format!("the walk: {b}")));
    for scheme in SCHEMES {
        for page in PAGES {
            if !walk
                .looks
                .iter()
                .any(|l| l.scheme == scheme && l.page == page)
            {
                failures.push(format!("{page} ({scheme}): not measured"));
            }
        }
    }
    for l in &walk.looks {
        let dark = l.measured["dark"].as_bool();
        if dark != Some(l.scheme == Scheme::Dark) {
            failures.push(format!(
                "{}: the page saw prefers-color-scheme: dark = {dark:?} in a browser set to {}",
                l.at(),
                l.scheme
            ));
        }
    }
    &walk.looks
}

/// Fails, listing them, when there are failures.
fn verdict(promise: &str, failures: &[String]) {
    if failures.is_empty() {
        return;
    }
    let shown: Vec<&str> = failures.iter().take(60).map(String::as_str).collect();
    let more = failures.len().saturating_sub(shown.len());
    panic!(
        "{promise}: {} failure(s)\n- {}{}",
        failures.len(),
        shown.join("\n- "),
        if more > 0 {
            format!("\n... and {more} more")
        } else {
            String::new()
        }
    );
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Whether a measured number is at least `min` (to a hundredth of a
/// pixel); a number not measured is not.
fn at_least(x: f64, min: f64) -> bool {
    x >= min - 0.01
}

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

#[test]
fn the_walk_measured_each_page_in_a_browser_set_to_light_and_in_one_set_to_dark() {
    let mut failures = Vec::new();
    let looks = looks(&mut failures);
    for l in looks {
        if l.measured["reduced_motion"].as_bool() != Some(true) {
            failures.push(format!(
                "{}: the page did not see prefers-reduced-motion: reduce",
                l.at()
            ));
        }
    }
    verdict("the walk", &failures);
}

#[test]
fn each_page_has_exactly_one_style_element_and_it_is_in_its_head() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let styles = items(&l.measured["styles"]);
        let in_head = styles
            .iter()
            .filter(|s| s["in_head"].as_bool() == Some(true))
            .count();
        if styles.len() != 1 || in_head != 1 {
            failures.push(format!(
                "{}: <style> elements: {} ({} in <head>), expected exactly 1, in <head>",
                l.at(),
                styles.len(),
                in_head
            ));
        }
    }
    verdict("one <style>, in <head>", &failures);
}

/// What a style names that it must not: `url(`, `@import`, `@font-face`.
fn addresses_in(style: &str) -> Vec<&'static str> {
    let lower = style.to_ascii_lowercase();
    ["url(", "@import", "@font-face"]
        .into_iter()
        .filter(|n| lower.contains(n))
        .collect()
}

#[test]
fn the_style_names_no_address_no_url_no_import_no_font_face() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        for style in items(&l.measured["styles"]) {
            let named = addresses_in(style["text"].as_str().unwrap_or_default());
            if !named.is_empty() {
                failures.push(format!("{}: its <style> holds {named:?}", l.at()));
            }
        }
    }
    verdict("the style names no address", &failures);
}

#[test]
fn each_page_has_the_color_scheme_meta_light_dark() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let metas: Vec<String> = items(&l.measured["color_scheme_metas"])
            .iter()
            .map(|m| {
                m.as_str()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        if metas != ["light dark"] {
            failures.push(format!(
                "{}: <meta name=\"color-scheme\"> contents {metas:?}, expected exactly one, \"light dark\"",
                l.at()
            ));
        }
    }
    verdict(
        "<meta name=\"color-scheme\" content=\"light dark\">",
        &failures,
    );
}

#[test]
fn each_page_has_html_lang_en_and_exactly_one_h1() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        if l.measured["lang"].as_str() != Some("en") {
            failures.push(format!(
                "{}: <html lang> is {}, expected \"en\"",
                l.at(),
                l.measured["lang"]
            ));
        }
        if l.measured["h1"].as_u64() != Some(1) {
            failures.push(format!(
                "{}: {} <h1> element(s), expected exactly 1",
                l.at(),
                l.measured["h1"]
            ));
        }
    }
    verdict("<html lang=\"en\"> and one <h1>", &failures);
}

#[test]
fn each_page_s_policy_adds_only_style_src_unsafe_inline_to_default_src_none() {
    let mut failures = Vec::new();
    let walk = walk();
    failures.extend(walk.broken.iter().map(|b| format!("the walk: {b}")));
    for page in PAGES {
        match walk.policies.iter().find(|(p, _)| *p == page) {
            None => failures.push(format!("{page}: its policy was not read")),
            Some((_, policy)) => {
                for problem in policy_problems(policy.as_deref()) {
                    failures.push(format!("{page}: content-security-policy {problem}"));
                }
            }
        }
    }
    verdict(
        "content-security-policy: default-src 'none', style-src 'unsafe-inline', no other source",
        &failures,
    );
}

#[test]
fn the_background_is_lighter_than_the_text_in_light_darker_in_dark_and_never_pure_black_and_white()
{
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let body = &l.measured["body"];
        let (Ok(background), Some(text)) = (
            backdrop(&body["chain"], l.canvas()),
            Colour::of(&body["color"]),
        ) else {
            failures.push(format!(
                "{}: cannot read the body's colours: {body}",
                l.at()
            ));
            continue;
        };
        let text = text.over(background);
        let (bg, fg) = (background.luminance(), text.luminance());
        let right = match l.scheme {
            Scheme::Light => bg > fg,
            Scheme::Dark => bg < fg,
        };
        if !right {
            failures.push(format!(
                "{}: the page's background {background} (luminance {bg:.3}) against the body's text {text} (luminance {fg:.3}): expected the background {} than the text",
                l.at(),
                if l.scheme == Scheme::Light { "lighter" } else { "darker" }
            ));
        }
        let pure = (background.is(Colour::WHITE) && text.is(Colour::BLACK))
            || (background.is(Colour::BLACK) && text.is(Colour::WHITE));
        if pure {
            failures.push(format!(
                "{}: the text {text} on the background {background} is pure black and pure white",
                l.at()
            ));
        }
    }
    verdict("the background against the text, light and dark", &failures);
}

/// Whether text of this size and weight is large (3:1 is enough).
fn large(size: f64, weight: f64) -> bool {
    size >= 24.0 || (size >= 18.66 && weight >= 700.0)
}

#[test]
fn every_text_has_a_contrast_of_4_5_to_1_or_3_to_1_when_large_with_no_picture_behind_it() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let texts = items(&l.measured["texts"]);
        if texts.is_empty() {
            failures.push(format!("{}: no text found to measure", l.at()));
        }
        for t in texts {
            let what = format!(
                "{}: {} {:?}",
                l.at(),
                t["el"].as_str().unwrap_or("?"),
                t["text"].as_str().unwrap_or("")
            );
            for picture in pictures_in(&t["chain"]) {
                failures.push(format!(
                    "{what}: a picture or gradient behind the text: {picture}"
                ));
            }
            let (Ok(background), Some(colour)) =
                (backdrop(&t["chain"], l.canvas()), Colour::of(&t["color"]))
            else {
                failures.push(format!(
                    "{what}: cannot read its colours: {} on {}",
                    t["color"], t["chain"]
                ));
                continue;
            };
            let shown = colour.over(background);
            let ratio = contrast(shown, background);
            let (size, weight) = (number(&t["size"]), number(&t["weight"]));
            let needed = if large(size, weight) { 3.0 } else { 4.5 };
            if ratio < needed {
                failures.push(format!(
                    "{what}: {shown} on {background}: contrast {ratio:.2}:1, needs {needed}:1 (font {size}px, weight {weight})"
                ));
            }
        }
    }
    verdict("every text's contrast", &failures);
}

/// The colour of a field's edge on one side: its border where it has one,
/// else its own background.
fn edges(field: &Value) -> Vec<(String, Colour)> {
    let own = Colour::of(&field["own"]);
    let visible: Vec<(String, Colour)> = items(&field["sides"])
        .iter()
        .zip(["top", "right", "bottom", "left"])
        .filter(|(s, _)| {
            !matches!(s["style"].as_str(), Some("none" | "hidden") | None)
                && number(&s["width"]) > 0.0
        })
        .filter_map(|(s, name)| Some((format!("its {name} border"), Colour::of(&s["color"])?)))
        .collect();
    if visible.is_empty() {
        own.map(|c| vec![("its own background (it has no border)".to_owned(), c)])
            .unwrap_or_default()
    } else {
        visible
    }
}

#[test]
fn the_edge_of_every_field_has_3_to_1_against_the_colour_around_it() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        for field in items(&l.measured["fields"]) {
            let what = format!("{}: {}", l.at(), field["el"].as_str().unwrap_or("?"));
            let Ok(around) = backdrop(&field["around"], l.canvas()) else {
                failures.push(format!("{what}: cannot read the colours around it"));
                continue;
            };
            for (side, colour) in edges(field) {
                let shown = colour.over(around);
                let ratio = contrast(shown, around);
                if ratio < 3.0 {
                    failures.push(format!(
                        "{what}: {side} {shown} against {around} around it: contrast {ratio:.2}:1, needs 3:1"
                    ));
                }
            }
        }
    }
    verdict("the edge of every field", &failures);
}

#[test]
fn no_text_is_under_12_css_pixels_and_what_a_person_reads_to_act_is_16_or_more() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        for t in items(&l.measured["texts"]) {
            let size = number(&t["size"]);
            let acts = t["acts"].as_bool() == Some(true);
            let needed = if acts { MIN_ACTING_TEXT } else { MIN_TEXT };
            if !at_least(size, needed) {
                failures.push(format!(
                    "{}: {} {:?}: font size {size}px, needs {needed}px{}",
                    l.at(),
                    t["el"].as_str().unwrap_or("?"),
                    t["text"].as_str().unwrap_or(""),
                    if acts {
                        " (a label, field, button or refusal)"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    verdict("the size of text", &failures);
}

fn too_small(b: &Value, width: f64, height: f64) -> bool {
    !(at_least(number(&b["width"]), width) && at_least(number(&b["height"]), height))
}

#[test]
fn every_button_field_and_link_not_in_a_sentence_is_at_least_24_by_24_css_pixels() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let links = items(&l.measured["links"])
            .iter()
            .filter(|a| a["in_sentence"].as_bool() != Some(true));
        for b in items(&l.measured["pressables"]).iter().chain(links) {
            if too_small(b, 24.0, 24.0) {
                failures.push(format!(
                    "{}: {} is {:.1} x {:.1} CSS px, needs 24 x 24",
                    l.at(),
                    b["el"].as_str().unwrap_or("?"),
                    number(&b["width"]),
                    number(&b["height"])
                ));
            }
        }
    }
    verdict("24 by 24", &failures);
}

#[test]
fn a_form_s_submit_button_and_text_fields_are_at_least_44_css_pixels_high() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        for form in items(&l.measured["forms"]) {
            for b in items(&form["submits"])
                .iter()
                .chain(items(&form["text_fields"]))
            {
                if !at_least(number(&b["height"]), 44.0) {
                    failures.push(format!(
                        "{}: {} in {}: {:.1} CSS px high, needs 44",
                        l.at(),
                        b["el"].as_str().unwrap_or("?"),
                        form["form"].as_str().unwrap_or("?"),
                        number(&b["height"])
                    ));
                }
            }
        }
    }
    verdict("44 high", &failures);
}

#[test]
fn the_keyboard_reaches_each_link_button_and_field_and_its_focus_outline_is_2_px_with_3_to_1() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        let steps = &l.focus.steps;
        if !l.focus.page_focused {
            failures.push(format!(
                "{}: the harness could not give the page the focus before pressing Tab",
                l.at()
            ));
        }
        if l.focus.wanted.is_empty() {
            failures.push(format!("{}: no link, button or field found", l.at()));
        }
        for (i, wanted) in l.focus.wanted.iter().enumerate() {
            let reached = match wanted["radio"].as_str() {
                Some(group) => steps.iter().any(|s| {
                    s["index"]
                        .as_u64()
                        .and_then(|j| l.focus.wanted.get(usize::try_from(j).ok()?))
                        .is_some_and(|w| w["radio"].as_str() == Some(group))
                }),
                None => steps
                    .iter()
                    .any(|s| s["index"].as_u64() == u64::try_from(i).ok()),
            };
            if !reached {
                failures.push(format!(
                    "{}: {} is never reached by the Tab key",
                    l.at(),
                    wanted["el"].as_str().unwrap_or("?")
                ));
            }
        }
        for s in steps {
            let what = format!(
                "{}: {} focused by Tab",
                l.at(),
                s["el"].as_str().unwrap_or("?")
            );
            if s["focus_visible"].as_bool() != Some(true) {
                failures.push(format!("{what}: does not match :focus-visible"));
            }
            let style = s["style"].as_str().unwrap_or("?");
            if style == "none" {
                failures.push(format!("{what}: outline-style none"));
            }
            let width = number(&s["width"]);
            if !at_least(width, 2.0) {
                failures.push(format!("{what}: outline-width {width}px, needs 2px"));
            }
            let canvas = Colour::of(&s["canvas"]).unwrap_or_else(|| l.canvas());
            match (backdrop(&s["behind"], canvas), Colour::of(&s["color"])) {
                (Ok(behind), Some(colour)) => {
                    let shown = colour.over(behind);
                    let ratio = contrast(shown, behind);
                    if ratio < 3.0 {
                        failures.push(format!(
                            "{what}: outline {shown} ({style}) against the page behind it {behind}: contrast {ratio:.2}:1, needs 3:1"
                        ));
                    }
                }
                _ => failures.push(format!("{what}: cannot read the outline's colours: {s}")),
            }
        }
    }
    verdict("the focus can be seen", &failures);
}

#[test]
fn with_reduced_motion_nothing_has_a_transition_or_an_animation_duration() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        for m in items(&l.measured["moving"]) {
            failures.push(format!(
                "{}: {}: transition-duration {}, animation-duration {}",
                l.at(),
                m["el"].as_str().unwrap_or("?"),
                m["transition"],
                m["animation"]
            ));
        }
    }
    verdict(
        "nothing moves with prefers-reduced-motion: reduce",
        &failures,
    );
}

#[test]
fn no_page_is_wider_than_a_window_320_css_pixels_wide_in_light_or_in_dark() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        if let Err(problem) = &l.narrow {
            failures.push(format!("{}: {problem}", l.at()));
        }
    }
    verdict("no wider than 320", &failures);
}

#[test]
fn the_tool_s_own_page_has_the_policy_one_style_in_its_head_the_color_scheme_lang_en_and_one_h1() {
    let mut failures = Vec::new();
    let walk = walk();
    match &walk.tool_page {
        Err(e) => failures.push(format!("the tool's own page could not be read: {e}")),
        Ok((policy, html)) => {
            let wanted: Vec<(String, Vec<String>)> = vec![
                ("default-src".to_owned(), vec!["'none'".to_owned()]),
                ("style-src".to_owned(), vec!["'unsafe-inline'".to_owned()]),
            ];
            let found = policy.as_deref().map(blackbox::look::directives);
            if found.as_ref() != Some(&wanted) {
                failures.push(format!(
                    "content-security-policy is {policy:?}, expected \"default-src 'none'; style-src 'unsafe-inline'\""
                ));
            }
            let document = Document::parse(html);
            let root = document.root();
            let styles = root.named("style");
            let in_head: usize = root
                .named("head")
                .iter()
                .map(|h| h.named("style").len())
                .sum();
            if styles.len() != 1 || in_head != 1 {
                failures.push(format!(
                    "<style> elements: {} ({in_head} in <head>), expected exactly 1, in <head>",
                    styles.len()
                ));
            }
            for style in &styles {
                let named = addresses_in(&style.text());
                if !named.is_empty() {
                    failures.push(format!("its <style> holds {named:?}"));
                }
            }
            let metas: Vec<String> = root
                .named("meta")
                .iter()
                .filter(|m| {
                    m.attr("name")
                        .is_some_and(|n| n.eq_ignore_ascii_case("color-scheme"))
                })
                .map(|m| {
                    m.attr("content")
                        .unwrap_or_default()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            if metas != ["light dark"] {
                failures.push(format!(
                    "<meta name=\"color-scheme\"> contents {metas:?}, expected exactly one, \"light dark\""
                ));
            }
            let lang = root.named("html").first().and_then(|h| h.attr("lang"));
            if lang != Some("en") {
                failures.push(format!("<html lang> is {lang:?}, expected \"en\""));
            }
            let h1 = root.named("h1").len();
            if h1 != 1 {
                failures.push(format!("{h1} <h1> element(s), expected exactly 1"));
            }
            if !failures.is_empty() {
                failures.push(format!(
                    "the page:\n{}",
                    html.chars().take(2000).collect::<String>()
                ));
            }
        }
    }
    verdict("the tool's own page on the loopback address", &failures);
}

/// What is wrong with each `<svg>` of `html` that is neither hidden from a
/// screen reader (`aria-hidden="true"`) nor an image with a name
/// (`role="img"` and an `aria-label` that is not empty).
fn unnamed_marks(html: &str) -> Vec<String> {
    Document::parse(html)
        .root()
        .named("svg")
        .iter()
        .filter(|svg| {
            let hidden = svg
                .attr("aria-hidden")
                .is_some_and(|h| h.trim().eq_ignore_ascii_case("true"));
            let named = svg
                .attr("role")
                .is_some_and(|r| r.trim().eq_ignore_ascii_case("img"))
                && svg.attr("aria-label").is_some_and(|l| !l.trim().is_empty());
            !hidden && !named
        })
        .map(|svg| format!("an <svg> with {:?}", svg.attributes()))
        .collect()
}

#[test]
fn every_svg_is_hidden_from_a_screen_reader_or_is_an_image_with_a_name() {
    let mut failures = Vec::new();
    for l in looks(&mut failures) {
        if l.html.is_empty() {
            failures.push(format!("{}: its markup was not read", l.at()));
        }
        for problem in unnamed_marks(&l.html) {
            failures.push(format!(
                "{}: {problem}, neither `aria-hidden=\"true\"` nor `role=\"img\"` with an `aria-label`",
                l.at()
            ));
        }
    }
    match &walk().tool_page {
        Ok((_, html)) => {
            for problem in unnamed_marks(html) {
                failures.push(format!(
                    "the tool's own page: {problem}, neither `aria-hidden=\"true\"` nor `role=\"img\"` with an `aria-label`"
                ));
            }
        }
        Err(e) => failures.push(format!("the tool's own page was not read: {e}")),
    }
    verdict(
        "every <svg> hidden beside its words, or an image with a name",
        &failures,
    );
}

#[test]
fn the_arithmetic_of_the_walk_is_wcag_2_s() {
    // No browser: the measuring stick itself.
    let ratio = |a: &str, b: &str| contrast(Colour::parse(a).unwrap(), Colour::parse(b).unwrap());
    assert!((ratio("rgb(0, 0, 0)", "#ffffff") - 21.0).abs() < 0.01);
    assert!(
        (ratio("#777777", "#ffffff") - 4.48).abs() < 0.01,
        "{}",
        ratio("#777777", "#ffffff")
    );
    assert!((ratio("rgb(255 255 255)", "rgb(255, 255, 255)") - 1.0).abs() < 1e-9);
    let half = Colour::parse("rgba(0, 0, 0, 0.5)")
        .unwrap()
        .over(Colour::WHITE);
    assert!(
        (half.r - 127.5).abs() < 0.01 && (half.a - 1.0).abs() < 1e-9,
        "{half}"
    );
    assert_eq!(Colour::parse("rgba(0, 0, 0, 0)").map(|c| c.a), Some(0.0));
    assert_eq!(Colour::parse("transparent").map(|c| c.a), Some(0.0));
    assert_eq!(Colour::parse("oklch(0.5 0.1 200)"), None);
    let painted = serde_json::json!({ "css": "oklch(0.5 0.1 200)", "bytes": [10, 20, 30, 255] });
    assert_eq!(Colour::of(&painted), Some(Colour::rgb(10.0, 20.0, 30.0)));
    let chain = serde_json::json!([
        { "el": "p", "bg": { "css": "rgba(0, 0, 0, 0)" }, "image": "none" },
        { "el": "body", "bg": { "css": "rgb(10, 10, 10)" }, "image": "none" }
    ]);
    assert_eq!(
        backdrop(&chain, Colour::WHITE),
        Ok(Colour::rgb(10.0, 10.0, 10.0))
    );
    assert!(
        large(24.0, 400.0) && large(18.66, 700.0) && !large(18.66, 400.0) && !large(18.0, 700.0)
    );
    assert!(policy_problems(Some("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'")).is_empty());
    assert_eq!(policy_problems(Some("default-src 'none'")).len(), 1);
    assert_eq!(
        policy_problems(Some(
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:"
        ))
        .len(),
        1
    );
    assert_eq!(policy_problems(None).len(), 1);
}
