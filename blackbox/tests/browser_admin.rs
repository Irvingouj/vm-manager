//! The pages of Platform administrators walked in a real browser over
//! HTTPS with OIDC connected to the harness's identity provider
//! (CONTRACT.md, "The Dashboard for Platform administrators: How the pages
//! look, and the browser's walks"):
//!
//! - as a Platform administrator (Ada): the way in, every Application over
//!   two pages, disabling one by its confirmation and enabling it by its
//!   button while a User's own browser (Grace's) sees it stop and serve
//!   again, the Operation log filtered with its form, paged and narrowed,
//!   the installation, the Resources;
//! - as someone who is not one (Grace, a Contributor of the Application):
//!   no way in, every address refused in words, the disable form refused;
//! - the Platform administrator taken out of the Group: the confirmation
//!   held across the renewal is refused;
//! - from another origin: a page of an Application posting a form to the
//!   disable is refused `403`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin_pages::{
    APPLICATIONS, INSTALLATION, OPERATION_LOG, RESOURCES, SITE_RESOURCE, disable_target,
    prepare_site,
};
use blackbox::api::version_body_declaring;
use blackbox::browser::url;
use blackbox::firefox::{self, Firefox};
use blackbox::fixture_bundle;
use blackbox::oidc::Person;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, grace};
use blackbox::tool::bearer;
use blackbox::walks::{
    Site, click_to_another_page, on_site, sign_in_to_dashboard, submit_button_of, wait_for_element,
};
use serde_json::{Value, json};

const APP: &str = "board";
const FIXTURE: &str = "admin-walk.js";

fn ada_admin() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

/// `board`, Grace's, [`FIXTURE`] declaring [`SITE_RESOURCE`] at `read`,
/// shared with everyone. Grace's tool credential.
fn graces_board(site: &Site) -> String {
    let credential = site.credential_for(GRACE);
    let post = |target: &str, body: &Value| {
        bearer(&site.at.vm, target, &credential)
            .method("POST")
            .json(body)
            .send()
    };
    post("/v1/applications", &json!({ "name": APP })).expect_status(201);
    post(
        &format!("/v1/applications/{APP}/versions"),
        &version_body_declaring(&fixture_bundle(FIXTURE), &[(SITE_RESOURCE, "read")]),
    )
    .expect_status(201);
    post(
        &format!("/v1/applications/{APP}/sharing"),
        &json!({ "add": [ { "everyone": true } ] }),
    )
    .expect_status(200);
    credential
}

fn disabled_now(site: &Site, credential: &str) -> Value {
    let answer = bearer(&site.at.vm, &format!("/v1/applications/{APP}"), credential).send();
    answer.expect_status(200);
    answer.json()["disabled"].clone()
}

/// The attribute of each element `css` finds, in order.
fn attributes(browser: &mut Firefox, css: &str, name: &str) -> Vec<String> {
    let found = browser.find_all(css);
    found
        .iter()
        .map(|e| browser.attribute(e, name).unwrap_or_default())
        .collect()
}

