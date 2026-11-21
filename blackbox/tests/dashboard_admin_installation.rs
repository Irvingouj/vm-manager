//! The installation on the Dashboard (CONTRACT.md, "The Dashboard for
//! Platform administrators: The installation"): what `GET /v1/installation`
//! answers (the domain, how people sign in, the sign-in address with OIDC);
//! with OIDC, the connection as `GET /v1/oidc` answers it (the issuer and
//! the administrators' Group first, the client id, the Groups claim, the
//! scopes, when and by whom one step away) and that it is changed with
//! `vibe admin connect-oidc`; with the stand-in, words saying so and no
//! issuer; and never a byte of a secret in the answer, headers and body:
//! the client secret, the setup credential, `VIBE_SECRET_KEY`, the
//! session's cookie, a refresh token, a tool credential.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin_pages::INSTALLATION;
use blackbox::connection::{Connection, SecretKey, connect_with_setup, hide, setup_credential};
use blackbox::dashboard::{
    Dashboard, Page, administrator, oidc_page, oidc_sign_in, stand_in_options,
};
use blackbox::oidc::{Person, Provider};
use blackbox::page::Element;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, Installation, SESSION_COOKIE, grace, oidc_options};
use blackbox::tool::{Machine, bearer};
use blackbox::{DOMAIN, Response, Vm};

/// The whole answer as text: every header and the body.
fn whole(response: &Response) -> String {
    let mut text: String = response
        .headers()
        .iter()
        .map(|(n, v)| format!("{n}: {v}\n"))
        .collect();
    text.push_str(&response.text());
    text
}

/// Fails if any of `secrets` (named, for the message) is in the answer.
fn assert_holds_none(response: &Response, secrets: &[(&str, &str)], context: &str) {
    let text = whole(response);
    for (what, secret) in secrets {
        assert!(
            !secret.is_empty(),
            "the harness's own check: {what} is known to the test"
        );
        assert!(
            !text.contains(secret),
            "{context}: the answer holds {what} (its value not shown here)"
        );
    }
}

fn texts(page: &Page, attribute: &str) -> Vec<(String, String)> {
    page.root()
        .with_attr(attribute)
        .iter()
        .map(|e| (e.attr(attribute).unwrap_or_default().to_owned(), e.text()))
        .collect()
}

#[test]
fn with_the_stand_in_it_says_the_domain_and_that_sign_in_is_the_stand_in_and_no_issuer() {
    let key = SecretKey::fresh();
    let vm = Vm::start(&stand_in_options().env("VIBE_SECRET_KEY", key.expose()));
    let answer = Dashboard::new(&vm, &administrator()).get(INSTALLATION);
    assert_eq!(answer.status, 200, "{}", answer.describe());
    assert_holds_none(
        &answer,
        &[("VIBE_SECRET_KEY", key.expose())],
        "the installation, stand-in",
    );
    let page = Page::read(
        answer,
        Dashboard::new(&vm, &administrator()).url(INSTALLATION),
        "installation",
    );
    let domain = texts(&page, "data-vm-domain");
    assert!(
        matches!(domain.as_slice(), [(_, t)] if t.contains(DOMAIN)),
        "expected one `data-vm-domain` holding {DOMAIN}, got {domain:?}"
    );
    let sign_in = texts(&page, "data-vm-sign-in");
    assert!(
        matches!(sign_in.as_slice(), [(v, _)] if v == "stand-in"),
        "expected `data-vm-sign-in=\"stand-in\"`, got {sign_in:?}"
    );
    let text = page.body_text().to_lowercase();
    assert!(
        text.contains("stand-in") && text.contains("oidc") && text.contains("not connected"),
        "expected words that sign-in is the stand-in, for tests and the local run, and that OIDC is not connected; got {text:?}"
    );
    assert!(
        text.contains("test"),
        "for tests and the local run: {text:?}"
    );
    assert_eq!(
        page.count("data-vm-issuer"),
        0,
        "no `data-vm-issuer` with the stand-in"
    );
    assert_eq!(page.count("data-vm-administrators-group"), 0);
}

fn ada_admin() -> Person {
    Person::new(ADA)
        .named("Ada Lovelace")
        .email("ada@example.test")
        .in_groups(&["finance", ADMINISTRATORS])
}

