//! A request from another origin is refused (CONTRACT.md, "Sign-in and
//! sessions: Whose request it is"): each case of the rule, under OIDC and
//! under the stand-in sign-in, the code not run and the Operation log
//! untouched; and the rule comes after the sign-in.
//!
//! Acceptance criterion: "A state-changing request from another origin is
//! refused".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::oidc::{Person, Provider};
use blackbox::scenario::{ADMINISTRATORS, Scenario, WRITERS, administrator, execute_sql};
use blackbox::sessions::{ADA, Installation, oidc_options};
use blackbox::{DOMAIN, Request, Response, User, Vm, VmOptions};
use serde_json::{Value, json};

/// One case of the rule: the method, the headers about where it comes
/// from (`{app}` and `{other}` stand for the Application's address and
/// another Application's), and whether it is refused.
struct Case {
    method: &'static str,
    headers: &'static [(&'static str, &'static str)],
    refused: bool,
}

const fn refused(method: &'static str, headers: &'static [(&'static str, &'static str)]) -> Case {
    Case {
        method,
        headers,
        refused: true,
    }
}

const fn allowed(method: &'static str, headers: &'static [(&'static str, &'static str)]) -> Case {
    Case {
        method,
        headers,
        refused: false,
    }
}

const CASES: &[Case] = &[
    // A method other than GET, HEAD, OPTIONS with an Origin that is not the
    // Application's address.
    refused("POST", &[("origin", "{other}")]),
    refused("POST", &[("origin", "null")]),
    refused("POST", &[("origin", "{app-other-scheme}")]),
    refused("POST", &[("origin", "{app-other-port}")]),
    refused(
        "POST",
        &[("origin", "{other}"), ("sec-fetch-site", "same-origin")],
    ),
    refused("PUT", &[("origin", "{other}")]),
    refused("PATCH", &[("origin", "{other}")]),
    refused("DELETE", &[("origin", "{other}")]),
    // ... or no Origin and a Sec-Fetch-Site other than same-origin or none.
    refused("POST", &[("sec-fetch-site", "same-site")]),
    refused("POST", &[("sec-fetch-site", "cross-site")]),
    refused("DELETE", &[("sec-fetch-site", "cross-site")]),
    // A GET or HEAD that is cross-site or same-site and not a navigation of
    // the whole page.
    refused(
        "GET",
        &[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "cors"),
            ("sec-fetch-dest", "empty"),
        ],
    ),
    refused(
        "GET",
        &[
            ("sec-fetch-site", "same-site"),
            ("sec-fetch-mode", "no-cors"),
            ("sec-fetch-dest", "image"),
        ],
    ),
    refused(
        "GET",
        &[
            ("sec-fetch-site", "same-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "iframe"),
        ],
    ),
    refused(
        "GET",
        &[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "frame"),
        ],
    ),
    refused(
        "GET",
        &[
            ("sec-fetch-site", "same-site"),
            ("sec-fetch-mode", "navigate"),
        ],
    ),
    refused(
        "HEAD",
        &[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "cors"),
            ("sec-fetch-dest", "empty"),
        ],
    ),
    // An OPTIONS with an Origin that is not the Application's address.
    refused("OPTIONS", &[("origin", "{other}")]),
    // What is taken.
    allowed("POST", &[("origin", "{app}")]),
    allowed(
        "POST",
        &[("origin", "{app}"), ("sec-fetch-site", "same-origin")],
    ),
    allowed("POST", &[("sec-fetch-site", "same-origin")]),
    allowed("POST", &[("sec-fetch-site", "none")]),
    allowed("POST", &[]),
    allowed("DELETE", &[("origin", "{app}")]),
    allowed(
        "GET",
        &[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "document"),
        ],
    ),
    allowed(
        "GET",
        &[
            ("sec-fetch-site", "same-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "document"),
        ],
    ),
    allowed(
        "GET",
        &[
            ("sec-fetch-site", "same-origin"),
            ("sec-fetch-mode", "cors"),
            ("sec-fetch-dest", "empty"),
        ],
    ),
    allowed(
        "GET",
        &[("sec-fetch-site", "none"), ("sec-fetch-mode", "navigate")],
    ),
    allowed("GET", &[]),
    allowed("HEAD", &[]),
    allowed("OPTIONS", &[("origin", "{app}")]),
    allowed("OPTIONS", &[]),
];

