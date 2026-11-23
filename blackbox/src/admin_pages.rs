//! What the tests of the Dashboard for Platform administrators share
//! (CONTRACT.md, "The Dashboard for Platform administrators"): the
//! administrator's paths under `/admin`, the way in (`a[data-vm-admin]`)
//! and the sub-navigation (`nav[data-vm-admin-sections]`), the refusal page
//! of someone who is not a Platform administrator, the forms of a disable
//! and an enable posted as a browser posts a form of the Dashboard's, the
//! Operation log page read into its entries, fields and links, times
//! written as a browser's `datetime-local` field sends them, and the
//! platform database's tables locked to hold a reading (as
//! `admin_bounds.rs` holds the API's).

use std::thread;
use std::time::{Duration, Instant};

use url::Url;

use crate::dashboard::{Dashboard, Page, host};
use crate::http::Response;
use crate::page::Element;
use crate::sql::Sql;
use crate::vm::Vm;

/// The prefix of every administrator's page.
pub const PREFIX: &str = "/admin";

/// The list of every Application.
pub const APPLICATIONS: &str = "/admin/applications";

/// The Operation log.
pub const OPERATION_LOG: &str = "/admin/operation-log";

/// The Resources and their Grants.
pub const RESOURCES: &str = "/admin/resources";

/// The outbound list.
pub const OUTBOUND: &str = "/admin/outbound";

/// The installation.
pub const INSTALLATION: &str = "/admin/installation";

/// The five sections of `nav[data-vm-admin-sections]`: (`data-vm-section`,
/// the path it links to).
pub const SECTIONS: [(&str, &str); 5] = [
    ("applications", APPLICATIONS),
    ("operation-log", OPERATION_LOG),
    ("resources", RESOURCES),
    ("outbound", OUTBOUND),
    ("installation", INSTALLATION),
];

/// The words of the administrator's link, as a screen reader says it.
pub const ADMIN_LINK_NAME: &str = "Administration";

/// The page that confirms a disable, and where the disable is posted.
pub fn disable_target(application: &str) -> String {
    format!("/admin/applications/{application}/disable")
}

/// Where the enable is posted.
pub fn enable_target(application: &str) -> String {
    format!("/admin/applications/{application}/enable")
}

/// Every `GET` path of the administrator's pages, with this Application's
/// page that confirms a disable: `/admin` first.
pub fn get_paths(application: &str) -> Vec<String> {
    vec![
        PREFIX.to_owned(),
        APPLICATIONS.to_owned(),
        disable_target(application),
        OPERATION_LOG.to_owned(),
        RESOURCES.to_owned(),
        OUTBOUND.to_owned(),
        INSTALLATION.to_owned(),
    ]
}

/// The pages of the administrator's that answer `200` to a Platform
/// administrator, and the section each is (none for the confirmation).
pub fn pages_with_sections(application: &str) -> Vec<(String, Option<&'static str>)> {
    vec![
        (APPLICATIONS.to_owned(), Some("applications")),
        (OPERATION_LOG.to_owned(), Some("operation-log")),
        (RESOURCES.to_owned(), Some("resources")),
        (OUTBOUND.to_owned(), Some("outbound")),
        (INSTALLATION.to_owned(), Some("installation")),
        (disable_target(application), None),
    ]
}

/// Whether an `href` of a page of the Dashboard leads under `/admin`: a
/// path, or an address on the Dashboard's host, whose path is `/admin` or
/// begins with `/admin`.
pub fn leads_under_admin(href: &str, base: &Url) -> bool {
    if href.trim_start().starts_with("/admin") {
        return true;
    }
    base.join(href)
        .is_ok_and(|to| to.host_str() == Some(host().as_str()) && to.path().starts_with("/admin"))
}

