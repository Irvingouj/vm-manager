//! The pages of Platform administrators under `/admin` (blackbox/CONTRACT.md,
//! "The Dashboard for Platform administrators"): HTML of the platform's own
//! in the one theme, as [`super::pages`] writes the rest of the Dashboard,
//! with everything that is not the platform's own text escaped.
//!
//! Each page shows what the API answers from the same reading (crate::api's
//! `read_every_application`, `read_operation_log`, `known_resources`,
//! `installation_facts` and `oidc_connection`): it adds no fact of its own.
//!
//! **Size.** Bounded by the readings: 100 Applications, each with at most
//! 20 Contributors and 200 declared Resources; 100 entries of the Operation
//! log within 4 MiB as the API writes them (escaping makes a page at most
//! six times that, at worst); every Resource with its Grants, as `GET
//! /v1/resources` answers them all today.

use std::fmt::Write as _;

use hyper::StatusCode;
use serde_json::Value;
use url::form_urlencoded;

use super::pages::{self, Person, Viewer, disabled_mark, not_deployed, thousands, when};
use crate::api::LogQuery;
use crate::oidc::connection::Stored;
use crate::oidc::pages::escape;
use crate::registry::AdminApplication;
use crate::resources::{Resource, level_name};
use crate::theme::{self, CHEVRON, Icon, Layout, Mark};

type Answer = hyper::Response<http_body_util::Full<bytes::Bytes>>;

/// The four sections of the pages of Platform administrators.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Applications,
    OperationLog,
    Resources,
    Installation,
}

impl Section {
    const ALL: [Self; 4] = [
        Self::Applications,
        Self::OperationLog,
        Self::Resources,
        Self::Installation,
    ];

    fn parts(self) -> (&'static str, &'static str, &'static str, Icon) {
        match self {
            Self::Applications => (
                "applications",
                "/admin/applications",
                "Applications",
                Icon::Window,
            ),
            Self::OperationLog => (
                "operation-log",
                "/admin/operation-log",
                "Operation log",
                Icon::Log,
            ),
            Self::Resources => ("resources", "/admin/resources", "Resources", Icon::Database),
            Self::Installation => (
                "installation",
                "/admin/installation",
                "Installation",
                Icon::Server,
            ),
        }
    }
}

/// The sections, under the header: the one of the page itself current.
fn sections(current: Section) -> String {
    let mut out =
        String::from("<nav class=\"sections\" data-vm-admin-sections aria-label=\"Sections\"><ul>");
    for section in Section::ALL {
        let (value, path, words, icon) = section.parts();
        let current = if section == current {
            " aria-current=\"page\""
        } else {
            ""
        };
        write!(
            out,
            "<li><a data-vm-section=\"{value}\" href=\"{path}\"{current}>{}<span class=\"word\">{words}</span></a></li>",
            icon.svg()
        )
        .ok();
    }
    out.push_str("</ul></nav>");
    out
}

/// A page of one of the sections: the sections, then `main`.
fn section_page(
    status: StatusCode,
    viewer: &Viewer<'_>,
    section: Section,
    title: &str,
    main: &str,
) -> Answer {
    pages::page(
        status,
        viewer,
        title,
        Layout::Column,
        &format!("{}{main}", sections(section)),
    )
}

/// `403` to everyone who is not a Platform administrator, whatever the
/// path under `/admin`, its parameters and its body: nothing in it depends
/// on them.
pub(crate) fn not_an_administrator(viewer: &Viewer<'_>) -> Answer {
    pages::page(
        StatusCode::FORBIDDEN,
        viewer,
        "For Platform administrators",
        Layout::Card,
        &format!(
            "{}<p data-vm-error=\"not-an-administrator\">These pages are for this installation's Platform administrators: the members of a Group of your company's directory. If you should be one, ask your company's IT.</p>\
             <div class=\"actions\"><a class=\"btn primary\" data-vm-back href=\"/\">{}Your Applications</a></div>",
            theme::state_title(Mark::Info, Icon::Lock, "For Platform administrators"),
            Icon::Back.svg()
        ),
    )
}

/// `503` at once: two readings for Platform administrators run already.
pub(crate) fn busy(viewer: &Viewer<'_>, section: Section, title: &str) -> Answer {
    section_page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        section,
        title,
        &format!("<h1 class=\"page-title\">{title}</h1>{}", busy_notice()),
    )
}

/// The words of a reading refused while two run.
fn busy_notice() -> String {
    theme::notice(
        Mark::Warn,
        "data-vm-error=\"unavailable\" role=\"alert\"",
        "Other readings for Platform administrators are running just now: try again shortly.",
    )
}

/// `503` of an Application's page or the page that confirms a disable,
/// while two readings run.
pub(crate) fn busy_card(viewer: &Viewer<'_>) -> Answer {
    pages::page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        "Try again shortly",
        Layout::Card,
        &format!(
            "{}<p data-vm-error=\"unavailable\">Other readings for Platform administrators are running just now: try again shortly.</p>",
            theme::state_title(Mark::Warn, Icon::Later, "Try again shortly")
        ),
    )
}

