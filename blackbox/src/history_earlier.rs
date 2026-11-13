//! What the test of an own database from before ticket #62 shares
//! (CONTRACT.md, "The Change history in the platform database: Where it is
//! kept": "An own database from before this ticket is brought to this form
//! when the program starts, before it serves").
//!
//! An own database "from before this ticket" is one made and written by
//! the program as it was when ticket #57 closed: commit [`EARLIER_COMMIT`]
//! on `main`. That program is got as a binary without reading its source:
//!
//! - [`earlier_binary`] checks the commit out in a git worktree of its own
//!   under `target/t62-earlier/src-<commit>` (with a link to this
//!   checkout's `.tools`, where `scripts/fetch-libclang.sh` puts libclang),
//!   builds `vm` there into `target/t62-earlier/target`, and keeps the
//!   binary as `target/t62-earlier/vm-<commit>`. When that file is there
//!   nothing is built. `rm -rf target/t62-earlier && git worktree prune`
//!   clears the cache.
//!
//! The harness starts every program with [`crate::program::vm_binary`],
//! which is fixed for the whole test process (`VM_BIN`, read once). So the
//! test runs itself again ([`with_both_programs`]): the outer process gets
//! both binaries, writes a small script that runs one or the other, and
//! starts its own test binary again for that one test with `VM_BIN` naming
//! the script. The inner process chooses, before each start, which program
//! the script runs ([`Programs::use_earlier`], [`Programs::use_current`]);
//! everything else (the harness's Postgres, [`crate::own_database::Own`],
//! the helpers of the other tests) is the harness as it is.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::own_database::{OwnDatabase, literal};
use crate::program::{manifest_dir, run_to_exit, vm_binary};

/// The commit of the program "from before this ticket": `main` when ticket
/// #57 closed, every gate green there.
pub const EARLIER_COMMIT: &str = "81f7016";

/// The environment variable that tells the inner run where the script
/// reads which program to run; set only in the inner run.
const SELECT_VAR: &str = "VM_BLACKBOX_T62_SELECT";

/// How long building the earlier program may take (a cold build compiles
/// the execution core).
const BUILD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// How long the inner run of a test may take.
const INNER_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The columns a change has in both forms (CONTRACT.md, "Change history"),
/// compared between `vm.history` before and `change_history` after.
pub const CHANGE_COLUMNS: [&str; 13] = [
    "id",
    "at",
    "schema_name",
    "table_name",
    "kind",
    "key",
    "old",
    "new",
    "changed",
    "user_id",
    "actor",
    "request",
    "migration",
];

/// The checkout's root: the `blackbox` crate's parent.
fn workspace() -> PathBuf {
    let manifest = manifest_dir();
    manifest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("the blackbox crate {} has no parent", manifest.display()))
}

/// `target/t62-earlier` of this checkout: the cache of the earlier program.
pub fn cache_dir() -> PathBuf {
    workspace().join("target").join("t62-earlier")
}

/// Runs `git` with these arguments in `dir`; its standard output, or the
/// failure.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.current_dir(dir).args(args);
    let finished = run_to_exit(command, Duration::from_secs(120))?;
    if finished.status.is_some_and(|s| s.success()) {
        Ok(finished.stdout.trim().to_owned())
    } else {
        Err(format!(
            "`git {}` in {} failed ({}): {}",
            args.join(" "),
            dir.display(),
            finished.status_text(),
            finished.stderr
        ))
    }
}

/// The `vm` binary at [`EARLIER_COMMIT`], built once and kept (see the
/// module's comment). Says on standard error how long a build took.
pub fn earlier_binary() -> PathBuf {
    let cache = cache_dir();
    let kept = cache.join(format!("vm-{EARLIER_COMMIT}"));
    if kept.is_file() {
        eprintln!(
            "the earlier program ({EARLIER_COMMIT}): kept at {}, not built",
            kept.display()
        );
        return kept;
    }
    let started = Instant::now();
    build_earlier(&cache, &kept).unwrap_or_else(|problem| {
        panic!("cannot build the program at {EARLIER_COMMIT}: {problem}")
    });
    eprintln!(
        "the earlier program ({EARLIER_COMMIT}): built in {} s, kept at {}",
        started.elapsed().as_secs(),
        kept.display()
    );
    kept
}

