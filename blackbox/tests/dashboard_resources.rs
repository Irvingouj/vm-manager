//! The Resources an Application's serving Version declares, as the
//! Dashboard shows them to each person (CONTRACT.md, "The Dashboard: The
//! list" and "One Application"), with the stand-in sign-in. On the
//! Application's own page: `data-vm-declared`, `data-vm-yours` (what
//! `your_access` of `GET /v1/resources/<name>` says) and `data-vm-standing`
//! (`enough`, `lower`, `none`, `not-registered`), each with a mark whose
//! name says the standing in words, and whom to ask (`data-vm-ask`, and in
//! the words) where the person holds no Grant or one too low. On the list:
//! none of that, only `data-vm-attention`, one mark at most for the worst
//! standing. (`unknown` is in `dashboard_lost_answer.rs`.)
//!
//! Acceptance criteria: "Each Application shows name, description,
//! Contributors and declared Resources; a Resource the person holds no
//! Grant on is marked, with whom to ask" (ticket "The Dashboard"); "The
//! Dashboard's list shows what is needed to choose and open an
//! Application; the rest is on the Application's page" (ticket "The pages
//! follow the product owner's four UI/UX rules").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::dashboard::{
    Dashboard, Declared, Page, assert_attention, assert_attention_attribute,
    assert_list_holds_nothing_of_an_applications_own_page, create, deploy, stand_in_options,
};
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
fn assert_yours_is_your_access(vm: &Vm, person: &User, page: &Page) {
    for resource in &page.resources() {
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

/// Fails unless the element says what the contract's table says for its
/// standing: the level declared, the level held where there is one, whom
/// to ask for `lower` and `none`; and unless it has a mark (an `<svg>`)
/// whose name, as a screen reader says the element, says the standing in
/// words: for `lower` and `none` that what needs more is refused.
fn assert_says_its_standing(resource: &Declared, context: &str) {
    let spoken = resource.spoken.to_lowercase();
    let declared = resource.declared.clone().unwrap_or_default();
    let yours = resource.yours.clone().unwrap_or_default();
    let standing = resource.standing.clone().unwrap_or_default();
    assert!(
        resource.text.contains(&resource.name),
        "{context}: expected the Resource shown by its name, got {:?}",
        resource.text
    );
    assert!(
        spoken.contains(&declared),
        "{context}: expected {:?} to say the level declared ({declared}), it says {:?}",
        resource.name,
        resource.spoken
    );
    let words: &[&str] = match standing.as_str() {
        "enough" => &["enough"],
        "lower" | "none" => &["refused"],
        "not-registered" => &["registered"],
        "unknown" => &["now"],
        other => panic!("{context}: an unexpected `data-vm-standing` {other:?}"),
    };
    assert!(
        words.iter().all(|w| spoken.contains(w)),
        "{context}: expected the name of the mark of `{standing}` on {:?} to say the standing in words ({words:?}), the element says {:?}",
        resource.name,
        resource.spoken
    );
    if matches!(standing.as_str(), "enough" | "lower") {
        assert!(
            spoken.contains(&yours),
            "{context}: expected {:?} to say the level held ({yours}), it says {:?}",
            resource.name,
            resource.spoken
        );
    }
    if standing == "none" {
        assert!(
            ["nothing", "none", "no access", "no grant"]
                .iter()
                .any(|w| spoken.contains(w)),
            "{context}: expected {:?} to say that they hold nothing, it says {:?}",
            resource.name,
            resource.spoken
        );
    }
    if matches!(standing.as_str(), "lower" | "none") {
        assert!(
            resource.text.contains(CONTACT),
            "{context}: expected {:?} to show whom to ask ({CONTACT:?}), it says {:?}",
            resource.name,
            resource.text
        );
    }
    let named = resource.marks.iter().any(|(hidden, role, label)| {
        hidden.as_deref() == Some("true")
            || (role.as_deref() == Some("img")
                && label.as_deref().is_some_and(|l| !l.trim().is_empty()))
    });
    assert!(
        named,
        "{context}: expected a mark (an `<svg>`, hidden beside its words or an image with a name) of `{standing}` in {:?}, its `<svg>`s are {:?}",
        resource.name, resource.marks
    );
}

#[test]
fn each_declared_resource_is_on_the_applications_page_with_the_level_declared_what_the_person_holds_and_whether_it_is_enough()
 {
    let s = world();
    let page = Dashboard::new(&s.vm, &alice()).details("board");
    assert_eq!(
        seen(&page.resources()),
        vec![
            row("enough_res", "read", "read", "enough", None),
            row("ghost_res", "read", "none", "not-registered", None),
            row("higher_res", "read", "write", "enough", None),
            row("lower_res", "write", "structure", "lower", Some(CONTACT)),
            row("none_res", "read", "none", "none", Some(CONTACT)),
        ],
        "expected each Resource the serving Version declares, by name, with `data-vm-declared`, `data-vm-yours`, `data-vm-standing`, and `data-vm-ask` the Resource's contact for `lower` and `none`, on the Application's page\n{}",
        page.describe()
    );
    assert_yours_is_your_access(&s.vm, &alice(), &page);
    for resource in &page.resources() {
        assert_says_its_standing(resource, "alice at board");
    }
}

#[test]
fn what_is_shown_is_the_persons_own_standing() {
    let s = world();
    let page = Dashboard::new(&s.vm, &bob()).details("board");
    assert_eq!(
        page.listed("board").relation.as_deref(),
        Some("shared"),
        "the harness's own check: board is shared with bob"
    );
    assert_eq!(
        seen(&page.resources()),
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
    assert_yours_is_your_access(&s.vm, &bob(), &page);
    for resource in &page.resources() {
        assert_says_its_standing(resource, "bob at board");
    }
}

#[test]
fn the_list_holds_no_resource_only_one_mark_for_the_worst_standing() {
    let s = world();
    let vm = &s.vm;
    // `ghostly`: one not registered, one enough. `fine`: all enough.
    // `lowered`: one `lower` and nothing else. `plain`: none declared.
    // `draft`: no Version.
    create(vm, &alice(), "ghostly", None);
    deploy(
        vm,
        &alice(),
        "ghostly",
        &[("ghost_res", "read"), ("enough_res", "read")],
    );
    create(vm, &alice(), "fine", None);
    deploy(
        vm,
        &alice(),
        "fine",
        &[("enough_res", "read"), ("higher_res", "write")],
    );
    create(vm, &alice(), "lowered", None);
    deploy(vm, &alice(), "lowered", &[("lower_res", "read")]);
    create(vm, &alice(), "plain", None);
    deploy(vm, &alice(), "plain", &[]);
    create(vm, &alice(), "draft", None);

    let dashboard = Dashboard::new(vm, &alice());
    let list = dashboard.list();
    assert_list_holds_nothing_of_an_applications_own_page(&list, "alice's list");
    for resource in [
        "enough_res",
        "ghost_res",
        "higher_res",
        "lower_res",
        "none_res",
    ] {
        assert!(
            !list.body_text().contains(resource),
            "expected no Resource named on the list, but it names {resource:?}\n{}",
            list.describe()
        );
    }
    for (name, wanted) in [
        ("board", "missing-access"),
        ("ghostly", "not-registered"),
        ("fine", "none"),
        ("lowered", "missing-access"),
        ("plain", "none"),
        ("draft", "none"),
    ] {
        let context = format!("alice's list, {name}");
        assert_attention(&list.listed(name), wanted, &context);
        assert_attention_attribute(
            &dashboard.details(name).listed(name),
            wanted,
            &format!("the page of {name} for alice"),
        );
    }

    let bobs = Dashboard::new(vm, &bob()).list();
    assert_list_holds_nothing_of_an_applications_own_page(&bobs, "bob's list");
    assert_attention(&bobs.listed("board"), "missing-access", "bob's list, board");
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
    let dashboard = Dashboard::new(vm, &alice());
    let page = dashboard.details("moving");
    assert_eq!(
        rows(&page.resources())
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
        let page = dashboard.details(name);
        assert!(
            page.resources().is_empty(),
            "expected no `data-vm-resource` on the page of {name:?}, which declares none or has no Version\n{}",
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
            .details("board")
            .resource("lower_res")
            .standing
            .as_deref(),
        Some("lower"),
        "the harness's own check"
    );
    assert_attention(
        &dashboard.list().listed("board"),
        "missing-access",
        "the list before the Grants are given",
    );
    s.grant("lower_res", "staff", "write");
    s.grant("none_res", "staff", "read");
    let page = dashboard.details("board");
    for name in ["lower_res", "none_res"] {
        let resource = page.resource(name);
        assert_eq!(
            resource.standing.as_deref(),
            Some("enough"),
            "expected {name:?} `enough` at the next request once alice's Group holds enough, got {resource:?}"
        );
    }
    assert_eq!(page.resource("lower_res").yours.as_deref(), Some("write"));
    // What is left is `ghost_res`, not registered: the list's mark says so.
    assert_attention(
        &dashboard.list().listed("board"),
        "not-registered",
        "the list once alice holds enough on every registered Resource",
    );
}

#[test]
fn a_resources_contact_is_escaped_in_the_attribute_and_in_the_words_and_is_not_on_the_list() {
    let s = world();
    let hostile = "Ops <ops@example.test> & \"Quinn\" 'Q'";
    let mut body = resource_body("hostile_res", &s.db, &s.db.reader);
    body["contact"] = serde_json::json!(hostile);
    s.register_body("hostile_res", &body);
    create(&s.vm, &alice(), "asks", None);
    deploy(&s.vm, &alice(), "asks", &[("hostile_res", "read")]);
    let dashboard = Dashboard::new(&s.vm, &alice());
    let page = dashboard.details("asks");
    let resource = page.resource("hostile_res");
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

    let list = dashboard.list();
    let html = list.response.text();
    assert!(
        !html.contains("ops@example.test") && !html.contains("Quinn"),
        "expected the contact nowhere on the list, escaped or not: it is on the Application's page\n{html}"
    );
    assert_attention(&list.listed("asks"), "missing-access", "the list, asks");
}
