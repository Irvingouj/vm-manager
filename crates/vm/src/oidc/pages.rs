//! The platform's own answers on the way through sign-in: its pages (a
//! sign-in that failed, the sign-out page and its answer, an unknown
//! Application), its plain refusals and its redirects.
//!
//! **Every answer** carries `cache-control: no-store`,
//! `x-content-type-options: nosniff`, `referrer-policy: no-referrer` and a
//! `content-security-policy` that lets a page run no script, load nothing
//! and be shown inside no other page ([`POLICY`]): [`own_headers`], which
//! Ingress's own plain answers carry too. A page has no script. Everything on a page that is not
//! the platform's own text (an Application's name, an address, a code of
//! the provider's) is escaped as HTML ([`escape`]). Nothing of a token, a
//! code or a secret is ever on a page.

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderValue, LOCATION, REFERRER_POLICY,
    RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use hyper::{HeaderMap, Response, StatusCode};

/// The policy of every answer of the platform's own here.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

const STYLE: &str = "body{font-family:system-ui,sans-serif;max-width:40rem;margin:3rem auto;padding:0 1rem;line-height:1.5}";

/// How long a `503` asks to wait, in seconds.
pub const RETRY_AFTER_SECONDS: &str = "10";

/// `text` escaped as HTML, for text and for an attribute's value in double
/// quotes alike.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The headers every answer of the platform's own carries.
pub fn own_headers(headers: &mut HeaderMap) {
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(POLICY));
}

fn own(status: StatusCode, content_type: &'static str, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    own_headers(headers);
    if status == StatusCode::SERVICE_UNAVAILABLE {
        headers.insert(RETRY_AFTER, HeaderValue::from_static(RETRY_AFTER_SECONDS));
    }
    response
}

/// A page: `title` and `body` are the platform's own HTML, with anything
/// else in them escaped already.
fn page(status: StatusCode, title: &str, body: &str) -> Response<Full<Bytes>> {
    own(
        status,
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title><style>{STYLE}</style></head><body><h1>{title}</h1>{body}</body></html>\n"
        ),
    )
}

/// A sign-in that failed: what failed, and a link to try again.
pub fn failed(status: StatusCode, what: &str, again: &str) -> Response<Full<Bytes>> {
    page(
        status,
        "Sign-in failed",
        &format!(
            "<p>{}</p><p><a href=\"{}\">Try again</a></p>",
            escape(what),
            escape(again)
        ),
    )
}

/// `404`: no Application by this name, for someone signed in.
pub fn no_application(name: &str, again: &str) -> Response<Full<Bytes>> {
    page(
        StatusCode::NOT_FOUND,
        "No such Application",
        &format!(
            "<p>There is no Application named <code>{}</code>.</p><p><a href=\"{}\">Back</a></p>",
            escape(name),
            escape(again)
        ),
    )
}

/// `404` of every other path of the sign-in address.
pub fn not_found() -> Response<Full<Bytes>> {
    page(
        StatusCode::NOT_FOUND,
        "Not found",
        "<p>The sign-in address has no such page.</p><p><a href=\"/sign-out\">Sign out</a></p>",
    )
}

/// `405` of a path of the sign-in address with a method it does not have.
pub fn method_not_allowed() -> Response<Full<Bytes>> {
    page(
        StatusCode::METHOD_NOT_ALLOWED,
        "Method not allowed",
        "<p>This page does not take that method.</p><p><a href=\"/sign-out\">Sign out</a></p>",
    )
}

/// `GET /sign-out`: one button that posts.
pub fn sign_out_form() -> Response<Full<Bytes>> {
    page(
        StatusCode::OK,
        "Sign out",
        "<p>Sign out of every Application of this installation in this browser.</p><form method=\"post\" action=\"/sign-out\"><button type=\"submit\">Sign out</button></form>",
    )
}

/// The answer to `POST /sign-out`.
pub fn signed_out(clear: Option<HeaderValue>) -> Response<Full<Bytes>> {
    let mut response = page(
        StatusCode::OK,
        "Signed out",
        "<p>You are signed out of every Application of this installation in this browser.</p><p>Your company's own sign-in may still be open: sign out there too if this browser is not yours alone.</p>",
    );
    if let Some(clear) = clear {
        response.headers_mut().append(SET_COOKIE, clear);
    }
    response
}

/// A short plain-text answer.
pub fn plain(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    own(status, "text/plain; charset=utf-8", text.to_owned())
}

/// `503` as a page: the platform database or the provider could not be
/// used.
pub fn unavailable(again: &str) -> Response<Full<Bytes>> {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        "Try again shortly",
        &format!(
            "<p>The sign-in cannot go on just now.</p><p><a href=\"{}\">Try again</a></p>",
            escape(again)
        ),
    )
}

/// A JSON answer.
pub fn json(body: String) -> Response<Full<Bytes>> {
    own(StatusCode::OK, "application/json", body)
}

/// `302` to `location`, setting these cookies.
pub fn redirect(location: &str, cookies: Vec<HeaderValue>) -> Response<Full<Bytes>> {
    let mut response = own(
        StatusCode::FOUND,
        "text/plain; charset=utf-8",
        String::new(),
    );
    let headers = response.headers_mut();
    match HeaderValue::from_str(location) {
        Ok(value) => {
            headers.insert(LOCATION, value);
        }
        Err(_) => {
            return failed(
                StatusCode::BAD_REQUEST,
                "The address to go on to cannot be written.",
                "/",
            );
        }
    }
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    response
}
