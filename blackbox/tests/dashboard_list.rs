//! The Dashboard's list (CONTRACT.md, "The Dashboard: The list"), with the
//! stand-in sign-in: exactly the Applications a person contributes to, then
//! those shared with them (by their id, by a Group, with everyone), each
//! group by name, and nothing else; `data-vm-relation`, `data-vm-serving`
//! and the link that opens an Application; its description and its
//! Contributors (the first 20, and how many more); pages of 100; and the
//! same Applications as `GET /v1/applications` and `vm applications`.
//!
//! Acceptance criteria: "A signed-in person sees the Applications they
//! contribute to and those shared with them, and no others"; "Each
//! Application shows name, description, Contributors and declared
//! Resources"; "The Dashboard and the tool's list of Applications agree
//! for the same person".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeMap;

use blackbox::dashboard::{
    CONTRIBUTORS_LISTED, Dashboard, Listed, PAGE_SIZE, administrator, create, deploy,
    stand_in_options, start,
};
use blackbox::sharing::{add, assert_opens, contributors_ok, everyone, group, share_ok, user};
use blackbox::tool::Machine;
use blackbox::{User, Vm, VmOptions};
use serde_json::Value;

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol").in_groups(&["finance"])
}

/// Shares `application` as `owner` with these entries.
fn share(vm: &Vm, owner: &User, application: &str, entries: &[Value]) {
    share_ok(&vm.api_as(owner), vm, application, &add(entries));
}

/// Alice contributes to `alpha-notes` and `zeta-notes` (her own) and to
/// `team-board` (by her Group `staff`); bob's `by-person` is shared with
/// her, `by-group` with `staff` and `everyone-board` with everyone. Bob's
/// `hidden-vault` (shared with nobody), `carols-desk` (shared with carol)
/// and `finance-only` (shared with the Group `finance`) are not hers.
fn alices_world(vm: &Vm) {
    create(vm, &alice(), "zeta-notes", None);
    create(vm, &alice(), "alpha-notes", None);
    create(vm, &bob(), "team-board", None);
    contributors_ok(
        &vm.api_as(&bob()),
        vm,
        "team-board",
        &add(&[group("staff")]),
    );
    create(vm, &bob(), "by-person", None);
    share(vm, &bob(), "by-person", &[user("alice")]);
    create(vm, &bob(), "by-group", None);
    share(vm, &bob(), "by-group", &[group("staff")]);
    create(vm, &bob(), "everyone-board", None);
    share(vm, &bob(), "everyone-board", &[everyone()]);
    create(vm, &bob(), "hidden-vault", Some("vault-secret-description"));
    create(vm, &bob(), "carols-desk", Some("carols-secret-description"));
    share(vm, &bob(), "carols-desk", &[user("carol")]);
    create(vm, &bob(), "finance-only", None);
    share(vm, &bob(), "finance-only", &[group("finance")]);
}

/// The names and relations of a list, in order.
fn relations(listed: &[Listed]) -> Vec<(String, String)> {
    listed
        .iter()
        .map(|a| (a.name.clone(), a.relation.clone().unwrap_or_default()))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(n, r)| ((*n).to_owned(), (*r).to_owned()))
        .collect()
}