/// `404` of an Application named on a page of Platform administrators
/// (also a name none could have): back to every Application.
pub(crate) fn no_application(viewer: &Viewer<'_>, name: &str) -> Answer {
    pages::page(
        StatusCode::NOT_FOUND,
        viewer,
        "No such Application",
        Layout::Card,
        &format!(
            "{}<p data-vm-error=\"not-found\">There is no Application named <code>{}</code>.</p>\
             <div class=\"actions\"><a class=\"btn primary\" data-vm-back href=\"/admin/applications\">{}Every Application</a></div>",
            theme::state_title(Mark::Info, Icon::Unknown, "No such Application"),
            escape(name),
            Icon::Back.svg()
        ),
    )
}

/// Usage as a line: requests and Users, with the period said.
fn usage_line(requests: i64, users: i64) -> String {
    format!(
        "<p class=\"usage-line\">{}<span class=\"num\">{}</span> {} by {}<span class=\"num\">{}</span> {} in the last 30 days</p>",
        Icon::Activity.svg(),
        thousands(requests),
        if requests == 1 { "request" } else { "requests" },
        Icon::Person.svg(),
        thousands(users),
        if users == 1 { "User" } else { "Users" },
    )
}

/// `GET /admin/applications/<name>/disable`: the page that names the
/// Application and asks; of one disabled already, since when and by whom.
pub(crate) fn confirm_disable(viewer: &Viewer<'_>, application: &AdminApplication) -> Answer {
    let name = escape(&application.name);
    let main = match &application.disabled {
        Some(disabled) => format!(
            "<div class=\"confirm\" data-vm-confirm=\"{name}\">{}\
             <p data-vm-disabled=\"{}\" data-vm-disabled-by=\"{}\">Disabled on <time datetime=\"{}\">{}</time> by <code class=\"id\">{}</code>. It is enabled again on its page.</p>\
             <div class=\"actions\"><a class=\"btn primary\" data-vm-back href=\"/applications/{name}\">{}Its page</a></div></div>",
            theme::state_title(
                Mark::Info,
                Icon::Lock,
                &format!("{name} is disabled already")
            ),
            escape(&disabled.since),
            escape(&disabled.by),
            escape(&disabled.since),
            escape(&when(&disabled.since)),
            escape(&disabled.by),
            Icon::Back.svg()
        ),
        None => format!(
            "<div class=\"confirm\" data-vm-confirm=\"{name}\">{}\
             <p class=\"lead\">From the moment you confirm, its address answers every request that it is disabled, those in flight included. Nothing of it is deleted, and its Contributors can still deploy. \u{201c}Enable {name}\u{201d} on its page serves it again.</p>\
             {}\
             <div class=\"actions\"><form data-vm-form=\"disable\" method=\"post\" action=\"/admin/applications/{name}/disable\"><button type=\"submit\" class=\"danger-fill\">{}Disable {name}</button></form>\
             <a class=\"btn quiet\" data-vm-back href=\"/applications/{name}\">Keep it serving</a></div></div>",
            theme::state_title(Mark::Warn, Icon::Stop, &format!("Disable {name}?")),
            usage_line(application.requests, application.users),
            Icon::Stop.svg()
        ),
    };
    pages::page(
        StatusCode::OK,
        viewer,
        &format!("Disable {name}"),
        Layout::Card,
        &main,
    )
}

/// `GET /admin/applications`: one page of every Application, as `GET
/// /v1/admin/applications` answers it. `declared`: each one's Declared
/// access cut, and how many more it declares.
pub(crate) fn every_application(
    viewer: &Viewer<'_>,
    rows: &[AdminApplication],
    declared: &[(Option<Value>, usize)],
    after: Option<&str>,
    next: Option<&str>,
) -> Answer {
    let mut main = String::from("<h1 class=\"page-title\">Every Application</h1>");
    if let Some(first) = rows.first() {
        write!(
            main,
            "<p class=\"legend\"><span>{}Requests</span><span>{}Users</span><span>in the last 30 days, counted from {}</span></p><ul class=\"apps\">",
            Icon::Activity.svg(),
            Icon::Person.svg(),
            escape(&first.usage_from)
        ).ok();
        let records = viewer.records_people();
        for (application, (declared, more)) in rows.iter().zip(declared) {
            main.push_str("<li>");
            admin_card(&mut main, application, declared.as_ref(), *more, records);
            main.push_str("</li>");
        }
        main.push_str("</ul>");
    } else if after.is_none() {
        main.push_str(
            "<div class=\"empty\"><p class=\"first\">The installation has no Application yet.</p>\
             <p>A Builder makes one with the command-line tool <code>vibe</code>.</p></div>",
        );
    } else {
        main.push_str(
            "<div class=\"empty\"><p class=\"first\">There is no Application after this place in the list.</p></div>",
        );
    }
    if after.is_some() || next.is_some() {
        main.push_str("<nav class=\"pages\" aria-label=\"Pages\">");
        if after.is_some() {
            write!(
                main,
                "<a class=\"btn\" data-vm-first href=\"/admin/applications\">{}The first page</a>",
                Icon::Back.svg()
            )
            .ok();
        }
        if let Some(next) = next {
            write!(
                main,
                "<a class=\"btn next\" data-vm-next href=\"/admin/applications?after={}\">Next page{}</a>",
                escape(&form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>()),
                Icon::Forward.svg()
            ).ok();
        }
        main.push_str("</nav>");
    }
    section_page(
        StatusCode::OK,
        viewer,
        Section::Applications,
        "Every Application",
        &main,
    )
}

