//! The Application a deploy goes to, and the order in which a deploy looks
//! at what it needs (CONTRACT.md, "Deploying: The command", steps 1 to 4,
//! "What this does not close"; ticket "Deploy: a Version built on the
//! Contributor's machine goes live": someone who is not a Contributor
//! cannot deploy to an existing Application).
//!
//! The API has no way yet to add a second Contributor to an Application
//! (CONTRACT.md, "The platform's API: Applications": the creator is its
//! first Contributor, and nothing adds another), so a second Contributor's
//! deploy is not tested here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::api::marked_bundle;
use blackbox::deploy::{
    DEPLOY_TIMEOUT, Deployed, MANIFEST_DESCRIPTION, commit_all, dana, deploy, deploy_args,
    deploy_in_words, deploy_project, deploy_with_env, init_repository, path_without_git,
    path_without_node, project_tree, signed_in, tool_path,
};
use blackbox::project::{manifest_ts, type_errors, write_checked};
use blackbox::tool::Machine;
use blackbox::{User, Vm, VmOptions};
use serde_json::json;

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn erin() -> User {
    User::new("erin").in_groups(&["elsewhere"])
}

#[test]
fn the_first_deploy_creates_the_application_with_the_manifests_description() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-creates", "created-board");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert!(deployed.created);
    assert_eq!(deployed.version, 1);
    let application = vm.api_as(&dana()).application("created-board");
    assert_eq!(application["description"], json!(MANIFEST_DESCRIPTION));
    assert_eq!(
        blackbox::sharing::keys(&application["contributors"]),
        ["user:dana"]
    );
    assert_eq!(application["created_by"], json!("dana"));
    assert_eq!(application["serving_version"], json!(1));
    // The page answers.
    let page = vm.get_as(&dana(), "created-board", "/");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert!(page.text().contains("hello dana"), "{}", page.describe());
}

#[test]
fn a_later_change_of_the_description_is_not_sent() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-description", "described-board");
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    project.write(
        "manifest.ts",
        &manifest_ts("described-board", "{}").replace(MANIFEST_DESCRIPTION, "Said otherwise."),
    );
    commit_all(&project.dir, "another description");
    let second = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(second.version, 2);
    assert!(!second.created);
    assert_eq!(
        vm.api_as(&dana()).application("described-board")["description"],
        json!(MANIFEST_DESCRIPTION),
        "expected the description of the first deploy kept"
    );
}

#[test]
fn a_name_taken_by_an_application_the_user_does_not_contribute_to_is_refused() {
    let vm = vm();
    let machine = signed_in(&vm);
    let erins = vm.api_as(&erin());
    // One with a Version, one created only.
    erins.create_with_bundle("taken-board", &marked_bundle("erin's"));
    erins.create_application("bare-board").expect_status(201);
    for name in ["taken-board", "bare-board"] {
        let project = deploy_project(&format!("deploy-taken-{name}"), name);
        let before = project_tree(&project.dir);
        let versions = erins.versions(name);
        let run = deploy(&machine, &project.dir, &[]);
        let message = run.json_error(3, "not-a-contributor");
        assert!(
            message.contains("erin"),
            "{name}: expected the message to name its Contributors (erin), got {message:?}"
        );
        let lower = message.to_lowercase();
        assert!(
            lower.contains("ask"),
            "{name}: expected the message to say to ask one of them, got {message:?}"
        );
        assert!(
            lower.contains("name") && message.contains("manifest.ts"),
            "{name}: expected the message to say to choose another `name` in manifest.ts, got {message:?}"
        );
        deploy_in_words(&machine, &project.dir, &[]).text_error(3);
        assert_eq!(
            erins.versions(name),
            versions,
            "{name}: expected no Version added"
        );
        assert_eq!(
            blackbox::sharing::keys(&erins.application(name)["contributors"]),
            ["user:erin"]
        );
        assert_eq!(
            before.differences(&project_tree(&project.dir)),
            Vec::<String>::new()
        );
    }
}

