//! `drizzle-kit generate` against a copy of `migrations/`, as `vibe check`
//! and `vibe generate` run it (blackbox/CONTRACT.md, "Checking a project:
//! migrations", "Generating migrations"), and the copy read by the command
//! itself.
//!
//! `drizzle-kit` writes its journal even when it writes no migration, so it
//! is only ever given a copy of `migrations/`, in the command's own folder
//! ([`copy_migrations`]). What it did there is read as an [`Outcome`]
//! ([`generate`]).
//!
//! **Bounds.** The copy: at most [`MAX_MIGRATION_ENTRIES`] files and
//! folders, [`MAX_MIGRATION_BYTES`], [`MAX_MIGRATION_DEPTH`] folders deep
//! (past any, the command ends with `project-file`); a journal is read to
//! [`MAX_JOURNAL_BYTES`] (a larger one is unreadable); of the files that do
//! not agree with the journal, [`MAX_NAMED`] are named and the rest
//! counted. `drizzle-kit`'s pipes: [`TEXT_BYTES`] each.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::Path;

use serde::Deserialize;

use super::Ending;
use super::child::Kept;
use super::project_tools::{Cx, TEXT_BYTES, Who, ended, from_to, own_folder_failed, tool_failed};

/// The entries (files and folders) of `migrations/` copied.
const MAX_MIGRATION_ENTRIES: usize = 10_000;

/// The bytes of `migrations/` copied.
const MAX_MIGRATION_BYTES: u64 = 256 * 1024 * 1024;

/// How deep `migrations/` is copied.
const MAX_MIGRATION_DEPTH: usize = 16;

/// The largest journal read.
pub(super) const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// The longest snapshot read, from what `drizzle-kit` wrote or from the
/// project's `migrations/meta/`.
pub(super) const MAX_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;

/// Reads at most `max` bytes of `path`: `None` when there are more.
pub(super) fn read_bounded(path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(u64::try_from(bytes.len())
        .is_ok_and(|n| n <= max)
        .then_some(bytes))
}

/// The number of a journal's tag `<number>_<name>`.
pub(super) fn number_of(tag: &str) -> Option<&str> {
    let (number, _) = tag.split_once('_')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

/// The disagreements between the journal and the `.sql` files named in one
/// message.
const MAX_NAMED: usize = 10;

/// The message of `migrations-unreadable`, the same from `vibe check` and
/// `vibe generate`: what is wrong (`what`, named as the project's), and the
/// fix: bring the folder back as it was last committed, remove a `.sql`
/// file the journal does not name if it was never committed (when there is
/// one: `stray`), then `vibe generate` for the migrations the schema needs.
pub(super) fn unreadable_message(what: &str, stray: bool, who: &Who) -> String {
    let stray = if stray {
        "; a .sql file the journal does not name is not removed by that if it was never committed (git does not know it): remove it yourself"
    } else {
        ""
    };
    let then = if who.command == "vibe generate" {
        "then run `vibe generate` again for the migrations the schema then needs".to_owned()
    } else {
        format!(
            "then generate the migrations the schema then needs with `vibe generate` and run {} again",
            who.command
        )
    };
    format!(
        "{what}. Migrations are never edited by hand: bring migrations/ back as it was last committed (git checkout -- migrations, or git restore migrations){stray}; {then}"
    )
}

/// How much of `migrations/` is left to copy.
struct Budget {
    entries: usize,
    bytes: u64,
}

fn too_large() -> io::Error {
    io::Error::other(format!(
        "it holds more than {MAX_MIGRATION_ENTRIES} files and folders, more than {} MiB, or folders more than {MAX_MIGRATION_DEPTH} deep",
        MAX_MIGRATION_BYTES / (1024 * 1024)
    ))
}

/// Copies the folder `from` into `to` (made): its files and folders, a
/// link to a file as the file, a link to a folder not at all.
fn copy_tree(from: &Path, to: &Path, budget: &mut Budget, depth: usize) -> io::Result<()> {
    if depth > MAX_MIGRATION_DEPTH {
        return Err(too_large());
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        budget.entries = budget.entries.checked_sub(1).ok_or_else(too_large)?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target, budget, depth.saturating_add(1))?;
        } else if let Ok(meta) = fs::metadata(&source)
            && meta.is_file()
        {
            budget.bytes = budget.bytes.checked_sub(meta.len()).ok_or_else(too_large)?;
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Copies the project's `migrations/` into the command's own folder (an
/// empty folder when the project has none).
pub(super) fn copy_migrations(cx: &Cx<'_>, who: &Who) -> Result<(), Ending> {
    let out = cx.copy();
    let own = cx.project.join("migrations");
    if own.is_dir() {
        let mut budget = Budget {
            entries: MAX_MIGRATION_ENTRIES,
            bytes: MAX_MIGRATION_BYTES,
        };
        copy_tree(&own, &out, &mut budget, 0).map_err(|error| {
            Ending::new(
                1,
                "project-file",
                format!(
                    "migrations/ cannot be copied for {} ({error}): make it readable by you and keep it to the project's migrations, then run {} again",
                    who.the, who.command
                ),
            )
        })
    } else {
        fs::create_dir(&out).map_err(|e| own_folder_failed(&e, who))
    }
}

/// The names of the `.sql` files in `dir`.
pub(super) fn sql_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".sql"))
                .take(MAX_MIGRATION_ENTRIES)
                .collect()
        })
        .unwrap_or_default()
}

