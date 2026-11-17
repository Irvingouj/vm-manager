//! What the tests of sign-in and sessions share (CONTRACT.md, "Sign-in and
//! sessions"): the harness's identity provider beside a program started
//! with OIDC (its fixture Applications installed first by a start with the
//! stand-in on the same platform database), the addresses a browser sees,
//! test people, and checks on cookies, pages and the log.

use std::time::{Duration, SystemTime};

use serde_json::Value;
use url::Url;

use crate::browser::{Browser, SetCookie, Step, describe_steps, url};
use crate::http::Response;
use crate::oidc::{CLIENT_ID, Person, Provider};
use crate::vm::{DOMAIN, Vm, VmOptions};

/// Ada: `sub` distinct from her name and email, in `finance`.
pub const ADA: &str = "00u-ada-1815";
/// Grace: in `devops`.
pub const GRACE: &str = "00u-grace-1906";

/// The platform's session cookie with `--public-scheme http`.
pub const SESSION_COOKIE: &str = "vm-session";
/// The cookie of step 1 with `--public-scheme http`.
pub const SIGN_IN_COOKIE: &str = "vm-sign-in";

pub fn ada() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance"])
}

pub fn grace() -> Person {
    Person::new(GRACE)
        .named("Grace Hopper")
        .email("grace@example.test")
        .in_groups(&["devops"])
}

/// The options that start the program with OIDC against this provider:
/// `--oidc-issuer`, `--oidc-client-id`, `VM_OIDC_CLIENT_SECRET`, and
/// `--public-scheme http` so the browser can follow every redirect.
pub fn oidc_options(provider: &Provider) -> VmOptions {
    VmOptions::new()
        .args_extra(&[
            "--oidc-issuer",
            provider.issuer(),
            "--oidc-client-id",
            CLIENT_ID,
            "--public-scheme",
            "http",
        ])
        .env("VM_OIDC_CLIENT_SECRET", provider.client_secret().expose())
}

/// A provider and a program signed in through it.
pub struct Installation {
    // The program first, so that it ends before the provider does.
    pub vm: Vm,
    pub provider: Provider,
}

impl Installation {
    /// Ada and Grace at the provider, these Applications (name, fixture
    /// file) installed, the program started with OIDC.
    pub fn start(applications: &[(&str, &str)]) -> Self {
        Self::start_with(applications, &[ada(), grace()], &[])
    }

    /// As [`Installation::start`] with these people and more arguments
    /// (`--session-renewal 2`, say).
    pub fn start_with(applications: &[(&str, &str)], people: &[Person], more: &[&str]) -> Self {
        let provider = Provider::start(people);
        let options = applications
            .iter()
            .fold(oidc_options(&provider).args_extra(more), |o, (n, f)| {
                o.application(n, f)
            });
        Self::from(provider, &options)
    }

    /// The program started with these options beside this provider.
    pub fn from(provider: Provider, options: &VmOptions) -> Self {
        let vm = Vm::start(options);
        Self { vm, provider }
    }

    /// The port browsers use: the listening one (`--public-scheme http`
    /// without `--public-port`).
    pub fn port(&self) -> u16 {
        self.vm.address().port()
    }

    pub fn auth_host(&self) -> String {
        format!("auth.{DOMAIN}")
    }

    /// `http://auth.<domain>:<port>`.
    pub fn auth_origin(&self) -> String {
        format!("http://auth.{DOMAIN}:{}", self.port())
    }

    /// `http://<application>.<domain>:<port>`.
    pub fn app_origin(&self, application: &str) -> String {
        format!("http://{application}.{DOMAIN}:{}", self.port())
    }

    /// The sign-in address with this path and query.
    pub fn auth_url(&self, target: &str) -> Url {
        url(&format!("{}{target}", self.auth_origin()))
    }

    /// An Application's address with this path and query.
    pub fn app_url(&self, application: &str, target: &str) -> Url {
        url(&format!("{}{target}", self.app_origin(application)))
    }

    /// `<sign-in address>/callback`: the redirect address registered at the
    /// provider.
    pub fn callback(&self) -> String {
        format!("{}/callback", self.auth_origin())
    }

    /// A browser signed in as no one anywhere.
    pub fn browser(&self) -> Browser {
        Browser::new(self.vm.address())
    }

    /// A browser signed in at the provider (only) as this person.
    pub fn browser_as(&self, sub: &str) -> Browser {
        let mut browser = self.browser();
        browser.sign_in_at_provider(&self.provider, sub);
        browser
    }

    /// Opens the Application's `target` and follows every redirect; fails
    /// unless it ends with `200` on that very address. Returns the steps.
    pub fn sign_in(&self, browser: &mut Browser, application: &str, target: &str) -> Vec<Step> {
        let steps = browser.navigate(&self.app_url(application, target));
        self.assert_landed(&steps, application, target);
        steps
    }

