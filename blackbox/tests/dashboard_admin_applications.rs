//! The list of every Application on the Dashboard (CONTRACT.md, "The
//! Dashboard for Platform administrators: Every Application"), with the
//! stand-in sign-in: exactly what `GET /v1/admin/applications` answers, a
//! page of it (one never shared, one with no Version, one disabled, each
//! once, by name), paged forward by `a[data-vm-next]` over more than 100
//! with `a[data-vm-first]` and no link back; each Application one element
//! with `data-vm-serving`, `data-vm-requests`, `data-vm-users` and, when
//! disabled, `data-vm-disabled`, and no `data-vm-relation` or
//! `data-vm-attention`; what is seen first (the name as `a[data-vm-details]`,
//! at most one mark, the usage, the description) and what is one step away
//! in a `<details>` (the Versions, the Declared access with
//! `data-vm-declared-more`, the Contributors, whom it is shared with
//! counted, who disabled it, who made it); no address and no
//! `a[data-vm-open]`; "in the last 30 days" said once; a malformed `after`
//! `400`; none, in words.
//!
//! Acceptance criterion (ticket "The Dashboard for Platform
//! administrators"): "A Platform administrator sees every Application with
//! its usage, its Contributors and its Declared access in the browser."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{disable_ok, list_page, place_usage};
use blackbox::admin_pages::APPLICATIONS;
use blackbox::dashboard::{Dashboard, Page, administrator, create, deploy, start};
use blackbox::logs::{from_to, go_back};
use blackbox::page::Element;
use blackbox::sharing::{add, contributors_ok, everyone, group, keys, share_ok, user};
use blackbox::{DOMAIN, User, Vm};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn carol() -> User {
    User::new("carol")
}

/// Five Applications:
/// - `board`, alice's, described, Versions 1 and 2 with Version 1 serving
///   (gone back), declaring `devops` read and `ghost` write, Contributors
///   alice, the Group staff and carol, shared with everyone, used by two
///   Users;
/// - `draft`, alice's, no Version, shared with nobody;
/// - `frozen`, alice's, a Version declaring nothing, shared with the Group
///   finance and bob and dave, disabled;
/// - `secret`, carol's, a Version, never shared;
/// - `stopped`, carol's, no Version, disabled.
fn world() -> Vm {
    let vm = start();
    create(&vm, &alice(), "board", Some("The incident board"));
    deploy(
        &vm,
        &alice(),
        "board",
        &[("devops", "read"), ("ghost", "write")],
    );
    deploy(
        &vm,
        &alice(),
        "board",
        &[("devops", "read"), ("ghost", "write")],
    );
    go_back(&vm.api_as(&alice()), "board", &from_to(2, None)).expect_status(200);
    contributors_ok(
        &vm.api_as(&alice()),
        &vm,
        "board",
        &add(&[group("staff"), user("carol")]),
    );
    share_ok(&vm.api_as(&alice()), &vm, "board", &add(&[everyone()]));
    place_usage(&vm, "board", 0, "bob", 7);
    place_usage(&vm, "board", 3, "dave", 5);
    place_usage(&vm, "board", 40, "erin", 100);
    create(&vm, &alice(), "draft", None);
    create(&vm, &alice(), "frozen", Some("Frozen for audit"));
    deploy(&vm, &alice(), "frozen", &[]);
    share_ok(
        &vm.api_as(&alice()),
        &vm,
        "frozen",
        &add(&[group("finance"), user("bob"), user("dave")]),
    );
    disable_ok(&vm, "frozen");
    create(&vm, &carol(), "secret", None);
    deploy(&vm, &carol(), "secret", &[]);
    create(&vm, &carol(), "stopped", None);
    disable_ok(&vm, "stopped");
    vm
}

/// The element of `name` on the page; fails unless there is exactly one.
fn element<'a>(page: &'a Page, name: &str) -> Element<'a> {
    let found = page.root().with_attr_value("data-vm-application", name);
    match found.as_slice() {
        [one] => *one,
        _ => panic!(
            "expected exactly one element `data-vm-application={name:?}`, found {}\n{}",
            found.len(),
            page.describe()
        ),
    }
}