fn query_of(browser: &mut Firefox) -> Vec<(String, String)> {
    let address = url(&browser.url());
    address
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Whether Grace's browser is served by `board` (its `/hello`).
fn served(browser: &mut Firefox, site: &Site) -> bool {
    browser.navigate(&site.application(APP, "/hello"));
    browser.status() == Some(200) && browser.text().contains(&format!("Hello, {GRACE}!"))
}

#[test]
fn a_platform_administrator_walks_every_page_of_theirs() {
    firefox::required();
    let site = Site::start(&[ada_admin(), grace()]);
    let prepared = prepare_site(&site, ADA, GRACE);
    let admin = prepared.credential.as_str();
    let grace_credential = graces_board(&site);
    // More than 100 Applications: a second page of every Application.
    for i in 0..100 {
        bearer(&site.at.vm, "/v1/applications", admin)
            .method("POST")
            .json(&json!({ "name": format!("filler-{i:03}") }))
            .send()
            .expect_status(201);
    }
    // More than 100 entries: older ones behind `a[data-vm-older]`.
    for _ in 0..101 {
        drop(
            bearer(
                &site.at.vm,
                &format!("/v1/resources/{SITE_RESOURCE}/structure"),
                admin,
            )
            .send(),
        );
    }
    // Grace's browser: board's query refused (devops holds no Grant), then,
    // a second on, allowed.
    let mut grace_browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut grace_browser, &site, GRACE);
    grace_browser.navigate(&site.application(APP, &format!("/query?resource={SITE_RESOURCE}")));
    assert_eq!(
        grace_browser.status(),
        Some(200),
        "{}",
        grace_browser.describe()
    );
    bearer(
        &site.at.vm,
        &format!("/v1/resources/{SITE_RESOURCE}/grants"),
        admin,
    )
    .method("POST")
    .json(&json!({ "group": "devops", "access": "read" }))
    .send()
    .expect_status(200);
    thread::sleep(Duration::from_millis(1_100));
    grace_browser.navigate(&site.application(APP, &format!("/query?resource={SITE_RESOURCE}")));
    assert_eq!(
        grace_browser.status(),
        Some(200),
        "{}",
        grace_browser.describe()
    );
    assert!(
        served(&mut grace_browser, &site),
        "the harness's own check: board serves Grace"
    );

    // 1. The way in, every Application, an Application never shared with
    // her, the second page.
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    let way_in = wait_for_element(&mut browser, "header a[data-vm-admin]");
    click_to_another_page(&mut browser, &way_in);
    let at = url(&browser.url());
    assert_eq!(
        at.path(),
        APPLICATIONS,
        "the way in leads to every Application"
    );
    let board = browser
        .find(&format!("[data-vm-application=\"{APP}\"]"))
        .unwrap_or_else(|| {
            panic!(
                "expected board, never shared with Ada, listed\n{}",
                browser.describe()
            )
        });
    for attribute in ["data-vm-requests", "data-vm-users"] {
        assert!(
            browser.attribute(&board, attribute).is_some(),
            "board: expected its usage (`{attribute}`)\n{}",
            browser.describe()
        );
    }
    let details = browser.find_within(&board, "details");
    assert_eq!(
        details.len(),
        1,
        "board: one `<details>`\n{}",
        browser.describe()
    );
    let summary = browser.find_within(&details[0], "summary");
    browser.click(&summary[0]);
    let opened = browser.text_of(&details[0]);
    assert!(
        opened.contains(GRACE) && opened.contains(SITE_RESOURCE),
        "board's `<details>` opened: expected its Contributors and Declared access, got {opened:?}"
    );
    let next = wait_for_element(&mut browser, "a[data-vm-next]");
    click_to_another_page(&mut browser, &next);
    let second = attributes(&mut browser, "[data-vm-application]", "data-vm-application");
    assert!(
        !second.is_empty() && !second.contains(&APP.to_owned()),
        "the second page: Applications after the first hundred, got {second:?}"
    );

    // 2. Disable board by its confirmation.
    browser.navigate(&site.dashboard(APPLICATIONS));
    let link = browser.the(&format!(
        "[data-vm-application=\"{APP}\"] a[data-vm-details]"
    ));
    click_to_another_page(&mut browser, &link);
    let disable = wait_for_element(&mut browser, "a[data-vm-disable]");
    click_to_another_page(&mut browser, &disable);
    assert_eq!(url(&browser.url()).path(), disable_target(APP));
    assert!(
        browser.text().contains(APP),
        "the confirmation names it\n{}",
        browser.describe()
    );
    let button = submit_button_of(&mut browser, "form[data-vm-form=\"disable\"]");
    click_to_another_page(&mut browser, &button);
    let notice = wait_for_element(&mut browser, "[data-vm-notice=\"disabled\"]");
    let said = browser.text_of(&notice);
    assert!(
        said.contains(ADA),
        "the notice says by whom (Ada), got {said:?}"
    );
    assert!(
        url(&browser.url()).path() == format!("/applications/{APP}"),
        "on the Application's page"
    );
    assert!(
        !disabled_now(&site, &grace_credential).is_null(),
        "disabled at the API"
    );
    grace_browser.navigate(&site.application(APP, "/hello"));
    assert_eq!(
        grace_browser.status(),
        Some(503),
        "{}",
        grace_browser.describe()
    );
    assert!(
        grace_browser
            .find(&format!("[data-vm-disabled=\"{APP}\"]"))
            .is_some(),
        "Grace's browser is shown the page of a disabled Application\n{}",
        grace_browser.describe()
    );
    browser.navigate(&site.dashboard(APPLICATIONS));
    let marked = browser.the(&format!("[data-vm-application=\"{APP}\"]"));
    assert!(
        browser.attribute(&marked, "data-vm-disabled").is_some()
            && browser.text_of(&marked).contains("Disabled"),
        "the list marks it \"Disabled\"\n{}",
        browser.describe()
    );

    // 3. Enable it by its button.
    browser.navigate(&site.dashboard(&format!("/applications/{APP}")));
    let button = submit_button_of(&mut browser, "form[data-vm-form=\"enable\"]");
    click_to_another_page(&mut browser, &button);
    wait_for_element(&mut browser, "[data-vm-notice=\"enabled\"]");
    assert!(
        disabled_now(&site, &grace_credential).is_null(),
        "enabled at the API"
    );
    assert!(
        served(&mut grace_browser, &site),
        "Grace's browser is served by board again"
    );

    // 4. The Operation log.
    browser.navigate(&site.dashboard(OPERATION_LOG));
    let form = "form[data-vm-form=\"operation-log\"]";
    let field = browser.the(&format!("{form} input[name=\"application\"]"));
    browser.type_into(&field, APP);
    let refused = browser.the(&format!(
        "{form} select[name=\"outcome\"] option[value=\"refused\"]"
    ));
    browser.click(&refused);
    let show = submit_button_of(&mut browser, form);
    click_to_another_page(&mut browser, &show);
    // The address a browser makes by sending the form holds every field,
    // the empty ones too (CONTRACT.md, `10e00ff`).
    let query = query_of(&mut browser);
    let mut names: Vec<&str> = query.iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "application",
            "host",
            "outcome",
            "resource",
            "since",
            "until",
            "user"
        ],
        "the address the form makes holds every field: {query:?}"
    );
    let mut given: Vec<(String, String)> =
        query.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    given.sort();
    assert_eq!(
        given,
        [
            ("application".to_owned(), APP.to_owned()),
            ("outcome".to_owned(), "refused".to_owned())
        ],
        "the filters given, the others empty"
    );
    let filtered = attributes(&mut browser, "[data-vm-entry]", "data-vm-entry");
    let api: Vec<String> = bearer(
        &site.at.vm,
        &format!("/v1/operation-log?application={APP}&outcome=refused&limit=100"),
        admin,
    )
    .send()
    .json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|e| e["id"].to_string())
        .collect();
    assert!(
        !api.is_empty(),
        "the harness's own check: board has a refused entry"
    );
    assert_eq!(
        filtered, api,
        "the entries the API answers to the same filters, newest first"
    );
    let address = browser.url();
    browser.navigate(&address);
    assert_eq!(
        attributes(&mut browser, "[data-vm-entry]", "data-vm-entry"),
        filtered,
        "opened again, the same entries"
    );

    browser.navigate(&site.dashboard(OPERATION_LOG));
    let all = attributes(&mut browser, "[data-vm-entry]", "data-vm-entry");
    assert_eq!(all.len(), 100, "a page of 100 entries");
    let newest_time = {
        let entries = browser.find_all("[data-vm-entry]");
        browser.attribute(&entries[0], "data-vm-time").unwrap()
    };
    let typed: String = newest_time.chars().take(19).collect();
    // The time is set as the field's value by WebDriver's script, then
    // the form sent by its button: keys sent into a `datetime-local` field
    // land in its segments in the order of the browser's locale (send-keys
    // gave `6112-02-02T11:08:32` in headless Firefox), which is the
    // browser's matter, not the page's.
    let since_field = format!("{form} input[name=\"since\"]");
    let set = browser.script(
        "const f = document.querySelector(arguments[0]); f.value = arguments[1]; return f.value;",
        &[json!(since_field), json!(typed)],
    );
    assert_eq!(
        set.as_str(),
        Some(typed.as_str()),
        "the harness's own check: the field holds the time set"
    );
    let show = submit_button_of(&mut browser, form);
    click_to_another_page(&mut browser, &show);
    let narrowed = attributes(&mut browser, "[data-vm-entry]", "data-vm-entry");
    assert!(
        !narrowed.is_empty() && narrowed.len() < all.len(),
        "a time typed into \"From (UTC)\" ({typed}) narrows the entries: {} of {}\n{}",
        narrowed.len(),
        all.len(),
        browser.describe()
    );

    browser.navigate(&site.dashboard(OPERATION_LOG));
    let older = wait_for_element(&mut browser, "a[data-vm-older]");
    click_to_another_page(&mut browser, &older);
    let page_two = attributes(&mut browser, "[data-vm-entry]", "data-vm-entry");
    assert!(
        !page_two.is_empty() && page_two.iter().all(|id| !all.contains(id)),
        "the older entries, none of the first page's: {page_two:?}"
    );
    browser.navigate(&site.dashboard(&format!("{OPERATION_LOG}?application={APP}")));
    let by_user = browser.find_all("a[data-vm-filter=\"user\"]");
    assert!(
        !by_user.is_empty(),
        "an entry's user link\n{}",
        browser.describe()
    );
    click_to_another_page(&mut browser, &by_user[0]);
    let query = query_of(&mut browser);
    assert!(
        query.contains(&("user".to_owned(), GRACE.to_owned()))
            && query.contains(&("application".to_owned(), APP.to_owned())),
        "an entry's user link narrows to that User, the other filters kept: {query:?}"
    );
    browser.navigate(&site.dashboard(&format!("{OPERATION_LOG}?user=nobody-at-all")));
    let empty = wait_for_element(&mut browser, "[data-vm-empty]");
    let words = browser.text_of(&empty);
    assert!(
        words.contains("nobody-at-all"),
        "a filter that matches nothing says so, naming it: {words:?}"
    );

    // 5. The installation.
    browser.navigate(&site.dashboard(INSTALLATION));
    let issuer = wait_for_element(&mut browser, "[data-vm-issuer]");
    assert!(
        browser.text_of(&issuer).contains(site.provider().issuer()),
        "the installation names the provider's issuer\n{}",
        browser.describe()
    );
    let secret = site.provider().client_secret();
    assert!(
        !browser.source().contains(secret.expose()),
        "the installation holds no byte of the client secret (its value not shown)"
    );

    // 6. The Resources.
    browser.navigate(&site.dashboard(RESOURCES));
    let resource = wait_for_element(
        &mut browser,
        &format!("[data-vm-resource=\"{SITE_RESOURCE}\"]"),
    );
    let grants = browser.find_within(&resource, "[data-vm-grant]");
    let groups: Vec<String> = grants
        .iter()
        .map(|g| browser.attribute(g, "data-vm-grant").unwrap_or_default())
        .collect();
    assert!(
        groups.contains(&"finance".to_owned()) && groups.contains(&"devops".to_owned()),
        "the Resource with its Grants: {groups:?}"
    );
    let source = browser.source();
    let connection = prepared.db.connection(&prepared.db.reader);
    for key in ["password", "role", "database"] {
        if let Some(value) = connection[key]
            .as_str()
            .or_else(|| connection["read"][key].as_str())
        {
            assert!(
                !source.contains(value),
                "the Resources hold nothing of its connection ({key}, its value not shown)"
            );
        }
    }
}

