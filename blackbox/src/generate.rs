//! What the tests of "Generate migrations, stopping before data is lost"
//! share (CONTRACT.md, "Generating migrations"):
//!
//! - `vm generate` run on a [`Machine`] ([`generate_args`], [`generate`],
//!   [`generate_in_words`]) and what it printed read with its shape checked:
//!   a migration generated ([`Generated::made`]), nothing to generate
//!   ([`Generated::nothing`]), a stop for data that would be lost
//!   ([`DataLoss::of`]);
//! - the entries of `destroys` as the contract's table writes them
//!   ([`table`], [`column`], [`type_change`], [`schema`]) and a comparison
//!   that reads a type as written with or without spaces
//!   ([`assert_destroys`]);
//! - the migrations of a project's `migrations/` as a Version carries them:
//!   in the journal's order, each `{ name: <tag>, sql: <the file's text> }`
//!   ([`journal_tags`], [`journal_migrations`]);
//! - the own schemas the tests change, one shape each, and a project whose
//!   first migration was made by the root's `drizzle-kit` by hand
//!   ([`project_with`]);
//! - a `TMPDIR` of a machine's own and what is in it ([`own_tmpdir`],
//!   [`listed`]), and a wait for the process ids a schema records
//!   ([`wait_for_pids`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::own_database::Migration;
use crate::program::run_to_exit;
use crate::project::{
    CHECK_TIMEOUT, Tree, checked_project, generate_migrations, pids_in, sql_files,
};
use crate::sdk::{TOOLING_TIMEOUT, TempProject, Tooled, repository_root};
use crate::tool::{Machine, Run};

/// How long a `vm generate` may take before the harness gives up: as for
/// `vm check`, below the command's own 300 s.
pub const GENERATE_TIMEOUT: Duration = CHECK_TIMEOUT;

/// The words the contract gives the message of `data-loss` (and AGENTS.md
/// its rule on data loss).
pub const UNLESS_SAID: &str =
    "unless the Builder has already clearly said that this data may be lost";

/// `vm generate --project <dir> --json`, and these arguments after.
pub fn generate_args<'a>(dir: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["generate", "--project", dir, "--json"];
    args.extend_from_slice(extra);
    args
}

fn utf8(dir: &Path) -> &str {
    dir.to_str().unwrap_or_else(|| panic!("not UTF-8: {dir:?}"))
}

/// Runs `vm generate --project <dir> --json <extra>` on this machine.
pub fn generate(machine: &Machine, dir: &Path, extra: &[&str]) -> Run {
    generate_with_env(machine, dir, extra, &[])
}

/// As [`generate`], with these environment variables added.
pub fn generate_with_env(
    machine: &Machine,
    dir: &Path,
    extra: &[&str],
    env: &[(&str, &str)],
) -> Run {
    machine.run_into_files(&generate_args(utf8(dir), extra), env, GENERATE_TIMEOUT)
}

/// Runs `vm generate --project <dir> <extra>` (in words) on this machine.
pub fn generate_in_words(machine: &Machine, dir: &Path, extra: &[&str]) -> Run {
    let mut args = vec!["generate", "--project", utf8(dir)];
    args.extend_from_slice(extra);
    machine.run_into_files(&args, &[], GENERATE_TIMEOUT)
}

/// What `vm generate --json` printed when it generated a migration.
#[derive(Clone, Debug)]
pub struct Generated {
    pub run: Run,
    pub body: Value,
    /// `generated.name`: `<number>_<name>`.
    pub name: String,
    /// `generated.file`: `migrations/<name>.sql`.
    pub file: String,
    pub statements: u64,
    pub destroys: Vec<Value>,
    pub files: Vec<String>,
}