/// Fails unless the page's header holds exactly one link
/// `a[data-vm-admin]` to `/admin/applications`, whose name as a screen
/// reader says it is "Administration", with a mark (`<svg>`) in it, and
/// unless no other element of the page has `data-vm-admin`.
pub fn assert_admin_link(page: &Page, context: &str) {
    let root = page.root();
    let all = root.with_attr("data-vm-admin");
    let headers = root.named("header");
    let in_header: Vec<Element<'_>> = headers
        .iter()
        .flat_map(|h| h.with_attr("data-vm-admin"))
        .collect();
    assert!(
        all.len() == 1 && in_header.len() == 1,
        "{context}: expected exactly one element `data-vm-admin` on the page, in its `<header>`; found {} on the page, {} in a `<header>`\n{}",
        all.len(),
        in_header.len(),
        page.describe()
    );
    let link = all.first().copied();
    let Some(link) = link else { return };
    let href = link.attr("href").unwrap_or_default();
    let to = page.url.join(href).ok();
    assert!(
        link.name() == "a"
            && to
                .as_ref()
                .is_some_and(|u| u.host_str() == Some(host().as_str())
                    && u.path() == APPLICATIONS
                    && u.query().is_none()),
        "{context}: expected `a[data-vm-admin]` to link to {APPLICATIONS}, it is <{}> with href {href:?}\n{}",
        link.name(),
        page.describe()
    );
    assert_eq!(
        link.accessible_name(),
        ADMIN_LINK_NAME,
        "{context}: expected the administrator's link's name, as a screen reader says it, to be {ADMIN_LINK_NAME:?}\n{}",
        page.describe()
    );
    assert!(
        !link.named("svg").is_empty(),
        "{context}: expected the administrator's link to hold its mark (a shield, an `<svg>`)\n{}",
        page.describe()
    );
}

/// Fails unless the page holds nothing of the administrator's way in: no
/// element `data-vm-admin` or `data-vm-admin-sections`, no link whose
/// `href` leads under `/admin`, and not the word "Administration" (in any
/// letter case, anywhere in its markup).
pub fn assert_no_admin_trace(page: &Page, context: &str) {
    for attribute in ["data-vm-admin", "data-vm-admin-sections", "data-vm-section"] {
        assert_eq!(
            page.count(attribute),
            0,
            "{context}: expected no element `{attribute}` for someone who is not a Platform administrator\n{}",
            page.describe()
        );
    }
    let under: Vec<String> = page
        .hrefs()
        .into_iter()
        .filter(|h| leads_under_admin(h, &page.url))
        .collect();
    assert!(
        under.is_empty(),
        "{context}: expected no link under /admin for someone who is not a Platform administrator, found {under:?}\n{}",
        page.describe()
    );
    assert!(
        !page
            .response
            .text()
            .to_lowercase()
            .contains("administration"),
        "{context}: expected not the word \"Administration\" on a page of someone who is not a Platform administrator\n{}",
        page.describe()
    );
}

/// Fails unless the page has, under its header, one
/// `nav[data-vm-admin-sections]` with the five links of [`SECTIONS`], each
/// to its path, the one of `current` with `aria-current="page"` and no
/// other with `aria-current`.
pub fn assert_sections(page: &Page, current: &str, context: &str) {
    let root = page.root();
    let navs = root.with_attr("data-vm-admin-sections");
    assert!(
        navs.len() == 1 && navs.first().is_some_and(|n| n.name() == "nav"),
        "{context}: expected one `nav[data-vm-admin-sections]`, found {:?}\n{}",
        navs.iter().map(Element::name).collect::<Vec<_>>(),
        page.describe()
    );
    let Some(nav) = navs.first() else { return };
    let links: Vec<(String, String, Option<String>)> = nav
        .with_attr("data-vm-section")
        .iter()
        .map(|a| {
            let href = a.attr("href").unwrap_or_default();
            let path = page
                .url
                .join(href)
                .map(|u| u.path().to_owned())
                .unwrap_or_default();
            (
                a.attr("data-vm-section").unwrap_or_default().to_owned(),
                format!("{}:{path}", a.name()),
                a.attr("aria-current").map(str::to_owned),
            )
        })
        .collect();
    let wanted: Vec<(String, String, Option<String>)> = SECTIONS
        .iter()
        .map(|(section, path)| {
            (
                (*section).to_owned(),
                format!("a:{path}"),
                (*section == current).then(|| "page".to_owned()),
            )
        })
        .collect();
    let mut sorted = links;
    sorted.sort();
    let mut wanted_sorted = wanted;
    wanted_sorted.sort();
    assert_eq!(
        sorted,
        wanted_sorted,
        "{context}: expected the five links of the sections (section, element:path, aria-current), {current:?} the current one\n{}",
        page.describe()
    );
    assert_eq!(
        root.with_attr("data-vm-section").len(),
        SECTIONS.len(),
        "{context}: expected the sections' links in `nav[data-vm-admin-sections]` only\n{}",
        page.describe()
    );
}

