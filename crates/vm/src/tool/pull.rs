//! `vm pull <resource> [--project <directory>] [--json]` (blackbox/
//! CONTRACT.md, "Pulling a Resource's structure: The tool").
//!
//! The Resource's name is checked on the command line, before it is part
//! of a path. The project is checked first (its `manifest.ts`; neither
//! generated file at its path written by someone else), then the structure
//! is asked for, then the two files are written: each compared with what
//! is there first, so that an unchanged file is not touched (its bytes and
//! modification time stay), and otherwise written whole to a file of its
//! own beside it, synced, and renamed over it. Nothing is written when the
//! pull is refused or fails.
//!
//! The definition is file content, not words: it is written as answered
//! (once its first line shows it is a definition). What is printed of the
//! installation's (the fingerprint, the Access level, the unsupported
//! columns) reaches words only through [`Theirs`].

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::credential_file::Place;
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, file_problem as project_file, get_signed_in, project_of, read_kept,
    shaped, unexpected,
};
use crate::resources::is_resource_name;
use crate::structure::FIRST_LINE as SCHEMA_FIRST_LINE;

/// `vm pull`.
#[derive(clap::Args)]
pub struct PullArgs {
    /// The Resource whose structure to pull.
    #[arg(value_parser = resource_name)]
    pub resource: String,
    /// The project to write into; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    #[command(flatten)]
    pub output: Output,
}

/// The first line of the generated link.
pub(super) const INDEX_FIRST_LINE: &str = "// Generated. Do not edit.";

/// The largest generated file read to compare: above what a pull answers.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

fn resource_name(text: &str) -> Result<String, String> {
    if is_resource_name(text) {
        Ok(text.to_owned())
    } else {
        Err("not a Resource's name: 1 to 63 lower-case letters, digits and underscores, starting with a letter".into())
    }
}

/// What `GET /v1/resources/<name>/structure` answers.
#[derive(Deserialize)]
struct Answered {
    resource: String,
    fingerprint: String,
    your_access: Theirs,
    /// Written into the project as it is; never printed.
    schema_ts: String,
    unsupported: Vec<UnsupportedColumn>,
}

#[derive(Deserialize, Serialize)]
struct UnsupportedColumn {
    schema: Theirs,
    table: Theirs,
    column: Theirs,
    #[serde(rename = "type")]
    printed: Theirs,
}

/// One generated file of the project.
struct Generated {
    /// Relative to the project, as printed.
    shown: String,
    path: PathBuf,
    first_line: &'static str,
}

pub(super) async fn pull(args: &PullArgs) -> Result<Done, Ending> {
    let project = project_of(args.project.as_deref(), "vm pull")?;
    let name = &args.resource;
    let folder = project.join("resources").join(name);
    let files = [
        Generated {
            shown: format!("resources/{name}/schema.ts"),
            path: folder.join("schema.ts"),
            first_line: SCHEMA_FIRST_LINE,
        },
        Generated {
            shown: format!("resources/{name}/index.ts"),
            path: folder.join("index.ts"),
            first_line: INDEX_FIRST_LINE,
        },
    ];
    no_link(&project, name)?;
    let mut present: Vec<Option<Vec<u8>>> = Vec::with_capacity(files.len());
    for file in &files {
        present.push(generated_or_none(file)?);
    }

    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let answered: Answered = shaped(
        kept.connect,
        get_signed_in(&kept, &format!("/v1/resources/{name}/structure")).await?,
    )?;
    let fingerprint_shape = answered
        .fingerprint
        .strip_prefix("sha256:")
        .is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    let definition_shape = answered
        .schema_ts
        .strip_prefix(SCHEMA_FIRST_LINE)
        .is_some_and(|rest| rest.starts_with('\n'));
    if &answered.resource != name || !fingerprint_shape || !definition_shape {
        return Err(unexpected(
            kept.connect,
            "its answer is not the structure of the Resource asked for",
        ));
    }

    let relations = folder.join("relations.ts").exists();
    let link = link(name, relations);
    let contents = [answered.schema_ts.as_bytes(), link.as_bytes()];
    // Checked again just before writing: a file made meanwhile by someone
    // else is not overwritten either, nor a link made meanwhile written
    // through.
    no_link(&project, name)?;
    for (file, was) in files.iter().zip(present.iter_mut()) {
        *was = generated_or_none(file)?;
    }
    fs::create_dir_all(&folder).map_err(|error| {
        file_problem(
            &format!("the folder resources/{name}"),
            "cannot be made",
            &error,
        )
    })?;
    let mut changed = [false, false];
    for ((file, content), (was, flag)) in files
        .iter()
        .zip(contents)
        .zip(present.iter().zip(changed.iter_mut()))
    {
        if was.as_deref() == Some(content) {
            continue;
        }
        write_whole(&file.path, content)
            .map_err(|error| file_problem(&file.shown, "cannot be written", &error))?;
        *flag = true;
    }

    let mut text = format!(
        "Pulled the structure of {name} (your access: {}; fingerprint {}).\n",
        answered.your_access, answered.fingerprint
    );
    for (file, flag) in files.iter().zip(changed) {
        let what = if flag { "written" } else { "unchanged" };
        text.push_str(&format!("  {}: {what}\n", file.shown));
    }
    if !answered.unsupported.is_empty() {
        text.push_str("Columns of a type the definition cannot express, typed unknown:\n");
    }
    for u in &answered.unsupported {
        text.push_str(&format!(
            "  {}.{}, column {}: {} is typed unknown\n",
            u.schema, u.table, u.column, u.printed
        ));
    }
    let listed: Vec<_> = files
        .iter()
        .zip(changed)
        .map(|(file, flag)| json!({ "path": file.shown, "changed": flag }))
        .collect();
    Ok(Done {
        json: json!({
            "resource": name,
            "fingerprint": answered.fingerprint,
            "your_access": answered.your_access,
            "files": listed,
            "unsupported": answered.unsupported,
        }),
        text,
    })
}