#[test]
fn someone_not_a_platform_administrator_is_refused_every_page_and_the_disable() {
    firefox::required();
    let site = Site::start(&[ada_admin(), grace()]);
    let grace_credential = graces_board(&site);
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, GRACE);
    assert!(
        browser.find("a[data-vm-admin]").is_none(),
        "Grace's header has no way in\n{}",
        browser.describe()
    );
    for path in [
        APPLICATIONS.to_owned(),
        format!("/applications/{APP}"),
        disable_target(APP),
        OPERATION_LOG.to_owned(),
        INSTALLATION.to_owned(),
        RESOURCES.to_owned(),
        blackbox::admin_pages::OUTBOUND.to_owned(),
    ] {
        browser.navigate(&site.dashboard(&path));
        if path.starts_with("/admin") {
            assert_eq!(
                browser.status(),
                Some(403),
                "{path}\n{}",
                browser.describe()
            );
            assert!(
                browser.find("[data-vm-error=\"not-an-administrator\"]").is_some()
                    && browser.find("[data-vm-application], [data-vm-entry], [data-vm-resource], [data-vm-issuer]").is_none(),
                "{path}: refused in words with nothing of its contents\n{}",
                browser.describe()
            );
        } else {
            assert!(
                browser
                    .find("a[data-vm-disable], [data-vm-usage]")
                    .is_none(),
                "{path}: Grace's own page shows nothing of a Platform administrator's\n{}",
                browser.describe()
            );
        }
        assert!(
            browser.find("a[data-vm-admin]").is_none(),
            "{path}: no way in"
        );
    }
    // The disable form, sent as a browser sends a form of the Dashboard's,
    // by the harness's own browser.
    let mut own = site.at.browser_as(GRACE);
    let list = url(&site.dashboard("/"));
    let steps = own.navigate(&list);
    let landed = steps.last().unwrap();
    assert_eq!(
        landed.response.status, 200,
        "the harness's own check: Grace on the Dashboard"
    );
    let action = url(&site.dashboard(&disable_target(APP)));
    let request = own.submit_form(&list, &landed.response, &action, "");
    let answer = own.send(&action, &request);
    assert_eq!(answer.status, 403, "{}", answer.describe());
    assert!(
        answer.text().contains("not-an-administrator"),
        "{}",
        answer.describe()
    );
    assert!(
        disabled_now(&site, &grace_credential).is_null(),
        "nothing changed"
    );
    assert!(served(&mut browser, &site), "board still serves");
}

