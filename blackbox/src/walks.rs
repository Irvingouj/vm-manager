//! What the tests that walk the platform's pages in a real browser share
//! (CONTRACT.md, "A real browser in the gate: What it walks"): an
//! installation reached over HTTPS as a person reaches one ([`Site`]), a
//! browser started for it that trusts its authority, the person's step at
//! the harness's identity provider taken by its form, signing in to the
//! Dashboard, an Application of a person's own, and reading what the page
//! shown says.
//!
//! The browser is [`crate::firefox::Firefox`]: everything it sends (its
//! `Origin`, `Referer`, `Sec-Fetch-...` headers and cookies) is its own; a
//! walk only opens addresses, clicks and types.
//!
//! **Over HTTPS** ("What it walks: Over HTTPS, as an installation is
//! reached"): the program is started with `--public-scheme https` and
//! `--public-port` the port of a TLS front of the harness's
//! ([`crate::tls`]) that stands before it, whose certificate (for
//! `api.<domain>` and `*.<domain>`) an authority of the test's own issued.
//! The browser trusts that authority, given to its fresh profile. The
//! identity provider stays the harness's, in plain HTTP on `127.0.0.1` (a
//! loopback address is a trustworthy origin to a browser).

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

use crate::api::version_body;
use crate::connection::SecretKey;
use crate::firefox::{COMMAND_TIMEOUT, Firefox, Options};
use crate::oidc::{Person, Provider};
use crate::sessions::Installation;
use crate::tls::{Authority, Bound, Files, Front, api_name, place_at, text_of};
use crate::tool::bearer;
use crate::tool_sign_ins::{Challenge, Decision};
use crate::vm::{DOMAIN, VmOptions, fixture_bundle};

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

/// An installation reached over HTTPS: the program with OIDC connected to
/// the harness's provider, behind a TLS front with a certificate of the
/// test's own authority ([`Site::start`]), or serving TLS itself with such
/// a certificate, no front between ([`Site::own_tls`]).
pub struct Site {
    // The front first: it ends before the program it stands before.
    pub front: Option<Front>,
    pub at: Installation,
    pub authority: Authority,
    // After the program: the files of its own TLS are here.
    pub files: Files,
    ca_file: PathBuf,
}

impl Site {
    /// These people at the provider, the program started with
    /// `--public-scheme https --public-port <the front's port>` and an
    /// installation's key, OIDC connected, and the front serving.
    pub fn start(people: &[Person]) -> Self {
        Self::start_with(people, &[])
    }

    /// As [`Site::start`], with more arguments of `vibe serve`
    /// (`--session-renewal 3`, say).
    pub fn start_with(people: &[Person], more: &[&str]) -> Self {
        Self::start_inner(people, more, false)
    }

    /// As [`Site::start_with`], the program listening on an address the
    /// harness chose, so that [`Site::restarted`] brings it back at the
    /// same address, behind the same front (a walk that needs what the
    /// program holds in memory to be read again from the platform
    /// database, after the test changed it by hand).
    pub fn start_restartable(people: &[Person], more: &[&str]) -> Self {
        Self::start_inner(people, more, true)
    }

    fn start_inner(people: &[Person], more: &[&str], fixed: bool) -> Self {
        let bound = Bound::new();
        let port = bound.port().to_string();
        let mut options = VmOptions::new()
            .args_extra(&["--public-scheme", "https", "--public-port", &port])
            .args_extra(more)
            .env("VIBE_SECRET_KEY", SecretKey::fresh().expose());
        if fixed {
            let chosen = crate::postgres::free_port()
                .unwrap_or_else(|e| panic!("cannot find a free port: {e}"));
            options = options.listen(std::net::SocketAddr::from(([127, 0, 0, 1], chosen)));
        }
        let at = Installation::from(Provider::start(people), &options);
        let authority = Authority::new("Harness authority of the walks");
        let identity = authority.issue(&[&api_name(), &format!("*.{DOMAIN}")]);
        let front = bound.tls(&identity, at.vm.address());
        let files = Files::new();
        let ca_file = files.authorities("walks-authority.pem", &[&authority]);
        Self {
            front: Some(front),
            at,
            authority,
            files,
            ca_file,
        }
    }

