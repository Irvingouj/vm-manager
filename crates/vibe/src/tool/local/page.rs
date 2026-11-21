//! What `vibe dev` answers by itself (blackbox/CONTRACT.md, "Which requests
//! it serves", "Choosing the test User", "The project as it is now"): its
//! pages, its redirects, its refusals.
//!
//! **Every answer** carries `cache-control: no-store`,
//! `x-content-type-options: nosniff` and a `content-security-policy` that
//! lets a page run no script, load nothing and be shown inside no other
//! page ([`POLICY`]). A page has no script.
//! Everything of the project's or the Builder's on a page (an id, a name,
//! a message) is escaped as HTML ([`escape`]), in text and in attributes
//! alike; a `Location` holds only bytes from `!` to `~`; what is
//! percent-encoded keeps only letters, digits, `-`, `.`, `_` and `~` (and
//! `/` in a path). A page is dressed by the one theme (crate::theme).

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderValue, LOCATION, SET_COOKIE,
    X_CONTENT_TYPE_OPTIONS,
};
use hyper::{Response, StatusCode};

use super::super::test_users::{TestUser, TestUsersFile};
use crate::theme::{self, Layout, Mark, Place};

/// The policy of every answer of the local run's own.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// The cookie that holds the chosen test User.
pub(super) const COOKIE: &str = "vm_test_user";

/// The page that chooses the test User.
pub(super) const CHOOSER: &str = "/_vm/test-user";

/// `text` escaped as HTML, for text and for an attribute's value in
/// double quotes alike.
pub(super) fn escape(text: &str) -> String {
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

/// `value` percent-encoded: every byte but letters, digits, `-`, `.`, `_`
/// and `~`, and `/` when `path`.
pub(super) fn percent_encode(value: &str, path: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (path && byte == b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `value` with each `%XX` taken back, and in a form each `+` as a space;
/// `None` when that is not UTF-8 or a `%` is not followed by two hex
/// digits.
pub(super) fn percent_decode(value: &str, form: bool) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'%' => {
                let digits = bytes.get(at.saturating_add(1)..at.saturating_add(3))?;
                let text = std::str::from_utf8(digits).ok()?;
                out.push(u8::from_str_radix(text, 16).ok()?);
                at = at.saturating_add(3);
            }
            b'+' if form => {
                out.push(b' ');
                at = at.saturating_add(1);
            }
            other => {
                out.push(other);
                at = at.saturating_add(1);
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The fields of a form (`application/x-www-form-urlencoded`) or a query.
pub(super) fn form_fields(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(name, true)?, percent_decode(value, true)?))
        })
        .collect()
}

/// `next` when the browser may be sent there: a path of this address,
/// beginning with one `/` and not `//` or `/\`, of bytes from `!` to `~`;
/// else `/`.
pub(super) fn local_path(next: Option<&str>) -> String {
    match next {
        Some(next)
            if next.starts_with('/')
                && !next.starts_with("//")
                && !next.starts_with("/\\")
                && next.bytes().all(|b| (0x21..=0x7e).contains(&b)) =>
        {
            next.to_owned()
        }
        _ => "/".to_owned(),
    }
}

/// An answer of the local run's own, with its headers.
fn own(status: StatusCode, content_type: &'static str, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(POLICY));
    response
}

/// A whole page: `title` (the platform's own text) and `body` (already
/// HTML), as one card or, with `layout`, in a column.
fn page(status: StatusCode, title: &str, layout: Layout, body: &str) -> Response<Full<Bytes>> {
    let title = escape(title);
    own(
        status,
        "text/html; charset=utf-8",
        theme::page(
            &title,
            Place::LocalRun,
            layout,
            &format!("<h1>{title}</h1>\n{body}"),
        ),
    )
}

/// `text/plain`.
pub(super) fn plain(status: StatusCode, text: String) -> Response<Full<Bytes>> {
    own(status, "text/plain; charset=utf-8", text)
}

/// `200`, `application/json`: what a command of the tool's prints.
pub(super) fn json(body: String) -> Response<Full<Bytes>> {
    own(StatusCode::OK, "application/json", body)
}