/// `400`: an `after` the API would refuse.
pub(crate) fn not_a_place(viewer: &Viewer<'_>) -> Answer {
    section_page(
        StatusCode::BAD_REQUEST,
        viewer,
        Section::Applications,
        "Every Application",
        &format!(
            "<h1 class=\"page-title\">Every Application</h1>{}\
             <nav class=\"pages\" aria-label=\"Pages\"><a class=\"btn\" data-vm-first href=\"/admin/applications\">{}The first page</a></nav>",
            theme::notice(
                Mark::Error,
                "data-vm-error=\"malformed\" role=\"alert\"",
                "This address does not name a place in the list: it names no Application that could be."
            ),
            Icon::Back.svg()
        ),
    )
}

/// One Application of the list of every Application.
fn admin_card(
    out: &mut String,
    application: &AdminApplication,
    declared: Option<&Value>,
    more: usize,
    records: bool,
) {
    let name = escape(&application.name);
    let serving = application
        .serving_version
        .map_or_else(|| "none".to_owned(), |n| n.to_string());
    write!(
        out,
        "<article class=\"app card\" data-vm-application=\"{name}\" data-vm-serving=\"{serving}\" data-vm-requests=\"{}\" data-vm-users=\"{}\"",
        application.requests, application.users
    ).ok();
    if let Some(disabled) = &application.disabled {
        write!(out, " data-vm-disabled=\"{}\"", escape(&disabled.since)).ok();
    }
    write!(
        out,
        "><div class=\"text\"><div class=\"app-head\"><h2><a class=\"name\" data-vm-details href=\"/applications/{name}\">{name}</a></h2>"
    ).ok();
    if application.disabled.is_some() {
        out.push_str(&disabled_mark());
    } else if application.serving_version.is_none() {
        out.push_str(&not_deployed());
    }
    out.push_str("</div>");
    if let Some(description) = &application.description {
        write!(
            out,
            "<p class=\"desc\" data-vm-description>{}</p>",
            escape(description)
        )
        .ok();
    }
    out.push_str("</div>");
    write!(
        out,
        "<p class=\"usage\"><span>{}<span class=\"num\">{}</span><span class=\"vh\"> requests,</span></span><span>{}<span class=\"num\">{}</span><span class=\"vh\"> Users</span></span></p>",
        Icon::Activity.svg(),
        thousands(application.requests),
        Icon::Person.svg(),
        thousands(application.users)
    ).ok();
    write!(
        out,
        "<details class=\"more\"><summary>Details<span class=\"vh\"> of {name}</span></summary><dl class=\"facts\">"
    ).ok();
    match (application.serving_version, application.latest_version) {
        (Some(serving), Some(latest)) if latest != serving => {
            write!(
                out,
                "<dt>Serving</dt><dd>Version {serving}</dd><dt>Latest</dt><dd>Version {latest}</dd>"
            )
            .ok();
        }
        (Some(serving), _) => {
            write!(out, "<dt>Serving</dt><dd>Version {serving}</dd>").ok();
        }
        (None, Some(latest)) => {
            write!(
                out,
                "<dt>Serving</dt><dd>No Version</dd><dt>Latest</dt><dd>Version {latest}</dd>"
            )
            .ok();
        }
        (None, None) => out.push_str("<dt>Serving</dt><dd>No Version yet</dd>"),
    }
    let shared = if application.everyone {
        "everyone who can sign in".to_owned()
    } else if application.groups == 0 && application.people == 0 {
        "nobody: only its Contributors open it".to_owned()
    } else {
        format!(
            "{} {} and {} {}",
            thousands(application.groups),
            if application.groups == 1 {
                "Group"
            } else {
                "Groups"
            },
            thousands(application.people),
            if application.people == 1 {
                "person"
            } else {
                "people"
            }
        )
    };
    write!(
        out,
        "<dt>Shared with</dt><dd data-vm-shared-count data-vm-everyone=\"{}\" data-vm-groups=\"{}\" data-vm-people=\"{}\">{shared}</dd>",
        application.everyone, application.groups, application.people
    ).ok();
    if let Some(disabled) = &application.disabled {
        let by = escape(&disabled.by);
        write!(
            out,
            "<dt>Disabled</dt><dd data-vm-disabled-by=\"{by}\">on <time datetime=\"{}\">{}</time> by <code class=\"id\">{by}</code></dd>",
            escape(&disabled.since),
            escape(&when(&disabled.since))
        ).ok();
    }
    write!(
        out,
        "<dt>Made</dt><dd>by <code class=\"id\">{}</code> on <time datetime=\"{}\">{}</time></dd></dl>",
        escape(&application.created_by),
        escape(&application.created_at),
        escape(&when(&application.created_at))
    ).ok();
    out.push_str("<h3 class=\"over\">Declared access</h3>");
    let resources = declared
        .and_then(|d| d.get("resources"))
        .and_then(Value::as_object);
    match (application.serving_version, resources) {
        (None, _) => {
            out.push_str("<p class=\"soft\">No Version serves, so it declares nothing.</p>")
        }
        (Some(_), Some(resources)) if !resources.is_empty() => {
            out.push_str("<ul class=\"tags\">");
            for (resource, said) in resources {
                let access = said.get("access").and_then(Value::as_str).unwrap_or("");
                let resource = escape(resource);
                let access = escape(access);
                write!(
                    out,
                    "<li data-vm-declared-resource=\"{resource}\" data-vm-declared=\"{access}\">{}<code>{resource}</code> <span class=\"lv-pill\">{access}</span></li>",
                    Icon::Database.svg()
                ).ok();
            }
            out.push_str("</ul>");
            if more > 0 {
                write!(
                    out,
                    "<p class=\"soft\" data-vm-declared-more=\"{more}\">and {more} more</p>"
                )
                .ok();
            }
        }
        (Some(_), _) => out.push_str("<p class=\"soft\">It declares no Resource.</p>"),
    }
    out.push_str("<h3 class=\"over\">Contributors</h3><ul class=\"rows\">");
    for entry in &application.contributors {
        let person = Person::of(
            &entry.named,
            entry.name.as_deref(),
            entry.email.as_deref(),
            records,
        );
        let (kind, id) = person.kind();
        write!(
            out,
            "<li data-vm-contributor=\"{kind}\" data-vm-id=\"{}\"{}><span class=\"who-is\">{}</span></li>",
            escape(id.unwrap_or_default()),
            person.marks(),
            person.row()
        ).ok();
    }
    out.push_str("</ul></details></article>");
}

