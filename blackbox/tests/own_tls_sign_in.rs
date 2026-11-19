//! What the platform derives from how it is reached, when the program
//! serves TLS itself: the same as with `--public-scheme https` behind a
//! reverse proxy (CONTRACT.md, "The program serves TLS itself: What is the
//! same as behind a proxy"; ticket "The program serves TLS itself with a
//! supplied wildcard certificate": "The addresses and origins the platform
//! derives are right with its own TLS (the tests of the reverse-proxy
//! case, run again this way)").
//!
//! The installation is [`Site::own_tls`]: the program with OIDC connected
//! to the harness's identity provider (plain HTTP on `127.0.0.1`), its
//! certificate for `api.<domain>` and `*.<domain>` under a test authority
//! in `VIBE_TLS_CERT_FILE`, listening on a port the harness chose and
//! given as `--public-port`. No front: the harness's small browser and the
//! tool speak TLS to the program itself. Here: the sign-in and its
//! redirect address, every redirect of the sign-in, of an Application's
//! first request and of the sign-out, the cookies, the `Origin` a form is
//! compared with, the address the tool's sign-in opens, and the tool's
//! `sign-in`, `deploy` and `logs --follow` over HTTPS with `VIBE_CA_FILE`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::browser::{Step, describe_steps, set_cookies, url};
use blackbox::connection::{Connection, SecretKey, hide};
use blackbox::deploy::{Deployed, commit_all, deploy_with_env, init_repository};
use blackbox::oidc::Provider;
use blackbox::own_tls::{CERT_VAR, KEY_VAR, Pair, body_lines, start_on_a_port};
use blackbox::project::new_project_in_repository;
use blackbox::sessions::{ADA, Installation, ada, grace};
use blackbox::tls::{Authority, Files, text_of};
use blackbox::tool::{Machine, Running};
use blackbox::tool_sign_ins::{BrowserCommand, Decision, OPEN_TIMEOUT};
use blackbox::vm::TlsClient;
use blackbox::walks::Site;
use blackbox::{DOMAIN, VmOptions};
use serde_json::{Value, json};

const APP: &str = "board";

fn site() -> Site {
    Site::own_tls(&[ada(), grace()])
}

/// Fails unless every redirect among `steps` to a name of the domain names
/// `https` and the public port, and the one to the identity provider gives
/// the redirect address `https://auth.<domain>:<port>/callback`.
fn assert_public_redirects(site: &Site, steps: &[Step], context: &str) {
    let suffix = format!(".{DOMAIN}");
    for step in steps.iter().filter(|s| s.is_redirect()) {
        let to = step.location();
        if to.host_str().is_some_and(|h| h.ends_with(&suffix)) {
            assert!(
                to.scheme() == "https" && to.port() == Some(site.port()),
                "{context}: expected every redirect within the installation to name https and the public port {}, one named {}\n{}",
                site.port(),
                blackbox::http::mask_values(to.as_str()),
                describe_steps(steps)
            );
        }
        if let Some((_, redirect)) = to.query_pairs().find(|(n, _)| n == "redirect_uri") {
            assert_eq!(
                redirect,
                format!("https://auth.{DOMAIN}:{}/callback", site.port()),
                "{context}: expected the redirect address given to the provider\n{}",
                describe_steps(steps)
            );
        }
    }
}

/// Fails unless every cookie set on these steps by the installation is a
/// `__Host-` cookie marked `Secure`.
fn assert_host_cookies(steps: &[Step], context: &str) {
    let suffix = format!(".{DOMAIN}");
    let mut seen = 0;
    for step in steps
        .iter()
        .filter(|s| s.url.host_str().is_some_and(|h| h.ends_with(&suffix)))
    {
        for cookie in set_cookies(&step.response) {
            seen += 1;
            assert!(
                cookie.name.starts_with("__Host-") && cookie.has("secure"),
                "{context}: expected a `__Host-` cookie marked Secure from {}, got {} (Secure: {})",
                step.url.host_str().unwrap_or_default(),
                cookie.name,
                cookie.has("secure")
            );
        }
    }
    assert!(seen > 0, "{context}: expected cookies set on the way");
}

#[test]
fn the_installation_gives_its_https_sign_in_address() {
    let site = site();
    let answer = site.at.vm.api_request("/v1/installation").send();
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({
            "domain": DOMAIN,
            "sign_in": "oidc",
            "sign_in_address": format!("https://auth.{DOMAIN}:{}", site.port()),
        }),
        "expected the sign-in address to be the program's own https address"
    );
}

