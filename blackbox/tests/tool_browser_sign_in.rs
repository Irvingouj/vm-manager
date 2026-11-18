//! The tool signs in through the browser (CONTRACT.md, "The tool signs in
//! through the browser, and OIDC is connected from the tool: Signing the
//! tool in", and "The tool: Signing in" and "The credential" for what
//! stands): the browser opened with `BROWSER`, or an address and a check
//! code shown; `/tool`, the page that asks, `POST /tool` and `POST
//! /v1/tool-sign-ins`; what a link sent to someone else gains (nothing);
//! nothing kept before someone signed in at the provider, five awaiting
//! for each User; the tool's `--timeout`; the tool credential's Groups read
//! again at each renewal; signing out and expiring; no secret where it must
//! not be.
//!
//! Acceptance criteria: "Signing the tool in opens the browser, and after
//! sign-in the tool is signed in with nothing copied by hand. It also works
//! on a machine with no browser by showing an address to open elsewhere",
//! "The tool's credential can be revoked, and expires".
//!
//! The harness plays the person: its `BROWSER` command writes the address
//! the tool opens into a file, and the test opens it in its own browser,
//! signed in at the harness's provider. Where the tool's part is played by
//! hand (a verifier, its challenge, `POST /v1/tool-sign-ins`), it is to see
//! what the platform answers someone who is not the tool.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::api::parse_rfc3339;
use blackbox::browser::{Step, query_param, url};
use blackbox::connection::{TOOL_PREFIX, hide, masked, shown};
use blackbox::http::mask_values;
use blackbox::oidc::Outage;
use blackbox::project::new_project;
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace, masked_steps, oidc_options};
use blackbox::tool::{Machine, Run, Running, bearer, windows_in};
use blackbox::tool_sign_ins::{
    BrowserCommand, Challenge, Decision, OPEN_TIMEOUT, check_code_in, check_code_of, machine_name,
    strip_tags, tool_address_in,
};
use blackbox::{DOMAIN, Response, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};
use url::Url;

/// How long a test waits for a run of `vm sign-in` to end once it should.
const END_TIMEOUT: Duration = Duration::from_secs(30);

fn installation() -> Installation {
    Installation::start(&[])
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn query_names(at: &Url) -> BTreeSet<String> {
    at.query_pairs().map(|(n, _)| n.into_owned()).collect()
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn is_base64url(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Waits at most `timeout` for what the run wrote on standard error to
/// hold an address of `/tool` and a check code; returns both, or fails.
fn shown_address_and_code(
    at: &Installation,
    running: &Running,
    timeout: Duration,
) -> (Url, String) {
    let deadline = Instant::now() + timeout;
    loop {
        let stderr = running.stderr_so_far();
        let address = tool_address_in(&stderr, &at.auth_origin());
        let code = check_code_in(&stderr);
        if let (Some(address), Some(code)) = (address, code) {
            return (address, code);
        }
        assert!(
            Instant::now() < deadline,
            "expected `vm sign-in` to write an address of {}/tool and a check code (XXXX-XXXX) on standard error within {timeout:?}; it wrote:\n{}",
            at.auth_origin(),
            masked(&stderr, &[])
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// The address the tool opened with the harness's `BROWSER`, or fails.
fn opened(opener: &BrowserCommand, running: &Running) -> Url {
    let opened = opener.wait_opened(OPEN_TIMEOUT).unwrap_or_else(|| {
        panic!(
            "expected `vm sign-in` to open the browser with BROWSER within {OPEN_TIMEOUT:?}; it wrote:\n{}",
            masked(&running.stderr_so_far(), &[])
        )
    });
    url(&opened)
}

fn port_of(address: &Url) -> u16 {
    query_param(address, "port")
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("expected a port in {address}"))
}

/// The run, waited for and with credentials taken out of it.
fn ended(running: Running) -> Run {
    hide(&running.wait(END_TIMEOUT), &[])
}

/// Fails unless the machine keeps nothing.
fn assert_nothing_kept(machine: &Machine, context: &str) {
    assert!(
        machine.credential_files().is_empty(),
        "{context}: expected no credential kept, found {:?}",
        machine.credential_files()
    );
}

/// The page of step 2 for a sign-in of the tool begun by hand with this
/// challenge and port (or none), walked by this person's browser.
fn page_for(
    at: &Installation,
    sub: &str,
    challenge: &Challenge,
    port: Option<u16>,
) -> (blackbox::browser::Browser, Step) {
    let mut browser = at.browser_as(sub);
    let address = at.tool_address(&challenge.challenge, port, "harness-machine");
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let page = steps.last().unwrap().clone();
    (browser, page)
}

fn a_port() -> u16 {
    blackbox::postgres::free_port().unwrap()
}

/// Fails unless this `POST /tool` was answered as one for a sign-in that is
/// over or not known: `400`, a page with no form.
fn assert_over(at: &Installation, answer: &Step, context: &str) {
    assert_eq!(
        answer.response.status,
        400,
        "{context}: expected 400, a page with no form\n{}",
        shown(&answer.response, &[])
    );
    assert!(
        at.tool_form(answer).is_none(),
        "{context}: the page has no form to answer again\n{}",
        shown(&answer.response, &[])
    );
}

/// Fails unless this `POST /tool` showed the page again: `200` with its
/// form.
fn assert_asked_again(at: &Installation, answer: &Step, context: &str) {
    assert_eq!(
        answer.response.status,
        200,
        "{context}: expected 200, the page again\n{}",
        shown(&answer.response, &[])
    );
    assert!(
        at.tool_form(answer).is_some(),
        "{context}: the page is shown again, with its form\n{}",
        shown(&answer.response, &[])
    );
}

/// The form of the page as a browser would post it, and where to.
fn submission_of(at: &Installation, page: &Step, decision: Decision) -> (String, Url) {
    let (form, action) = at.tool_form(page).unwrap();
    let body = form.submission(decision, None).unwrap();
    (body, action)
}

fn assert_not_confirmed(answer: &Response, context: &str) {
    assert_eq!(
        answer.status,
        409,
        "{context}: expected 409 sign-in-not-confirmed\n{}",
        shown(answer, &[])
    );
    assert_api_error(answer, 409, "sign-in-not-confirmed");
}

#[test]
fn signing_the_tool_in_opens_the_browser_and_ends_signed_in_with_nothing_copied_by_hand() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(&machine, &["--json"], &[(name, value.as_str())]);
    let address = opened(&opener, &running);
    assert_eq!(
        format!(
            "{}://{}:{}",
            address.scheme(),
            address.host_str().unwrap_or_default(),
            address.port_or_known_default().unwrap_or_default()
        ),
        at.auth_origin(),
        "the tool opens the sign-in address: {address}"
    );
    assert_eq!(address.path(), "/tool", "{address}");
    assert_eq!(
        query_names(&address),
        names(&["challenge", "machine", "port"]),
        "{address}"
    );
    let challenge = query_param(&address, "challenge").unwrap();
    assert!(
        challenge.len() == 43 && is_base64url(&challenge),
        "a challenge of 43 base64url characters: {challenge:?}"
    );
    let port = port_of(&address);
    assert!(port >= 1024, "a port of 1024 to 65535: {port}");
    assert_eq!(
        query_param(&address, "machine").as_deref(),
        Some(machine_name().as_str()),
        "the machine's name"
    );
    assert!(
        running.stderr_so_far().contains(address.as_str())
            || running
                .stderr_so_far()
                .contains(&format!("{}/tool?", at.auth_origin())),
        "the tool also writes the address on standard error, for a browser that did not open:\n{}",
        masked(&running.stderr_so_far(), &[])
    );
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let page = steps.last().unwrap();
    let text = strip_tags(&page.response.text());
    for (what, shown_there) in [
        ("the person's name", "Ada Lovelace"),
        ("the person's email", "ada@example.test"),
        ("the person's id", ADA),
        ("the machine", machine_name().as_str()),
    ] {
        assert!(
            text.contains(shown_there),
            "expected the page to say {what} ({shown_there:?}), it says: {text}"
        );
    }
    let confirmed = at.answer_tool_page(&mut browser, page, Decision::Confirm, None);
    at.grant_of(&confirmed, port);
    let at_tool = browser.follow(&confirmed);
    assert_eq!(
        at_tool.response.status,
        200,
        "the tool answers the browser with a page\n{}",
        shown(&at_tool.response, &[])
    );
    assert!(
        at_tool
            .response
            .header("content-type")
            .is_some_and(|c| c.starts_with("text/html")),
        "a page saying it is signed in\n{}",
        shown(&at_tool.response, &[])
    );
    let run = ended(running);
    let printed = run.ok_json();
    assert_eq!(
        keys(&printed),
        [
            "domain",
            "expires_at",
            "groups",
            "platform_administrator",
            "user"
        ],
        "{printed}"
    );
    assert_eq!(printed["user"], ADA, "the credential's User is the sub");
    assert_eq!(printed["groups"], json!(["finance"]));
    assert_eq!(printed["platform_administrator"], false);
    assert_eq!(printed["domain"], DOMAIN);
    let who = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(who["user"], ADA, "{who}");
    let credential = machine.kept_credential(&at.vm);
    assert!(credential.starts_with(TOOL_PREFIX));
    assert!(
        !at_tool.response.text().contains(&credential),
        "the Builder never sees a credential"
    );
}

#[test]
fn the_tool_asks_at_once_when_the_browser_reaches_its_port() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let mut running = at.spawn_sign_in(&machine, &["--json"], &[(name, value.as_str())]);
    let address = opened(&opener, &running);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let confirmed =
        at.answer_tool_page(&mut browser, steps.last().unwrap(), Decision::Confirm, None);
    // Past a poll, so that the next one is two seconds away.
    thread::sleep(Duration::from_millis(300));
    browser.follow(&confirmed);
    let reached = Instant::now();
    while !running.has_ended() && reached.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(20));
    }
    let took = reached.elapsed();
    ended(running).ok_json();
    assert!(
        took < Duration::from_millis(1500),
        "expected the tool to ask at once when the browser reached its port, not at its next poll; it ended {took:?} after"
    );
}

#[test]
fn without_a_browser_the_tool_shows_an_address_to_open_elsewhere_and_a_check_code() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(
        &machine,
        &["--no-browser", "--json"],
        &[(name, value.as_str())],
    );
    let (address, code) = shown_address_and_code(&at, &running, OPEN_TIMEOUT);
    assert_eq!(
        query_names(&address),
        names(&["challenge", "machine"]),
        "no port without a browser: {address}"
    );
    assert_eq!(
        query_param(&address, "machine").as_deref(),
        Some(machine_name().as_str())
    );
    assert_check_code_alphabet(&code);
    let challenge = query_param(&address, "challenge").unwrap();
    assert_eq!(
        check_code_of(&challenge).as_deref(),
        Some(code.as_str()),
        "the check code is made from the challenge as the contract says"
    );
    // On another device: a browser of its own.
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let page = steps.last().unwrap();
    let (form, _) = at.tool_form(page).unwrap();
    assert!(
        form.typed_field().is_some(),
        "without a port the page asks for the check code: {:?}",
        form.fields
    );
    let compact: String = code.chars().filter(|c| *c != '-').collect();
    let html = page.response.text();
    assert!(
        !html.contains(&code) && !html.contains(&compact),
        "the page never shows the check code"
    );
    let confirmed = at.answer_tool_page(&mut browser, page, Decision::Confirm, Some(&code));
    assert_eq!(
        confirmed.response.status,
        200,
        "a page says so, and to go back to the tool\n{}",
        shown(&confirmed.response, &[])
    );
    let run = ended(running);
    let printed = run.ok_json();
    assert_eq!(printed["user"], ADA);
    assert!(
        opener.opened().is_empty(),
        "--no-browser opens no browser, also with BROWSER set: {:?}",
        opener.opened()
    );
    machine.run(&["whoami", "--json"]).ok_json();
}

