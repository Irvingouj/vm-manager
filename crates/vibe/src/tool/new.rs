//! `vm new <name> [--directory <directory>] [--json]` (blackbox/
//! CONTRACT.md, "Creating a project").
//!
//! The name is checked on the command line, as the API checks an
//! Application's. The directory (`--directory`, or `./<name>`) must be
//! missing (it is made, with its parents) or an empty folder; anything else
//! is refused before anything is written. Then each file of the template
//! ([`template`]) is made, in the order of their paths, with the folders it
//! needs; a file that is there already is not overwritten. It needs no
//! installation, no sign-in and no other program.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::template;
use super::theirs::Theirs;
use super::{Done, Ending, Output, file_problem};
use crate::registry::{NameProblem, check_name};

/// `vm new`.
#[derive(clap::Args)]
pub struct NewArgs {
    /// The Application's name: a lower-case DNS label (letters, digits and
    /// hyphens), which becomes its address.
    #[arg(value_parser = application_name)]
    pub name: String,
    /// Where to make the project: a missing or empty folder. Without it,
    /// ./<name>.
    #[arg(long)]
    pub directory: Option<PathBuf>,
    #[command(flatten)]
    pub output: Output,
}

/// What to run in a new project, in order.
const NEXT: [&str; 2] = ["npm install", "vm check"];

fn application_name(text: &str) -> Result<String, String> {
    match check_name(text) {
        Ok(()) => Ok(text.to_owned()),
        Err(NameProblem::Invalid) => Err("not an Application's name: 1 to 63 lower-case letters, digits and hyphens, not starting or ending with a hyphen, and not beginning with xn--".into()),
        Err(NameProblem::Reserved) => Err("that name is one of the platform's own (api, auth, dashboard, git, admin, www): choose another".into()),
    }
}

pub(super) fn new(args: &NewArgs) -> Result<Done, Ending> {
    let dir: &Path = args
        .directory
        .as_deref()
        .unwrap_or_else(|| Path::new(&args.name));
    let shown = dir.display().to_string();
    prepare(dir, &shown)?;
    let files = template::files(&args.name);
    for file in &files {
        write_new(&dir.join(&file.path), &file.bytes)
            .map_err(|(how, error)| file_problem(&file.path, how, &error, "vm new"))?;
    }
    let project = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let project = project.to_string_lossy();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    let mut text = format!(
        "Made the project {} in {}:\n",
        args.name,
        Theirs::from(project.as_ref())
    );
    for path in &paths {
        text.push_str(&format!("  {path}\n"));
    }
    text.push_str("AGENTS.md says how to work in it; manifest.ts names the Application.\nNext, in the project's folder, run:\n");
    for command in NEXT {
        text.push_str(&format!("  {command}\n"));
    }
    Ok(Done {
        json: json!({
            "project": project,
            "name": args.name,
            "files": paths,
            "next": NEXT,
        }),
        text,
    })
}

/// That `dir` is an empty folder, made (with its parents) if it is not
/// there.
fn prepare(dir: &Path, shown: &str) -> Result<(), Ending> {
    let shown = Theirs::from(shown);
    match fs::metadata(dir) {
        Ok(meta) if !meta.is_dir() => Err(Ending::new(
            1,
            "not-a-directory",
            format!(
                "{shown} is there and is not a folder: give another with --directory; nothing was written"
            ),
        )),
        Ok(_) => {
            let mut entries = fs::read_dir(dir).map_err(|error| {
                file_problem(&shown.to_string(), "cannot be read", &error, "vm new")
            })?;
            if entries.next().is_some() {
                Err(Ending::new(
                    1,
                    "directory-not-empty",
                    format!(
                        "{shown} is not empty: give a missing or empty folder with --directory; nothing was written"
                    ),
                ))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir_all(dir)
            .map_err(|error| file_problem(&shown.to_string(), "cannot be made", &error, "vm new")),
        Err(error) => Err(file_problem(
            &shown.to_string(),
            "cannot be read",
            &error,
            "vm new",
        )),
    }
}

/// Makes the file at `path` with `bytes`, and the folders it is in; never
/// over a file that is there.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), (&'static str, std::io::Error)> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)
            .map_err(|e| ("cannot be written (its folder cannot be made)", e))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| ("cannot be written", e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| ("cannot be written", e))
}