/// A redirect to `location`, which holds only bytes from `!` to `~`.
pub(super) fn redirect(status: StatusCode, location: &str) -> Response<Full<Bytes>> {
    let mut response = own(status, "text/plain; charset=utf-8", String::new());
    let value = HeaderValue::from_str(location).unwrap_or_else(|_| HeaderValue::from_static("/"));
    response.headers_mut().insert(LOCATION, value);
    response
}

/// `303` to `location`, choosing the test User `id`.
pub(super) fn chosen(location: &str, id: &str) -> Response<Full<Bytes>> {
    let mut response = redirect(StatusCode::SEE_OTHER, location);
    let cookie = format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Lax",
        percent_encode(id, false)
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    response
}

/// `404`: the same answer for every path under `/_vm/` that is nothing,
/// and for a control request without the secret.
pub(super) fn not_found() -> Response<Full<Bytes>> {
    page(
        StatusCode::NOT_FOUND,
        "Not found",
        Layout::Card,
        "<p>Nothing of the local run is at this address. Its page is <a href=\"/_vm/test-user\">/_vm/test-user</a>.</p>\n",
    )
}

/// `421`: a `Host` that is not the Application's.
pub(super) fn misdirected(address: &str) -> Response<Full<Bytes>> {
    let address = escape(address);
    page(
        StatusCode::MISDIRECTED_REQUEST,
        "Not this address",
        Layout::Card,
        &format!(
            "<p>This is the local run of the Application, and it answers only at its own address: <a href=\"{address}\">{address}</a>. Nothing was done.</p>\n"
        ),
    )
}

/// `405` for a method the page does not take.
pub(super) fn method_not_allowed() -> Response<Full<Bytes>> {
    let mut response = page(
        StatusCode::METHOD_NOT_ALLOWED,
        "Method not allowed",
        Layout::Card,
        "<p>This page takes GET and POST.</p>\n",
    );
    response.headers_mut().insert(
        hyper::header::ALLOW,
        HeaderValue::from_static("GET, HEAD, POST"),
    );
    response
}

/// `403`: a choice posted from another site's page.
pub(super) fn other_origin() -> Response<Full<Bytes>> {
    page(
        StatusCode::FORBIDDEN,
        "Not from this address",
        Layout::Card,
        "<p>A test User is chosen only from the local run's own page. Nothing was chosen.</p>\n",
    )
}

/// `401`, `text/plain`: no test User is chosen.
pub(super) fn no_user(address: &str) -> Response<Full<Bytes>> {
    plain(
        StatusCode::UNAUTHORIZED,
        format!(
            "No test User is chosen, so the Application was not given the request. Choose one at {address}_vm/test-user, or send the header Cookie: {COOKIE}=<id> with the id of a test User of vibe.dev.json.\n"
        ),
    )
}

/// `503`: the local run could not take the request now.
pub(super) fn unavailable(why: &str) -> Response<Full<Bytes>> {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        "Not now",
        Layout::Card,
        &format!("<p>{}</p>\n", escape(why)),
    )
}

/// `500`: the project as it is now does not build (or its test Users, its
/// migrations or its test rows fail): the reason and the message the
/// command gives, escaped.
pub(super) fn failed(error: &serde_json::Value) -> Response<Full<Bytes>> {
    let reason = error["reason"].as_str().unwrap_or("failed");
    let message = error["message"].as_str().unwrap_or_default();
    let mut body = format!(
        "<p class=\"lead\">The project as it is now cannot be served, so no Version of it is: the local run answers this page to every request until the project is corrected, and the first request after a change tries again.</p>\n<p class=\"facts-line\">Reason: <code>{}</code></p>\n<pre class=\"message\">{}</pre>\n",
        escape(reason),
        escape(message)
    );
    if let Some(fields) = error.as_object() {
        for (name, value) in fields {
            if name == "reason" || name == "message" {
                continue;
            }
            if let Some(value) = value.as_str() {
                body.push_str(&format!(
                    "<p class=\"facts-line\">{}: <code>{}</code></p>\n",
                    escape(name),
                    escape(value)
                ));
            }
        }
    }
    page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The project does not build",
        Layout::Column,
        &body,
    )
}

