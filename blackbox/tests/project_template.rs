//! What a new project holds (CONTRACT.md, "A project: the template,
//! creating one and checking one: What a new project holds", "The
//! instruction file"; ticket "The project template, and creating and
//! checking a project": a new project passes the check untouched; the
//! instruction file carries the three corrections). Each test makes the
//! project with `vibe new` and reads what it wrote, runs the SDK's tooling,
//! `tsc`, `drizzle-kit`, `git` and `tar` on it, sends it as a Version, and
//! checks it with `vibe check`, once with its packages resolved from the
//! repository's root and once installed by `npm install` outside it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use blackbox::program::run_to_exit;
use blackbox::project::{
    PACKAGES, Report, Tree, check, check_in_words, dev_users, generate_migrations, help_commands,
    html_text, lock_version, new_project, new_project_in_repository, paragraphs, sdk_version,
    sql_files, squashed, tar_list, tar_member, tarball_path, vm_commands_in,
};
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, build, install, repository_root, run_tooling};
use blackbox::structure::type_check;
use blackbox::tool::Machine;
use blackbox::{User, Vm, VmOptions};
use serde_json::{Value, json};

const NAME: &str = "incident-notes";

fn read(dir: &Path, relative: &str) -> String {
    fs::read_to_string(dir.join(relative))
        .unwrap_or_else(|e| panic!("cannot read {relative} of the new project: {e}"))
}

fn read_json(dir: &Path, relative: &str) -> Value {
    serde_json::from_str(&read(dir, relative))
        .unwrap_or_else(|e| panic!("expected {relative} of the new project to be JSON: {e}"))
}

/// A new project on a machine of its own, outside the repository.
fn made() -> (Machine, PathBuf) {
    let machine = Machine::new();
    let dir = machine.root().join(NAME);
    new_project(&machine, NAME, &dir);
    (machine, dir)
}

fn bea() -> User {
    User::new("bea").in_groups(&["builders"])
}

#[test]
fn the_manifest_is_a_plain_literal_with_the_name_that_the_tooling_reads() {
    let (_machine, dir) = made();
    let text = read(&dir, "manifest.ts");
    assert!(text.contains("defineManifest("), "{text}");
    let manifest = run_tooling("manifest", &dir);
    assert_eq!(
        manifest.code,
        Some(0),
        "expected the SDK's tooling to read the new Manifest as a plain literal\n{}",
        manifest.describe()
    );
    let printed = manifest.json().unwrap();
    assert_eq!(printed["name"], json!(NAME));
    assert!(
        printed["description"]
            .as_str()
            .is_some_and(|d| !d.trim().is_empty()),
        "expected a description, got {printed}"
    );
    assert_eq!(printed["declared_access"], json!({ "resources": {} }));
}

#[test]
fn claude_md_is_one_line_that_points_at_agents_md() {
    let (_machine, dir) = made();
    let text = read(&dir, "CLAUDE.md");
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert_eq!(text.trim(), "@AGENTS.md");
}

#[test]
fn vm_dev_json_holds_one_user_outsider_in_no_group_and_no_grant() {
    let (_machine, dir) = made();
    let users = read_json(&dir, "vibe.dev.json");
    let mut keys: Vec<&String> = users.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["about", "grants", "users"], "{users}");
    assert!(
        users["about"]
            .as_str()
            .is_some_and(|a| !a.trim().is_empty()),
        "expected `about` to say what the file is for, got {users}"
    );
    assert_eq!(users["users"], dev_users());
    assert_eq!(users["grants"], json!([]));
}

