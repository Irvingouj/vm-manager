//! The tool's sign-in, both ways, with the page walked in a real browser
//! over HTTPS, the tool reaching the installation over HTTPS with
//! `VIBE_CA_FILE` (CONTRACT.md, "The tool reaches an installation over
//! HTTPS"; "A real browser in the gate: What it walks", 6; "The tool
//! signs in through the browser, and OIDC is connected from the tool:
//! Signing the tool in").
//!
//! Acceptance criteria: "In a real browser, as tests of the gate: ... the
//! confirmation page of the tool's sign-in".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::firefox::{self, Firefox};
use blackbox::sessions::{ADA, ada, grace};
use blackbox::tool::{Machine, Running};
use blackbox::tool_sign_ins::{
    BrowserCommand, OPEN_TIMEOUT, check_code_in, check_code_of, tool_address_in,
};
use blackbox::walks::{Site, choose_at_provider, click_to_another_page, on_site};
use url::Url;

fn site() -> Site {
    Site::start(&[ada(), grace()])
}

/// Starts `vibe sign-in` reaching the installation over HTTPS (`--domain`,
/// `--resolve`, `--port` of the front, `VIBE_CA_FILE` naming its
/// authority), with these more arguments and environment variables.
fn spawn_sign_in(site: &Site, machine: &Machine, more: &[&str], env: &[(&str, &str)]) -> Running {
    let mut args = vec!["sign-in".to_owned()];
    args.extend(site.place());
    args.extend(more.iter().map(|a| (*a).to_owned()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ca_name, ca_value) = site.ca_env();
    let mut all: Vec<(&str, &str)> = vec![(ca_name, ca_value.as_str())];
    all.extend_from_slice(env);
    machine.spawn_into_files(&machine.cwd(), &refs, &all)
}

/// `vibe whoami --json` through the front, as the tool reaches it.
fn whoami(site: &Site, machine: &Machine) -> blackbox::tool::Run {
    let (ca_name, ca_value) = site.ca_env();
    machine.run_with_env(&["whoami", "--json"], &[(ca_name, ca_value.as_str())])
}

/// The address and the check code `vibe sign-in --no-browser` wrote on
/// standard error, waited for.
fn shown_address_and_code(site: &Site, running: &Running) -> (Url, String) {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        let stderr = running.stderr_so_far();
        if let (Some(address), Some(code)) = (
            tool_address_in(&stderr, &site.origin("auth")),
            check_code_in(&stderr),
        ) {
            return (address, code);
        }
        assert!(
            Instant::now() < deadline,
            "expected `vibe sign-in --no-browser` to show an address and a check code within {OPEN_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Opens the tool's address in the browser and goes through the provider
/// as Ada, to the page that asks.
fn to_the_page(browser: &mut Firefox, site: &Site, address: &Url) {
    assert_eq!(
        address.scheme(),
        "https",
        "expected the tool to give an https address: {address}"
    );
    browser.navigate(address.as_str());
    choose_at_provider(browser, site.provider(), ADA);
    browser.wait_for_page("the page of the tool's sign-in", |u| {
        on_site(u, site, "auth")
    });
    assert_eq!(
        browser.status(),
        Some(200),
        "expected the page that asks\n{}",
        browser.describe()
    );
}

/// The page's confirming button (`answer` = `confirm`).
fn confirm_button(browser: &mut Firefox) -> blackbox::firefox::Element {
    browser.the("form button[name=\"answer\"][value=\"confirm\"], form input[type=\"submit\"][name=\"answer\"][value=\"confirm\"]")
}

fn wait_end(running: Running) -> blackbox::tool::Run {
    running.wait(Duration::from_secs(60))
}

#[test]
fn without_a_browser_the_check_code_typed_into_the_page_signs_the_tool_in() {
    firefox::required();
    let site = site();
    let machine = Machine::new();
    let running = spawn_sign_in(&site, &machine, &["--no-browser", "--json"], &[]);
    let (address, code) = shown_address_and_code(&site, &running);
    let mut browser = site.browser(&[]);
    to_the_page(&mut browser, &site, &address);
    let input = browser.the("form input[name=\"code\"]");
    browser.type_into(&input, &code);
    let confirm = confirm_button(&mut browser);
    click_to_another_page(&mut browser, &confirm);
    assert_eq!(
        browser.status(),
        Some(200),
        "expected the confirmation taken\n{}",
        browser.describe()
    );
    assert!(
        browser.find("form input[name=\"code\"]").is_none(),
        "expected a page saying it is done, not the form again\n{}",
        browser.describe()
    );
    let run = wait_end(running);
    let printed = run.ok_json();
    assert_eq!(printed["user"], ADA, "{}", run.describe());
    let who = whoami(&site, &machine).ok_json();
    assert_eq!(who["user"], ADA, "expected `vibe whoami` to say Ada: {who}");
}

#[test]
fn a_wrong_check_code_is_refused_on_the_page_in_words_and_the_tool_is_not_signed_in() {
    firefox::required();
    let site = site();
    let machine = Machine::new();
    let running = spawn_sign_in(
        &site,
        &machine,
        &["--no-browser", "--json", "--timeout", "10"],
        &[],
    );
    let (address, code) = shown_address_and_code(&site, &running);
    let challenge = address
        .query_pairs()
        .find(|(n, _)| n == "challenge")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    assert_eq!(check_code_of(&challenge).as_deref(), Some(code.as_str()));
    let wrong = if code == "0000-0000" {
        "1111-1111"
    } else {
        "0000-0000"
    };
    let mut browser = site.browser(&[]);
    to_the_page(&mut browser, &site, &address);
    let input = browser.the("form input[name=\"code\"]");
    browser.type_into(&input, wrong);
    let confirm = confirm_button(&mut browser);
    click_to_another_page(&mut browser, &confirm);
    assert_eq!(
        browser.status(),
        Some(200),
        "expected the page shown again\n{}",
        browser.describe()
    );
    assert!(
        browser.find("form input[name=\"code\"]").is_some(),
        "expected the page again, with its form\n{}",
        browser.describe()
    );
    let said = browser.text().to_lowercase();
    assert!(
        said.contains("code")
            && (said.contains("wrong")
                || said.contains("not the")
                || said.contains("does not match")
                || said.contains("incorrect")),
        "expected the page to say in words that the check code was wrong\n{}",
        browser.describe()
    );
    let run = wait_end(running);
    run.json_error(3, "sign-in-not-finished");
    whoami(&site, &machine).json_error(4, "not-signed-in");
}

#[test]
fn with_a_browser_on_the_machine_the_pages_confirmation_signs_the_tool_in() {
    firefox::required();
    let site = site();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = spawn_sign_in(&site, &machine, &["--json"], &[(name, value.as_str())]);
    let opened = opener
        .wait_opened(OPEN_TIMEOUT)
        .expect("expected `vibe sign-in` to hand an address to BROWSER");
    let address = Url::parse(&opened).unwrap();
    let mut browser = site.browser(&[]);
    to_the_page(&mut browser, &site, &address);
    assert!(
        browser.find("form input[name=\"code\"]").is_none(),
        "with a port the page asks for no check code\n{}",
        browser.describe()
    );
    let confirm = confirm_button(&mut browser);
    click_to_another_page(&mut browser, &confirm);
    let url = browser.url();
    let at_tool = Url::parse(&url).unwrap();
    assert_eq!(
        (at_tool.scheme(), at_tool.path()),
        ("http", "/"),
        "expected the tool's own loopback address in plain HTTP\n{}",
        browser.describe()
    );
    assert_eq!(
        at_tool.host_str(),
        Some("127.0.0.1"),
        "expected the browser sent to the tool on this machine\n{}",
        browser.describe()
    );
    assert_eq!(
        browser.status(),
        Some(200),
        "expected the tool's page saying it is signed in\n{}",
        browser.describe()
    );
    let run = wait_end(running);
    let printed = run.ok_json();
    assert_eq!(printed["user"], ADA, "{}", run.describe());
    let who = whoami(&site, &machine).ok_json();
    assert_eq!(who["user"], ADA, "expected `vibe whoami` to say Ada: {who}");
}
