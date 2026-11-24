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
use hyper::{Method, Response, StatusCode};
use url::form_urlencoded;

use super::admin_pages::{self, LogShown, Section, Typed};
use super::pages::{self, Refusal, Viewer};
use super::{Ingress, MAX_FORM_BYTES};
use crate::api::{
    AdminRead, AdminResourceRead, LOG_FILTERS, LogProblem, LogQuery, MAX_ADMIN_PAGE, Which,
    query_parameters, rfc3339_micros,
};
use crate::ingress::BODY_READ_TIMEOUT;
use crate::registry::{NameProblem, check_name};
use crate::resources::GrantWritten;

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
    /// `/admin/resources/<name>`
    Resource(&'a str),
    /// `/admin/resources/<name>/grants`
    ResourceGrants(&'a str),
    /// `/admin/resources/<name>/revoke`
    ResourceRevoke(&'a str),
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
        if let Some(rest) = path.strip_prefix("/admin/resources/") {
            if rest.is_empty() {
                return None;
            }
            return match rest.split_once('/') {
                None => Some(Self::Resource(rest)),
                Some((name, "grants")) if !name.is_empty() => Some(Self::ResourceGrants(name)),
                Some((name, "revoke")) if !name.is_empty() => Some(Self::ResourceRevoke(name)),
                _ => None,
            };
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
            Some(Route::ResourceGrants(name)) if method == Method::POST => {
                self.set_grant(viewer, name, body).await
            }
            Some(Route::ResourceGrants(_)) => pages::method_not_allowed(viewer, "POST"),
            Some(Route::ResourceRevoke(name)) if read => {
                self.confirm_revoke(viewer, name, query).await
            }
            Some(Route::ResourceRevoke(name)) if method == Method::POST => {
                self.revoke(viewer, name, body).await
            }
            Some(Route::ResourceRevoke(_)) => pages::method_not_allowed(viewer, "GET, HEAD, POST"),
            Some(_) if !read => pages::method_not_allowed(viewer, "GET, HEAD"),
            Some(Route::Root) => pages::see_other("/admin/applications"),
            Some(Route::Applications) => self.every_application_page(viewer, query).await,
            Some(Route::OperationLog) => self.operation_log_page(viewer, query).await,
            Some(Route::Resources) => self.resources_page(viewer, query).await,
            Some(Route::Resource(name)) => self.resource_page(viewer, name, query).await,
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
        match read_form_body(body).await {
            Ok(_) => {}
            Err(FormProblem::TooLarge) => {
                return pages::refused(
                    viewer,
                    name,
                    &Refusal::Malformed("The form is larger than 4 KiB.".to_owned()),
                );
            }
            Err(FormProblem::Unreadable) => {
                return pages::refused(
                    viewer,
                    name,
                    &Refusal::Malformed("The form could not be read: send it again.".to_owned()),
                );
            }
            Err(FormProblem::TimedOut) => return pages::refused(viewer, name, &Refusal::TimedOut),
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

    /// `GET /admin/resources`: one page of `GET /v1/admin/resources`, from
    /// the same reading, once the platform database is seen to answer.
    async fn resources_page(&self, viewer: &Viewer<'_>, query: Option<&str>) -> Answer {
        let after = match query_parameters(query, &["after"]) {
            Ok(mut found) => found.remove("after"),
            Err(_) => return admin_pages::resources_not_a_place(viewer),
        };
        if let Some(after) = &after
            && !crate::resources::is_resource_name(after)
        {
            return admin_pages::resources_not_a_place(viewer);
        }
        if let Err(error) = self.registry.probe().await {
            return self.unavailable(viewer, &error);
        }
        match self
            .api
            .read_admin_resources(after.as_deref(), usize::from(MAX_ADMIN_PAGE))
            .await
        {
            Ok(read) => admin_pages::resources(viewer, &read, after.as_deref()),
            Err(AdminResourceRead::Unknown(resource)) => {
                admin_pages::grants_unknown(viewer, &resource)
            }
            Err(AdminResourceRead::Failed(error)) => self.unavailable(viewer, &error),
            Err(AdminResourceRead::NoSuchResource | AdminResourceRead::Found(_)) => {
                admin_pages::resources_not_a_place(viewer)
            }
        }
    }

    /// `GET /admin/resources/<name>`: the Resource as `GET
    /// /v1/admin/resources/<name>` answers it and its Grants as `GET
    /// /v1/resources/<name>/grants` does, with the notice a change left.
    async fn resource_page(&self, viewer: &Viewer<'_>, name: &str, query: Option<&str>) -> Answer {
        if let Err(error) = self.registry.probe().await {
            return self.unavailable(viewer, &error);
        }
        let read = match self.api.read_admin_resource(name).await {
            AdminResourceRead::Found(read) => read,
            AdminResourceRead::NoSuchResource => return admin_pages::no_resource(viewer, name),
            AdminResourceRead::Unknown(resource) => {
                return admin_pages::grants_unknown(viewer, &resource);
            }
            AdminResourceRead::Failed(error) => return self.unavailable(viewer, &error),
        };
        let notice = resource_notice(query);
        admin_pages::resource_page(
            viewer,
            &read,
            StatusCode::OK,
            notice
                .as_ref()
                .map(|(changed, group)| (*changed, group.as_str())),
            &admin_pages::GrantForm::default(),
        )
    }

    /// `POST /admin/resources/<name>/grants`: the change `POST
    /// /v1/resources/<name>/grants` makes, through the same write
    /// ([`crate::resources::Resources::write_grant`]): its checks, records
    /// and bounds are not written twice.
    async fn set_grant<B>(&self, viewer: &Viewer<'_>, name: &str, body: B) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let Some(resource) = (match self.resources.find(name).await {
            Ok(found) => found,
            Err(error) => return self.unavailable(viewer, &error),
        }) else {
            return admin_pages::no_resource(viewer, name);
        };
        let bytes = match read_form_body(body).await {
            Ok(bytes) => bytes,
            Err(FormProblem::TooLarge) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "The form is larger than 4 KiB: send it again with less.",
                );
            }
            Err(FormProblem::Unreadable) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "The form could not be read: send it again.",
                );
            }
            Err(FormProblem::TimedOut) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::REQUEST_TIMEOUT,
                    "timed-out",
                    "The form did not arrive in time: send it again.",
                );
            }
        };
        let form = Asked::read(&bytes);
        let group = form.group.given();
        let level = form
            .access
            .given()
            .as_deref()
            .and_then(crate::resources::level_of);
        let group_error = match &group {
            Some(group) if crate::resources::is_group(group) => None,
            Some(_) => Some(
                "A Group is 1 to 256 bytes of UTF-8 without a control character: write it exactly as the company's directory names it."
                    .to_owned(),
            ),
            None if matches!(form.group, Field::Twice) => {
                Some("The form gives the Group twice: give it once.".to_owned())
            }
            None => Some(
                "Give the Group: 1 to 256 bytes of UTF-8 without a control character, written exactly as the company's directory names it."
                    .to_owned(),
            ),
        };
        let access_error = match level {
            Some(level) if crate::resources::level_allowed(resource.kind, level) => None,
            _ => Some(if resource.kind == crate::registry::ResourceKind::Storage {
                "A Storage Resource has two levels, read and write: choose one of them.".to_owned()
            } else {
                "Access is Structure, Read or Write: choose one of those.".to_owned()
            }),
        };
        if group_error.is_some() || access_error.is_some() {
            let read = match self.api.read_admin_resource(&resource.name).await {
                AdminResourceRead::Found(read) => read,
                AdminResourceRead::NoSuchResource => {
                    return admin_pages::no_resource(viewer, name);
                }
                AdminResourceRead::Unknown(resource) => {
                    return admin_pages::grants_unknown(viewer, &resource);
                }
                AdminResourceRead::Failed(error) => return self.unavailable(viewer, &error),
            };
            return admin_pages::resource_page(
                viewer,
                &read,
                StatusCode::BAD_REQUEST,
                None,
                &admin_pages::GrantForm {
                    group: group.unwrap_or_default(),
                    access: form.access.given(),
                    group_error,
                    access_error,
                },
            );
        }
        // Both were found good above; a level is never made up here.
        let (Some(group), Some(level)) = (group, level) else {
            return admin_pages::resource_not_taken(
                viewer,
                name,
                StatusCode::BAD_REQUEST,
                "malformed",
                "The form could not be read: send it again.",
            );
        };
        match self
            .resources
            .write_grant(&resource, &group, Some(level), &viewer.user.id)
            .await
        {
            Ok(GrantWritten::Written(change)) => {
                let changed = if !change.changed {
                    "nothing"
                } else if change.was.is_none() {
                    "granted"
                } else {
                    "changed"
                };
                pages::see_other(&format!(
                    "/admin/resources/{name}?changed={changed}&group={}",
                    crate::resources::encode_segment(&group)
                ))
            }
            Ok(GrantWritten::TooMany) => admin_pages::resource_not_taken(
                viewer,
                name,
                StatusCode::CONFLICT,
                "too-many",
                "The Resource holds the 1,000 Grants it may hold: revoke one that is no longer needed before giving another.",
            ),
            Ok(GrantWritten::NoSuchResource) => admin_pages::no_resource(viewer, name),
            Ok(GrantWritten::Unknown) => admin_pages::resource_finding_out(viewer, name),
            Ok(GrantWritten::Busy) => admin_pages::resource_busy(viewer, name),
            Err(error) => {
                self.database_failed(&error);
                admin_pages::write_unavailable(viewer, name)
            }
        }
    }

    /// `GET /admin/resources/<name>/revoke?group=<group>`: the page that
    /// names the Group and the Resource and asks.
    async fn confirm_revoke(&self, viewer: &Viewer<'_>, name: &str, query: Option<&str>) -> Answer {
        if let Err(error) = self.registry.probe().await {
            return self.unavailable(viewer, &error);
        }
        let read = match self.api.read_admin_resource(name).await {
            AdminResourceRead::Found(read) => read,
            AdminResourceRead::NoSuchResource => return admin_pages::no_resource(viewer, name),
            AdminResourceRead::Unknown(resource) => {
                return admin_pages::grants_unknown(viewer, &resource);
            }
            AdminResourceRead::Failed(error) => return self.unavailable(viewer, &error),
        };
        let group = match query_parameters(query, &["group"]) {
            Ok(mut found) => found.remove("group"),
            Err(_) => return admin_pages::revoke_malformed(viewer, name),
        };
        let Some(group) = group.filter(|group| crate::resources::is_group(group)) else {
            return admin_pages::revoke_malformed(viewer, name);
        };
        let held = read.grants.iter().find(|grant| grant.group == group);
        admin_pages::confirm_revoke(viewer, &read, &group, held)
    }

    /// `POST /admin/resources/<name>/revoke`: the change `DELETE
    /// /v1/resources/<name>/grants/<group>` makes, through the same write.
    async fn revoke<B>(&self, viewer: &Viewer<'_>, name: &str, body: B) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let Some(resource) = (match self.resources.find(name).await {
            Ok(found) => found,
            Err(error) => return self.unavailable(viewer, &error),
        }) else {
            return admin_pages::no_resource(viewer, name);
        };
        let bytes = match read_form_body(body).await {
            Ok(bytes) => bytes,
            Err(FormProblem::TooLarge) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "The form is larger than 4 KiB: send it again with less.",
                );
            }
            Err(FormProblem::Unreadable) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "The form could not be read: send it again.",
                );
            }
            Err(FormProblem::TimedOut) => {
                return admin_pages::resource_not_taken(
                    viewer,
                    name,
                    StatusCode::REQUEST_TIMEOUT,
                    "timed-out",
                    "The form did not arrive in time: send it again.",
                );
            }
        };
        let form = Asked::read(&bytes);
        let group = match form.group {
            Field::Given(group) if crate::resources::is_group(&group) => group,
            _ => return admin_pages::revoke_malformed(viewer, name),
        };
        match self
            .resources
            .write_grant(&resource, &group, None, &viewer.user.id)
            .await
        {
            Ok(GrantWritten::Written(change)) => pages::see_other(&format!(
                "/admin/resources/{name}?changed={}&group={}",
                if change.changed { "revoked" } else { "nothing" },
                crate::resources::encode_segment(&group)
            )),
            Ok(GrantWritten::NoSuchResource) => admin_pages::no_resource(viewer, name),
            Ok(GrantWritten::Unknown) => admin_pages::resource_finding_out(viewer, name),
            Ok(GrantWritten::Busy) => admin_pages::resource_busy(viewer, name),
            Ok(GrantWritten::TooMany) => admin_pages::resource_not_taken(
                viewer,
                name,
                StatusCode::CONFLICT,
                "too-many",
                "The Resource holds the 1,000 Grants it may hold: revoke one that is no longer needed.",
            ),
            Err(error) => {
                self.database_failed(&error);
                admin_pages::write_unavailable(viewer, name)
            }
        }
    }

    /// One record of a platform database that did not answer, as the API's
    /// own `503` writes.
    fn database_failed(&self, error: &crate::platform_db::DbError) {
        crate::log::record(
            "platform-database-failed",
            serde_json::json!({ "error": error.to_string() }),
        );
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

/// Why a form's body could not be read.
enum FormProblem {
    TooLarge,
    TimedOut,
    Unreadable,
}

/// The form's body, at most 4 KiB, whole within 10 seconds.
async fn read_form_body<B>(body: B) -> Result<Bytes, FormProblem>
where
    B: Body<Data = Bytes> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let read = Limited::new(body, MAX_FORM_BYTES).collect();
    match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error)) => {
            if error
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                Err(FormProblem::TooLarge)
            } else {
                Err(FormProblem::Unreadable)
            }
        }
        Err(_) => Err(FormProblem::TimedOut),
    }
}

