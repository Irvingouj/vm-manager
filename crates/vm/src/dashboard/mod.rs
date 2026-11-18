//! The Dashboard, at `dashboard.<domain>` (blackbox/CONTRACT.md, "The
//! Dashboard"): the first page an employee sees. Pages of the platform's
//! own, with links and forms and no script ([`pages`]).
//!
//! Ingress signs a request to it in as it signs one to an Application in
//! (with OIDC the session is an Application session named
//! [`DASHBOARD_NAME`], crate::oidc::signs_in_to; with the stand-in its
//! headers), refuses one from another origin, and hands the rest to
//! [`Ingress::dashboard`]:
//!
//! - `GET /`: the list. Exactly what `GET /v1/applications` answers the
//!   User, read by the same query of the registry
//!   ([`Registry::listed_for`], one page of it), then what each of the
//!   page's Applications shows besides ([`Registry::shown`], one query for
//!   the whole page: its serving Version's declared Resources and its first
//!   20 Contributors with the count of all).
//! - `GET /applications/<name>`: one Application, to whoever can open it
//!   and to a Platform administrator, as the API's `GET
//!   /v1/applications/<name>` (the same registry read and the same rule).
//! - `POST /applications/<name>/sharing`: a form that is turned into the
//!   change `POST /v1/applications/<name>/sharing` makes, and made by the
//!   API's own code (crate::api's `change_sharing`): the same rules, the
//!   same refusals, the same `sharing-changed` record.
//!
//! A User's access to each declared Resource comes from the Grants in
//! memory (crate::resources); those memory does not know now are read back
//! as the API's paths read them, within one limit for the whole page, and
//! shown `unknown` while they cannot be told.
//!
//! Statements on the platform database for a page (beside the session's,
//! with OIDC): the person's name (with OIDC only), then for the list the
//! page of the listing and, when it holds an Application, the query of
//! what the page shows: four at most, whatever the number of Applications
//! on it. For one Application: the registry's read of it (two), and the
//! query of what it shows (one).

mod pages;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use authz::AccessLevel;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::CONTENT_TYPE;
use hyper::http::request::Parts;
use hyper::{Method, Response};
use url::form_urlencoded;

use crate::api::{ListChanged, MAX_ENTRY_BYTES, entry_text};
use crate::ingress::{BODY_READ_TIMEOUT, Ingress};
use crate::oidc::AUTH_NAME;
use crate::registry::{Asked, Change, Changed, List, Listing, Named, check_name};
use crate::resources::{READ_BACK_WAIT, Resource};
use crate::user::User;
use pages::{Notice, Refusal, Viewer};

/// The name under the domain where the Dashboard is. One of the names no
/// Application can have (crate::registry::RESERVED_NAMES).
pub const DASHBOARD_NAME: &str = "dashboard";

/// How many Applications a page of the list holds.
const PAGE_SIZE: i64 = 100;

/// The highest `?page=`.
const MAX_PAGE: i64 = 10_000;

/// How many Contributors an Application shows in the list.
const CONTRIBUTORS_LISTED: i64 = 20;

/// The largest body of a change of sharing.
const MAX_FORM_BYTES: usize = 4096;

type Answer = Response<Full<Bytes>>;

