//! The harness's own checks of what the tests of creating and checking a
//! project stand on (`blackbox/src/project.rs`), which hold today: the
//! project of the harness's own type-checks, its Manifest reads, and its
//! migrations are in step as `drizzle-kit` sees them; the shapes the tests
//! give `drizzle-kit` do what the tests expect of it; the fixtures that
//! start processes do; a tree snapshot sees every kind of change; whether a
//! process runs is told right, zombies included; a tarball is listed and
//! read; a stand-in `node` says it is Node 20.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::process::Command;
use std::time::Duration;

use blackbox::program::run_to_exit;
use blackbox::project::{
    CHECKED, EMPTY_SCHEMA_TS, NODE_20, NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS,
    NOTES_SCHEMA_TS, Tree, checked_project, generate_migrations, is_running, kill_all, pids_in,
    pulled_by_hand, schema_starting_a_child, sql_files, stand_ins, still_running_after, tar_list,
    tar_member, type_errors,
};
use blackbox::sdk::{TempProject, run_tooling};
use blackbox::structure::type_check;
use serde_json::json;

#[test]
fn harness_the_checked_project_type_checks_and_its_manifest_reads() {
    let project = checked_project("harness-checked");
    let checked = type_check(&project.dir);
    assert_eq!(checked.code, Some(0), "{}", checked.describe());
    let manifest = run_tooling("manifest", &project.dir);
    assert_eq!(manifest.code, Some(0), "{}", manifest.describe());
    assert_eq!(
        manifest.json().unwrap(),
        json!({
            "name": CHECKED,
            "description": "A project for the tests of vm check.",
            "declared_access": { "resources": {} }
        })
    );
    // With files written as `vm pull` writes them, it still type-checks.
    pulled_by_hand(&project, "inventory");
    let checked = type_check(&project.dir);
    assert_eq!(checked.code, Some(0), "{}", checked.describe());
}

#[test]
fn harness_drizzle_kit_does_with_the_tests_shapes_what_the_tests_expect() {
    let project = checked_project("harness-drizzle");
    // An empty schema and a `migrations/` holding only `.gitkeep`: no
    // migration is written (drizzle-kit writes its journal, with no entry).
    let generated = generate_migrations(&project.dir);
    assert_eq!(generated.code, Some(0), "{}", generated.describe());
    assert!(
        sql_files(&project.dir).is_empty(),
        "{}",
        generated.describe()
    );
    let journal = fs::read_to_string(project.path("migrations/meta/_journal.json")).unwrap();
    let journal: serde_json::Value = serde_json::from_str(&journal).unwrap();
    assert_eq!(journal["entries"], json!([]), "{journal}");

    // A table: one migration; again: none more.
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let generated = generate_migrations(&project.dir);
    assert_eq!(generated.code, Some(0), "{}", generated.describe());
    assert_eq!(sql_files(&project.dir).len(), 1, "{}", generated.describe());
    let before = Tree::of(&project.dir.join("migrations"));
    let again = generate_migrations(&project.dir);
    assert_eq!(again.code, Some(0), "{}", again.describe());
    assert_eq!(
        before.differences(&Tree::of(&project.dir.join("migrations"))),
        Vec::<String>::new(),
        "expected a second generate to write nothing\n{}",
        again.describe()
    );

    // A renamed column: no terminal, so drizzle-kit asks, writes nothing,
    // and still ends with 0.
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    let renamed = generate_migrations(&project.dir);
    assert_eq!(renamed.code, Some(0), "{}", renamed.describe());
    assert!(
        renamed.stderr.contains("TTY") || renamed.stdout.contains("TTY"),
        "{}",
        renamed.describe()
    );
    assert_eq!(
        before.differences(&Tree::of(&project.dir.join("migrations"))),
        Vec::<String>::new(),
        "expected nothing written for a rename\n{}",
        renamed.describe()
    );

    // A second table: a second migration.
    project.write("src/db/schema.ts", NOTES_AND_TAGS_SCHEMA_TS);
    let tags = generate_migrations(&project.dir);
    assert_eq!(tags.code, Some(0), "{}", tags.describe());
    assert_eq!(sql_files(&project.dir).len(), 2, "{}", tags.describe());

    // Back to no table type-checks.
    project.write("src/db/schema.ts", EMPTY_SCHEMA_TS);
    let checked = type_check(&project.dir);
    assert_eq!(checked.code, Some(0), "{}", checked.describe());
}

#[test]
fn harness_the_type_error_fixtures_give_the_errors_the_tests_count() {
    let project = checked_project("harness-types");
    project.write("src/many.ts", &type_errors(60));
    let odd = "src/odd\u{1b}[31mname.ts";
    project.write(odd, "export const wrong: number = \"text\";\n");
    let checked = type_check(&project.dir);
    assert_ne!(checked.code, Some(0));
    let errors: Vec<&str> = checked
        .stdout
        .lines()
        .filter(|l| l.contains("error TS"))
        .collect();
    assert_eq!(errors.len(), 61, "{}", checked.describe());
    assert!(
        checked.stdout.contains(
            "src/many.ts(1,14): error TS2322: Type 'string' is not assignable to type 'number'."
        ),
        "{}",
        checked.describe()
    );
    assert!(
        checked.stdout.contains(&format!("{odd}(1,14)")),
        "expected tsc to print the file name with its escape character raw\n{}",
        checked.describe()
    );
}

