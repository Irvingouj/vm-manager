//! From a new project (CONTRACT.md, "Deploying: From a new project", "What
//! it prints", "The instruction file"; ticket "Deploy: a Version built on
//! the Contributor's machine goes live": from a fresh template project,
//! deploy puts the Application at its address, where a signed-in User gets
//! the Application's response; the output names who can see the
//! Application, its Contributors, by name; the Version records the commit).
//!
//! A project made by `vm new` under the repository's `target/` (so that its
//! packages resolve from the root's `node_modules`, as if installed), in a
//! repository of its own, committed, deployed by a machine signed in as
//! dana.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::deploy::{
    Deployed, commit_all, dana, deploy, deploy_in_words, head, init_repository, project_tree,
    signed_in,
};
use blackbox::project::{
    code_in, help_commands, html_text, new_project, new_project_in_repository, paragraphs,
    squashed, vm_commands_in,
};
use blackbox::sdk::run_tooling;
use blackbox::sharing::keys;
use blackbox::tool::Machine;
use blackbox::{DOMAIN, Vm, VmOptions};
use serde_json::json;

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// The template's `src/index.ts` with its greeting changed to `Welcome
/// back, <id>.`: a committed change of the page.
fn changed_page(template: &str) -> String {
    assert!(
        template.contains("<p>Hello, "),
        "the harness's own check: the template's page greets with `Hello, `\n{template}"
    );
    template.replace("<p>Hello, ", "<p>Welcome back, ")
}

#[test]
fn a_new_project_deploys_as_it_stands_and_a_signed_in_user_gets_its_page() {
    let vm = vm();
    let machine = signed_in(&vm);
    let name = "hello-board";
    let project = new_project_in_repository(&machine, name);
    init_repository(&project.dir);
    let commit = commit_all(&project.dir, "A new project");
    let before = project_tree(&project.dir);

    let run = deploy(&machine, &project.dir, &[]);
    let deployed = Deployed::of(&run);
    // `visible_to` holds entries (CONTRACT.md, "Contributors and sharing:
    // The tool"), whose fields beside the one that names them are not this
    // test's: they are compared by what they name.
    assert_eq!(
        keys(&deployed.body["visible_to"]["contributors"]),
        ["user:dana"],
        "expected dana, the one who deployed, as its only Contributor\n{}",
        run.describe()
    );
    assert_eq!(
        deployed.shared_with,
        Vec::<serde_json::Value>::new(),
        "expected a new Application to be shared with nobody\n{}",
        run.describe()
    );
    let mut body = deployed.body;
    body["visible_to"] = json!(null);
    assert_eq!(
        body,
        json!({
            "application": name,
            "address": format!("{name}.{DOMAIN}"),
            "version": 1,
            "created": true,
            "git_commit": commit,
            "migrations_applied": [],
            "structures": {},
            "pulled": [],
            "visible_to": null,
            "warnings": [],
            "notes": [],
        }),
        "expected exactly the contract's object for a new project's first deploy\n{}",
        run.describe()
    );
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new(),
        "expected the deploy to write nothing in the project"
    );

    // The Application: the Manifest's description, dana its Contributor.
    let manifest = run_tooling("manifest", &project.dir).json().unwrap();
    let api = vm.api_as(&dana());
    let application = api.application(name);
    assert_eq!(application["description"], manifest["description"]);
    assert_eq!(keys(&application["contributors"]), ["user:dana"]);
    assert_eq!(application["created_by"], json!("dana"));
    assert_eq!(application["serving_version"], json!(1));
    // The Version: the commit, no structure, no migration.
    let version = api.get_version(name, 1).expect_status(200).json();
    assert_eq!(version["git_commit"], json!(commit));
    assert_eq!(version["structures"], json!({}));
    assert_eq!(version["declared_access"], json!({ "resources": {} }));
    assert_eq!(version["created_by"], json!("dana"));

    // The page, at <name>.<domain>.
    let page = vm.get_as(&dana(), name, "/");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert!(
        page.header("content-type")
            .is_some_and(|t| t.starts_with("text/html")),
        "{}",
        page.describe()
    );
    assert!(
        html_text(&page.text()).contains("Hello, dana."),
        "expected the template's page greeting dana\n{}",
        page.describe()
    );

    // A committed change of the page: Version 2, the new page.
    let index = project.dir.join("src/index.ts");
    let template = fs::read_to_string(&index).unwrap();
    fs::write(&index, changed_page(&template)).unwrap();
    let second_commit = commit_all(&project.dir, "Another greeting");
    let second = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(second.version, 2);
    assert!(!second.created, "expected `created` false the second time");
    assert_eq!(second.git_commit, second_commit);
    assert_eq!(keys(&json!(second.contributors)), ["user:dana"]);
    assert_eq!(second.migrations_applied, Vec::<String>::new());
    let page = vm.get_as(&dana(), name, "/");
    assert!(
        html_text(&page.text()).contains("Welcome back, dana."),
        "expected the new page after the second deploy\n{}",
        page.describe()
    );
    let versions = api.versions(name);
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1]["git_commit"], json!(second_commit));
    assert_eq!(api.application(name)["serving_version"], json!(2));
}