#[test]
fn with_no_browser_command_and_no_display_the_tool_shows_the_address_and_the_check_code() {
    let at = installation();
    let machine = Machine::new();
    // The machine's environment holds no BROWSER, DISPLAY or
    // WAYLAND_DISPLAY.
    let running = at.spawn_sign_in(&machine, &["--json", "--timeout", "10"], &[]);
    let (address, code) = shown_address_and_code(&at, &running, OPEN_TIMEOUT);
    assert!(
        query_param(&address, "port").is_none(),
        "no port without a browser: {address}"
    );
    // Typed in lower case and without the hyphen: the page takes it.
    let typed: String = code
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let confirmed = at.answer_tool_page(
        &mut browser,
        steps.last().unwrap(),
        Decision::Confirm,
        Some(&typed),
    );
    assert_eq!(
        confirmed.response.status,
        200,
        "{}",
        shown(&confirmed.response, &[])
    );
    ended(running).ok_json();
}

/// Fails unless the check code is `XXXX-XXXX` of digits and capital
/// letters without I, L, O and U.
fn assert_check_code_alphabet(code: &str) {
    let ok = code.len() == 9
        && code.chars().enumerate().all(|(i, c)| {
            if i == 4 {
                c == '-'
            } else {
                (c.is_ascii_digit() || c.is_ascii_uppercase()) && !"ILOU".contains(c)
            }
        });
    assert!(
        ok,
        "expected a check code XXXX-XXXX of digits and capital letters without I, L, O and U, got {code:?}"
    );
}

#[test]
fn the_tool_waits_at_most_its_timeout_then_says_sign_in_not_finished_and_keeps_nothing() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let began = Instant::now();
    let running = at.spawn_sign_in(
        &machine,
        &["--json", "--timeout", "10"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let run = ended(running);
    let took = began.elapsed();
    let message = run.json_error(3, "sign-in-not-finished");
    assert!(
        took >= Duration::from_secs(10) && took < Duration::from_secs(16),
        "expected the tool to wait its 10 seconds and then end, it took {took:?}"
    );
    assert!(
        message.contains(&format!("{}/tool?", at.auth_origin())),
        "expected the message to give the address again ({address}), got {message:?}"
    );
    assert!(
        message.contains("vm sign-in"),
        "expected the message to say to run the command again, got {message:?}"
    );
    assert_nothing_kept(&machine, "after sign-in-not-finished");
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "not-signed-in");
}

