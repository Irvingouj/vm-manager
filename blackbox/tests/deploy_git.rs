//! What a deploy asks of Git (CONTRACT.md, "Deploying: The command", step
//! 2; ticket "Deploy: a Version built on the Contributor's machine goes
//! live": uncommitted changes are refused; the Version records the
//! commit). `git` on the `PATH` (`git-missing`); the project inside a
//! repository with a commit (`not-a-repository`); nothing of the project's
//! folder uncommitted: no file changed, staged, removed or untracked
//! (ignored files do not count), else `uncommitted-changes` with the files
//! relative to the project, at most 50 and `more`, and nothing sent. The
//! Version's commit is `HEAD`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::deploy::{
    Deployed, commit_all, dana, deploy, deploy_in_words, deploy_project, deploy_with_env, git_ok,
    head, init_repository, path_without_git, project_tree, signed_in,
};
use blackbox::project::{manifest_ts, write_checked};
use blackbox::sdk::TempProject;
use blackbox::tool::Run;
use blackbox::{Vm, VmOptions};
use serde_json::{Value, json};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// Fails unless the Application `name` was not created: nothing was sent.
fn assert_not_created(vm: &Vm, name: &str, context: &str) {
    let answer = vm.api_as(&dana()).get_application(name);
    assert_eq!(
        answer.status,
        404,
        "{context}: expected nothing sent (no Application {name:?} created), got\n{}",
        answer.describe()
    );
}

/// Fails unless the run is `uncommitted-changes`, exit code `1`, with
/// exactly these `files` (in any order) and no `more`, and a message that
/// says to commit and deploy again; returns the error.
fn assert_uncommitted(run: &Run, files: &[&str], context: &str) -> Value {
    let message = run.json_error(1, "uncommitted-changes");
    let error = run.json()["error"].clone();
    let mut got: Vec<String> = error["files"]
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected `files` an array\n{}", run.describe()))
        .iter()
        .map(|f| f.as_str().unwrap_or_default().to_owned())
        .collect();
    got.sort();
    let mut want: Vec<String> = files.iter().map(|f| (*f).to_owned()).collect();
    want.sort();
    assert_eq!(
        got,
        want,
        "{context}: expected `files` relative to the project\n{}",
        run.describe()
    );
    assert!(
        error.get("more").is_none(),
        "{context}: expected no `more` with {} files\n{}",
        files.len(),
        run.describe()
    );
    let lower = message.to_lowercase();
    assert!(
        lower.contains("commit") && lower.contains("deploy"),
        "{context}: expected the message to say to commit them and deploy again, got {message:?}"
    );
    error
}

#[test]
fn without_git_on_the_path_it_ends_git_missing_and_sends_nothing() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-git-missing", "no-git");
    let before = project_tree(&project.dir);
    let path = path_without_git(&machine);
    let run = deploy_with_env(&machine, &project.dir, &[], &[("PATH", &path)]);
    let message = run.json_error(1, "git-missing");
    assert!(
        message.contains("git"),
        "expected the message to say git is needed, got {message:?}"
    );
    machine
        .run_into_files(
            &["deploy", "--project", project.dir.to_str().unwrap()],
            &[("PATH", &path)],
            blackbox::deploy::DEPLOY_TIMEOUT,
        )
        .text_error(1);
    assert_not_created(&vm, "no-git", "git-missing");
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn a_project_in_no_repository_or_in_one_with_no_commit_is_not_a_repository() {
    let vm = vm();
    let machine = signed_in(&vm);
    // A folder of the machine's, in no repository.
    let dir = machine.root().join("plain");
    write_checked(&dir);
    fs::write(dir.join("manifest.ts"), manifest_ts("plain-board", "{}")).unwrap();
    let run = deploy(&machine, &dir, &[]);
    run.json_error(1, "not-a-repository");
    deploy_in_words(&machine, &dir, &[]).text_error(1);
    // A repository with no commit.
    init_repository(&dir);
    deploy(&machine, &dir, &[]).json_error(1, "not-a-repository");
    assert_not_created(&vm, "plain-board", "not-a-repository");
}