/// The access the Grants of `file` give `user` on `resource`: the highest
/// of its Groups', or `none`.
fn access(file: &TestUsersFile, user: &TestUser, resource: &str) -> &'static str {
    let rank = |access: &str| match access {
        "structure" => 1,
        "read" => 2,
        "write" => 3,
        _ => 0,
    };
    let best = file
        .grants
        .iter()
        .filter(|g| g.resource == resource && user.groups.contains(&g.group))
        .map(|g| rank(&g.access))
        .max()
        .unwrap_or(0);
    match best {
        1 => "structure",
        2 => "read",
        3 => "write",
        _ => "none",
    }
}

/// What the chooser says of what happened, above the list.
pub(super) enum Said<'a> {
    Nothing,
    NotInFile(&'a str),
}

/// The page that chooses the test User: every User of `file`, each with
/// its id, its name and email when it has them, its Groups, its access on
/// each pulled Resource of `pulled`, and a button; `chosen` marked.
pub(super) fn chooser(
    status: StatusCode,
    file: &TestUsersFile,
    pulled: &[String],
    chosen: Option<&str>,
    next: &str,
    said: &Said<'_>,
) -> Response<Full<Bytes>> {
    let mut body = String::new();
    if let Said::NotInFile(id) = said {
        body.push_str(&theme::notice(
            Mark::Error,
            "role=\"alert\"",
            &format!(
                "<code>{}</code> is not a test User of vibe.dev.json: choose one of those below.",
                escape(id)
            ),
        ));
    }
    body.push_str(
        "<p class=\"lead\">The Application runs on this machine and is given each request as the test User chosen here, from vibe.dev.json as it is now.</p>\n<ul class=\"users\">\n",
    );
    for user in &file.users {
        let is_chosen = chosen == Some(user.id.as_str());
        let id = escape(&user.id);
        // The form holds the whole card, its hidden `user` first: one
        // button for each User, and everything said of them inside it.
        body.push_str(if is_chosen {
            "<li class=\"card chosen\">"
        } else {
            "<li class=\"card\">"
        });
        body.push_str(&format!(
            "<form method=\"post\" action=\"/_vm/test-user\"><input type=\"hidden\" name=\"user\" value=\"{id}\"><input type=\"hidden\" name=\"next\" value=\"{}\">",
            escape(next)
        ));
        let name = user.display_name.as_deref().map(escape);
        body.push_str(&format!(
            "<div class=\"user-head\"><span class=\"name\">{}</span>",
            name.as_deref().unwrap_or(&id)
        ));
        if is_chosen {
            body.push_str("<span class=\"pill on\">\u{2713} Chosen now</span>");
        }
        body.push_str("</div>");
        if user.email.is_some() || name.is_some() {
            body.push_str("<p class=\"ident\">");
            if let Some(email) = &user.email {
                body.push_str(&format!(
                    "<span class=\"soft\">&lt;{}&gt;</span> ",
                    escape(email)
                ));
            }
            if name.is_some() {
                body.push_str(&format!("<code class=\"id\">{id}</code>"));
            }
            body.push_str("</p>");
        }
        let groups: Vec<String> = user.groups.iter().map(|g| escape(g)).collect();
        body.push_str(&format!(
            "<dl class=\"facts\"><dt>Groups</dt><dd>{}</dd>",
            if groups.is_empty() {
                "(none)".to_owned()
            } else {
                groups.join(", ")
            }
        ));
        for resource in pulled {
            body.push_str(&format!(
                "<dt><code>{}</code></dt><dd>{}</dd>",
                escape(resource),
                access(file, user, resource)
            ));
        }
        body.push_str(&format!(
            "</dl><div class=\"form-actions\"><button type=\"submit\">Be this User<span class=\"vh\"> {}</span></button></div></form></li>\n",
            name.as_deref().unwrap_or(&id)
        ));
    }
    body.push_str("</ul>\n");
    page(status, "Choose the test User", Layout::Column, &body)
}