    /// Steps 1 to 3 by hand for a browser with no platform session: the
    /// Application's page (`302` to the sign-in address), the sign-in
    /// address (`302` to the provider), the provider (`302` back to the
    /// callback). Fails unless each is such a redirect. The next step,
    /// following the last, is the callback.
    pub fn walk_to_callback(
        &self,
        browser: &mut Browser,
        application: &str,
        target: &str,
    ) -> Vec<Step> {
        let mut steps = vec![browser.open(&self.app_url(application, target))];
        for _ in 0..2 {
            let last = steps.last().unwrap_or_else(|| panic!("no steps"));
            if !last.is_redirect() {
                self.vm.fail(&format!(
                    "expected a redirect on the way to the callback, but the way went:\n{}",
                    describe_steps(&steps)
                ));
            }
            let next = browser.follow(last);
            steps.push(next);
        }
        let last = steps.last().unwrap_or_else(|| panic!("no steps"));
        let to_callback =
            last.is_redirect() && last.location().as_str().starts_with(&self.callback());
        if !to_callback {
            self.vm.fail(&format!(
                "expected the identity provider to send the browser to {}, but the way went:\n{}",
                self.callback(),
                describe_steps(&steps)
            ));
        }
        steps
    }

    /// Steps 1 to 3 and the callback (step 3's end): returns the address of
    /// step 4, `<application address>/_vm/session?code=...`, not yet
    /// visited.
    pub fn walk_to_session_code(
        &self,
        browser: &mut Browser,
        application: &str,
        target: &str,
    ) -> Url {
        let mut steps = self.walk_to_callback(browser, application, target);
        let last = steps.last().unwrap_or_else(|| panic!("no steps"));
        let callback = browser.follow(last);
        steps.push(callback);
        let callback = steps.last().unwrap_or_else(|| panic!("no steps"));
        let to_session = callback.is_redirect()
            && callback.location().host_str() == Some(format!("{application}.{DOMAIN}").as_str())
            && callback.location().path() == "/_vm/session";
        if !to_session {
            self.vm.fail(&format!(
                "expected the callback to send the browser to {application}'s /_vm/session, but the way went:\n{}",
                describe_steps(&steps)
            ));
        }
        callback.location()
    }

    /// Fails unless the navigation ended with `200` at the Application's
    /// `target`.
    pub fn assert_landed(&self, steps: &[Step], application: &str, target: &str) {
        let want = self.app_url(application, target);
        let last = steps.last().unwrap_or_else(|| panic!("no steps"));
        if last.response.status != 200 || last.url != want {
            self.vm.fail(&format!(
                "expected the browser to land signed in on {want} with 200, but the way went:\n{}",
                describe_steps(steps)
            ));
        }
    }

    /// A `GET` of the Application's `target` asking for JSON (not a page),
    /// with the browser's cookies.
    pub fn get_json(&self, browser: &mut Browser, application: &str, target: &str) -> Response {
        let at = self.app_url(application, target);
        let request = browser
            .request("GET", &at)
            .header("accept", "application/json");
        browser.send(&at, &request)
    }

    /// The User `fixtures/honest.js` was given (`ctx.user()`), for a browser
    /// with a session there; fails unless it answers `200`.
    pub fn user_seen(&self, browser: &mut Browser, application: &str) -> Value {
        let answer = self.get_json(browser, application, "/");
        if answer.status != 200 {
            self.vm.fail(&format!(
                "expected 200 from {application} for a browser with a session, got\n{}",
                answer.describe()
            ));
        }
        answer.json()["user"].clone()
    }

    /// `GET <sign-in address>/me` with the browser's cookies.
    pub fn me(&self, browser: &mut Browser) -> Response {
        let at = self.auth_url("/me");
        let request = browser
            .request("GET", &at)
            .header("accept", "application/json");
        browser.send(&at, &request)
    }

    /// Fails unless this browser has no session on the Application: a
    /// request for JSON is `401` and a page is sent to the sign-in address.
    pub fn assert_no_session(&self, browser: &mut Browser, application: &str, context: &str) {
        let json = self.get_json(browser, application, "/");
        if json.status != 401 {
            self.vm.fail(&format!(
                "{context}: expected no session on {application} (401 for a request that is not a page), got\n{}",
                json.describe()
            ));
        }
        self.vm.records_of(&json).assert_no_application_code_ran();
        let page = browser.open(&self.app_url(application, "/"));
        let goes_to_sign_in = page.is_redirect()
            && page.location().host_str() == Some(self.auth_host().as_str())
            && page.location().path() == "/sign-in";
        if !goes_to_sign_in {
            self.vm.fail(&format!(
                "{context}: expected a page of {application} to be sent to the sign-in address, got\n{}",
                page.response.describe()
            ));
        }
    }

    /// The value of the cookie of this name the browser holds for the
    /// Application's (or sign-in) host, failing if there is none.
    pub fn cookie_value(&self, browser: &Browser, host: &str, name: &str) -> String {
        browser
            .jar
            .get(host, name)
            .map(|c| c.value.clone())
            .unwrap_or_else(|| {
                self.vm.fail(&format!(
                    "expected the browser to hold a {name:?} cookie for {host}; it holds {:?} (refused: {:?})",
                    browser.jar.all(),
                    browser.jar.rejected()
                ))
            })
    }

