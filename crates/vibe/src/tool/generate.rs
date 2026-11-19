//! `vibe generate [--project <directory>] [--name <name>] [--allow-data-loss]
//! [--empty] [--timeout <seconds>] [--json]` (blackbox/CONTRACT.md,
//! "Generating migrations").
//!
//! What is looked at first, in order: the command line (clap: `2`), the
//! project (`not-a-project`), `node` (`node-missing`), `drizzle-kit`
//! (`not-installed`). It needs no sign-in and asks the installation
//! nothing.
//!
//! **Decided in the command's own folder, written after.** `migrations/`
//! is copied into the command's own folder ([`drizzle::copy_migrations`])
//! and `drizzle-kit generate` runs against the copy ([`drizzle::generate`],
//! which also reads the copy itself: `migrations-unreadable`,
//! `own-schema-failed`). With `--empty` it runs first without `--custom`,
//! and only when that finds the schema in step does it run with
//! `--custom`. What it wrote in the copy is read into memory (bounded:
//! [`MAX_SQL_BYTES`], [`MAX_SNAPSHOT_BYTES`], the journal as
//! [`drizzle::MAX_JOURNAL_BYTES`]), the migration's statements are read
//! with the platform's parser ([`crate::migration_text::parsed`]) and what
//! they destroy decided ([`data_loss`]). All of that is inside the time and
//! the signals, as for `vibe check`; any ending there leaves the project as
//! it was, and the own folder is removed.
//!
//! Only then, when there is a migration to keep, are its three files
//! written into `migrations/`: the migration, its snapshot, the journal
//! last, each whole ([`write_whole`]: a temporary file beside it, synced,
//! renamed over). This is after the time and outside the signals' reach
//! (their handlers stay in place, so a signal then ends nothing), and has
//! no wait in it: the three writes are not cut off half way by the
//! command. All three or none: when one cannot be written, the files
//! written before it (new files: the journal, which replaces the old one,
//! is last) and the folders made for it, when empty, are removed again
//! ([`Undo`]), and only what could not be removed is named as left. What
//! only a crash or `SIGKILL` between two of them can leave:
//! the `.sql` file alone, or with its snapshot, which the journal does not
//! name (`vibe check` and `vibe generate` then say `migrations-unreadable`,
//! naming it), and at most one temporary file `.<name>.vibe-generate`,
//! which no command reads and the next write of that file removes.
//!
//! The tool writes inside the project only: when `migrations/` or
//! `migrations/meta/` is a symbolic link, it is `not-a-project`, found out
//! before `drizzle-kit` runs, and nothing is written (looked at again when
//! writing).
//!
//! **Blocking work.** As for `vibe check`: the copying, the reading and the
//! writing of the project's files, and the parsing of the generated
//! migration (at most [`MAX_SQL_BYTES`]), are blocking calls on the tool's
//! one thread, bounded as above; only the tools it runs can take long, and
//! they are inside the time.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::data_loss::{self, Destroyed, lost, push_statement};
use super::drizzle::{self, Journal, Outcome};
use super::project_tools::{
    Bounded, Cx, DEFAULT_TIMEOUT, Scratch, Signals, Who, drizzle_kit, node_is_there,
    own_folder_failed,
};
use super::theirs::Theirs;
use super::{Done, Ending, Output, project_of, write_whole};
use crate::migration_text;

/// `vibe generate`.
#[derive(clap::Args)]
pub struct GenerateArgs {
    /// The project; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The migration's name after its number (0001_<name>): 1 to 100
    /// lower-case letters, digits and _. Without it, drizzle-kit's own.
    #[arg(long, value_parser = migration_name)]
    pub name: Option<String>,
    /// Generate a migration that destroys data (drops a table, a column or
    /// a schema, or changes a type to one that is not wider). Only when the
    /// Builder has clearly said that this data may be lost.
    #[arg(long)]
    pub allow_data_loss: bool,
    /// Write an empty migration, for SQL of your own (such as copying
    /// values); only when the schema is in step with the migrations.
    #[arg(long)]
    pub empty: bool,
    /// The longest the command may take, in seconds (1 to 3600; 300 without
    /// it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// How the command's messages name it.
const WHO: Who = Who {
    command: "vibe generate",
    the: "vibe generate",
    folder: "vibe-generate",
};

/// The longest migration read from what `drizzle-kit` wrote.
const MAX_SQL_BYTES: u64 = 16 * 1024 * 1024;

/// The longest `--name`.
const MAX_NAME: usize = 100;

fn migration_name(text: &str) -> Result<String, String> {
    let fits = !text.is_empty()
        && text.len() <= MAX_NAME
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if fits {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "not a migration's name: use 1 to {MAX_NAME} lower-case letters, digits and _"
        ))
    }
}