#[test]
fn with_oidc_it_shows_the_connection_as_the_api_answers_it_and_never_a_secret() {
    let provider = Provider::start(&[ada_admin(), grace()]);
    let options = oidc_options();
    let key = options.env_value("VIBE_SECRET_KEY").unwrap().to_owned();
    let at = Installation::unconnected(provider, &options);
    let connection = Connection::to(&at.provider)
        .groups_claim("groups")
        .scopes("openid profile email offline_access");
    let setup = setup_credential(at.vm.platform());
    let run = connect_with_setup(&Machine::new(), &at.vm, &setup, &connection, true);
    let shown = hide(&run, &[setup.expose(), connection.secret().expose()]);
    assert_eq!(
        shown.code,
        Some(0),
        "the harness's own step: connecting\n{}",
        shown.describe()
    );

    let credential = at.tool_credential_for(ADA);
    let oidc = bearer(&at.vm, "/v1/oidc", &credential).send();
    assert_eq!(oidc.status, 200, "the harness's own check: GET /v1/oidc");
    let oidc = oidc.json();
    let installation = blackbox::connection::installation_of(&at.vm).json();

    let mut browser = at.browser_as(ADA);
    oidc_sign_in(&at, &mut browser);
    let page = oidc_page(
        &at,
        &mut browser,
        INSTALLATION,
        200,
        "the installation with OIDC",
    );
    let session = at.cookie_value(&browser, &format!("dashboard.{DOMAIN}"), SESSION_COOKIE);
    let refresh = at.provider.refresh_tokens_issued();
    let mut secrets: Vec<(&str, &str)> = vec![
        ("the OIDC client secret", connection.secret().expose()),
        ("the setup credential", setup.expose()),
        ("VIBE_SECRET_KEY", key.as_str()),
        ("the session's cookie", session.as_str()),
        ("a tool credential", credential.as_str()),
    ];
    for token in &refresh {
        secrets.push(("a refresh token", token.as_str()));
    }
    assert_holds_none(&page.response, &secrets, "the installation with OIDC");

    let domain = texts(&page, "data-vm-domain");
    assert!(
        matches!(domain.as_slice(), [(_, t)] if t.contains(installation["domain"].as_str().unwrap())),
        "expected the domain {installation}, got {domain:?}"
    );
    let sign_in = texts(&page, "data-vm-sign-in");
    assert!(
        matches!(sign_in.as_slice(), [(v, _)] if v == "oidc"),
        "got {sign_in:?}"
    );
    let address = installation["sign_in_address"].as_str().unwrap();
    assert!(
        page.body_text().contains(address),
        "expected the sign-in address {address} as text\n{}",
        page.describe()
    );
    let issuer = texts(&page, "data-vm-issuer");
    assert!(
        matches!(issuer.as_slice(), [(_, t)] if t.contains(oidc["issuer"].as_str().unwrap())),
        "expected `data-vm-issuer` holding {}, got {issuer:?}",
        oidc["issuer"]
    );
    let group = texts(&page, "data-vm-administrators-group");
    assert!(
        matches!(group.as_slice(), [(_, t)] if t.contains(ADMINISTRATORS)),
        "expected `data-vm-administrators-group` holding {ADMINISTRATORS}, got {group:?}"
    );
    // The issuer and the Group seen first; the rest in a `<details>`.
    let root = page.root();
    let details: Vec<Element<'_>> = root.named("details");
    let in_details: String = details
        .iter()
        .map(Element::text)
        .collect::<Vec<_>>()
        .join(" ");
    for (what, value) in [
        (
            "the client id",
            oidc["client_id"].as_str().unwrap().to_owned(),
        ),
        (
            "the Groups claim",
            oidc["groups_claim"].as_str().unwrap().to_owned(),
        ),
        (
            "when it was connected (to the minute)",
            oidc["connected_at"]
                .as_str()
                .unwrap()
                .chars()
                .take(10)
                .collect(),
        ),
    ] {
        assert!(
            in_details.contains(&value),
            "expected {what} ({value}) one step away, in a `<details>`; they say {in_details:?}"
        );
    }
    for scope in oidc["scopes"].as_array().unwrap() {
        let scope = scope.as_str().unwrap();
        assert!(
            in_details.contains(scope),
            "expected the scope {scope} in the `<details>`"
        );
    }
    assert_eq!(
        oidc["connected_by"], "setup-credential",
        "the harness's own check"
    );
    assert!(
        in_details.to_lowercase().contains("the setup credential"),
        "expected \"the setup credential\" for who connected it, got {in_details:?}"
    );
    for e in root
        .with_attr("data-vm-issuer")
        .iter()
        .chain(root.with_attr("data-vm-administrators-group").iter())
    {
        assert!(
            !details.iter().any(|d| d.with_attr("data-vm-issuer").len()
                + d.with_attr("data-vm-administrators-group").len()
                > 0),
            "expected the issuer and the administrators' Group seen first, not in a `<details>`: {:?}",
            e.text()
        );
    }
    assert!(
        page.body_text().contains("vibe admin connect-oidc"),
        "expected words saying the connection is changed with `vibe admin connect-oidc`"
    );
}
