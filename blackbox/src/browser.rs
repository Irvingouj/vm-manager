//! A small browser for the tests of sign-in and sessions (CONTRACT.md,
//! "Sign-in and sessions"): a cookie jar that keeps cookies per host as a
//! browser does, and navigations that follow redirects one step at a time so
//! a test can look at each.
//!
//! Addresses: a URL whose host is an IP address (the harness's identity
//! provider, `http://127.0.0.1:<port>`) is connected to as it says; any
//! other host (`auth.vm.test`, `<application>.vm.test`) is connected to at
//! the program's listening address. `Host` is the URL's host and port, as a
//! browser sends it. A name given to [`Browser::resolve`] is reached at the
//! address given there. A browser made [`Browser::behind_tls`] speaks TLS
//! for an `https` address of the installation (the install tests: the
//! Compose file's proxy ends TLS), asking for the URL's host as the server
//! name; otherwise such an address goes to the program in plain HTTP.
//!
//! The jar follows RFC 6265 and what browsers do today:
//!
//! - a cookie without `Domain` is host-only: sent back to exactly the host
//!   that set it, never to a sibling or a parent; one with `Domain` is sent
//!   to that domain and every name under it, and is refused unless the
//!   setting host is that domain or under it (and a one-label domain, such
//!   as `test`, is refused as a public suffix);
//! - `Path` (or the default path of the request) is matched as RFC 6265
//!   says; ports are ignored, as browsers ignore them for cookies;
//! - `Max-Age` wins over `Expires`; a cookie whose time is past is removed;
//! - `Secure`: the tests run with `--public-scheme http`, where the
//!   contract's cookies carry no `Secure` and no `__Host-` prefix. Over
//!   plain `http` this jar does what browsers do for a host that is not
//!   `localhost`: it **refuses** a cookie with `Secure` (and so every
//!   `__Host-` and `__Secure-` cookie, which need it), records the refusal
//!   in [`Jar::rejected`], and never sends a `Secure` cookie over `http`.
//!   A `__Host-` cookie is also refused with a `Domain` or a `Path` other
//!   than `/`;
//! - `HttpOnly` and `SameSite` are kept for the tests to look at, not
//!   enforced: every `<application>.vm.test` is the same site as every
//!   other, so `SameSite=Lax` keeps nothing from them, and the tests of the
//!   origin rule set `Origin` and `Sec-Fetch-*` themselves.
//!
//! Navigations send what a browser sends for a top-level navigation (an
//! `Accept` with `text/html`, `Sec-Fetch-Mode: navigate`, `Sec-Fetch-Dest:
//! document`; `Sec-Fetch-Site: none` for an address typed by the person,
//! which a browser keeps through every redirect). Browsers send
//! `Sec-Fetch-*` only to `https` and `localhost`; this browser sends them
//! over `http` too, as the installation's browsers will over `https`.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::{Host, Url};

use crate::http::{Request, Response};
use crate::oidc::{PERSON_COOKIE, Provider};

/// The longest one step may take: more than the 10 seconds the program
/// may wait for the identity provider, so that its own answer is heard.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(25);

/// The most redirects one navigation follows.
pub const MAX_REDIRECTS: usize = 20;

/// The `Accept` of a navigation, as browsers send it.
pub const NAVIGATION_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

/// One `Set-Cookie` line, parsed.
#[derive(Clone, Debug)]
pub struct SetCookie {
    pub raw: String,
    pub name: String,
    pub value: String,
    /// Attribute names as given, values trimmed; `None` for a flag.
    pub attributes: Vec<(String, Option<String>)>,
}

