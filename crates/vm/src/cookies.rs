//! The platform's cookies (blackbox/CONTRACT.md, "Sign-in and sessions: The
//! cookies"): their names, finding one in a request, taking them out of the
//! `Cookie` header before a request reaches Application code, and dropping
//! a `Set-Cookie` of an Application's that names one of them or has a
//! `Domain` attribute.
//!
//! Names are matched exactly, letter case included. What is taken out and
//! what is dropped is the same whatever the sign-in, so that Application
//! code never sees nor sets them.

use hyper::HeaderMap;
use hyper::header::{COOKIE, HeaderValue};

/// The session cookie over `http`.
pub const SESSION: &str = "vm-session";
/// The session cookie over `https`.
pub const SESSION_SECURE: &str = "__Host-vm-session";
/// The cookie of a sign-in under way over `http`.
pub const SIGN_IN: &str = "vm-sign-in";
/// The cookie of a sign-in under way over `https`.
pub const SIGN_IN_SECURE: &str = "__Host-vm-sign-in";

/// Every name of the platform's, which Application code never sees nor sets.
pub const PLATFORM_NAMES: [&str; 4] = [SESSION, SESSION_SECURE, SIGN_IN, SIGN_IN_SECURE];

/// The session cookie's name with or without `Secure`.
pub fn session_name(secure: bool) -> &'static str {
    if secure { SESSION_SECURE } else { SESSION }
}

/// The sign-in cookie's name with or without `Secure`.
pub fn sign_in_name(secure: bool) -> &'static str {
    if secure { SIGN_IN_SECURE } else { SIGN_IN }
}

/// The name and the value of one `name=value` pair of a `Cookie` header,
/// spaces around each trimmed; a pair without `=` is a nameless cookie.
fn pair(part: &[u8]) -> (&[u8], &[u8]) {
    let part = part.trim_ascii();
    match part.iter().position(|b| *b == b'=') {
        Some(at) => {
            let (name, value) = part.split_at(at);
            (
                name.trim_ascii(),
                value.get(1..).unwrap_or_default().trim_ascii(),
            )
        }
        None => (&[], part),
    }
}

/// The value of the first cookie named exactly `name` in the request's
/// `Cookie` headers, when it is text.
pub fn find<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(COOKIE)
        .iter()
        .flat_map(|value| value.as_bytes().split(|b| *b == b';'))
        .map(pair)
        .find(|(n, _)| *n == name.as_bytes())
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
}

fn is_platform(name: &[u8]) -> bool {
    PLATFORM_NAMES.iter().any(|n| n.as_bytes() == name)
}

/// Takes the platform's cookies out of the `Cookie` header; the others stay
/// byte for byte, in their order, joined by `; `. A header left empty is
/// left out. Nothing changes when there is none of the platform's.
pub fn remove_platform_cookies(headers: &mut HeaderMap) {
    let holds = headers.get_all(COOKIE).iter().any(|value| {
        value
            .as_bytes()
            .split(|b| *b == b';')
            .any(|part| is_platform(pair(part).0))
    });
    if !holds {
        return;
    }
    // Rebuilt only when there is one to take out: the other pairs, copied
    // once into one header.
    let mut kept: Vec<u8> = Vec::new();
    for value in headers.get_all(COOKIE) {
        for part in value.as_bytes().split(|b| *b == b';') {
            let trimmed = part.trim_ascii();
            if trimmed.is_empty() || is_platform(pair(part).0) {
                continue;
            }
            if !kept.is_empty() {
                kept.extend_from_slice(b"; ");
            }
            kept.extend_from_slice(trimmed);
        }
    }
    headers.remove(COOKIE);
    if kept.is_empty() {
        return;
    }
    if let Ok(value) = HeaderValue::from_bytes(&kept) {
        headers.insert(COOKIE, value);
    }
}

/// Why a `Set-Cookie` of an Application's is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// It names one of the platform's cookies.
    PlatformName,
    /// It has a `Domain` attribute: it would be sent to the other
    /// Applications.
    Domain,
}

impl Dropped {
    pub fn reason(self) -> &'static str {
        match self {
            Self::PlatformName => "platform-name",
            Self::Domain => "domain",
        }
    }
}

/// Whether an Application's `Set-Cookie` value is dropped, and why. A
/// nameless cookie (no `=` before the first `;`, or an empty name) is sent
/// back by browsers as its bare value, so it is read as the pair its value
/// would make.
pub fn judge_set_cookie(value: &[u8]) -> Option<Dropped> {
    let mut parts = value.split(|b| *b == b';');
    let first = parts.next().unwrap_or_default();
    let (name, rest) = pair(first);
    let named = if name.is_empty() { pair(rest).0 } else { name };
    if is_platform(named) {
        return Some(Dropped::PlatformName);
    }
    let domain = parts.any(|attribute| {
        let name = pair(attribute).0;
        let name = if name.is_empty() {
            attribute.trim_ascii()
        } else {
            name
        };
        name.eq_ignore_ascii_case(b"domain")
    });
    domain.then_some(Dropped::Domain)
}

/// A `Set-Cookie` value of the platform's: `HttpOnly; SameSite=Lax; Path=/`,
/// no `Domain`, `Secure` when `secure`, living `max_age` seconds (`0`
/// clears it).
pub fn set_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> Option<HeaderValue> {
    let secure = if secure { "; Secure" } else { "" };
    let text = format!("{name}={value}; Max-Age={max_age}; Path=/; HttpOnly; SameSite=Lax{secure}");
    let mut header = HeaderValue::from_str(&text).ok()?;
    header.set_sensitive(true);
    Some(header)
}