#[test]
fn the_list_holds_what_they_contribute_to_then_what_is_shared_with_them_each_by_name_and_nothing_else()
 {
    let vm = start();
    alices_world(&vm);
    let page = Dashboard::new(&vm, &alice()).list();
    assert_eq!(
        relations(&page.applications()),
        pairs(&[
            ("alpha-notes", "contributor"),
            ("team-board", "contributor"),
            ("zeta-notes", "contributor"),
            ("by-group", "shared"),
            ("by-person", "shared"),
            ("everyone-board", "shared"),
        ]),
        "expected the Applications alice is a Contributor of (by her id or her Group), then those she can open besides (shared with her, her Group, everyone), each group by name, and no other\n{}",
        page.describe()
    );
    let html = page.response.text();
    for hidden in [
        "hidden-vault",
        "vault-secret-description",
        "carols-desk",
        "carols-secret-description",
        "finance-only",
    ] {
        assert!(
            !html.contains(hidden),
            "expected nothing of an Application alice cannot open on her page, but it holds {hidden:?}\n{html}"
        );
    }

    let bobs = Dashboard::new(&vm, &bob()).list();
    assert_eq!(
        relations(&bobs.applications()),
        pairs(&[
            ("by-group", "contributor"),
            ("by-person", "contributor"),
            ("carols-desk", "contributor"),
            ("everyone-board", "contributor"),
            ("finance-only", "contributor"),
            ("hidden-vault", "contributor"),
            ("team-board", "contributor"),
        ]),
        "expected bob's own Applications, and none of alice's (shared with nobody)\n{}",
        bobs.describe()
    );

    let carols = Dashboard::new(&vm, &carol()).list();
    assert_eq!(
        relations(&carols.applications()),
        pairs(&[
            ("carols-desk", "shared"),
            ("everyone-board", "shared"),
            ("finance-only", "shared"),
        ]),
        "expected what is shared with carol, with her Group `finance` and with everyone\n{}",
        carols.describe()
    );
}

#[test]
fn a_platform_administrators_list_is_their_own() {
    let vm = start();
    alices_world(&vm);
    create(&vm, &administrator(), "admin-tools", None);
    let page = Dashboard::new(&vm, &administrator()).list();
    assert_eq!(
        relations(&page.applications()),
        pairs(&[("admin-tools", "contributor"), ("everyone-board", "shared")]),
        "expected a Platform administrator's list to be their own, as anyone's (\"What this does not close\")\n{}",
        page.describe()
    );
}

#[test]
fn each_application_shows_its_serving_version_and_a_link_that_opens_it_only_when_one_serves() {
    let vm = start();
    create(&vm, &alice(), "board", Some("The incident board"));
    deploy(&vm, &alice(), "board", &[]);
    deploy(&vm, &alice(), "board", &[]);
    create(&vm, &alice(), "draft", None);
    let dashboard = Dashboard::new(&vm, &alice());
    let page = dashboard.list();

    let board = page.listed("board");
    assert_eq!(
        board.serving.as_deref(),
        Some("2"),
        "expected `data-vm-serving` the serving Version's number\n{}",
        page.describe()
    );
    assert_eq!(
        board.open_href(),
        Some(dashboard.open_href("board")),
        "expected `a[data-vm-open]` whose `href` is the Application's address in full, with the path `/`\n{}",
        page.describe()
    );
    assert_eq!(
        board.description(),
        Some("The incident board"),
        "expected its description in the element `data-vm-description`"
    );
    assert!(
        board.text.contains("board"),
        "expected the element to show the Application's name, got {:?}",
        board.text
    );

    let draft = page.listed("draft");
    assert_eq!(
        draft.serving.as_deref(),
        Some("none"),
        "expected `data-vm-serving=\"none\"` for an Application no Version serves\n{}",
        page.describe()
    );
    assert_eq!(
        draft.open_href(),
        None,
        "expected no `a[data-vm-open]` when no Version serves"
    );
    assert!(
        draft.text.to_lowercase().contains("deployed"),
        "expected words saying that nothing is deployed yet, got {:?}",
        draft.text
    );
    assert_eq!(
        draft.description(),
        None,
        "expected no `data-vm-description` for an Application with no description"
    );

    for name in ["board", "draft"] {
        let listed = page.listed(name);
        let details = listed
            .details_href()
            .unwrap_or_else(|| panic!("expected `a[data-vm-details]` in {name:?}"));
        let at = page.url.join(&details).unwrap();
        assert!(
            at.host_str() == Some("dashboard.vm.test")
                && at.path() == format!("/applications/{name}"),
            "expected `a[data-vm-details]` to lead to /applications/{name} on the Dashboard, got {details:?}"
        );
    }
    // The link opens it.
    assert_opens(&vm, &alice(), "board", "the Application the link names");
}

