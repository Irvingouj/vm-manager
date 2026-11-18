//! The Resources an Application's serving Version declares, as the
//! Dashboard shows them to each person (CONTRACT.md, "The Dashboard: The
//! list"), with the stand-in sign-in: `data-vm-declared`, `data-vm-yours`
//! (what `your_access` of `GET /v1/resources/<name>` says) and
//! `data-vm-standing` (`enough`, `lower`, `none`, `not-registered`), with
//! whom to ask (`data-vm-ask`, and in the words) where the person holds no
//! Grant or one too low. (`unknown` is in `dashboard_lost_answer.rs`.)
//!
//! Acceptance criterion: "Each Application shows name, description,
//! Contributors and declared Resources; a Resource the person holds no
//! Grant on is marked, with whom to ask".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::dashboard::{Dashboard, Declared, Listed, create, deploy, stand_in_options};
use blackbox::scenario::{CONTACT, Scenario, resource_body};
use blackbox::sharing::{add, share_ok, user};
use blackbox::{User, Vm};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// What `board` declares: (Resource, level).
const DECLARED: [(&str, &str); 5] = [
    ("none_res", "read"),
    ("enough_res", "read"),
    ("ghost_res", "read"),
    ("lower_res", "write"),
    ("higher_res", "read"),
];

/// Four Resources registered (`ghost_res` is not): `staff` holds `read` on
/// `enough_res`, `structure` on `lower_res`, `write` on `higher_res`, and
/// nothing on `none_res` (another Group does). Alice's `board` declares
/// [`DECLARED`] and is shared with bob, who is in no Group.
fn world() -> Scenario {
    let s = Scenario::start_with(&stand_in_options());
    for resource in ["enough_res", "lower_res", "none_res", "higher_res"] {
        s.register(resource);
    }
    s.grant("enough_res", "staff", "read");
    s.grant("lower_res", "staff", "structure");
    s.grant("higher_res", "staff", "write");
    s.grant("none_res", "others", "write");
    create(&s.vm, &alice(), "board", None);
    deploy(&s.vm, &alice(), "board", &DECLARED);
    share_ok(&s.vm.api_as(&alice()), &s.vm, "board", &add(&[user("bob")]));
    s
}

/// (name, declared, yours, standing, ask) of each Resource shown, in order.
type Row = (String, String, String, String, Option<String>);

fn rows(resources: &[Declared]) -> Vec<Row> {
    resources
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                r.declared.clone().unwrap_or_default(),
                r.yours.clone().unwrap_or_default(),
                r.standing.clone().unwrap_or_default(),
                r.ask.clone(),
            )
        })
        .collect()
}

/// [`rows`], with `data-vm-ask` kept only for `lower` and `none`, where it
/// is promised; elsewhere it is not looked at.
fn seen(resources: &[Declared]) -> Vec<Row> {
    rows(resources)
        .into_iter()
        .map(|(n, d, y, standing, ask)| {
            let ask = if matches!(standing.as_str(), "lower" | "none") {
                ask
            } else {
                None
            };
            (n, d, y, standing, ask)
        })
        .collect()
}

fn row(name: &str, declared: &str, yours: &str, standing: &str, ask: Option<&str>) -> Row {
    (
        name.to_owned(),
        declared.to_owned(),
        yours.to_owned(),
        standing.to_owned(),
        ask.map(str::to_owned),
    )
}

/// Fails unless what the Dashboard says the User holds on each registered
/// Resource is what `your_access` of `GET /v1/resources/<name>` says (null
/// as `none`).
fn assert_yours_is_your_access(vm: &Vm, person: &User, listed: &Listed) {
    for resource in &listed.resources {
        let answer = vm.api_as(person).get_resource(&resource.name);
        let api = match answer.status {
            200 => answer.json()["your_access"]
                .as_str()
                .unwrap_or("none")
                .to_owned(),
            404 => "none".to_owned(),
            _ => panic!(
                "the harness's own check: reading {:?}\n{}",
                resource.name,
                answer.describe()
            ),
        };
        assert_eq!(
            resource.yours.as_deref(),
            Some(api.as_str()),
            "{}: expected `data-vm-yours` of {:?} to be what `your_access` of `GET /v1/resources/{}` says ({api})",
            person.id,
            resource.name,
            resource.name
        );
    }
}

#[test]
fn each_declared_resource_shows_the_level_declared_what_the_person_holds_and_whether_it_is_enough()
{
    let s = world();
    let page = Dashboard::new(&s.vm, &alice()).list();
    let board = page.listed("board");
    assert_eq!(
        seen(&board.resources),
        vec![
            row("enough_res", "read", "read", "enough", None),
            row("ghost_res", "read", "none", "not-registered", None),
            row("higher_res", "read", "write", "enough", None),
            row("lower_res", "write", "structure", "lower", Some(CONTACT)),
            row("none_res", "read", "none", "none", Some(CONTACT)),
        ],
        "expected each Resource the serving Version declares, by name, with `data-vm-declared`, `data-vm-yours`, `data-vm-standing`, and `data-vm-ask` the Resource's contact for `lower` and `none`\n{}",
        page.describe()
    );
    assert_yours_is_your_access(&s.vm, &alice(), &board);

    let lower = board.resource("lower_res");
    assert!(
        lower.text.contains(CONTACT) && lower.text.contains("structure"),
        "expected the words of `lower` to say what alice holds (structure) and whom to ask ({CONTACT:?}), got {:?}",
        lower.text
    );
    let none = board.resource("none_res");
    assert!(
        none.text.contains(CONTACT),
        "expected the words of `none` to show whom to ask ({CONTACT:?}), got {:?}",
        none.text
    );
    let ghost = board.resource("ghost_res");
    assert!(
        ghost.text.to_lowercase().contains("registered"),
        "expected the words of `not-registered` to say no such Resource is registered, got {:?}",
        ghost.text
    );
    for resource in &board.resources {
        assert!(
            resource.text.contains(&resource.name),
            "expected each Resource shown by its name, got {:?}",
            resource.text
        );
    }
}