/// Fails unless the page has no `nav[data-vm-admin-sections]`.
pub fn assert_no_sections(page: &Page, context: &str) {
    assert_eq!(
        page.count("data-vm-admin-sections"),
        0,
        "{context}: expected no `nav[data-vm-admin-sections]` on this page\n{}",
        page.describe()
    );
}

/// Fails unless `<title>` is the page's own name, then ` — Dashboard`.
pub fn assert_title(page: &Page, context: &str) {
    let titles: Vec<String> = page
        .root()
        .named("title")
        .iter()
        .map(Element::text)
        .collect();
    assert!(
        matches!(titles.as_slice(), [t] if t.ends_with(" — Dashboard") && t.len() > " — Dashboard".len()),
        "{context}: expected one `<title>`, the page's own name then \" — Dashboard\", got {titles:?}"
    );
}

/// The refusal page of someone who is not a Platform administrator: `403`,
/// a page of the Dashboard with one `data-vm-error="not-an-administrator"`,
/// one `<h1>`, words saying the pages are for the installation's Platform
/// administrators, members of a Group of the company's directory, to ask
/// IT, a link `a[data-vm-back]` to `/`, and nothing of the way in. Returns
/// the page.
pub fn assert_not_an_administrator(response: Response, base: &Url, context: &str) -> Page {
    assert!(
        response.status == 403,
        "{context}: expected the `403` refusal page of someone who is not a Platform administrator, got\n{}",
        response.describe()
    );
    let page = Page::read(response, base.clone(), context);
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(reason, text)] if reason == "not-an-administrator" && !text.is_empty()),
        "{context}: expected one element `data-vm-error=\"not-an-administrator\"` with words, got {errors:?}\n{}",
        page.describe()
    );
    let h1 = page.root().named("h1").len();
    assert_eq!(h1, 1, "{context}: expected one `<h1>`\n{}", page.describe());
    let text = page.body_text();
    let lower = text.to_lowercase();
    for (words, what) in [
        (
            "platform administrator",
            "that these pages are for Platform administrators",
        ),
        (
            "group",
            "that they are the members of a Group of the company's directory",
        ),
    ] {
        assert!(
            lower.contains(words),
            "{context}: expected the refusal to say {what}; it says {text:?}"
        );
    }
    assert!(
        text.contains("IT"),
        "{context}: expected the refusal to say to ask the company's IT; it says {text:?}"
    );
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.host_str() == Some(host().as_str()) && b.path() == "/"),
        "{context}: expected a link `a[data-vm-back]` to `/`, got {back:?}\n{}",
        page.describe()
    );
    assert_eq!(
        page.count("data-vm-admin"),
        0,
        "{context}: expected the refusal's header without `a[data-vm-admin]`\n{}",
        page.describe()
    );
    page
}

/// A form of the Dashboard posted to `target` with `body`, as a browser
/// posts a form of the Dashboard's (see [`Dashboard::post_form`]).
pub fn post(dashboard: &Dashboard<'_>, target: &str, body: &str) -> Response {
    dashboard.post_form(target, body)
}

/// The disable form of the page that confirms it, submitted as a person
/// submits it: the page read (`200`), its one `form[data-vm-form="disable"]`
/// posted from that page. The answer.
pub fn confirm_disable(dashboard: &Dashboard<'_>, application: &str) -> Response {
    let target = disable_target(application);
    let page = dashboard.page(
        &target,
        200,
        &format!("the page that confirms disabling {application:?}"),
    );
    let forms = page.forms_of("disable");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"disable\"]` on the page that confirms it, found {}\n{}",
            forms.len(),
            page.describe()
        );
    };
    let action = page.action_of(form);
    dashboard
        .form_request_from(
            &page.response,
            &crate::browser::target_of(&action),
            &form.body(&[]),
        )
        .send()
}