/// The Dashboard's paths.
#[derive(Clone, Copy)]
enum Route<'a> {
    List,
    Application(&'a str),
    Sharing(&'a str),
}

impl<'a> Route<'a> {
    /// The route of `path`, as sent; `None` for any other path.
    fn of(path: &'a str) -> Option<Self> {
        if path == "/" {
            return Some(Self::List);
        }
        let rest = path.strip_prefix("/applications/")?;
        match rest.split_once('/') {
            None if !rest.is_empty() => Some(Self::Application(rest)),
            Some((name, "sharing")) if !name.is_empty() => Some(Self::Sharing(name)),
            _ => None,
        }
    }
}

/// What the page shows of a User's standing with one declared Resource.
pub(crate) struct Seen {
    /// The Resource, when one of that name is registered.
    resource: Option<Arc<Resource>>,
    /// The level they hold (`None` inside: no Grant); `None` while memory
    /// cannot tell.
    held: Option<Option<AccessLevel>>,
}

/// A form of the Dashboard's, read: what it asks for.
struct ShareForm {
    share: bool,
    asked: Asked,
}

impl Ingress {
    /// Answers a request to the Dashboard of `user`, signed in and from
    /// the Dashboard's own origin.
    pub(crate) async fn dashboard<B>(
        &self,
        parts: &Parts,
        body: B,
        user: &User,
        port: Option<u16>,
    ) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let viewer = self.viewer(user, port).await;
        let read = parts.method == Method::GET || parts.method == Method::HEAD;
        let mut answer = match (Route::of(parts.uri.path()), read) {
            (None, _) => pages::not_found(&viewer),
            (Some(Route::List), true) => self.list(&viewer, parts.uri.query(), port).await,
            (Some(Route::Application(name)), true) => {
                self.application(&viewer, name, parts.uri.query(), port)
                    .await
            }
            (Some(Route::Sharing(name)), false) if parts.method == Method::POST => {
                self.sharing(&viewer, parts, name, body).await
            }
            (Some(Route::List | Route::Application(_)), false) => {
                pages::method_not_allowed(&viewer, "GET, HEAD")
            }
            (Some(Route::Sharing(_)), _) => pages::method_not_allowed(&viewer, "POST"),
        };
        if parts.method == Method::HEAD {
            *answer.body_mut() = Full::new(Bytes::new());
        }
        answer
    }

    /// Whom a page says the person is signed in as: with OIDC their name,
    /// read for it (when that fails, the page shows the id alone), and the
    /// sign-in address's sign-out; the stand-in keeps no record of people
    /// and has no sign-out.
    async fn viewer<'a>(&self, user: &'a User, port: Option<u16>) -> Viewer<'a> {
        if self.oidc.as_ref().is_some_and(|o| o.connected().is_some()) {
            let name = self.registry.person_name(&user.id).await.ok().flatten();
            let sign_out = format!(
                "{}/sign-out",
                self.public
                    .origin(&format!("{AUTH_NAME}.{}", self.domain), port)
            );
            Viewer {
                user,
                name,
                sign_out: Some(sign_out),
            }
        } else {
            Viewer {
                user,
                name: None,
                sign_out: None,
            }
        }
    }

    /// An Application's address as a browser reaches it, with the path `/`.
    fn open_address(&self, name: &str, port: Option<u16>) -> String {
        format!(
            "{}/",
            self.public.origin(&format!("{name}.{}", self.domain), port)
        )
    }

    /// `GET /`.
    async fn list(&self, viewer: &Viewer<'_>, query: Option<&str>, port: Option<u16>) -> Answer {
        let Some(page) = page_of(query) else {
            return pages::bad_page(viewer);
        };
        let offset = (page - 1) * PAGE_SIZE;
        // One more than a page: whether there is a next one.
        let listing = Listing::Paged {
            limit: PAGE_SIZE + 1,
            offset,
        };
        let mut listed = match self.registry.listed_for(viewer.user, listing).await {
            Ok(listed) => listed,
            Err(error) => return self.unavailable(viewer, &error),
        };
        let more = listed.len() > usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX);
        listed.truncate(usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX));
        let names: Vec<&str> = listed.iter().map(|a| a.name.as_str()).collect();
        let shown = if names.is_empty() {
            HashMap::new()
        } else {
            match self.registry.shown(&names, CONTRIBUTORS_LISTED).await {
                Ok(shown) => shown,
                Err(error) => return self.unavailable(viewer, &error),
            }
        };
        let declared: BTreeSet<&str> = shown
            .values()
            .flat_map(|s| s.declared.keys().map(String::as_str))
            .collect();
        let seen = self.seen(viewer.user, declared).await;
        let blocks: Vec<pages::Block<'_>> = listed
            .iter()
            .map(|listed| {
                let shown = shown.get(&listed.name);
                pages::Block {
                    name: &listed.name,
                    description: listed.description.as_deref(),
                    relation: Some(if listed.contributor {
                        "contributor"
                    } else {
                        "shared"
                    }),
                    serving: listed.serving_version,
                    open: listed
                        .serving_version
                        .map(|_| self.open_address(&listed.name, port)),
                    address: None,
                    details: true,
                    contributors: shown.map_or_else(Vec::new, |s| {
                        s.contributors
                            .iter()
                            .map(|(named, person)| {
                                pages::Person::of(named, person.as_deref(), None)
                            })
                            .collect()
                    }),
                    more_contributors: shown.map_or(0, |s| {
                        s.contributors_count
                            .saturating_sub(i64::try_from(s.contributors.len()).unwrap_or(0))
                    }),
                    declared: shown.map(|s| &s.declared),
                    seen: &seen,
                }
            })
            .collect();
        pages::list(viewer, &blocks, page, more)
    }

    /// `GET /applications/<name>`: by whoever can open it, and by a
    /// Platform administrator, as `GET /v1/applications/<name>`.
    async fn application(
        &self,
        viewer: &Viewer<'_>,
        name: &str,
        query: Option<&str>,
        port: Option<u16>,
    ) -> Answer {
        let user = viewer.user;
        if check_name(name).is_err() {
            return pages::no_application(viewer, name);
        }
        let record = match self.registry.application(name).await {
            Ok(Some(record)) => record,
            Ok(None) => return pages::no_application(viewer, name),
            Err(error) => return self.unavailable(viewer, &error),
        };
        let contributor = record.is_contributor(user);
        let opens = record.can_open(user);
        let administrator = self.api.is_administrator(user);
        if !opens && !administrator {
            return pages::not_shared(viewer, name);
        }
        let shown = match self.registry.shown(&[name], 0).await {
            Ok(shown) => shown,
            Err(error) => return self.unavailable(viewer, &error),
        };
        let declared = shown.get(name).map(|s| &s.declared);
        let seen = self
            .seen(
                user,
                declared
                    .into_iter()
                    .flat_map(|d| d.keys().map(String::as_str))
                    .collect(),
            )
            .await;
        let relation = if contributor {
            Some("contributor")
        } else if opens {
            Some("shared")
        } else {
            None
        };
        let block = pages::Block {
            name: &record.name,
            description: record.description.as_deref(),
            relation,
            serving: record.serving_version,
            open: record
                .serving_version
                .map(|_| self.open_address(&record.name, port)),
            address: Some(
                self.public
                    .origin(&format!("{}.{}", record.name, self.domain), port),
            ),
            details: false,
            contributors: record
                .contributors
                .iter()
                .map(|e| pages::Person::of(&e.named, e.name.as_deref(), e.email.as_deref()))
                .collect(),
            more_contributors: 0,
            declared,
            seen: &seen,
        };
        let shared: Option<Vec<pages::Person<'_>>> = (contributor || administrator).then(|| {
            record
                .shared_with
                .iter()
                .map(|e| pages::Person::of(&e.named, e.name.as_deref(), e.email.as_deref()))
                .collect()
        });
        pages::application(
            viewer,
            &block,
            shared.as_deref(),
            contributor,
            notice_of(query),
        )
    }

    /// `POST /applications/<name>/sharing`: the change the API makes, by
    /// its own code; `303` to the Application's page when made.
    async fn sharing<B>(&self, viewer: &Viewer<'_>, parts: &Parts, name: &str, body: B) -> Answer
    where
        B: Body<Data = Bytes> + Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // The body is read before anything is decided, bounded; what is
        // wrong with it is told after `not-found` and `not-a-contributor`.
        let read = Limited::new(body, MAX_FORM_BYTES).collect();
        let form = match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(collected)) => {
                if is_form(parts) {
                    read_form(&collected.to_bytes())
                } else {
                    Err("The change did not come as the form of this page.".to_owned())
                }
            }
            Ok(Err(_)) => Err("The form is larger than 4 KiB.".to_owned()),
            Err(_) => {
                return pages::refused(viewer, name, &Refusal::TimedOut);
            }
        };
        let share = form.as_ref().map_or(true, |f| f.share);
        let change = form.map(|form| {
            let (add, remove) = if form.share {
                (vec![form.asked], Vec::new())
            } else {
                (Vec::new(), vec![form.asked])
            };
            Change {
                list: List::SharedWith,
                add,
                remove,
            }
        });
        let made = match self.api.change_sharing(viewer.user, name, change).await {
            Ok(made) => made,
            Err(error) => {
                crate::log::record(
                    "platform-database-failed",
                    serde_json::json!({ "error": error.to_string() }),
                );
                return pages::refused(viewer, name, &Refusal::Unavailable);
            }
        };
        let refusal = match made {
            ListChanged::NoSuchApplication | ListChanged::Made(Changed::NoSuchApplication) => {
                Refusal::NotFound
            }
            ListChanged::NotAContributor | ListChanged::Made(Changed::NotAContributor) => {
                Refusal::NotAContributor
            }
            ListChanged::Malformed(problem) => Refusal::Malformed(problem),
            ListChanged::Made(Changed::Changed { added, removed, .. }) => {
                let changed = match (added.is_empty(), removed.is_empty()) {
                    (true, true) => "nothing",
                    _ if share => "shared",
                    _ => "unshared",
                };
                return pages::see_other(&format!("/applications/{name}?changed={changed}"));
            }
            ListChanged::Made(Changed::UnknownEmail(email)) => Refusal::UnknownEmail(email),
            ListChanged::Made(Changed::AmbiguousEmail(email)) => Refusal::AmbiguousEmail(email),
            ListChanged::Made(Changed::TooMany(_)) => Refusal::TooMany,
            // Neither can come of one entry of whom it is shared with;
            // said as the API would say them, all the same.
            ListChanged::Made(Changed::LastContributor(_)) => Refusal::LastContributor,
            ListChanged::Made(Changed::SameInBoth(_)) => {
                Refusal::Malformed("The same person is both shared with and unshared.".to_owned())
            }
        };
        pages::refused(viewer, name, &refusal)
    }

    /// The `503` page of a read of the platform database that failed.
    fn unavailable(&self, viewer: &Viewer<'_>, error: &crate::platform_db::DbError) -> Answer {
        crate::log::record(
            "platform-database-failed",
            serde_json::json!({ "error": error.to_string() }),
        );
        pages::unavailable(viewer)
    }

    /// The User's standing with each of `names`, from the Grants in memory:
    /// those memory does not know now are read back first, as the API's
    /// paths read them, all within one limit ([`READ_BACK_WAIT`]); each is
    /// read at most once.
    async fn seen<'n>(&self, user: &User, names: BTreeSet<&'n str>) -> BTreeMap<&'n str, Seen> {
        let groups: Vec<&str> = user.groups.iter().map(String::as_str).collect();
        let deadline = tokio::time::Instant::now() + READ_BACK_WAIT;
        let mut seen = BTreeMap::new();
        for name in names {
            let Some(resource) = self.resources.get(name) else {
                seen.insert(
                    name,
                    Seen {
                        resource: None,
                        held: Some(None),
                    },
                );
                continue;
            };
            if !resource.grants_known() {
                self.resources.read_back(&resource, deadline).await;
            }
            let held = resource.with_grants(|grants| authz::held(&groups, grants));
            seen.insert(
                name,
                Seen {
                    resource: Some(resource),
                    held,
                },
            );
        }
        seen
    }
}

