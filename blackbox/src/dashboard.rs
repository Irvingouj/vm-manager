//! What the tests of the Dashboard share (CONTRACT.md, "The Dashboard"):
//! its address, requests to it as a User of the stand-in sign-in or from a
//! browser signed in through OIDC, its pages read with the harness's own
//! reader ([`crate::page`]) into what the contract's `data-vm-...`
//! attributes say, its forms filled as a person fills them, and the checks
//! every page of it must pass.
//!
//! A page is read only when it is one of the Dashboard's: `text/html` with
//! exactly one element `data-vm-user`; anything else fails the test with
//! the answer shown, so a test never reads a list out of some other page.

use serde_json::{Value, json};
use url::Url;

use crate::browser::{Browser, NAVIGATION_ACCEPT, Step, describe_steps};
use crate::http::{Request, Response, User};
use crate::page::{Document, Element};
use crate::sessions::Installation;
use crate::sharing::assert_own_page_headers;
use crate::vm::{DOMAIN, Vm, VmOptions};

/// The name in the Dashboard's address: `dashboard.<domain>`.
pub const NAME: &str = "dashboard";

/// How many Applications a page of the list holds.
pub const PAGE_SIZE: usize = 100;

/// The largest body of a change of sharing.
pub const MAX_FORM_BODY: usize = 4096;

/// The most pages a test follows `data-vm-next` through.
const MAX_PAGES_FOLLOWED: usize = 200;

/// The Group named with `--administrators-group` in these tests.
pub const ADMINISTRATORS: &str = crate::scenario::ADMINISTRATORS;

/// `dashboard.<domain>`.
pub fn host() -> String {
    format!("{NAME}.{DOMAIN}")
}

/// The options of a program for the tests of the Dashboard with the
/// stand-in: the stand-in on, [`ADMINISTRATORS`], and `--public-scheme
/// http`, so that the Dashboard's address is
/// `http://dashboard.<domain>:<the listening port>`, as with OIDC in the
/// harness.
pub fn stand_in_options() -> VmOptions {
    VmOptions::new()
        .stand_in_sign_in()
        .administrators_group(ADMINISTRATORS)
        .args_extra(&["--public-scheme", "http"])
}

/// A program started with [`stand_in_options`].
pub fn start() -> Vm {
    Vm::start(&stand_in_options())
}

/// A Platform administrator of [`stand_in_options`].
pub fn administrator() -> User {
    crate::scenario::administrator()
}

/// The Dashboard for one User of the stand-in sign-in, on a program
/// started with `--public-scheme http` and no `--public-port`.
pub struct Dashboard<'a> {
    pub vm: &'a Vm,
    pub user: User,
}

impl<'a> Dashboard<'a> {
    pub fn new(vm: &'a Vm, user: &User) -> Self {
        Self {
            vm,
            user: user.clone(),
        }
    }

    /// `http://dashboard.<domain>:<port>`.
    pub fn origin(&self) -> String {
        format!("http://{}:{}", host(), self.vm.address().port())
    }

    /// The Dashboard's address with this path and query.
    pub fn url(&self, target: &str) -> Url {
        crate::browser::url(&format!("{}{target}", self.origin()))
    }

    /// An Application's address as the Dashboard links it here:
    /// `http://<name>.<domain>:<port>/`.
    pub fn open_href(&self, application: &str) -> String {
        format!(
            "http://{application}.{DOMAIN}:{}/",
            self.vm.address().port()
        )
    }

    /// A request to the Dashboard as this User and nothing else: no
    /// `Accept`, no `Origin`, no `Sec-Fetch-*`.
    pub fn bare(&self, method: &str, target: &str) -> Request {
        Request::new(
            self.vm.address(),
            target,
            &format!("{}:{}", host(), self.vm.address().port()),
        )
        .method(method)
        .as_user(&self.user)
    }

    /// The request of a navigation to the Dashboard, typed by the person:
    /// what a browser sends (see [`crate::browser`]).
    pub fn navigation(&self, method: &str, target: &str) -> Request {
        self.bare(method, target)
            .header("accept", NAVIGATION_ACCEPT)
            .header("sec-fetch-site", "none")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
    }

    /// A navigation `GET` of `target`.
    pub fn get(&self, target: &str) -> Response {
        self.navigation("GET", target).send()
    }

    /// A navigation `GET` of `target`, which must be answered `status`
    /// with a page of the Dashboard's; the page, read.
    pub fn page(&self, target: &str, status: u16, context: &str) -> Page {
        let response = self.get(target);
        if response.status != status {
            self.vm.fail(&format!(
                "{context}: expected `{status}` and a page of the Dashboard for {:?} at {target}, got\n{}",
                self.user.id,
                response.describe()
            ));
        }
        Page::read(response, self.url(target), context)
    }

    /// The list's first page (`GET /`), which must be `200`.
    pub fn list(&self) -> Page {
        self.page("/", 200, &format!("the Dashboard of {:?}", self.user.id))
    }

