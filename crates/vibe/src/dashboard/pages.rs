//! The Dashboard's pages: HTML of the platform's own, with the headers
//! every page of the platform's own carries (crate::oidc::pages's
//! `own_headers`: `no-store`, `nosniff`, `same-origin`, and a policy with
//! `default-src 'none'`, `frame-ancestors 'none'` and `form-action
//! 'self'`), the one theme of the platform's pages (crate::theme), no
//! script, no outside resource.
//! Everything that is not the platform's own text is escaped as HTML
//! (crate::oidc::pages's `escape`), in text and in attributes alike.
//!
//! The `data-vm-...` attributes are what the contract names for a test (and
//! a Coding agent) to read; the words beside them say the same for a
//! person, and a Resource's standing is said in words with a sign, never in
//! colour alone.
//!
//! **Size.** Bounded by what a page shows: at most 100 Applications, each
//! with at most 20 Contributors (one Application's page: at most 100, and
//! at most 1,000 entries of whom it is shared with, each with its unshare
//! form), each an id or Group of at most 256 bytes and a name and email of
//! at most 256 each, at most six times longer once escaped (`&quot;`); and
//! the Resources each serving Version declares. So about 10 MiB at worst
//! for either page, plus the declared Resources; measured with ids of 15
//! bytes: 222 KiB for 100 Applications of 30 Contributors, 401 KiB for one
//! shared with 1,000 (/tmp/t44-bench/result.json).

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
use crate::theme::{self, CHEVRON, Layout, Mark, Place};
use crate::user::User;

type Answer = Response<Full<Bytes>>;

/// Whom a page is for: the User, their name when the platform knows it,
/// and the sign-in address's sign-out with OIDC.
pub(crate) struct Viewer<'a> {
    pub user: &'a User,
    pub name: Option<String>,
    pub sign_out: Option<String>,
}

impl Viewer<'_> {
    /// Whether the platform keeps a record of people here: with OIDC (whose
    /// pages link the sign-out); the stand-in keeps none.
    pub fn records_people(&self) -> bool {
        self.sign_out.is_some()
    }
}

/// A person, a Group or everyone, as a page shows them.
pub(crate) struct Person<'a> {
    named: &'a Named,
    name: Option<&'a str>,
    email: Option<&'a str>,
    /// A person the platform has never seen: it keeps a record of people
    /// (OIDC) and knows neither a name nor an email of them.
    unseen: bool,
}

impl<'a> Person<'a> {
    /// `records`: whether the platform keeps a record of people here
    /// ([`Viewer::records_people`]).
    pub fn of(
        named: &'a Named,
        name: Option<&'a str>,
        email: Option<&'a str>,
        records: bool,
    ) -> Self {
        let unseen =
            records && matches!(named, Named::User(_)) && name.is_none() && email.is_none();
        Self {
            named,
            name,
            email,
            unseen,
        }
    }

    /// The attributes of a person's element beyond its kind and id:
    /// `data-vm-unseen` for one never seen.
    fn marks(&self) -> &'static str {
        if self.unseen { " data-vm-unseen" } else { "" }
    }

    /// `user`, `group` or `everyone`, and the id or the Group.
    fn kind(&self) -> (&'static str, Option<&'a str>) {
        match self.named {
            Named::User(id) => ("user", Some(id)),
            Named::Group(group) => ("group", Some(group)),
            Named::Everyone => ("everyone", None),
        }
    }

    /// The words: a person by what is known of them (name, and email when
    /// asked for or when there is no name) and id, or that they were not
    /// seen signing in here yet, as the tool says it
    /// (crate::tool::sharing's `entry_words`); a Group as a Group,
    /// everyone as everyone.
    /// The name, louder than the identifier after it: the name (600), the
    /// email, then the id small and soft.
    fn words(&self, with_email: bool) -> String {
        match self.named {
            Named::User(id) => {
                let mut text = String::new();
                if let Some(name) = self.name {
                    text.push_str(&format!("<span class=\"name\">{}</span> ", escape(name)));
                }
                if (with_email || self.name.is_none())
                    && let Some(email) = self.email
                {
                    let class = if self.name.is_none() { "name" } else { "soft" };
                    text.push_str(&format!(
                        "<span class=\"{class}\">&lt;{}&gt;</span> ",
                        escape(email)
                    ));
                }
                text.push_str(&format!("<code class=\"id\">{}</code>", escape(id)));
                if self.unseen {
                    text.push_str(
                        " <span class=\"soft small\">(not seen signing in here yet: check the id)</span>",
                    );
                }
                text
            }
            Named::Group(group) => format!(
                "<span class=\"name\">the Group <code>{}</code></span>",
                escape(group)
            ),
            Named::Everyone => {
                "<span class=\"name\">everyone who can sign in to this installation</span>"
                    .to_owned()
            }
        }
    }

    /// Whom an unshare button names, for a reader who hears it: the name,
    /// else the email, else the id; the Group; everyone.
    fn object(&self) -> String {
        match self.named {
            Named::User(id) => escape(self.name.or(self.email).unwrap_or(id)),
            Named::Group(group) => format!("the Group {}", escape(group)),
            Named::Everyone => "everyone".to_owned(),
        }
    }
}