#[test]
fn a_timeout_outside_10_to_600_seconds_is_exit_code_2() {
    let at = installation();
    let machine = Machine::new();
    for timeout in ["9", "601", "0", "ten", "-5"] {
        let args = at.sign_in_args(&["--json", "--timeout", timeout]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = machine.run(&refs);
        assert_eq!(run.code, Some(2), "--timeout {timeout}\n{}", run.describe());
    }
    assert_nothing_kept(&machine, "after wrong command lines");
}

#[test]
fn an_installation_without_oidc_refuses_the_sign_in_saying_how_to_connect_it() {
    let vm = Vm::start(&oidc_options());
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let args = [
        "sign-in",
        "--domain",
        DOMAIN,
        "--connect",
        &vm.address().to_string(),
        "--json",
    ]
    .map(str::to_owned);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = machine.run_with_env(&refs, &[(name, value.as_str())]);
    let message = run.json_error(3, "sign-in-refused");
    assert!(
        message.contains("vm admin connect-oidc"),
        "expected the message to say a Platform administrator connects OIDC with `vm admin connect-oidc`, got {message:?}"
    );
    assert!(opener.opened().is_empty(), "no browser opened");
    assert_nothing_kept(&machine, "after sign-in-refused");
}

#[test]
fn stand_in_user_on_an_installation_with_oidc_is_sign_in_refused() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let args = at.sign_in_args(&["--stand-in-user", ADA, "--json"]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = machine.run_with_env(&refs, &[(name, value.as_str())]);
    run.json_error(3, "sign-in-refused");
    assert!(opener.opened().is_empty(), "no browser opened");
    assert_nothing_kept(&machine, "after sign-in-refused");
}

#[test]
fn the_stand_in_without_stand_in_user_is_sign_in_refused_saying_so() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    let args = [
        "sign-in",
        "--domain",
        DOMAIN,
        "--connect",
        &vm.address().to_string(),
        "--json",
    ]
    .map(str::to_owned);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let message = machine.run(&refs).json_error(3, "sign-in-refused");
    assert!(
        message.contains("--stand-in-user"),
        "expected the message to say the stand-in needs --stand-in-user, got {message:?}"
    );
    assert_nothing_kept(&machine, "after sign-in-refused");
}

/// `GET /tool` with this query, in a browser signed in nowhere.
fn get_tool(at: &Installation, query: &str) -> Step {
    let mut browser = at.browser();
    browser.open(&at.auth_url(&format!("/tool?{query}")))
}

#[test]
fn get_tool_takes_only_a_well_formed_challenge_port_and_machine() {
    let at = installation();
    let challenge = Challenge::fresh().challenge;
    let mut not_base64url = challenge.clone();
    not_base64url.replace_range(0..1, "+");
    let long_machine = "m".repeat(65);
    let bad: Vec<(&str, String)> = vec![
        ("no challenge", "machine=box".to_owned()),
        (
            "a challenge of 42 characters",
            format!("challenge={}&machine=box", challenge.get(..42).unwrap()),
        ),
        (
            "a challenge of 44 characters",
            format!("challenge={challenge}A&machine=box"),
        ),
        (
            "a challenge that is not base64url",
            format!(
                "challenge={}&machine=box",
                not_base64url.replace('+', "%2B")
            ),
        ),
        (
            "port 1023",
            format!("challenge={challenge}&port=1023&machine=box"),
        ),
        (
            "port 65536",
            format!("challenge={challenge}&port=65536&machine=box"),
        ),
        (
            "port 0",
            format!("challenge={challenge}&port=0&machine=box"),
        ),
        (
            "a port that is no number",
            format!("challenge={challenge}&port=x&machine=box"),
        ),
        (
            "a machine of 65 bytes",
            format!("challenge={challenge}&machine={long_machine}"),
        ),
        (
            "challenge given twice",
            format!("challenge={challenge}&challenge={challenge}&machine=box"),
        ),
        (
            "port given twice",
            format!("challenge={challenge}&port=1024&port=1024&machine=box"),
        ),
        (
            "machine given twice",
            format!("challenge={challenge}&machine=box&machine=box"),
        ),
    ];
    for (case, query) in &bad {
        let step = get_tool(&at, query);
        assert_eq!(
            step.response.status,
            400,
            "{case}\n{}",
            shown(&step.response, &[])
        );
        assert!(
            step.response
                .header("content-type")
                .is_some_and(|c| c.starts_with("text/html")),
            "{case}: a page of the platform's own\n{}",
            shown(&step.response, &[])
        );
    }
    let machine_64 = "m".repeat(64);
    let good = [
        format!("challenge={challenge}&machine=box"),
        format!("challenge={challenge}&port=1024&machine=box"),
        format!("challenge={challenge}&port=65535&machine=box"),
        format!("challenge={challenge}&machine={machine_64}"),
        format!("challenge={challenge}&machine=bo%07x"),
    ];
    let provider_host = at.provider.address().ip().to_string();
    for query in &good {
        let step = get_tool(&at, query);
        assert!(
            step.is_redirect()
                && step.location().host_str() == Some(provider_host.as_str())
                && step.location().path() == "/authorize",
            "{query}: expected 302 to the identity provider\n{}",
            shown(&step.response, &[])
        );
    }
}

#[test]
fn control_characters_are_dropped_from_the_machine_s_name() {
    let at = installation();
    let challenge = Challenge::fresh();
    let mut browser = at.browser_as(ADA);
    let address = at.auth_url(&format!(
        "/tool?challenge={}&machine=build%07box%1b%5B31m",
        challenge.challenge
    ));
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let html = steps.last().unwrap().response.text();
    assert!(
        html.contains("buildbox[31m") || strip_tags(&html).contains("buildbox[31m"),
        "expected the machine's name without its control characters on the page, got {html}"
    );
    assert!(
        !html.contains('\u{7}') && !html.contains('\u{1b}'),
        "no control character reaches the page"
    );
}

#[test]
fn get_tool_is_an_authorization_of_its_own_also_with_a_platform_session() {
    let at = Installation::start(&[("honest", "honest.js")]);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let exchanged = at.provider.grants("authorization_code").len();
    let challenge = Challenge::fresh();
    let port = a_port();
    let first = browser.open(&at.tool_address(&challenge.challenge, Some(port), "box"));
    assert!(
        first.is_redirect()
            && first.location().host_str() == Some(at.provider.address().ip().to_string().as_str()),
        "expected /tool to go to the provider also for a browser with a platform session\n{}",
        shown(&first.response, &[])
    );
    let mut steps = vec![first];
    while steps.last().unwrap().is_redirect() && steps.len() < 5 {
        let next = browser.follow(steps.last().unwrap());
        steps.push(next);
    }
    let page = steps.last().unwrap();
    let confirmed = at.answer_tool_page(&mut browser, page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    let answer = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
    assert_eq!(answer.status, 201, "{}", shown(&answer, &[]));
    assert_eq!(
        at.provider.grants("authorization_code").len(),
        exchanged + 1,
        "the tool's credential came from a code of its own (and so has its own refresh token)"
    );
    assert_eq!(
        at.user_seen(&mut browser, "honest")["id"],
        ADA,
        "the browser's session is untouched"
    );
}

#[test]
fn post_tool_sign_ins_answers_the_credential_once_the_sign_in_is_confirmed() {
    let at = installation();
    let challenge = Challenge::fresh();
    let port = a_port();
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "before the sign-in is begun",
    );
    let (mut browser, page) = page_for(&at, ADA, &challenge, Some(port));
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "while the page asks",
    );
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    let answer = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
    assert_eq!(answer.status, 201, "{}", shown(&answer, &[]));
    let body = answer.json();
    assert_eq!(
        keys(&body),
        [
            "credential",
            "expires_at",
            "groups",
            "platform_administrator",
            "user"
        ],
        "the answer of POST /v1/tool-credentials"
    );
    assert_eq!(body["user"], ADA);
    assert_eq!(body["groups"], json!(["finance"]));
    let credential = body["credential"].as_str().unwrap();
    assert!(credential.starts_with(TOOL_PREFIX));
    assert_eq!(bearer(&at.vm, "/v1/me", credential).send().status, 200);
    let again = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
    assert!(!again.text().contains(TOOL_PREFIX));
    assert_not_confirmed(&again, "the same verifier asked again after its 201");
    // The page answered already: posting it again is a sign-in that is over.
    let posted_again = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    assert_over(&at, &posted_again, "the page posted again once answered");
}