    /// Every Application of the list, page after page, following
    /// `a[data-vm-next]` from the first.
    pub fn every_listed(&self) -> Vec<Listed> {
        let mut page = self.list();
        let mut all = page.applications();
        for _ in 0..MAX_PAGES_FOLLOWED {
            let Some(next) = page.link("data-vm-next") else {
                return all;
            };
            let response = self.get(&crate::browser::target_of(&next));
            if response.status != 200 {
                self.vm.fail(&format!(
                    "expected `200` for the next page of the list, {next}, got\n{}",
                    response.describe()
                ));
            }
            page = Page::read(response, next, "the next page of the list");
            all.extend(page.applications());
        }
        panic!("more than {MAX_PAGES_FOLLOWED} pages of the list")
    }

    /// One Application's page (`GET /applications/<name>`), which must be
    /// `200`.
    pub fn details(&self, application: &str) -> Page {
        self.page(
            &format!("/applications/{application}"),
            200,
            &format!("the page of {application:?} for {:?}", self.user.id),
        )
    }

    /// A form posted from the Dashboard's own list to `target`, as a
    /// browser submits it (see [`crate::browser::submitted`]): `Origin`
    /// what the list's referrer policy makes it, `Sec-Fetch-Site:
    /// same-origin`, and this `application/x-www-form-urlencoded` body.
    pub fn post_form(&self, target: &str, body: &str) -> Response {
        self.form_request(target, body).send()
    }

    /// The request of [`Dashboard::post_form`], to add to. It opens the
    /// Dashboard's list first, to know the `Origin` its referrer policy
    /// gives a form on it.
    pub fn form_request(&self, target: &str, body: &str) -> Request {
        let list = self.navigation("GET", "/").send();
        self.form_request_from(&list, target, body)
    }

    /// The request a browser makes for a form of this page of the
    /// Dashboard, posted to `target` with `body`.
    pub fn form_request_from(&self, page: &Response, target: &str, body: &str) -> Request {
        let origin = crate::browser::form_origin(&self.origin(), page);
        crate::browser::submitted(
            self.bare("POST", target)
                .header("accept", NAVIGATION_ACCEPT),
            &origin,
            "same-origin",
            body,
        )
    }

    /// Shares the Application through the share form of its page, as a
    /// person fills it: `kind` chosen, `value` typed. The answer.
    pub fn share(&self, application: &str, kind: &str, value: &str) -> Response {
        self.share_request(application, kind, value).send()
    }

    /// The request of [`Dashboard::share`], to add to.
    pub fn share_request(&self, application: &str, kind: &str, value: &str) -> Request {
        let page = self.details(application);
        let (action, body) = page.share_submission(kind, value);
        self.form_request_from(&page.response, &crate::browser::target_of(&action), &body)
    }

    /// Unshares the entry `kind` / `value` (a person by `user`) through
    /// the unshare form beside it on the Application's page. The answer.
    pub fn unshare(&self, application: &str, kind: &str, value: &str) -> Response {
        let page = self.details(application);
        let (action, body) = page.unshare_submission(kind, value);
        self.form_request_from(&page.response, &crate::browser::target_of(&action), &body)
            .send()
    }

    /// `/applications/<name>/sharing`.
    pub fn sharing_target(application: &str) -> String {
        format!("/applications/{application}/sharing")
    }
}

/// A body of a change of sharing with these fields, in order, encoded as
/// a form encodes them.
pub fn form_body(fields: &[(&str, &str)]) -> String {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        body.append_pair(name, value);
    }
    body.finish()
}

/// The Dashboard's address in an installation of the harness signed in
/// through OIDC, with this path and query.
pub fn oidc_url(at: &Installation, target: &str) -> Url {
    at.app_url(NAME, target)
}

/// Opens the Dashboard's `target` in this browser, which must have a
/// session there: the answer must be `status` on that very address (no
/// redirect). The page, read.
pub fn oidc_page(
    at: &Installation,
    browser: &mut Browser,
    target: &str,
    status: u16,
    context: &str,
) -> Page {
    let address = oidc_url(at, target);
    let step = browser.open(&address);
    if step.response.status != status {
        at.vm.fail(&format!(
            "{context}: expected `{status}` and a page of the Dashboard at {address}, got\n{}",
            describe_steps(std::slice::from_ref(&step))
        ));
    }
    Page::read(step.response, address, context)
}

/// Signs this browser in to the Dashboard through the provider (or
/// without it, with a platform session), following every redirect; fails
/// unless the way ends with `200` on the Dashboard's `/`. Returns the
/// steps and the page.
pub fn oidc_sign_in(at: &Installation, browser: &mut Browser) -> (Vec<Step>, Page) {
    let address = oidc_url(at, "/");
    let steps = browser.navigate(&address);
    let last = steps.last().unwrap_or_else(|| panic!("no steps"));
    if last.response.status != 200 || last.url != address {
        at.vm.fail(&format!(
            "expected the browser to land signed in on the Dashboard ({address}) with 200, but the way went:\n{}",
            describe_steps(&steps)
        ));
    }
    let page = Page::read(
        last.response.clone(),
        address,
        "the Dashboard after signing in",
    );
    (steps, page)
}

