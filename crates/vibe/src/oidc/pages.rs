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
//! page. A page is dressed by the one theme (crate::theme): a card that
//! says one thing, under a header that links nowhere, so that the links a
//! page has are exactly those said here.

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderValue, LOCATION, REFERRER_POLICY,
    RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use hyper::{HeaderMap, Response, StatusCode};

use crate::theme::{self, Icon, Layout, Mark, Place};

/// The policy of every answer of the platform's own here.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// What the page of a sign-in of the tool with a port lets its form lead
/// to besides: the redirect that follows the confirm, to the tool on this
/// computer (browsers hold the redirects of a form's submission to
/// `form-action` too).
fn policy_with_port(port: u16) -> String {
    format!("{POLICY} http://127.0.0.1:{port}")
}

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

/// A page of the installation `at` (its domain; `None` where it is not
/// known): `title` is the `<title>`, `heading` the one `<h1>` and `body`
/// the rest, the platform's own HTML with anything else in them escaped
/// already.
fn page(
    at: Option<&str>,
    status: StatusCode,
    title: &str,
    heading: &str,
    body: &str,
) -> Response<Full<Bytes>> {
    let installation = at.map(escape);
    own(
        status,
        "text/html; charset=utf-8",
        Bytes::from(theme::page(
            title,
            Place::SignIn {
                installation: installation.as_deref(),
            },
            Layout::Card,
            &format!("{heading}{body}"),
        )),
    )
}

/// The way on from a page: a link to the Application's address when it is
/// known which Application the sign-in was for; else words saying to open
/// it again, and no link.
fn way_on(again: Option<&str>) -> String {
    match again {
        Some(again) => format!(
            "<div class=\"actions\"><a class=\"btn primary\" href=\"{}\">Try again</a></div>",
            escape(again)
        ),
        None => {
            "<p class=\"soft\">To try again, open the Application's address again.</p>".to_owned()
        }
    }
}

/// A sign-in that failed at the installation `at`: what failed, and the
/// way on ([`way_on`]).
pub fn failed(
    at: &str,
    status: StatusCode,
    what: &str,
    again: Option<&str>,
) -> Response<Full<Bytes>> {
    failed_at(Some(at), status, what, again)
}

fn failed_at(
    at: Option<&str>,
    status: StatusCode,
    what: &str,
    again: Option<&str>,
) -> Response<Full<Bytes>> {
    page(
        at,
        status,
        "Sign-in failed",
        &theme::state_title(Mark::Error, Icon::Cross, "Sign-in failed"),
        &format!("<p role=\"alert\">{}</p>{}", escape(what), way_on(again)),
    )
}

/// `404`: no Application by this name, for someone signed in. No link:
/// there is no Dashboard yet to go back to.
pub fn no_application(at: &str, name: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::NOT_FOUND,
        "No such Application",
        &theme::state_title(
            Mark::Info,
            Icon::Unknown,
            &format!("No Application named <code>{}</code>", escape(name)),
        ),
        "",
    )
}

/// `403`: the Application is not shared with the person signed in. It names
/// the Application and whom they are signed in as (their id, and their
/// name when the platform knows it), all escaped, and no Contributor;
/// with OIDC one link, to the sign-in address's sign-out (`sign_out`),
/// beside whom they are signed in as.
pub fn not_shared(
    at: &str,
    application: &str,
    id: &str,
    name: Option<&str>,
    sign_out: Option<&str>,
) -> Response<Full<Bytes>> {
    let who = match name {
        Some(name) => format!(
            "<strong>{}</strong> <code class=\"id\">{}</code>",
            escape(name),
            escape(id)
        ),
        None => format!("<code class=\"id as-name\">{}</code>", escape(id)),
    };
    page(
        Some(at),
        StatusCode::FORBIDDEN,
        "Not shared with you",
        &theme::state_title(Mark::Info, Icon::Lock, "Not shared with you"),
        &format!(
            "<p>To open <code>{}</code>, ask one of its Contributors to share it with you.</p>\
             <div class=\"aside signed\"><p class=\"small soft\">{}<span class=\"vh\">You are signed in as </span>{who}</p>{}</div>",
            escape(application),
            Icon::Person.svg(),
            sign_out.map_or_else(String::new, |address| format!(
                "<a class=\"btn quiet compact\" href=\"{}\">Sign out</a>",
                escape(address)
            )),
        ),
    )
}