#[test]
fn the_command_line_comes_first_then_the_project_then_the_sign_in() {
    let vm = vm();
    let signed = signed_in(&vm);
    let not_signed = Machine::new();
    let empty = not_signed.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let empty_text = empty.to_str().unwrap();

    // The command line: 2, on a directory that is no project, not signed in.
    let run = not_signed.run_into_files(
        &deploy_args(empty_text, &["--timeout", "0"]),
        &[("PATH", &tool_path(&not_signed))],
        DEPLOY_TIMEOUT,
    );
    assert_eq!(run.code, Some(2), "{}", run.describe());
    // The project before the sign-in.
    deploy(&not_signed, &empty, &[]).json_error(1, "not-a-project");
    deploy(&signed, &empty, &[]).json_error(1, "not-a-project");
    // Without --project: the current directory, which is no project.
    signed
        .run_into_files(
            &["deploy", "--json"],
            &[("PATH", &tool_path(&signed))],
            DEPLOY_TIMEOUT,
        )
        .json_error(1, "not-a-project");
    // The sign-in before git: a project in no repository.
    let plain = not_signed.root().join("plain");
    write_checked(&plain);
    let message = deploy(&not_signed, &plain, &[]).json_error(4, "not-signed-in");
    assert!(message.contains("vm sign-in"), "{message:?}");
}

#[test]
fn git_comes_before_node_and_the_packages() {
    let vm = vm();
    let machine = signed_in(&vm);
    let plain = machine.root().join("plain");
    write_checked(&plain);
    let nothing = machine.root().join("nothing");
    fs::create_dir_all(&nothing).unwrap();
    // No git and no node: git-missing.
    deploy_with_env(
        &machine,
        &plain,
        &[],
        &[("PATH", nothing.to_str().unwrap())],
    )
    .json_error(1, "git-missing");
    // No repository, no node: not-a-repository.
    deploy_with_env(
        &machine,
        &plain,
        &[],
        &[("PATH", &path_without_node(&machine))],
    )
    .json_error(1, "not-a-repository");
    // Uncommitted changes, no node: uncommitted-changes.
    let project = deploy_project("deploy-order-uncommitted", "order-board");
    project.write("notes.txt", "untracked\n");
    deploy_with_env(
        &machine,
        &project.dir,
        &[],
        &[("PATH", &path_without_node(&machine))],
    )
    .json_error(1, "uncommitted-changes");
    // The control: path_without_git has node, and still git-missing.
    deploy_with_env(
        &machine,
        &project.dir,
        &[],
        &[("PATH", &path_without_git(&machine))],
    )
    .json_error(1, "git-missing");
}

#[test]
fn node_comes_before_the_packages_and_the_packages_before_the_application() {
    let vm = vm();
    let machine = signed_in(&vm);
    vm.api_as(&erin())
        .create_application("taken-board")
        .expect_status(201);
    // Outside this repository, so that nothing resolves; committed.
    let bare = machine.root().join("bare");
    write_checked(&bare);
    fs::write(bare.join("manifest.ts"), manifest_ts("taken-board", "{}")).unwrap();
    init_repository(&bare);
    commit_all(&bare, "first");
    let message = deploy_with_env(
        &machine,
        &bare,
        &[],
        &[("PATH", &path_without_node(&machine))],
    )
    .json_error(1, "node-missing");
    assert!(
        message.contains("Node") && message.contains("22"),
        "expected the message to say Node 22 or later is needed, got {message:?}"
    );
    let message = deploy(&machine, &bare, &[]).json_error(1, "not-installed");
    assert!(
        message.contains("npm install"),
        "expected the message to say to run `npm install`, got {message:?}"
    );
}

#[test]
fn the_application_comes_before_the_pull_and_the_checks() {
    let vm = vm();
    let machine = signed_in(&vm);
    vm.api_as(&erin())
        .create_application("taken-board")
        .expect_status(201);
    // A project with a type error, declaring a Resource that is not
    // registered: the deploy refuses for the name before it checks.
    let project = deploy_project("deploy-order-application", "taken-board");
    project.write("src/wrong.ts", &type_errors(1));
    project.write(
        "manifest.ts",
        &manifest_ts(
            "taken-board",
            "{ nosuch: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    commit_all(&project.dir, "broken");
    deploy(&machine, &project.dir, &[]).json_error(3, "not-a-contributor");
}