#[test]
fn post_tool_sign_ins_with_a_body_that_is_not_such_json_is_malformed() {
    let at = installation();
    let verifier = Challenge::fresh().verifier().to_owned();
    let cases: Vec<(&str, Option<Value>, &str)> = vec![
        ("not JSON", None, "{\"verifier\": "),
        ("a JSON array", Some(json!([verifier])), ""),
        ("no verifier", Some(json!({ "grant": "g" })), ""),
        (
            "a verifier that is a number",
            Some(json!({ "verifier": 7 })),
            "",
        ),
        (
            "a grant that is a number",
            Some(json!({ "verifier": verifier, "grant": 7 })),
            "",
        ),
    ];
    for (case, value, raw) in &cases {
        let request = at.vm.api_request("/v1/tool-sign-ins").method("POST");
        let request = match value {
            Some(value) => request.json(value),
            None => request.header("content-type", "application/json").body(raw),
        };
        let answer = request.send();
        assert_eq!(answer.status, 400, "{case}\n{}", shown(&answer, &[]));
        assert_api_error(&answer, 400, "malformed");
    }
}

#[test]
fn a_verifier_nothing_is_known_of_is_409_however_many_are_tried() {
    let at = installation();
    for i in 0..300 {
        let answer = at.post_tool_sign_in(Challenge::fresh().verifier(), None);
        assert_not_confirmed(&answer, &format!("unknown verifier {i}"));
    }
    let with_grant = at.post_tool_sign_in(Challenge::fresh().verifier(), Some("made-up-grant"));
    assert_not_confirmed(&with_grant, "an unknown verifier with a made-up grant");
}

#[test]
fn declining_signs_nothing_in() {
    let at = installation();
    let challenge = Challenge::fresh();
    let port = a_port();
    let (mut browser, page) = page_for(&at, ADA, &challenge, Some(port));
    let declined = at.answer_tool_page(&mut browser, &page, Decision::Decline, None);
    assert_eq!(
        declined.response.status,
        200,
        "a page says nothing was signed in (and the browser is sent nowhere)\n{}",
        shown(&declined.response, &[])
    );
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "after declining",
    );
    // Declined, the sign-in is over: confirming it afterwards is 400.
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    assert_over(&at, &confirmed, "a confirm after declining");
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "after a confirm that came after declining",
    );
}

#[test]
fn declined_in_the_browser_the_tool_ends_at_its_timeout_with_nothing_kept() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(
        &machine,
        &["--json", "--timeout", "10"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let declined =
        at.answer_tool_page(&mut browser, steps.last().unwrap(), Decision::Decline, None);
    assert!(
        !declined.is_redirect(),
        "{}",
        shown(&declined.response, &[])
    );
    ended(running).json_error(3, "sign-in-not-finished");
    assert_nothing_kept(&machine, "after declining");
}

#[test]
fn with_a_port_the_credential_needs_the_grant_the_browser_was_sent_to_the_tool_with() {
    // A link sent to someone else: they confirm, and the grant goes to
    // their own machine; the sender holds the verifier but not the grant.
    let at = installation();
    let challenge = Challenge::fresh();
    let port = a_port();
    let (mut browser, page) = page_for(&at, GRACE, &challenge, Some(port));
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "the verifier without the grant",
    );
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), Some("not-the-grant")),
        "the verifier with another grant",
    );
    let other = Challenge::fresh();
    assert_not_confirmed(
        &at.post_tool_sign_in(other.verifier(), Some(&grant)),
        "the grant with another verifier",
    );
    let taken = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
    assert_eq!(
        taken.status,
        201,
        "the control: with both it is taken\n{}",
        shown(&taken, &[])
    );
}

#[test]
fn without_a_port_the_sign_in_needs_the_check_code_only_the_tool_shows() {
    // A link of `--no-browser` sent to someone else: they cannot type the
    // code the sender's tool shows.
    let at = installation();
    let machine = Machine::new();
    let running = at.spawn_sign_in(
        &machine,
        &["--no-browser", "--json", "--timeout", "15"],
        &[],
    );
    let (address, code) = shown_address_and_code(&at, &running, OPEN_TIMEOUT);
    let mut wrong = code;
    let first = wrong.remove(0);
    wrong.insert(0, if first == 'A' { 'B' } else { 'A' });
    let mut browser = at.browser_as(GRACE);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let page = steps.last().unwrap();
    for (case, typed) in [("no code", ""), ("a guessed code", wrong.as_str())] {
        let answer = at.answer_tool_page(&mut browser, page, Decision::Confirm, Some(typed));
        assert_asked_again(&at, &answer, case);
    }
    let run = ended(running);
    run.json_error(3, "sign-in-not-finished");
    assert!(!run.stdout.contains(GRACE), "nobody was signed in");
    assert_nothing_kept(&machine, "after a link sent to someone else");
}

#[test]
fn a_wrong_check_code_shows_the_page_again_and_after_the_fifth_the_sign_in_is_over() {
    let at = installation();
    let machine = Machine::new();
    let running = at.spawn_sign_in(
        &machine,
        &["--no-browser", "--json", "--timeout", "15"],
        &[],
    );
    let (address, code) = shown_address_and_code(&at, &running, OPEN_TIMEOUT);
    let mut wrong = code.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == '7' { '8' } else { '7' });
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let mut page = steps.last().unwrap().clone();
    for attempt in 1..=4 {
        let answer = at.answer_tool_page(&mut browser, &page, Decision::Confirm, Some(&wrong));
        assert_asked_again(&at, &answer, &format!("wrong code {attempt}"));
        page = answer;
    }
    let fifth = at.answer_tool_page(&mut browser, &page, Decision::Confirm, Some(&wrong));
    assert_over(&at, &fifth, "the fifth wrong code");
    // The right code, on the page as it was, now signs nothing in.
    let right = at.answer_tool_page(&mut browser, &page, Decision::Confirm, Some(&code));
    assert_over(&at, &right, "the right code after five wrong ones");
    let run = ended(running);
    run.json_error(3, "sign-in-not-finished");
    assert_nothing_kept(&machine, "after five wrong check codes");
}

#[test]
fn post_tool_from_another_origin_is_refused() {
    let at = installation();
    let challenge = Challenge::fresh();
    let port = a_port();
    let (mut browser, page) = page_for(&at, ADA, &challenge, Some(port));
    let (form, action) = at.tool_form(&page).unwrap();
    let body = form.submission(Decision::Confirm, None).unwrap();
    let elsewhere = url(&format!("http://evil.{DOMAIN}:{}/", at.port()));
    let cross = browser.form_from(&elsewhere, &action, &body);
    let answer = browser.send(&action, &cross);
    assert_eq!(
        answer.status,
        403,
        "a form of another origin\n{}",
        shown(&answer, &[])
    );
    let no_origin = browser
        .request("POST", &action)
        .header("sec-fetch-site", "cross-site")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(&body);
    let answer = browser.send(&action, &no_origin);
    assert_eq!(
        answer.status,
        403,
        "no Origin and Sec-Fetch-Site: cross-site\n{}",
        shown(&answer, &[])
    );
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "after the refused posts",
    );
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    assert_eq!(
        at.post_tool_sign_in(challenge.verifier(), Some(&grant))
            .status,
        201,
        "the control: the same form from its own page is taken"
    );
}