impl SetCookie {
    /// Parses a `Set-Cookie` value; `None` when it has no `=` in its first
    /// part (browsers drop those) or no name and no value.
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split(';');
        let (name, value) = parts.next()?.split_once('=')?;
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() && value.is_empty() {
            return None;
        }
        let attributes = parts
            .filter(|p| !p.trim().is_empty())
            .map(|p| match p.split_once('=') {
                Some((n, v)) => (n.trim().to_owned(), Some(v.trim().to_owned())),
                None => (p.trim().to_owned(), None),
            })
            .collect();
        Some(Self {
            raw: line.to_owned(),
            name: name.to_owned(),
            value: value.to_owned(),
            attributes,
        })
    }

    /// The last attribute of this name, in any letter case: `Some(None)` for
    /// a flag, `Some(Some(value))` for a value.
    pub fn attribute(&self, name: &str) -> Option<Option<&str>> {
        self.attributes
            .iter()
            .rev()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_deref())
    }

    pub fn has(&self, name: &str) -> bool {
        self.attribute(name).is_some()
    }

    /// The value of the last attribute of this name, if it has one.
    pub fn value_of(&self, name: &str) -> Option<&str> {
        self.attribute(name).flatten()
    }

    pub fn max_age(&self) -> Option<i64> {
        self.value_of("max-age").and_then(|v| v.parse().ok())
    }

    pub fn expires(&self) -> Option<SystemTime> {
        self.value_of("expires").and_then(parse_cookie_date)
    }

    /// When the cookie ends, as a browser reads it: `Max-Age` first, then
    /// `Expires`; `None` for a cookie that lasts the browser's session.
    pub fn ends_at(&self, now: SystemTime) -> Option<SystemTime> {
        match self.max_age() {
            Some(age) if age <= 0 => Some(UNIX_EPOCH),
            Some(age) => Some(now + Duration::from_secs(u64::try_from(age).unwrap_or(0))),
            None => self.expires(),
        }
    }

    /// Whether this line removes the cookie (its time is already past).
    pub fn clears(&self) -> bool {
        let now = SystemTime::now();
        self.ends_at(now).is_some_and(|end| end <= now)
    }
}

/// A cookie kept in the jar.
#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The host that set it (host-only), or its `Domain`.
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
    pub expires: Option<SystemTime>,
    created: u64,
}

/// The cookies of one browser.
#[derive(Clone, Debug, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    rejected: Vec<String>,
    next: u64,
}