/// The enable form of the Application's page, submitted as a person
/// submits it. The answer.
pub fn press_enable(dashboard: &Dashboard<'_>, application: &str) -> Response {
    let page = dashboard.details(application);
    let forms = page.forms_of("enable");
    let [form] = forms.as_slice() else {
        panic!(
            "expected one `form[data-vm-form=\"enable\"]` on the Application's page, found {}\n{}",
            forms.len(),
            page.describe()
        );
    };
    let action = page.action_of(form);
    dashboard
        .form_request_from(
            &page.response,
            &crate::browser::target_of(&action),
            &form.body(&[]),
        )
        .send()
}

/// An entry of the Operation log as its page shows it.
#[derive(Clone, Debug)]
pub struct Shown {
    pub id: String,
    pub outcome: Option<String>,
    pub time: Option<String>,
    /// The whole element's text.
    pub text: String,
    /// The element's text as a screen reader says it.
    pub spoken: String,
    /// The text of what is in its `<details>`, if it has one.
    pub details: Option<String>,
    /// The text before its `<details>`: what is seen first.
    pub first: String,
    /// `data-vm-reason`'s text, `data-vm-error-code`'s text.
    pub reason: Option<String>,
    pub error_code: Option<String>,
    /// The links `a[data-vm-filter]`: (kind, `href` resolved).
    pub filters: Vec<(String, Url)>,
    /// Whether the element has `data-vm-user`.
    pub has_user_mark: bool,
    /// The names of the `<svg>`s (`aria-label`) inside it.
    pub marks: Vec<Option<String>>,
}

/// Every entry of an Operation log page, in the page's order.
pub fn entries(page: &Page) -> Vec<Shown> {
    page.root()
        .with_attr("data-vm-entry")
        .iter()
        .map(|e| {
            let details = e.named("details");
            let details_text = details.first().map(Element::text);
            let text = e.text();
            let first = match &details_text {
                Some(d) if !d.is_empty() => text.replacen(d.as_str(), "", 1),
                _ => text.clone(),
            };
            Shown {
                id: e.attr("data-vm-entry").unwrap_or_default().to_owned(),
                outcome: e.attr("data-vm-outcome").map(str::to_owned),
                time: e.attr("data-vm-time").map(str::to_owned),
                text,
                spoken: e.spoken(),
                details: details_text,
                first,
                reason: e.with_attr("data-vm-reason").first().map(Element::text),
                error_code: e.with_attr("data-vm-error-code").first().map(Element::text),
                filters: e
                    .with_attr("data-vm-filter")
                    .iter()
                    .filter(|a| a.name() == "a")
                    .filter_map(|a| {
                        Some((
                            a.attr("data-vm-filter")?.to_owned(),
                            page.url.join(a.attr("href")?).ok()?,
                        ))
                    })
                    .collect(),
                has_user_mark: e.has("data-vm-user") || !e.with_attr("data-vm-user").is_empty(),
                marks: e
                    .named("svg")
                    .iter()
                    .map(|s| s.attr("aria-label").map(str::to_owned))
                    .collect(),
            }
        })
        .collect()
}

/// The ids of [`entries`], in the page's order.
pub fn entry_ids(page: &Page) -> Vec<String> {
    entries(page).into_iter().map(|e| e.id).collect()
}

/// The value a field of the filter form holds when the page is shown: the
/// `value` of the input named `name`, or the `value` of the `selected`
/// option of the `select` named `name` (`None` when none is selected).
pub fn field_value(page: &Page, name: &str) -> Option<String> {
    let root = page.root();
    let forms = root.with_attr_value("data-vm-form", "operation-log");
    let form = forms.first()?;
    if let Some(input) = form
        .named("input")
        .into_iter()
        .find(|i| i.attr("name") == Some(name))
    {
        return Some(input.attr("value").unwrap_or_default().to_owned());
    }
    let select = form
        .named("select")
        .into_iter()
        .find(|s| s.attr("name") == Some(name))?;
    let options = select.named("option");
    options
        .iter()
        .find(|o| o.has("selected"))
        .map(|o| o.attr("value").map_or_else(|| o.text(), str::to_owned))
}

/// The elements `data-vm-field-error`: (field, text).
pub fn field_errors(page: &Page) -> Vec<(String, String)> {
    page.root()
        .with_attr("data-vm-field-error")
        .iter()
        .map(|e| {
            (
                e.attr("data-vm-field-error").unwrap_or_default().to_owned(),
                e.text(),
            )
        })
        .collect()
}

/// The query of an address as (name, value) pairs, decoded.
pub fn query_pairs(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(n, v)| (n.into_owned(), v.into_owned()))
        .collect()
}