#[test]
fn harness_the_schemas_that_start_a_child_do() {
    for hang in [true, false] {
        let project = checked_project("harness-child");
        let pids = project.dir.with_extension("pids");
        project.write("src/db/schema.ts", &schema_starting_a_child(&pids, hang));
        let checked = type_check(&project.dir);
        assert_eq!(checked.code, Some(0), "{}", checked.describe());
        let mut command = Command::new("node");
        command
            .current_dir(&project.dir)
            .arg(blackbox::sdk::repository_root().join("node_modules/drizzle-kit/bin.cjs"))
            .args([
                "generate",
                "--dialect",
                "postgresql",
                "--schema",
                "src/db/schema.ts",
                "--out",
                "migrations",
            ]);
        let finished = run_to_exit(command, Duration::from_secs(20)).unwrap();
        let found = pids_in(&pids);
        assert_eq!(found.len(), 2, "expected two process ids in {pids:?}");
        if hang {
            assert!(
                finished.status.is_none(),
                "expected drizzle-kit never to finish with the hanging schema: {}\n{}",
                finished.status_text(),
                finished.stderr
            );
        } else {
            assert!(
                finished.status.is_some_and(|s| s.success()),
                "expected drizzle-kit to finish: {}\n{}",
                finished.status_text(),
                finished.stderr
            );
            assert!(!is_running(found[0]), "expected node to have ended");
        }
        // The child runs on after node (killed or done) is gone.
        assert!(
            is_running(found[1]),
            "expected the schema's child still running"
        );
        kill_all(&found);
        assert!(still_running_after(&found, Duration::from_secs(5)).is_empty());
        drop(fs::remove_file(&pids));
    }
}

#[test]
fn harness_a_tree_snapshot_sees_every_kind_of_change() {
    let project = TempProject::new("harness-tree");
    project.write("a/b.txt", "one");
    let first = Tree::of(&project.dir);
    assert_eq!(first.paths(), ["a/", "a/b.txt"]);
    assert_eq!(first.files(), ["a/b.txt"]);
    assert!(first.differences(&Tree::of(&project.dir)).is_empty());

    std::thread::sleep(Duration::from_millis(20));
    project.write("a/b.txt", "one");
    let rewritten = Tree::of(&project.dir);
    assert_eq!(
        first.differences(&rewritten).len(),
        1,
        "the same bytes written again: the modification time differs"
    );

    project.write("a/b.txt", "two");
    assert!(!rewritten.differences(&Tree::of(&project.dir)).is_empty());

    let before = Tree::of(&project.dir);
    fs::create_dir(project.path("c")).unwrap();
    assert!(!before.differences(&Tree::of(&project.dir)).is_empty());

    let before = Tree::of(&project.dir);
    std::thread::sleep(Duration::from_millis(20));
    fs::write(project.path("a/tmp"), "x").unwrap();
    fs::remove_file(project.path("a/tmp")).unwrap();
    assert!(
        !before.differences(&Tree::of(&project.dir)).is_empty(),
        "a file made and removed again changes its folder's modification time"
    );
}

#[test]
fn harness_whether_a_process_runs_is_told_right() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    assert!(is_running(pid));
    child.kill().unwrap();
    // Killed and not yet reaped: a zombie, which does not run.
    assert!(still_running_after(&[pid], Duration::from_secs(5)).is_empty());
    child.wait().unwrap();
    assert!(!is_running(pid));
    assert!(!is_running(u32::MAX));
}

#[test]
fn harness_a_tarball_is_listed_and_read() {
    let project = TempProject::new("harness-tar");
    project.write(
        "package/package.json",
        "{ \"name\": \"@vm/sdk\", \"version\": \"0.0.0\" }\n",
    );
    project.write("package/index.d.ts", "export {};\n");
    let tarball = project.path("vm-sdk-0.0.0.tgz");
    let mut command = Command::new("tar");
    command
        .current_dir(&project.dir)
        .args(["-czf", "vm-sdk-0.0.0.tgz", "package"]);
    let made = run_to_exit(command, Duration::from_secs(30)).unwrap();
    assert!(made.status.is_some_and(|s| s.success()), "{}", made.stderr);
    let listed = tar_list(&tarball).unwrap();
    assert!(
        listed.contains(&"package/package.json".to_owned()),
        "{listed:?}"
    );
    let package: serde_json::Value =
        serde_json::from_str(&tar_member(&tarball, "package/package.json").unwrap()).unwrap();
    assert_eq!(package["name"], json!("@vm/sdk"));
    // Not a tarball.
    fs::write(project.path("not.tgz"), "plain text").unwrap();
    assert!(tar_list(&project.path("not.tgz")).is_err());
}

#[test]
fn harness_a_stand_in_node_says_it_is_node_20() {
    let project = TempProject::new("harness-node20");
    let bin = stand_ins(&project.path("bin"), &[("node", NODE_20)]);
    for args in [
        vec!["--version"],
        vec!["-v"],
        vec!["-p", "process.version"],
        vec!["-e", "console.log(process.versions.node)"],
    ] {
        let mut command = Command::new("node");
        command.env_clear().env("PATH", &bin).args(&args);
        let finished = run_to_exit(command, Duration::from_secs(10)).unwrap();
        assert!(
            finished.stdout.trim() == "v20.0.0" || finished.stdout.trim() == "20.0.0",
            "{args:?}: {}",
            finished.stdout
        );
    }
}
