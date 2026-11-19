//! The local run's test-User page, walked in a real browser (CONTRACT.md,
//! "A real browser in the gate: What it walks", 7; "The local loop, kept
//! running: Choosing the test User").
//!
//! Acceptance criteria: "In a real browser, as tests of the gate: ... the
//! local run's test-User page of `vm dev`".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::dev::Dev;
use blackbox::firefox::{self, Firefox, Options};
use blackbox::local::{Local, OUTSIDER, dev_json, local_project, write_dev_json};
use blackbox::walks::{click_to_another_page, page_json, submit_button_for_user};
use serde_json::json;

const NAME: &str = "walk-board";
const REVIEWER: &str = "reviewer";

#[test]
fn the_browser_is_shown_the_test_users_chooses_one_by_its_form_and_then_another() {
    firefox::required();
    let local = Local::new();
    let project = local_project(&local.machine, NAME, &[]);
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .expect("expected `users` in a new project's vibe.dev.json")
        .push(json!({ "id": REVIEWER, "displayName": "Rita Reviewer", "groups": [] }));
    write_dev_json(&project.dir, &file);
    let dev = Dev::start(&local, &project.dir, NAME);
    // `<application>.localhost` reaches the loopback address by itself.
    let mut browser = Firefox::start_with(&Options::default());
    let address = format!("http://{NAME}.localhost:{}/", dev.port);
    browser.navigate(&address);
    let url = url::Url::parse(&browser.url()).unwrap();
    assert_eq!(
        (url.host_str(), url.path()),
        (Some(format!("{NAME}.localhost").as_str()), "/_vm/test-user"),
        "expected the browser with no test User chosen shown the page of test Users\n{}",
        browser.describe()
    );
    assert_eq!(browser.status(), Some(200), "{}", browser.describe());
    let text = browser.text();
    for id in [OUTSIDER, REVIEWER] {
        assert!(
            text.contains(id),
            "expected the page to list the test User {id}\n{}",
            browser.describe()
        );
    }

    for chosen in [OUTSIDER, REVIEWER] {
        if chosen != OUTSIDER {
            browser.navigate(&format!(
                "http://{NAME}.localhost:{}/_vm/test-user",
                dev.port
            ));
        }
        let button = submit_button_for_user(&mut browser, chosen);
        click_to_another_page(&mut browser, &button);
        let at = url::Url::parse(&browser.url()).unwrap();
        assert_eq!(
            (at.host_str(), at.path()),
            (Some(format!("{NAME}.localhost").as_str()), "/"),
            "expected the browser sent to the Application's page after choosing {chosen}\n{}",
            browser.describe()
        );
        assert_eq!(
            page_json(&mut browser),
            json!({ "user": chosen }),
            "expected the Application's page for {chosen}\n{}",
            browser.describe()
        );
    }
    drop(browser);
    dev.stop("INT").expect_code(0);
}