#[test]
fn the_link_to_open_is_the_applications_address_as_a_browser_reaches_it() {
    for (more, wanted) in [
        (vec![], "https://board.vm.test/"),
        (
            vec!["--public-scheme", "http", "--public-port", "8080"],
            "http://board.vm.test:8080/",
        ),
        (
            vec!["--public-scheme", "https", "--public-port", "8443"],
            "https://board.vm.test:8443/",
        ),
    ] {
        let vm = Vm::start(&VmOptions::new().stand_in_sign_in().args_extra(&more));
        create(&vm, &alice(), "board", None);
        deploy(&vm, &alice(), "board", &[]);
        let page = Dashboard::new(&vm, &alice()).list();
        assert_eq!(
            page.listed("board").open_href().as_deref(),
            Some(wanted),
            "with {more:?}: expected the link to be `<scheme>://<name>.<domain>[:<port>]/`, the port left out when it is the scheme's own\n{}",
            page.describe()
        );
    }
}

#[test]
fn contributors_are_shown_in_the_order_of_the_list_the_first_twenty_and_how_many_more() {
    let vm = start();
    create(&vm, &alice(), "crowded", None);
    let mut entries = vec![group("ops-team")];
    let ids: Vec<String> = (1..=23).map(|i| format!("u{i:02}")).collect();
    entries.extend(ids.iter().map(|id| user(id)));
    contributors_ok(&vm.api_as(&alice()), &vm, "crowded", &add(&entries));
    create(&vm, &alice(), "pair", None);
    contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        "pair",
        &add(&[group("night-shift")]),
    );

    let mut all = vec!["user:alice".to_owned(), "group:ops-team".to_owned()];
    all.extend(ids.iter().map(|id| format!("user:{id}")));
    assert_eq!(all.len(), 25, "the harness's own count");

    let dashboard = Dashboard::new(&vm, &alice());
    let page = dashboard.list();
    let crowded = page.listed("crowded");
    assert_eq!(
        crowded.contributor_keys(),
        all[..CONTRIBUTORS_LISTED].to_vec(),
        "expected the first 20 Contributors in the order of the list, each `data-vm-contributor` with `data-vm-id`\n{}",
        page.describe()
    );
    assert_eq!(
        crowded.more_contributors,
        vec!["5".to_owned()],
        "expected one element `data-vm-more-contributors=\"5\"`\n{}",
        page.describe()
    );
    for shown in &crowded.contributors {
        let id = shown.id.clone().unwrap_or_default();
        assert!(
            shown.text.contains(&id),
            "expected each Contributor's text to show their id or the Group, got {shown:?}"
        );
    }
    let pair = page.listed("pair");
    assert_eq!(
        pair.contributor_keys(),
        vec!["user:alice".to_owned(), "group:night-shift".to_owned()],
        "{}",
        page.describe()
    );
    assert!(
        pair.more_contributors.is_empty(),
        "expected no `data-vm-more-contributors` for 2 Contributors"
    );

    let details = dashboard.details("crowded");
    let shown = details.listed("crowded");
    assert_eq!(
        shown.contributor_keys(),
        all,
        "expected every Contributor on the Application's own page\n{}",
        details.describe()
    );
    assert!(
        shown.more_contributors.is_empty()
            && details
                .root()
                .with_attr("data-vm-more-contributors")
                .is_empty(),
        "expected no `data-vm-more-contributors` on the Application's own page"
    );
}

/// `c-000`, `c-001`, ...
fn numbered(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}-{i:03}")).collect()
}

