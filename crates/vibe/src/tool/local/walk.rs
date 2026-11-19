//! Whether the project changed (blackbox/CONTRACT.md, "The project as it
//! is now"): every entry of the project but those under a folder named
//! `node_modules`, `.vibe` or `.git`, by its path, its kind, its size and its
//! time of modification, hashed into one value that differs when any of
//! them does.
//!
//! **Bounds.** At most [`MAX_ENTRIES`] entries are looked at; past them
//! the look gives up ([`Look::TooMany`]). No link is followed (an entry's
//! own metadata is read, not its target's), so the walk stays inside the
//! project and ends. It reads the file system: the caller runs it off the
//! async workers (`run::off_thread`).

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The most entries looked at.
pub(super) const MAX_ENTRIES: usize = 20_000;

/// Folders never looked into, wherever they are.
const SKIPPED: [&str; 3] = ["node_modules", ".vibe", ".git"];

/// What a look at the project found.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Look {
    /// The project as it is now, in one value.
    Seen([u8; 32]),
    /// More than [`MAX_ENTRIES`] entries.
    TooMany,
}

/// Looks at the project in `project`; the number of entries looked at.
pub(super) fn look(project: &Path) -> (Look, usize) {
    let mut hash = Sha256::new();
    let mut entries: usize = 0;
    // Folders still to read, each with its path inside the project. Bound:
    // every folder on it is an entry counted below, so at most
    // MAX_ENTRIES.
    let mut folders: Vec<(PathBuf, PathBuf)> = vec![(project.to_path_buf(), PathBuf::new())];
    while let Some((folder, inside)) = folders.pop() {
        let Ok(read) = fs::read_dir(&folder) else {
            // A folder that cannot be read counts as it is: unreadable.
            hash.update(b"unreadable\0");
            hash.update(inside.as_os_str().as_bytes());
            continue;
        };
        let mut found: Vec<(std::ffi::OsString, Option<fs::Metadata>)> = Vec::new();
        for entry in read {
            let Ok(entry) = entry else {
                hash.update(b"unreadable entry\0");
                continue;
            };
            entries = entries.saturating_add(1);
            if entries > MAX_ENTRIES {
                return (Look::TooMany, entries);
            }
            // The entry's own metadata: a link is not followed.
            found.push((entry.file_name(), entry.metadata().ok()));
        }
        // In name order, so that the value does not depend on the order
        // the system lists them in.
        found.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, meta) in found {
            let path = inside.join(&name);
            hash.update(path.as_os_str().as_bytes());
            hash.update([0]);
            let Some(meta) = meta else {
                hash.update(b"gone\0");
                continue;
            };
            let skipped = SKIPPED.iter().any(|s| name.as_bytes() == s.as_bytes());
            if skipped && meta.is_dir() {
                // Only that it is there: nothing of it is looked at, its
                // own time included (what the local run writes in .vibe/
                // changes it).
                hash.update(b"s");
                continue;
            }
            let kind: &[u8] = if meta.is_dir() {
                b"d"
            } else if meta.is_file() {
                b"f"
            } else if meta.file_type().is_symlink() {
                b"l"
            } else {
                b"o"
            };
            hash.update(kind);
            hash.update(meta.len().to_le_bytes());
            hash.update(meta.mtime().to_le_bytes());
            hash.update(meta.mtime_nsec().to_le_bytes());
            if meta.is_dir() {
                folders.push((folder.join(&name), path));
            }
        }
    }
    (Look::Seen(hash.finalize().into()), entries)
}