/// Posts the form of `page` (its address `page.url`) to `action` with this
/// body from the browser, as the browser posts a form of that page.
pub fn oidc_post(browser: &mut Browser, page: &Page, action: &Url, body: &str) -> Response {
    let request = browser.submit_form(&page.url, &page.response, action, body);
    browser.send(action, &request)
}

/// The fields `data-vm-...` say of one element of the list (or of an
/// Application's own page). What only an Application's own page holds (its
/// Contributors, its Resources) is read from the whole page:
/// [`Page::contributors`], [`Page::resources`].
#[derive(Clone, Debug)]
pub struct Listed {
    pub name: String,
    pub relation: Option<String>,
    pub serving: Option<String>,
    /// `data-vm-attention`: the worst standing among the Resources its
    /// serving Version declares, as one mark.
    pub attention: Option<String>,
    /// The text of the element `data-vm-description`, if there is one;
    /// `Some` of each when there are more (a test says that is wrong).
    pub descriptions: Vec<String>,
    /// The elements `data-vm-open`: (element name, `href`).
    pub open: Vec<(String, Option<String>)>,
    /// The name of each element `data-vm-open` as a screen reader says it.
    pub open_names: Vec<String>,
    /// The elements `data-vm-details`: (element name, `href`).
    pub details: Vec<(String, Option<String>)>,
    /// The text of each element `data-vm-details`.
    pub details_texts: Vec<String>,
    /// The whole element's text.
    pub text: String,
    /// The whole element's text as a screen reader says it
    /// ([`Element::spoken`]).
    pub spoken: String,
}

/// A person, a Group or everyone as a page shows them: the kind
/// (`data-vm-contributor` or `data-vm-shared`), `data-vm-id`, and the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub kind: String,
    pub id: Option<String>,
    pub text: String,
    /// Whether the element has `data-vm-unseen`: a person the platform
    /// knows neither a name nor an email of.
    pub unseen: bool,
}

impl Shown {
    /// `user:<id>`, `group:<Group>` or `everyone`, as
    /// [`crate::sharing::entry_key`] names entries.
    pub fn key(&self) -> String {
        match (self.kind.as_str(), &self.id) {
            ("everyone", _) => "everyone".to_owned(),
            (kind, Some(id)) => format!("{kind}:{id}"),
            (kind, None) => format!("{kind}:<no data-vm-id>"),
        }
    }
}

/// A declared Resource as an Application's own page shows it.
#[derive(Clone, Debug)]
pub struct Declared {
    pub name: String,
    pub declared: Option<String>,
    pub yours: Option<String>,
    pub standing: Option<String>,
    pub ask: Option<String>,
    pub text: String,
    /// The element's text as a screen reader says it ([`Element::spoken`]):
    /// a mark's name included, what is hidden from it left out.
    pub spoken: String,
    /// Each `<svg>` inside it: (`aria-hidden`, `role`, `aria-label`).
    pub marks: Vec<(Option<String>, Option<String>, Option<String>)>,
}

impl Declared {
    fn read(e: &Element<'_>) -> Self {
        Self {
            name: e.attr("data-vm-resource").unwrap_or_default().to_owned(),
            declared: e.attr("data-vm-declared").map(str::to_owned),
            yours: e.attr("data-vm-yours").map(str::to_owned),
            standing: e.attr("data-vm-standing").map(str::to_owned),
            ask: e.attr("data-vm-ask").map(str::to_owned),
            text: e.text(),
            spoken: e.spoken(),
            marks: e
                .named("svg")
                .iter()
                .map(|svg| {
                    (
                        svg.attr("aria-hidden").map(str::to_owned),
                        svg.attr("role").map(str::to_owned),
                        svg.attr("aria-label").map(str::to_owned),
                    )
                })
                .collect(),
        }
    }
}

fn shown_contributor(e: &Element<'_>) -> Shown {
    Shown {
        kind: e.attr("data-vm-contributor").unwrap_or_default().to_owned(),
        id: e.attr("data-vm-id").map(str::to_owned),
        text: e.text(),
        unseen: e.has("data-vm-unseen"),
    }
}

impl Listed {
    fn read(element: &Element<'_>) -> Self {
        let link = |e: &Element<'_>| (e.name().to_owned(), e.attr("href").map(str::to_owned));
        Self {
            name: element
                .attr("data-vm-application")
                .unwrap_or_default()
                .to_owned(),
            relation: element.attr("data-vm-relation").map(str::to_owned),
            serving: element.attr("data-vm-serving").map(str::to_owned),
            attention: element.attr("data-vm-attention").map(str::to_owned),
            descriptions: element
                .with_attr("data-vm-description")
                .iter()
                .map(Element::text)
                .collect(),
            open: element.with_attr("data-vm-open").iter().map(link).collect(),
            open_names: element
                .with_attr("data-vm-open")
                .iter()
                .map(Element::accessible_name)
                .collect(),
            details: element
                .with_attr("data-vm-details")
                .iter()
                .map(link)
                .collect(),
            details_texts: element
                .with_attr("data-vm-details")
                .iter()
                .map(Element::text)
                .collect(),
            text: element.text(),
            spoken: element.spoken(),
        }
    }