#[test]
fn tsconfig_is_strict_with_the_own_structure_path_and_the_three_includes() {
    let (_machine, dir) = made();
    let config = read_json(&dir, "tsconfig.json");
    assert_eq!(config["compilerOptions"]["strict"], json!(true), "{config}");
    assert_eq!(
        config["compilerOptions"]["paths"],
        json!({ "@vm/own-structure": ["./src/db/schema.ts"] }),
        "{config}"
    );
    let includes: Vec<&str> = config["include"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `include` as an array, got {config}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for include in ["manifest.ts", "src", "resources"] {
        assert!(
            includes.contains(&include),
            "expected `include` to hold {include:?}, got {includes:?}"
        );
    }
}

#[test]
fn gitignore_ignores_node_modules_and_the_tools_folder_but_its_packages() {
    let (_machine, dir) = made();
    // The machine's own git, in the test's own environment (its `HOME`
    // left alone: the git on the `PATH` may be a wrapper that finds the
    // real one through it), with no configuration but the repository's.
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command
            .current_dir(&dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args);
        run_to_exit(command, Duration::from_secs(30)).unwrap()
    };
    let init = git(&["init", "-q"]);
    assert!(init.status.is_some_and(|s| s.success()), "{}", init.stderr);
    let ignored = |path: &str| {
        let finished = git(&["check-ignore", "-q", "--no-index", path]);
        match finished.status.and_then(|s| s.code()) {
            Some(0) => true,
            Some(1) => false,
            other => panic!(
                "git check-ignore {path} ended {other:?}: {}",
                finished.stderr
            ),
        }
    };
    for path in [
        "node_modules/drizzle-orm/index.js",
        ".vibe/state.json",
        ".vibe/cache/x",
    ] {
        assert!(ignored(path), "expected {path} ignored by .gitignore");
    }
    for path in [
        tarball_path(&sdk_version()).as_str(),
        "manifest.ts",
        "migrations/.gitkeep",
        "src/index.ts",
        "vibe.dev.json",
        "AGENTS.md",
        "package.json",
    ] {
        assert!(!ignored(path), "expected {path} not ignored by .gitignore");
    }
}

#[test]
fn package_json_is_private_with_exact_versions_of_the_lock_file_and_the_sdk_by_its_tarball() {
    let (_machine, dir) = made();
    let package = read_json(&dir, "package.json");
    assert_eq!(package["private"], json!(true), "{package}");
    let names = |field: &str| -> Vec<String> {
        let mut names: Vec<String> = package[field]
            .as_object()
            .unwrap_or_else(|| panic!("expected `{field}` as an object, got {package}"))
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    };
    assert_eq!(names("dependencies"), ["@vm/sdk", "drizzle-orm"]);
    assert_eq!(
        names("devDependencies"),
        ["drizzle-kit", "esbuild", "typescript"]
    );
    for (field, name) in [
        ("dependencies", "drizzle-orm"),
        ("devDependencies", "typescript"),
        ("devDependencies", "drizzle-kit"),
        ("devDependencies", "esbuild"),
    ] {
        assert_eq!(
            package[field][name],
            json!(lock_version(name)),
            "expected {name} at the exact version of the repository's own lock file"
        );
    }
    let tarball = tarball_path(&sdk_version());
    assert_eq!(
        package["dependencies"]["@vm/sdk"],
        json!(format!("file:{tarball}"))
    );
    assert!(dir.join(&tarball).is_file(), "expected {tarball} there");
}

#[test]
fn the_tarball_is_the_sdk_this_program_carries() {
    let (_machine, dir) = made();
    let version = sdk_version();
    let packages: Vec<String> = fs::read_dir(dir.join(PACKAGES))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(packages, [format!("vm-sdk-{version}.tgz")]);
    let tarball = dir.join(tarball_path(&version));
    let listed = tar_list(&tarball)
        .unwrap_or_else(|problem| panic!("expected a gzip'd tar that tar lists: {problem}"));
    for member in [
        "package/package.json",
        "package/index.d.ts",
        "package/index.js",
        "package/tooling/main.js",
    ] {
        assert!(
            listed.iter().any(|l| l == member),
            "expected {member} in the tarball, got {listed:?}"
        );
    }
    let package: Value =
        serde_json::from_str(&tar_member(&tarball, "package/package.json").unwrap()).unwrap();
    assert_eq!(package["name"], json!("@vm/sdk"));
    assert_eq!(
        package["version"],
        json!(version),
        "expected the version in the file's name"
    );
    assert_eq!(
        tar_member(&tarball, "package/index.d.ts").unwrap(),
        fs::read_to_string(repository_root().join("sdk/index.d.ts")).unwrap(),
        "expected the SDK's types as the repository has them"
    );
}