/// One field of a form: not given, given once, or given twice.
#[derive(Default)]
enum Field {
    #[default]
    Missing,
    Given(String),
    Twice,
}

impl Field {
    fn given(&self) -> Option<String> {
        match self {
            Self::Given(value) => Some(value.clone()),
            Self::Missing | Self::Twice => None,
        }
    }
}

/// The fields of a Grant's form as sent, `group` and `access`; another
/// field is not looked at, and neither is white space dropped.
#[derive(Default)]
struct Asked {
    group: Field,
    access: Field,
}

impl Asked {
    fn read(body: &[u8]) -> Self {
        let mut form = Self::default();
        for (name, value) in form_urlencoded::parse(body) {
            let field = match name.as_ref() {
                "group" => &mut form.group,
                "access" => &mut form.access,
                _ => continue,
            };
            *field = match std::mem::take(field) {
                Field::Missing => Field::Given(value.into_owned()),
                _ => Field::Twice,
            };
        }
        form
    }
}

/// The notice a change leads to: `?changed=` with `&group=`, both as the
/// address gives them; any other value, a missing Group or one that is none
/// has no notice.
fn resource_notice(query: Option<&str>) -> Option<(&'static str, String)> {
    let found = query_parameters(query, &["changed", "group"]).ok()?;
    let changed = match found.get("changed").map(String::as_str) {
        Some("granted") => "granted",
        Some("changed") => "changed",
        Some("revoked") => "revoked",
        Some("nothing") => "nothing",
        _ => return None,
    };
    let group = found.get("group")?;
    crate::resources::is_group(group).then(|| (changed, group.clone()))
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