    /// The description shown, failing if there is more than one.
    pub fn description(&self) -> Option<&str> {
        match self.descriptions.as_slice() {
            [] => None,
            [one] => Some(one),
            many => panic!(
                "expected at most one element `data-vm-description` in {:?}, got {many:?}",
                self.name
            ),
        }
    }

    /// The `href` of the one `a[data-vm-open]`, `None` when there is no
    /// element `data-vm-open`; fails when there are more, or it is not a
    /// link.
    pub fn open_href(&self) -> Option<String> {
        one_link(&self.open, "data-vm-open", &self.name)
    }

    /// The `href` of the one `a[data-vm-details]`, as [`Listed::open_href`].
    pub fn details_href(&self) -> Option<String> {
        one_link(&self.details, "data-vm-details", &self.name)
    }
}

/// The few words of the list's mark for each `data-vm-attention` but
/// `none` (CONTRACT.md, "The Dashboard: The list").
pub const ATTENTION_WORDS: [(&str, &str); 3] = [
    ("missing-access", "Missing access"),
    ("not-registered", "Not set up"),
    ("unknown", "Access unknown"),
];

/// Fails unless the Application's element (on its own page, where the
/// element holds the whole of it and the words of the list's mark are not
/// promised) has `data-vm-attention` of `wanted`.
pub fn assert_attention_attribute(listed: &Listed, wanted: &str, context: &str) {
    assert_eq!(
        listed.attention.as_deref(),
        Some(wanted),
        "{context}: expected `data-vm-attention={wanted:?}` on {:?}, got {:?}",
        listed.name,
        listed.attention
    );
}

/// Fails unless the Application's element of the list has
/// `data-vm-attention` of `wanted` and holds the words of that mark (as a screen reader says the
/// element, in any letter case) and no other mark's; for `none`, no mark's
/// words at all.
pub fn assert_attention(listed: &Listed, wanted: &str, context: &str) {
    assert_attention_attribute(listed, wanted, context);
    let spoken = listed.spoken.to_lowercase();
    for (attention, words) in ATTENTION_WORDS {
        let held = spoken.contains(&words.to_lowercase());
        assert!(
            held == (attention == wanted),
            "{context}: expected {:?} to hold {} the words {words:?} (the mark of `{attention}`) with `data-vm-attention={wanted:?}`; it says {:?}",
            listed.name,
            if attention == wanted { "" } else { "not" },
            listed.spoken
        );
    }
}

/// Fails unless the page of the list holds what finds and opens an
/// Application and none of what is on an Application's own page: no
/// element `data-vm-contributor`, `data-vm-more-contributors`,
/// `data-vm-resource` or `data-vm-shared` (CONTRACT.md, "How the pages
/// look: Said once").
pub fn assert_list_holds_nothing_of_an_applications_own_page(page: &Page, context: &str) {
    for attribute in [
        "data-vm-contributor",
        "data-vm-more-contributors",
        "data-vm-resource",
        "data-vm-shared",
    ] {
        assert_eq!(
            page.count(attribute),
            0,
            "{context}: expected no element `{attribute}` on the Dashboard's list, it is on the Application's own page\n{}",
            page.describe()
        );
    }
}

fn one_link(
    links: &[(String, Option<String>)],
    attribute: &str,
    application: &str,
) -> Option<String> {
    match links {
        [] => None,
        [(tag, Some(href))] if tag == "a" => Some(href.clone()),
        other => panic!(
            "expected at most one link `a[{attribute}]` with an `href` in {application:?}, got {other:?}"
        ),
    }
}

/// One form of a page, as a browser would submit it.
#[derive(Clone, Debug)]
pub struct Form {
    /// `data-vm-form`.
    pub kind: Option<String>,
    /// `method`, lower case.
    pub method: Option<String>,
    pub action: Option<String>,
    /// `<input>`s: (name, type in lower case, value).
    pub inputs: Vec<(Option<String>, String, String)>,
    /// `<select>`s: (name, the values of their options).
    pub selects: Vec<(Option<String>, Vec<String>)>,
    /// `<button>`s that submit: (name, value).
    pub buttons: Vec<(Option<String>, String)>,
}