/// The text of the element before its `<details>`: what is seen first.
fn seen_first(e: &Element<'_>) -> String {
    let text = e.spoken();
    let details: Vec<String> = e.named("details").iter().map(Element::spoken).collect();
    details.iter().fold(text, |t, d| {
        if d.is_empty() {
            t
        } else {
            t.replacen(d.as_str(), "", 1)
        }
    })
}

/// The one `<details>` of the element.
fn details_of<'a>(page: &Page, e: &Element<'a>, name: &str) -> Element<'a> {
    let details = e.named("details");
    match details.as_slice() {
        [one] => *one,
        _ => panic!(
            "expected one `<details>` in the element of {name:?}, found {}\n{}",
            details.len(),
            page.describe()
        ),
    }
}

#[test]
fn the_page_is_exactly_what_the_api_answers_one_page_of() {
    let vm = world();
    let api = list_page(&vm, "");
    let page = Dashboard::new(&vm, &administrator()).page(APPLICATIONS, 200, "every Application");
    let api_names: Vec<String> = api["applications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        api_names,
        ["board", "draft", "frozen", "secret", "stopped"],
        "the harness's own check of the API's answer"
    );
    assert_eq!(
        page.names(),
        api_names,
        "expected every Application, each once, by name, as the API answers them\n{}",
        page.describe()
    );
    for application in api["applications"].as_array().unwrap() {
        let name = application["name"].as_str().unwrap();
        let e = element(&page, name);
        let context = format!("the element of {name:?}");
        let serving = application["serving_version"]
            .as_u64()
            .map_or_else(|| "none".to_owned(), |n| n.to_string());
        assert_eq!(
            e.attr("data-vm-serving"),
            Some(serving.as_str()),
            "{context}: data-vm-serving"
        );
        assert_eq!(
            e.attr("data-vm-requests"),
            Some(application["usage"]["requests"].to_string().as_str()),
            "{context}: data-vm-requests is usage.requests ({application})"
        );
        assert_eq!(
            e.attr("data-vm-users"),
            Some(application["usage"]["users"].to_string().as_str()),
            "{context}: data-vm-users is usage.users"
        );
        assert_eq!(
            e.attr("data-vm-disabled"),
            application["disabled"]["since"].as_str(),
            "{context}: data-vm-disabled is disabled.since, absent when not disabled"
        );
        for absent in ["data-vm-relation", "data-vm-attention"] {
            assert!(
                !e.has(absent),
                "{context}: expected no `{absent}` on the list of every Application\n{}",
                page.describe()
            );
        }
        // The name is the link to the Application's page.
        let details_links = e.with_attr("data-vm-details");
        assert!(
            matches!(details_links.as_slice(), [a] if a.name() == "a"
                && a.attr("href").and_then(|h| page.url.join(h).ok()).is_some_and(|u| u.path() == format!("/applications/{name}"))
                && a.text().contains(name)),
            "{context}: expected its name as the one link `a[data-vm-details]` to /applications/{name}\n{}",
            page.describe()
        );
        let description: Vec<String> = e
            .with_attr("data-vm-description")
            .iter()
            .map(Element::text)
            .collect();
        match application["description"].as_str() {
            Some(d) => assert_eq!(description, [d], "{context}: its description"),
            None => assert!(description.is_empty(), "{context}: no description element"),
        }
        assert!(
            e.with_attr("data-vm-open").is_empty(),
            "{context}: expected no `a[data-vm-open]`: opening is for those it is shared with\n{}",
            page.describe()
        );
    }
    let text = page.body_text();
    for name in ["board", "draft", "frozen", "secret", "stopped"] {
        let address = format!("{name}.{DOMAIN}");
        assert!(
            !text.contains(&address),
            "expected no address as text on the list of every Application, found {address:?}\n{}",
            page.describe()
        );
    }
}