#[test]
fn what_is_shown_is_the_persons_own_standing() {
    let s = world();
    let page = Dashboard::new(&s.vm, &bob()).list();
    let board = page.listed("board");
    assert_eq!(
        board.relation.as_deref(),
        Some("shared"),
        "the harness's own check: board is shared with bob"
    );
    assert_eq!(
        seen(&board.resources),
        vec![
            row("enough_res", "read", "none", "none", Some(CONTACT)),
            row("ghost_res", "read", "none", "not-registered", None),
            row("higher_res", "read", "none", "none", Some(CONTACT)),
            row("lower_res", "write", "none", "none", Some(CONTACT)),
            row("none_res", "read", "none", "none", Some(CONTACT)),
        ],
        "expected bob, who holds no Grant, to see each registered Resource marked `none` with whom to ask\n{}",
        page.describe()
    );
    assert_yours_is_your_access(&s.vm, &bob(), &board);
}

#[test]
fn the_applications_own_page_shows_the_same_resources() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &alice());
    let listed = dashboard.list().listed("board");
    let details = dashboard.details("board");
    assert_eq!(
        rows(&details.listed("board").resources),
        rows(&listed.resources),
        "expected the Application's own page to show the same Resources as the list\n{}",
        details.describe()
    );
}

#[test]
fn only_the_serving_versions_resources_are_shown_and_none_without_a_version() {
    let s = world();
    let vm = &s.vm;
    create(vm, &alice(), "moving", None);
    deploy(vm, &alice(), "moving", &[("enough_res", "read")]);
    deploy(vm, &alice(), "moving", &[("lower_res", "read")]);
    create(vm, &alice(), "plain", None);
    deploy(vm, &alice(), "plain", &[]);
    create(vm, &alice(), "draft", None);
    let page = Dashboard::new(vm, &alice()).list();
    assert_eq!(
        rows(&page.listed("moving").resources)
            .into_iter()
            .map(|r| (r.0, r.1, r.2, r.3))
            .collect::<Vec<_>>(),
        vec![(
            "lower_res".to_owned(),
            "read".to_owned(),
            "structure".to_owned(),
            "lower".to_owned()
        )],
        "expected only what the serving Version (2) declares\n{}",
        page.describe()
    );
    for name in ["plain", "draft"] {
        assert!(
            page.listed(name).resources.is_empty(),
            "expected no `data-vm-resource` in {name:?}, which declares none or has no Version\n{}",
            page.describe()
        );
    }
}

#[test]
fn a_grant_given_shows_at_the_next_request() {
    let s = world();
    let dashboard = Dashboard::new(&s.vm, &alice());
    assert_eq!(
        dashboard
            .list()
            .listed("board")
            .resource("lower_res")
            .standing
            .as_deref(),
        Some("lower"),
        "the harness's own check"
    );
    s.grant("lower_res", "staff", "write");
    s.grant("none_res", "staff", "read");
    let board = dashboard.list().listed("board");
    for name in ["lower_res", "none_res"] {
        let resource = board.resource(name);
        assert_eq!(
            resource.standing.as_deref(),
            Some("enough"),
            "expected {name:?} `enough` at the next request once alice's Group holds enough, got {resource:?}"
        );
    }
    assert_eq!(board.resource("lower_res").yours.as_deref(), Some("write"));
}

#[test]
fn a_resources_contact_is_escaped_in_the_attribute_and_in_the_words() {
    let s = world();
    let hostile = "Ops <ops@example.test> & \"Quinn\" 'Q'";
    let mut body = resource_body("hostile_res", &s.db, &s.db.reader);
    body["contact"] = serde_json::json!(hostile);
    s.register_body("hostile_res", &body);
    create(&s.vm, &alice(), "asks", None);
    deploy(&s.vm, &alice(), "asks", &[("hostile_res", "read")]);
    let page = Dashboard::new(&s.vm, &alice()).list();
    let resource = page.listed("asks").resource("hostile_res").clone();
    assert_eq!(
        resource.ask.as_deref(),
        Some(hostile),
        "expected `data-vm-ask` to hold the contact exactly, escaped as HTML in the attribute\n{}",
        page.describe()
    );
    assert!(
        resource.text.contains(hostile),
        "expected the words to show the contact as those characters, got {:?}",
        resource.text
    );
    let html = page.response.text();
    assert!(
        !html.contains("<ops@example.test>") && !html.contains("\"Quinn\""),
        "expected the contact escaped as HTML wherever it is on the page\n{html}"
    );
}