impl Generated {
    /// Fails unless the run ended with `0` and printed `{ "generated": {
    /// name, file, statements, destroys }, "files": [...] }` with `file`
    /// `migrations/<name>.sql`, `files` the migration, its snapshot
    /// `migrations/meta/<number>_snapshot.json` and the journal
    /// `migrations/meta/_journal.json` in the order of their paths, each of
    /// them there in the project `dir`, and the journal's last entry the
    /// migration.
    pub fn made(run: &Run, dir: &Path) -> Self {
        let body = run.ok_json();
        let describe = || run.describe();
        let mut keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(keys, ["files", "generated"], "the keys\n{}", describe());
        let generated = &body["generated"];
        let mut inner: Vec<&str> = generated
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        inner.sort_unstable();
        assert_eq!(
            inner,
            ["destroys", "file", "name", "statements"],
            "expected `generated` to be an object of name, file, statements and destroys\n{}",
            describe()
        );
        let name = generated["name"]
            .as_str()
            .unwrap_or_else(|| panic!("expected `generated.name` a string\n{}", describe()))
            .to_owned();
        let number = number_of(&name).unwrap_or_else(|| {
            panic!("expected `generated.name` to be <number>_<name>, got {name:?}")
        });
        let file = generated["file"].as_str().unwrap_or_default().to_owned();
        assert_eq!(
            file,
            format!("migrations/{name}.sql"),
            "expected `generated.file` to be migrations/<name>.sql\n{}",
            describe()
        );
        let statements = generated["statements"].as_u64().unwrap_or_else(|| {
            panic!(
                "expected `generated.statements` a whole number\n{}",
                describe()
            )
        });
        let destroys = generated["destroys"]
            .as_array()
            .unwrap_or_else(|| panic!("expected `generated.destroys` an array\n{}", describe()))
            .clone();
        let files: Vec<String> = body["files"]
            .as_array()
            .unwrap_or_else(|| panic!("expected `files` an array\n{}", describe()))
            .iter()
            .map(|f| f.as_str().unwrap_or_default().to_owned())
            .collect();
        let want = vec![
            file.clone(),
            format!("migrations/meta/{number}_snapshot.json"),
            "migrations/meta/_journal.json".to_owned(),
        ];
        assert_eq!(
            files,
            want,
            "expected `files` to be the migration, its snapshot and the journal, in the order of their paths\n{}",
            describe()
        );
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted, "the harness's own check: in path order");
        for f in &files {
            assert!(
                dir.join(f).is_file(),
                "expected {f} written in the project\n{}",
                describe()
            );
        }
        assert_eq!(
            journal_tags(dir).last(),
            Some(&name),
            "expected the journal's last entry to be the migration generated\n{}",
            describe()
        );
        Self {
            run: run.clone(),
            body,
            name,
            file,
            statements,
            destroys,
            files,
        }
    }

    /// Fails unless the run ended with `0` and printed exactly
    /// `{ "generated": null, "files": [] }`.
    pub fn nothing(run: &Run) {
        let body = run.ok_json();
        assert_eq!(
            body,
            json!({ "generated": null, "files": [] }),
            "expected nothing generated\n{}",
            run.describe()
        );
    }

    /// The migration's text in the project `dir`.
    pub fn sql(&self, dir: &Path) -> String {
        fs::read_to_string(dir.join(&self.file))
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", self.file))
    }
}

/// The number of a migration's name `<number>_<name>`: its leading digits,
/// when a `_` and something follow them.
pub fn number_of(name: &str) -> Option<&str> {
    let (number, rest) = name.split_once('_')?;
    (number.len() == 4 && number.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty())
        .then_some(number)
}

/// What `vm generate --json` printed when it stopped for data that would
/// be lost.
#[derive(Clone, Debug)]
pub struct DataLoss {
    pub run: Run,
    pub message: String,
    pub destroys: Vec<Value>,
    pub statements: Vec<String>,
}

impl DataLoss {
    /// Fails unless the run ended with `3`, reason `data-loss`, and the
    /// error has `destroys` (an array) and `statements` (an array of
    /// strings) beside its `reason` and `message`.
    pub fn of(run: &Run) -> Self {
        let message = run.json_error(3, "data-loss");
        let body = run.json();
        let error = &body["error"];
        let mut keys: Vec<&str> = error
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["destroys", "message", "reason", "statements"],
            "expected the error to have destroys and statements beside reason and message\n{}",
            run.describe()
        );
        let destroys = error["destroys"].as_array().cloned().unwrap_or_default();
        let statements: Vec<String> = error["statements"]
            .as_array()
            .unwrap_or_else(|| panic!("expected `statements` an array\n{}", run.describe()))
            .iter()
            .map(|s| {
                s.as_str()
                    .unwrap_or_else(|| panic!("expected each statement a string, got {s}"))
                    .to_owned()
            })
            .collect();
        Self {
            run: run.clone(),
            message,
            destroys,
            statements,
        }
    }
}