/// One Application as the list and its own page show it.
pub(crate) struct Block<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    /// `contributor`, `shared`, or `none` for a Platform administrator it
    /// is not shared with.
    pub relation: &'static str,
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
fn page(
    status: StatusCode,
    viewer: &Viewer<'_>,
    title: &str,
    layout: Layout,
    main: &str,
) -> Answer {
    let id = escape(&viewer.user.id);
    let person = match &viewer.name {
        Some(name) => format!(
            "<span class=\"name\">{}</span> <code class=\"id\">{id}</code>",
            escape(name)
        ),
        None => format!("<code class=\"id\">{id}</code>"),
    };
    let sign_out = viewer.sign_out.as_deref().map_or_else(String::new, |at| {
        format!(
            "<a class=\"btn quiet compact\" href=\"{}\">Sign out</a>",
            escape(at)
        )
    });
    let who = format!(
        "<div class=\"who\"><p class=\"person\" data-vm-user=\"{id}\"><span class=\"vh\">Signed in as </span>{person}</p>{sign_out}</div>"
    );
    let html = theme::page(
        &format!("{title} \u{2014} Dashboard"),
        Place::Dashboard { who: &who },
        layout,
        main,
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
    let mut main = String::from("<h1 class=\"page-title\">Your Applications</h1>");
    let (mine, shared): (Vec<&Block<'_>>, Vec<&Block<'_>>) =
        blocks.iter().partition(|b| b.relation == "contributor");
    for (heading, group) in [("You contribute to", mine), ("Shared with you", shared)] {
        if group.is_empty() {
            continue;
        }
        main.push_str(&format!(
            "<section class=\"group\"><h2 class=\"over\">{heading}</h2><ul class=\"apps\">"
        ));
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
                "<div class=\"empty\"><p class=\"first\">Nothing is shared with you yet, and you contribute to no Application.</p>\
                 <p>When someone shares an Application with you, it shows here. An Application is made with the command-line tool <code>vibe</code>: a Coding agent runs it for you.</p></div>",
            );
        } else {
            main.push_str(
                "<div class=\"empty\"><p class=\"first\">There is no Application on this page.</p></div>",
            );
        }
    }
    if page_number > 1 || more {
        main.push_str("<nav class=\"pages\" aria-label=\"Pages\">");
        if page_number > 1 {
            main.push_str(&format!(
                "<a class=\"btn\" data-vm-previous href=\"/?page={}\">\u{2190} Previous page</a>",
                page_number - 1
            ));
        }
        if more {
            main.push_str(&format!(
                "<a class=\"btn next\" data-vm-next href=\"/?page={}\">Next page \u{2192}</a>",
                page_number + 1
            ));
        }
        main.push_str("</nav>");
    }
    page(
        StatusCode::OK,
        viewer,
        "Your Applications",
        Layout::Column,
        &main,
    )
}

