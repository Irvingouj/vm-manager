//! The pages under `/admin`, for Platform administrators
//! (blackbox/CONTRACT.md, "The Dashboard for Platform administrators"). The
//! person is a Platform administrator here: everyone else is refused before
//! [`Ingress::admin`] is called, whatever the path, the method and the body.
//!
//! Each page is one reading of the API's own code (crate::api), so it shows
//! what the API answers from the same reading, within the same bounds:
//! every Application and the Operation log count against the readings at
//! once of Platform administrators, the API's and these pages' together.
//! A disable and an enable are the API's own change (crate::api's
//! `switch`).

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::http::request::Parts;
use hyper::{Method, Response};
use url::form_urlencoded;

use super::admin_pages::{self, LogShown, Section, Typed};
use super::pages::{self, Refusal, Viewer};
use super::{Ingress, MAX_FORM_BYTES};
use crate::api::{
    AdminRead, LOG_FILTERS, LogProblem, LogQuery, MAX_ADMIN_PAGE, Which, query_parameters,
    rfc3339_micros,
};
use crate::ingress::BODY_READ_TIMEOUT;
use crate::registry::{NameProblem, check_name};

type Answer = Response<Full<Bytes>>;

/// The paths under `/admin`.
#[derive(Clone, Copy)]
enum Route<'a> {
    Root,
    Applications,
    Disable(&'a str),
    Enable(&'a str),
    OperationLog,
    Resources,
    Outbound,
    Installation,
}

impl<'a> Route<'a> {
    /// The route of `path` (which is `/admin` or begins with `/admin/`);
    /// `None` for any other.
    fn of(path: &'a str) -> Option<Self> {
        match path {
            "/admin" => return Some(Self::Root),
            "/admin/applications" => return Some(Self::Applications),
            "/admin/operation-log" => return Some(Self::OperationLog),
            "/admin/resources" => return Some(Self::Resources),
            "/admin/outbound" => return Some(Self::Outbound),
            "/admin/installation" => return Some(Self::Installation),
            _ => {}
        }
        let rest = path.strip_prefix("/admin/applications/")?;
        match rest.split_once('/') {
            Some((name, "disable")) if !name.is_empty() => Some(Self::Disable(name)),
            Some((name, "enable")) if !name.is_empty() => Some(Self::Enable(name)),
            _ => None,
        }
    }
}

/// Whether `path` is one of the pages of Platform administrators: `/admin`
/// or under `/admin/`.
pub(super) fn is_admin_path(path: &str) -> bool {
    path == "/admin" || path.starts_with("/admin/")
}

impl Ingress {
    /// A request under `/admin` by a Platform administrator.
    pub(super) async fn admin<B>(&self, viewer: &Viewer<'_>, parts: &Parts, body: B) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let method = &parts.method;
        let read = method == Method::GET || method == Method::HEAD;
        let query = parts.uri.query();
        match Route::of(parts.uri.path()) {
            None => pages::not_found(viewer),
            Some(Route::Disable(name)) if read => self.confirm_disable(viewer, name).await,
            Some(Route::Disable(name)) if method == Method::POST => {
                self.switch(viewer, name, true, body).await
            }
            Some(Route::Disable(_)) => pages::method_not_allowed(viewer, "GET, HEAD, POST"),
            Some(Route::Enable(name)) if method == Method::POST => {
                self.switch(viewer, name, false, body).await
            }
            Some(Route::Enable(_)) => pages::method_not_allowed(viewer, "POST"),
            Some(_) if !read => pages::method_not_allowed(viewer, "GET, HEAD"),
            Some(Route::Root) => pages::see_other("/admin/applications"),
            Some(Route::Applications) => self.every_application_page(viewer, query).await,
            Some(Route::OperationLog) => self.operation_log_page(viewer, query).await,
            Some(Route::Resources) => self.resources_page(viewer).await,
            Some(Route::Outbound) => self.outbound_page(viewer).await,
            Some(Route::Installation) => self.installation_page(viewer).await,
        }
    }

    /// `GET /admin/applications`: one page of `GET /v1/admin/applications`,
    /// from the same reading. Only `after` is read; other parameters are
    /// ignored.
    async fn every_application_page(&self, viewer: &Viewer<'_>, query: Option<&str>) -> Answer {
        let after = match query_parameters(query, &["after"]) {
            Ok(mut found) => found.remove("after"),
            Err(_) => return admin_pages::not_a_place(viewer),
        };
        if let Some(after) = &after
            && matches!(check_name(after), Err(NameProblem::Invalid))
        {
            return admin_pages::not_a_place(viewer);
        }
        let read = match self
            .api
            .read_every_application(Which::After(after.as_deref()), MAX_ADMIN_PAGE)
            .await
        {
            Ok(read) => read,
            Err(AdminRead::Busy) => {
                return admin_pages::busy(viewer, Section::Applications, "Every Application");
            }
            Err(AdminRead::TooLong(error) | AdminRead::Failed(error)) => {
                return self.unavailable(viewer, &error);
            }
        };
        admin_pages::every_application(
            viewer,
            &read.rows,
            &read.declared,
            after.as_deref(),
            read.next(),
        )
    }

    /// `GET /admin/applications/<name>/disable`: the page that confirms
    /// it, with its usage from a reading of every Application.
    async fn confirm_disable(&self, viewer: &Viewer<'_>, name: &str) -> Answer {
        if check_name(name).is_err() {
            return admin_pages::no_application(viewer, name);
        }
        match self.api.read_every_application(Which::Named(name), 1).await {
            Ok(read) => match read.rows.first() {
                Some(application) => admin_pages::confirm_disable(viewer, application),
                None => admin_pages::no_application(viewer, name),
            },
            Err(AdminRead::Busy) => admin_pages::busy_card(viewer),
            Err(AdminRead::TooLong(error) | AdminRead::Failed(error)) => {
                self.unavailable(viewer, &error)
            }
        }
    }

    /// `POST /admin/applications/<name>/disable` (`disable`) and
    /// `/enable`: the change the API makes, by its own code; `303` to the
    /// Application's page when made. Nothing in the body is looked at.
    async fn switch<B>(&self, viewer: &Viewer<'_>, name: &str, disable: bool, body: B) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let read = Limited::new(body, MAX_FORM_BYTES).collect();
        match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                return pages::refused(
                    viewer,
                    name,
                    &Refusal::Malformed("The form is larger than 4 KiB.".to_owned()),
                );
            }
            Err(_) => return pages::refused(viewer, name, &Refusal::TimedOut),
        }
        match self.api.switch(viewer.user, name, disable).await {
            Ok(Some((_, changed))) => {
                let notice = match (changed, disable) {
                    (false, _) => "nothing",
                    (true, true) => "disabled",
                    (true, false) => "enabled",
                };
                pages::see_other(&format!("/applications/{name}?changed={notice}"))
            }
            Ok(None) => admin_pages::no_application(viewer, name),
            Err(error) => {
                crate::log::record(
                    "platform-database-failed",
                    serde_json::json!({ "error": error.to_string() }),
                );
                pages::refused(viewer, name, &Refusal::Unavailable)
            }
        }
    }

    /// `GET /admin/operation-log`: the entries `GET /v1/operation-log`
    /// answers to the same filters with `limit=100`, from the same reading.
    async fn operation_log_page(&self, viewer: &Viewer<'_>, query: Option<&str>) -> Answer {
        let typed = typed(query);
        // Each Resource's mark: a Storage Resource is never shown as a
        // database.
        let kinds = self.api.resource_kinds();
        let asked = match asked(query) {
            Ok(asked) => asked,
            Err(problem) => {
                let (field, words) = field_error(&problem, &typed);
                return admin_pages::operation_log(
                    viewer,
                    &typed,
                    &LogShown::Malformed { field, words },
                    &kinds,
                );
            }
        };
        match self.api.read_operation_log(&asked).await {
            Ok((entries, earlier)) => admin_pages::operation_log(
                viewer,
                &typed,
                &LogShown::Entries {
                    asked: &asked,
                    entries: &entries,
                    earlier,
                },
                &kinds,
            ),
            Err(AdminRead::Busy) => {
                admin_pages::operation_log(viewer, &typed, &LogShown::Busy, &kinds)
            }
            Err(AdminRead::TooLong(_)) => {
                admin_pages::operation_log(viewer, &typed, &LogShown::TooLong, &kinds)
            }
            Err(AdminRead::Failed(error)) => self.unavailable(viewer, &error),
        }
    }

    /// `GET /admin/resources`: every Resource with its Grants, as `GET
    /// /v1/resources` and `GET /v1/resources/<name>/grants` answer them
    /// (memory, read back where it cannot tell), once the platform
    /// database is seen to answer.
    async fn resources_page(&self, viewer: &Viewer<'_>) -> Answer {
        if let Err(error) = self.registry.probe().await {
            return self.unavailable(viewer, &error);
        }
        match self.api.known_resources().await {
            Ok(all) => admin_pages::resources(viewer, &all),
            Err(resource) => admin_pages::grants_unknown(viewer, &resource),
        }
    }

    /// `GET /admin/outbound`: the list from memory, as `GET
    /// /v1/outbound-hosts` answers it, once the platform database is seen
    /// to answer (a page of the Dashboard tells nothing while it cannot).
    async fn outbound_page(&self, viewer: &Viewer<'_>) -> Answer {
        if let Err(error) = self.registry.probe().await {
            return self.unavailable(viewer, &error);
        }
        let entries = self.api.outbound_entries();
        admin_pages::outbound(viewer, &entries)
    }

    /// `GET /admin/installation`: what `GET /v1/installation` answers, and
    /// with OIDC the connection as `GET /v1/oidc` reads it.
    async fn installation_page(&self, viewer: &Viewer<'_>) -> Answer {
        let (domain, sign_in, address) = self.api.installation_facts();
        let connection = if sign_in == "oidc" {
            match self.api.oidc_connection().await {
                Ok(connection) => connection,
                Err(error) => return self.unavailable(viewer, &error),
            }
        } else {
            if let Err(error) = self.registry.probe().await {
                return self.unavailable(viewer, &error);
            }
            None
        };
        admin_pages::installation(
            viewer,
            domain,
            sign_in,
            address.as_deref(),
            connection.as_ref(),
        )
    }
}

