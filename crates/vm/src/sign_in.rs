//! Who is this request's User.
//!
//! Ingress asks a [`SignIn`] for the User of every request before it looks
//! the Application up. Real sessions (OIDC, a cookie per Application's
//! subdomain) are a later ticket and fill the same seam; the Request runner
//! never sees how a User was established.
//!
//! - [`NoSignIn`]: nobody is signed in. Used until real sessions exist,
//!   unless the stand-in is switched on.
//! - [`StandInSignIn`]: development only. The caller states the User and
//!   their Groups in two headers.
//!
//! Whatever the caller uses to state the User never reaches Application
//! code: Ingress removes every header under [`RESERVED_HEADER_PREFIX`],
//! whichever sign-in is in use ([`remove_reserved_headers`]).

use hyper::HeaderMap;
use hyper::header::HeaderValue;

use crate::user::User;

/// Every request header whose name starts with this belongs to the
/// platform, and is removed before the request reaches Application code.
pub const RESERVED_HEADER_PREFIX: &str = "x-vm-";

/// The stand-in sign-in's header for the User's id.
pub const STAND_IN_USER_HEADER: &str = "x-vm-stand-in-user";

/// The stand-in sign-in's header for the User's Groups, comma-separated.
pub const STAND_IN_GROUPS_HEADER: &str = "x-vm-stand-in-groups";

/// The longest stand-in header value accepted, in bytes.
pub const MAX_STAND_IN_VALUE_BYTES: usize = 256;

/// What a sign-in method says about a request.
#[derive(Debug, PartialEq, Eq)]
pub enum SignedIn {
    User(User),
    /// No User: the request is refused before any Application code runs.
    NotSignedIn,
    /// The caller tried to sign in, but what it sent makes no sense. The
    /// reason is for the log; it never echoes what was sent.
    Malformed(&'static str),
}

/// A way of establishing the User of a request.
pub trait SignIn: Send + Sync {
    fn user_of(&self, headers: &HeaderMap) -> SignedIn;

    /// Whether this is the development-only stand-in.
    fn is_stand_in(&self) -> bool;
}

/// Nobody is signed in.
pub struct NoSignIn;

impl SignIn for NoSignIn {
    fn user_of(&self, _headers: &HeaderMap) -> SignedIn {
        SignedIn::NotSignedIn
    }

    fn is_stand_in(&self) -> bool {
        false
    }
}

/// Development only: the caller states who the User is.
///
/// - [`STAND_IN_USER_HEADER`]: the User's id, exactly once, handed over
///   unchanged.
/// - [`STAND_IN_GROUPS_HEADER`]: their Groups, comma-separated, at most
///   once; entries trimmed, empty ones dropped, repeats kept once, in the
///   order first stated.
///
/// Each value is UTF-8, at most [`MAX_STAND_IN_VALUE_BYTES`] long, with no
/// control character.
pub struct StandInSignIn;

impl SignIn for StandInSignIn {
    fn user_of(&self, headers: &HeaderMap) -> SignedIn {
        let id = match one_value(headers, STAND_IN_USER_HEADER) {
            Ok(Some(id)) => id,
            Ok(None) => return SignedIn::NotSignedIn,
            Err(reason) => return SignedIn::Malformed(reason),
        };
        if id.is_empty() {
            return SignedIn::Malformed("the stand-in User is empty");
        }
        let groups = match one_value(headers, STAND_IN_GROUPS_HEADER) {
            Ok(groups) => groups.unwrap_or_default(),
            Err(reason) => return SignedIn::Malformed(reason),
        };
        let mut kept: Vec<String> = Vec::new();
        for group in groups.split(',').map(str::trim) {
            if !group.is_empty() && !kept.iter().any(|g| g == group) {
                kept.push(group.to_owned());
            }
        }
        SignedIn::User(User {
            id: id.to_owned(),
            groups: kept,
        })
    }

    fn is_stand_in(&self) -> bool {
        true
    }
}

/// The one value of a header, checked; `None` when absent.
fn one_value<'h>(headers: &'h HeaderMap, name: &str) -> Result<Option<&'h str>, &'static str> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("a stand-in header is given more than once");
    }
    checked(value).map(Some)
}

fn checked(value: &HeaderValue) -> Result<&str, &'static str> {
    if value.len() > MAX_STAND_IN_VALUE_BYTES {
        return Err("a stand-in header is too long");
    }
    let text =
        std::str::from_utf8(value.as_bytes()).map_err(|_| "a stand-in header is not UTF-8")?;
    if text.chars().any(char::is_control) {
        return Err("a stand-in header holds a control character");
    }
    Ok(text)
}

/// Removes every platform header ([`RESERVED_HEADER_PREFIX`]); header
/// names are already in lower case.
pub fn remove_reserved_headers(headers: &mut HeaderMap) {
    // The names are copied because the map cannot be changed while its
    // keys are borrowed; only the reserved ones, usually one or two.
    let reserved: Vec<_> = headers
        .keys()
        .filter(|name| name.as_str().starts_with(RESERVED_HEADER_PREFIX))
        .cloned()
        .collect();
    for name in &reserved {
        headers.remove(name);
    }
}