#[test]
fn the_confirmation_page_has_referrer_policy_same_origin_so_its_form_is_sent_with_its_origin() {
    let at = installation();
    for port in [Some(a_port()), None] {
        let (_browser, page) = page_for(&at, ADA, &Challenge::fresh(), port);
        assert_eq!(
            page.response
                .header("referrer-policy")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase(),
            "same-origin",
            "the tool's confirmation page (port {port:?}): expected `referrer-policy: same-origin`, as every page of the platform's own from the ticket \"The Dashboard\" on\n{}",
            shown(&page.response, &[])
        );
    }
}

#[test]
fn the_page_s_form_is_sign_in_code_and_two_buttons_named_answer() {
    let at = installation();
    for port in [Some(a_port()), None] {
        let challenge = Challenge::fresh();
        let (mut browser, page) = page_for(&at, ADA, &challenge, port);
        let (form, action) = at.tool_form(&page).unwrap();
        let case = if port.is_some() {
            "with a port"
        } else {
            "without a port"
        };
        let named = |name: &str| -> Vec<&blackbox::tool_sign_ins::Field> {
            form.fields
                .iter()
                .filter(|f| f.name.as_deref() == Some(name))
                .collect()
        };
        let sign_in = named("sign_in");
        assert!(
            sign_in.len() == 1
                && sign_in[0].tag == "input"
                && sign_in[0].kind == "hidden"
                && !sign_in[0].value.is_empty(),
            "{case}: one hidden input `sign_in` with a value: {:?}",
            form.fields
                .iter()
                .map(|f| (&f.tag, &f.kind, &f.name))
                .collect::<Vec<_>>()
        );
        let code = named("code");
        if port.is_some() {
            assert!(code.is_empty(), "{case}: no input `code`");
        } else {
            assert!(
                code.len() == 1 && code[0].tag == "input" && code[0].kind == "text",
                "{case}: a text input `code`: {:?}",
                form.fields
                    .iter()
                    .map(|f| (&f.tag, &f.kind, &f.name))
                    .collect::<Vec<_>>()
            );
        }
        let answers: BTreeSet<String> = named("answer")
            .iter()
            .filter(|f| f.kind == "submit")
            .map(|f| f.value.clone())
            .collect();
        assert_eq!(
            answers,
            names(&["confirm", "decline"]),
            "{case}: two submit buttons named `answer`"
        );
        // A post that is not such a form (no `answer`): 400.
        let sign_in_value = sign_in[0].value.as_str();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("sign_in", sign_in_value)
            .finish();
        let request = browser.submit_form(&page.url, &page.response, &action, &body);
        let answer = browser.send(&action, &request);
        assert_eq!(
            answer.status,
            400,
            "{case}: a post without `answer`\n{}",
            shown(&answer, &[])
        );
        assert_not_confirmed(
            &at.post_tool_sign_in(challenge.verifier(), None),
            &format!("{case}: after a post without `answer`"),
        );
    }
}

#[test]
fn a_sign_in_value_posted_from_another_browser_is_400_and_confirms_nothing() {
    let at = installation();
    let challenge = Challenge::fresh();
    let port = a_port();
    let (mut browser, page) = page_for(&at, ADA, &challenge, Some(port));
    let (body, action) = submission_of(&at, &page, Decision::Confirm);
    // A browser with no cookie of the sign-in address.
    let mut bare = at.browser();
    let request = bare.submit_form(&page.url, &page.response, &action, &body);
    let answer = Step {
        url: action.clone(),
        response: bare.send(&action, &request),
    };
    assert_over(&at, &answer, "from a browser with no cookie");
    // Another person's browser, with a sign-in of the tool of its own.
    let (mut grace_browser, _grace_page) =
        page_for(&at, GRACE, &Challenge::fresh(), Some(a_port()));
    let request = grace_browser.submit_form(&page.url, &page.response, &action, &body);
    let answer = Step {
        url: action.clone(),
        response: grace_browser.send(&action, &request),
    };
    assert_over(&at, &answer, "from another person's browser");
    assert_not_confirmed(
        &at.post_tool_sign_in(challenge.verifier(), None),
        "after the posts from other browsers",
    );
    // The control: from the browser it was made for, it is taken.
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    assert_eq!(
        at.post_tool_sign_in(challenge.verifier(), Some(&grant))
            .status,
        201,
        "the control: the browser the sign-in was made for confirms it"
    );
}

#[test]
fn the_browser_is_never_sent_to_another_host_than_127_0_0_1() {
    let at = installation();
    // Other parameters are not looked at: the way goes on as without them.
    let provider_host = at.provider.address().ip().to_string();
    for extra in [
        "&host=evil.example",
        "&redirect_uri=http%3A%2F%2Fevil.example%2F",
        "&next=http%3A%2F%2Fevil.example%2F",
    ] {
        let challenge = Challenge::fresh();
        let port = a_port();
        let mut browser = at.browser_as(ADA);
        let address = url(&format!(
            "{}{extra}",
            at.tool_address(&challenge.challenge, Some(port), "box")
        ));
        let steps = at.walk_tool_to_page(&mut browser, &address);
        let first = &steps[0];
        assert!(
            first.is_redirect() && first.location().host_str() == Some(provider_host.as_str()),
            "{extra}: expected 302 to the identity provider\n{}",
            masked_steps(&steps)
        );
        let last = steps.last().unwrap();
        let confirmed = at.answer_tool_page(&mut browser, last, Decision::Confirm, None);
        assert!(
            confirmed.is_redirect(),
            "{extra}\n{}",
            shown(&confirmed.response, &[])
        );
        let to = confirmed.location();
        let shown_to = mask_values(to.as_str());
        assert_eq!(to.host_str(), Some("127.0.0.1"), "{extra}: {shown_to}");
        assert_eq!(to.scheme(), "http", "{extra}: {shown_to}");
    }
}

#[test]
fn nothing_is_kept_before_someone_signed_in_at_the_provider() {
    let at = installation();
    let provider_host = at.provider.address().ip().to_string();
    let mut refused = Vec::new();
    for i in 0..500 {
        let challenge = Challenge::fresh();
        let port = if i % 2 == 0 { Some(a_port()) } else { None };
        let mut browser = at.browser();
        let step = browser.open(&at.tool_address(&challenge.challenge, port, "box"));
        let to_provider =
            step.is_redirect() && step.location().host_str() == Some(provider_host.as_str());
        if !to_provider {
            refused.push(format!("{i}: {}", step.response.status));
        }
    }
    assert!(
        refused.is_empty(),
        "expected each of 500 sign-ins of the tool begun and never finished to be sent to the provider; some were not: {refused:?}"
    );
    at.tool_credential_for(ADA);
    at.tool_credential_for(GRACE);
}