/// Fails unless a message of `data-loss` names each of `named`, says that
/// deploying an older Version does not bring it back, says to ask the
/// Builder unless the Builder has already clearly said this data may be
/// lost, and names `--allow-data-loss`.
pub fn assert_data_loss_message(message: &str, named: &[&str]) {
    let flat = crate::project::squashed(message);
    let lower = flat.to_lowercase();
    for name in named {
        assert!(
            message.contains(name),
            "expected the message to name {name:?}, got {message:?}"
        );
    }
    assert!(
        lower.contains("older version") && lower.contains("back"),
        "expected the message to say that deploying an older Version later does not bring it back, got {message:?}"
    );
    assert!(
        flat.contains("Builder") && lower.contains("ask"),
        "expected the message to say to ask the Builder, got {message:?}"
    );
    assert!(
        flat.contains(UNLESS_SAID),
        "expected the message to have the words {UNLESS_SAID:?}, got {message:?}"
    );
    assert!(
        message.contains("--allow-data-loss"),
        "expected the message to name --allow-data-loss, got {message:?}"
    );
}

/// `{ "what": "table", "table": <schema.table> }`.
pub fn table(qualified: &str) -> Value {
    json!({ "what": "table", "table": qualified })
}

/// `{ "what": "column", "table": <schema.table>, "column": <column> }`.
pub fn column(qualified: &str, column: &str) -> Value {
    json!({ "what": "column", "table": qualified, "column": column })
}

/// `{ "what": "type", "table", "column", "from", "to" }`.
pub fn type_change(qualified: &str, column: &str, from: &str, to: &str) -> Value {
    json!({ "what": "type", "table": qualified, "column": column, "from": from, "to": to })
}

/// `{ "what": "schema", "schema": <name> }`.
pub fn schema(name: &str) -> Value {
    json!({ "what": "schema", "schema": name })
}

/// An entry of `destroys` with its `from` and `to` read without white
/// space and in lower case: `numeric(10, 2)` and `numeric(10,2)` are the
/// same type as written (the contract says `"<type>"` and no more).
pub fn normalized(entry: &Value) -> Value {
    let mut entry = entry.clone();
    for key in ["from", "to"] {
        if let Some(text) = entry.get(key).and_then(Value::as_str) {
            let squeezed: String = text
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase();
            entry[key] = Value::from(squeezed);
        }
    }
    entry
}

/// Fails unless `got` is exactly `want`, in order, each entry read by
/// [`normalized`].
pub fn assert_destroys(got: &[Value], want: &[Value], context: &str) {
    let got: Vec<Value> = got.iter().map(normalized).collect();
    let want: Vec<Value> = want.iter().map(normalized).collect();
    assert_eq!(got, want, "{context}: `destroys`");
}

/// The journal `migrations/meta/_journal.json` of the project `dir`.
pub fn journal(dir: &Path) -> Value {
    let path = dir.join("migrations/meta/_journal.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// The tags of the journal's entries, in its order; none when there is no
/// journal.
pub fn journal_tags(dir: &Path) -> Vec<String> {
    if !dir.join("migrations/meta/_journal.json").is_file() {
        return Vec::new();
    }
    journal(dir)["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|e| e["tag"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The migrations of the project `dir` as a Version carries them
/// (CONTRACT.md, "Generating migrations: The platform applies what it
/// generates"): in the journal's order, each named by its tag with the
/// text of `migrations/<tag>.sql`.
pub fn journal_migrations(dir: &Path) -> Vec<Migration> {
    journal_tags(dir)
        .into_iter()
        .map(|tag| {
            let path = dir.join(format!("migrations/{tag}.sql"));
            let sql = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            Migration { name: tag, sql }
        })
        .collect()
}

/// A `src/db/schema.ts` with one table, `notes`: `id` and `amount`, the
/// column whose Drizzle builder is `builder` (`integer("amount")`, say).
pub fn amount_schema(builder: &str) -> String {
    format!(
        "import {{ bigint, char, doublePrecision, integer, numeric, pgTable, real, serial, smallint, text, varchar }} from \"drizzle-orm/pg-core\";\n\nexport const notes = pgTable(\"notes\", {{\n  id: serial(\"id\").primaryKey(),\n  amount: {builder},\n}});\n\n// Every builder imported is used, so that the file type-checks whichever one `amount` has.\nexport const unused = [bigint, char, doublePrecision, integer, numeric, real, smallint, text, varchar];\n"
    )
}

/// `notes` with a third column, `title`, which may be null.
pub const NOTES_WITH_TITLE_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  title: text("title"),
});
"#;

/// The first step of the path the contract gives for a rename of `body`
/// to `text`: `text` added (it may be null, so it can be added over rows),
/// `body` kept.
pub const NOTES_BODY_AND_TEXT_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  text: text("text"),
});
"#;