#[test]
fn each_kind_of_uncommitted_change_is_refused_with_its_files_and_nothing_sent() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-uncommitted", "uncommitted-board");
    let dir = &project.dir;
    let reset = |dir: &Path| {
        git_ok(dir, &["reset", "-q", "--hard"]);
        git_ok(dir, &["clean", "-q", "-f", "-d"]);
    };
    let index = dir.join("src/index.ts");

    // A file changed.
    fs::write(&index, "// changed\nexport {};\n").unwrap();
    assert_uncommitted(&deploy(&machine, dir, &[]), &["src/index.ts"], "changed");
    let words = deploy_in_words(&machine, dir, &[]).text_error(1);
    assert!(
        words.contains("src/index.ts"),
        "expected the words to name the file, got {words:?}"
    );
    reset(dir);

    // A file changed and staged.
    fs::write(&index, "// staged\nexport {};\n").unwrap();
    git_ok(dir, &["add", "src/index.ts"]);
    assert_uncommitted(&deploy(&machine, dir, &[]), &["src/index.ts"], "staged");
    reset(dir);

    // A file removed.
    fs::remove_file(dir.join("tsconfig.json")).unwrap();
    assert_uncommitted(&deploy(&machine, dir, &[]), &["tsconfig.json"], "removed");
    reset(dir);

    // A file removed and staged.
    git_ok(dir, &["rm", "-q", "tsconfig.json"]);
    assert_uncommitted(
        &deploy(&machine, dir, &[]),
        &["tsconfig.json"],
        "removed, staged",
    );
    reset(dir);

    // Files untracked: at the top and in a tracked folder.
    project.write("notes.txt", "mine\n");
    project.write("src/extra.ts", "export const extra = 1;\n");
    assert_uncommitted(
        &deploy(&machine, dir, &[]),
        &["notes.txt", "src/extra.ts"],
        "untracked",
    );
    reset(dir);

    assert_not_created(&vm, "uncommitted-board", "uncommitted changes");
    assert_eq!(
        blackbox::deploy::status(dir),
        "",
        "the harness's own check: everything is committed again"
    );
}

#[test]
fn ignored_files_do_not_count_and_the_version_records_head() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-ignored", "ignored-board");
    project.write(".gitignore", "*.log\nscratch/\n");
    let commit = commit_all(&project.dir, "ignore logs");
    project.write("debug.log", "ignored\n");
    project.write("scratch/notes.txt", "ignored too\n");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(deployed.git_commit, commit);
    assert_eq!(deployed.git_commit, head(&project.dir));
    let version = vm
        .api_as(&dana())
        .get_version("ignored-board", 1)
        .expect_status(200)
        .json();
    assert_eq!(
        version["git_commit"],
        json!(commit),
        "expected the Version's commit to be the repository's HEAD"
    );
}

#[test]
fn at_most_50_files_are_named_and_then_how_many_more() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-many", "many-board");
    let all: Vec<String> = (1..=53).map(|i| format!("extra-{i:02}.txt")).collect();
    for file in &all {
        project.write(file, "x\n");
    }
    let run = deploy(&machine, &project.dir, &[]);
    run.json_error(1, "uncommitted-changes");
    let error = run.json()["error"].clone();
    let files: Vec<String> = error["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        files.len(),
        50,
        "expected at most 50 files\n{}",
        run.describe()
    );
    let mut distinct = files.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 50, "expected 50 distinct files");
    assert!(
        files.iter().all(|f| all.contains(f)),
        "expected each one of the untracked files, by its path relative to the project\n{}",
        run.describe()
    );
    assert_eq!(
        error["more"],
        json!(3),
        "expected `more` 3\n{}",
        run.describe()
    );

    // Exactly 50: no `more`.
    for file in all.iter().skip(50) {
        fs::remove_file(project.dir.join(file)).unwrap();
    }
    let fifty: Vec<&str> = all.iter().take(50).map(String::as_str).collect();
    assert_uncommitted(&deploy(&machine, &project.dir, &[]), &fifty, "exactly 50");
    assert_not_created(&vm, "many-board", "many files");
}

