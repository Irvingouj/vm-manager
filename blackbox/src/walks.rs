//! What the tests that walk the platform's pages in a real browser share
//! (CONTRACT.md, "A real browser in the gate: What it walks"): the names a
//! browser must reach, a browser started for an installation, the person's
//! step at the harness's identity provider taken by its form, signing in
//! to the Dashboard, an Application of a person's own, and reading what
//! the page shown says.
//!
//! The browser is [`crate::firefox::Firefox`]: everything it sends (its
//! `Origin`, `Referer`, `Sec-Fetch-...` headers and cookies) is its own; a
//! walk only opens addresses, clicks and types.

use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

use crate::api::version_body;
use crate::firefox::{COMMAND_TIMEOUT, Firefox, Options};
use crate::oidc::Provider;
use crate::sessions::Installation;
use crate::tool::bearer;
use crate::vm::{DOMAIN, fixture_bundle};

/// The fixture an Application of the walks runs: it answers JSON naming
/// the User it was given the request as.
pub const FIXTURE: &str = "honest.js";

/// The names a browser of an installation reaches: the sign-in address, the
/// Dashboard's, and these Applications'.
pub fn names_of(applications: &[&str]) -> Vec<String> {
    let mut names = vec![format!("auth.{DOMAIN}"), format!("dashboard.{DOMAIN}")];
    names.extend(applications.iter().map(|a| format!("{a}.{DOMAIN}")));
    names
}

/// A browser that reaches the installation's names (the sign-in address,
/// the Dashboard and these Applications) on this machine.
pub fn browser_for(applications: &[&str]) -> Firefox {
    Firefox::start_with(&Options::reaching(&names_of(applications)))
}

/// The Dashboard's address of `at` with this path and query, as text.
pub fn dashboard(at: &Installation, target: &str) -> String {
    crate::dashboard::oidc_url(at, target).to_string()
}

/// An Application's address of `at` with this path and query, as text.
pub fn application(at: &Installation, name: &str, target: &str) -> String {
    at.app_url(name, target).to_string()
}

/// The sign-in address of `at` with this path and query, as text.
pub fn auth(at: &Installation, target: &str) -> String {
    at.auth_url(target).to_string()
}

/// Whether `url` is on this host and port.
pub fn on(url: &str, host: &str, port: u16) -> bool {
    Url::parse(url).is_ok_and(|u| u.host_str() == Some(host) && u.port() == Some(port))
}

/// Whether the page shown is the provider's.
pub fn at_provider(firefox: &mut Firefox, provider: &Provider) -> bool {
    let url = firefox.url();
    on(
        &url,
        &provider.address().ip().to_string(),
        provider.address().port(),
    )
}

/// When the page shown is the provider's page asking who you are: chooses
/// this person by its form, as a person does, and waits until the browser
/// has left the provider. A browser the provider knows already (it signed
/// in there before) was sent on without being asked, and is left as it is.
pub fn choose_at_provider(firefox: &mut Firefox, provider: &Provider, sub: &str) {
    let host = provider.address().ip().to_string();
    let port = provider.address().port();
    firefox.wait_for_page("a page loaded", |_| true);
    if !on(&firefox.url(), &host, port) {
        return;
    }
    let button = firefox.the(&format!(
        "form[data-harness-person=\"{sub}\"] button[type=\"submit\"]"
    ));
    firefox.click(&button);
    firefox.wait_for_page("the browser to leave the identity provider", |u| {
        !on(u, &host, port)
    });
}

/// Opens the Dashboard with no session, is sent to the provider, chooses
/// `sub` there, and must land on the Dashboard's `/`.
pub fn sign_in_to_dashboard(firefox: &mut Firefox, at: &Installation, sub: &str) {
    firefox.navigate(&dashboard(at, "/"));
    choose_at_provider(firefox, &at.provider, sub);
    let host = format!("dashboard.{DOMAIN}");
    let port = at.port();
    let landed = firefox.wait_for_page("the Dashboard after signing in", |u| on(u, &host, port));
    let path = Url::parse(&landed).map(|u| u.path().to_owned());
    assert_eq!(
        path.as_deref(),
        Ok("/"),
        "expected to land on the Dashboard's `/`, got {landed}"
    );
}