#[test]
fn the_own_schema_has_no_table_and_migrations_holds_only_gitkeep_and_is_in_step() {
    let machine = Machine::new();
    let project = new_project_in_repository(&machine, NAME);
    let listed: Vec<String> = fs::read_dir(project.path("migrations"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, [".gitkeep"]);
    assert_eq!(read(&project.dir, "migrations/.gitkeep"), "");
    let generated = generate_migrations(&project.dir);
    assert_eq!(generated.code, Some(0), "{}", generated.describe());
    assert!(
        sql_files(&project.dir).is_empty(),
        "expected the new schema to have no table: drizzle-kit generated a migration\n{}",
        generated.describe()
    );
}

#[test]
fn a_new_project_type_checks() {
    let machine = Machine::new();
    let project = new_project_in_repository(&machine, NAME);
    let checked = type_check(&project.dir);
    assert_eq!(checked.code, Some(0), "{}", checked.describe());
}

#[test]
fn a_new_project_is_an_application_as_it_stands() {
    let machine = Machine::new();
    let project = new_project_in_repository(&machine, NAME);
    let built = build(&project.dir).unwrap_or_else(|problem| panic!("{problem}"));
    assert_eq!(built.name, NAME);
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    install(&vm, &built);
    for id in ["outsider", "bea", "<b>a&b</b> \"quoted\" 'single'"] {
        let response = vm
            .request(NAME, "/")
            .as_user(&User::new(id))
            .timeout(SDK_REQUEST_TIMEOUT)
            .send();
        if response.status != 200 {
            vm.fail(&format!(
                "expected `200` from GET / of a new project as {id:?}, got\n{}",
                response.describe()
            ));
        }
        let content_type = response.header("content-type").unwrap_or_default();
        assert!(
            content_type.starts_with("text/html"),
            "expected text/html, got {content_type:?}"
        );
        let page = response.text();
        assert!(
            html_text(&page).contains(id),
            "expected the page to greet the User by id {id:?}, got\n{page}"
        );
        if id.contains('<') {
            assert!(
                !page.contains("<b>a&b</b>"),
                "expected the User's id written as text of the page, escaped for HTML, got\n{page}"
            );
        }
    }
}

/// The instruction file of a new project.
fn agents_md() -> String {
    let (_machine, dir) = made();
    read(&dir, "AGENTS.md")
}

#[test]
fn agents_md_is_at_most_120_lines_and_says_how_an_application_reaches_data() {
    let text = agents_md();
    let lines = text.lines().count();
    assert!(lines <= 120, "expected at most 120 lines, got {lines}");
    for named in ["ctx.user().db(", "ctx.user().ownDb()", "ctx.app().ownDb()"] {
        assert!(text.contains(named), "expected AGENTS.md to name {named}");
    }
}

#[test]
fn agents_md_carries_the_three_corrections() {
    let text = agents_md();
    let flat = squashed(&text);
    for words in [
        "unless the Builder has already clearly said that this data may be lost",
        "from the tool's own output",
    ] {
        assert!(
            flat.contains(words),
            "expected AGENTS.md to have the words {words:?}\n{text}"
        );
    }
    assert!(
        paragraphs(&text)
            .iter()
            .any(|p| p.contains("vibe.dev.json") && p.contains("outsider")),
        "expected one paragraph of AGENTS.md to name both vibe.dev.json and outsider\n{text}"
    );
}

#[test]
fn agents_md_says_a_refused_pull_is_told_to_the_builder_not_worked_around() {
    let text = agents_md();
    let rule = paragraphs(&text).into_iter().find(|p| {
        let lower = p.to_lowercase();
        p.contains("vibe pull") && lower.contains("refus")
    });
    let rule = rule.unwrap_or_else(|| {
        panic!("expected a paragraph of AGENTS.md on a refused `vibe pull`\n{text}")
    });
    let lower = squashed(&rule).to_lowercase();
    for word in ["builder", "ask", "invent", "mock"] {
        assert!(
            lower.contains(word),
            "expected the rule on a refused `vibe pull` to say {word:?}, got\n{rule}"
        );
    }
}

#[test]
fn agents_md_names_only_commands_this_program_has() {
    let text = agents_md();
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let listed = help_commands(&help.stdout);
    let named = vm_commands_in(&text);
    for command in ["check", "pull"] {
        assert!(
            named.iter().any(|c| c == command),
            "expected AGENTS.md to name `vibe {command}`, it names {named:?}"
        );
    }
    for command in &named {
        assert!(
            listed.contains(command),
            "AGENTS.md names `vibe {command}`, which `vibe --help` does not list ({listed:?})"
        );
    }
}

#[test]
fn a_new_project_passes_the_check_untouched() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &bea());
    let project = new_project_in_repository(&machine, NAME);
    let before = Tree::of(&project.dir);
    Report::clean(&check(&machine, &project.dir));
    let words = check_in_words(&machine, &project.dir);
    words.ok_text();
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new(),
        "expected the check to write nothing in the project"
    );
}