fn build_earlier(cache: &Path, kept: &Path) -> Result<(), String> {
    let root = workspace();
    std::fs::create_dir_all(cache).map_err(|e| format!("cannot make {}: {e}", cache.display()))?;
    let full = git(
        &root,
        &["rev-parse", &format!("{EARLIER_COMMIT}^{{commit}}")],
    )?;
    let tree = cache.join(format!("src-{EARLIER_COMMIT}"));
    if tree.is_dir() {
        let head = git(&tree, &["rev-parse", "HEAD"])?;
        if head != full {
            return Err(format!(
                "{} is checked out at {head}, not {full}; remove it (`git worktree remove --force {}`)",
                tree.display(),
                tree.display()
            ));
        }
    } else {
        let tree_text = tree.to_string_lossy().into_owned();
        git(&root, &["worktree", "add", "--detach", &tree_text, &full])?;
    }
    // libclang for the statement parser's build, as scripts/fetch-libclang.sh
    // left it in this checkout (the worktree has no `.tools` of its own).
    let tools = root.join(".tools");
    let tree_tools = tree.join(".tools");
    if tools.is_dir() && std::fs::symlink_metadata(&tree_tools).is_err() {
        std::os::unix::fs::symlink(&tools, &tree_tools)
            .map_err(|e| format!("cannot link {}: {e}", tree_tools.display()))?;
    }
    let target = cache.join("target");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(&tree)
        .args([
            "build",
            "-p",
            "vm",
            "--bin",
            "vm",
            "--message-format=json-render-diagnostics",
        ])
        .env("CARGO_TARGET_DIR", &target)
        // What the worktree's own `.cargo/config.toml` gives, set here
        // because cargo gives the test process this checkout's values, and
        // `[env]` does not replace a variable that is already set.
        .env(
            "RUNTIME_SNAPSHOT_PATH",
            tree.join("target").join("runtime-snapshot.bin"),
        )
        .env("LIBRARY_PATH", tree_tools.join("libclang"))
        .env("CLANG_PATH", tree_tools.join("libclang").join("system-cc"));
    let finished = run_to_exit(command, BUILD_TIMEOUT)?;
    if !finished.status.is_some_and(|s| s.success()) {
        return Err(format!(
            "`cargo build -p vm --bin vm` in {} failed ({}):\n{}",
            tree.display(),
            finished.status_text(),
            finished.stderr
        ));
    }
    let built = finished
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "vm")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or("cargo built no `vm` executable")?;
    let partial = cache.join(format!(
        "vm-{EARLIER_COMMIT}.partial-{}",
        std::process::id()
    ));
    std::fs::copy(&built, &partial).map_err(|e| {
        format!(
            "cannot copy {} to {}: {e}",
            built.display(),
            partial.display()
        )
    })?;
    std::fs::rename(&partial, kept).map_err(|e| {
        format!(
            "cannot rename {} to {}: {e}",
            partial.display(),
            kept.display()
        )
    })
}

/// The inner run's choice of program.
pub struct Programs {
    select: PathBuf,
}

impl Programs {
    fn choose(&self, which: &str) {
        std::fs::write(&self.select, which)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", self.select.display()));
    }

    /// The programs started from now on are the one at [`EARLIER_COMMIT`].
    pub fn use_earlier(&self) {
        self.choose("earlier");
    }

    /// The programs started from now on are the one under test.
    pub fn use_current(&self) {
        self.choose("current");
    }
}

/// Quotes a path for the script; fails on a path the script cannot hold.
fn quoted(path: &Path) -> String {
    let text = path.to_string_lossy();
    assert!(
        !text.contains('\''),
        "the harness's own check: a path with a quote in it cannot go into the script: {text}"
    );
    format!("'{text}'")
}