    /// Fails if any of these values appears in any line of the program's
    /// log.
    pub fn assert_none_in_log(&self, values: &[String], context: &str) {
        let lines = self.vm.lines();
        for value in values.iter().filter(|v| !v.is_empty()) {
            if let Some(line) = lines.iter().find(|l| l.raw.contains(value.as_str())) {
                self.vm.fail(&format!(
                    "{context}: expected no line of the log to hold a code, token or secret, but this one does: {}",
                    line.raw.replace(value.as_str(), "<IT>")
                ));
            }
        }
    }
}

/// Fails unless `value` is 256 bits of randomness as text: 64 hex digits,
/// or 43 characters of base64url (44 with padding).
pub fn assert_256_bits(value: &str, context: &str) {
    let hex = value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit());
    let base64 = (43..=44).contains(&value.len())
        && value
            .trim_end_matches('=')
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/');
    assert!(
        hex || base64,
        "{context}: expected a value of 256 bits (64 hex digits or 43 base64url characters), got {} characters: {value:?}",
        value.len()
    );
}

/// Fails unless this `Set-Cookie` is one of the platform's over `http`:
/// this name, `HttpOnly`, `SameSite=Lax`, `Path=/`, no `Domain`, no
/// `Secure`.
pub fn assert_platform_cookie(cookie: &SetCookie, name: &str, context: &str) {
    assert_eq!(
        cookie.name, name,
        "{context}: the cookie's name in {:?}",
        cookie.raw
    );
    assert!(
        cookie.has("httponly"),
        "{context}: expected HttpOnly in {:?}",
        cookie.raw
    );
    assert!(
        cookie
            .value_of("samesite")
            .is_some_and(|v| v.eq_ignore_ascii_case("lax")),
        "{context}: expected SameSite=Lax in {:?}",
        cookie.raw
    );
    assert_eq!(
        cookie.value_of("path"),
        Some("/"),
        "{context}: expected Path=/ in {:?}",
        cookie.raw
    );
    assert!(
        !cookie.has("domain"),
        "{context}: expected no Domain in {:?}: no cookie spans the wildcard domain",
        cookie.raw
    );
    assert!(
        !cookie.has("secure"),
        "{context}: expected no Secure with --public-scheme http in {:?}",
        cookie.raw
    );
}

/// Fails unless the cookie lives about ten minutes (`Max-Age=600`, or an
/// `Expires` ten minutes ahead).
pub fn assert_ten_minutes(cookie: &SetCookie, context: &str) {
    let now = SystemTime::now();
    let end = cookie.ends_at(now).unwrap_or_else(|| {
        panic!(
            "{context}: expected an age of 10 minutes on {:?}, it has none",
            cookie.raw
        )
    });
    let left = end.duration_since(now).unwrap_or_default();
    assert!(
        left > Duration::from_secs(590) && left <= Duration::from_secs(601),
        "{context}: expected an age of 10 minutes on {:?}, it is {left:?}",
        cookie.raw
    );
}

/// The one `Set-Cookie` of this name on an answer, failing unless there is
/// exactly one.
pub fn the_set_cookie(response: &Response, name: &str, context: &str) -> SetCookie {
    let all = crate::browser::set_cookies(response);
    let named: Vec<&SetCookie> = all.iter().filter(|c| c.name == name).collect();
    match named.as_slice() {
        [one] => (*one).clone(),
        _ => panic!(
            "{context}: expected exactly one Set-Cookie named {name:?}, got\n{}",
            response.describe()
        ),
    }
}

/// Fails unless the answer is a page of the platform's own with this
/// status: `text/html`, a link (`href`), and none of these values (codes,
/// tokens, the secret) in its headers or body, nor anything shaped like a
/// JWT.
pub fn assert_failure_page(
    response: &Response,
    statuses: &[u16],
    hidden: &[String],
    context: &str,
) {
    assert!(
        statuses.contains(&response.status),
        "{context}: expected a page with status {statuses:?}, got\n{}",
        response.describe()
    );
    assert!(
        response
            .header("content-type")
            .is_some_and(|c| c.to_ascii_lowercase().starts_with("text/html")),
        "{context}: expected a page (text/html), got\n{}",
        response.describe()
    );
    let text = response.describe();
    assert!(
        response.text().to_ascii_lowercase().contains("href"),
        "{context}: expected the page to give a link to try again, got\n{text}"
    );
    for value in hidden.iter().filter(|v| v.len() >= 8) {
        assert!(
            !text.contains(value.as_str()),
            "{context}: expected nothing of a code, token or secret on the page, but it holds one:\n{}",
            text.replace(value.as_str(), "<IT>")
        );
    }
    assert!(
        !text.contains("eyJ"),
        "{context}: expected nothing shaped like a JWT on the page, got\n{text}"
    );
    assert!(
        crate::browser::set_cookies(response)
            .iter()
            .all(|c| c.name != SESSION_COOKIE && c.name != format!("__Host-{SESSION_COOKIE}")),
        "{context}: expected no session to begin, but the page sets a session cookie\n{text}"
    );
}