/// Where the command is, for the message of one that ran out of time or
/// was interrupted.
#[derive(Clone, Copy)]
enum Stage {
    Node,
    Packages,
    Copying,
    InStep,
    Generating,
    Reading,
}

impl Stage {
    fn words(self) -> &'static str {
        match self {
            Self::Node => "asking node its version",
            Self::Packages => "finding the project's packages",
            Self::Copying => "copying migrations/ into its own folder",
            Self::InStep => {
                "finding out whether the schema is in step (drizzle-kit generate loading src/db/schema.ts)"
            }
            Self::Generating => "drizzle-kit generate loading src/db/schema.ts",
            Self::Reading => "reading the migration drizzle-kit generated",
        }
    }
}

/// A migration generated and decided on, to be written into the project.
struct Migration {
    /// Its tag in the journal: `<number>_<name>`.
    tag: String,
    /// Its snapshot's file name: `<number>_snapshot.json`.
    snapshot_name: String,
    sql: Vec<u8>,
    snapshot: Vec<u8>,
    journal: Vec<u8>,
    /// Its statements as it has them, for the words.
    statements: Vec<String>,
    destroyed: Vec<Destroyed>,
}

pub(super) async fn generate(args: &GenerateArgs) -> Result<Done, Ending> {
    let project = project_of(args.project.as_deref(), "vibe generate")?;
    // Node resolves packages from the real path, and the tools are given
    // absolute paths.
    let project = fs::canonicalize(&project).map_err(|error| {
        Ending::new(
            1,
            "project-file",
            format!(
                "the project's folder cannot be read ({error}): make it readable by you, then run vibe generate again"
            ),
        )
    })?;
    // In place before the first tool starts, and kept until the command
    // ends: a signal ends the deciding below, and nothing after.
    let mut signals = Signals::listen(&WHO)?;
    let own = Scratch::make(&WHO).map_err(|e| own_folder_failed(&e, &WHO))?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let stage = Cell::new(Stage::Node);
    let decided = match signals
        .within(limit, decided(&project, &own.path, args, &stage))
        .await
    {
        Bounded::Ended(decided) => decided,
        Bounded::TimedOut => Err(Ending::new(
            1,
            "generate-timed-out",
            format!(
                "vibe generate did not end within {} s and was stopped, with everything it started; it was in {}, and nothing was written. Look at what that runs (src/db/schema.ts and what it imports are run by drizzle-kit), or give more time with --timeout <seconds>, then run vibe generate again",
                limit.as_secs(),
                stage.get().words()
            ),
        )),
        Bounded::Interrupted(signal) => Err(Ending::new(
            1,
            "interrupted",
            format!(
                "vibe generate was interrupted ({signal}) in {}, and stopped everything it had started; nothing was written: run vibe generate again",
                stage.get().words()
            ),
        )),
    };
    // Nothing writes in the own folder any more.
    drop(own);
    let done = match decided? {
        None => nothing(),
        Some(migration) => {
            let files = write(&project, &migration)?;
            generated(&migration, &files)
        }
    };
    drop(signals);
    Ok(done)
}

/// The migration to keep, or none when there is nothing to generate.
async fn decided(
    project: &Path,
    own: &Path,
    args: &GenerateArgs,
    stage: &Cell<Stage>,
) -> Result<Option<Migration>, Ending> {
    let cx = Cx::new(project, own, &WHO);
    stage.set(Stage::Node);
    node_is_there(&cx, &WHO).await?;
    stage.set(Stage::Packages);
    let kit = drizzle_kit(project, &WHO)?;
    stage.set(Stage::Copying);
    // Found out before drizzle-kit runs: a link would be written through.
    for (path, shown) in [
        (project.join("migrations"), "migrations/"),
        (project.join("migrations").join("meta"), "migrations/meta/"),
    ] {
        no_link(&path, shown)?;
    }
    drizzle::copy_migrations(&cx, &WHO)?;

    let mut extra: Vec<&OsStr> = Vec::new();
    if args.empty {
        // In step first: --custom writes its file whether or not.
        stage.set(Stage::InStep);
        match drizzle::generate(&cx, &kit, &[], &WHO).await? {
            Outcome::InStep => {}
            Outcome::Written(_) => return Err(behind(false)),
            Outcome::Asked => return Err(behind(true)),
            other => return Err(stopped(other)),
        }
        extra.push(OsStr::new("--custom"));
    }
    if let Some(name) = &args.name {
        extra.push(OsStr::new("--name"));
        extra.push(OsStr::new(name));
    }
    stage.set(Stage::Generating);
    match drizzle::generate(&cx, &kit, &extra, &WHO).await? {
        Outcome::InStep if args.empty => Err(failed(
            "drizzle-kit wrote no empty migration when asked for one",
        )),
        Outcome::InStep => Ok(None),
        Outcome::Written(written) => {
            stage.set(Stage::Reading);
            let mut written = written.into_iter();
            match (written.next(), written.next()) {
                (Some(file), None) => read_generated(&cx, &file, args.allow_data_loss).map(Some),
                _ => Err(failed("drizzle-kit wrote more than one migration at once")),
            }
        }
        Outcome::Asked => Err(rename_not_generated()),
        other => Err(stopped(other)),
    }
}