/// The addresses the placeholders of [`CASES`] stand for.
struct Origins {
    app: String,
    other: String,
    app_other_scheme: String,
    app_other_port: String,
}

impl Origins {
    fn fill(&self, value: &str) -> String {
        value
            .replace("{app-other-scheme}", &self.app_other_scheme)
            .replace("{app-other-port}", &self.app_other_port)
            .replace("{app}", &self.app)
            .replace("{other}", &self.other)
    }
}

fn describe(case: &Case, origins: &Origins) -> String {
    let headers: Vec<String> = case
        .headers
        .iter()
        .map(|(n, v)| format!("{n}: {}", origins.fill(v)))
        .collect();
    format!("{} with [{}]", case.method, headers.join(", "))
}

/// Sends every case through `send` and checks each answer: a refusal is
/// `403`, `text/plain`, with no Application code run; what is taken is
/// answered by the code.
fn check_cases(
    vm: &Vm,
    origins: &Origins,
    send: impl Fn(&'static str, Vec<(String, String)>) -> Response,
) {
    for case in CASES {
        let headers = case
            .headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), origins.fill(v)))
            .collect();
        let answer = send(case.method, headers);
        let context = describe(case, origins);
        if case.refused {
            assert_eq!(
                answer.status,
                403,
                "{context} is refused\n{}",
                answer.describe()
            );
            assert!(
                answer
                    .header("content-type")
                    .is_some_and(|c| c.starts_with("text/plain")),
                "{context}: the refusal is text/plain\n{}",
                answer.describe()
            );
            vm.records_of(&answer).assert_no_application_code_ran();
        } else {
            assert_eq!(
                answer.status,
                200,
                "{context} is taken\n{}",
                answer.describe()
            );
            assert!(
                !vm.records_of(&answer).printed().is_empty(),
                "{context}: expected the code to run"
            );
        }
    }
}

#[test]
fn under_oidc_a_request_from_another_origin_is_refused_and_one_from_the_applications_own_taken() {
    let at = Installation::start(&[("honest", "honest.js"), ("ledger", "honest.js")]);
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let origins = Origins {
        app: at.app_origin("honest"),
        other: at.app_origin("ledger"),
        app_other_scheme: format!("https://honest.{DOMAIN}:{}", at.port()),
        app_other_port: format!("http://honest.{DOMAIN}:{}", at.port().wrapping_add(1)),
    };
    let page = at.app_url("honest", "/");
    check_cases(&at.vm, &origins, |method, headers| {
        let request = headers.iter().fold(
            browser
                .request(method, &page)
                .header("accept", "application/json"),
            |r, (n, v)| r.header(n, v),
        );
        request.send()
    });
}

#[test]
fn under_the_stand_in_a_request_from_another_origin_is_refused_too() {
    let vm = Vm::with_stand_in(&[("honest", "honest.js"), ("ledger", "honest.js")]);
    let ada = User::new("ada").in_groups(&["finance"]);
    // Without --public-scheme and --public-port an Application's address is
    // https://<application>.<domain>.
    let origins = Origins {
        app: format!("https://honest.{DOMAIN}"),
        other: format!("https://ledger.{DOMAIN}"),
        app_other_scheme: format!("http://honest.{DOMAIN}"),
        app_other_port: format!("https://honest.{DOMAIN}:8443"),
    };
    check_cases(&vm, &origins, |method, headers| {
        headers
            .iter()
            .fold(
                vm.request("honest", "/").as_user(&ada).method(method),
                |r, (n, v)| r.header(n, v),
            )
            .send()
    });
}