/// `404` of every other path of the sign-in address.
pub fn not_found(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::NOT_FOUND,
        "No such page",
        &theme::state_title(Mark::Info, Icon::Unknown, "No such page"),
        "<div class=\"actions\"><a class=\"btn\" href=\"/sign-out\">Sign out</a></div>",
    )
}

/// `405` of a path of the sign-in address with a method it does not have.
pub fn method_not_allowed(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::METHOD_NOT_ALLOWED,
        "Method not allowed",
        &theme::state_title(Mark::Info, Icon::Unknown, "Method not allowed"),
        "<div class=\"actions\"><a class=\"btn\" href=\"/sign-out\">Sign out</a></div>",
    )
}

/// `GET /sign-out`: one button that posts.
pub fn sign_out_form(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::OK,
        "Sign out",
        "<h1>Sign out of every Application?</h1>",
        &format!(
            "<p class=\"soft\">In this browser only.</p><form method=\"post\" action=\"/sign-out\"><div class=\"actions\"><button type=\"submit\" class=\"primary\">{}Sign out</button></div></form>",
            Icon::Leave.svg()
        ),
    )
}

/// The answer to `POST /sign-out`.
pub fn signed_out(at: &str, clear: Option<HeaderValue>) -> Response<Full<Bytes>> {
    let mut response = page(
        Some(at),
        StatusCode::OK,
        "Signed out",
        &theme::state_title(Mark::Ok, Icon::Check, "Signed out"),
        &format!(
            "<p class=\"soft\">Of every Application, in this browser.</p>{}",
            theme::notice(
                Mark::Warn,
                "role=\"status\"",
                "Your company's own sign-in may still be open: sign out there too if this browser is not yours alone."
            )
        ),
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
pub fn unavailable(at: &str, again: Option<&str>) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::SERVICE_UNAVAILABLE,
        "Try again shortly",
        &theme::state_title(Mark::Warn, Icon::Later, "Try again shortly"),
        &format!("<p>The sign-in cannot go on just now.</p>{}", way_on(again)),
    )
}

/// A JSON answer.
pub fn json(body: String) -> Response<Full<Bytes>> {
    own(StatusCode::OK, "application/json", Bytes::from(body))
}

/// The next step after a sign-in of the tool that did not happen.
const RUN_AGAIN: &str = "<p class=\"soft\">To sign the tool in, run <code>vibe sign-in</code> again on your computer.</p>";

/// A sign-in of the tool that failed before it awaited an answer: what
/// failed, and to run `vibe sign-in` again. No link.
pub fn tool_failed(at: &str, status: StatusCode, what: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        status,
        "The tool was not signed in",
        &theme::state_title(Mark::Error, Icon::Cross, "The tool was not signed in"),
        &format!("<p>{}</p>{RUN_AGAIN}", escape(what)),
    )
}

/// `POST /tool` for a sign-in of the tool that is over or not known: no
/// form. Why it may be over is one step away.
pub fn tool_over(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::BAD_REQUEST,
        "This sign-in of the tool is over",
        &theme::state_title(Mark::Error, Icon::Cross, "This sign-in of the tool is over"),
        &format!(
            "<p>Nothing was signed in.</p>{RUN_AGAIN}\
             <details class=\"small soft\"><summary>Why it may be over</summary><p>It was answered already, pushed out by a newer one, or ended after five wrong check codes or 10 minutes; or it was begun in another browser.</p></details>"
        ),
    )
}