impl Jar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one `Set-Cookie` line received from `url`; an error says why a
    /// browser would refuse it (also kept in [`Jar::rejected`]).
    pub fn store(&mut self, url: &Url, line: &str) -> Result<(), String> {
        let stored = self.try_store(url, line);
        if let Err(why) = &stored {
            self.rejected.push(format!("{line:?} from {url}: {why}"));
        }
        stored
    }

    fn try_store(&mut self, url: &Url, line: &str) -> Result<(), String> {
        let parsed = SetCookie::parse(line).ok_or("not a cookie")?;
        let host = host_of(url);
        let now = SystemTime::now();
        let expires = parsed.ends_at(now);
        let (domain, host_only) = match parsed
            .value_of("domain")
            .map(|d| d.trim_start_matches('.').to_ascii_lowercase())
            .filter(|d| !d.is_empty())
        {
            None => (host, true),
            Some(domain) => {
                if !domain.contains('.') {
                    return Err(format!("Domain={domain} is a public suffix"));
                }
                if !domain_matches(&host, &domain) {
                    return Err(format!("Domain={domain} does not cover the host {host}"));
                }
                (domain, false)
            }
        };
        let path = parsed
            .value_of("path")
            .filter(|p| p.starts_with('/'))
            .map_or_else(|| default_path(url), str::to_owned);
        let secure = parsed.has("secure");
        if secure && url.scheme() != "https" {
            return Err("Secure over plain http".to_owned());
        }
        let lower = parsed.name.to_ascii_lowercase();
        if lower.starts_with("__secure-") && !secure {
            return Err("a __Secure- cookie without Secure".to_owned());
        }
        if lower.starts_with("__host-") && (!secure || !host_only || path != "/") {
            return Err("a __Host- cookie needs Secure, no Domain and Path=/".to_owned());
        }
        let same = |c: &Cookie| {
            c.name == parsed.name
                && c.domain == domain
                && c.host_only == host_only
                && c.path == path
        };
        let created = match self.cookies.iter().position(same) {
            Some(at) => self.cookies.remove(at).created,
            None => {
                self.next += 1;
                self.next
            }
        };
        if expires.is_some_and(|end| end <= now) {
            return Ok(());
        }
        self.cookies.push(Cookie {
            name: parsed.name.clone(),
            value: parsed.value.clone(),
            domain,
            host_only,
            path,
            secure,
            http_only: parsed.has("httponly"),
            same_site: parsed.value_of("samesite").map(str::to_owned),
            expires,
            created,
        });
        Ok(())
    }

    /// The `Cookie` header a browser sends to `url`, or `None` when it
    /// sends none.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let host = host_of(url);
        let now = SystemTime::now();
        let mut sent: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                if c.host_only {
                    c.domain == host
                } else {
                    domain_matches(&host, &c.domain)
                }
            })
            .filter(|c| path_matches(url.path(), &c.path))
            .filter(|c| !c.secure || url.scheme() == "https")
            .filter(|c| c.expires.is_none_or(|end| end > now))
            .collect();
        sent.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.created.cmp(&b.created))
        });
        if sent.is_empty() {
            return None;
        }
        Some(
            sent.iter()
                .map(|c| {
                    if c.name.is_empty() {
                        c.value.clone()
                    } else {
                        format!("{}={}", c.name, c.value)
                    }
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// The cookie of this name kept for exactly this host (host-only), if
    /// any and not expired.
    pub fn get(&self, host: &str, name: &str) -> Option<&Cookie> {
        let now = SystemTime::now();
        self.cookies.iter().find(|c| {
            c.host_only
                && c.domain.eq_ignore_ascii_case(host)
                && c.name == name
                && c.expires.is_none_or(|end| end > now)
        })
    }

    /// Every cookie kept, expired ones included.
    pub fn all(&self) -> &[Cookie] {
        &self.cookies
    }

    /// Puts a host-only cookie with `Path=/` in the jar by hand.
    pub fn insert(&mut self, host: &str, name: &str, value: &str) {
        let host = host.to_ascii_lowercase();
        self.cookies
            .retain(|c| !(c.host_only && c.domain == host && c.name == name && c.path == "/"));
        self.next += 1;
        self.cookies.push(Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: host,
            host_only: true,
            path: "/".to_owned(),
            secure: false,
            http_only: false,
            same_site: None,
            expires: None,
            created: self.next,
        });
    }

    /// Removes every cookie of this name kept for this host.
    pub fn remove(&mut self, host: &str, name: &str) {
        self.cookies
            .retain(|c| !(c.domain.eq_ignore_ascii_case(host) && c.name == name));
    }

    /// Every `Set-Cookie` line refused so far, and why.
    pub fn rejected(&self) -> &[String] {
        &self.rejected
    }
}

/// The URL's host in lower case, without brackets for IPv6.
fn host_of(url: &Url) -> String {
    match url.host() {
        Some(Host::Ipv6(ip)) => ip.to_string(),
        Some(host) => host.to_string().to_ascii_lowercase(),
        None => String::new(),
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.parse::<IpAddr>().is_err()
            && host
                .strip_suffix(domain)
                .is_some_and(|rest| rest.ends_with('.')))
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/')
                || request
                    .get(cookie.len()..)
                    .is_some_and(|rest| rest.starts_with('/'))))
}

fn default_path(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(at) => path.get(..at).unwrap_or("/").to_owned(),
    }
}