/// Runs `body` with both programs at hand. In the outer run (the test as
/// cargo starts it) this gets both binaries, writes the script, runs this
/// test (`test`, its name as libtest knows it) again in a process of its
/// own with `VM_BIN` naming the script, and fails unless that run passed
/// it. In that inner run it calls `body`, which starts the program it
/// chose before each start; the earlier one first.
pub fn with_both_programs(test: &str, body: impl FnOnce(&Programs)) {
    if let Some(select) = std::env::var_os(SELECT_VAR) {
        let programs = Programs {
            select: PathBuf::from(select),
        };
        programs.use_earlier();
        body(&programs);
        return;
    }
    let current = vm_binary().to_path_buf();
    let earlier = earlier_binary();
    let dir = std::env::temp_dir().join(format!("vm-blackbox-t62-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
    let select = dir.join("select");
    let script = dir.join("vm");
    std::fs::write(&select, "earlier")
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", select.display()));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n# Runs the earlier program or the one under test, as {select} says.\nif [ \"$(cat {select})\" = earlier ]; then exec {earlier} \"$@\"; fi\nexec {current} \"$@\"\n",
            select = quoted(&select),
            earlier = quoted(&earlier),
            current = quoted(&current),
        ),
    )
    .unwrap_or_else(|e| panic!("cannot write {}: {e}", script.display()));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("cannot make {} executable: {e}", script.display()));
    }
    let me =
        std::env::current_exe().unwrap_or_else(|e| panic!("cannot find this test binary: {e}"));
    let mut command = Command::new(me);
    command
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env("VM_BIN", &script)
        .env(SELECT_VAR, &select);
    let finished = run_to_exit(command, INNER_TIMEOUT);
    drop(std::fs::remove_dir_all(&dir));
    let finished = finished.unwrap_or_else(|problem| panic!("cannot run {test} again: {problem}"));
    let passed = finished.status.is_some_and(|s| s.success())
        && finished.stdout.contains("test result: ok. 1 passed");
    assert!(
        passed,
        "{test}, run with the earlier program ({EARLIER_COMMIT}, {}) and then the one under test ({}), failed ({}).\n--- its standard output ---\n{}\n--- its standard error ---\n{}",
        earlier.display(),
        current.display(),
        finished.status_text(),
        finished.stdout,
        finished.stderr
    );
}

/// Every row of `vm.history` in `db`, oldest first, as `to_jsonb` of the
/// row, read as the harness's superuser: the record the earlier form kept.
pub fn earlier_history(db: &OwnDatabase) -> Vec<Value> {
    json_rows(
        &db.superuser()
            .batch("SELECT to_jsonb(h)::text FROM vm.history h ORDER BY h.id"),
    )
}

/// The rows of `change_history` of `db`'s Application, oldest first, as
/// `to_jsonb` of the row, read now as the harness's superuser in the
/// platform database; the failure when they cannot be read.
pub fn change_history_now(db: &OwnDatabase) -> Result<Vec<Value>, String> {
    crate::history::platform_sql(db)
        .try_batch(&format!(
            "SELECT to_jsonb(h)::text FROM change_history h WHERE h.application = {} ORDER BY h.id",
            literal(&db.application)
        ))
        .map(|rows| json_rows(&rows))
}

fn json_rows(rows: &[Vec<Option<String>>]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let text = row.first().cloned().flatten().unwrap_or_default();
            serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("a row read as to_jsonb is not JSON ({e}): {text}"))
        })
        .collect()
}

/// A row's `id` as a number.
pub fn id_of(row: &Value) -> i64 {
    row["id"]
        .as_i64()
        .or_else(|| row["id"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("expected a row with an integer `id`, got {row}"))
}

/// What is wrong with `after` (rows of `change_history`) as the record of
/// `before` (rows of `vm.history`): each earlier change must be there
/// exactly once, with the same `id` and the same [`CHANGE_COLUMNS`], the
/// Application's name, and `structure` null. Empty when nothing is.
pub fn differences(application: &str, before: &[Value], after: &[Value]) -> Vec<String> {
    let mut wrong = Vec::new();
    for earlier in before {
        let id = id_of(earlier);
        let found: Vec<&Value> = after.iter().filter(|r| id_of(r) == id).collect();
        let [row] = found.as_slice() else {
            wrong.push(format!(
                "the change with id {id} is in change_history {} times, not once: {earlier}",
                found.len()
            ));
            continue;
        };
        for column in CHANGE_COLUMNS {
            if earlier[column] != row[column] {
                wrong.push(format!(
                    "the change with id {id}: `{column}` was {} in vm.history, is {} in change_history",
                    earlier[column], row[column]
                ));
            }
        }
        if row["application"] != application {
            wrong.push(format!(
                "the change with id {id}: `application` is {}, not {application:?}",
                row["application"]
            ));
        }
        if !row["structure"].is_null() {
            wrong.push(format!(
                "the change with id {id}: `structure` is {}, not null",
                row["structure"]
            ));
        }
    }
    wrong
}

/// The ids that are in `rows` more than once.
pub fn duplicate_ids(rows: &[Value]) -> Vec<i64> {
    let mut ids: Vec<i64> = rows.iter().map(id_of).collect();
    ids.sort_unstable();
    let mut twice: Vec<i64> = ids
        .windows(2)
        .filter_map(|w| match w {
            [a, b] if a == b => Some(*a),
            _ => None,
        })
        .collect();
    twice.dedup();
    twice
}