#[test]
fn at_most_five_sign_ins_of_the_tool_await_an_answer_for_each_user_and_the_oldest_goes() {
    let at = installation();
    let grace_challenge = Challenge::fresh();
    let grace_port = a_port();
    let (mut grace_browser, grace_page) = page_for(&at, GRACE, &grace_challenge, Some(grace_port));
    let mut awaiting = Vec::new();
    for _ in 0..6 {
        let challenge = Challenge::fresh();
        let port = a_port();
        let (browser, page) = page_for(&at, ADA, &challenge, Some(port));
        awaiting.push((challenge, port, browser, page));
    }
    let mut taken = Vec::new();
    for (i, (challenge, port, browser, page)) in awaiting.iter_mut().enumerate() {
        let confirmed = at.answer_tool_page(browser, page, Decision::Confirm, None);
        if i == 0 {
            assert_over(&at, &confirmed, "the confirm of the oldest of six");
        }
        let to_tool = confirmed.is_redirect()
            && confirmed.location().host_str() == Some("127.0.0.1")
            && confirmed.location().port() == Some(*port);
        let grant = to_tool
            .then(|| query_param(&confirmed.location(), "grant"))
            .flatten();
        let answer = at.post_tool_sign_in(challenge.verifier(), grant.as_deref());
        taken.push((i, answer.status));
    }
    assert_eq!(
        taken.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
        vec![409, 201, 201, 201, 201, 201],
        "expected the oldest of Ada's six to have gone and the five newest to be taken: {taken:?}"
    );
    let confirmed = at.answer_tool_page(&mut grace_browser, &grace_page, Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, grace_port);
    assert_eq!(
        at.post_tool_sign_in(grace_challenge.verifier(), Some(&grant))
            .status,
        201,
        "another User's sign-in awaiting is not Ada's to push out"
    );
}

/// An installation renewing every `seconds`, with Ada and Grace.
fn renewing_every(seconds: u64) -> Installation {
    Installation::start_with(
        &[],
        &[ada(), grace()],
        &["--session-renewal", &seconds.to_string()],
    )
}

fn wait_past(since: Instant, period: Duration) {
    let until = since + period + Duration::from_millis(700);
    thread::sleep(until.saturating_duration_since(Instant::now()));
}

#[test]
fn the_tool_credential_s_groups_are_read_again_at_each_renewal() {
    let at = renewing_every(5);
    let machine = Machine::new();
    let began = Instant::now();
    at.sign_in_tool(&machine, ADA);
    let signed_in = Instant::now();
    at.provider.set_groups(ADA, &["devops", "audit"]);
    let before = machine.run(&["whoami", "--json"]).ok_json();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the test was too slow to look before the renewal period ended ({:?})",
        began.elapsed()
    );
    assert_eq!(
        before["groups"],
        json!(["finance"]),
        "the Groups as last read"
    );
    wait_past(signed_in, Duration::from_secs(5));
    let after = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(after["groups"], json!(["devops", "audit"]), "{after}");
    let credential = machine.kept_credential(&at.vm);
    let me = bearer(&at.vm, "/v1/me", &credential).send();
    assert_eq!(me.json()["groups"], json!(["devops", "audit"]));
    assert!(
        !at.provider.grants("refresh_token").is_empty(),
        "renewed with its refresh token"
    );
}

#[test]
fn a_refusal_of_the_provider_at_a_renewal_ends_the_tool_credential() {
    let at = renewing_every(3);
    let machine = Machine::new();
    at.sign_in_tool(&machine, ADA);
    let signed_in = Instant::now();
    let credential = machine.kept_credential(&at.vm);
    at.provider.disable(ADA);
    wait_past(signed_in, Duration::from_secs(3));
    let answer = bearer(&at.vm, "/v1/me", &credential).send();
    assert_eq!(answer.status, 401, "{}", shown(&answer, &[]));
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "sign-in-expired");
    assert_eq!(
        bearer(&at.vm, "/v1/me", &credential).send().status,
        401,
        "and stays ended"
    );
}

#[test]
fn a_provider_that_does_not_answer_at_a_renewal_is_503_and_the_credential_stays() {
    let at = renewing_every(3);
    let machine = Machine::new();
    at.sign_in_tool(&machine, ADA);
    let signed_in = Instant::now();
    let credential = machine.kept_credential(&at.vm);
    at.provider.set_groups(ADA, &["devops"]);
    at.provider.set_outage(Outage::Close);
    wait_past(signed_in, Duration::from_secs(3));
    let answer = bearer(&at.vm, "/v1/me", &credential).send();
    assert_api_error(&answer, 503, "unavailable");
    machine
        .run(&["whoami", "--json"])
        .json_error(1, "unavailable");
    at.provider.set_outage(Outage::None);
    let after = bearer(&at.vm, "/v1/me", &credential).send();
    assert_eq!(after.status, 200, "{}", shown(&after, &[]));
    assert_eq!(after.json()["groups"], json!(["devops"]));
}

#[test]
fn a_tool_credential_whose_sign_in_brought_no_refresh_token_ends_at_its_first_renewal() {
    let at = renewing_every(3);
    at.provider.issue_refresh_tokens(false);
    let credential = at.tool_credential_for(ADA);
    let signed_in = Instant::now();
    assert_eq!(bearer(&at.vm, "/v1/me", &credential).send().status, 200);
    wait_past(signed_in, Duration::from_secs(3));
    assert_eq!(bearer(&at.vm, "/v1/me", &credential).send().status, 401);
}

#[test]
fn vm_sign_out_ends_the_tool_credential_signed_in_through_the_browser() {
    let at = installation();
    let machine = Machine::new();
    at.sign_in_tool(&machine, ADA);
    let credential = machine.kept_credential(&at.vm);
    let out = machine.run(&["sign-out", "--json"]).ok_json();
    assert_eq!(
        out,
        json!({ "signed_out": true, "ended_at_installation": true })
    );
    assert_eq!(bearer(&at.vm, "/v1/me", &credential).send().status, 401);
    assert_nothing_kept(&machine, "after signing out");
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "not-signed-in");
}

#[test]
fn the_tool_credential_signed_in_through_the_browser_expires() {
    let at = Installation::start_with(&[], &[ada()], &["--tool-credential-lifetime", "6"]);
    let machine = Machine::new();
    let printed = at.sign_in_tool(&machine, ADA);
    let signed_in = Instant::now();
    let expires = printed["expires_at"].as_str().and_then(parse_rfc3339);
    assert!(expires.is_some(), "{printed}");
    let credential = machine.kept_credential(&at.vm);
    wait_past(signed_in, Duration::from_secs(6));
    assert_eq!(bearer(&at.vm, "/v1/me", &credential).send().status, 401);
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "sign-in-expired");
}

#[test]
fn signing_in_again_through_the_browser_replaces_the_credential_and_ends_the_earlier_one() {
    let at = installation();
    let machine = Machine::new();
    at.sign_in_tool(&machine, ADA);
    let first = machine.kept_credential(&at.vm);
    at.sign_in_tool(&machine, ADA);
    let second = machine.kept_credential(&at.vm);
    assert_ne!(first, second);
    assert_eq!(bearer(&at.vm, "/v1/me", &first).send().status, 401);
    assert_eq!(machine.credential_files().len(), 1);
}

#[test]
fn a_seventeenth_tool_credential_ends_the_one_made_longest_ago() {
    let at = installation();
    let credentials: Vec<String> = (0..17).map(|_| at.tool_credential_for(ADA)).collect();
    assert_eq!(
        bearer(&at.vm, "/v1/me", &credentials[0]).send().status,
        401,
        "the oldest ended"
    );
    for (i, credential) in credentials.iter().enumerate().skip(1) {
        assert_eq!(
            bearer(&at.vm, "/v1/me", credential).send().status,
            200,
            "credential {i}"
        );
    }
}