/// A cookie date as RFC 6265 section 5.1.1 reads it.
pub fn parse_cookie_date(text: &str) -> Option<SystemTime> {
    let tokens = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == ':'))
        .filter(|t| !t.is_empty());
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in tokens {
        if time.is_none() {
            let parts: Vec<&str> = token.split(':').collect();
            if let [h, m, s] = parts.as_slice()
                && let (Ok(h), Ok(m), Ok(s)) =
                    (h.parse::<u64>(), m.parse::<u64>(), s.parse::<u64>())
            {
                time = Some((h, m, s));
                continue;
            }
        }
        if day.is_none() && token.len() <= 2 && token.chars().all(|c| c.is_ascii_digit()) {
            day = token.parse::<u64>().ok();
            continue;
        }
        if month.is_none() && token.len() >= 3 {
            let names = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ];
            let start = token.get(..3).unwrap_or_default().to_ascii_lowercase();
            if let Some(at) = names.iter().position(|n| *n == start) {
                month = u64::try_from(at).ok().map(|m| m + 1);
                continue;
            }
        }
        if year.is_none()
            && (2..=4).contains(&token.len())
            && token.chars().all(|c| c.is_ascii_digit())
        {
            year = token.parse::<i64>().ok().map(|y| match y {
                70..=99 => y + 1900,
                0..=69 => y + 2000,
                _ => y,
            });
        }
    }
    let ((h, mi, s), d, mo, y) = (time?, day?, month?, year?);
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + i64::try_from(h * 3600 + mi * 60 + s).ok()?;
    if secs < 0 {
        return Some(UNIX_EPOCH);
    }
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let month = i64::try_from(month).unwrap_or(1);
    let day = i64::try_from(day).unwrap_or(1);
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// How two URLs relate, as `Sec-Fetch-Site` names it.
pub fn relation(from: &Url, to: &Url) -> &'static str {
    if from.origin() == to.origin() {
        return "same-origin";
    }
    if from.scheme() == to.scheme() && site_of(from) == site_of(to) {
        return "same-site";
    }
    "cross-site"
}

/// The registrable domain the tests' hosts have: the last two labels of a
/// name (`vm.test`), or the whole address of an IP.
fn site_of(url: &Url) -> String {
    let host = host_of(url);
    if host.parse::<IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    labels
        .get(labels.len().saturating_sub(2)..)
        .unwrap_or_default()
        .join(".")
}

/// The origin of a URL as `Origin` carries it.
pub fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The path and query of a URL, as a request target.
pub fn target_of(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

/// Parses a URL the test wrote, failing the test if it is not one.
pub fn url(text: &str) -> Url {
    Url::parse(text)
        .unwrap_or_else(|e| panic!("{:?} is not a URL: {e}", crate::http::mask_values(text)))
}

/// The value of a query parameter of a URL.
pub fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.into_owned())
}

/// One step of a navigation: the address asked for and the answer.
#[derive(Clone, Debug)]
pub struct Step {
    pub url: Url,
    pub response: Response,
}

impl Step {
    /// Whether the answer is a redirect with a `Location`.
    pub fn is_redirect(&self) -> bool {
        matches!(self.response.status, 301 | 302 | 303 | 307 | 308)
            && self.response.header("location").is_some()
    }

    /// The redirect's target, resolved against this step's address.
    pub fn location(&self) -> Url {
        let location = self.response.header("location").unwrap_or_else(|| {
            panic!(
                "expected a Location on the answer to {}\n{}",
                crate::http::mask_values(self.url.as_str()),
                self.response.describe()
            )
        });
        self.url.join(location).unwrap_or_else(|e| {
            panic!(
                "the Location {:?} on the answer to {} is not a URL: {e}",
                crate::http::mask_values(location),
                crate::http::mask_values(self.url.as_str())
            )
        })
    }

    /// The `Set-Cookie` lines of the answer, parsed.
    pub fn set_cookies(&self) -> Vec<SetCookie> {
        set_cookies(&self.response)
    }
}

/// The `Set-Cookie` lines of an answer, parsed (unparseable ones left out).
pub fn set_cookies(response: &Response) -> Vec<SetCookie> {
    response
        .headers_named("set-cookie")
        .into_iter()
        .filter_map(SetCookie::parse)
        .collect()
}