/// The ending of what stops the command before a migration is generated.
fn stopped(outcome: Outcome) -> Ending {
    match outcome {
        Outcome::Unreadable { what, stray } => Ending::new(
            1,
            "migrations-unreadable",
            format!(
                "{}; nothing was written",
                drizzle::unreadable_message(&what, stray, &WHO)
            ),
        ),
        Outcome::SchemaFailed(how) => {
            Ending::new(1, "own-schema-failed", drizzle::schema_failed(&how, &WHO))
        }
        Outcome::Written(_) | Outcome::InStep | Outcome::Asked => {
            failed("drizzle-kit did what vibe generate did not expect")
        }
    }
}

fn failed(what: &str) -> Ending {
    Ending::new(
        1,
        "failed",
        format!(
            "{what}; nothing was written. Check that the project's packages are installed whole (run npm install), then run vibe generate again"
        ),
    )
}

/// `--empty` while the schema is not in step.
fn behind(asked: bool) -> Ending {
    let question = if asked {
        " (and drizzle-kit has a question about the change: run vibe generate without --empty to see how to make it)"
    } else {
        ""
    };
    Ending::new(
        1,
        "migrations-behind",
        format!(
            "src/db/schema.ts changed since the last migration in migrations/{question}, so no empty migration was written: generate the pending change first with `vibe generate`, then run `vibe generate --empty` again; nothing was written"
        ),
    )
}

fn rename_not_generated() -> Ending {
    Ending::new(
        1,
        "rename-not-generated",
        "drizzle-kit cannot tell a rename from a removal and an addition: a table or a column disappeared from src/db/schema.ts and another appeared in its place, and without a terminal drizzle-kit asks nothing and writes nothing, so nothing was written. Take the path that needs no question, one step at a time, each followed by vibe generate: 1. add the new column (or table) to src/db/schema.ts, keeping the old one, then run `vibe generate`; 2. copy the values: `vibe generate --empty --name <name>` writes an empty migration, into which you put the one statement that copies the values (UPDATE <table> SET <new> = <old>;); 3. remove the old column (or table) from src/db/schema.ts, then run `vibe generate`, which stops for the data the old one holds: ask the Builder, as it says".to_owned(),
    )
}

/// What `drizzle-kit` wrote in the copy for the migration `file` (a `.sql`
/// file's name), read and decided on.
fn read_generated(cx: &Cx<'_>, file: &str, allow_data_loss: bool) -> Result<Migration, Ending> {
    let copy = cx.copy();
    let meta = copy.join("meta");
    let tag = file.strip_suffix(".sql").unwrap_or(file).to_owned();
    let unread = |what: &str| failed(&format!("the migration drizzle-kit generated {what}"));

    let journal = File::open(meta.join("_journal.json"))
        .ok()
        .and_then(|f| drizzle::journal_bytes(f).ok().flatten())
        .ok_or_else(|| unread("has a journal that cannot be read"))?;
    let entries = serde_json::from_slice::<Journal>(&journal)
        .map_err(|_| unread("has a journal that cannot be read"))?
        .entries;
    let mut tags = entries.iter().rev().map(|e| e.tag.as_str());
    if tags.next() != Some(tag.as_str()) {
        return Err(unread("is not the last one its journal names"));
    }
    let previous = tags.next();
    let number = drizzle::number_of(&tag).ok_or_else(|| unread("has a name without its number"))?;
    let snapshot_name = format!("{number}_snapshot.json");
    let snapshot = drizzle::read_bounded(&meta.join(&snapshot_name), drizzle::MAX_SNAPSHOT_BYTES)
        .ok()
        .flatten()
        .ok_or_else(|| unread("has a snapshot that cannot be read"))?;
    let sql = drizzle::read_bounded(&copy.join(file), MAX_SQL_BYTES)
        .ok()
        .flatten()
        .ok_or_else(|| unread("cannot be read"))?;
    let text = std::str::from_utf8(&sql).map_err(|_| unparseable("its text is not UTF-8"))?;

    let statements = migration_text::parsed(text).map_err(|refused| {
        unparseable(&match refused.words {
            Some(words) => format!(
                "at the statement beginning on line {} of it: {words}",
                refused.line
            ),
            None => format!(
                "from line {}: it cannot say where one of its statements is",
                refused.line
            ),
        })
    })?;
    let decision = data_loss::decide(
        text,
        &statements,
        || {
            let previous = previous.and_then(drizzle::number_of)?;
            drizzle::read_bounded(
                &meta.join(format!("{previous}_snapshot.json")),
                drizzle::MAX_SNAPSHOT_BYTES,
            )
            .ok()
            .flatten()
        },
        false,
    );
    let (destroyed, texts) = (decision.destroyed, decision.statements);
    if !destroyed.is_empty() && !allow_data_loss {
        return Err(data_loss_ending(&destroyed, &texts));
    }
    Ok(Migration {
        tag,
        snapshot_name,
        sql,
        snapshot,
        journal,
        statements: texts,
        destroyed: destroyed.into_iter().map(|(_, d)| d).collect(),
    })
}