#[test]
fn visible_to_is_the_contributors_as_the_installation_answers_them() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = new_project_in_repository(&machine, "seen-by");
    init_repository(&project.dir);
    commit_all(&project.dir, "first");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let application = vm.api_as(&dana()).application("seen-by");
    assert_eq!(
        keys(&json!(deployed.contributors)),
        keys(&application["contributors"])
    );
    assert_eq!(
        keys(&json!(deployed.shared_with)),
        keys(&application["shared_with"])
    );
}

#[test]
fn in_words_it_says_where_it_is_live_and_that_only_its_contributors_see_it() {
    let vm = vm();
    let machine = signed_in(&vm);
    let name = "hello-words";
    let project = new_project_in_repository(&machine, name);
    init_repository(&project.dir);
    commit_all(&project.dir, "first");
    let run = deploy_in_words(&machine, &project.dir, &[]);
    let text = run.ok_text();
    let flat = squashed(&text);
    let lower = flat.to_lowercase();
    assert!(
        flat.contains(&format!("{name}.{DOMAIN}")),
        "expected the words to give the address\n{text}"
    );
    assert!(
        lower.contains("live") && lower.contains("version 1"),
        "expected the words to say it is live as Version 1\n{text}"
    );
    assert!(
        flat.contains("dana"),
        "expected the words to name its Contributor, dana\n{text}"
    );
    assert!(
        lower.contains("contributor") && lower.contains("only"),
        "expected the words to say it is visible to its Contributors only\n{text}"
    );
    // From the ticket on Contributors and sharing on: Contributors only, and
    // that `vm share` shares it.
    assert!(
        flat.contains("vm share"),
        "expected the words to say that `vm share` shares it\n{text}"
    );
    assert!(
        !lower.contains("team"),
        "expected the words never to say more than the Contributors (no \"team\")\n{text}"
    );
    // A second deploy says Version 2.
    let index = project.dir.join("src/index.ts");
    let template = fs::read_to_string(&index).unwrap();
    fs::write(&index, changed_page(&template)).unwrap();
    commit_all(&project.dir, "second");
    let text = deploy_in_words(&machine, &project.dir, &[]).ok_text();
    assert!(
        squashed(&text).to_lowercase().contains("version 2"),
        "expected the words to say Version 2\n{text}"
    );
    assert_eq!(head(&project.dir).len(), 40);
}

#[test]
fn vm_help_lists_deploy_and_its_own_help_names_its_options() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    assert!(
        commands.iter().any(|c| c == "deploy"),
        "expected `vm --help` to list `deploy`, got {commands:?}"
    );
    let own = machine.run(&["deploy", "--help"]);
    own.expect_code(0);
    for option in ["--project", "--allow-data-loss", "--timeout", "--json"] {
        assert!(
            own.stdout.contains(option),
            "expected `vm deploy --help` to name {option}\n{}",
            own.describe()
        );
    }
}

#[test]
fn agents_md_names_vm_deploy_and_how_a_deploy_goes() {
    let machine = Machine::new();
    let dir = machine.root().join("new");
    new_project(&machine, "incident-notes", &dir);
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        text.lines().count() <= 120,
        "expected AGENTS.md at most 120 lines"
    );
    let commands = vm_commands_in(&text);
    assert!(
        commands.iter().any(|c| c == "deploy"),
        "expected AGENTS.md to name `vm deploy` in code, got {commands:?}\n{text}"
    );
    let listed = help_commands(&machine.run(&["--help"]).stdout);
    for command in &commands {
        assert!(
            listed.contains(command),
            "expected every `vm <command>` of AGENTS.md to be one `vm --help` lists; {command:?} is not ({listed:?})"
        );
    }
    let flat = squashed(&text).to_lowercase();
    for gone in [
        "no deploy command",
        "has no deploy",
        "there is no deploy",
        "cannot deploy yet",
    ] {
        assert!(
            !flat.contains(gone),
            "expected AGENTS.md no longer to say there is no deploy command ({gone:?})\n{text}"
        );
    }
    let paragraphs: Vec<String> = paragraphs(&text)
        .iter()
        .map(|p| squashed(p).to_lowercase())
        .collect();
    let any = |what: &str, words: &[&str]| {
        assert!(
            paragraphs
                .iter()
                .any(|p| p.contains("deploy") && words.iter().all(|w| p.contains(w))),
            "expected a paragraph of AGENTS.md about deploying that says {what} (words {words:?})\n{text}"
        );
    };
    any("a deploy needs everything committed", &["commit"]);
    any(
        "a destroying migration is asked about again at deploy",
        &["again", "data"],
    );
    assert!(
        paragraphs
            .iter()
            .any(|p| p.contains("delete") && p.contains("truncate") && p.contains("never")),
        "expected AGENTS.md to say a migration written by hand removes rows with DELETE and never with TRUNCATE\n{text}"
    );
    any(
        "who can see the Application is told from the deploy's own output",
        &["own output", "see"],
    );
    assert!(
        code_in(&text).iter().any(|c| c
            .split_whitespace()
            .collect::<Vec<_>>()
            .starts_with(&["vm", "deploy"])),
        "expected `vm deploy` in a code span"
    );
}