/// `notes` and `labels`: from [`crate::project::NOTES_AND_TAGS_SCHEMA_TS`],
/// `tags` gone and `labels` there with the same columns, a change
/// `drizzle-kit` asks about.
pub const NOTES_AND_LABELS_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
});

export const labels = pgTable("labels", {
  id: serial("id").primaryKey(),
  label: text("label").notNull(),
});
"#;

/// `notes`, and a schema `archive` with one table, `old`.
pub const NOTES_AND_ARCHIVE_SCHEMA_TS: &str = r#"import { pgSchema, pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
});

export const archiveSchema = pgSchema("archive");

export const archiveOld = archiveSchema.table("old", {
  id: serial("id").primaryKey(),
});
"#;

/// Before three losses at once: `notes` with `body` and a `bigint`
/// `amount`, and `tags`.
pub const SEVERAL_BEFORE_SCHEMA_TS: &str = r#"import { bigint, pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  amount: bigint("amount", { mode: "number" }),
});

export const tags = pgTable("tags", {
  id: serial("id").primaryKey(),
  label: text("label").notNull(),
});
"#;

/// After them: `tags` dropped, `amount` made `integer`, `body` dropped.
pub const SEVERAL_AFTER_SCHEMA_TS: &str = r#"import { integer, pgTable, serial } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  amount: integer("amount"),
});
"#;

/// `notes` with a column `hint` whose default is a string that reads as
/// destroying statements.
pub const DEFAULT_SAYING_DROP_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  hint: text("hint").default("DROP TABLE notes; ALTER TABLE notes DROP COLUMN body"),
});
"#;

/// The default's string as `drizzle-kit` writes it into the migration.
pub const DEFAULT_SAYING_DROP_SQL: &str = "ALTER TABLE \"notes\" ADD COLUMN \"hint\" text DEFAULT 'DROP TABLE notes; ALTER TABLE notes DROP COLUMN body';";

/// `notes` with a column `odd` whose default is SQL that does not parse
/// (`(((`), which `drizzle-kit` writes as it is.
pub const UNPARSEABLE_DEFAULT_SCHEMA_TS: &str = r#"import { sql } from "drizzle-orm";
import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  odd: text("odd").default(sql`(((`),
});
"#;

/// A change of `amount`'s type: the builder before and its type as the
/// contract writes it, the builder after and its type.
pub type TypeChange = (&'static str, &'static str, &'static str, &'static str);