impl Form {
    fn read(element: &Element<'_>) -> Self {
        let inputs = element
            .named("input")
            .iter()
            .map(|e| {
                (
                    e.attr("name").map(str::to_owned),
                    e.attr("type").unwrap_or("text").to_ascii_lowercase(),
                    e.attr("value").unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let selects = element
            .named("select")
            .iter()
            .map(|e| {
                (
                    e.attr("name").map(str::to_owned),
                    e.named("option")
                        .iter()
                        .map(|o| o.attr("value").map_or_else(|| o.text(), str::to_owned))
                        .collect(),
                )
            })
            .collect();
        let buttons = element
            .named("button")
            .iter()
            .filter(|e| {
                e.attr("type")
                    .is_none_or(|t| t.eq_ignore_ascii_case("submit"))
            })
            .map(|e| {
                (
                    e.attr("name").map(str::to_owned),
                    e.attr("value").unwrap_or_default().to_owned(),
                )
            })
            .collect();
        Self {
            kind: element.attr("data-vm-form").map(str::to_owned),
            method: element.attr("method").map(str::to_ascii_lowercase),
            action: element.attr("action").map(str::to_owned),
            inputs,
            selects,
            buttons,
        }
    }

    /// The value of the hidden input of this name, if there is one.
    pub fn hidden(&self, name: &str) -> Option<&str> {
        self.inputs
            .iter()
            .find(|(n, t, _)| n.as_deref() == Some(name) && t == "hidden")
            .map(|(_, _, v)| v.as_str())
    }

    /// The values a person can choose for `name`: the options of a
    /// `select`, or the values of the radio buttons, of that name.
    pub fn choices(&self, name: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .selects
            .iter()
            .filter(|(n, _)| n.as_deref() == Some(name))
            .flat_map(|(_, options)| options.iter().cloned())
            .collect();
        found.extend(
            self.inputs
                .iter()
                .filter(|(n, t, _)| n.as_deref() == Some(name) && t == "radio")
                .map(|(_, _, v)| v.clone()),
        );
        found
    }

    /// Whether the form has an input a person types into named `name`.
    pub fn has_text_input(&self, name: &str) -> bool {
        self.inputs.iter().any(|(n, t, _)| {
            n.as_deref() == Some(name) && matches!(t.as_str(), "text" | "email" | "search")
        })
    }

    /// The body a browser sends for this form with `typed` filled in or
    /// chosen: every hidden input that is not among `typed`, then `typed`,
    /// then the first submit button's name and value when it has a name.
    pub fn body(&self, typed: &[(&str, &str)]) -> String {
        let mut fields: Vec<(String, String)> = self
            .inputs
            .iter()
            .filter(|(_, t, _)| t == "hidden")
            .filter_map(|(n, _, v)| n.clone().map(|n| (n, v.clone())))
            .filter(|(n, _)| !typed.iter().any(|(t, _)| t == n))
            .collect();
        fields.extend(
            typed
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned())),
        );
        if let Some((Some(name), value)) = self.buttons.first() {
            fields.push((name.clone(), value.clone()));
        }
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &fields {
            body.append_pair(name, value);
        }
        body.finish()
    }
}

/// A page of the Dashboard, read.
#[derive(Clone, Debug)]
pub struct Page {
    pub response: Response,
    /// The address it was asked at.
    pub url: Url,
    document: Document,
}

impl Page {
    /// Reads an answer as a page of the Dashboard, failing unless it is
    /// one: `text/html` with exactly one element `data-vm-user`. Also fails
    /// unless it passes [`assert_page_rules`] (the headers, no script, and
    /// the viewport declared).
    pub fn read(response: Response, url: Url, context: &str) -> Self {
        let html_type = response
            .header("content-type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/html"));
        let document = Document::parse(&response.text());
        let users = document.root().with_attr("data-vm-user").len();
        assert!(
            html_type && users == 1,
            "{context}: expected a page of the Dashboard (text/html with one element `data-vm-user`) at {url}, got {} element(s) `data-vm-user` in\n{}",
            users,
            response.describe()
        );
        assert_page_rules(&response, context);
        Self {
            response,
            url,
            document,
        }
    }