/// One Application's block: `level` is its heading's.
/// In the list (`level` 3) it is a card; on its own page (`level` 1) it is
/// the page's head and sections.
fn application_block(out: &mut String, block: &Block<'_>, level: u8) {
    let sub = level + 1;
    let whole = level == 1;
    let name = escape(block.name);
    let class = if whole { "app whole" } else { "app card" };
    out.push_str(&format!(
        "<article class=\"{class}\" data-vm-application=\"{name}\""
    ));
    out.push_str(&format!(" data-vm-relation=\"{}\"", block.relation));
    match block.serving {
        Some(number) => {
            out.push_str(&format!(" data-vm-serving=\"{number}\">"));
        }
        None => out.push_str(" data-vm-serving=\"none\">"),
    }
    out.push_str(&format!(
        "<div class=\"app-head\"><h{level}>{name}</h{level}>"
    ));
    if let (Some(_), Some(number)) = (&block.open, block.serving) {
        out.push_str(&format!("<span class=\"pill\">Version {number}</span>"));
    }
    out.push_str("</div>");
    if let Some(description) = block.description {
        out.push_str(&format!(
            "<p class=\"desc\" data-vm-description>{}</p>",
            escape(description)
        ));
    }
    if let Some(address) = &block.address {
        out.push_str(&format!(
            "<p class=\"small soft\">Address: <code class=\"id\">{}</code></p>",
            escape(address)
        ));
    }
    match (&block.open, block.serving) {
        (Some(_), Some(_)) => {}
        (None, Some(number)) => {
            out.push_str(&format!(
                "<p class=\"soft\">Version {number} serves it.</p>"
            ));
        }
        (_, None) => {
            out.push_str("<p class=\"soft\">Nothing is deployed yet: no Version serves it.</p>")
        }
    }
    if block.relation == "none" {
        out.push_str(&theme::notice(
            Mark::Info,
            "",
            "You read this page as a Platform administrator. It is not shared with you, so you cannot open it: ask one of its Contributors to share it with you if you need to.",
        ));
    }
    let open = match (&block.open, block.serving) {
        (Some(open), Some(_)) => Some(format!(
            "<a class=\"btn\" data-vm-open href=\"{}\">Open {name}</a>",
            escape(open)
        )),
        _ => None,
    };
    if open.is_some() || block.details {
        out.push_str("<div class=\"actions\">");
        if let Some(open) = &open {
            out.push_str(open);
        }
        if block.details {
            out.push_str(&format!(
                "<a class=\"btn quiet\" data-vm-details href=\"/applications/{name}\">Details and sharing</a>"
            ));
        }
        out.push_str("</div>");
    }
    let heading = if whole { "" } else { " class=\"over\"" };
    out.push_str(&format!(
        "<div class=\"meta\"><h{sub}{heading}>Contributors</h{sub}><ul class=\"inline-people\">"
    ));
    for person in &block.contributors {
        let (kind, id) = person.kind();
        out.push_str(&format!(
            "<li data-vm-contributor=\"{kind}\" data-vm-id=\"{}\"{}>{}</li>",
            escape(id.unwrap_or_default()),
            person.marks(),
            person.words(false)
        ));
    }
    if block.more_contributors > 0 {
        out.push_str(&format!(
            "<li class=\"soft\" data-vm-more-contributors=\"{0}\">and {0} more</li>",
            block.more_contributors
        ));
    }
    out.push_str("</ul></div>");
    if block.serving.is_some()
        && let Some(declared) = block.declared
        && !declared.is_empty()
    {
        out.push_str(&format!(
            "<div class=\"meta\"><h{sub}{heading}>Resources it uses</h{sub}><ul class=\"resources\">"
        ));
        for (resource, level) in declared {
            if let Some(seen) = block.seen.get(resource.as_str()) {
                resource_item(out, resource, *level, seen);
            }
        }
        out.push_str("</ul></div>");
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
        "><code>{}</code> <span class=\"soft\">(needs <b>{declared_name}</b>)</span> ",
        escape(name)
    ));
    let ask_words = ask.map_or_else(String::new, |a| format!(" Ask {}.", escape(a)));
    let (mark, words) = match standing {
        "enough" => ("\u{2713} Enough", format!("You hold <b>{yours}</b>.")),
        "lower" => (
            "! Too low",
            format!(
                "You hold <b>{yours}</b>, so what needs more will be refused for you.{ask_words}"
            ),
        ),
        "none" => (
            "\u{2717} No access",
            format!(
                "You hold no Grant on it, so this part of the Application will be refused for you.{ask_words}"
            ),
        ),
        "not-registered" => (
            "? Not registered",
            "No Resource of this name is registered, so this part works for nobody yet.".to_owned(),
        ),
        _ => (
            "\u{2026} Cannot tell",
            "Whether you hold a Grant on it cannot be told just now. Look again shortly."
                .to_owned(),
        ),
    };
    out.push_str(&format!(
        "<span class=\"st\">{mark}</span><span class=\"words\">{words}</span></li>"
    ));
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
    let mut main = String::from(
        "<nav class=\"crumbs\" aria-label=\"Back\"><a class=\"btn quiet compact back\" href=\"/\">\u{2190} All your Applications</a></nav>",
    );
    if let Some(notice) = notice {
        let (value, mark, words) = match notice {
            Notice::Shared => (
                "shared",
                Mark::Ok,
                "Shared. The people it is shared with find it on their Dashboard and can open it now.",
            ),
            Notice::Unshared => (
                "unshared",
                Mark::Ok,
                "Unshared. Whoever it was shared with that way no longer finds it or opens it.",
            ),
            Notice::Nothing => ("nothing", Mark::Info, "Nothing changed: it was so already."),
        };
        main.push_str(&theme::notice(
            mark,
            &format!("role=\"status\" data-vm-notice=\"{value}\""),
            words,
        ));
    }
    application_block(&mut main, block, 1);
    main.push_str("<section class=\"section\"><h2>Who can open it</h2>");
    match shared {
        None => main.push_str(
            "<p class=\"soft\">Its Contributors can. Only its Contributors see whom else it is shared with.</p>",
        ),
        Some([]) => main.push_str(
            "<p class=\"soft\">It is shared with nobody: only its Contributors can open it.</p>",
        ),
        Some(entries) => {
            main.push_str("<p class=\"soft\">Its Contributors, and:</p><ul class=\"rows\">");
            for person in entries {
                let (kind, id) = person.kind();
                main.push_str(&format!("<li data-vm-shared=\"{kind}\""));
                if let Some(id) = id {
                    main.push_str(&format!(" data-vm-id=\"{}\"", escape(id)));
                }
                main.push_str(&format!(
                    "{}><span class=\"who-is\">{}</span>",
                    person.marks(),
                    person.words(true)
                ));
                if contributor {
                    main.push_str(&format!(
                        "<form data-vm-form=\"unshare\" method=\"post\" action=\"{action}\">\
                         <input type=\"hidden\" name=\"action\" value=\"unshare\">\
                         <input type=\"hidden\" name=\"kind\" value=\"{kind}\">\
                         <input type=\"hidden\" name=\"value\" value=\"{}\">\
                         <button type=\"submit\" class=\"quiet danger\">Unshare<span class=\"vh\"> {}</span></button></form>",
                        escape(id.unwrap_or_default()),
                        person.object()
                    ));
                }
                main.push_str("</li>");
            }
            main.push_str("</ul>");
        }
    }
    if !contributor && shared.is_some() {
        main.push_str("<p class=\"soft\">Only its Contributors change whom it is shared with.</p>");
    }
    main.push_str("</section>");
    if contributor {
        main.push_str(&format!("<section class=\"section\"><h2>Share it</h2>\
             <form class=\"share card\" data-vm-form=\"share\" method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"action\" value=\"share\">\
             <div class=\"field\"><label for=\"kind\">With</label>\
             <div class=\"control\"><select id=\"kind\" name=\"kind\">\
             <option value=\"email\">a person, by email address</option>\
             <option value=\"user\">a person, by user id</option>\
             <option value=\"group\">a Group</option>\
             <option value=\"everyone\">everyone who can sign in</option></select>{CHEVRON}</div></div>\
             <div class=\"field\"><label for=\"value\">Email address, user id or Group</label>\
             <p class=\"help\" id=\"value-help\">Leave it empty for everyone. Contributors are changed with the <code>vibe</code> tool.</p>\
             <div class=\"control\"><input id=\"value\" type=\"text\" name=\"value\" autocomplete=\"off\" spellcheck=\"false\" aria-describedby=\"value-help\"></div></div>\
             <div class=\"form-actions\"><button type=\"submit\" class=\"primary\">Share {name}</button></div></form></section>"
        ));
    }
    page(StatusCode::OK, viewer, &name, Layout::Column, &main)
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
        "<h1>Not changed</h1>{}\
         <div class=\"actions\"><a class=\"btn primary\" data-vm-back href=\"/applications/{name}\">\u{2190} Back to {name}</a></div>",
        theme::notice(
            Mark::Error,
            &format!("role=\"alert\" data-vm-error=\"{reason}\""),
            &words
        )
    );
    page(status, viewer, "Not changed", Layout::Card, &main)
}