#[test]
fn no_secret_is_in_the_log_any_output_or_on_the_machine_but_the_credential_in_its_file() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(&machine, &[], &[(name, value.as_str())]);
    let address = opened(&opener, &running);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let confirmed =
        at.answer_tool_page(&mut browser, steps.last().unwrap(), Decision::Confirm, None);
    let at_tool = browser.follow(&confirmed);
    let raw = running.wait(END_TIMEOUT);
    let run = hide(&raw, &[]);
    assert_eq!(run.code, Some(0), "{}", run.describe());
    let credential = machine.kept_credential(&at.vm);
    let who = machine.run(&["whoami"]);
    let mut texts = raw.texts();
    texts.extend(who.texts());
    texts.push(("the tool's page".to_owned(), at_tool.response.describe()));
    texts.push((
        "the confirming page".to_owned(),
        confirmed.response.describe(),
    ));
    let found = windows_in(&texts, &credential);
    assert!(
        found.is_empty(),
        "expected the tool credential printed nowhere: {found:?}"
    );
    let holding = machine.files_holding(&credential);
    assert_eq!(
        holding,
        vec![machine.credential_file()],
        "expected the credential in its file only"
    );
    let secrets: Vec<(&str, String)> = vec![
        (
            "the client secret",
            at.provider.client_secret().expose().to_owned(),
        ),
        (
            "the installation's key",
            at.key().unwrap().expose().to_owned(),
        ),
    ];
    for (what, secret) in &secrets {
        assert!(
            windows_in(&texts, secret).is_empty() && machine.files_holding(secret).is_empty(),
            "{what} is printed or kept on the Builder's machine"
        );
    }
    let mut hidden: Vec<String> = at.provider.issued();
    hidden.push(credential);
    hidden.extend(secrets.into_iter().map(|(_, s)| s));
    if let Some(grant) = query_param(&confirmed.location(), "grant") {
        hidden.push(grant);
    }
    at.assert_none_in_log(&hidden, "after the tool signed in through the browser");
}

#[test]
fn the_instruction_file_says_to_run_vm_sign_in_and_what_sign_in_not_finished_means() {
    let machine = Machine::new();
    let dir = machine.root().join("instructed");
    new_project(&machine, "instructed", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        text.contains("vm sign-in"),
        "expected AGENTS.md to name `vm sign-in`\n{text}"
    );
    assert!(
        text.contains("sign-in-not-finished"),
        "expected AGENTS.md to say what `sign-in-not-finished` means\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("browser"),
        "expected AGENTS.md to say `vm sign-in` opens the Builder's browser\n{text}"
    );
}

#[test]
fn the_harness_s_browser_command_writes_the_address_it_is_given_last() {
    // The harness's own check: its `BROWSER` is a command the tool runs
    // with the address as its last argument.
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (_, script) = opener.env();
    let status = std::process::Command::new(&script)
        .args(["--new-window", "http://auth.vm.test:1/tool?challenge=x"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        opener.opened(),
        vec!["http://auth.vm.test:1/tool?challenge=x".to_owned()]
    );
    assert_eq!(
        opener.wait_opened(Duration::ZERO).as_deref(),
        Some("http://auth.vm.test:1/tool?challenge=x")
    );
    assert_eq!(
        opener.wait_opened(Duration::ZERO),
        None,
        "each address is returned once"
    );
    // A second sign-in on the same machine: its own address, not the first.
    let status = std::process::Command::new(&script)
        .arg("http://auth.vm.test:1/tool?challenge=y")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        opener.wait_opened(Duration::ZERO).as_deref(),
        Some("http://auth.vm.test:1/tool?challenge=y")
    );
    // A command made for a later sign-in never returns what was opened
    // before it.
    let later = BrowserCommand::on(&machine);
    assert_eq!(later.wait_opened(Duration::ZERO), None);
}

#[test]
fn the_harness_s_lingering_browser_command_runs_until_it_is_dropped() {
    let machine = Machine::new();
    let opener = BrowserCommand::lingering(&machine);
    let (_, script) = opener.env();
    let mut child = std::process::Command::new(&script)
        .arg("http://auth.vm.test:1/tool?challenge=z")
        .spawn()
        .unwrap();
    assert_eq!(
        opener.wait_opened(Duration::from_secs(5)).as_deref(),
        Some("http://auth.vm.test:1/tool?challenge=z")
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while opener.running().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        opener.running(),
        vec![child.id()],
        "it runs, by its own pid"
    );
    assert!(child.try_wait().unwrap().is_none(), "it has not ended");
    drop(opener);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ended = None;
    while ended.is_none() && Instant::now() < deadline {
        ended = child.try_wait().unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    if ended.is_none() {
        drop(child.kill());
        drop(child.wait());
        panic!("dropping the command ends it");
    }
}

#[test]
fn the_harness_makes_the_check_code_from_the_challenge() {
    let challenge = Challenge::fresh().challenge;
    let code = check_code_of(&challenge).unwrap();
    assert_check_code_alphabet(&code);
    assert_eq!(check_code_of(&challenge), Some(code), "the same each time");
    assert_eq!(check_code_of("not-a-challenge"), None);
}

#[test]
fn the_tool_does_not_wait_for_the_browser_command_to_end() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::lingering(&machine);
    let (name, value) = opener.env();
    let mut running = at.spawn_sign_in(
        &machine,
        &["--json", "--timeout", "30"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let port = port_of(&address);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let confirmed =
        at.answer_tool_page(&mut browser, steps.last().unwrap(), Decision::Confirm, None);
    let confirmed_at = Instant::now();
    at.grant_of(&confirmed, port);
    let at_tool = browser.follow(&confirmed);
    let answered = confirmed_at.elapsed();
    assert_eq!(
        at_tool.response.status,
        200,
        "the tool answers the browser at its port\n{}",
        shown(&at_tool.response, &[])
    );
    assert!(
        answered < Duration::from_secs(2),
        "expected the tool's port to answer the browser within 2 s of the confirm while the BROWSER command runs on; it took {answered:?}"
    );
    while !running.has_ended() && confirmed_at.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(20));
    }
    let took = confirmed_at.elapsed();
    let ended_in_time = running.has_ended();
    let run = ended(running);
    assert!(
        ended_in_time,
        "expected the tool to end within 5 s of the confirm while the BROWSER command runs on; it had not after {took:?}\n{}",
        run.describe()
    );
    let printed = run.ok_json();
    assert_eq!(printed["user"], ADA, "{printed}");
    assert!(
        !run.stderr.contains("has not ended"),
        "the tool does not wait for the command, nor say it has not ended:\n{}",
        run.stderr
    );
    assert!(
        !opener.running().is_empty(),
        "a command still running when the sign-in ends is left running"
    );
}