/// Every row of the contract's "Wider" that `drizzle-kit` generates a
/// change of type for (`varchar(n)` to `varchar(n)` is no change at all).
pub const WIDER: [TypeChange; 10] = [
    (
        "smallint(\"amount\")",
        "smallint",
        "integer(\"amount\")",
        "integer",
    ),
    (
        "smallint(\"amount\")",
        "smallint",
        "bigint(\"amount\", { mode: \"number\" })",
        "bigint",
    ),
    (
        "integer(\"amount\")",
        "integer",
        "bigint(\"amount\", { mode: \"number\" })",
        "bigint",
    ),
    (
        "real(\"amount\")",
        "real",
        "doublePrecision(\"amount\")",
        "double precision",
    ),
    (
        "varchar(\"amount\", { length: 20 })",
        "varchar(20)",
        "varchar(\"amount\", { length: 50 })",
        "varchar(50)",
    ),
    (
        "varchar(\"amount\", { length: 20 })",
        "varchar(20)",
        "varchar(\"amount\")",
        "varchar",
    ),
    (
        "varchar(\"amount\", { length: 20 })",
        "varchar(20)",
        "text(\"amount\")",
        "text",
    ),
    (
        "char(\"amount\", { length: 5 })",
        "char(5)",
        "text(\"amount\")",
        "text",
    ),
    (
        "numeric(\"amount\", { precision: 10, scale: 2 })",
        "numeric(10,2)",
        "numeric(\"amount\", { precision: 12, scale: 3 })",
        "numeric(12,3)",
    ),
    (
        "numeric(\"amount\", { precision: 10, scale: 2 })",
        "numeric(10,2)",
        "numeric(\"amount\")",
        "numeric",
    ),
];

/// Changes of type that are not wider: each counts as destroying.
pub const NOT_WIDER: [TypeChange; 6] = [
    (
        "text(\"amount\")",
        "text",
        "varchar(\"amount\", { length: 10 })",
        "varchar(10)",
    ),
    (
        "bigint(\"amount\", { mode: \"number\" })",
        "bigint",
        "integer(\"amount\")",
        "integer",
    ),
    ("text(\"amount\")", "text", "integer(\"amount\")", "integer"),
    (
        "numeric(\"amount\", { precision: 10, scale: 2 })",
        "numeric(10,2)",
        "numeric(\"amount\", { precision: 8, scale: 2 })",
        "numeric(8,2)",
    ),
    (
        "numeric(\"amount\", { precision: 10, scale: 2 })",
        "numeric(10,2)",
        "numeric(\"amount\", { precision: 10, scale: 3 })",
        "numeric(10,3)",
    ),
    (
        "varchar(\"amount\", { length: 50 })",
        "varchar(50)",
        "varchar(\"amount\", { length: 20 })",
        "varchar(20)",
    ),
];

/// `drizzle-kit generate --dialect postgresql --schema src/db/schema.ts
/// --out migrations <extra>`, run in `dir` with the root's `drizzle-kit`
/// and no terminal, as [`generate_migrations`] runs it: for the harness's
/// own steps (`--custom --name <name>`).
pub fn drizzle_kit(dir: &Path, extra: &[&str]) -> Tooled {
    let bin = repository_root().join("node_modules/drizzle-kit/bin.cjs");
    let mut args = vec![
        "generate",
        "--dialect",
        "postgresql",
        "--schema",
        "src/db/schema.ts",
        "--out",
        "migrations",
    ];
    args.extend_from_slice(extra);
    let mut command = Command::new("node");
    command
        .current_dir(dir)
        .arg(&bin)
        .args(&args)
        .stdin(std::process::Stdio::null());
    let text = format!(
        "(in {}) node {} {}",
        dir.display(),
        bin.display(),
        args.join(" ")
    );
    let finished = run_to_exit(command, TOOLING_TIMEOUT)
        .unwrap_or_else(|problem| panic!("cannot run {text}: {problem}"));
    Tooled {
        code: finished.status.and_then(|s| s.code()),
        stdout: finished.stdout,
        stderr: finished.stderr,
        command: text,
    }
}