/// A browser: a jar and a way to the program and the provider.
#[derive(Clone, Debug)]
pub struct Browser {
    program: SocketAddr,
    pub jar: Jar,
    /// The authorities it trusts for an `https` address (none unless a
    /// test adds one: no public authority is built in).
    trusted: Vec<rustls::pki_types::CertificateDer<'static>>,
    /// Names reached at an address of their own (the install tests'
    /// identity provider on the Docker bridge's address), whatever else
    /// they would be: a host file of this browser's.
    names: Vec<(String, IpAddr)>,
    /// Whether an `https` address of the installation goes to the program's
    /// address over TLS (an installation behind the Compose file's proxy),
    /// rather than in plain HTTP (the program itself).
    installation_over_tls: bool,
}

impl Browser {
    /// An empty browser that reaches names at the program's listening
    /// address.
    pub fn new(program: SocketAddr) -> Self {
        Self {
            program,
            jar: Jar::new(),
            trusted: Vec::new(),
            names: Vec::new(),
            installation_over_tls: false,
        }
    }

    /// A browser for an installation behind what ends TLS on `address`
    /// (the Compose file's proxy, or a front of the test's): an `https`
    /// address of the installation goes there over TLS, asking for the
    /// address's host as the server name and trusting `authority` (the
    /// wildcard certificate's), as a person's browser does.
    pub fn behind_tls(
        address: SocketAddr,
        authority: rustls::pki_types::CertificateDer<'static>,
    ) -> Self {
        let mut browser = Self::new(address);
        browser.installation_over_tls = true;
        browser.trust(authority);
        browser
    }

    /// Reaches `host` at `ip` (with the URL's port) from now on, whatever
    /// else it would be: one line of a host file.
    pub fn resolve(&mut self, host: &str, ip: IpAddr) {
        let host = host.to_ascii_lowercase();
        self.names.retain(|(n, _)| *n != host);
        self.names.push((host, ip));
    }

    /// Trusts this authority for `https` addresses from now on (the
    /// harness's identity provider behind TLS).
    pub fn trust(&mut self, authority: rustls::pki_types::CertificateDer<'static>) {
        self.trusted.push(authority);
    }

    /// Reaches names at this address from now on: the program started
    /// again, the cookies kept.
    pub fn reach(&mut self, program: SocketAddr) {
        self.program = program;
    }