/// The page shown, as JSON (an Application's answer shown as its text).
pub fn page_json(firefox: &mut Firefox) -> Value {
    let text = firefox.wait_for("a page whose text is JSON", Duration::from_secs(10), |f| {
        let text = f.script(
            "return document.body ? document.body.textContent : '';",
            &[],
        );
        text.as_str()
            .filter(|t| serde_json::from_str::<Value>(t).is_ok())
            .map(str::to_owned)
    });
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// How many authorization requests the provider has been sent.
pub fn authorizations(provider: &Provider) -> usize {
    provider.requests_to("/authorize").len()
}

/// `name`, made by `sub` through the platform's API with `description`,
/// its first Version the fixture [`FIXTURE`], shared with nobody. Returns
/// `sub`'s tool credential.
pub fn application_of(
    at: &Installation,
    sub: &str,
    name: &str,
    description: Option<&str>,
) -> String {
    let credential = at.tool_credential_for(sub);
    let created = bearer(&at.vm, "/v1/applications", &credential)
        .method("POST")
        .json(&json!({ "name": name, "description": description }))
        .send();
    created.expect_status(201);
    let sent = bearer(
        &at.vm,
        &format!("/v1/applications/{name}/versions"),
        &credential,
    )
    .method("POST")
    .json(&version_body(&fixture_bundle(FIXTURE)))
    .send();
    sent.expect_status(201);
    credential
}

/// The entries `name` is shared with, as the platform's API answers its
/// Contributor (`user:<id>`, `group:<Group>`, `everyone`).
pub fn shared_with(at: &Installation, credential: &str, name: &str) -> Vec<String> {
    let answer = bearer(&at.vm, &format!("/v1/applications/{name}"), credential).send();
    answer.expect_status(200);
    crate::sharing::keys(&answer.json()["shared_with"])
}

/// Waits for an element `css` on the page shown; fails, showing the page,
/// unless one comes within the command timeout.
pub fn wait_for_element(firefox: &mut Firefox, css: &str) -> crate::firefox::Element {
    firefox.wait_for(&format!("an element `{css}`"), COMMAND_TIMEOUT, |f| {
        f.find(css)
    })
}

/// The submit button of the form `form` finds (its one `button` or
/// `input[type=submit]`); fails, showing the page, unless there is one.
pub fn submit_button_of(firefox: &mut Firefox, form: &str) -> crate::firefox::Element {
    let form_element = firefox.the(form);
    let mut buttons = firefox.find_within(
        &form_element,
        "button:not([type=\"button\"]):not([type=\"reset\"]), input[type=\"submit\"]",
    );
    if buttons.len() != 1 {
        let page = firefox.describe();
        panic!(
            "expected one submit button in `{form}`, found {}\n{page}",
            buttons.len()
        );
    }
    buttons.remove(0)
}

/// Fills the share form of the Application's page shown, as a person
/// does: `kind` chosen (in its `select` or among its radio buttons),
/// `value` typed (unless empty), and its button clicked. Waits for the
/// page that comes back.
pub fn share_by_form(firefox: &mut Firefox, kind: &str, value: &str) {
    const FORM: &str = "form[data-vm-form=\"share\"]";
    let form = firefox.the(FORM);
    let option = firefox
        .find_within(
            &form,
            &format!("select[name=\"kind\"] option[value=\"{kind}\"]"),
        )
        .into_iter()
        .next()
        .or_else(|| {
            firefox
                .find_within(
                    &form,
                    &format!("input[type=\"radio\"][name=\"kind\"][value=\"{kind}\"]"),
                )
                .into_iter()
                .next()
        });
    let Some(option) = option else {
        let page = firefox.describe();
        panic!("expected the share form to offer the kind {kind:?}\n{page}");
    };
    firefox.click(&option);
    if !value.is_empty() {
        let input = firefox
            .find_within(&form, "input[name=\"value\"]")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("expected a text input `value` in the share form"));
        firefox.type_into(&input, value);
    }
    let button = submit_button_of(firefox, FORM);
    click_to_another_page(firefox, &button);
}

/// Clicks the button of the unshare form beside the entry of `kind` and
/// `id` on the Application's page shown, and waits for the page that comes
/// back.
pub fn unshare_by_form(firefox: &mut Firefox, kind: &str, id: &str) {
    let form = format!(
        "form[data-vm-form=\"unshare\"]:has(input[name=\"kind\"][value=\"{kind}\"]):has(input[name=\"value\"][value=\"{id}\"])"
    );
    let button = submit_button_of(firefox, &form);
    click_to_another_page(firefox, &button);
}

/// Clicks `element` (a link, a form's button) as a person does, and waits
/// until the page it leads to has replaced the one shown and has loaded.
pub fn click_to_another_page(firefox: &mut Firefox, element: &crate::firefox::Element) {
    // A mark on the window of the page shown: gone once another document
    // is shown. It changes nothing the page sends.
    firefox.script("window.__harnessMarked = true; return null;", &[]);
    firefox.click(element);
    firefox.wait_for("the page the click led to", COMMAND_TIMEOUT, |f| {
        f.command(
            "WebDriver:ExecuteScript",
            &json!({ "script": "return window.__harnessMarked === undefined && document.readyState === 'complete';", "args": [] }),
        )
        .ok()
        .and_then(|v| v["value"].as_bool())
        .filter(|done| *done)
    });
}

/// The button that chooses the test User `id` on the local run's page of
/// test Users shown: a submit button named `user` with that value, or the
/// one button of a form that sends `user` = `id`.
pub fn submit_button_for_user(firefox: &mut Firefox, id: &str) -> crate::firefox::Element {
    let named = format!(
        "form button[name=\"user\"][value=\"{id}\"], form input[type=\"submit\"][name=\"user\"][value=\"{id}\"]"
    );
    let mut found = firefox.find_all(&named);
    if found.is_empty() {
        let form = format!("form:has(input[type=\"hidden\"][name=\"user\"][value=\"{id}\"])");
        if firefox.find(&form).is_some() {
            return submit_button_of(firefox, &form);
        }
    }
    if found.len() != 1 {
        let page = firefox.describe();
        panic!(
            "expected one button choosing the test User {id:?} on the page, found {}\n{page}",
            found.len()
        );
    }
    found.remove(0)
}