/// An entry's `time` (RFC 3339 in UTC with microseconds, `...Z`) as a
/// browser's `datetime-local` field with `step="1"` sends it: the same
/// without the `Z`.
pub fn as_datetime_local(time: &str) -> String {
    time.trim_end_matches(['Z', 'z']).to_owned()
}

/// A time to the second (`YYYY-MM-DDTHH:MM:SS`), cut from an entry's time.
pub fn to_the_second(time: &str) -> String {
    time.chars().take(19).collect()
}

/// A time to the minute (`YYYY-MM-DDTHH:MM`), cut from an entry's time.
pub fn to_the_minute(time: &str) -> String {
    time.chars().take(16).collect()
}

/// An address of the Operation log page with these filters, each encoded
/// as a form encodes it.
pub fn log_target(filters: &[(&str, &str)]) -> String {
    if filters.is_empty() {
        return OPERATION_LOG.to_owned();
    }
    format!("{OPERATION_LOG}?{}", crate::dashboard::form_body(filters))
}

/// The tables of the platform database, quoted.
fn tables(sql: &Sql) -> Vec<String> {
    sql.batch(
        "SELECT quote_ident(schemaname) || '.' || quote_ident(tablename) FROM pg_tables \
         WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1",
    )
    .into_iter()
    .filter_map(|row| row.into_iter().next().flatten())
    .collect()
}

/// Locks every table of the program's platform database in a transaction
/// of a connection of the harness's own, which stays open until
/// [`let_go`]: a reading of the platform database waits on it, while the
/// program serves Applications from what it holds in memory.
pub fn lock_everything(vm: &Vm) -> Sql {
    let sql = crate::admin::platform_sql(vm);
    let all = tables(&sql);
    assert!(
        !all.is_empty(),
        "the harness's own check: the platform database has tables"
    );
    sql.batch(&format!(
        "BEGIN; SET LOCAL lock_timeout = '10s'; LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        all.join(", ")
    ));
    sql
}

/// Ends the transaction of [`lock_everything`].
pub fn let_go(sql: &Sql) {
    sql.batch("ROLLBACK");
}