    pub fn root(&self) -> Element<'_> {
        self.document.root()
    }

    pub fn status(&self) -> u16 {
        self.response.status
    }

    /// The text of the whole page.
    pub fn text(&self) -> String {
        self.root().text()
    }

    /// The page as text, for failure messages.
    pub fn describe(&self) -> String {
        self.response.describe()
    }

    /// The element `data-vm-user`: (its value, its text).
    pub fn user(&self) -> (String, String) {
        let users = self.root().with_attr("data-vm-user");
        let user = users
            .first()
            .unwrap_or_else(|| panic!("a page read has one `data-vm-user`"));
        (
            user.attr("data-vm-user").unwrap_or_default().to_owned(),
            user.text(),
        )
    }

    /// Every element `data-vm-application`, in order, read.
    pub fn applications(&self) -> Vec<Listed> {
        self.root()
            .with_attr("data-vm-application")
            .iter()
            .map(Listed::read)
            .collect()
    }

    /// The names of [`Page::applications`], in order.
    pub fn names(&self) -> Vec<String> {
        self.applications().into_iter().map(|a| a.name).collect()
    }

    /// The Application of this name, failing unless the page has exactly
    /// one element of it.
    pub fn listed(&self, name: &str) -> Listed {
        let found: Vec<Listed> = self
            .applications()
            .into_iter()
            .filter(|a| a.name == name)
            .collect();
        match found.as_slice() {
            [one] => one.clone(),
            _ => panic!(
                "expected exactly one element `data-vm-application={name:?}` on the page at {}, found {}\n{}",
                self.url,
                found.len(),
                self.describe()
            ),
        }
    }

    /// The `href` of the one `a[<attribute>]` of the page, resolved against
    /// its address; `None` when there is none; fails when there are more.
    pub fn link(&self, attribute: &str) -> Option<Url> {
        let links = self.root().with_attr(attribute);
        match links.as_slice() {
            [] => None,
            [one] if one.name() == "a" && one.has("href") => {
                let href = one.attr("href").unwrap_or_default();
                Some(self.url.join(href).unwrap_or_else(|e| {
                    panic!("the `href` {href:?} of `a[{attribute}]` is not an address: {e}")
                }))
            }
            _ => panic!(
                "expected at most one link `a[{attribute}]` with an `href` on the page at {}\n{}",
                self.url,
                self.describe()
            ),
        }
    }

    /// Every `href` of the page, as written.
    pub fn hrefs(&self) -> Vec<String> {
        self.root()
            .with_attr("href")
            .iter()
            .map(|e| e.attr("href").unwrap_or_default().to_owned())
            .collect()
    }

    /// The value of every element `data-vm-notice`.
    pub fn notices(&self) -> Vec<String> {
        self.root()
            .with_attr("data-vm-notice")
            .iter()
            .map(|e| e.attr("data-vm-notice").unwrap_or_default().to_owned())
            .collect()
    }

    /// Every element `data-vm-error`: (its value, its text).
    pub fn errors(&self) -> Vec<(String, String)> {
        self.root()
            .with_attr("data-vm-error")
            .iter()
            .map(|e| {
                (
                    e.attr("data-vm-error").unwrap_or_default().to_owned(),
                    e.text(),
                )
            })
            .collect()
    }

    /// Every element `data-vm-shared`, in order.
    pub fn shared(&self) -> Vec<Shown> {
        self.root()
            .with_attr("data-vm-shared")
            .iter()
            .map(|e| Shown {
                kind: e.attr("data-vm-shared").unwrap_or_default().to_owned(),
                id: e.attr("data-vm-id").map(str::to_owned),
                text: e.text(),
                unseen: e.has("data-vm-unseen"),
            })
            .collect()
    }

    /// What each element `data-vm-shared` names, in order.
    pub fn shared_keys(&self) -> Vec<String> {
        self.shared().iter().map(Shown::key).collect()
    }

    /// Every element `data-vm-contributor` of the page, in order.
    pub fn contributors(&self) -> Vec<Shown> {
        self.root()
            .with_attr("data-vm-contributor")
            .iter()
            .map(shown_contributor)
            .collect()
    }

    /// What each Contributor of the page names (see [`Shown::key`]), in
    /// order.
    pub fn contributor_keys(&self) -> Vec<String> {
        self.contributors().iter().map(Shown::key).collect()
    }

    /// Every element `data-vm-resource` of the page, in order.
    pub fn resources(&self) -> Vec<Declared> {
        self.root()
            .with_attr("data-vm-resource")
            .iter()
            .map(Declared::read)
            .collect()
    }

    /// The declared Resource of this name, failing unless the page has
    /// exactly one element of it.
    pub fn resource(&self, name: &str) -> Declared {
        let found: Vec<Declared> = self
            .resources()
            .into_iter()
            .filter(|r| r.name == name)
            .collect();
        match found.as_slice() {
            [one] => one.clone(),
            _ => panic!(
                "expected exactly one element `data-vm-resource={name:?}` on the page at {}, found {}\n{}",
                self.url,
                found.len(),
                self.describe()
            ),
        }
    }

    /// How many elements of the page carry this attribute.
    pub fn count(&self, attribute: &str) -> usize {
        self.root().with_attr(attribute).len()
    }

    /// The text a person reads on the page: that of its `<body>`, without
    /// the `<title>` (the whole page's when there is no `<body>`).
    pub fn body_text(&self) -> String {
        let bodies = self.root().named("body");
        bodies.first().map_or_else(|| self.text(), Element::text)
    }

    /// Every form of the page.
    pub fn forms(&self) -> Vec<Form> {
        self.root().named("form").iter().map(Form::read).collect()
    }

    /// The forms whose `data-vm-form` is `kind`.
    pub fn forms_of(&self, kind: &str) -> Vec<Form> {
        self.forms()
            .into_iter()
            .filter(|f| f.kind.as_deref() == Some(kind))
            .collect()
    }

    /// The address a form posts to, resolved against the page's.
    pub fn action_of(&self, form: &Form) -> Url {
        let action = form.action.as_deref().unwrap_or_default();
        self.url
            .join(action)
            .unwrap_or_else(|e| panic!("the form's action {action:?} is not an address: {e}"))
    }

    /// The one share form; fails unless there is exactly one.
    pub fn share_form(&self) -> Form {
        let forms = self.forms_of("share");
        match forms.as_slice() {
            [one] => one.clone(),
            _ => panic!(
                "expected exactly one `form[data-vm-form=\"share\"]` on the page at {}, found {}\n{}",
                self.url,
                forms.len(),
                self.describe()
            ),
        }
    }

    /// Where the share form posts and the body a person sends with it,
    /// `kind` chosen and `value` typed. Fails unless the form offers that
    /// `kind` and has a text input `value`.
    pub fn share_submission(&self, kind: &str, value: &str) -> (Url, String) {
        let form = self.share_form();
        assert!(
            form.choices("kind").iter().any(|c| c == kind),
            "expected the share form to offer the kind {kind:?} (a `select` or radio buttons named `kind`), it offers {:?}",
            form.choices("kind")
        );
        assert!(
            form.has_text_input("value"),
            "expected the share form to have a text input named `value`: {form:?}"
        );
        (
            self.action_of(&form),
            form.body(&[("kind", kind), ("value", value)]),
        )
    }

    /// The unshare form whose hidden fields name this entry (`kind` and
    /// `value`); fails unless there is exactly one.
    pub fn unshare_form(&self, kind: &str, value: &str) -> Form {
        let forms: Vec<Form> = self
            .forms_of("unshare")
            .into_iter()
            .filter(|f| {
                f.hidden("kind") == Some(kind)
                    && (kind == "everyone" || f.hidden("value") == Some(value))
            })
            .collect();
        match forms.as_slice() {
            [one] => one.clone(),
            _ => panic!(
                "expected exactly one `form[data-vm-form=\"unshare\"]` naming {kind} {value:?} in its hidden fields on the page at {}, found {}: {:?}",
                self.url,
                forms.len(),
                self.forms_of("unshare")
            ),
        }
    }

    /// Where the unshare form of this entry posts, and its body.
    pub fn unshare_submission(&self, kind: &str, value: &str) -> (Url, String) {
        let form = self.unshare_form(kind, value);
        (self.action_of(&form), form.body(&[]))
    }
}