/// The statements of a migration's text, as `drizzle-kit` separates them
/// (`--> statement-breakpoint`), each trimmed; empty pieces left out.
pub fn statements_of(sql: &str) -> Vec<String> {
    sql.split(crate::own_database::BREAKPOINT)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A project of the harness's own ([`checked_project`]) whose own schema
/// is `first`, with its migration made by the root's `drizzle-kit` by hand
/// ([`generate_migrations`]): one `.sql` file. Then a test changes the
/// schema and runs `vm generate`.
pub fn project_with(label: &str, first: &str) -> TempProject {
    let project = checked_project(label);
    project.write("src/db/schema.ts", first);
    let made = generate_migrations(&project.dir);
    assert_eq!(made.code, Some(0), "{}", made.describe());
    assert_eq!(
        sql_files(&project.dir).len(),
        1,
        "the harness's own check: expected drizzle-kit to make the first migration\n{}",
        made.describe()
    );
    // What the test does next is told from this by its modification time.
    std::thread::sleep(Duration::from_millis(50));
    project
}

/// Fails unless nothing in `dir` differs from `before`.
pub fn assert_unchanged(before: &Tree, dir: &Path, context: &str) {
    assert_eq!(
        before.differences(&Tree::of(dir)),
        Vec::<String>::new(),
        "{context}: expected nothing written in the project"
    );
}

/// Every file under `dir` (not folders, not links) with its bytes: what a
/// test compares when folders' modification times may change (a file
/// written and removed again).
pub fn files_and_bytes(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    Tree::of(dir)
        .0
        .into_iter()
        .filter_map(|(path, entry)| match entry {
            crate::project::Entry::File { bytes, .. } => Some((path, bytes)),
            _ => None,
        })
        .collect()
}

/// Fails unless a message of `migrations-unreadable` gives, as its fix,
/// the command `vm generate` for the migrations the schema then needs, and
/// never `drizzle-kit` run by hand.
pub fn assert_unreadable_fix_names_vm_generate(message: &str) {
    assert!(
        message.contains("vm generate"),
        "expected the fix of migrations-unreadable to name `vm generate`, got {message:?}"
    );
    for by_hand in [
        "drizzle-kit generate",
        "npx drizzle-kit",
        "with drizzle-kit",
    ] {
        assert!(
            !message.contains(by_hand),
            "expected the fix of migrations-unreadable not to tell the agent to run drizzle-kit by hand ({by_hand:?}), got {message:?}"
        );
    }
}

/// Fails unless a message of `migrations-unreadable` for a `.sql` file the
/// journal does not name says to remove the file if it was never
/// committed.
pub fn assert_says_remove_if_never_committed(message: &str) {
    let lower = crate::project::squashed(message).to_lowercase();
    assert!(
        lower.contains("remove")
            && (lower.contains("never committed")
                || lower.contains("not committed")
                || lower.contains("never been committed")),
        "expected the message to say to remove the file if it was never committed, got {message:?}"
    );
}

/// Sets a folder's mode, and gives it back its earlier mode when dropped
/// (so that the test's folders can be removed, failing or not).
pub struct ModeFor {
    path: PathBuf,
    earlier: u32,
}

impl ModeFor {
    pub fn set(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let earlier = fs::metadata(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
            .permissions()
            .mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("cannot set the mode of {}: {e}", path.display()));
        Self {
            path: path.to_path_buf(),
            earlier,
        }
    }
}

impl Drop for ModeFor {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        drop(fs::set_permissions(
            &self.path,
            fs::Permissions::from_mode(self.earlier),
        ));
    }
}

/// A `TMPDIR` of the machine's own, made empty.
pub fn own_tmpdir(machine: &Machine) -> PathBuf {
    let tmp = machine.root().join("tmp");
    fs::create_dir_all(&tmp).unwrap_or_else(|e| panic!("cannot make {}: {e}", tmp.display()));
    tmp
}

/// What is in `dir`, by name.
pub fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Waits until the file `pids` holds at least `n` process ids, at most
/// `limit`; returns them.
pub fn wait_for_pids(pids: &Path, n: usize, limit: Duration) -> Vec<u32> {
    let deadline = Instant::now() + limit;
    loop {
        let found = pids_in(pids);
        if found.len() >= n || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_is_read_without_its_spaces() {
        assert_eq!(
            normalized(&type_change(
                "public.notes",
                "amount",
                "numeric(10, 2)",
                "NUMERIC(8,2)"
            )),
            type_change("public.notes", "amount", "numeric(10,2)", "numeric(8,2)")
        );
        assert_eq!(normalized(&table("public.tags")), table("public.tags"));
    }

    #[test]
    fn a_migrations_number_is_its_four_digits() {
        assert_eq!(number_of("0001_add_notes"), Some("0001"));
        assert_eq!(number_of("0012_x"), Some("0012"));
        assert_eq!(number_of("0001_"), None);
        assert_eq!(number_of("add_notes"), None);
        assert_eq!(number_of("01_x"), None);
    }
}