#[test]
fn a_new_project_installed_by_npm_outside_the_repository_passes_the_check() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &bea());
    let dir = machine.root().join(NAME);
    let printed = new_project(&machine, NAME, &dir);
    assert!(
        printed["next"][0]
            .as_str()
            .is_some_and(|c| c.starts_with("npm install")),
        "{printed}"
    );
    // The machine's own npm, in the test's own environment (its `HOME`,
    // and so the user's npm configuration and cache, left alone: npm on
    // the `PATH` may need them, and `--prefer-offline` uses the cache).
    let mut command = Command::new("npm");
    command
        .current_dir(&dir)
        .args(["install", "--prefer-offline", "--no-audit", "--no-fund"]);
    let installed = run_to_exit(command, Duration::from_secs(600)).unwrap();
    assert!(
        installed.status.is_some_and(|s| s.success()),
        "expected `npm install` of a new project to succeed: {}\n{}\n{}",
        installed.status_text(),
        installed.stdout,
        installed.stderr
    );
    assert!(dir.join("node_modules/@vm/sdk/package.json").is_file());
    let before = Tree::of_skipping(&dir, &["node_modules"]);
    Report::clean(&check(&machine, &dir));
    assert_eq!(
        before.differences(&Tree::of_skipping(&dir, &["node_modules"])),
        Vec::<String>::new(),
        "expected the check to write nothing in the project"
    );
}

#[test]
fn agents_md_says_grants_unknown_and_log_unavailable_are_nothing_to_fix() {
    // CONTRACT.md, "A write whose answer is lost": the template's AGENTS.md
    // names both beside the other reasons and says of both that nothing in
    // the project is wrong and the same request works again shortly.
    let text = agents_md();
    for reason in ["grants-unknown", "log-unavailable"] {
        assert!(
            text.contains(reason),
            "expected AGENTS.md to name the refusal reason `{reason}`\n{text}"
        );
    }
    let rule = paragraphs(&text)
        .into_iter()
        .find(|p| p.contains("grants-unknown") && p.contains("log-unavailable"));
    let rule = rule.unwrap_or_else(|| {
        panic!("expected a paragraph of AGENTS.md saying what `grants-unknown` and `log-unavailable` mean\n{text}")
    });
    let lower = squashed(&rule).to_lowercase();
    for word in ["nothing", "again"] {
        assert!(
            lower.contains(word),
            "expected the paragraph on `grants-unknown` and `log-unavailable` to say {word:?} (nothing in the project is wrong; the same request works again shortly), got\n{rule}"
        );
    }
}

#[test]
fn agents_md_says_the_local_databases_are_not_kept_through_a_crash_and_what_to_do() {
    // CONTRACT.md, "The local run on a busy machine: The local databases do
    // not wait on the disk": where the template's AGENTS.md describes `vibe
    // local reset`, it says that the local databases are not kept through
    // a crash of the machine or a loss of power, that `vibe local reset`
    // starts them anew, and that rows which must be there belong in
    // `test-rows/`.
    let text = agents_md();
    let found = paragraphs(&text).into_iter().find(|p| {
        let lower = squashed(p).to_lowercase();
        lower.contains("vibe local reset")
            && (lower.contains("crash") || lower.contains("power"))
            && lower.contains("test-rows")
    });
    assert!(
        found.is_some(),
        "expected one paragraph of AGENTS.md naming `vibe local reset` to say that the local databases are not kept through a crash of the machine or a loss of power, and that rows which must be there belong in `test-rows/`\n{text}"
    );
}