/// `POST /tool` with a body that is not the page's form.
pub fn tool_not_a_form(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::BAD_REQUEST,
        "Not the form of this page",
        &theme::state_title(Mark::Error, Icon::Cross, "Not the form of this page"),
        "<p>This address takes only the answer of the page that asks whether to sign the tool in. Nothing was signed in.</p>",
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
/// computer, and without, it asks for the check code. Who and where are
/// two rows, each with its mark; the warning stays in words, whole: here a
/// person decides whether to trust what they approve.
pub fn tool_ask(
    at: &str,
    person: &Person<'_>,
    machine: &str,
    port: Option<u16>,
    sign_in: &str,
    wrong_code: bool,
) -> Response<Full<Bytes>> {
    let id = escape(person.id);
    // The first thing known of the person is their name on the page; the
    // id is small and soft only where something stands before it.
    let who = match (person.name, person.email) {
        (Some(name), Some(email)) => format!(
            "<span class=\"name\">{}</span><span class=\"more\">{} <code class=\"id\">{id}</code></span>",
            escape(name),
            escape(email)
        ),
        (Some(name), None) => format!(
            "<span class=\"name\">{}</span><span class=\"more\"><code class=\"id\">{id}</code></span>",
            escape(name)
        ),
        (None, Some(email)) => format!(
            "<span class=\"name\">{}</span><span class=\"more\"><code class=\"id\">{id}</code></span>",
            escape(email)
        ),
        (None, None) => {
            format!("<span class=\"name\"><code class=\"id as-name\">{id}</code></span>")
        }
    };
    let machine = if machine.is_empty() {
        "a machine that gave no name".to_owned()
    } else {
        format!("the machine <strong>{}</strong>", escape(machine))
    };
    let here = if port.is_some() {
        "<span class=\"more\">On this computer: once you confirm, this browser takes the sign-in to it.</span>"
    } else {
        ""
    };
    let field = match port {
        Some(_) => String::new(),
        None => {
            let (wrong, described, invalid) = if wrong_code {
                (
                    theme::notice(
                        Mark::Error,
                        "id=\"code-error\" role=\"alert\"",
                        "<strong>That is not the check code the tool shows.</strong> Look at it again; after five wrong codes this sign-in ends.",
                    ),
                    "code-help code-error",
                    " aria-invalid=\"true\"",
                )
            } else {
                (String::new(), "code-help", "")
            };
            format!(
                "<div class=\"field\"><label for=\"code\">Check code</label>\
                 <p class=\"help\" id=\"code-help\">The eight letters and digits the tool shows.</p>{wrong}\
                 <div class=\"control\"><input id=\"code\" class=\"code\" type=\"text\" name=\"code\" autocomplete=\"off\" spellcheck=\"false\" maxlength=\"32\" aria-describedby=\"{described}\"{invalid}></div></div>"
            )
        }
    };
    let warning = theme::notice(
        Mark::Warn,
        "",
        "<strong>Go on only if you ran <code>vibe sign-in</code> yourself, just now.</strong> If someone sent you this address, do not sign in: whoever runs that tool would act as you.",
    );
    let body = format!(
        "<ul class=\"rows pair\">\
         <li><span class=\"who-is\">{}<span class=\"lines\">{who}</span></span></li>\
         <li><span class=\"who-is\">{}<span class=\"lines\"><span>The command-line tool <code>vibe</code> on {machine}</span>{here}</span></span></li></ul>\
         {warning}\
         <form method=\"post\" action=\"/tool\">\
         <input type=\"hidden\" name=\"sign_in\" value=\"{}\">{field}\
         <div class=\"actions form-actions\"><button type=\"submit\" name=\"answer\" value=\"confirm\" class=\"primary\">Yes, sign it in</button> \
         <button type=\"submit\" name=\"answer\" value=\"decline\">No</button></div>\
         </form>",
        theme::icon(Icon::Person, "As:"),
        theme::icon(Icon::Terminal, "Asks:"),
        escape(sign_in)
    );
    let mut response = page(
        Some(at),
        StatusCode::OK,
        "Sign the tool in?",
        "<h1>Sign the tool in?</h1>",
        &body,
    );
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
pub fn tool_signed_in(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::OK,
        "The tool is signed in",
        &theme::state_title(Mark::Ok, Icon::Check, "The tool is signed in"),
        "<p>Go back to it: it finishes on its own within a few seconds.</p>",
    )
}

/// A sign-in of the tool declined.
pub fn tool_declined(at: &str) -> Response<Full<Bytes>> {
    page(
        Some(at),
        StatusCode::OK,
        "Nothing was signed in",
        &theme::state_title(Mark::Info, Icon::Cross, "Nothing was signed in"),
        "<p>This sign-in is over. To sign the tool in after all, run <code>vibe sign-in</code> again.</p>",
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
            return failed_at(
                None,
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
