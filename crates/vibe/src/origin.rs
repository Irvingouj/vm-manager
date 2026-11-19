//! How a browser reaches the installation, and the rule that refuses a
//! request from another origin (blackbox/CONTRACT.md, "Sign-in and
//! sessions: Starting with OIDC" on `--public-scheme` and `--public-port`,
//! and "Whose request it is").
//!
//! [`Public`] writes the addresses the platform puts into redirects and
//! compares an `Origin` with: `<scheme>://<name>.<domain>[:<port>]`, the
//! port left out when it is the scheme's own. [`from_another_origin`] is
//! the rule, the same whatever the sign-in: an Application's page cannot
//! call another Application as the same User, post a form to it, or show
//! it in a frame.

use hyper::header::{HeaderName, ORIGIN};
use hyper::{HeaderMap, Method};

/// `https` or `http`, as `--public-scheme` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scheme {
    Https,
    Http,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }

    /// The port a browser leaves out of an address of this scheme.
    fn default_port(self) -> u16 {
        match self {
            Self::Https => 443,
            Self::Http => 80,
        }
    }
}

/// Where the port of the addresses comes from.
#[derive(Clone, Copy, Debug)]
enum Port {
    /// The installation's: `--public-port`, or what it stands for.
    Fixed(u16),
    /// The one the request's `Host` names (the local run's, whose own
    /// front has checked the `Host` already).
    OfTheHost,
}

/// How a browser reaches the installation.
#[derive(Clone, Copy, Debug)]
pub struct Public {
    scheme: Scheme,
    port: Port,
}

impl Public {
    /// `vm serve`'s: the scheme, and the port (`--public-port`, else the
    /// listening port with `http`, else `443`).
    pub fn new(scheme: Scheme, port: u16) -> Self {
        Self {
            scheme,
            port: Port::Fixed(port),
        }
    }

    /// The local run's: `http`, at the port the browser used.
    pub fn local() -> Self {
        Self {
            scheme: Scheme::Http,
            port: Port::OfTheHost,
        }
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Whether cookies are `Secure` and `__Host-` prefixed.
    pub fn secure(&self) -> bool {
        self.scheme == Scheme::Https
    }

    /// `<scheme>://<host>[:<port>]`: the origin of the name `host` (an
    /// Application's `<name>.<domain>`, or `auth.<domain>`), for a request
    /// whose `Host` named `host_port` as its port.
    pub fn origin(&self, host: &str, host_port: Option<u16>) -> String {
        let port = match self.port {
            Port::Fixed(port) => Some(port),
            Port::OfTheHost => host_port,
        };
        let scheme = self.scheme.as_str();
        match port.filter(|p| *p != self.scheme.default_port()) {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        }
    }
}

/// The one value of a header, as text; `None` when absent. A header given
/// more than once reads as a value nothing equals.
fn one<'h>(headers: &'h HeaderMap, name: &HeaderName) -> Option<&'h str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return Some("\u{0}given more than once");
    }
    Some(first.to_str().unwrap_or("\u{0}not text"))
}

const SEC_FETCH_SITE: &str = "sec-fetch-site";
const SEC_FETCH_MODE: &str = "sec-fetch-mode";
const SEC_FETCH_DEST: &str = "sec-fetch-dest";

/// Whether the request comes from another origin than `own` and is
/// refused (blackbox/CONTRACT.md, "Whose request it is"):
///
/// - a method other than `GET`, `HEAD` and `OPTIONS`: with an `Origin`, it
///   decides; without one, a `Sec-Fetch-Site` other than `same-origin` or
///   `none`;
/// - a `GET` or `HEAD`: an `Origin` that is not `own`, or a `Sec-Fetch-Site`
///   of `cross-site` or `same-site` unless it is a navigation of the whole
///   page (`Sec-Fetch-Mode: navigate`, `Sec-Fetch-Dest: document`, no
///   `Origin`);
/// - an `OPTIONS`: an `Origin` that is not `own`.
///
/// **An `Origin` of `null` together with `Sec-Fetch-Site: same-origin` is
/// `own`**, in all three: it is what a browser sends with a form posted
/// from a page of this very origin whose referrer policy is `no-referrer`.
/// So in the `GET` and `HEAD` branch it is taken as any request of the
/// own origin is (`same-origin` is no other site; the navigation exception,
/// which asks for no `Origin`, is not needed for it). `null` with any other
/// `Sec-Fetch-Site`, or with none, stays another origin. `Sec-Fetch-Site`
/// can be trusted for this, and only for this: a browser sets it itself (no
/// page can), and says `same-origin` only when the page that made the
/// request, and every redirect on the way, is of this very origin; a page
/// with no origin (a sandboxed frame, a `data:` address) is `cross-site`.
/// It tells nothing of a request that is no browser's, which may say
/// anything, and such a request has no session of a browser to borrow.
///
/// A request with neither header is no browser's, and is taken.
pub fn from_another_origin(method: &Method, headers: &HeaderMap, own: &str) -> bool {
    let origin = one(headers, &ORIGIN);
    let site = one(headers, &HeaderName::from_static(SEC_FETCH_SITE));
    let is = |value: Option<&str>, wanted: &str| {
        value.is_some_and(|v| v.trim().eq_ignore_ascii_case(wanted))
    };
    let null_of_own = is(origin, "null") && is(site, "same-origin");
    let foreign = !null_of_own && origin.is_some_and(|o| !o.trim().eq_ignore_ascii_case(own));
    if *method == Method::OPTIONS {
        return foreign;
    }
    if *method == Method::GET || *method == Method::HEAD {
        if foreign {
            return true;
        }
        let other_site = is(site, "cross-site") || is(site, "same-site");
        if !other_site {
            return false;
        }
        let navigation = origin.is_none()
            && is(
                one(headers, &HeaderName::from_static(SEC_FETCH_MODE)),
                "navigate",
            )
            && is(
                one(headers, &HeaderName::from_static(SEC_FETCH_DEST)),
                "document",
            );
        return !navigation;
    }
    if origin.is_some() {
        return foreign;
    }
    match site {
        Some(_) => !(is(site, "same-origin") || is(site, "none")),
        None => false,
    }
}