/// What the command finds wrong in `migrations/` by reading it itself: a
/// file (named as the project's) and what is wrong with it.
struct Unreadable {
    file: String,
    why: String,
    /// Whether a `.sql` file is there that the journal does not name.
    stray: bool,
}

/// The journal `drizzle-kit` keeps: the migrations, in order, by tag.
#[derive(Deserialize)]
pub(super) struct Journal {
    pub entries: Vec<JournalEntry>,
}

#[derive(Deserialize)]
pub(super) struct JournalEntry {
    pub tag: String,
}

/// The bytes of the journal `file`, at most [`MAX_JOURNAL_BYTES`]:
/// `None` when there are more.
pub(super) fn journal_bytes(file: File) -> io::Result<Option<Vec<u8>>> {
    let mut text = Vec::new();
    file.take(MAX_JOURNAL_BYTES.saturating_add(1))
        .read_to_end(&mut text)?;
    if u64::try_from(text.len()).map_or(true, |n| n > MAX_JOURNAL_BYTES) {
        return Ok(None);
    }
    Ok(Some(text))
}

/// Reads the copy of `migrations/` as `drizzle-kit` wrote it: the journal
/// and every snapshot parse as JSON, and the journal and the `.sql` files
/// agree (each migration the journal names has its `.sql` file, each `.sql`
/// file is named). A folder with no journal and no `.sql` file holds no
/// migration and is as it should be.
fn read_migrations(copy: &Path, sql: &BTreeSet<String>) -> Result<(), Unreadable> {
    let journal_path = copy.join("meta").join("_journal.json");
    let journal_file = "migrations/meta/_journal.json".to_owned();
    let file = match File::open(&journal_path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return match sql.first() {
                None => Ok(()),
                Some(first) => Err(Unreadable {
                    file: journal_file,
                    why: format!("is not there, and migrations/{first} is"),
                    stray: true,
                }),
            };
        }
        Err(error) => {
            return Err(Unreadable {
                file: journal_file,
                why: format!("cannot be read ({error})"),
                stray: false,
            });
        }
    };
    let text = journal_bytes(file)
        .map_err(|error| Unreadable {
            file: journal_file.clone(),
            why: format!("cannot be read ({error})"),
            stray: false,
        })?
        .ok_or_else(|| Unreadable {
            file: journal_file.clone(),
            why: format!("is larger than {} MiB", MAX_JOURNAL_BYTES / (1024 * 1024)),
            stray: false,
        })?;
    let journal: Journal = serde_json::from_slice(&text).map_err(|error| Unreadable {
        file: journal_file.clone(),
        why: format!("is not the journal drizzle-kit writes ({error})"),
        stray: false,
    })?;

    // Every snapshot, without keeping it: the copy is bounded already.
    let mut snapshots: Vec<String> = fs::read_dir(copy.join("meta"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with("_snapshot.json"))
                .collect()
        })
        .unwrap_or_default();
    snapshots.sort();
    for name in snapshots {
        let file = format!("migrations/meta/{name}");
        let opened = File::open(copy.join("meta").join(&name)).map_err(|error| Unreadable {
            file: file.clone(),
            why: format!("cannot be read ({error})"),
            stray: false,
        })?;
        serde_json::from_reader::<_, serde::de::IgnoredAny>(io::BufReader::new(opened)).map_err(
            |error| Unreadable {
                file,
                why: format!("is not a snapshot drizzle-kit writes ({error})"),
                stray: false,
            },
        )?;
    }

    let tags: BTreeSet<String> = journal
        .entries
        .into_iter()
        .map(|e| format!("{}.sql", e.tag))
        .collect();
    let stray = sql.difference(&tags).next().is_some();
    let mut wrong = Vec::new();
    for gone in tags.difference(sql) {
        wrong.push(format!(
            "migrations/{gone} is named in migrations/meta/_journal.json and is not there"
        ));
    }
    for stray in sql.difference(&tags) {
        wrong.push(format!(
            "migrations/{stray} is not named in migrations/meta/_journal.json"
        ));
    }
    if wrong.is_empty() {
        return Ok(());
    }
    let more = wrong.len().saturating_sub(MAX_NAMED);
    wrong.truncate(MAX_NAMED);
    let mut why = wrong.join("; ");
    if more > 0 {
        why.push_str(&format!("; and {more} more"));
    }
    Err(Unreadable {
        file: "migrations/".to_owned(),
        why: format!("does not agree with its journal: {why}"),
        stray,
    })
}