fn unparseable(what: &str) -> Ending {
    Ending::new(
        1,
        "migration-unparseable",
        format!(
            "the migration drizzle-kit generated from src/db/schema.ts cannot be read by Postgres's parser, {what}. It comes from what src/db/schema.ts writes as SQL itself (a default or another expression given with sql`...`): correct that in src/db/schema.ts, then run vibe generate again; nothing was written"
        ),
    )
}

/// The stop for data that would be lost: `destroys` (every one, in the
/// order of the statements) and the destroying statements.
fn data_loss_ending(destroyed: &[(usize, Destroyed)], texts: &[String]) -> Ending {
    let statements = data_loss::destroying(destroyed, texts);
    let all: Vec<&Destroyed> = destroyed.iter().map(|(_, d)| d).collect();
    let message = format!(
        "The migration that src/db/schema.ts now needs destroys data, so nothing was written. {}",
        data_loss::ask_the_builder(&lost(&all), "vibe generate")
    );
    let destroys: Vec<&Value> = all.iter().map(|d| &d.json).collect();
    let mut text = format!("{}\n", Theirs::from(message.as_str()));
    text.push_str("The statements that destroy it:\n");
    for statement in &statements {
        push_statement(&mut text, statement);
    }
    Ending::detailed(
        3,
        "data-loss",
        &message,
        Done {
            json: json!({ "error": {
                "reason": "data-loss",
                "message": message,
                "destroys": destroys,
                "statements": statements,
            } }),
            text,
        },
    )
}

/// That `path` of the project (shown as `shown`) is no symbolic link (one
/// that is not there is none).
fn no_link(path: &Path, shown: &str) -> Result<(), Ending> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(Ending::new(
            1,
            "not-a-project",
            format!(
                "{shown} is a symbolic link: vibe generate writes inside the project only. Replace it with a folder of the project's own, then run vibe generate again; nothing was written"
            ),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(project_file(&format!("{shown} cannot be read ({error})"))),
    }
}

fn project_file(what: &str) -> Ending {
    Ending::new(
        1,
        "project-file",
        format!(
            "{what}; nothing was written. Make migrations/ readable and writable by you, then run vibe generate again"
        ),
    )
}

/// What [`write`] did in the project, undone in reverse when a later step
/// fails: the folders it made and the files it wrote. Each written file is
/// a new one (the migration, its snapshot), so removing it is the project
/// as it was; the journal, which replaces the old one, is written last.
#[derive(Default)]
struct Undo<'a> {
    made: Vec<(&'a Path, &'a str)>,
    written: Vec<(&'a Path, &'a str)>,
}

impl<'a> Undo<'a> {
    /// Undoes it all; what could not be undone, by its path in the project.
    fn undo(self) -> Vec<&'a str> {
        let mut left = Vec::new();
        for (path, shown) in self.written.into_iter().rev() {
            if fs::remove_file(path).is_err() {
                left.push(shown);
            }
        }
        // A folder made here is removed only when nothing is in it.
        for (path, shown) in self.made.into_iter().rev() {
            if fs::remove_dir(path).is_err() {
                left.push(shown);
            }
        }
        left
    }
}

/// Why [`write`] stopped: what, and whether it is a link (`not-a-project`)
/// rather than a file problem (`project-file`).
struct Stop {
    what: String,
    link: bool,
}