/// The Operation log's filters as the page holds them in its form: what
/// was typed (white space dropped), a time as a `datetime-local` field
/// holds it in UTC (empty when it cannot be read).
#[derive(Default)]
pub(crate) struct Typed {
    pub user: String,
    pub application: String,
    pub resource: String,
    pub outcome: String,
    pub since: String,
    pub until: String,
    /// A time typed that cannot be read: quoted in its error's words.
    pub since_typed: String,
    pub until_typed: String,
}

impl Typed {
    /// Whether a filter is given.
    fn any(&self) -> bool {
        [
            &self.user,
            &self.application,
            &self.resource,
            &self.outcome,
            &self.since,
            &self.until,
        ]
        .iter()
        .any(|v| !v.is_empty())
            || !self.since_typed.is_empty()
            || !self.until_typed.is_empty()
    }
}

/// What a page of the Operation log shows besides its form.
pub(crate) enum LogShown<'a> {
    /// The entries (oldest first, as the API answers them) and `earlier`.
    Entries {
        asked: &'a LogQuery,
        entries: &'a [Value],
        earlier: Option<i64>,
    },
    /// A filter refused: (field, words, HTML).
    Malformed { field: &'static str, words: String },
    /// The reading passed its 10 seconds.
    TooLong,
    /// Two readings run already.
    Busy,
}

/// An address of the Operation log with these filters (the empty ones
/// left out), and `before`.
fn log_address(filters: &[(&str, &str)], before: Option<i64>) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (name, value) in filters {
        if !value.is_empty() {
            query.append_pair(name, value);
        }
    }
    if let Some(before) = before {
        query.append_pair("before", &before.to_string());
    }
    let query = query.finish();
    if query.is_empty() {
        "/admin/operation-log".to_owned()
    } else {
        format!("/admin/operation-log?{query}")
    }
}

/// The filters of a reading, as an address carries them.
fn asked_filters(asked: &LogQuery) -> Vec<(&'static str, &str)> {
    [
        ("user", asked.user.as_deref()),
        ("application", asked.application.as_deref()),
        ("resource", asked.resource.as_deref()),
        ("outcome", asked.outcome.as_deref()),
        ("since", asked.since.as_deref()),
        ("until", asked.until.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|v| (name, v)))
    .collect()
}

