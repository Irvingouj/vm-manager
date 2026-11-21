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
//! a Coding agent) to read; the page says the same for a person, by the
//! product owner's four rules (`.claude/skills/ui-ux/SKILL.md`): each thing
//! once, a mark and a colour before words (a state's mark always with its
//! words beside it or hidden for a reader who hears the page, never colour
//! alone), the list holding only what is needed to choose and open an
//! Application and its own page the rest.
//!
//! **Size.** Bounded by what a page shows: at most 100 Applications, each
//! a name, a description and its address (one Application's page: at most
//! 100 Contributors, and at most 1,000 entries of whom it is shared with,
//! each with its unshare form), each an id or Group of at most 256 bytes
//! and a name and email of at most 256 each, at most six times longer once
//! escaped (`&quot;`); and the Resources each serving Version declares.
//! So about 10 MiB at worst for the Application's page, plus the declared
//! Resources, and less for the list; measured before the list lost its
//! Contributors, with ids of 15 bytes: 222 KiB for 100 Applications of 30
//! Contributors, 401 KiB for one shared with 1,000
//! (/tmp/t44-bench/result.json).

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
use crate::theme::{self, CHEVRON, Icon, Layout, Mark, Place};
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

    /// A person as a row shows them: the mark of their kind (a person, a
    /// Group, everyone), then what is known of them, the name first and
    /// loudest, the email and the id small beneath it; for one the
    /// platform has never seen, a mark and words saying so and to check
    /// the id, as the tool says it (crate::tool::sharing's `entry_words`).
    fn row(&self) -> String {
        match self.named {
            Named::User(id) => {
                let mut lines = String::new();
                let id = escape(id);
                match (self.name, self.email) {
                    (Some(name), email) => {
                        lines.push_str(&format!("<span class=\"name\">{}</span>", escape(name)));
                        lines.push_str("<span class=\"more\">");
                        if let Some(email) = email {
                            lines.push_str(&escape(email));
                        }
                        lines.push_str(&format!("<code class=\"id\">{id}</code></span>"));
                    }
                    (None, Some(email)) => {
                        lines.push_str(&format!(
                            "<span class=\"name\">{}</span><span class=\"more\"><code class=\"id\">{id}</code></span>",
                            escape(email)
                        ));
                    }
                    (None, None) => {
                        lines.push_str(&format!(
                            "<span class=\"name\"><code class=\"id\">{id}</code></span>"
                        ));
                    }
                }
                if self.unseen {
                    lines.push_str(&format!(
                        "<span class=\"unseen\">{}not seen signing in here yet: check the id</span>",
                        Icon::Warn.svg()
                    ));
                }
                format!(
                    "{}<span class=\"lines\">{lines}</span>",
                    theme::icon(Icon::Person, "A person:")
                )
            }
            Named::Group(group) => format!(
                "{}<span class=\"lines\"><span class=\"name\">{}</span></span>",
                theme::icon(Icon::Group, "The Group"),
                escape(group)
            ),
            Named::Everyone => format!(
                "{}<span class=\"lines\"><span class=\"name\">Everyone who can sign in here</span></span>",
                Icon::Everyone.svg()
            ),
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
    /// Its Contributors, shown on its own page (the list shows none).
    pub contributors: Vec<Person<'a>>,
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
/// Each is a card that holds what is needed to choose and open it: its
/// name (the way to its own page), its description, its state, and the
/// way to open it. Everything else known of it is one step away, on its
/// own page.
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
            card(&mut main, block);
            main.push_str("</li>");
        }
        main.push_str("</ul></section>");
    }
    if blocks.is_empty() {
        if page_number == 1 {
            main.push_str(
                "<div class=\"empty\"><p class=\"first\">Nothing is shared with you yet, and you contribute to no Application.</p>\
                 <p>An Application is made with the command-line tool <code>vibe</code>: a Coding agent runs it for you.</p></div>",
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
                "<a class=\"btn\" data-vm-previous href=\"/?page={}\">{}Previous page</a>",
                page_number - 1,
                Icon::Back.svg()
            ));
        }
        if more {
            main.push_str(&format!(
                "<a class=\"btn next\" data-vm-next href=\"/?page={}\">Next page{}</a>",
                page_number + 1,
                Icon::Forward.svg()
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

/// The attributes every element of an Application carries, in the list
/// and on its own page.
fn application_attributes(block: &Block<'_>, name: &str) -> String {
    let serving = block
        .serving
        .map_or_else(|| "none".to_owned(), |number| number.to_string());
    format!(
        "data-vm-application=\"{name}\" data-vm-relation=\"{}\" data-vm-serving=\"{serving}\"",
        block.relation
    )
}

/// The mark of an Application no Version serves: a dashed circle and the
/// words.
fn not_deployed() -> String {
    format!(
        "<span class=\"pill\">{}Not deployed</span>",
        Icon::Empty.svg()
    )
}

/// The link that opens an Application at its address: the mark of
/// opening and the word, the name for a reader who hears it.
fn open_link(open: &str, name: &str, class: &str) -> String {
    format!(
        "<a class=\"{class}\" data-vm-open href=\"{}\">{}Open<span class=\"vh\"> {name}</span></a>",
        escape(open),
        Icon::Open.svg()
    )
}

/// One Application as a card of the list: its name (a link to its own
/// page), its state (not deployed; a part that will refuse the person),
/// its description, and the way to open it.
fn card(out: &mut String, block: &Block<'_>) {
    let name = escape(block.name);
    out.push_str(&format!(
        "<article class=\"app card\" {}><div class=\"text\"><div class=\"app-head\">\
         <h3><a class=\"name\" data-vm-details href=\"/applications/{name}\">{name}</a></h3>",
        application_attributes(block, &name)
    ));
    if block.serving.is_none() {
        out.push_str(&not_deployed());
    } else if let Some(mark) = worst_standing(block) {
        out.push_str(&mark);
    }
    out.push_str("</div>");
    if let Some(description) = block.description {
        out.push_str(&format!(
            "<p class=\"desc\" data-vm-description>{}</p>",
            escape(description)
        ));
    }
    out.push_str("</div>");
    if let (Some(open), Some(_)) = (&block.open, block.serving) {
        out.push_str(&open_link(open, &name, "btn"));
    }
    out.push_str("</article>");
}

/// The mark a card of the list carries when a part of the Application
/// will not work for the person: the worst of its Resources' standings
/// (whom to ask, and which part, are on its own page).
fn worst_standing(block: &Block<'_>) -> Option<String> {
    let declared = block.declared?;
    let mut worst = 0;
    for (resource, level) in declared {
        if let Some(seen) = block.seen.get(resource.as_str()) {
            let rank = match standing(*level, seen).1 {
                "lower" | "none" => 3,
                "not-registered" => 2,
                "unknown" => 1,
                _ => 0,
            };
            worst = worst.max(rank);
        }
    }
    let (class, icon, words) = match worst {
        3 => ("err", Icon::Cross, "Missing access"),
        2 => ("warn", Icon::Warn, "Not set up"),
        1 => ("", Icon::Pending, "Access unknown"),
        _ => return None,
    };
    Some(format!(
        "<span class=\"pill {class}\">{}{words}</span>",
        icon.svg()
    ))
}

/// What the User holds of a Resource declared at `declared`, as
/// `data-vm-yours` says it, and their standing (`data-vm-standing`).
fn standing(declared: AccessLevel, seen: &Seen) -> (&'static str, &'static str) {
    match (&seen.resource, seen.held) {
        (None, _) => ("none", "not-registered"),
        (Some(_), None) => ("unknown", "unknown"),
        (Some(_), Some(None)) => ("none", "none"),
        (Some(_), Some(Some(held))) if held >= declared => (level_name(held), "enough"),
        (Some(_), Some(Some(held))) => (level_name(held), "lower"),
    }
}

/// One Application on its own page: its name and Version, its
/// description, the way to open it beside its address, who can open it,
/// and the Resources it uses.
fn whole(out: &mut String, block: &Block<'_>, people: &str) {
    let name = escape(block.name);
    out.push_str(&format!(
        "<article class=\"app whole\" {}><div class=\"app-head\"><h1>{name}</h1>",
        application_attributes(block, &name)
    ));
    match block.serving {
        Some(number) => out.push_str(&format!(
            "<span class=\"pill ver\"><span aria-hidden=\"true\">v{number}</span><span class=\"vh\">Version {number}</span></span>"
        )),
        None => out.push_str(&not_deployed()),
    }
    out.push_str("</div>");
    if let Some(description) = block.description {
        out.push_str(&format!(
            "<p class=\"desc\" data-vm-description>{}</p>",
            escape(description)
        ));
    }
    out.push_str("<div class=\"actions\">");
    if let (Some(open), Some(_)) = (&block.open, block.serving) {
        out.push_str(&open_link(open, &name, "btn"));
    }
    if block.relation == "none" {
        out.push_str(&format!(
            "<p class=\"lockline\">{}<span>Not shared with you, so you cannot open it: you read this page as a Platform administrator.</span></p>",
            Icon::Lock.svg()
        ));
    }
    if let Some(address) = &block.address {
        out.push_str(&format!(
            "<code class=\"id address\">{}</code>",
            escape(address)
        ));
    }
    out.push_str("</div>");
    if block.serving.is_some()
        && let Some(declared) = block.declared
        && !declared.is_empty()
    {
        out.push_str(
            "<section class=\"section\"><h2>Resources it uses</h2><table class=\"res\">\
             <thead><tr><th scope=\"col\">Resource</th><th scope=\"col\">Needs</th><th scope=\"col\">You hold</th></tr></thead><tbody>",
        );
        for (resource, level) in declared {
            if let Some(seen) = block.seen.get(resource.as_str()) {
                resource_row(out, resource, *level, seen);
            }
        }
        out.push_str("</tbody></table></section>");
    }
    out.push_str(people);
    out.push_str("</article>");
}

/// One declared Resource and the User's standing with it: a mark in the
/// standing's colour (its name hidden beside it for a reader who hears
/// it), what it needs, what they hold, and whom to ask where they hold
/// too little.
fn resource_row(out: &mut String, name: &str, declared: AccessLevel, seen: &Seen) {
    let declared_name = level_name(declared);
    let (yours, standing) = standing(declared, seen);
    let ask = match (&seen.resource, standing) {
        (Some(resource), "lower" | "none") => Some(resource.contact.as_str()),
        _ => None,
    };
    out.push_str(&format!("<tr class=\"{standing}\" data-vm-resource=\"{}\" data-vm-declared=\"{declared_name}\" data-vm-yours=\"{yours}\" data-vm-standing=\"{standing}\"",
        escape(name)
    ));
    if let Some(ask) = ask {
        out.push_str(&format!(" data-vm-ask=\"{}\"", escape(ask)));
    }
    let (icon, said, held) = match standing {
        "enough" => (Icon::Check, "Enough:", yours.to_owned()),
        "lower" => (
            Icon::Warn,
            "Too low, so what needs more is refused for you:",
            yours.to_owned(),
        ),
        "none" => (
            Icon::Cross,
            "No access, so this part is refused for you:",
            "nothing".to_owned(),
        ),
        "not-registered" => (
            Icon::Unknown,
            "Works for nobody yet:",
            "not registered".to_owned(),
        ),
        _ => (Icon::Pending, "Cannot tell:", "cannot tell now".to_owned()),
    };
    out.push_str(&format!(
        "><td><div class=\"it\">{}<div><code>{}</code>",
        theme::icon(icon, said),
        escape(name)
    ));
    if let Some(ask) = ask {
        out.push_str(&format!("<p class=\"ask\">Ask {}.</p>", escape(ask)));
    }
    out.push_str(&format!(
        "</div></div></td><td class=\"lv\" data-label=\"needs\">{declared_name}</td><td class=\"lv\" data-label=\"you hold\">{held}</td></tr>"
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
    let mut main = format!(
        "<nav class=\"crumbs\" aria-label=\"Back\"><a class=\"btn quiet compact back\" href=\"/\">{}Your Applications</a></nav>",
        Icon::Back.svg()
    );
    if let Some(notice) = notice {
        let (value, mark, words) = match notice {
            Notice::Shared => ("shared", Mark::Ok, "Shared: they can open it now."),
            Notice::Unshared => (
                "unshared",
                Mark::Ok,
                "Unshared: whoever it was shared with that way can no longer open it.",
            ),
            Notice::Nothing => ("nothing", Mark::Info, "Nothing changed: it was so already."),
        };
        main.push_str(&theme::notice(
            mark,
            &format!("role=\"status\" data-vm-notice=\"{value}\""),
            words,
        ));
    }
    let mut people = String::from("<section class=\"section\"><h2>Who can open it</h2>");
    people.push_str("<div class=\"sub\"><h3 class=\"over\">Contributors</h3>");
    if contributor {
        people.push_str(
            "<p class=\"small soft\">Changed with the command-line tool <code>vibe</code></p>",
        );
    }
    people.push_str("</div><ul class=\"rows\">");
    for person in &block.contributors {
        let (kind, id) = person.kind();
        people.push_str(&format!(
            "<li data-vm-contributor=\"{kind}\" data-vm-id=\"{}\"{}><span class=\"who-is\">{}</span></li>",
            escape(id.unwrap_or_default()),
            person.marks(),
            person.row()
        ));
    }
    people.push_str("</ul>");
    match shared {
        None => people.push_str(&format!(
            "<p class=\"lockline after\">{}<span>Only its Contributors see whom else it is shared with.</span></p>",
            Icon::Lock.svg()
        )),
        Some([]) => {
            people.push_str("<p class=\"soft after\">Only its Contributors can open it.</p>");
        }
        Some(entries) => {
            people.push_str("<div class=\"sub\"><h3 class=\"over\">Shared with</h3></div><ul class=\"rows\">");
            for person in entries {
                let (kind, id) = person.kind();
                people.push_str(&format!("<li data-vm-shared=\"{kind}\""));
                if let Some(id) = id {
                    people.push_str(&format!(" data-vm-id=\"{}\"", escape(id)));
                }
                people.push_str(&format!(
                    "{}><span class=\"who-is\">{}</span>",
                    person.marks(),
                    person.row()
                ));
                if contributor {
                    people.push_str(&format!(
                        "<form data-vm-form=\"unshare\" method=\"post\" action=\"{action}\">\
                         <input type=\"hidden\" name=\"action\" value=\"unshare\">\
                         <input type=\"hidden\" name=\"kind\" value=\"{kind}\">\
                         <input type=\"hidden\" name=\"value\" value=\"{}\">\
                         <button type=\"submit\" class=\"quiet danger\">Unshare<span class=\"vh\"> {}</span></button></form>",
                        escape(id.unwrap_or_default()),
                        person.object()
                    ));
                }
                people.push_str("</li>");
            }
            people.push_str("</ul>");
        }
    }
    if !contributor && shared.is_some() {
        people.push_str(&format!(
            "<p class=\"lockline after\">{}<span>Only its Contributors change whom it is shared with.</span></p>",
            Icon::Lock.svg()
        ));
    }
    if contributor {
        people.push_str(&format!(
            "<form class=\"share card\" data-vm-form=\"share\" method=\"post\" action=\"{action}\" aria-label=\"Share {name}\">\
             <input type=\"hidden\" name=\"action\" value=\"share\">\
             <div class=\"field\"><label for=\"kind\">Share with</label>\
             <div class=\"control\"><select id=\"kind\" name=\"kind\">\
             <option value=\"email\">a person, by email address</option>\
             <option value=\"user\">a person, by user id</option>\
             <option value=\"group\">a Group</option>\
             <option value=\"everyone\">everyone who can sign in</option></select>{CHEVRON}</div></div>\
             <div class=\"field\"><label for=\"value\">Email address, user id or Group</label>\
             <p class=\"help\" id=\"value-help\">Empty for everyone.</p>\
             <div class=\"control\"><input id=\"value\" type=\"text\" name=\"value\" autocomplete=\"off\" spellcheck=\"false\" aria-describedby=\"value-help\"></div></div>\
             <div class=\"form-actions\"><button type=\"submit\" class=\"primary\">Share<span class=\"vh\"> {name}</span></button></div></form>"
        ));
    }
    people.push_str("</section>");
    whole(&mut main, block, &people);
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
            format!("There is no Application named <code>{name}</code>."),
        ),
        Refusal::NotAContributor => (
            StatusCode::FORBIDDEN,
            "not-a-contributor",
            "Only the Application's Contributors change whom it is shared with, and you are not one of them: ask one of them.".to_owned(),
        ),
        Refusal::Malformed(problem) => (
            StatusCode::BAD_REQUEST,
            "malformed",
            format!("{} Fill in the form again.", escape(problem)),
        ),
        Refusal::UnknownEmail(email) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown-email",
            format!(
                "Nobody who signed in here has the email address <code>{}</code>. Share with the person's user id instead (they see it when they sign in to the Dashboard), or ask them to sign in here once.",
                escape(email)
            ),
        ),
        Refusal::AmbiguousEmail(email) => (
            StatusCode::CONFLICT,
            "ambiguous-email",
            format!(
                "More than one person who signed in here has the email address <code>{}</code>. Share with the user id of the one you mean instead.",
                escape(email)
            ),
        ),
        Refusal::TooMany => (
            StatusCode::CONFLICT,
            "too-many",
            format!(
                "It is shared with {} entries already, the most it can be. Unshare some first, or share with a Group in place of many people.",
                thousands(MAX_SHARED_WITH)
            ),
        ),
        Refusal::LastContributor => (
            StatusCode::CONFLICT,
            "last-contributor",
            "That would leave the Application with no Contributor.".to_owned(),
        ),
        Refusal::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The platform could not make this change just now, and cannot tell yet whether it was made. Look at the Application's page again shortly before trying again.".to_owned(),
        ),
        Refusal::TimedOut => (
            StatusCode::REQUEST_TIMEOUT,
            "timed-out",
            "The form did not arrive in time: send it again.".to_owned(),
        ),
    };
    let main = format!(
        "{}<p class=\"said\" data-vm-error=\"{reason}\" role=\"alert\">{words}</p>\
         <div class=\"actions\"><a class=\"btn primary\" data-vm-back href=\"/applications/{name}\">{}<span class=\"vh\">Back to </span>{name}</a></div>",
        theme::state_title(Mark::Error, Icon::Cross, "Not changed"),
        Icon::Back.svg()
    );
    page(status, viewer, "Not changed", Layout::Card, &main)
}

/// The way back from a page that says one thing: the list.
fn back() -> String {
    format!(
        "<div class=\"actions\"><a class=\"btn primary\" href=\"/\">{}Your Applications</a></div>",
        Icon::Back.svg()
    )
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
        Layout::Card,
        &format!(
            "{}<p>To open <code>{name}</code> or see it here, ask one of its Contributors to share it with you.</p>{}",
            theme::state_title(Mark::Info, Icon::Lock, "Not shared with you"),
            back()
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
            "{}{}",
            theme::state_title(
                Mark::Info,
                Icon::Unknown,
                &format!("No Application named <code>{}</code>", escape(application))
            ),
            back()
        ),
    )
}

/// `404` of a path the Dashboard has not.
pub(crate) fn not_found(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::NOT_FOUND,
        viewer,
        "No such page",
        Layout::Card,
        &format!(
            "{}{}",
            theme::state_title(Mark::Info, Icon::Unknown, "No such page"),
            back()
        ),
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
            "{}{}",
            theme::state_title(Mark::Info, Icon::Unknown, "Method not allowed"),
            back()
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
        &format!(
            "{}<p>The pages of the list are numbered from 1 to 10000.</p>\
             <div class=\"actions\"><a class=\"btn primary\" href=\"/\">{}The first page</a></div>",
            theme::state_title(Mark::Info, Icon::Unknown, "No such page"),
            Icon::Back.svg()
        ),
    )
}

/// `503`: the platform database cannot be reached.
pub(crate) fn unavailable(viewer: &Viewer<'_>) -> Answer {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        "Try again shortly",
        Layout::Card,
        &format!(
            "{}<p>The Dashboard cannot be shown just now.</p>",
            theme::state_title(Mark::Warn, Icon::Later, "Try again shortly")
        ),
    )
}