#[test]
fn a_project_in_a_sub_folder_of_a_repository_counts_only_its_own_folder() {
    let vm = vm();
    let machine = signed_in(&vm);
    let root = TempProject::new("deploy-sub-folder");
    let app = root.dir.join("app");
    write_checked(&app);
    fs::write(app.join("manifest.ts"), manifest_ts("sub-board", "{}")).unwrap();
    root.write("README.md", "The repository's own file.\n");
    init_repository(&root.dir);
    let commit = commit_all(&root.dir, "first");

    // Outside the project's folder: changed and untracked. They do not count.
    root.write("README.md", "Changed outside the project.\n");
    root.write("other.txt", "Untracked outside the project.\n");
    let deployed = Deployed::of(&deploy(&machine, &app, &[]));
    assert_eq!(deployed.version, 1);
    assert_eq!(
        deployed.git_commit, commit,
        "expected the repository's HEAD"
    );

    // Inside it: named relative to the project, not to the repository.
    fs::write(app.join("src/index.ts"), "// changed\nexport {};\n").unwrap();
    assert_uncommitted(
        &deploy(&machine, &app, &[]),
        &["src/index.ts"],
        "a change inside the project's folder",
    );
    assert_eq!(vm.api_as(&dana()).versions("sub-board").len(), 1);
}

#[test]
fn uncommitted_files_are_each_named_as_they_are_in_the_order_of_their_paths() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-uncommitted-names", "names-board");
    // A folder git does not know: each of its files, not the folder.
    project.write("drafts/a.txt", "a\n");
    project.write("drafts/deeper/b.txt", "b\n");
    // Names with a space and with a letter that is not ASCII.
    project.write("with space.txt", "s\n");
    project.write("café.txt", "c\n");
    // A file renamed: both its names.
    git_ok(&project.dir, &["mv", "src/index.ts", "src/main.ts"]);
    let run = deploy(&machine, &project.dir, &[]);
    run.json_error(1, "uncommitted-changes");
    let error = run.json()["error"].clone();
    let files: Vec<String> = error["files"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `files`\n{}", run.describe()))
        .iter()
        .map(|f| f.as_str().unwrap().to_owned())
        .collect();
    let mut want: Vec<String> = [
        "café.txt",
        "drafts/a.txt",
        "drafts/deeper/b.txt",
        "src/index.ts",
        "src/main.ts",
        "with space.txt",
    ]
    .iter()
    .map(|f| (*f).to_owned())
    .collect();
    want.sort();
    assert_eq!(
        files,
        want,
        "expected each file by its own path, as it is (no quoting, no escapes), in the order of their paths\n{}",
        run.describe()
    );
    assert!(error.get("more").is_none());
    assert_not_created(&vm, "names-board", "names");
}

#[test]
fn a_repository_in_the_sha_256_object_format_is_not_a_repository() {
    let vm = vm();
    let machine = signed_in(&vm);
    let dir = TempProject::new("deploy-sha256");
    write_checked(&dir.dir);
    fs::write(dir.dir.join("manifest.ts"), manifest_ts("sha-board", "{}")).unwrap();
    let made = blackbox::deploy::git(&dir.dir, &["init", "-q", "--object-format=sha256"]);
    if made.code != Some(0) {
        eprintln!(
            "SKIPPED: this machine's git cannot make a SHA-256 repository\n{}",
            made.describe()
        );
        return;
    }
    let commit = commit_all(&dir.dir, "first");
    assert_eq!(
        commit.len(),
        64,
        "the harness's own check: a SHA-256 commit"
    );
    let before = project_tree(&dir.dir);
    let message = deploy(&machine, &dir.dir, &[]).json_error(1, "not-a-repository");
    assert!(!message.trim().is_empty());
    assert_not_created(&vm, "sha-board", "a SHA-256 repository");
    assert_eq!(
        before.differences(&project_tree(&dir.dir)),
        Vec::<String>::new()
    );
}
