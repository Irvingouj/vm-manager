//! The Dashboard's pages: HTML of the platform's own, with the headers
//! every page of the platform's own carries (crate::oidc::pages's
//! `own_headers`: `no-store`, `nosniff`, `no-referrer`, and a policy with
//! `default-src 'none'`, `frame-ancestors 'none'` and `form-action
//! 'self'`), one small inline stylesheet, no script, no outside resource.
//! Everything that is not the platform's own text is escaped as HTML
//! (crate::oidc::pages's `escape`), in text and in attributes alike.
//!
//! The `data-vm-...` attributes are what the contract names for a test (and
//! a Coding agent) to read; the words beside them say the same for a
//! person, and a Resource's standing is said in words with a sign, never in
//! colour alone.
//!
//! **Size.** A page shows at most 100 Applications, each with at most 20
//! Contributors (one Application's page: at most 100, and at most 1,000
//! entries of whom it is shared with), each at most 256 bytes of id and
//! 256 of name, at most six times longer once escaped; and the Resources
//! each serving Version declares. So a list is bounded by about 100 × 20 ×
//! 4 KiB (8 MiB) plus the declared Resources, and in practice a few
//! hundred kilobytes.

use std::collections::BTreeMap;

use authz::AccessLevel;
use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{ALLOW, CONTENT_TYPE, HeaderValue, LOCATION, RETRY_AFTER};
use hyper::{Response, StatusCode};

use super::Seen;
use crate::oidc::pages::{RETRY_AFTER_SECONDS, escape, own_headers};
use crate::registry::{Declared, MAX_SHARED_WITH, Named};
use crate::resources::level_name;
use crate::user::User;

type Answer = Response<Full<Bytes>>;

/// One small stylesheet: the system's fonts, a width that reads on a
/// laptop and a phone, a dark scheme, a visible focus.
const STYLE: &str = "\
:root{color-scheme:light dark;--fg:#1c1c21;--bg:#fff;--muted:#5d5d68;--line:#d9d9e0;--card:#f6f6f9;--accent:#0b57d0;--on-accent:#fff;--warn:#a1340f}\
@media (prefers-color-scheme:dark){:root{--fg:#e9e9ee;--bg:#141417;--muted:#a6a6b1;--line:#35353d;--card:#1d1d22;--accent:#8ab4f8;--on-accent:#0b1b33;--warn:#ffb595}}\
*{box-sizing:border-box}\
body{margin:0;font:1rem/1.5 system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;color:var(--fg);background:var(--bg)}\
header,main{max-width:46rem;margin:0 auto;padding:1rem}\
header{display:flex;flex-wrap:wrap;gap:.25rem 1rem;align-items:baseline;border-bottom:1px solid var(--line)}\
header p{margin:0;flex:1;color:var(--muted)}\
a{color:var(--accent)}\
:focus-visible{outline:3px solid var(--accent);outline-offset:2px}\
h1{font-size:1.6rem;margin:.5rem 0 1rem}h2{font-size:1.2rem;margin:1.5rem 0 .5rem}\
h3,h4{margin:1rem 0 .25rem}h4{font-size:.95rem;color:var(--muted)}\
ul{padding-left:1.25rem}ul.apps{list-style:none;padding:0}\
.app{background:var(--card);border:1px solid var(--line);border-radius:.5rem;padding:1rem;margin:0 0 1rem}\
.app>h1,.app>h3{margin-top:0}\
.open{display:inline-block;margin:.25rem 0;padding:.45rem 1.1rem;border-radius:.4rem;background:var(--accent);color:var(--on-accent);font-weight:600;text-decoration:none}\
.muted{color:var(--muted)}\
.mark{font-weight:600}.lower .mark,.none .mark,.error{color:var(--warn)}\
.notice,.error{padding:.5rem .75rem;border-left:4px solid currentColor;background:var(--card)}\
code{overflow-wrap:anywhere}\
li form{display:inline;margin-left:.5rem}\
button,input,select{font:inherit}\
form.share label{display:block;font-weight:600}\
form.share input,form.share select{width:100%;max-width:24rem;margin-bottom:.5rem}\
nav{display:flex;gap:1rem}";