#[test]
fn an_applications_first_request_and_the_sign_in_redirect_over_https_and_set_host_cookies() {
    let site = site();
    site.application_of(ADA, APP, None);
    let at = &site.at;
    let mut browser = at.browser_as(ADA);
    let steps = at.sign_in(&mut browser, APP, "/");
    assert_eq!(
        steps[0].response.status,
        302,
        "expected the Application's first request sent to sign in\n{}",
        describe_steps(&steps)
    );
    let first = steps[0].location();
    assert_eq!(
        (first.scheme(), first.host_str(), first.port(), first.path()),
        (
            "https",
            Some(format!("auth.{DOMAIN}").as_str()),
            Some(site.port()),
            "/sign-in"
        ),
        "expected the first request sent to the https sign-in address\n{}",
        describe_steps(&steps)
    );
    assert!(
        steps.iter().any(|s| s
            .response
            .header("location")
            .is_some_and(|l| l.contains("redirect_uri="))),
        "expected the way to pass the provider with a redirect address\n{}",
        describe_steps(&steps)
    );
    assert_public_redirects(&site, &steps, "the sign-in");
    assert_host_cookies(&steps, "the sign-in");
    let session = browser
        .jar
        .get(&format!("{APP}.{DOMAIN}"), "__Host-vm-session")
        .unwrap_or_else(|| {
            panic!(
                "expected the Application's session cookie; the jar holds {:?}",
                browser.jar.all()
            )
        });
    assert!(session.secure && session.http_only);
    assert_eq!(at.user_seen(&mut browser, APP)["id"], ADA);
}

#[test]
fn the_sign_out_by_its_form_over_https_ends_the_session() {
    let site = site();
    site.application_of(ADA, APP, None);
    let at = &site.at;
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, APP, "/");
    let out = at.sign_out(&mut browser);
    assert_eq!(
        out.response.status,
        200,
        "expected the sign-out by its form (Origin https://auth.{DOMAIN}:{}) taken\n{}",
        site.port(),
        out.response.describe()
    );
    for cookie in set_cookies(&out.response) {
        assert!(
            cookie.name.starts_with("__Host-") && cookie.has("secure"),
            "expected the cleared cookie to be `__Host-` and Secure: {}",
            cookie.name
        );
    }
    at.assert_no_session(&mut browser, APP, "after signing out over https");
    // A page of the Application now sends the browser to the https
    // sign-in address.
    let page = browser.open(&at.app_url(APP, "/"));
    assert_public_redirects(&site, &[page], "after signing out");
}

#[test]
fn a_forms_origin_is_compared_with_the_https_address() {
    let site = site();
    site.application_of(ADA, APP, None);
    let at = &site.at;
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, APP, "/");
    let page = at.app_url(APP, "/");
    let post = |origin: &str| {
        browser
            .request("POST", &page)
            .header("accept", "application/json")
            .header("origin", origin)
            .send()
    };
    let own = post(&format!("https://{APP}.{DOMAIN}:{}", site.port()));
    assert_eq!(
        own.status,
        200,
        "expected a POST from the Application's own https origin taken\n{}",
        own.describe()
    );
    for other in [
        format!("http://{APP}.{DOMAIN}:{}", site.port()),
        format!("https://{APP}.{DOMAIN}"),
        format!("https://dashboard.{DOMAIN}:{}", site.port()),
    ] {
        let refused = post(&other);
        assert_eq!(
            refused.status,
            403,
            "expected a POST from {other} refused\n{}",
            refused.describe()
        );
    }
}

#[test]
fn without_public_port_the_addresses_name_https_and_leave_the_port_out() {
    let authority = Authority::new("Harness authority, port 443");
    let pair = Pair::installation(&authority);
    let files = Files::new();
    let cert = files.write("fullchain.pem", pair.cert_pem().as_bytes());
    let key_text = pair.key_pem();
    let key = files.write("privkey.pem", key_text.as_bytes());
    let ca_file = files.authorities("authority.pem", &[&authority]);
    let options = body_lines(&key_text).iter().fold(
        VmOptions::new()
            .env("VIBE_SECRET_KEY", SecretKey::fresh().expose())
            .env(CERT_VAR, &text_of(&cert))
            .env(KEY_VAR, &text_of(&key))
            .reached_over_tls(TlsClient {
                roots: vec![authority.der()],
                ca_file,
            }),
        |o, l| o.hide(l),
    );
    let vm = start_on_a_port(&options, false);
    let at = Installation::of(vm, Provider::start(&[ada()]));
    at.connect(&Connection::to(&at.provider));
    let answer = at.vm.api_request("/v1/installation").send();
    assert_eq!(
        answer.json()["sign_in_address"],
        format!("https://auth.{DOMAIN}"),
        "expected the sign-in address on 443, the port left out: {}",
        answer.describe()
    );
    // An Application's first request, at the port it listens on: sent to
    // the sign-in address on 443.
    let mut browser = at.browser_as(ADA);
    let first = browser.open(&url(&format!(
        "https://anything.{DOMAIN}:{}/",
        at.vm.address().port()
    )));
    let location = first
        .response
        .header("location")
        .unwrap_or_default()
        .to_owned();
    if first.is_redirect() {
        assert!(
            location.starts_with(&format!("https://auth.{DOMAIN}/")),
            "expected a redirect to https://auth.{DOMAIN}/..., got {}",
            blackbox::http::mask_values(&location)
        );
    }
}