#[test]
fn the_list_is_paged_by_100_with_links_to_the_next_and_the_previous_page() {
    let vm = start();
    let pager = User::new("pager");
    let owner = User::new("owner");
    let own = numbered("c", 60);
    let shared = numbered("s", 45);
    for name in &own {
        create(&vm, &pager, name, None);
    }
    for name in &shared {
        create(&vm, &owner, name, None);
        share(&vm, &owner, name, &[user("pager")]);
    }
    let all: Vec<String> = own.iter().chain(shared.iter()).cloned().collect();
    let dashboard = Dashboard::new(&vm, &pager);

    let first = dashboard.list();
    assert_eq!(
        first.names(),
        all[..PAGE_SIZE].to_vec(),
        "expected the first page to hold the first 100 of the order: the 60 contributed to, then 40 of those shared\n{}",
        first.describe()
    );
    assert_eq!(
        first.link("data-vm-previous"),
        None,
        "expected no `a[data-vm-previous]` on the first page"
    );
    let next = first.link("data-vm-next").unwrap_or_else(|| {
        panic!(
            "expected `a[data-vm-next]` when there are more\n{}",
            first.describe()
        )
    });
    assert!(
        next.host_str() == Some("dashboard.vm.test") && next.path() == "/",
        "expected the next page on the Dashboard's `/`, got {next}"
    );

    let second = dashboard.page(&blackbox::browser::target_of(&next), 200, "the next page");
    assert_eq!(
        second.names(),
        all[PAGE_SIZE..].to_vec(),
        "expected the second page to hold the rest, in the same order\n{}",
        second.describe()
    );
    assert_eq!(
        second.link("data-vm-next"),
        None,
        "expected no `a[data-vm-next]` on the last page"
    );
    let previous = second.link("data-vm-previous").unwrap_or_else(|| {
        panic!(
            "expected `a[data-vm-previous]` from the second page on\n{}",
            second.describe()
        )
    });
    let back = dashboard.page(
        &blackbox::browser::target_of(&previous),
        200,
        "the previous page",
    );
    assert_eq!(
        back.names(),
        first.names(),
        "the previous page is the first"
    );

    assert_eq!(
        dashboard.page("/?page=1", 200, "?page=1").names(),
        first.names(),
        "expected `?page=1` to be the first page"
    );
    assert_eq!(
        dashboard.page("/?page=2", 200, "?page=2").names(),
        second.names(),
        "expected `?page=2` to be the second page"
    );
    for past in ["/?page=3", "/?page=10000"] {
        let page = dashboard.page(past, 200, past);
        assert!(
            page.names().is_empty(),
            "{past}: expected a page past the last to be `200` with no Application\n{}",
            page.describe()
        );
        assert_eq!(page.link("data-vm-next"), None, "{past}: no next page");
    }
    assert_eq!(
        dashboard
            .every_listed()
            .into_iter()
            .map(|a| a.name)
            .collect::<Vec<_>>(),
        all,
        "expected every Application once, page after page"
    );
}

#[test]
fn exactly_100_applications_are_one_page_with_no_link_to_a_next() {
    let vm = start();
    let pager = User::new("pager");
    let names = numbered("app", PAGE_SIZE);
    for name in &names {
        create(&vm, &pager, name, None);
    }
    let page = Dashboard::new(&vm, &pager).list();
    assert_eq!(page.names(), names, "{}", page.describe());
    assert_eq!(
        page.link("data-vm-next"),
        None,
        "expected no `a[data-vm-next]` when 100 Applications are all there are\n{}",
        page.describe()
    );
}