    /// Signs this browser in at the provider as the person with this `sub`
    /// (the provider's own session, [`PERSON_COOKIE`]).
    pub fn sign_in_at_provider(&mut self, provider: &Provider, sub: &str) {
        // On the issuer's host: `127.0.0.1` for a plain provider,
        // `localhost` for one behind TLS.
        let host = Url::parse(provider.issuer())
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| provider.address().ip().to_string());
        self.jar.insert(&host, PERSON_COOKIE, sub);
    }

    fn address_for(&self, url: &Url) -> SocketAddr {
        let port = url.port_or_known_default().unwrap_or(80);
        if let Some(Host::Domain(name)) = url.host()
            && let Some((_, ip)) = self
                .names
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            return SocketAddr::new(*ip, port);
        }
        match url.host() {
            Some(Host::Ipv4(ip)) => SocketAddr::new(IpAddr::V4(ip), port),
            Some(Host::Ipv6(ip)) => SocketAddr::new(IpAddr::V6(ip), port),
            // The harness's identity provider behind TLS.
            Some(Host::Domain("localhost")) => SocketAddr::from(([127, 0, 0, 1], port)),
            _ => self.program,
        }
    }

    /// A request to `url` with `Host` from it and this browser's cookies,
    /// nothing else: the test adds what it needs and sends it with
    /// [`Browser::send`].
    pub fn request(&self, method: &str, url: &Url) -> Request {
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let request = Request::new(self.address_for(url), &target_of(url), &host)
            .method(method)
            .timeout(STEP_TIMEOUT);
        // An `https` address of the installation goes to the program in
        // plain HTTP, as before (it listens behind what ends TLS), unless
        // this browser is [`Browser::behind_tls`]; one elsewhere (the
        // identity provider behind its TLS front) over TLS.
        let to_program = self.address_for(url) == self.program;
        let request = if url.scheme() == "https" && (!to_program || self.installation_over_tls) {
            request.over_tls(url.host_str().unwrap_or_default(), &self.trusted)
        } else {
            request
        };
        match self.jar.header_for(url) {
            Some(cookies) => request.header("cookie", cookies),
            None => request,
        }
    }

    /// Sends a request made for `url` and keeps the cookies it sets.
    pub fn send(&mut self, url: &Url, request: &Request) -> Response {
        let response = request.send();
        self.keep_cookies(url, &response);
        response
    }

    /// As [`Browser::send`], an error when no answer came.
    pub fn try_send(&mut self, url: &Url, request: &Request) -> Result<Response, String> {
        let response = request.try_send()?;
        self.keep_cookies(url, &response);
        Ok(response)
    }

    fn keep_cookies(&mut self, url: &Url, response: &Response) {
        for line in response.headers_named("set-cookie") {
            drop(self.jar.store(url, line));
        }
    }

    /// The request of a top-level navigation to `url`, typed by the person:
    /// what a browser sends.
    pub fn navigation(&self, url: &Url) -> Request {
        self.request("GET", url)
            .header("accept", NAVIGATION_ACCEPT)
            .header("sec-fetch-site", "none")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-user", "?1")
    }

    /// One step of a navigation typed by the person.
    pub fn open(&mut self, url: &Url) -> Step {
        let request = self.navigation(url);
        let response = self.send(url, &request);
        Step {
            url: url.clone(),
            response,
        }
    }

    /// As [`Browser::open`], an error when no answer came.
    pub fn try_open(&mut self, url: &Url) -> Result<Step, String> {
        let request = self.navigation(url);
        let response = self.try_send(url, &request)?;
        Ok(Step {
            url: url.clone(),
            response,
        })
    }

    /// The next step after a redirect.
    pub fn follow(&mut self, step: &Step) -> Step {
        self.open(&step.location())
    }

    /// Opens `url` and follows every redirect, returning each step; the
    /// last is the one that is not a redirect.
    pub fn navigate(&mut self, url: &Url) -> Vec<Step> {
        let mut steps = vec![self.open(url)];
        while let Some(last) = steps.last().filter(|s| s.is_redirect()) {
            assert!(
                steps.len() <= MAX_REDIRECTS,
                "more than {MAX_REDIRECTS} redirects from {url}:\n{}",
                describe_steps(&steps)
            );
            let next = self.follow(last);
            steps.push(next);
        }
        steps
    }

    /// A request a script on the page at `page` makes to `url` with
    /// `fetch`: `Origin` (for a method other than `GET` and `HEAD`, or a
    /// cross-origin `GET`), `Sec-Fetch-Site` as the two relate,
    /// `Sec-Fetch-Mode: cors`, `Sec-Fetch-Dest: empty`.
    pub fn fetch_from(&self, page: &Url, method: &str, url: &Url) -> Request {
        let relation = relation(page, url);
        let mut request = self
            .request(method, url)
            .header("accept", "*/*")
            .header("sec-fetch-site", relation)
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-dest", "empty");
        if !matches!(method, "GET" | "HEAD") || relation != "same-origin" {
            request = request.header("origin", origin_of(page));
        }
        request
    }

    /// A form of a page of the platform's (or any page this browser read)
    /// submitted as a browser submits it: the page at `page`, answered with
    /// `page_response`, holds the form; it is posted to `action` with
    /// `Origin` as [`form_origin`] says from the page's referrer policy,
    /// `Sec-Fetch-Site` as the two relate (`same-origin` for a form of the
    /// page's own address), `Sec-Fetch-Mode: navigate`, `Sec-Fetch-Dest:
    /// document`, `Sec-Fetch-User: ?1`, and this
    /// `application/x-www-form-urlencoded` body. Every harness that posts a
    /// form it read from a page posts it so; [`Browser::form_from`] is for
    /// a made-up page.
    pub fn submit_form(
        &self,
        page: &Url,
        page_response: &Response,
        action: &Url,
        body: &str,
    ) -> Request {
        submitted(
            self.request("POST", action)
                .header("accept", NAVIGATION_ACCEPT),
            &form_origin(&origin_of(page), page_response),
            relation(page, action),
            body,
        )
    }

    /// A form on the page at `page` posted to `url`: a navigation with
    /// `Origin`, `Sec-Fetch-Site` as the two relate, and this
    /// `application/x-www-form-urlencoded` body.
    pub fn form_from(&self, page: &Url, url: &Url, body: &str) -> Request {
        self.request("POST", url)
            .header("accept", NAVIGATION_ACCEPT)
            .header("origin", origin_of(page))
            .header("sec-fetch-site", relation(page, url))
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
    }
}

