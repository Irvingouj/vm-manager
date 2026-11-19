//! The platform's own answers on the way through sign-in: its pages (a
//! sign-in that failed, the sign-out page and its answer, an unknown
//! Application), its plain refusals and its redirects.
//!
//! **Every answer** carries `cache-control: no-store`,
//! `x-content-type-options: nosniff`, `referrer-policy: same-origin` (no
//! address of the platform's is told to another origin, and a form is sent
//! with the page's real origin) and a `content-security-policy` that lets
//! a page run no script, load nothing and be shown inside no other page
//! ([`POLICY`]): [`own_headers`], which Ingress's own plain answers and
//! the Dashboard's pages (crate::dashboard) carry too. A page has no
//! script. Everything on a page that is not the platform's own text (an
//! Application's name, an address, a code of the provider's) is escaped as
//! HTML ([`escape`]). Nothing of a token, a code or a secret is ever on a
//! page.

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderValue, LOCATION, REFERRER_POLICY,
    RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use hyper::{HeaderMap, Response, StatusCode};

/// The policy of every answer of the platform's own here.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// What the page of a sign-in of the tool with a port lets its form lead
/// to besides: the redirect that follows the confirm, to the tool on this
/// computer (browsers hold the redirects of a form's submission to
/// `form-action` too).
fn policy_with_port(port: u16) -> String {
    format!("{POLICY} http://127.0.0.1:{port}")
}

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
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(POLICY));
}

fn own(status: StatusCode, content_type: &'static str, body: Bytes) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body));
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
        Bytes::from(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title><style>{STYLE}</style></head><body><h1>{title}</h1>{body}</body></html>\n"
        )),
    )
}

/// The way on from a page: a link to the Application's address when it is
/// known which Application the sign-in was for; else words saying to open
/// it again, and no link.
fn way_on(again: Option<&str>) -> String {
    match again {
        Some(again) => format!("<p><a href=\"{}\">Try again</a></p>", escape(again)),
        None => "<p>To try again, open the Application's address again.</p>".to_owned(),
    }
}

/// A sign-in that failed: what failed, and the way on ([`way_on`]).
pub fn failed(status: StatusCode, what: &str, again: Option<&str>) -> Response<Full<Bytes>> {
    page(
        status,
        "Sign-in failed",
        &format!("<p>{}</p>{}", escape(what), way_on(again)),
    )
}

/// `404`: no Application by this name, for someone signed in. No link:
/// there is no Dashboard yet to go back to.
pub fn no_application(name: &str) -> Response<Full<Bytes>> {
    page(
        StatusCode::NOT_FOUND,
        "No such Application",
        &format!(
            "<p>There is no Application named <code>{}</code>.</p>",
            escape(name)
        ),
    )
}

/// `403`: the Application is not shared with the person signed in. It names
/// the Application and whom they are signed in as (their id, and their
/// name when the platform knows it), all escaped, and no Contributor;
/// with OIDC one link, to the sign-in address's sign-out (`sign_out`).
pub fn not_shared(
    application: &str,
    id: &str,
    name: Option<&str>,
    sign_out: Option<&str>,
) -> Response<Full<Bytes>> {
    let who = match name {
        Some(name) => format!(
            "<strong>{}</strong> (id <code>{}</code>)",
            escape(name),
            escape(id)
        ),
        None => format!("id <code>{}</code>", escape(id)),
    };
    page(
        StatusCode::FORBIDDEN,
        "This Application is not shared with you",
        &format!(
            "<p>The Application <code>{}</code> is not shared with you. You are signed in as {who}.</p>\
             <p>To open it, ask one of its Contributors to share it with you.</p>{}",
            escape(application),
            sign_out.map_or_else(String::new, |address| format!(
                "<p>Signed in as the wrong person? <a href=\"{}\">Sign out</a></p>",
                escape(address)
            )),
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
    own(
        status,
        "text/plain; charset=utf-8",
        Bytes::from_static(text.as_bytes()),
    )
}

/// `503` as a page: the platform database could not be used, or no
/// randomness was to be had.
pub fn unavailable(again: Option<&str>) -> Response<Full<Bytes>> {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        "Try again shortly",
        &format!("<p>The sign-in cannot go on just now.</p>{}", way_on(again)),
    )
}

/// A JSON answer.
pub fn json(body: String) -> Response<Full<Bytes>> {
    own(StatusCode::OK, "application/json", Bytes::from(body))
}

/// A sign-in of the tool that failed before it awaited an answer: what
/// failed, and to run `vibe sign-in` again. No link.
pub fn tool_failed(status: StatusCode, what: &str) -> Response<Full<Bytes>> {
    page(
        status,
        "The tool was not signed in",
        &format!(
            "<p>{}</p><p>To sign the tool in, run <code>vibe sign-in</code> again on your computer.</p>",
            escape(what)
        ),
    )
}

/// `POST /tool` for a sign-in of the tool that is over or not known: no
/// form.
pub fn tool_over() -> Response<Full<Bytes>> {
    page(
        StatusCode::BAD_REQUEST,
        "This sign-in of the tool is over",
        "<p>It was answered already, pushed out by a newer one, or ended after five wrong check codes or 10 minutes; or it was begun in another browser. Nothing was signed in.</p><p>To sign the tool in, run <code>vibe sign-in</code> again on your computer.</p>",
    )
}

/// `POST /tool` with a body that is not the page's form.
pub fn tool_not_a_form() -> Response<Full<Bytes>> {
    page(
        StatusCode::BAD_REQUEST,
        "Not the form of this page",
        "<p>This address takes the answer of the page that asks whether to sign the tool in. Nothing was signed in.</p>",
    )
}

/// Who is being signed in, as the page of a sign-in of the tool says it.
pub struct Person<'a> {
    pub id: &'a str,
    pub name: Option<&'a str>,
    pub email: Option<&'a str>,
}