#[test]
fn a_page_that_is_not_a_whole_number_from_1_to_10000_is_400() {
    let vm = start();
    create(&vm, &alice(), "board", None);
    let dashboard = Dashboard::new(&vm, &alice());
    for page in [
        "0",
        "-1",
        "10001",
        "abc",
        "1.5",
        "",
        "1e2",
        "%201",
        "99999999999999999999999",
    ] {
        let target = format!("/?page={page}");
        let answer = dashboard.page(
            &target,
            400,
            &format!("{target}: a `page` that is not a whole number from 1 to 10000"),
        );
        assert!(
            answer.names().is_empty(),
            "{target}: expected no Application on the `400`\n{}",
            answer.describe()
        );
    }
    for page in ["1", "10000"] {
        dashboard.page(&format!("/?page={page}"), 200, &format!("?page={page}"));
    }
}

#[test]
fn someone_with_no_application_is_told_nothing_is_shared_with_them_yet_and_that_the_vm_tool_makes_one()
 {
    let vm = start();
    create(&vm, &alice(), "board", None);
    let page = Dashboard::new(&vm, &User::new("dave")).list();
    assert!(
        page.names().is_empty(),
        "expected no Application for dave\n{}",
        page.describe()
    );
    let text = page.text().to_lowercase();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    assert!(
        text.contains("shared") && words.contains(&"vm"),
        "expected words saying that nothing is shared with the person yet and that an Application is made with the `vm` tool, got {text:?}"
    );
}

/// What a list says of each Application: (contributor, serving Version or
/// none, description), by name.
type Facts = BTreeMap<String, (bool, Option<u64>, Option<String>)>;

fn facts_of_listed(listed: &[Listed]) -> Facts {
    listed
        .iter()
        .map(|a| {
            let contributor = match a.relation.as_deref() {
                Some("contributor") => true,
                Some("shared") => false,
                other => panic!(
                    "expected `data-vm-relation` contributor or shared in {:?}, got {other:?}",
                    a.name
                ),
            };
            let serving = match a.serving.as_deref() {
                Some("none") => None,
                Some(n) => Some(
                    n.parse()
                        .unwrap_or_else(|_| panic!("`data-vm-serving` {n:?} is not a number")),
                ),
                None => panic!("expected `data-vm-serving` in {:?}", a.name),
            };
            (
                a.name.clone(),
                (contributor, serving, a.description().map(str::to_owned)),
            )
        })
        .collect()
}

fn facts_of_json(list: &Value) -> Facts {
    list["applications"]
        .as_array()
        .unwrap_or_else(|| panic!("expected {{ applications: [...] }}, got {list}"))
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap_or_default().to_owned(),
                (
                    a["contributor"]
                        .as_bool()
                        .unwrap_or_else(|| panic!("`contributor` in {a}")),
                    a["serving_version"].as_u64(),
                    a["description"].as_str().map(str::to_owned),
                ),
            )
        })
        .collect()
}

#[test]
fn the_dashboard_and_the_tools_list_agree_for_the_same_person() {
    let vm = Vm::start(&stand_in_options());
    alices_world(&vm);
    deploy(&vm, &alice(), "alpha-notes", &[]);
    deploy(&vm, &bob(), "by-group", &[]);
    deploy(&vm, &bob(), "by-group", &[]);
    create(&vm, &bob(), "described", Some("Shared with a description"));
    share(&vm, &bob(), "described", &[user("alice")]);
    for person in [alice(), bob(), carol(), User::new("dave")] {
        let listed = Dashboard::new(&vm, &person).every_listed();
        let dashboard = facts_of_listed(&listed);
        let api = vm.api_as(&person).list_applications();
        api.expect_status(200);
        assert_eq!(
            dashboard,
            facts_of_json(&api.json()),
            "{}: expected the Dashboard to show exactly the Applications `GET /v1/applications` answers, saying the same of each which of the two it is",
            person.id
        );
        let machine = Machine::new();
        machine.sign_in(&vm, &person);
        let printed = machine.run(&["applications", "--json"]).ok_json();
        assert_eq!(
            dashboard,
            facts_of_json(&printed),
            "{}: expected the Dashboard and `vm applications` to show the same Applications to the same person, and say the same of each which of the two it is",
            person.id
        );
    }
}