#[test]
fn the_rule_comes_after_the_sign_in_a_request_without_a_session_is_answered_as_ever() {
    let at = Installation::start(&[("honest", "honest.js"), ("ledger", "honest.js")]);
    let browser = at.browser();
    let page = at.app_url("honest", "/");
    let post = browser
        .request("POST", &page)
        .header("origin", at.app_origin("ledger"))
        .send();
    assert_eq!(post.status, 401, "{}", post.describe());
    let framed = browser
        .request("GET", &page)
        .header("accept", "text/html")
        .header("sec-fetch-site", "same-site")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "iframe")
        .send();
    assert_eq!(framed.status, 302, "{}", framed.describe());
    let stand_in = Vm::with_stand_in(&[("honest", "honest.js")]);
    let anonymous = stand_in
        .request("honest", "/")
        .method("POST")
        .header("origin", format!("https://ledger.{DOMAIN}"))
        .send();
    assert_eq!(anonymous.status, 401, "{}", anonymous.describe());
}

fn writer() -> User {
    User::new("wanda").in_groups(&[WRITERS])
}

const WRITE: &str = "update incidents set open = false where id = 1";

/// `POST /` on `board` (`fixtures/operations.js`) with one `execute`.
fn write_request(vm: &Vm, origin: &str) -> Request {
    vm.request("board", "/")
        .as_user(&writer())
        .method("POST")
        .header("origin", origin)
        .json(&json!({ "calls": [execute_sql(WRITE)] }))
}

fn entries_of(entries: &[Value], response: &Response) -> usize {
    let id = response.request_id();
    entries
        .iter()
        .filter(|e| e["request"].as_str() == Some(id.as_str()))
        .count()
}

#[test]
fn a_refused_request_leaves_the_operation_log_untouched_under_the_stand_in() {
    let s = Scenario::writable();
    let before = s.log().len();
    let refused = write_request(&s.vm, &format!("https://viewer.{DOMAIN}")).send();
    assert_eq!(refused.status, 403, "{}", refused.describe());
    s.vm.records_of(&refused).assert_no_application_code_ran();
    let after = s.log();
    assert_eq!(after.len(), before, "the Operation log is untouched");
    assert_eq!(entries_of(&after, &refused), 0);
    let taken = write_request(&s.vm, &format!("https://board.{DOMAIN}")).send();
    assert_eq!(taken.status, 200, "{}", taken.describe());
    assert_eq!(entries_of(&s.log(), &taken), 1, "a taken write is logged");
}

#[test]
fn a_refused_request_leaves_the_operation_log_untouched_under_oidc() {
    let Scenario { vm, db } = Scenario::writable();
    let platform = vm.platform().clone();
    drop(vm);
    let provider = Provider::start(&[Person::new(ADA).in_groups(&[WRITERS])]);
    let options = oidc_options(&provider).platform(&platform);
    let at = Installation::from(provider, &options);
    let mut browser = at.browser_as(ADA);
    // board's code answers only a POST, so the sign-in lands on an error of
    // its own: what matters is that it lands there with a session.
    let steps = browser.navigate(&at.app_url("board", "/"));
    assert_eq!(
        steps.last().unwrap().url,
        at.app_url("board", "/"),
        "{}",
        blackbox::browser::describe_steps(&steps)
    );
    at.cookie_value(
        &browser,
        &format!("board.{DOMAIN}"),
        blackbox::sessions::SESSION_COOKIE,
    );
    let page = at.app_url("board", "/");
    let body = json!({ "calls": [execute_sql(WRITE)] });
    let post = |origin: String| {
        browser
            .request("POST", &page)
            .header("origin", origin)
            .json(&body)
            .send()
    };
    let refused = post(at.app_origin("viewer"));
    assert_eq!(refused.status, 403, "{}", refused.describe());
    at.vm.records_of(&refused).assert_no_application_code_ran();
    let taken = post(at.app_origin("board"));
    assert_eq!(taken.status, 200, "{}", taken.describe());
    drop(at);
    let reader = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS)
            .platform(&platform),
    );
    let entries = reader.api_as(&administrator()).operation_log_entries();
    assert_eq!(
        entries_of(&entries, &refused),
        0,
        "the refused request left no entry"
    );
    let logged: Vec<&Value> = entries
        .iter()
        .filter(|e| e["request"].as_str() == Some(taken.request_id().as_str()))
        .collect();
    assert_eq!(
        logged.len(),
        1,
        "the taken request left one entry: {entries:?}"
    );
    assert_eq!(
        logged[0]["user"], ADA,
        "the entry names the sub as the User"
    );
    drop(db);
}