/// The page of a sign-in of the tool awaiting an answer: who, on which
/// machine, and one form that posts to `/tool`. `sign_in` is the value
/// that names it; with a `port`, the form may lead to the tool on this
/// computer, and without, it asks for the check code.
pub fn tool_ask(
    person: &Person<'_>,
    machine: &str,
    port: Option<u16>,
    sign_in: &str,
    wrong_code: bool,
) -> Response<Full<Bytes>> {
    let mut who = String::new();
    if let Some(name) = person.name {
        who.push_str(&format!("<strong>{}</strong>, ", escape(name)));
    }
    if let Some(email) = person.email {
        who.push_str(&format!("{}, ", escape(email)));
    }
    who.push_str(&format!("id <code>{}</code>", escape(person.id)));
    let machine = if machine.is_empty() {
        "a machine that gave no name".to_owned()
    } else {
        format!("the machine <strong>{}</strong>", escape(machine))
    };
    let (where_, code) = match port {
        Some(_) => (
            "The tool is on this computer: once you confirm, this browser takes the sign-in to it.",
            String::new(),
        ),
        None => (
            "Type the check code the tool shows (eight letters and digits) to confirm.",
            "<p><label>Check code <input type=\"text\" name=\"code\" autocomplete=\"off\" spellcheck=\"false\" maxlength=\"32\"></label></p>".to_owned(),
        ),
    };
    let wrong = if wrong_code {
        "<p><strong>That is not the check code the tool shows.</strong> Look at it again; after five wrong codes this sign-in ends.</p>"
    } else {
        ""
    };
    let body = format!(
        "<p>The command-line tool <code>vibe</code> on {machine} asks to be signed in as {who}.</p>\
         <p><strong>Go on only if you ran <code>vibe sign-in</code> yourself, just now.</strong> If someone sent you this address, do not sign in: whoever runs that tool would act as you.</p>\
         <p>{where_}</p>{wrong}\
         <form method=\"post\" action=\"/tool\">\
         <input type=\"hidden\" name=\"sign_in\" value=\"{}\">{code}\
         <p><button type=\"submit\" name=\"answer\" value=\"confirm\">Sign the tool in</button> \
         <button type=\"submit\" name=\"answer\" value=\"decline\">Do not sign it in</button></p>\
         </form>",
        escape(sign_in)
    );
    let mut response = page(StatusCode::OK, "Sign the tool in?", &body);
    if let Some(port) = port
        && let Ok(policy) = HeaderValue::from_str(&policy_with_port(port))
    {
        response
            .headers_mut()
            .insert(CONTENT_SECURITY_POLICY, policy);
    }
    response
}

/// The confirm of a sign-in of the tool without a port: the tool takes it.
pub fn tool_signed_in() -> Response<Full<Bytes>> {
    page(
        StatusCode::OK,
        "The tool is signed in",
        "<p>Go back to the tool: it finishes the sign-in on its own within a few seconds.</p>",
    )
}

/// A sign-in of the tool declined.
pub fn tool_declined() -> Response<Full<Bytes>> {
    page(
        StatusCode::OK,
        "Nothing was signed in",
        "<p>The tool was not signed in, and this sign-in is over. If you did mean to sign it in, run <code>vibe sign-in</code> again.</p>",
    )
}

/// `303` to `location`: after the form of a sign-in of the tool, a `GET`.
pub fn see_other(location: &str) -> Response<Full<Bytes>> {
    let mut response = redirect(location, Vec::new());
    if response.status() == StatusCode::FOUND {
        *response.status_mut() = StatusCode::SEE_OTHER;
    }
    response
}

/// `302` to `location`, setting these cookies.
pub fn redirect(location: &str, cookies: Vec<HeaderValue>) -> Response<Full<Bytes>> {
    let mut response = own(StatusCode::FOUND, "text/plain; charset=utf-8", Bytes::new());
    let headers = response.headers_mut();
    match HeaderValue::from_str(location) {
        Ok(value) => {
            headers.insert(LOCATION, value);
        }
        Err(_) => {
            return failed(
                StatusCode::BAD_REQUEST,
                "The address to go on to cannot be written.",
                None,
            );
        }
    }
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    response
}