/// Fails unless the answer keeps the rules of every page of the
/// Dashboard: the headers of every page of the platform's own and a
/// `content-security-policy` with `form-action 'self'`; no `<script>`, no
/// event-handler attribute, no `javascript:` address.
pub fn assert_page_rules(response: &Response, context: &str) {
    assert_own_page_headers(response, context);
    let policy = response
        .header("content-security-policy")
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        policy.contains("form-action 'self'"),
        "{context}: expected a `content-security-policy` with `form-action 'self'`, got\n{}",
        response.describe()
    );
    let html = response.text();
    assert!(
        !html.to_ascii_lowercase().contains("<script"),
        "{context}: expected no `<script>` on a page of the Dashboard\n{html}"
    );
    let document = Document::parse(&html);
    assert_viewport(&document, context, &html);
    for element in document.root().descendants() {
        for (name, value) in element.attributes() {
            assert!(
                !name.starts_with("on"),
                "{context}: expected no event-handler attribute, found `{name}` on <{}>\n{html}",
                element.name()
            );
            let squashed: String = value
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .collect::<String>()
                .to_ascii_lowercase();
            assert!(
                !squashed.starts_with("javascript:"),
                "{context}: expected no `javascript:` address, found `{name}={value:?}` on <{}>\n{html}",
                element.name()
            );
        }
    }
}

/// Fails unless the page declares, in exactly one `meta` element with
/// `name="viewport"`, a `content` with `width=device-width` and
/// `initial-scale=1` (CONTRACT.md, "The Dashboard: What it answers": a
/// page is never wider than its window).
fn assert_viewport(document: &Document, context: &str, html: &str) {
    let viewports: Vec<Element<'_>> = document
        .root()
        .named("meta")
        .into_iter()
        .filter(|m| {
            m.attr("name")
                .is_some_and(|n| n.trim().eq_ignore_ascii_case("viewport"))
        })
        .collect();
    let [viewport] = viewports.as_slice() else {
        panic!(
            "{context}: expected one `<meta name=\"viewport\">`, found {}\n{html}",
            viewports.len()
        );
    };
    let content = viewport.attr("content").unwrap_or_default();
    let pairs: Vec<(String, String)> = content
        .split([',', ';'])
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((
                key.trim().to_ascii_lowercase(),
                value.trim().to_ascii_lowercase(),
            ))
        })
        .collect();
    let has =
        |key: &str, wanted: &dyn Fn(&str) -> bool| pairs.iter().any(|(k, v)| k == key && wanted(v));
    assert!(
        has("width", &|v| v == "device-width") && has("initial-scale", &|v| v == "1" || v == "1.0"),
        "{context}: expected the viewport's `content` to have `width=device-width` and `initial-scale=1`, got {content:?}"
    );
}