#[test]
fn what_is_seen_first_is_the_name_one_mark_at_most_the_usage_and_the_description() {
    let vm = world();
    let page = Dashboard::new(&vm, &administrator()).page(APPLICATIONS, 200, "every Application");
    for (name, mark) in [
        ("board", None),
        ("draft", Some("Not deployed")),
        ("frozen", Some("Disabled")),
        ("secret", None),
        ("stopped", Some("Disabled")),
    ] {
        let e = element(&page, name);
        let first = seen_first(&e);
        let lower = first.to_lowercase();
        let has_disabled = lower.contains("disabled");
        let has_not_deployed = lower.contains("not deployed");
        assert_eq!(
            (has_disabled, has_not_deployed),
            (mark == Some("Disabled"), mark == Some("Not deployed")),
            "{name}: expected the mark {mark:?} beside the name and no other (\"Disabled\" before \"Not deployed\"); what is seen first says {first:?}"
        );
        assert!(
            first.contains(name),
            "{name}: expected the name seen first, it says {first:?}"
        );
    }
    // The usage, as numbers seen first; the words "in the last 30 days"
    // and the day `usage.from` once, over the list.
    let api = list_page(&vm, "");
    let board = &api["applications"][0];
    let first = seen_first(&element(&page, "board"));
    for number in [
        board["usage"]["requests"].to_string(),
        board["usage"]["users"].to_string(),
    ] {
        assert!(
            first
                .split(|c: char| !c.is_ascii_digit())
                .any(|w| w == number),
            "expected the usage number {number} seen first in board's element, it says {first:?}"
        );
    }
    assert_eq!(
        (
            board["usage"]["requests"].as_u64(),
            board["usage"]["users"].as_u64()
        ),
        (Some(12), Some(2)),
        "the harness's own check: usage within the 30 days only"
    );
    let text = page.body_text().to_lowercase();
    assert_eq!(
        text.matches("in the last 30 days").count(),
        1,
        "expected \"in the last 30 days\" said once on the page\n{}",
        page.describe()
    );
    let from = board["usage"]["from"].as_str().unwrap();
    assert_eq!(
        page.body_text().matches(from).count(),
        1,
        "expected the day usage.from ({from}) said once, over the list\n{}",
        page.describe()
    );
    for name in ["board", "draft", "frozen", "secret", "stopped"] {
        let e = element(&page, name);
        assert!(
            !e.text().to_lowercase().contains("30 days") && !e.text().contains(from),
            "{name}: expected the period not said in each element, it says {:?}",
            e.text()
        );
    }
}