/// The filters as the form holds them again: the first value of each,
/// white space dropped; a time as a `datetime-local` field holds it, in
/// UTC.
fn typed(query: Option<&str>) -> Typed {
    // Owned copies of what was typed (each at most what one address
    // holds): the page writes them back into its form.
    let mut typed = Typed::default();
    let mut seen = std::collections::BTreeSet::new();
    for (name, value) in form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        if !seen.insert(name.clone()) {
            continue;
        }
        let value = value.trim();
        match name.as_ref() {
            "user" => value.clone_into(&mut typed.user),
            "application" => value.clone_into(&mut typed.application),
            "resource" => value.clone_into(&mut typed.resource),
            "host" => value.clone_into(&mut typed.host),
            "outcome" => value.clone_into(&mut typed.outcome),
            "since" | "until" => {
                let held = as_rfc3339(value)
                    .and_then(|t| rfc3339_micros(&t))
                    .map(datetime_local);
                let (slot, quoted) = if name == "since" {
                    (&mut typed.since, &mut typed.since_typed)
                } else {
                    (&mut typed.until, &mut typed.until_typed)
                };
                match held {
                    Some(held) => *slot = held,
                    None => value.clone_into(quoted),
                }
            }
            _ => {}
        }
    }
    typed
}

/// The filters read as the API reads them: an empty value (white space
/// dropped) is a filter not given (but `before`, which no field sends); a
/// time as a `datetime-local` field sends it is read in UTC.
fn asked(query: Option<&str>) -> Result<LogQuery, LogProblem> {
    let found = query_parameters(query, &LOG_FILTERS).map_err(LogProblem::Repeated)?;
    let mut kept = std::collections::BTreeMap::new();
    for (name, value) in found {
        let value = value.trim();
        if value.is_empty() && name != "before" {
            continue;
        }
        let value = match name {
            "since" | "until" => as_rfc3339(value).unwrap_or_else(|| value.to_owned()),
            _ => value.to_owned(),
        };
        kept.insert(name, value);
    }
    LogQuery::of_filters(kept)
}