/// How many sessions of the program's platform database wait on a lock.
pub fn waiting_on_locks(vm: &Vm) -> usize {
    crate::admin::platform_sql(vm)
        .value(&format!(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = '{}' AND wait_event_type = 'Lock'",
            vm.platform().name()
        ))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Waits until at least `at_least` sessions wait on a lock, at most 10
/// seconds; the harness's own check fails otherwise.
pub fn wait_for_lock_waiters(vm: &Vm, at_least: usize, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let now = waiting_on_locks(vm);
        if now >= at_least {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the harness's own check, {context}: expected {at_least} readings waiting on the lock within 10 s, {now} are"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// The Resource [`prepare_site`] registers over a real shared database.
pub const SITE_RESOURCE: &str = "lookdb";
/// The Resource [`prepare_site`] registers over a database nobody serves.
pub const SITE_GONE_RESOURCE: &str = "gonedb";
/// The Application [`prepare_site`] makes and disables.
pub const SITE_DISABLED: &str = "frozen";
/// The Application [`prepare_site`] makes without a Version.
pub const SITE_DRAFT: &str = "draft";
/// The Group [`prepare_site`] grants `read` on both Resources.
pub const SITE_GROUP: &str = "finance";

/// What [`prepare_site`] made; the shared database lives as long as it.
pub struct PreparedSite {
    pub db: crate::shared_database::SharedDatabase,
    /// The administrator's tool credential.
    pub credential: String,
}

/// Prepares an installation of the walks (OIDC, over HTTPS) for the pages
/// of Platform administrators, through the platform's API with `admin`'s
/// tool credential (`admin` must be in the administrators' Group at the
/// provider, and in [`SITE_GROUP`]): [`SITE_DISABLED`] made with a
/// Version and disabled, [`SITE_DRAFT`] made with none, [`SITE_RESOURCE`]
/// registered over a shared database of the test's own and
/// [`SITE_GONE_RESOURCE`] over an address nothing serves, both granted
/// `read` to [`SITE_GROUP`]; and Operation log entries of three outcomes,
/// each a pull of a Resource's structure: allowed (`admin`'s, on
/// [`SITE_RESOURCE`]), refused (`other`'s, who holds no Grant) and failed
/// (`admin`'s, on [`SITE_GONE_RESOURCE`]). The harness's own check fails
/// unless the Operation log then has an entry of each.
pub fn prepare_site(site: &crate::walks::Site, admin: &str, other: &str) -> PreparedSite {
    use crate::tool::bearer;
    use serde_json::json;
    let vm = &site.at.vm;
    let credential = site.application_of(admin, SITE_DISABLED, Some("Frozen while IT looks at it"));
    let post = |target: &str, body: &serde_json::Value| {
        bearer(vm, target, &credential)
            .method("POST")
            .json(body)
            .send()
    };
    post(
        &format!("/v1/applications/{SITE_DISABLED}/disable"),
        &json!({}),
    )
    .expect_status(200);
    post("/v1/applications", &json!({ "name": SITE_DRAFT })).expect_status(201);
    let db = crate::shared_database::SharedDatabase::create();
    let mut body = crate::scenario::resource_body(SITE_RESOURCE, &db, &db.reader);
    post("/v1/resources", &body).expect_status(201);
    body["name"] = json!(SITE_GONE_RESOURCE);
    let port =
        crate::postgres::free_port().unwrap_or_else(|e| panic!("cannot find a free port: {e}"));
    body["connection"]["host"] = json!("127.0.0.1");
    body["connection"]["port"] = json!(port);
    post("/v1/resources", &body).expect_status(201);
    for resource in [SITE_RESOURCE, SITE_GONE_RESOURCE] {
        post(
            &format!("/v1/resources/{resource}/grants"),
            &json!({ "group": SITE_GROUP, "access": "read" }),
        )
        .expect_status(200);
    }
    let other_credential = site.credential_for(other);
    for (who, resource) in [
        (credential.as_str(), SITE_RESOURCE),
        (other_credential.as_str(), SITE_RESOURCE),
        (credential.as_str(), SITE_GONE_RESOURCE),
    ] {
        drop(bearer(vm, &format!("/v1/resources/{resource}/structure"), who).send());
    }
    let log = bearer(vm, "/v1/operation-log?limit=1000", &credential).send();
    log.expect_status(200);
    let log = log.json();
    let outcomes: Vec<&str> = log["entries"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| e["outcome"].as_str()).collect())
        .unwrap_or_default();
    for outcome in ["allowed", "refused", "failed"] {
        assert!(
            outcomes.contains(&outcome),
            "the harness's own check: expected an Operation log entry `{outcome}` from the pulls, got {outcomes:?}"
        );
    }
    PreparedSite { db, credential }
}

/// The tables of the platform database the two readings of
/// [`while_two_readings_run`] are held on: the usage that every Application
/// reads and the Operation log. Their names are what the platform database
/// shows (`pg_tables`).
pub const READINGS_TABLES: [&str; 2] = ["public.application_usage", "public.operation_log"];

/// While two readings for Platform administrators run (`GET
/// /v1/admin/applications` and `GET /v1/operation-log` as
/// [`crate::admin::administrator`], each held by [`READINGS_TABLES`]
/// locked, and only those: what does not read them is answered as ever
/// meanwhile), runs `during`; then lets them end. Returns what `during`
/// returned.
pub fn while_two_readings_run<T>(vm: &Vm, during: impl FnOnce() -> T) -> T {
    let sql = crate::admin::platform_sql(vm);
    let all = tables(&sql);
    for table in READINGS_TABLES {
        assert!(
            all.iter().any(|t| t == table),
            "the harness's own check: the platform database has {table}, it has {all:?}"
        );
    }
    sql.batch(&format!(
        "BEGIN; SET LOCAL lock_timeout = '10s'; LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        READINGS_TABLES.join(", ")
    ));
    let read = |target: &str| {
        vm.api_as(&crate::admin::administrator())
            .request(target)
            .timeout(Duration::from_secs(40))
            .send()
    };
    thread::scope(|scope| {
        let first = scope.spawn(|| read(crate::admin::LIST));
        let second = scope.spawn(|| read(crate::admin::OPERATION_LOG));
        wait_for_lock_waiters(vm, 2, "two readings");
        let result = during();
        let_go(&sql);
        drop(first.join());
        drop(second.join());
        result
    })
}