/// `vibe sign-in --json` over HTTPS to the site, through the browser, as
/// Ada: fails unless the address the tool opens is the https sign-in
/// address with the public port, and the tool ends with `0`. Returns the
/// machine signed in.
fn signed_in_tool(site: &Site) -> Machine {
    let at = &site.at;
    let machine = Machine::new();
    let opener = BrowserCommand::on(&machine);
    let (name, value) = opener.env();
    let (ca_name, ca_value) = site.ca_env();
    let args = at.sign_in_args(&["--json"]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let running = machine.spawn_into_files(
        &machine.cwd(),
        &refs,
        &[(name, value.as_str()), (ca_name, ca_value.as_str())],
    );
    let Some(opened) = opener.wait_opened(OPEN_TIMEOUT) else {
        let stderr = running.stderr_so_far();
        drop(running);
        panic!(
            "expected `vibe sign-in` to open the browser within {OPEN_TIMEOUT:?}; it wrote:\n{}",
            blackbox::connection::masked(&stderr, &[])
        );
    };
    let want = format!("https://auth.{DOMAIN}:{}/tool?", site.port());
    assert!(
        opened.starts_with(&want),
        "expected the tool to open the https sign-in address {want}..., it opened {}",
        blackbox::http::mask_values(&opened)
    );
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_tool_to_page(&mut browser, &url(&opened));
    assert_public_redirects(site, &steps, "the tool's sign-in");
    let page = steps.last().unwrap();
    let confirmed = at.answer_tool_page(&mut browser, page, Decision::Confirm, None);
    let at_tool = browser.follow(&confirmed);
    let run = hide(&running.wait(Duration::from_secs(30)), &[]);
    assert!(
        at_tool.response.status == 200 && run.code == Some(0),
        "expected the tool signed in over https\n{}",
        run.describe()
    );
    assert_eq!(run.json()["user"], ADA);
    machine
}

#[test]
fn the_tool_signs_in_through_the_browser_over_the_programs_own_tls() {
    let site = site();
    let machine = signed_in_tool(&site);
    let (ca_name, ca_value) = site.ca_env();
    let me = machine
        .run_with_env(&["whoami", "--json"], &[(ca_name, ca_value.as_str())])
        .ok_json();
    assert_eq!(me["user"], ADA, "{me}");
}

/// Waits until `running` printed a line holding `needle`, at most 15 s.
fn wait_for_line(running: &mut Running, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = running.stdout_so_far();
        if out.lines().any(|l| l.contains(needle)) && out.ends_with('\n') {
            return out;
        }
        assert!(
            !running.has_ended() && Instant::now() < deadline,
            "expected `vibe logs --follow` to print {needle:?}; it printed\n{out}\nstandard error:\n{}",
            running.stderr_so_far()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_tool_deploys_the_template_and_follows_its_logs_over_the_programs_own_tls() {
    let site = site();
    let machine = signed_in_tool(&site);
    let (ca_name, ca_value) = site.ca_env();
    let name = "greeter";
    let project = new_project_in_repository(&machine, name);
    init_repository(&project.dir);
    commit_all(&project.dir, "A new project");
    let run = deploy_with_env(&machine, &project.dir, &[], &[(ca_name, ca_value.as_str())]);
    let deployed = Deployed::of(&run);
    assert_eq!(deployed.application, name);
    assert_eq!(deployed.version, 1);

    // She opens it in her browser at its https address.
    let at = &site.at;
    let mut browser = at.browser_as(ADA);
    let steps = at.sign_in(&mut browser, name, "/");
    assert_public_redirects(&site, &steps, "the deployed Application's first request");
    assert!(
        steps.last().unwrap().response.text().contains(ADA),
        "expected the template's Application to answer her\n{}",
        describe_steps(&steps)
    );

    // `vibe logs --follow` receives an entry made after it started.
    let mut running = machine.spawn_into_files(
        &machine.cwd(),
        &["logs", name, "--follow", "--json"],
        &[(ca_name, ca_value.as_str())],
    );
    thread::sleep(Duration::from_secs(2));
    let target = at.app_url(name, "/after-it-started");
    let request = browser
        .request("GET", &target)
        .header("accept", "text/html");
    let answer = browser.send(&target, &request);
    let id = answer.request_id();
    let out = wait_for_line(&mut running, &id);
    running.signal("INT");
    running.wait(Duration::from_secs(10)).expect_code(0);
    let line: Value = out
        .lines()
        .find(|l| l.contains(&id))
        .map(|l| serde_json::from_str(l).unwrap())
        .unwrap();
    assert_eq!(line["application"], name, "{line}");
    assert_eq!(line["path"], "/after-it-started", "{line}");
}