impl Stop {
    fn file(what: String) -> Self {
        Self { what, link: false }
    }
}

/// That `path` of the project is a folder, and no link: made when it is
/// not there (and then recorded in `done`).
fn folder<'a>(path: &'a Path, shown: &'a str, done: &mut Undo<'a>) -> Result<(), Stop> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(Stop {
            what: format!(
                "{shown} became a symbolic link while vibe generate ran: it writes inside the project only"
            ),
            link: true,
        }),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(Stop::file(format!("{shown} is not a folder"))),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            DirBuilder::new()
                .create(path)
                .map_err(|error| Stop::file(format!("{shown} cannot be made ({error})")))?;
            done.made.push((path, shown));
            Ok(())
        }
        Err(error) => Err(Stop::file(format!("{shown} cannot be read ({error})"))),
    }
}

/// Writes the migration's three files into `migrations/`, each whole, the
/// journal last: all three or none. When a step fails, what was done
/// before it is undone, and the message says whether anything is left.
/// The files, by path, in the order of their paths.
fn write(project: &Path, migration: &Migration) -> Result<Vec<String>, Ending> {
    let migrations = project.join("migrations");
    let meta = migrations.join("meta");
    let files = [
        (
            format!("migrations/{}.sql", migration.tag),
            migrations.join(format!("{}.sql", migration.tag)),
            &migration.sql,
        ),
        (
            format!("migrations/meta/{}", migration.snapshot_name),
            meta.join(&migration.snapshot_name),
            &migration.snapshot,
        ),
        (
            "migrations/meta/_journal.json".to_owned(),
            meta.join("_journal.json"),
            &migration.journal,
        ),
    ];
    let mut done = Undo::default();
    let mut steps = || -> Result<(), Stop> {
        folder(&migrations, "migrations/", &mut done)?;
        folder(&meta, "migrations/meta/", &mut done)?;
        for (shown, path, content) in &files {
            write_whole(path, content, "vibe-generate")
                .map_err(|error| Stop::file(format!("{shown} cannot be written ({error})")))?;
            done.written.push((path, shown));
        }
        Ok(())
    };
    if let Err(Stop { what, link }) = steps() {
        let left = done.undo();
        let after = if left.is_empty() {
            "nothing was left: what was written before it was removed again".to_owned()
        } else {
            format!(
                "these were written or made and could not be removed again, and are left: {}; remove them",
                left.join(", ")
            )
        };
        let fix = if link {
            "replace it with a folder of the project's own, then run vibe generate again"
        } else {
            "make migrations/ writable by you, then run vibe generate again"
        };
        let reason = if link {
            "not-a-project"
        } else {
            "project-file"
        };
        return Err(Ending::new(
            1,
            reason,
            format!("{what}; {after}. To fix it, {fix}"),
        ));
    }
    let mut shown: Vec<String> = files.into_iter().map(|(shown, _, _)| shown).collect();
    shown.sort();
    Ok(shown)
}

fn nothing() -> Done {
    Done {
        json: json!({ "generated": null, "files": [] }),
        text: "The migrations are in step with src/db/schema.ts: there is nothing to generate, and nothing was written.\n".to_owned(),
    }
}

fn generated(migration: &Migration, files: &[String]) -> Done {
    let count = migration.statements.len();
    let destroys: Vec<&Value> = migration.destroyed.iter().map(|d| &d.json).collect();
    let file = format!("migrations/{}.sql", migration.tag);
    let tag = Theirs::from(migration.tag.as_str());
    let s = if count == 1 { "" } else { "s" };
    let mut text = format!("Generated the migration {tag}, with {count} statement{s}");
    if count == 0 {
        text.push_str(": it is empty, for SQL of your own (put it in before committing).\n");
    } else {
        text.push_str(":\n");
        for statement in &migration.statements {
            push_statement(&mut text, statement);
        }
    }
    text.push_str("It is in three files:\n");
    for f in files {
        text.push_str(&format!("    {}\n", Theirs::from(f.as_str())));
    }
    if !migration.destroyed.is_empty() {
        let all: Vec<&Destroyed> = migration.destroyed.iter().collect();
        text.push_str(&format!(
            "When it is applied, this will be lost: {}. Deploying an older Version later does not bring it back.\n",
            Theirs::from(lost(&all).as_str())
        ));
    }
    text.push_str("Read the statements before going on, then commit the three files together.\n");
    Done {
        json: json!({
            "generated": {
                "name": migration.tag,
                "file": file,
                "statements": count,
                "destroys": destroys,
            },
            "files": files,
        }),
        text,
    }
}