#[test]
fn with_a_port_a_sign_in_confirmed_elsewhere_without_a_port_does_not_sign_the_tool_in() {
    // Grace has seen the address: she opens it without the port and types
    // the check code anyone can make from the challenge.
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(
        &machine,
        &["--json", "--timeout", "12"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let challenge = query_param(&address, "challenge").unwrap();
    let code = check_code_of(&challenge).unwrap();
    let mut browser = at.browser_as(GRACE);
    let elsewhere = at.auth_url(&format!("/tool?challenge={challenge}&machine=elsewhere"));
    let steps = at.walk_tool_to_page(&mut browser, &elsewhere);
    let confirmed = at.answer_tool_page(
        &mut browser,
        steps.last().unwrap(),
        Decision::Confirm,
        Some(&code),
    );
    assert_eq!(
        confirmed.response.status,
        200,
        "{}",
        shown(&confirmed.response, &[])
    );
    assert!(
        at.tool_form(&confirmed).is_none(),
        "the code made from the challenge is taken (not the page again for a wrong code)\n{}",
        shown(&confirmed.response, &[])
    );
    let run = ended(running);
    assert!(
        !run.stdout.contains(GRACE),
        "the tool waiting with a port is not signed in as Grace:\n{}",
        run.describe()
    );
    run.json_error(3, "sign-in-not-finished");
    assert_nothing_kept(
        &machine,
        "after a sign-in confirmed elsewhere without a port",
    );
}

/// What a plain socket gets from the tool's port for this request head:
/// the status and the body. Every wait is bounded.
fn raw_to_tool(port: u16, head: &str) -> (u16, String) {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // The tool may answer before it has read the whole head: what it did
    // not read is no failure of the test.
    drop(stream.write_all(head.as_bytes()));
    let mut answer = Vec::new();
    drop(stream.read_to_end(&mut answer));
    let text = String::from_utf8_lossy(&answer).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// A request head to the tool's port with a `Cookie` of `cookie` bytes.
fn head_with_cookie(target: &str, port: u16, cookie: usize) -> String {
    format!(
        "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: harness\r\nCookie: c={}\r\nConnection: close\r\n\r\n",
        "x".repeat(cookie)
    )
}

/// The tool waiting with a port, confirmed in Ada's browser, which has not
/// gone to the port yet: the run, the port and the grant.
fn confirmed_not_yet_at_the_port(at: &Installation, machine: &Machine) -> (Running, u16, String) {
    let opener = BrowserCommand::on(machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(
        machine,
        &["--json", "--timeout", "30"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let port = port_of(&address);
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &address);
    let confirmed =
        at.answer_tool_page(&mut browser, steps.last().unwrap(), Decision::Confirm, None);
    let grant = at.grant_of(&confirmed, port);
    (running, port, grant)
}

#[test]
fn the_tool_s_port_reads_a_request_head_of_up_to_64_kib() {
    // A browser sends there the cookies other programs on 127.0.0.1 set.
    let at = installation();
    for kib in [12, 60] {
        let machine = Machine::new();
        let (running, port, grant) = confirmed_not_yet_at_the_port(&at, &machine);
        let head = head_with_cookie(&format!("/?grant={grant}"), port, kib * 1024);
        let (status, body) = raw_to_tool(port, &head);
        let run = ended(running);
        assert_eq!(
            status,
            200,
            "a head with a Cookie of {kib} KiB: expected 200 with the page\n{}\n{}",
            masked(&body, &[&grant]),
            run.describe()
        );
        let printed = run.ok_json();
        assert_eq!(printed["user"], ADA, "{kib} KiB: {printed}");
    }
}

#[test]
fn the_tool_s_port_answers_404_to_anything_but_get_with_a_grant_and_goes_on_waiting() {
    let at = installation();
    let machine = Machine::new();
    let (mut running, port, grant) = confirmed_not_yet_at_the_port(&at, &machine);
    for (case, head) in [
        (
            "GET /other",
            format!(
                "GET /other?grant={grant} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            ),
        ),
        (
            "POST /?grant=",
            format!(
                "POST /?grant={grant} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        ),
    ] {
        let (status, body) = raw_to_tool(port, &head);
        assert_eq!(status, 404, "{case}\n{}", masked(&body, &[&grant]));
        thread::sleep(Duration::from_millis(300));
        assert!(
            !running.has_ended(),
            "{case}: the tool goes on waiting\n{}",
            masked(&running.stderr_so_far(), &[&grant])
        );
    }
    let head = head_with_cookie(&format!("/?grant={grant}"), port, 0);
    let (status, body) = raw_to_tool(port, &head);
    assert_eq!(
        status,
        200,
        "the real grant after them still signs in\n{}",
        masked(&body, &[&grant])
    );
    let printed = ended(running).ok_json();
    assert_eq!(printed["user"], ADA, "{printed}");
}

/// A value of 43 base64url characters that no sign-in gave: a made-up
/// grant.
fn made_up_grant() -> String {
    Challenge::fresh().challenge
}

#[test]
fn a_sign_in_begun_without_a_port_is_not_given_to_a_request_that_carries_a_grant() {
    let at = installation();
    let challenge = Challenge::fresh();
    let code = check_code_of(&challenge.challenge).unwrap();
    let (mut browser, page) = page_for(&at, ADA, &challenge, None);
    let confirmed = at.answer_tool_page(&mut browser, &page, Decision::Confirm, Some(&code));
    assert_eq!(
        confirmed.response.status,
        200,
        "{}",
        shown(&confirmed.response, &[])
    );
    assert!(
        at.tool_form(&confirmed).is_none(),
        "the control: the check code is taken and the sign-in confirmed\n{}",
        shown(&confirmed.response, &[])
    );
    let grant = made_up_grant();
    let with_grant = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
    assert_not_confirmed(
        &with_grant,
        "a sign-in begun without a port, asked with a made-up grant",
    );
    assert!(!with_grant.text().contains(TOOL_PREFIX));
    let without = at.post_tool_sign_in(challenge.verifier(), None);
    assert_eq!(
        without.status,
        201,
        "the control: the same verifier without a grant takes it\n{}",
        shown(&without, &[])
    );
    assert_eq!(without.json()["user"], ADA);
}

#[test]
fn a_made_up_grant_at_the_tool_s_port_does_not_take_a_sign_in_confirmed_elsewhere_without_a_port() {
    let at = installation();
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let running = at.spawn_sign_in(
        &machine,
        &["--json", "--timeout", "14"],
        &[(name, value.as_str())],
    );
    let address = opened(&opener, &running);
    let port = port_of(&address);
    let challenge = query_param(&address, "challenge").unwrap();
    let code = check_code_of(&challenge).unwrap();
    let mut browser = at.browser_as(GRACE);
    let elsewhere = at.auth_url(&format!("/tool?challenge={challenge}&machine=elsewhere"));
    let steps = at.walk_tool_to_page(&mut browser, &elsewhere);
    let confirmed = at.answer_tool_page(
        &mut browser,
        steps.last().unwrap(),
        Decision::Confirm,
        Some(&code),
    );
    assert!(
        confirmed.response.status == 200 && at.tool_form(&confirmed).is_none(),
        "the control: Grace's sign-in without a port is confirmed\n{}",
        shown(&confirmed.response, &[])
    );
    // A page in the browser, or anything on the machine, sends the tool's
    // port a grant it made up.
    let grant = made_up_grant();
    let head = head_with_cookie(&format!("/?grant={grant}"), port, 0);
    let (status, body) = raw_to_tool(port, &head);
    assert_ne!(
        status,
        0,
        "the tool's port answers\n{}",
        masked(&body, &[&grant])
    );
    let run = ended(running);
    assert!(
        !run.stdout.contains(GRACE),
        "the tool is not signed in as Grace with a made-up grant (its port answered {status}):\n{}",
        run.describe()
    );
    run.json_error(3, "sign-in-not-finished");
    assert_nothing_kept(&machine, "after a made-up grant at the tool's port");
}