/// What `drizzle-kit generate` did with the copy of `migrations/`.
pub(super) enum Outcome {
    /// `migrations/` cannot be read: what is wrong, named as the project's
    /// (the message's beginning: see [`unreadable_message`]), and whether a
    /// `.sql` file is there that the journal does not name.
    Unreadable { what: String, stray: bool },
    /// It wrote these `.sql` files (by name), which were not there before.
    Written(BTreeSet<String>),
    /// Nothing to generate: the schema is in step with the migrations.
    InStep,
    /// It has a question about the change (a rename?) and wrote nothing.
    Asked,
    /// It could not load `src/db/schema.ts`: how it ended and its words.
    SchemaFailed(String),
}

/// `drizzle-kit generate --dialect postgresql --schema src/db/schema.ts`
/// with `extra`, against the copy of `migrations/` ([`copy_migrations`]),
/// which is also read by the command itself.
///
/// How it is decided, from what can be observed reliably:
/// 1. The command reads the copy itself ([`read_migrations`]). When the
///    journal or a snapshot does not parse, or the journal and the `.sql`
///    files disagree, it is unreadable, naming the file; with
///    `drizzle-kit`'s words too when it did not end in step.
/// 2. Else a new `.sql` file in the copy is written.
/// 3. Else `drizzle-kit`'s sentence "No schema changes" on its standard
///    output (where it writes it, and not where the schema's own output
///    lands with an error) is in step.
/// 4. Else its error "Interactive prompts require a TTY" on standard error
///    (a question about a rename) is asked.
/// 5. Else, when its words name a file of the copy, it could not read
///    `migrations/`: unreadable.
/// 6. Else it could not load `src/db/schema.ts`.
pub(super) async fn generate(
    cx: &Cx<'_>,
    drizzle_kit: &Path,
    extra: &[&OsStr],
    who: &Who,
) -> Result<Outcome, Ending> {
    let out = cx.copy();
    let before = sql_files(&out);
    let read = read_migrations(&out, &before);
    // drizzle-kit puts `./` in front of the folder it is given, so it is
    // given the copy's path relative to the project.
    let out_from_project = fs::canonicalize(&out)
        .map(|out| from_to(cx.project, &out))
        .map_err(|e| own_folder_failed(&e, who))?;
    let mut args = vec![
        drizzle_kit.as_os_str(),
        OsStr::new("generate"),
        OsStr::new("--dialect"),
        OsStr::new("postgresql"),
        OsStr::new("--schema"),
        OsStr::new("src/db/schema.ts"),
        OsStr::new("--out"),
        out_from_project.as_os_str(),
    ];
    args.extend_from_slice(extra);
    let ran = cx
        .run(&args, Kept::up_to(TEXT_BYTES), Kept::up_to(TEXT_BYTES))
        .await
        .map_err(|error| tool_failed("node", &format!("cannot be started ({error})"), who))?;
    let written: BTreeSet<String> = sql_files(&out).difference(&before).cloned().collect();
    let (out_text, err_text) = (ran.out.text(), ran.err.text());
    let in_step = out_text.lines().any(|l| l.contains("No schema changes"));
    let asked = err_text.lines().any(|l| {
        l.trim_start()
            .starts_with("Error: Interactive prompts require a TTY")
    });
    let tool_said = cx.words(&err_text, &out_text);
    Ok(if let Err(Unreadable { file, why, stray }) = read {
        let mut what = format!("{file} {why}");
        if written.is_empty() && !in_step && !asked && !tool_said.is_empty() {
            what.push_str(&format!("; drizzle-kit says: {tool_said}"));
        }
        Outcome::Unreadable { what, stray }
    } else if !written.is_empty() {
        Outcome::Written(written)
    } else if in_step {
        Outcome::InStep
    } else if asked {
        Outcome::Asked
    } else if cx.names_the_copy(&err_text) || cx.names_the_copy(&out_text) {
        Outcome::Unreadable {
            what: format!(
                "drizzle-kit cannot read migrations/ (it {}: {tool_said})",
                ended(ran.status)
            ),
            stray: false,
        }
    } else {
        Outcome::SchemaFailed(format!("it {}: {tool_said}", ended(ran.status)))
    })
}

/// The message of `own-schema-failed`, with how `drizzle-kit` ended and its
/// words.
pub(super) fn schema_failed(how: &str, who: &Who) -> String {
    format!(
        "drizzle-kit cannot load src/db/schema.ts ({how}): correct src/db/schema.ts (it must be there, load without throwing, and import only what is there), then run {} again",
        who.command
    )
}