/// The way back from a page that says one thing: the list.
const BACK: &str = "<div class=\"actions\"><a class=\"btn primary\" href=\"/\">\u{2190} All your Applications</a></div>";

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
        Layout::Card,
        &format!(
            "<h1>Not shared with you</h1><p>The Application <code>{name}</code> is not shared with you.</p>\
             <p class=\"soft\">To open it or see it here, ask one of its Contributors to share it with you.</p>\
             {BACK}"
        ),
    )
}

/// `404`: no Application of that name.
pub(crate) fn no_application(viewer: &Viewer<'_>, application: &str) -> Answer {
    page(
        StatusCode::NOT_FOUND,
        viewer,
        "No such Application",
        Layout::Card,
        &format!(
            "<h1>No such Application</h1><p>There is no Application named <code>{}</code>.</p>{BACK}",
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
        Layout::Card,
        &format!("<h1>Not found</h1><p>The Dashboard has no such page.</p>{BACK}"),
    )
}

/// `405`, with `allow`.
pub(crate) fn method_not_allowed(viewer: &Viewer<'_>, allow: &'static str) -> Answer {
    let mut response = page(
        StatusCode::METHOD_NOT_ALLOWED,
        viewer,
        "Method not allowed",
        Layout::Card,
        &format!(
            "<h1>Method not allowed</h1><p>This address of the Dashboard does not take that method.</p>{BACK}"
        ),
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
        Layout::Card,
        "<h1>No such page</h1><p>A page of the list is a whole number from 1 to 10000.</p><div class=\"actions\"><a class=\"btn primary\" href=\"/\">\u{2190} The first page</a></div>",
    )
}

/// `503`: the platform database cannot be reached.
pub(crate) fn unavailable(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        "Try again shortly",
        Layout::Card,
        "<h1>Try again shortly</h1><p>The Dashboard cannot be shown just now. Try again in a few seconds.</p>",
    )
}