/// A time as RFC 3339 the API takes: as given when it is; else as a
/// browser's `datetime-local` field sends it (`YYYY-MM-DDTHH:MM`, with
/// seconds, with a fraction of up to six digits), read as UTC.
fn as_rfc3339(text: &str) -> Option<String> {
    if rfc3339_micros(text).is_some() {
        return Some(text.to_owned());
    }
    [format!("{text}Z"), format!("{text}:00Z")]
        .into_iter()
        .find(|t| rfc3339_micros(t).is_some())
}

/// Microseconds since 1970 as a `datetime-local` field holds them, in UTC:
/// `YYYY-MM-DDTHH:MM:SS`, to the second, its fraction cut (not rounded):
/// the field shows no finer time, and none fits it at 320 pixels.
fn datetime_local(micros: i64) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// The field a refused filter is beside, and words saying what was wrong
/// and what is taken.
fn field_error(problem: &LogProblem, typed: &Typed) -> (&'static str, String) {
    let quoted = |text: &str| {
        if text.is_empty() {
            String::new()
        } else {
            format!(" \u{201c}{}\u{201d}", crate::oidc::pages::escape(text))
        }
    };
    match problem {
        LogProblem::Repeated(name) => (
            *name,
            "This filter is given twice in the address: give it once.".to_owned(),
        ),
        LogProblem::Order => (
            "until",
            "\u{201c}Before\u{201d} must be later than \u{201c}From\u{201d}.".to_owned(),
        ),
        LogProblem::Value("user") => (
            "user",
            "A user id is 1 to 256 bytes, without control characters.".to_owned(),
        ),
        LogProblem::Value("application") => (
            "application",
            "An Application's name is 1 to 63 lower-case letters, digits and hyphens.".to_owned(),
        ),
        LogProblem::Value("resource") => (
            "resource",
            "A Resource's name is lower-case letters, digits and underscores.".to_owned(),
        ),
        LogProblem::Value("outcome") => (
            "outcome",
            "Choose an outcome: allowed, refused, failed or unfinished.".to_owned(),
        ),
        LogProblem::Value("host") => (
            "host",
            "A host is a name or an address, with its port when it is not 443, as the list writes one (api.example.com, api.example.com:8443, [2001:db8::7])."
                .to_owned(),
        ),
        LogProblem::Value("since") => (
            "since",
            format!(
                "The time{} cannot be read: give a day and a time in UTC, such as 2026-11-21T09:30.",
                quoted(&typed.since_typed)
            ),
        ),
        LogProblem::Value("until") => (
            "until",
            format!(
                "The time{} cannot be read: give a day and a time in UTC, such as 2026-11-21T09:30.",
                quoted(&typed.until_typed)
            ),
        ),
        LogProblem::Value(_) => (
            "before",
            "This address does not name a place in the Operation log.".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_of_a_datetime_local_field_is_read_as_utc() {
        assert_eq!(
            as_rfc3339("2026-11-21T10:00").as_deref(),
            Some("2026-11-21T10:00:00Z")
        );
        assert_eq!(
            as_rfc3339("2026-11-21T10:00:05").as_deref(),
            Some("2026-11-21T10:00:05Z")
        );
        assert_eq!(
            as_rfc3339("2026-11-21T10:00:05.123456").as_deref(),
            Some("2026-11-21T10:00:05.123456Z")
        );
        assert_eq!(
            as_rfc3339("2026-11-21T10:00:05+01:00").as_deref(),
            Some("2026-11-21T10:00:05+01:00")
        );
        assert_eq!(as_rfc3339("2026-11-21T10:00:05.1234567"), None);
        assert_eq!(as_rfc3339("yesterday"), None);
    }

    #[test]
    fn a_time_is_held_as_a_datetime_local_field_holds_it_in_utc_to_the_second() {
        let micros = rfc3339_micros("2026-11-21T10:00:05.123456+01:00").unwrap_or_default();
        assert_eq!(datetime_local(micros), "2026-11-21T09:00:05");
        // The fraction is cut, not rounded.
        let micros = rfc3339_micros("2026-11-21T09:00:05.999999Z").unwrap_or_default();
        assert_eq!(datetime_local(micros), "2026-11-21T09:00:05");
        let micros = rfc3339_micros("2024-02-29T23:59:59Z").unwrap_or_default();
        assert_eq!(datetime_local(micros), "2024-02-29T23:59:59");
        let micros = rfc3339_micros("1970-01-01T00:00:00Z").unwrap_or_default();
        assert_eq!(datetime_local(micros), "1970-01-01T00:00:00");
    }
}