/// The filters typed, as an address carries them.
fn typed_filters(typed: &Typed) -> Vec<(&'static str, &str)> {
    vec![
        ("user", typed.user.as_str()),
        ("application", typed.application.as_str()),
        ("resource", typed.resource.as_str()),
        ("outcome", typed.outcome.as_str()),
        ("since", typed.since.as_str()),
        ("until", typed.until.as_str()),
    ]
}

/// `GET /admin/operation-log`.
pub(crate) fn operation_log(viewer: &Viewer<'_>, typed: &Typed, shown: &LogShown<'_>) -> Answer {
    let mut main = String::from("<h1 class=\"page-title\">Operation log</h1>");
    let field_error = match shown {
        LogShown::Malformed { field, words } => Some((*field, words.as_str())),
        _ => None,
    };
    filter_form(&mut main, typed, field_error);
    main.push_str("<section class=\"log\" aria-label=\"Entries\">");
    let status = match shown {
        LogShown::Entries {
            asked,
            entries,
            earlier,
        } => {
            entries_html(&mut main, asked, entries, *earlier);
            StatusCode::OK
        }
        LogShown::Malformed { field, words } => {
            if *field == "before" {
                write!(
                    main,
                    "<p class=\"notice error\" data-vm-field-error=\"before\" role=\"alert\">{}<span>{words} <a data-vm-newest href=\"{}\">The newest entries</a></span></p>",
                    Mark::Error.parts().1,
                    escape(&log_address(&typed_filters(typed), None))
                ).ok();
            }
            StatusCode::BAD_REQUEST
        }
        LogShown::TooLong => {
            main.push_str(&theme::notice(
                Mark::Warn,
                "data-vm-error=\"unavailable\" role=\"alert\"",
                "The reading took too long (more than 10 seconds) and was ended: narrow it with a time range or another filter, and show the entries again.",
            ));
            StatusCode::SERVICE_UNAVAILABLE
        }
        LogShown::Busy => {
            main.push_str(&busy_notice());
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    main.push_str("</section>");
    let mut answer = section_page(
        status,
        viewer,
        Section::OperationLog,
        "Operation log",
        &main,
    );
    if matches!(shown, LogShown::TooLong) {
        // Trying again at once would take as long: no `retry-after`.
        answer.headers_mut().remove(hyper::header::RETRY_AFTER);
    }
    answer
}

/// The filter form, each field holding what was typed; `error`, beside its
/// field.
fn filter_form(out: &mut String, typed: &Typed, error: Option<(&str, &str)>) {
    out.push_str("<form class=\"card filters\" data-vm-form=\"operation-log\" method=\"get\" action=\"/admin/operation-log\"><div class=\"filter-grid\">");
    let error_of = |field: &str| -> String {
        match error {
            Some((f, words)) if f == field => format!(
                "<p class=\"field-error\" id=\"{field}-error\" data-vm-field-error=\"{field}\">{}<span>{words}</span></p>",
                Icon::Warn.svg()
            ),
            _ => String::new(),
        }
    };
    let invalid = |field: &str| -> String {
        match error {
            Some((f, _)) if f == field => {
                format!(" aria-invalid=\"true\" aria-describedby=\"{field}-error\"")
            }
            _ => String::new(),
        }
    };
    for (field, label, value) in [
        ("user", "User id", &typed.user),
        ("application", "Application", &typed.application),
        ("resource", "Resource", &typed.resource),
    ] {
        write!(
            out,
            "<div class=\"field\"><label for=\"f-{field}\">{label}</label><div class=\"control\"><input id=\"f-{field}\" type=\"text\" name=\"{field}\" value=\"{}\" autocomplete=\"off\" spellcheck=\"false\"{}></div>{}</div>",
            escape(value),
            invalid(field),
            error_of(field)
        ).ok();
    }
    write!(
        out,
        "<div class=\"field\"><label for=\"f-outcome\">Outcome</label><div class=\"control\"><select id=\"f-outcome\" name=\"outcome\"{}>",
        invalid("outcome")
    ).ok();
    for (value, words) in [
        ("", "Any"),
        ("allowed", "Allowed"),
        ("refused", "Refused"),
        ("failed", "Failed"),
        ("unfinished", "Unfinished"),
    ] {
        let selected = if typed.outcome == value {
            " selected"
        } else {
            ""
        };
        write!(out, "<option value=\"{value}\"{selected}>{words}</option>").ok();
    }
    write!(out, "</select>{CHEVRON}</div>{}</div>", error_of("outcome")).ok();
    for (field, label, value) in [
        ("since", "From (UTC)", &typed.since),
        ("until", "Before (UTC)", &typed.until),
    ] {
        write!(
            out,
            "<div class=\"field\"><label for=\"f-{field}\">{label}</label><div class=\"control\"><input id=\"f-{field}\" type=\"datetime-local\" step=\"1\" name=\"{field}\" value=\"{}\"{}></div>{}</div>",
            escape(value),
            invalid(field),
            error_of(field)
        ).ok();
    }
    out.push_str("</div><div class=\"form-actions\"><button type=\"submit\" class=\"primary\">Show entries</button>");
    if typed.any() {
        out.push_str(
            "<a class=\"btn quiet\" data-vm-clear href=\"/admin/operation-log\">Clear the filters</a>",
        );
    }
    out.push_str("</div></form>");
}

/// The mark of an outcome: its drawing, its colour and its word.
fn outcome_mark(outcome: &str) -> String {
    let (icon, word) = match outcome {
        "allowed" => (Icon::Check, "Allowed"),
        "refused" => (Icon::Cross, "Refused"),
        "failed" => (Icon::Warn, "Failed"),
        _ => (Icon::Pending, "Unfinished"),
    };
    let class = match outcome {
        "allowed" | "refused" | "failed" => outcome,
        _ => "unfinished",
    };
    theme::named(icon, word, &format!("mark {class}"))
}

/// The entries, newest first, with the links to older and newest ones.
fn entries_html(out: &mut String, asked: &LogQuery, entries: &[Value], earlier: Option<i64>) {
    let filters = asked_filters(asked);
    if entries.is_empty() {
        let words = if filters.is_empty() && asked.before.is_none() {
            "The Operation log has no entry yet. Each operation of an Application on a Resource or on its own database writes one.".to_owned()
        } else if filters.is_empty() {
            "No entry is older than these.".to_owned()
        } else {
            let named: Vec<String> = filters
                .iter()
                .map(|(name, value)| {
                    let label = match *name {
                        "user" => "User id",
                        "application" => "Application",
                        "resource" => "Resource",
                        "outcome" => "Outcome",
                        "since" => "From",
                        _ => "Before",
                    };
                    format!("{label} <code>{}</code>", escape(value))
                })
                .collect();
            format!("No entry matches {}.", named.join(", "))
        };
        write!(
            out,
            "<div class=\"empty\" data-vm-empty><p class=\"first\">{words}</p></div>"
        )
        .ok();
    } else {
        out.push_str("<p class=\"over\">Newest first, times in UTC</p><ol class=\"entries\">");
        for entry in entries.iter().rev() {
            entry_html(out, &filters, entry);
        }
        out.push_str("</ol>");
    }
    if asked.before.is_some() || earlier.is_some() {
        out.push_str("<nav class=\"pages\" aria-label=\"Pages\">");
        if asked.before.is_some() {
            write!(
                out,
                "<a class=\"btn\" data-vm-newest href=\"{}\">{}The newest entries</a>",
                escape(&log_address(&filters, None)),
                Icon::Back.svg()
            )
            .ok();
        }
        if let Some(earlier) = earlier {
            write!(
                out,
                "<a class=\"btn next\" data-vm-older href=\"{}\">Older entries{}</a>",
                escape(&log_address(&filters, Some(earlier))),
                Icon::Forward.svg()
            )
            .ok();
        }
        out.push_str("</nav>");
    }
}

/// A link narrowing to `kind` = `value`: the other filters kept,
/// `before` dropped.
fn filter_link(filters: &[(&str, &str)], kind: &str, value: &str) -> String {
    let mut narrowed: Vec<(&str, &str)> = filters
        .iter()
        .filter(|(name, _)| *name != kind)
        .copied()
        .collect();
    narrowed.push((kind, value));
    format!(
        "<a data-vm-filter=\"{kind}\" href=\"{}\">{}</a>",
        escape(&log_address(&narrowed, None)),
        escape(value)
    )
}

/// One entry: what is seen first, and the rest one step away.
fn entry_html(out: &mut String, filters: &[(&str, &str)], entry: &Value) {
    let text = |name: &str| entry.get(name).and_then(Value::as_str);
    let id = entry.get("id").and_then(Value::as_i64).unwrap_or_default();
    let outcome = text("outcome").unwrap_or_default();
    let time = text("time").unwrap_or_default();
    // To the second: `YYYY-MM-DD HH:MM:SS`.
    let shown_time = match (time.get(..10), time.get(11..19)) {
        (Some(day), Some(second)) => format!("{day} {second}"),
        _ => time.to_owned(),
    };
    write!(
        out,
        "<li class=\"entry\" data-vm-entry=\"{id}\" data-vm-outcome=\"{}\" data-vm-time=\"{}\"><div class=\"entry-line\">{}<time datetime=\"{}\">{}</time>",
        escape(outcome),
        escape(time),
        outcome_mark(outcome),
        escape(time),
        escape(&shown_time)
    ).ok();
    match text("user") {
        Some(user) => {
            write!(
                out,
                "<span>{}{}</span>",
                theme::icon(Icon::Person, "User"),
                filter_link(filters, "user", user)
            )
            .ok();
        }
        None => {
            write!(
                out,
                "<span>{}<span class=\"soft\">\u{2014}</span></span>",
                theme::icon(Icon::Person, "No User")
            )
            .ok();
        }
    }
    match text("application") {
        Some(application) => {
            let version = entry
                .get("version")
                .and_then(Value::as_i64)
                .map_or_else(String::new, |v| {
                    format!(" <span class=\"soft\">v{v}</span>")
                });
            write!(
                out,
                "<span>{}{}{version}</span>",
                theme::icon(Icon::Window, "Application"),
                filter_link(filters, "application", application)
            )
            .ok();
        }
        None => {
            write!(
                out,
                "<span>{}<span class=\"soft\">\u{2014}</span></span>",
                theme::icon(Icon::Window, "No Application")
            )
            .ok();
        }
    }
    match text("resource") {
        Some(resource) => {
            write!(
                out,
                "<span>{}{}</span>",
                theme::icon(Icon::Database, "Resource"),
                filter_link(filters, "resource", resource)
            )
            .ok();
        }
        None => {
            write!(out, "<span>{}own database</span>", Icon::Database.svg()).ok();
        }
    }
    write!(
        out,
        "<span><code>{}</code></span>",
        escape(text("operation").unwrap_or_default())
    )
    .ok();
    match (outcome, text("reason"), text("error")) {
        ("refused", Some(reason), _) => {
            write!(
                out,
                "<span class=\"reason\"><code data-vm-reason>{}</code></span>",
                escape(reason)
            )
            .ok();
        }
        ("failed", _, Some(error)) => {
            write!(
                out,
                "<span class=\"reason failed\"><code data-vm-error-code>{}</code></span>",
                escape(error)
            )
            .ok();
        }
        _ => {}
    }
    write!(
        out,
        "</div><details><summary>Details<span class=\"vh\"> of entry {id}</span></summary>"
    )
    .ok();
    if let Some(statement) = text("statement") {
        write!(out, "<pre class=\"statement\">{}</pre>", escape(statement)).ok();
    }
    out.push_str("<dl class=\"facts\">");
    let number = |name: &str| {
        entry
            .get(name)
            .and_then(Value::as_i64)
            .map_or_else(|| "\u{2014}".to_owned(), |n| n.to_string())
    };
    write!(
        out,
        "<dt>Rows</dt><dd>{}</dd><dt>Duration</dt><dd>{}</dd>",
        number("rows"),
        entry
            .get("duration_ms")
            .and_then(Value::as_i64)
            .map_or_else(|| "\u{2014}".to_owned(), |n| format!("{n} ms"))
    )
    .ok();
    for (label, name) in [
        ("Request", "request"),
        ("Transaction", "transaction"),
        ("As", "actor"),
    ] {
        let value = text(name).map_or_else(
            || "\u{2014}".to_owned(),
            |v| format!("<code>{}</code>", escape(v)),
        );
        write!(out, "<dt>{label}</dt><dd>{value}</dd>").ok();
    }
    write!(
        out,
        "<dt>Entry</dt><dd><code>{id}</code></dd></dl></details></li>"
    )
    .ok();
}

/// `GET /admin/installation`: what `GET /v1/installation` answers, and
/// with OIDC the connection as `GET /v1/oidc` answers it (never its
/// secret).
pub(crate) fn installation(
    viewer: &Viewer<'_>,
    domain: &str,
    sign_in: &str,
    address: Option<&str>,
    connection: Option<&Stored>,
) -> Answer {
    let mut main =
        String::from("<h1 class=\"page-title\">Installation</h1><dl class=\"facts big\">");
    write!(
        main,
        "<dt>Domain</dt><dd data-vm-domain><code>{}</code></dd>",
        escape(domain)
    )
    .ok();
    match sign_in {
        "oidc" => {
            write!(
                main,
                "<dt>Sign-in</dt><dd data-vm-sign-in=\"oidc\">OIDC, at <code>{}</code></dd>",
                escape(address.unwrap_or_default())
            ).ok();
        }
        "stand-in" => main.push_str(
            "<dt>Sign-in</dt><dd data-vm-sign-in=\"stand-in\">The stand-in, for tests and the local run: OIDC is not connected.</dd>",
        ),
        _ => main.push_str(
            "<dt>Sign-in</dt><dd data-vm-sign-in=\"none\">Nobody can sign in: OIDC is not connected.</dd>",
        ),
    }
    if let Some(connection) = connection {
        write!(
            main,
            "<dt>Issuer</dt><dd data-vm-issuer><code>{}</code></dd><dt>Administrators' Group</dt><dd data-vm-administrators-group>{}<code>{}</code></dd>",
            escape(&connection.issuer),
            Icon::Group.svg(),
            escape(&connection.administrators_group)
        ).ok();
    }
    main.push_str("</dl>");
    if let Some(connection) = connection {
        let by = if connection.connected_by == "setup-credential" {
            "the setup credential".to_owned()
        } else {
            format!(
                "<code class=\"id\">{}</code>",
                escape(&connection.connected_by)
            )
        };
        let scopes: Vec<String> = connection
            .scopes
            .iter()
            .map(|s| format!("<code>{}</code>", escape(s)))
            .collect();
        write!(
            main,
            "<details class=\"more\"><summary>The connection's details</summary><dl class=\"facts\">\
             <dt>Client id</dt><dd><code>{}</code></dd><dt>Groups claim</dt><dd><code>{}</code></dd>\
             <dt>Scopes</dt><dd>{}</dd><dt>Connected</dt><dd>on <time datetime=\"{}\">{}</time> by {by}</dd></dl></details>",
            escape(&connection.client_id),
            escape(&connection.groups_claim),
            scopes.join(" "),
            escape(&connection.connected_at),
            escape(&when(&connection.connected_at))
        ).ok();
    }
    write!(
        main,
        "<p class=\"lockline\">{}<span>The connection is changed with the command-line tool: <code class=\"cmd\">vibe admin connect-oidc</code>.</span></p>",
        Icon::Terminal.svg()
    ).ok();
    section_page(
        StatusCode::OK,
        viewer,
        Section::Installation,
        "Installation",
        &main,
    )
}

/// `GET /admin/resources`: every Resource with its Grants, read only.
pub(crate) fn resources(viewer: &Viewer<'_>, all: &[std::sync::Arc<Resource>]) -> Answer {
    let mut main = String::from("<h1 class=\"page-title\">Resources and Grants</h1>");
    if all.is_empty() {
        main.push_str(
            "<div class=\"empty\"><p class=\"first\">No Resource is registered yet.</p>\
             <p>A Platform administrator registers one on the platform's API.</p></div>",
        );
    } else {
        main.push_str(
            "<p class=\"lead\">Read only: Grants are given and Resources registered on the platform's API for now.</p><ul class=\"apps\">",
        );
        for resource in all {
            resource_card(&mut main, resource);
        }
        main.push_str("</ul>");
    }
    section_page(
        StatusCode::OK,
        viewer,
        Section::Resources,
        "Resources and Grants",
        &main,
    )
}

/// One Resource: its name, description and Grants, and one step away
/// whose it is and its Operation log.
fn resource_card(out: &mut String, resource: &Resource) {
    let name = escape(&resource.name);
    write!(
        out,
        "<li><article class=\"card res-card\" data-vm-resource=\"{name}\" data-vm-kind=\"postgres\"><h2>{}<code>{name}</code></h2>",
        Icon::Database.svg()
    ).ok();
    if !resource.description.is_empty() {
        write!(
            out,
            "<p class=\"desc\">{}</p>",
            escape(&resource.description)
        )
        .ok();
    }
    let grants = resource.grants().unwrap_or_default();
    if grants.is_empty() {
        out.push_str(
            "<p class=\"soft\">No Group holds a Grant on it, so nobody's request reaches it.</p>",
        );
    } else {
        out.push_str("<ul class=\"grants\" aria-label=\"Grants\">");
        for (group, level) in &grants {
            let group = escape(group);
            let level = level_name(*level);
            write!(
                out,
                "<li data-vm-grant=\"{group}\" data-vm-access=\"{level}\">{}<span class=\"g\">{group}</span> <span class=\"lv-pill\">{level}</span></li>",
                theme::icon(Icon::Group, "The Group")
            ).ok();
        }
        out.push_str("</ul>");
    }
    write!(
        out,
        "<details class=\"more\"><summary>Details<span class=\"vh\"> of {name}</span></summary><dl class=\"facts\">\
         <dt>Owner</dt><dd data-vm-owner>{}</dd><dt>Ask</dt><dd data-vm-contact>{}</dd><dt>Kind</dt><dd>Postgres</dd>\
         <dt>Registered</dt><dd>by <code class=\"id\">{}</code> on <time datetime=\"{}\">{}</time></dd></dl>\
         <a data-vm-operation-log href=\"/admin/operation-log?resource={}\">{}Its Operation log</a></details></article></li>",
        escape(&resource.owner),
        escape(&resource.contact),
        escape(&resource.created_by),
        escape(&resource.created_at),
        escape(&when(&resource.created_at)),
        escape(&form_urlencoded::byte_serialize(resource.name.as_bytes()).collect::<String>()),
        Icon::Log.svg()
    ).ok();
}

/// `503` while the Grants on a Resource cannot be told: no Grant on a
/// guess.
pub(crate) fn grants_unknown(viewer: &Viewer<'_>, resource: &str) -> Answer {
    section_page(
        StatusCode::SERVICE_UNAVAILABLE,
        viewer,
        Section::Resources,
        "Resources and Grants",
        &format!(
            "<h1 class=\"page-title\">Resources and Grants</h1>{}",
            theme::notice(
                Mark::Warn,
                "data-vm-error=\"unavailable\" role=\"alert\"",
                &format!(
                    "The platform is finding out the Grants on the Resource <code>{}</code>: try again shortly.",
                    escape(resource)
                )
            )
        ),
    )
}