#[test]
fn one_step_away_are_the_versions_declared_access_contributors_sharing_and_who_disabled_and_made_it()
 {
    let vm = world();
    let api = list_page(&vm, "");
    let page = Dashboard::new(&vm, &administrator()).page(APPLICATIONS, 200, "every Application");
    for application in api["applications"].as_array().unwrap() {
        let name = application["name"].as_str().unwrap();
        let context = format!("the details of {name:?}");
        let e = element(&page, name);
        let details = details_of(&page, &e, name);
        let summaries = details.named("summary");
        assert!(
            matches!(summaries.as_slice(), [s] if s.spoken().contains(name)),
            "{context}: expected one `<summary>` naming the Application for a reader who hears it\n{}",
            page.describe()
        );
        let text = details.text();
        // Declared access.
        let declared: Vec<(String, String)> = details
            .with_attr("data-vm-declared-resource")
            .iter()
            .map(|r| {
                (
                    r.attr("data-vm-declared-resource")
                        .unwrap_or_default()
                        .to_owned(),
                    r.attr("data-vm-declared").unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let wanted: Vec<(String, String)> = application["declared_access"]["resources"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v["access"].as_str().unwrap().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            declared,
            wanted,
            "{context}: its Declared access, each Resource with its level\n{}",
            page.describe()
        );
        for (resource, level) in &wanted {
            assert!(
                text.contains(resource.as_str()) && text.contains(level.as_str()),
                "{context}: expected the Resource {resource} and its level {level} in words, got {text:?}"
            );
        }
        assert_eq!(
            details.with_attr("data-vm-declared-more").len(),
            0,
            "{context}: declared_more is 0: no element `data-vm-declared-more`"
        );
        // Contributors.
        let contributors: Vec<String> = details
            .with_attr("data-vm-contributor")
            .iter()
            .map(|c| {
                format!(
                    "{}:{}",
                    c.attr("data-vm-contributor").unwrap_or_default(),
                    c.attr("data-vm-id").unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            contributors,
            keys(&application["contributors"]),
            "{context}: every Contributor, in the order of the list"
        );
        for c in details.with_attr("data-vm-contributor") {
            assert!(
                !c.has("data-vm-user"),
                "{context}: a Contributor carries no `data-vm-user` (said once)"
            );
        }
        // Whom it is shared with, counted.
        let counts = details.with_attr("data-vm-shared-count");
        let shared = &application["shared_with"];
        assert!(
            matches!(counts.as_slice(), [c] if c.attr("data-vm-everyone") == Some(&shared["everyone"].to_string())
                && c.attr("data-vm-groups") == Some(&shared["groups"].to_string())
                && c.attr("data-vm-people") == Some(&shared["people"].to_string())),
            "{context}: expected one `data-vm-shared-count` saying {shared}\n{}",
            page.describe()
        );
        assert_eq!(
            details.with_attr("data-vm-shared").len(),
            0,
            "{context}: whom it is shared with is counted here, not listed"
        );
        // Who disabled it.
        let by = details.with_attr("data-vm-disabled-by");
        match application["disabled"]["by"].as_str() {
            Some(who) => assert!(
                matches!(by.as_slice(), [b] if b.attr("data-vm-disabled-by") == Some(who) && b.text().contains(who)),
                "{context}: expected `data-vm-disabled-by={who:?}` holding the id\n{}",
                page.describe()
            ),
            None => assert!(
                by.is_empty(),
                "{context}: not disabled, no `data-vm-disabled-by`"
            ),
        }
        // Who made it.
        let creator = application["created_by"].as_str().unwrap();
        assert!(
            text.contains(creator),
            "{context}: expected who made it ({creator}) one step away, got {text:?}"
        );
    }
    // The Versions: board serves 1, its latest is 2; both are said.
    let board = details_of(&page, &element(&page, "board"), "board").text();
    let numbers: Vec<&str> = board
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .collect();
    assert!(
        numbers.contains(&"1") && numbers.contains(&"2"),
        "expected board's serving Version (1) and latest (2) in its details, got {board:?}"
    );
    // Shared with nobody: words saying only its Contributors open it.
    let secret = details_of(&page, &element(&page, "secret"), "secret")
        .text()
        .to_lowercase();
    assert!(
        secret.contains("contributor"),
        "expected words saying that only its Contributors open secret, got {secret:?}"
    );
    let board = details_of(&page, &element(&page, "board"), "board")
        .text()
        .to_lowercase();
    assert!(
        board.contains("everyone"),
        "expected board shared with everyone said, got {board:?}"
    );
}

#[test]
fn declared_more_is_said_when_more_than_200_resources_are_declared() {
    let vm = start();
    let names: Vec<String> = (0..203).map(|i| format!("res_{i:03}")).collect();
    let declared: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "read")).collect();
    create(&vm, &alice(), "wide", None);
    deploy(&vm, &alice(), "wide", &declared);
    let api = list_page(&vm, "");
    assert_eq!(
        api["applications"][0]["declared_more"], 3,
        "the harness's own check"
    );
    let page = Dashboard::new(&vm, &administrator()).page(APPLICATIONS, 200, "every Application");
    let e = element(&page, "wide");
    assert_eq!(e.with_attr("data-vm-declared-resource").len(), 200);
    let more = e.with_attr("data-vm-declared-more");
    assert!(
        matches!(more.as_slice(), [m] if m.attr("data-vm-declared-more") == Some("3") && m.text().contains('3')),
        "expected `data-vm-declared-more=\"3\"` saying how many more\n{}",
        page.describe()
    );
}

#[test]
fn more_than_100_are_paged_forward_by_next_with_a_link_to_the_first() {
    let vm = start();
    let mut names: Vec<String> = (0..205).map(|i| format!("app-{i:03}")).collect();
    for name in &names {
        create(&vm, &alice(), name, None);
    }
    names.sort();
    let dashboard = Dashboard::new(&vm, &administrator());
    let mut page = dashboard.page(APPLICATIONS, 200, "the first page");
    assert!(
        page.link("data-vm-first").is_none(),
        "no `a[data-vm-first]` on the first page"
    );
    let mut seen: Vec<String> = Vec::new();
    let mut query = String::new();
    for round in 0..10 {
        assert!(
            page.count("data-vm-previous") == 0,
            "round {round}: there is no link to the page before\n{}",
            page.describe()
        );
        let api = list_page(&vm, &query);
        let api_names: Vec<String> = api["applications"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            page.names(),
            api_names,
            "round {round}: the API's page{query}"
        );
        seen.extend(page.names());
        match api["next"].as_str() {
            Some(next) => {
                let link = page.link("data-vm-next").unwrap_or_else(|| {
                    panic!(
                        "round {round}: expected `a[data-vm-next]`\n{}",
                        page.describe()
                    )
                });
                assert_eq!(
                    (link.path(), blackbox::admin_pages::query_pairs(&link)),
                    (APPLICATIONS, vec![("after".to_owned(), next.to_owned())]),
                    "round {round}: `a[data-vm-next]` leads to {APPLICATIONS}?after=<next>"
                );
                query = format!("?after={next}");
                page = dashboard.page(&blackbox::browser::target_of(&link), 200, "a next page");
                let first = page.link("data-vm-first");
                assert!(
                    first
                        .as_ref()
                        .is_some_and(|f| f.path() == APPLICATIONS && f.query().is_none()),
                    "a later page has `a[data-vm-first]` to {APPLICATIONS}, got {first:?}"
                );
            }
            None => {
                assert!(
                    page.link("data-vm-next").is_none(),
                    "the last page has no `a[data-vm-next]`"
                );
                break;
            }
        }
    }
    assert_eq!(
        seen, names,
        "following `a[data-vm-next]` shows every Application once"
    );
}

#[test]
fn a_malformed_after_is_400_saying_so_with_a_link_to_the_first_page() {
    let vm = world();
    let dashboard = Dashboard::new(&vm, &administrator());
    let long = "a".repeat(64);
    for query in [
        "?after=Not_A_Name".to_owned(),
        "?after=".to_owned(),
        "?after=board&after=draft".to_owned(),
        format!("?after={long}"),
        "?after=-board".to_owned(),
    ] {
        let target = format!("{APPLICATIONS}{query}");
        let page = dashboard.page(&target, 400, &target);
        let errors = page.errors();
        assert!(
            matches!(errors.as_slice(), [(r, t)] if r == "malformed" && !t.is_empty()),
            "{target}: expected one `data-vm-error=\"malformed\"` with words, got {errors:?}"
        );
        let first = page.link("data-vm-first");
        assert!(
            first
                .as_ref()
                .is_some_and(|f| f.path() == APPLICATIONS && f.query().is_none()),
            "{target}: expected `a[data-vm-first]` to {APPLICATIONS}, got {first:?}"
        );
        assert_eq!(
            page.count("data-vm-application"),
            0,
            "{target}: no Application listed"
        );
    }
    // An `after` an Application could be named is taken, whether one is;
    // other parameters are ignored.
    let page = dashboard.page(&format!("{APPLICATIONS}?after=c"), 200, "after c");
    assert_eq!(page.names(), ["draft", "frozen", "secret", "stopped"]);
    let page = dashboard.page(
        &format!("{APPLICATIONS}?limit=1&page=2&x=y"),
        200,
        "other parameters",
    );
    assert_eq!(
        page.names(),
        ["board", "draft", "frozen", "secret", "stopped"]
    );
}

#[test]
fn with_no_application_the_page_says_so_and_how_one_is_made() {
    let vm = start();
    let page = Dashboard::new(&vm, &administrator()).page(APPLICATIONS, 200, "no Application");
    assert_eq!(page.count("data-vm-application"), 0);
    let text = page.body_text().to_lowercase();
    assert!(
        text.contains("no application") && text.contains("builder") && text.contains("vibe"),
        "expected words saying the installation has no Application yet and that a Builder makes one with the `vibe` tool, got {text:?}"
    );
    assert!(page.link("data-vm-next").is_none());
}

#[test]
fn the_platform_administrators_own_list_is_unchanged() {
    let vm = world();
    // The administrator contributes to nothing and nothing is shared with
    // them but board (everyone): their own list is theirs.
    let page = Dashboard::new(&vm, &administrator()).list();
    let listed: Vec<(String, Option<String>)> = page
        .applications()
        .into_iter()
        .map(|a| (a.name, a.relation))
        .collect();
    assert_eq!(
        listed,
        vec![("board".to_owned(), Some("shared".to_owned()))],
        "expected the administrator's own list to be theirs, as anyone's\n{}",
        page.describe()
    );
}
