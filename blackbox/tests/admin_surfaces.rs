//! What every surface shows of a disabled Application (CONTRACT.md, "What
//! Platform administrators see and stop: What every surface shows"): `vibe
//! applications` and `vibe show` in words and with `--json`, `vibe deploy`
//! and `vibe go-back` to a disabled Application (made, exit code `0`, the
//! `disabled` member and a warning `application-disabled`), and the
//! Dashboard's list and Application page (`data-vm-disabled`, no
//! `a[data-vm-open]`, the mark "Disabled"). How the pages look is not
//! tested here (`browser_look.rs`).
#![expect(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::{self, administrator, assert_disabled_member, disable_ok};
use blackbox::api::{marked_bundle, version_body};
use blackbox::dashboard::Dashboard;
use blackbox::deploy::{
    Deployed, commit_all, dana, deploy, deploy_in_words, deploy_project, signed_in,
};
use blackbox::project::{manifest_ts, squashed};
use blackbox::sharing::{TRACED, add, create_unshared, share_ok, user};
use blackbox::tool::{Machine, Run};
use blackbox::{DOMAIN, User, Vm};
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn signed_in_as(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

/// A program with `board` (alice's, shared with bob, disabled) and
/// `other` (alice's, not disabled); the disable's `disabled`.
fn with_disabled_board() -> (Vm, Value) {
    let vm = admin::start();
    create_unshared(&vm, &alice(), "board", TRACED);
    create_unshared(&vm, &alice(), "other", TRACED);
    share_ok(&vm.api_as(&alice()), &vm, "board", &add(&[user("bob")]));
    share_ok(&vm.api_as(&alice()), &vm, "other", &add(&[user("bob")]));
    let disabled = disable_ok(&vm, "board")["disabled"].clone();
    (vm, disabled)
}

#[test]
fn vm_applications_says_which_is_disabled() {
    let (vm, _) = with_disabled_board();
    for user in [alice(), bob()] {
        let machine = signed_in_as(&vm, &user);
        let printed = machine.run(&["applications", "--json"]).ok_json();
        assert_eq!(printed, vm.api_as(&user).list_applications().json());
        assert_eq!(printed["applications"][0]["disabled"], true, "{printed}");
        assert_eq!(printed["applications"][1]["disabled"], false, "{printed}");
        let words = machine.run(&["applications"]).ok_text();
        let board = block(&words, "board");
        let other = block(&words, "other");
        assert!(
            board.to_lowercase().contains("disabled")
                && board.to_lowercase().contains("platform administrator"),
            "expected the block of a disabled Application to say it is disabled by a Platform administrator\n{words}"
        );
        assert!(
            !other.to_lowercase().contains("disabled"),
            "the block of one that is not disabled says nothing of it\n{words}"
        );
    }
}

/// The lines of `words` from the one that names `name` first to the next
/// blank line.
fn block(words: &str, name: &str) -> String {
    let lines: Vec<&str> = words.lines().collect();
    let start = lines
        .iter()
        .position(|l| {
            l.split_whitespace()
                .any(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-') == name)
        })
        .unwrap_or_else(|| panic!("expected a block naming {name:?}\n{words}"));
    lines[start..]
        .iter()
        .take_while(|l| !l.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn vm_show_says_since_when_by_whom_and_that_an_administrator_enables_it() {
    let (vm, disabled) = with_disabled_board();
    let since = disabled["since"].as_str().unwrap();
    for user in [alice(), bob()] {
        let machine = signed_in_as(&vm, &user);
        let shown = machine.run(&["show", "board", "--json"]).ok_json();
        assert_eq!(shown, vm.api_as(&user).application("board"));
        assert_eq!(shown["disabled"], disabled, "{shown}");
        let words = machine.run(&["show", "board"]).ok_text();
        let lower = words.to_lowercase();
        assert!(
            lower.contains("disabled")
                && words.contains(since)
                && words.contains(&administrator().id)
                && lower.contains("platform administrator")
                && lower.contains("enable"),
            "expected `vibe show` to say it is disabled, since when, by whom, and that a Platform administrator enables it again\n{words}"
        );
        let other = machine.run(&["show", "other"]).ok_text();
        assert!(!other.to_lowercase().contains("disabled"), "{other}");
    }
}

/// Fails unless go-back's `--json` output has `disabled` after
/// `migrations_not_undone`, as the contract orders its members.
fn assert_disabled_after_migrations(run: &Run) {
    let migrations = run.stdout.find("\"migrations_not_undone\"");
    let disabled = run.stdout.find("\"disabled\"");
    assert!(
        matches!((migrations, disabled), (Some(m), Some(d)) if m < d),
        "expected `disabled` after `migrations_not_undone`\n{}",
        run.describe()
    );
}

#[test]
fn vm_go_back_on_an_application_that_is_not_disabled_has_disabled_null_and_no_warnings() {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("v1"));
    api.send_version("board", &version_body(&marked_bundle("v2")))
        .expect_status(201);
    let machine = signed_in_as(&vm, &alice());
    let run = machine.run(&["go-back", "board", "--json"]);
    let gone = run.ok_json();
    assert_eq!(
        gone,
        json!({ "application": "board", "address": format!("board.{DOMAIN}"),
                "serving_version": 1, "was": 2, "migrations_not_undone": [],
                "disabled": null }),
        "go-back's output on an Application that is not disabled, exactly (no `warnings`)\n{}",
        run.describe()
    );
    assert_disabled_after_migrations(&run);
}

#[test]
fn vm_go_back_on_a_disabled_application_is_made_and_warns() {
    let vm = admin::start();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("v1"));
    api.send_version("board", &version_body(&marked_bundle("v2")))
        .expect_status(201);
    let disabled = disable_ok(&vm, "board")["disabled"].clone();
    let machine = signed_in_as(&vm, &alice());
    let run = machine.run(&["go-back", "board", "--json"]);
    let gone = run.ok_json();
    assert_disabled_after_migrations(&run);
    let message = gone["warnings"][0]["message"].clone();
    assert_eq!(
        gone,
        json!({ "application": "board", "address": format!("board.{DOMAIN}"),
                "serving_version": 1, "was": 2, "migrations_not_undone": [],
                "disabled": disabled,
                "warnings": [ { "reason": "application-disabled", "message": message } ] }),
        "go-back's output on a disabled Application, exactly\n{}",
        run.describe()
    );
    assert_disabled_warning(&gone, &run.describe());
    assert_eq!(api.application("board")["serving_version"], 1);
    assert!(
        !api.application("board")["disabled"].is_null(),
        "a go-back does not enable"
    );
    // In words, the same; exit code 0.
    api.send_version("board", &version_body(&marked_bundle("v3")))
        .expect_status(201);
    let words = machine.run(&["go-back", "board"]).ok_text();
    let lower = squashed(&words).to_lowercase();
    assert!(
        lower.contains("disabled") && lower.contains("platform administrator"),
        "expected the words to warn that the Application is disabled\n{words}"
    );
}

/// Fails unless `printed` has a warning with reason `application-disabled`
/// whose message says it will not be served until a Platform
/// administrator enables the Application, and to tell the Builder.
fn assert_disabled_warning(printed: &Value, context: &str) {
    let warnings: Vec<&Value> = printed["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `warnings`, got {printed}\n{context}"))
        .iter()
        .filter(|w| w["reason"] == "application-disabled")
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "expected one warning `application-disabled`, got {printed}\n{context}"
    );
    let message = warnings[0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    for said in ["platform administrator", "enable", "builder"] {
        assert!(
            message.contains(said),
            "expected the warning to say {said:?}, got {message:?}\n{context}"
        );
    }
}

#[test]
fn vm_deploy_to_a_disabled_application_is_made_and_warns_and_never_says_it_is_live() {
    let vm = admin::start();
    let machine = signed_in(&vm);
    let project = deploy_project("admin-deploy-disabled", "parked");
    let first = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert!(
        first.body["disabled"].is_null(),
        "`disabled` is null when it is not disabled\n{}",
        first.run.describe()
    );
    assert!(first.warnings_for("application-disabled").is_empty());
    disable_ok(&vm, "parked");

    project.write(
        "manifest.ts",
        &manifest_ts("parked", "{}")
            .replace("A project for the tests", "Another word for the tests"),
    );
    commit_all(&project.dir, "a second Version");
    let run = deploy(&machine, &project.dir, &[]);
    let second = Deployed::of(&run);
    assert_eq!(second.version, 2);
    assert_disabled_member(
        &second.body["disabled"],
        &administrator().id,
        "the deploy's `disabled`",
    );
    assert_disabled_warning(&second.body, &run.describe());
    let read = vm.api_as(&dana()).application("parked");
    assert_eq!(
        read["serving_version"], 2,
        "the Version was made and serves once enabled"
    );
    assert_eq!(second.body["address"], format!("parked.{DOMAIN}"));
    let page = vm
        .request("parked", "/")
        .as_user(&dana())
        .header("accept", "application/json")
        .send();
    admin::assert_disabled_plain(&page, "parked", "after a deploy while disabled");

    project.write(
        "manifest.ts",
        &manifest_ts("parked", "{}")
            .replace("A project for the tests", "A third word for the tests"),
    );
    commit_all(&project.dir, "a third Version");
    let words = deploy_in_words(&machine, &project.dir, &[]);
    words.expect_code(0);
    let text = squashed(&words.stdout).to_lowercase();
    assert!(
        text.contains("disabled") && text.contains("platform administrator"),
        "expected the words to warn that the Application is disabled\n{}",
        words.describe()
    );
    assert!(
        !text.contains("live"),
        "the words never say the Application is live at its address while it is disabled\n{}",
        words.describe()
    );
    // Enabled, it serves the latest deploy.
    admin::enable_ok(&vm, "parked");
    let served = vm.get_as(&dana(), "parked", "/");
    assert_eq!(served.status, 200, "{}", served.describe());
}

#[test]
fn the_dashboard_marks_a_disabled_application_and_offers_no_way_to_open_it() {
    let (vm, disabled) = with_disabled_board();
    let since = disabled["since"].as_str().unwrap().to_owned();
    for user in [alice(), bob()] {
        let dashboard = Dashboard::new(&vm, &user);
        let list = dashboard.list();
        let root = list.root();
        let elements = root.with_attr_value("data-vm-application", "board");
        assert_eq!(elements.len(), 1, "{}", list.describe());
        let board = elements[0];
        assert!(
            board.has("data-vm-disabled"),
            "the list's element has data-vm-disabled\n{}",
            list.describe()
        );
        assert_eq!(
            board.attr("data-vm-serving"),
            Some("1"),
            "data-vm-serving is unchanged"
        );
        assert!(
            board.with_attr("data-vm-open").is_empty(),
            "no a[data-vm-open]\n{}",
            list.describe()
        );
        assert!(
            board.spoken().contains("Disabled"),
            "expected a mark a screen reader names \"Disabled\", got {:?}",
            board.spoken()
        );
        let other = root.with_attr_value("data-vm-application", "other");
        assert!(!other[0].has("data-vm-disabled"));
        assert_eq!(other[0].with_attr("data-vm-open").len(), 1);

        let page = dashboard.details("board");
        let root = page.root();
        let elements = root.with_attr_value("data-vm-application", "board");
        assert_eq!(elements.len(), 1, "{}", page.describe());
        assert_eq!(
            elements[0].attr("data-vm-disabled"),
            Some(since.as_str()),
            "the page's element has data-vm-disabled=\"<since>\"\n{}",
            page.describe()
        );
        assert!(
            root.with_attr("data-vm-open").is_empty(),
            "no a[data-vm-open]\n{}",
            page.describe()
        );
        let text = root.text();
        assert!(
            text.contains(&format!("board.{DOMAIN}")),
            "the address is still there, as text\n{text}"
        );
        assert!(
            text.to_lowercase().contains("platform administrator")
                && text.to_lowercase().contains("disabled"),
            "words saying that a Platform administrator disabled it\n{text}"
        );
        assert!(
            !page.text().contains(&administrator().id),
            "who disabled it is not on the Dashboard's pages\n{text}"
        );
    }
}