/// `request` with what a browser adds to a form it submits (its `Accept`
/// aside): `Origin`, the `Sec-Fetch-*` of a navigation the person made, and the
/// body as `application/x-www-form-urlencoded`.
pub fn submitted(request: Request, origin: &str, site: &str, body: &str) -> Request {
    request
        .header("origin", origin)
        .header("sec-fetch-site", site)
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-user", "?1")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
}

/// The referrer policies a browser knows, by the names of the header.
const REFERRER_POLICIES: [&str; 8] = [
    "no-referrer",
    "no-referrer-when-downgrade",
    "same-origin",
    "origin",
    "strict-origin",
    "origin-when-cross-origin",
    "strict-origin-when-cross-origin",
    "unsafe-url",
];

/// The `Origin` a browser sends with a form of a page whose origin is
/// `page_origin` and which was answered with `page`: `null` when the
/// page's referrer policy is `no-referrer` (as Firefox and Chromium do),
/// else the page's origin. The policy is the last one the browser knows
/// among the values of `referrer-policy`.
pub fn form_origin(page_origin: &str, page: &Response) -> String {
    let policy = page
        .headers_named("referrer-policy")
        .iter()
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .rfind(|t| REFERRER_POLICIES.contains(&t.as_str()));
    if policy.as_deref() == Some("no-referrer") {
        "null".to_owned()
    } else {
        page_origin.to_owned()
    }
}