/// Whom a page is for: the User, their name when the platform knows it,
/// and the sign-in address's sign-out with OIDC.
pub(crate) struct Viewer<'a> {
    pub user: &'a User,
    pub name: Option<String>,
    pub sign_out: Option<String>,
}

/// A person, a Group or everyone, as a page shows them.
pub(crate) struct Person<'a> {
    named: &'a Named,
    name: Option<&'a str>,
    email: Option<&'a str>,
}

impl<'a> Person<'a> {
    pub fn of(named: &'a Named, name: Option<&'a str>, email: Option<&'a str>) -> Self {
        Self { named, name, email }
    }

    /// `user`, `group` or `everyone`, and the id or the Group.
    fn kind(&self) -> (&'static str, Option<&'a str>) {
        match self.named {
            Named::User(id) => ("user", Some(id)),
            Named::Group(group) => ("group", Some(group)),
            Named::Everyone => ("everyone", None),
        }
    }

    /// The words: a person by name (and email when shown) and id, a Group
    /// as a Group, everyone as everyone.
    fn words(&self, with_email: bool) -> String {
        match self.named {
            Named::User(id) => {
                let mut text = String::new();
                if let Some(name) = self.name {
                    text.push_str(&format!("{} ", escape(name)));
                }
                if with_email && let Some(email) = self.email {
                    text.push_str(&format!("&lt;{}&gt; ", escape(email)));
                }
                text.push_str(&format!("<code>{}</code>", escape(id)));
                text
            }
            Named::Group(group) => format!("the Group <code>{}</code>", escape(group)),
            Named::Everyone => "everyone who can sign in to this installation".to_owned(),
        }
    }
}

/// One Application as the list and its own page show it.
pub(crate) struct Block<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    /// `contributor` or `shared`; none for a Platform administrator it is
    /// not shared with.
    pub relation: Option<&'static str>,
    pub serving: Option<u64>,
    /// Its address with `/`, when a Version serves.
    pub open: Option<String>,
    /// Its address, shown on its own page.
    pub address: Option<String>,
    /// Whether to link its own page.
    pub details: bool,
    pub contributors: Vec<Person<'a>>,
    /// How many Contributors are not shown.
    pub more_contributors: i64,
    /// What its serving Version declares.
    pub declared: Option<&'a Declared>,
    pub seen: &'a BTreeMap<&'a str, Seen>,
}

/// What a change led to.
#[derive(Clone, Copy)]
pub(crate) enum Notice {
    Shared,
    Unshared,
    Nothing,
}

/// Why a change of sharing was refused.
pub(crate) enum Refusal {
    NotFound,
    NotAContributor,
    Malformed(String),
    UnknownEmail(String),
    AmbiguousEmail(String),
    TooMany,
    LastContributor,
    Unavailable,
    TimedOut,
}