/// Fails unless the answer is the `303` of a change made, to
/// `/applications/<name>?changed=<changed>` on the Dashboard's address.
pub fn assert_changed(
    response: &Response,
    base: &Url,
    application: &str,
    changed: &str,
    context: &str,
) {
    let location = response.header("location").map(|l| base.join(l));
    let went = match &location {
        Some(Ok(at)) => {
            at.host_str() == Some(host().as_str())
                && at.path() == format!("/applications/{application}")
                && at.query() == Some(&format!("changed={changed}"))
        }
        _ => false,
    };
    assert!(
        response.status == 303 && went,
        "{context}: expected `303` to /applications/{application}?changed={changed}, got\n{}",
        response.describe()
    );
}

/// Fails unless the answer is the refusal page of a change of sharing:
/// `status`, a page of the Dashboard with one element `data-vm-error` of
/// `reason` whose text is not empty, and a link `a[data-vm-back]` to
/// `/applications/<name>`. Returns the page.
pub fn assert_refused(
    response: Response,
    base: &Url,
    status: u16,
    reason: &str,
    application: &str,
    context: &str,
) -> Page {
    assert!(
        response.status == status,
        "{context}: expected the refusal `{status}` {reason:?}, got\n{}",
        response.describe()
    );
    let page = Page::read(response, base.clone(), context);
    let errors = page.errors();
    assert!(
        matches!(errors.as_slice(), [(r, text)] if r == reason && !text.is_empty()),
        "{context}: expected one element `data-vm-error={reason:?}` with words, got {errors:?}\n{}",
        page.describe()
    );
    let text = error_text(&page);
    assert!(
        !text.contains("/v1/"),
        "{context}: expected the words of the refusal to have no path of the API, they are {text:?}"
    );
    if status == 503 {
        assert!(
            page.response.header("retry-after").is_some(),
            "{context}: expected `retry-after` on the `503`\n{}",
            page.describe()
        );
    }
    let back = page.link("data-vm-back");
    assert!(
        back.as_ref()
            .is_some_and(|b| b.host_str() == Some(host().as_str())
                && b.path() == format!("/applications/{application}")),
        "{context}: expected a link `a[data-vm-back]` to /applications/{application}, got {back:?}\n{}",
        page.describe()
    );
    page
}

/// The text of the one element `data-vm-error` of a refusal page.
pub fn error_text(page: &Page) -> String {
    page.errors()
        .into_iter()
        .next()
        .map(|(_, text)| text)
        .unwrap_or_default()
}

/// Fails unless the words of a refusal's `data-vm-error` are for a person
/// at a browser and name what the refusal is about: each of `named` is in
/// them (any one of the alternatives of each), and no path of the API
/// (`/v1/`) is.
pub fn assert_refusal_words(page: &Page, named: &[&[&str]], context: &str) {
    let text = error_text(page);
    for alternatives in named {
        assert!(
            alternatives.iter().any(|a| text.contains(a)),
            "{context}: expected the words of the refusal to name {alternatives:?}, they are {text:?}"
        );
    }
    assert!(
        !text.contains("/v1/"),
        "{context}: expected the words of the refusal to be for a person at a browser, with no path of the API, they are {text:?}"
    );
}

/// Creates the Application `name` as this User, with this description or
/// none, and no Version; fails unless it is `201`.
pub fn create(vm: &Vm, user: &User, name: &str, description: Option<&str>) {
    let body = match description {
        Some(d) => json!({ "name": name, "description": d }),
        None => json!({ "name": name }),
    };
    let answer = vm.api_as(user).create_application_with(&body);
    if answer.status != 201 {
        vm.fail(&format!(
            "expected `201` creating {name:?} as {:?}, got\n{}",
            user.id,
            answer.describe()
        ));
    }
}

/// Sends `fixtures/honest.js` as a Version of `name` as this User, with
/// these declared Resources; fails unless it is `201`. Returns its number.
pub fn deploy(vm: &Vm, user: &User, name: &str, declared: &[(&str, &str)]) -> u64 {
    let answer = vm.api_as(user).send_version(
        name,
        &crate::api::version_body_declaring(
            &crate::vm::fixture_bundle(crate::sharing::TRACED),
            declared,
        ),
    );
    if answer.status != 201 {
        vm.fail(&format!(
            "expected `201` sending a Version of {name:?} as {:?}, got\n{}",
            user.id,
            answer.describe()
        ));
    }
    answer.json()["number"].as_u64().unwrap_or_default()
}

/// The Application as `GET /v1/applications/<name>` answers this User,
/// which must be `200`.
pub fn read_back(vm: &Vm, user: &User, name: &str) -> Value {
    let answer = vm.api_as(user).get_application(name);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` reading {name:?} back as {:?}, got\n{}",
            user.id,
            answer.describe()
        ));
    }
    answer.json()
}
