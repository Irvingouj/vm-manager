//! The harness's own checks for the tests of deploying (`blackbox/src/
//! deploy.rs`): `git` works on a machine whose `HOME` is the test's own;
//! each test project has a repository of its own, which `git` run in it
//! finds rather than this repository; a project in a sub-folder of a
//! repository is in that repository; the `PATH`s that lack `git` or `node`
//! lack them; the project that type-checks and does not bundle does both;
//! the project's tree leaves `.git` out. These pass before `vm deploy`
//! exists.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::deploy::{
    IMPORTS_NOWHERE_TS, NOWHERE_DTS, commit_all, deploy_project, git_binary, head, init_repository,
    machine_git, path_without_git, path_without_node, project_tree, status, toplevel,
};
use blackbox::project::{checked_project, manifest_ts, write_checked};
use blackbox::sdk::{TempProject, repository_root};
use blackbox::structure::type_check;
use blackbox::tool::Machine;

#[test]
fn harness_git_runs_on_the_machine_and_finds_the_projects_own_repository() {
    let machine = Machine::new();
    let project = deploy_project("harness-deploy-git", "harness-git");
    let dir = project.dir.canonicalize().unwrap();
    let seen = machine_git(
        &machine,
        &project.dir,
        &["rev-parse", "--show-toplevel", "HEAD"],
    );
    assert_eq!(seen.code, Some(0), "{}", seen.describe());
    let lines: Vec<&str> = seen.stdout.lines().collect();
    assert_eq!(
        lines
            .first()
            .map(|l| std::path::PathBuf::from(l).canonicalize().unwrap()),
        Some(dir),
        "expected git, run as on the machine, to find the project's own repository, not {}\n{}",
        repository_root().display(),
        seen.describe()
    );
    assert_eq!(lines.get(1).copied(), Some(head(&project.dir).as_str()));
    let clean = machine_git(&machine, &project.dir, &["status", "--porcelain"]);
    assert_eq!(clean.code, Some(0), "{}", clean.describe());
    assert_eq!(clean.stdout, "", "expected everything committed");
    assert_ne!(
        toplevel(&project.dir).canonicalize().unwrap(),
        repository_root().canonicalize().unwrap()
    );
    assert!(git_binary().is_file());
}

#[test]
fn harness_a_folder_with_no_repository_is_in_none() {
    let machine = Machine::new();
    let dir = machine.root().join("plain");
    write_checked(&dir);
    let seen = machine_git(&machine, &dir, &["rev-parse", "--show-toplevel"]);
    assert_ne!(
        seen.code,
        Some(0),
        "expected a folder of the machine's to be in no repository\n{}",
        seen.describe()
    );
    init_repository(&dir);
    let no_commit = machine_git(&machine, &dir, &["rev-parse", "--verify", "HEAD"]);
    assert_ne!(
        no_commit.code,
        Some(0),
        "expected a repository with no commit to have no HEAD\n{}",
        no_commit.describe()
    );
}

#[test]
fn harness_a_project_in_a_sub_folder_is_in_its_parents_repository() {
    let root = TempProject::new("harness-deploy-sub");
    let app = root.dir.join("app");
    write_checked(&app);
    fs::write(app.join("manifest.ts"), manifest_ts("sub-app", "{}")).unwrap();
    root.write("README.md", "The repository's own file.\n");
    init_repository(&root.dir);
    let commit = commit_all(&root.dir, "first");
    assert_eq!(
        toplevel(&app).canonicalize().unwrap(),
        root.dir.canonicalize().unwrap()
    );
    assert_eq!(head(&app), commit);
    root.write("README.md", "Changed.\n");
    root.write("other.txt", "Untracked.\n");
    let porcelain = status(&root.dir);
    assert!(
        porcelain.contains("README.md") && porcelain.contains("other.txt"),
        "{porcelain}"
    );
}

#[test]
fn harness_the_paths_lack_what_they_say() {
    let machine = Machine::new();
    let no_git = std::path::PathBuf::from(path_without_git(&machine));
    assert!(no_git.join("node").exists() && !no_git.join("git").exists());
    let no_node = std::path::PathBuf::from(path_without_node(&machine));
    assert!(no_node.join("git").exists() && !no_node.join("node").exists());
}

#[test]
fn harness_the_project_that_type_checks_and_does_not_bundle() {
    let project = checked_project("harness-deploy-build");
    project.write("src/nowhere.d.ts", NOWHERE_DTS);
    project.write("src/index.ts", IMPORTS_NOWHERE_TS);
    let checked = type_check(&project.dir);
    assert_eq!(checked.code, Some(0), "{}", checked.describe());
    let bundled = project.bundle();
    assert_ne!(
        bundled.code,
        Some(0),
        "expected the SDK's tooling not to bundle it\n{}",
        bundled.describe()
    );
    assert!(
        format!("{}{}", bundled.stdout, bundled.stderr).contains("vm-harness-nowhere"),
        "{}",
        bundled.describe()
    );
}

#[test]
fn harness_the_projects_tree_leaves_git_out() {
    let project = deploy_project("harness-deploy-tree", "harness-tree");
    let before = project_tree(&project.dir);
    assert!(before.paths().iter().all(|p| !p.starts_with(".git")));
    // git refreshing its index changes nothing of the tree.
    status(&project.dir);
    project.write("new.txt", "x\n");
    commit_all(&project.dir, "second");
    let after = project_tree(&project.dir);
    assert_eq!(before.differences(&after), vec!["new.txt: new".to_owned()]);
}