/// A page: `title` (escaped already) and `main`, the platform's own HTML
/// with anything else in it escaped already.
fn page(status: StatusCode, viewer: &Viewer<'_>, title: &str, main: &str) -> Answer {
    let who = match &viewer.name {
        Some(name) => format!(
            "<strong>{}</strong> (<code>{}</code>)",
            escape(name),
            escape(&viewer.user.id)
        ),
        None => format!("<code>{}</code>", escape(&viewer.user.id)),
    };
    let sign_out = viewer.sign_out.as_deref().map_or_else(String::new, |at| {
        format!("<a href=\"{}\">Sign out</a>", escape(at))
    });
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} \u{2014} Dashboard</title><style>{STYLE}</style></head><body>\
         <header><a href=\"/\">Dashboard</a><p data-vm-user=\"{}\">Signed in as {who}</p>{sign_out}</header>\
         <main>{main}</main></body></html>\n",
        escape(&viewer.user.id)
    );
    let mut response = Response::new(Full::new(Bytes::from(html)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    own_headers(headers);
    if status == StatusCode::SERVICE_UNAVAILABLE {
        headers.insert(RETRY_AFTER, HeaderValue::from_static(RETRY_AFTER_SECONDS));
    }
    response
}

/// `GET /`: the Applications of this page, those contributed to first.
pub(crate) fn list(
    viewer: &Viewer<'_>,
    blocks: &[Block<'_>],
    page_number: i64,
    more: bool,
) -> Answer {
    let mut main = String::from("<h1>Your Applications</h1>");
    let (mine, shared): (Vec<&Block<'_>>, Vec<&Block<'_>>) = blocks
        .iter()
        .partition(|b| b.relation == Some("contributor"));
    for (heading, group) in [("You contribute to", mine), ("Shared with you", shared)] {
        if group.is_empty() {
            continue;
        }
        main.push_str(&format!("<section><h2>{heading}</h2><ul class=\"apps\">"));
        for block in group {
            main.push_str("<li>");
            application_block(&mut main, block, 3);
            main.push_str("</li>");
        }
        main.push_str("</ul></section>");
    }
    if blocks.is_empty() {
        if page_number == 1 {
            main.push_str(
                "<p>Nothing is shared with you yet, and you contribute to no Application.</p>\
                 <p>When someone shares an Application with you, it shows here. An Application is made with the command-line tool <code>vm</code>: a Coding agent runs it for you.</p>",
            );
        } else {
            main.push_str("<p>There is no Application on this page.</p>");
        }
    }
    if page_number > 1 || more {
        main.push_str("<nav aria-label=\"Pages\">");
        if page_number > 1 {
            main.push_str(&format!(
                "<a data-vm-previous href=\"/?page={}\">\u{2190} Previous page</a>",
                page_number - 1
            ));
        }
        if more {
            main.push_str(&format!(
                "<a data-vm-next href=\"/?page={}\">Next page \u{2192}</a>",
                page_number + 1
            ));
        }
        main.push_str("</nav>");
    }
    page(StatusCode::OK, viewer, "Your Applications", &main)
}

/// One Application's block: `level` is its heading's.
fn application_block(out: &mut String, block: &Block<'_>, level: u8) {
    let sub = level + 1;
    let name = escape(block.name);
    out.push_str(&format!(
        "<article class=\"app\" data-vm-application=\"{name}\""
    ));
    if let Some(relation) = block.relation {
        out.push_str(&format!(" data-vm-relation=\"{relation}\""));
    }
    match block.serving {
        Some(number) => {
            out.push_str(&format!(" data-vm-serving=\"{number}\">"));
        }
        None => out.push_str(" data-vm-serving=\"none\">"),
    }
    out.push_str(&format!("<h{level}>{name}</h{level}>"));
    if let Some(description) = block.description {
        out.push_str(&format!(
            "<p data-vm-description>{}</p>",
            escape(description)
        ));
    }
    match (&block.open, block.serving) {
        (Some(open), Some(number)) => {
            out.push_str(&format!("<p><a class=\"open\" data-vm-open href=\"{}\">Open {name}</a> <span class=\"muted\">Version {number}</span></p>",
                escape(open)
            ));
        }
        _ => out.push_str("<p class=\"muted\">Nothing is deployed yet: no Version serves it.</p>"),
    }
    if let Some(address) = &block.address {
        out.push_str(&format!("<p>Address: <code>{}</code></p>", escape(address)));
    }
    if block.details {
        out.push_str(&format!(
            "<p><a data-vm-details href=\"/applications/{name}\">Details and sharing</a></p>"
        ));
    }
    out.push_str(&format!("<h{sub}>Contributors</h{sub}><ul>"));
    for person in &block.contributors {
        let (kind, id) = person.kind();
        out.push_str(&format!(
            "<li data-vm-contributor=\"{kind}\" data-vm-id=\"{}\">{}</li>",
            escape(id.unwrap_or_default()),
            person.words(false)
        ));
    }
    if block.more_contributors > 0 {
        out.push_str(&format!(
            "<li data-vm-more-contributors=\"{0}\">and {0} more</li>",
            block.more_contributors
        ));
    }
    out.push_str("</ul>");
    if block.serving.is_some()
        && let Some(declared) = block.declared
        && !declared.is_empty()
    {
        out.push_str(&format!(
            "<h{sub}>Resources it uses</h{sub}><ul class=\"resources\">"
        ));
        for (resource, level) in declared {
            if let Some(seen) = block.seen.get(resource.as_str()) {
                resource_item(out, resource, *level, seen);
            }
        }
        out.push_str("</ul>");
    }
    out.push_str("</article>");
}

/// One declared Resource and the User's standing with it, in words with a
/// sign, and whom to ask where they hold too little.
fn resource_item(out: &mut String, name: &str, declared: AccessLevel, seen: &Seen) {
    let declared_name = level_name(declared);
    let (yours, standing) = match (&seen.resource, seen.held) {
        (None, _) => ("none", "not-registered"),
        (Some(_), None) => ("unknown", "unknown"),
        (Some(_), Some(None)) => ("none", "none"),
        (Some(_), Some(Some(held))) if held >= declared => (level_name(held), "enough"),
        (Some(_), Some(Some(held))) => (level_name(held), "lower"),
    };
    let ask = match (&seen.resource, standing) {
        (Some(resource), "lower" | "none") => Some(resource.contact.as_str()),
        _ => None,
    };
    out.push_str(&format!("<li class=\"{standing}\" data-vm-resource=\"{}\" data-vm-declared=\"{declared_name}\" data-vm-yours=\"{yours}\" data-vm-standing=\"{standing}\"",
        escape(name)
    ));
    if let Some(ask) = ask {
        out.push_str(&format!(" data-vm-ask=\"{}\"", escape(ask)));
    }
    out.push_str(&format!(
        "><code>{}</code> (needs <b>{declared_name}</b>): ",
        escape(name)
    ));
    let ask_words = ask.map_or_else(String::new, |a| format!(" Ask {}.", escape(a)));
    match standing {
        "enough" => {
            out.push_str(&format!("<span class=\"mark\">\u{2713} Enough</span>: you hold <b>{yours}</b>."
            ));
        }
        "lower" => {
            out.push_str(&format!("<span class=\"mark\">! Too low</span>: you hold <b>{yours}</b>, so what needs more will be refused for you.{ask_words}"
            ));
        }
        "none" => {
            out.push_str(&format!("<span class=\"mark\">\u{2717} No access</span>: you hold no Grant on it, so this part of the Application will be refused for you.{ask_words}"
            ));
        }
        "not-registered" => out.push_str(
            "<span class=\"mark\">? Not registered</span>: no Resource of this name is registered, so this part works for nobody yet.",
        ),
        _ => out.push_str(
            "<span class=\"mark\">\u{2026} Cannot tell</span>: whether you hold a Grant on it cannot be told just now. Look again shortly.",
        ),
    }
    out.push_str("</li>");
}

/// `GET /applications/<name>`: the Application; whom it is shared with
/// (`shared`) to a Contributor and a Platform administrator; the forms to
/// a Contributor.
pub(crate) fn application(
    viewer: &Viewer<'_>,
    block: &Block<'_>,
    shared: Option<&[Person<'_>]>,
    contributor: bool,
    notice: Option<Notice>,
) -> Answer {
    let name = escape(block.name);
    let action = format!("/applications/{name}/sharing");
    let mut main = String::new();
    if let Some(notice) = notice {
        let (value, words) = match notice {
            Notice::Shared => (
                "shared",
                "Shared. The people it is shared with find it on their Dashboard and can open it now.",
            ),
            Notice::Unshared => (
                "unshared",
                "Unshared. Whoever it was shared with that way no longer finds it or opens it.",
            ),
            Notice::Nothing => ("nothing", "Nothing changed: it was so already."),
        };
        main.push_str(&format!(
            "<p class=\"notice\" role=\"status\" data-vm-notice=\"{value}\">{words}</p>"
        ));
    }
    application_block(&mut main, block, 1);
    main.push_str("<section><h2>Who can open it</h2>");
    match shared {
        None => main.push_str(
            "<p>Its Contributors can. Only its Contributors see whom else it is shared with.</p>",
        ),
        Some([]) => {
            main.push_str("<p>It is shared with nobody: only its Contributors can open it.</p>")
        }
        Some(entries) => {
            main.push_str("<p>Its Contributors, and:</p><ul>");
            for person in entries {
                let (kind, id) = person.kind();
                main.push_str(&format!("<li data-vm-shared=\"{kind}\""));
                if let Some(id) = id {
                    main.push_str(&format!(" data-vm-id=\"{}\"", escape(id)));
                }
                main.push_str(&format!(">{}", person.words(true)));
                if kind == "user" && person.name.is_none() {
                    main.push_str(" <span class=\"muted\">(has not signed in here)</span>");
                }
                if contributor {
                    main.push_str(&format!(
                        "<form data-vm-form=\"unshare\" method=\"post\" action=\"{action}\">\
                         <input type=\"hidden\" name=\"action\" value=\"unshare\">\
                         <input type=\"hidden\" name=\"kind\" value=\"{kind}\">\
                         <input type=\"hidden\" name=\"value\" value=\"{}\">\
                         <button type=\"submit\">Unshare</button></form>",
                        escape(id.unwrap_or_default())
                    ));
                }
                main.push_str("</li>");
            }
            main.push_str("</ul>");
        }
    }
    if contributor {
        main.push_str(&format!("<h2>Share it</h2>\
             <form class=\"share\" data-vm-form=\"share\" method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"action\" value=\"share\">\
             <label for=\"kind\">With</label>\
             <select id=\"kind\" name=\"kind\">\
             <option value=\"email\">a person, by email address</option>\
             <option value=\"user\">a person, by user id</option>\
             <option value=\"group\">a Group</option>\
             <option value=\"everyone\">everyone who can sign in</option></select>\
             <label for=\"value\">Email address, user id or Group</label>\
             <input id=\"value\" type=\"text\" name=\"value\" autocomplete=\"off\" spellcheck=\"false\">\
             <p class=\"muted\">Leave it empty for everyone. Contributors are changed with the <code>vm</code> tool.</p>\
             <button type=\"submit\">Share</button></form>"
        ));
    } else if shared.is_some() {
        main.push_str("<p>Only its Contributors change whom it is shared with.</p>");
    }
    main.push_str("</section><p><a href=\"/\">\u{2190} All your Applications</a></p>");
    page(StatusCode::OK, viewer, &name, &main)
}

/// `303` to `location`, on the Dashboard.
pub(crate) fn see_other(location: &str) -> Answer {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::SEE_OTHER;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    own_headers(headers);
    match HeaderValue::from_str(location) {
        Ok(value) => {
            headers.insert(LOCATION, value);
        }
        Err(_) => *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR,
    }
    response
}

/// A change of sharing refused: the API's status and reason, the words for
/// a person, and the way back to the Application's page.
pub(crate) fn refused(viewer: &Viewer<'_>, application: &str, refusal: &Refusal) -> Answer {
    let name = escape(application);
    let (status, reason, words) = match refusal {
        Refusal::NotFound => (
            StatusCode::NOT_FOUND,
            "not-found",
            format!("There is no Application named <code>{name}</code>, so nothing was shared."),
        ),
        Refusal::NotAContributor => (
            StatusCode::FORBIDDEN,
            "not-a-contributor",
            "Only the Application's Contributors change whom it is shared with, and you are not one of them. Nothing was changed: ask one of its Contributors.".to_owned(),
        ),
        Refusal::Malformed(problem) => (
            StatusCode::BAD_REQUEST,
            "malformed",
            format!("{} Nothing was changed: fill in the form again.", escape(problem)),
        ),
        Refusal::UnknownEmail(email) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown-email",
            format!(
                "Nobody who signed in here has the email address <code>{}</code>. Share with the person's user id instead (they see it when they sign in to the Dashboard), or ask them to sign in here once. Nothing was changed.",
                escape(email)
            ),
        ),
        Refusal::AmbiguousEmail(email) => (
            StatusCode::CONFLICT,
            "ambiguous-email",
            format!(
                "More than one person who signed in here has the email address <code>{}</code>. Share with the user id of the one you mean instead. Nothing was changed.",
                escape(email)
            ),
        ),
        Refusal::TooMany => (
            StatusCode::CONFLICT,
            "too-many",
            format!(
                "It is shared with {} entries already, the most it can be. Unshare some first, or share with a Group in place of many people. Nothing was changed.",
                thousands(MAX_SHARED_WITH)
            ),
        ),
        Refusal::LastContributor => (
            StatusCode::CONFLICT,
            "last-contributor",
            "That would leave the Application with no Contributor. Nothing was changed.".to_owned(),
        ),
        Refusal::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The platform could not make this change just now, and cannot tell yet whether it was made. Look at the Application's page again shortly before trying again.".to_owned(),
        ),
        Refusal::TimedOut => (
            StatusCode::REQUEST_TIMEOUT,
            "timed-out",
            "The form did not arrive in time. Nothing was changed: send it again.".to_owned(),
        ),
    };
    let main = format!(
        "<h1>Not changed</h1><p class=\"error\" role=\"alert\" data-vm-error=\"{reason}\">{words}</p>\
         <p><a data-vm-back href=\"/applications/{name}\">\u{2190} Back to {name}</a></p>"
    );
    page(status, viewer, "Not changed", &main)
}

/// `1,000`.
fn thousands(n: i64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, c) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `403`: the Application is not shared with the person. Names no
/// Contributor and nothing else of it.
pub(crate) fn not_shared(viewer: &Viewer<'_>, application: &str) -> Answer {
    let name = escape(application);
    page(
        StatusCode::FORBIDDEN,
        viewer,
        "Not shared with you",
        &format!(
            "<h1>Not shared with you</h1><p>The Application <code>{name}</code> is not shared with you.</p>\
             <p>To open it or see it here, ask one of its Contributors to share it with you.</p>\
             <p><a href=\"/\">\u{2190} All your Applications</a></p>"
        ),
    )
}

/// `404`: no Application of that name.
pub(crate) fn no_application(viewer: &Viewer<'_>, application: &str) -> Answer {
    page(
        StatusCode::NOT_FOUND,
        viewer,
        "No such Application",
        &format!(
            "<h1>No such Application</h1><p>There is no Application named <code>{}</code>.</p>\
             <p><a href=\"/\">\u{2190} All your Applications</a></p>",
            escape(application)
        ),
    )
}

/// `404` of a path the Dashboard has not.
pub(crate) fn not_found(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::NOT_FOUND,
        viewer,
        "Not found",
        "<h1>Not found</h1><p>The Dashboard has no such page.</p><p><a href=\"/\">\u{2190} All your Applications</a></p>",
    )
}

/// `405`, with `allow`.
pub(crate) fn method_not_allowed(viewer: &Viewer<'_>, allow: &'static str) -> Answer {
    let mut response = page(
        StatusCode::METHOD_NOT_ALLOWED,
        viewer,
        "Method not allowed",
        "<h1>Method not allowed</h1><p>This address of the Dashboard does not take that method.</p><p><a href=\"/\">\u{2190} All your Applications</a></p>",
    );
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static(allow));
    response
}

/// `400`: a `page` that is not a whole number from 1 to 10000.
pub(crate) fn bad_page(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::BAD_REQUEST,
        viewer,
        "No such page",
        "<h1>No such page</h1><p>A page of the list is a whole number from 1 to 10000.</p><p><a href=\"/\">\u{2190} The first page</a></p>",
    )
}

/// `503`: the platform database cannot be reached.
pub(crate) fn unavailable(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        "Try again shortly",
        "<h1>Try again shortly</h1><p>The Dashboard cannot be shown just now. Try again in a few seconds.</p>",
    )
}