/// `?page=`: a whole number from 1 to [`MAX_PAGE`], written in digits; 1
/// without it. `None` for anything else.
fn page_of(query: Option<&str>) -> Option<i64> {
    let Some(given) = query.and_then(|q| {
        form_urlencoded::parse(q.as_bytes())
            .find(|(name, _)| name == "page")
            .map(|(_, value)| value)
    }) else {
        return Some(1);
    };
    if given.is_empty() || given.len() > 5 || !given.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    given
        .parse::<i64>()
        .ok()
        .filter(|n| (1..=MAX_PAGE).contains(n))
}

/// `?changed=`: the notice a change leads to; any other value is ignored.
fn notice_of(query: Option<&str>) -> Option<Notice> {
    let given = form_urlencoded::parse(query?.as_bytes())
        .find(|(name, _)| name == "changed")
        .map(|(_, value)| value)?;
    match given.as_ref() {
        "shared" => Some(Notice::Shared),
        "unshared" => Some(Notice::Unshared),
        "nothing" => Some(Notice::Nothing),
        _ => None,
    }
}

/// Whether the body is declared `application/x-www-form-urlencoded`.
fn is_form(parts: &Parts) -> bool {
    let mut types = parts.headers.get_all(CONTENT_TYPE).iter();
    let (Some(one), None) = (types.next(), types.next()) else {
        return false;
    };
    one.to_str().is_ok_and(|t| {
        t.split(';').next().is_some_and(|m| {
            m.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
    })
}

/// The share and unshare forms: `action`, `kind` and `value` (surrounding
/// white space dropped; not looked at for `everyone`), each at most once;
/// other fields are not looked at. The words say what was wrong.
fn read_form(body: &[u8]) -> Result<ShareForm, String> {
    let mut action = None;
    let mut kind = None;
    let mut value = None;
    for (name, given) in form_urlencoded::parse(body) {
        let slot = match name.as_ref() {
            "action" => &mut action,
            "kind" => &mut kind,
            "value" => &mut value,
            _ => continue,
        };
        if slot.is_some() {
            return Err(format!(
                "The form gives the field \u{201c}{name}\u{201d} twice."
            ));
        }
        *slot = Some(given);
    }
    let share = match action.as_deref() {
        Some("share") => true,
        Some("unshare") => false,
        Some(_) => return Err("The form asks for neither sharing nor unsharing.".to_owned()),
        None => return Err("The form does not say whether to share or to unshare.".to_owned()),
    };
    let Some(kind) = kind else {
        return Err("The form does not say whom to share with: a person by email or user id, a Group, or everyone.".to_owned());
    };
    if kind == "everyone" {
        return Ok(ShareForm {
            share,
            asked: Asked::Entry(Named::Everyone),
        });
    }
    let what = match kind.as_ref() {
        "email" => "email address",
        "user" => "user id",
        "group" => "Group",
        _ => {
            return Err(
                "The form names no kind of entry it takes: a person by email or user id, a Group, or everyone."
                    .to_owned(),
            );
        }
    };
    let Some(value) = value else {
        return Err(format!("The form gives no {what}."));
    };
    let value = value.trim();
    if !entry_text(value) {
        return Err(format!(
            "The {what} must be 1 to {MAX_ENTRY_BYTES} bytes, without control characters."
        ));
    }
    // Owned: the change is handed to the registry; once a change.
    let value = value.to_owned();
    let asked = match kind.as_ref() {
        "email" => Asked::Email(value),
        "user" => Asked::Entry(Named::User(value)),
        _ => Asked::Entry(Named::Group(value)),
    };
    Ok(ShareForm { share, asked })
}