#[test]
fn a_platform_administrator_taken_out_of_the_group_is_refused_the_confirmation_held_across_the_renewal()
 {
    firefox::required();
    let renewal = Duration::from_secs(3);
    let site = Site::start_with(&[ada_admin(), grace()], &["--session-renewal", "3"]);
    let grace_credential = graces_board(&site);
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    browser.navigate(&site.dashboard(&disable_target(APP)));
    let button = submit_button_of(&mut browser, "form[data-vm-form=\"disable\"]");
    let loaded = Instant::now();
    site.provider().set_groups(ADA, &["finance"]);
    let until = loaded + renewal + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
    click_to_another_page(&mut browser, &button);
    assert_eq!(browser.status(), Some(403), "{}", browser.describe());
    assert!(
        browser
            .find("[data-vm-error=\"not-an-administrator\"]")
            .is_some(),
        "the confirmation submitted after the renewal is refused\n{}",
        browser.describe()
    );
    assert!(
        browser.find("a[data-vm-admin]").is_none(),
        "no way in after the renewal"
    );
    assert!(
        disabled_now(&site, &grace_credential).is_null(),
        "board still serves"
    );
    browser.navigate(&site.dashboard("/"));
    assert!(
        browser.find("a[data-vm-admin]").is_none(),
        "the header has no way in\n{}",
        browser.describe()
    );
    browser.navigate(&site.application(APP, "/hello"));
    assert_eq!(
        browser.status(),
        Some(200),
        "board still serves\n{}",
        browser.describe()
    );
}

#[test]
fn a_page_of_an_application_posting_to_the_disable_is_refused_403() {
    firefox::required();
    let site = Site::start(&[ada_admin(), grace()]);
    let grace_credential = graces_board(&site);
    let mut browser = site.browser(&[APP]);
    sign_in_to_dashboard(&mut browser, &site, ADA);
    let action = site.dashboard(&disable_target(APP));
    let mut form_page = url(&site.application(APP, "/form"));
    form_page.query_pairs_mut().append_pair("action", &action);
    browser.navigate(form_page.as_str());
    assert!(
        on_site(&browser.url(), &site, APP),
        "the harness's own check: on board's page with the form\n{}",
        browser.describe()
    );
    let send = browser.the("#send");
    click_to_another_page(&mut browser, &send);
    assert_eq!(browser.status(), Some(403), "{}", browser.describe());
    assert!(
        disabled_now(&site, &grace_credential).is_null(),
        "nothing changed"
    );
    browser.navigate(&site.application(APP, "/hello"));
    assert_eq!(
        browser.status(),
        Some(200),
        "board still serves\n{}",
        browser.describe()
    );
}