    /// Ends the program and starts it again at the address it had (a site
    /// made with [`Site::start_restartable`]), on the same platform
    /// database, beside the same provider and behind the same front. A
    /// person's session and a tool credential stay as they were.
    #[must_use]
    pub fn restarted(self) -> Self {
        let Self {
            front,
            at,
            authority,
            files,
            ca_file,
        } = self;
        Self {
            front,
            at: at.restarted(),
            authority,
            files,
            ca_file,
        }
    }

    /// These people at the provider, the program serving TLS itself
    /// (CONTRACT.md, "The program serves TLS itself") with a certificate
    /// for `api.<domain>` and `*.<domain>` of the test's own authority and
    /// its key in `VIBE_TLS_CERT_FILE` and `VIBE_TLS_KEY_FILE`, listening
    /// on a port the harness chose and given as `--public-port`, with an
    /// installation's key, OIDC connected over HTTPS. No front: a browser
    /// reaches the program itself.
    pub fn own_tls(people: &[Person]) -> Self {
        let authority = Authority::new("Harness authority of the walks, served by the program");
        let pair = crate::own_tls::Pair::installation(&authority);
        let files = Files::new();
        let cert = files.write("fullchain.pem", pair.cert_pem().as_bytes());
        let key_text = pair.key_pem();
        let key = files.write("privkey.pem", key_text.as_bytes());
        let ca_file = files.authorities("walks-authority.pem", &[&authority]);
        let mut options = VmOptions::new()
            .env("VIBE_SECRET_KEY", SecretKey::fresh().expose())
            .env(crate::own_tls::CERT_VAR, &text_of(&cert))
            .env(crate::own_tls::KEY_VAR, &text_of(&key))
            .reached_over_tls(crate::vm::TlsClient {
                roots: vec![authority.der()],
                ca_file: ca_file.clone(),
            });
        for line in crate::own_tls::body_lines(&key_text) {
            options = options.hide(&line);
        }
        let vm = crate::own_tls::start_on_a_port(&options, true);
        let at = Installation::of(vm, Provider::start(people));
        at.connect(&crate::connection::Connection::to(&at.provider));
        Self {
            front: None,
            at,
            authority,
            files,
            ca_file,
        }
    }

    /// The port a browser reaches the installation at: the front's, or the
    /// program's own when it serves TLS itself.
    pub fn port(&self) -> u16 {
        self.front
            .as_ref()
            .map_or_else(|| self.at.vm.address().port(), Front::port)
    }

    pub fn provider(&self) -> &Provider {
        &self.at.provider
    }

    /// `https://<name>.<domain>:<port>`.
    pub fn origin(&self, name: &str) -> String {
        format!("https://{name}.{DOMAIN}:{}", self.port())
    }

    /// The host of a name of the installation: `<name>.<domain>`.
    pub fn host(name: &str) -> String {
        format!("{name}.{DOMAIN}")
    }

    /// The Dashboard's address with this path and query.
    pub fn dashboard(&self, target: &str) -> String {
        format!("{}{target}", self.origin("dashboard"))
    }

    /// An Application's address with this path and query.
    pub fn application(&self, name: &str, target: &str) -> String {
        format!("{}{target}", self.origin(name))
    }

    /// The sign-in address with this path and query.
    pub fn auth(&self, target: &str) -> String {
        format!("{}{target}", self.origin("auth"))
    }

    /// A browser that reaches the installation's names (the sign-in
    /// address, the Dashboard and these Applications) on this machine and
    /// trusts the installation's authority.
    pub fn browser(&self, applications: &[&str]) -> Firefox {
        Firefox::start_with(
            &Options::reaching(&names_of(applications)).trusting(self.authority.der().as_ref()),
        )
    }