/// The generated link of `name` (blackbox/CONTRACT.md, "A project"), with
/// the `relations` lines when `relations`.
fn link(name: &str, relations: bool) -> String {
    let mut text = format!("{INDEX_FIRST_LINE}\nimport type * as schema from \"./schema.ts\";\n");
    if relations {
        text.push_str("import type * as relations from \"./relations.ts\";\n");
    }
    let structure = if relations {
        "typeof schema & typeof relations"
    } else {
        "typeof schema"
    };
    text.push_str(&format!(
        "\ndeclare module \"@vm/sdk\" {{\n  interface SharedStructures {{\n    \"{name}\": {structure};\n  }}\n}}\n"
    ));
    text
}

/// That none of `resources`, `resources/<name>` and the two files in it is
/// a symbolic link (each looked at itself, never followed), so that the
/// tool writes inside the project only. What is not there is not a link.
fn no_link(project: &Path, name: &str) -> Result<(), Ending> {
    let shown = [
        "resources".to_owned(),
        format!("resources/{name}"),
        format!("resources/{name}/schema.ts"),
        format!("resources/{name}/index.ts"),
    ];
    for path in &shown {
        match fs::symlink_metadata(project.join(path)) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Ending::new(
                    1,
                    "not-a-project",
                    format!(
                        "{path} is a symbolic link: vm pull writes inside the project only. Replace it with a folder or file of the project's own, then run vm pull again; nothing was written"
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(file_problem(path, "cannot be read", &error)),
        }
    }
    Ok(())
}

/// What is at `file`'s path: nothing, or a file the tool wrote (its bytes,
/// when it is small enough to compare). Anything else is `not-generated`.
fn generated_or_none(file: &Generated) -> Result<Option<Vec<u8>>, Ending> {
    let opened = match File::open(&file.path) {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_problem(&file.shown, "cannot be read", &error)),
    };
    let not_generated = || {
        Ending::new(
            1,
            "not-generated",
            format!(
                "{} was not written by vm pull (it does not begin with `{}`): move it away or remove it, then run vm pull again; nothing was written",
                file.shown, file.first_line
            ),
        )
    };
    match opened.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(not_generated()),
        Err(error) => return Err(file_problem(&file.shown, "cannot be read", &error)),
    }
    let mut bytes = Vec::new();
    opened
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| file_problem(&file.shown, "cannot be read", &error))?;
    let begins = bytes
        .strip_prefix(file.first_line.as_bytes())
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"\n"));
    if !begins {
        return Err(not_generated());
    }
    let whole = u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_FILE_BYTES);
    // A file too large to compare is rewritten: it differs from any answer.
    Ok(Some(if whole { bytes } else { Vec::new() }))
}

/// Writes `content` to `path` whole or not at all: to a file of its own
/// beside it, synced, then renamed over it.
fn write_whole(path: &Path, content: &[u8]) -> io::Result<()> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = folder.join(format!(".{file_name}.vm-pull"));
    // What a crash between writing and renaming left.
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(content)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = written {
        drop(fs::remove_file(&temporary));
        return Err(error);
    }
    if let Ok(folder) = File::open(folder) {
        drop(folder.sync_all());
    }
    Ok(())
}

fn file_problem(what: &str, how: &str, error: &io::Error) -> Ending {
    project_file(what, how, error, "vm pull")
}