/// Every step of a navigation, for failure messages.
pub fn describe_steps(steps: &[Step]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "step {}: GET {}\n{}\n",
                i + 1,
                crate::http::mask_values(s.url.as_str()),
                s.response.describe()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Url {
        url(text)
    }

    #[test]
    fn a_cookie_without_domain_goes_back_to_its_host_only() {
        let mut jar = Jar::new();
        let one = at("http://one.vm.test:8080/x");
        jar.store(&one, "vm-session=abc; Path=/; HttpOnly; SameSite=Lax")
            .unwrap();
        assert_eq!(
            jar.header_for(&at("http://one.vm.test:9/y")).as_deref(),
            Some("vm-session=abc")
        );
        assert_eq!(jar.header_for(&at("http://two.vm.test:8080/")), None);
        assert_eq!(jar.header_for(&at("http://vm.test:8080/")), None);
        assert_eq!(jar.header_for(&at("http://sub.one.vm.test:8080/")), None);
        let kept = jar.get("one.vm.test", "vm-session").unwrap();
        assert!(kept.http_only && kept.host_only);
        assert_eq!(kept.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn a_cookie_with_domain_spans_the_names_under_it() {
        let mut jar = Jar::new();
        jar.store(
            &at("http://one.vm.test/"),
            "wide=1; Domain=.vm.test; Path=/",
        )
        .unwrap();
        assert_eq!(
            jar.header_for(&at("http://two.vm.test/")).as_deref(),
            Some("wide=1")
        );
        assert!(
            jar.store(&at("http://one.vm.test/"), "x=1; Domain=other.test")
                .is_err()
        );
        assert!(
            jar.store(&at("http://one.vm.test/"), "x=1; Domain=test")
                .is_err()
        );
    }

    #[test]
    fn secure_and_prefixed_cookies_are_refused_over_http() {
        let mut jar = Jar::new();
        let page = at("http://one.vm.test/");
        assert!(jar.store(&page, "s=1; Secure").is_err());
        assert!(
            jar.store(&page, "__Host-vm-session=1; Path=/; HttpOnly")
                .is_err()
        );
        assert!(jar.store(&page, "__Secure-x=1").is_err());
        assert_eq!(jar.rejected().len(), 3);
        let secure_page = at("https://one.vm.test/");
        jar.store(&secure_page, "__Host-vm-session=1; Path=/; Secure")
            .unwrap();
        assert!(
            jar.store(&secure_page, "__Host-x=1; Path=/x; Secure")
                .is_err()
        );
        assert_eq!(jar.header_for(&page), None);
        assert_eq!(
            jar.header_for(&secure_page).as_deref(),
            Some("__Host-vm-session=1")
        );
    }

    #[test]
    fn max_age_zero_and_a_past_expires_remove_a_cookie() {
        let mut jar = Jar::new();
        let page = at("http://one.vm.test/");
        jar.store(&page, "a=1; Path=/").unwrap();
        jar.store(&page, "b=2; Path=/").unwrap();
        jar.store(&page, "a=; Path=/; Max-Age=0").unwrap();
        jar.store(&page, "b=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT")
            .unwrap();
        assert_eq!(jar.header_for(&page), None);
    }

    #[test]
    fn paths_match_as_rfc_6265_says() {
        let mut jar = Jar::new();
        jar.store(&at("http://one.vm.test/a/b"), "d=1").unwrap();
        jar.store(&at("http://one.vm.test/"), "p=2; Path=/x")
            .unwrap();
        assert_eq!(
            jar.header_for(&at("http://one.vm.test/a/c")).as_deref(),
            Some("d=1")
        );
        assert_eq!(jar.header_for(&at("http://one.vm.test/ab")), None);
        assert_eq!(
            jar.header_for(&at("http://one.vm.test/x/y")).as_deref(),
            Some("p=2")
        );
        assert_eq!(jar.header_for(&at("http://one.vm.test/xy")), None);
    }

    #[test]
    fn cookie_dates_are_read() {
        let date = parse_cookie_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(
            date.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_445_412_480
        );
        let dashed = parse_cookie_date("Wednesday, 21-Oct-15 07:28:00 GMT").unwrap();
        assert_eq!(date, dashed);
    }

    #[test]
    fn a_form_of_a_no_referrer_page_is_sent_with_origin_null() {
        let answer = |policy: &str| {
            Response::parse_for_tests(&format!(
                "HTTP/1.1 200 OK\r\n{policy}Content-Length: 0\r\n\r\n"
            ))
        };
        let origin = "http://dashboard.vm.test:8080";
        assert_eq!(
            form_origin(origin, &answer("Referrer-Policy: no-referrer\r\n")),
            "null"
        );
        assert_eq!(
            form_origin(origin, &answer("referrer-policy: same-origin\r\n")),
            origin
        );
        assert_eq!(form_origin(origin, &answer("")), origin);
        assert_eq!(
            form_origin(origin, &answer("referrer-policy: no-referrer, bogus\r\n")),
            "null"
        );
        assert_eq!(
            form_origin(
                origin,
                &answer("referrer-policy: no-referrer, strict-origin\r\n")
            ),
            origin
        );
    }

    #[test]
    fn sites_and_origins_relate_as_fetch_says() {
        let one = at("http://one.vm.test:8080/");
        assert_eq!(
            relation(&one, &at("http://one.vm.test:8080/x")),
            "same-origin"
        );
        assert_eq!(relation(&one, &at("http://two.vm.test:8080/")), "same-site");
        assert_eq!(relation(&one, &at("http://127.0.0.1:9/")), "cross-site");
        assert_eq!(relation(&one, &at("https://one.vm.test/")), "cross-site");
        assert_eq!(origin_of(&one), "http://one.vm.test:8080");
    }
}