    /// `VIBE_CA_FILE` naming the authority's certificate, for the tool.
    pub fn ca_env(&self) -> (&'static str, String) {
        ("VIBE_CA_FILE", text_of(&self.ca_file))
    }

    /// `--domain <domain> --resolve 127.0.0.1 --port <the front's port>`:
    /// where the tool reaches the installation.
    pub fn place(&self) -> Vec<String> {
        place_at(self.port())
    }

    /// A tool credential for this person, made as the tool makes one with
    /// a browser, played by the harness's own small browser (setup, not a
    /// walk): a challenge with a port, `/tool` at the `https` sign-in
    /// address and the provider, confirmed, and `POST /v1/tool-sign-ins`.
    pub fn credential_for(&self, sub: &str) -> String {
        let at = &self.at;
        let challenge = Challenge::fresh();
        let port =
            crate::postgres::free_port().unwrap_or_else(|e| panic!("cannot find a free port: {e}"));
        let mut browser = at.browser_as(sub);
        let mut address = crate::browser::url(&self.auth("/tool"));
        address
            .query_pairs_mut()
            .append_pair("challenge", &challenge.challenge)
            .append_pair("port", &port.to_string())
            .append_pair("machine", "harness-machine");
        let steps = at.walk_tool_to_page(&mut browser, &address);
        let page = steps.last().unwrap_or_else(|| panic!("no steps"));
        let confirmed = at.answer_tool_page(&mut browser, page, Decision::Confirm, None);
        let grant = at.grant_of(&confirmed, port);
        let answer = at.post_tool_sign_in(challenge.verifier(), Some(&grant));
        if answer.status != 201 {
            at.vm.fail(&format!(
                "expected 201 from POST /v1/tool-sign-ins for a confirmed sign-in\n{}",
                crate::connection::shown(&answer, &[])
            ));
        }
        answer.json()["credential"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a credential in the answer"))
            .to_owned()
    }

    /// `name`, made by `sub` through the platform's API with
    /// `description`, its first Version the fixture [`FIXTURE`], shared
    /// with nobody. Returns `sub`'s tool credential.
    pub fn application_of(&self, sub: &str, name: &str, description: Option<&str>) -> String {
        let credential = self.credential_for(sub);
        let created = bearer(&self.at.vm, "/v1/applications", &credential)
            .method("POST")
            .json(&json!({ "name": name, "description": description }))
            .send();
        created.expect_status(201);
        let sent = bearer(
            &self.at.vm,
            &format!("/v1/applications/{name}/versions"),
            &credential,
        )
        .method("POST")
        .json(&version_body(&fixture_bundle(FIXTURE)))
        .send();
        sent.expect_status(201);
        credential
    }

    /// The entries `name` is shared with, as the platform's API answers
    /// its Contributor (`user:<id>`, `group:<Group>`, `everyone`).
    pub fn shared_with(&self, credential: &str, name: &str) -> Vec<String> {
        let answer = bearer(&self.at.vm, &format!("/v1/applications/{name}"), credential).send();
        answer.expect_status(200);
        crate::sharing::keys(&answer.json()["shared_with"])
    }
}

/// Whether `url` is on this host and port.
pub fn on(url: &str, host: &str, port: u16) -> bool {
    Url::parse(url).is_ok_and(|u| u.host_str() == Some(host) && u.port() == Some(port))
}

/// Whether `url` is an `https` address on this name of the installation
/// and port.
pub fn on_site(url: &str, site: &Site, name: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some(Site::host(name).as_str())
            && u.port() == Some(site.port())
    })
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
pub fn sign_in_to_dashboard(firefox: &mut Firefox, site: &Site, sub: &str) {
    firefox.navigate(&site.dashboard("/"));
    choose_at_provider(firefox, site.provider(), sub);
    let landed = firefox.wait_for_page("the Dashboard after signing in", |u| {
        on_site(u, site, "dashboard")
    });
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
