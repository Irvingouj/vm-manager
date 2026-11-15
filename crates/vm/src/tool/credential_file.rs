//! The credential file on the Builder's machine (blackbox/CONTRACT.md, "The
//! tool: The credential").
//!
//! - The folder is `$XDG_CONFIG_HOME/vm/` when that variable holds an
//!   absolute path, else `$HOME/.config/vm/`. When it is not there it is made
//!   with mode `0700` (set again after making, whatever the umask).
//! - The file is written whole to a file of its own in the folder, made new
//!   with mode `0600` (set again on the open file, whatever the umask),
//!   synced, then renamed over the old one: a crash leaves the old file or
//!   the new one, never half of one, and the file is never readable by
//!   anyone else, whatever mode an earlier file had.
//! - The credential is a [`Secret`], which does not print. Problems name
//!   the path and never repeat what the file holds.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::theirs::Theirs;
use crate::registry::is_dns_label;

/// The file's name in the folder.
const FILE_NAME: &str = "tool-credential.json";

/// What the file is written to first, in the same folder.
const TEMPORARY_NAME: &str = "tool-credential.json.new";

/// The form of the file this program writes.
const FORM: u32 = 1;

/// The largest file read: far above what the tool writes.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The shortest and longest credential text taken from the installation or
/// the file.
const CREDENTIAL_LENGTH: std::ops::RangeInclusive<usize> = 43..=512;

/// A tool credential's text on the Builder's machine. No `Debug`, no
/// `Display`: it is read only to be sent, through [`Secret::expose`].
pub struct Secret(String);

impl Secret {
    /// The text, if it can be a credential: 43 to 512 characters of
    /// letters, digits, `-` and `_`.
    pub fn new(text: String) -> Option<Self> {
        let fits = CREDENTIAL_LENGTH.contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        fits.then_some(Self(text))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// What the tool keeps after signing in.
pub struct Kept {
    /// Checked: DNS labels joined by dots.
    pub domain: String,
    pub connect: SocketAddr,
    /// As the installation said.
    pub user: Theirs,
    pub groups: Vec<Theirs>,
    pub credential: Secret,
    /// RFC 3339, as the installation said.
    pub expires_at: Theirs,
}

/// The file as written, borrowed from what is kept.
#[derive(Serialize)]
struct Written<'a> {
    form: u32,
    domain: &'a str,
    connect: SocketAddr,
    user: &'a Theirs,
    groups: &'a [Theirs],
    credential: &'a str,
    expires_at: &'a Theirs,
}

/// The file as read.
#[derive(Deserialize)]
struct OnDisk {
    form: u32,
    domain: String,
    connect: SocketAddr,
    user: Theirs,
    groups: Vec<Theirs>,
    credential: String,
    expires_at: Theirs,
}

/// What reading found.
pub enum Found {
    /// No file: not signed in.
    Nothing,
    Kept(Kept),
}

/// Where the credential is kept.
pub struct Place {
    folder: PathBuf,
}

impl Place {
    /// The folder from the environment; a message when there is none.
    pub fn from_environment() -> Result<Self, String> {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            let config = PathBuf::from(config);
            if config.is_absolute() {
                return Ok(Self {
                    folder: config.join("vm"),
                });
            }
        }
        match std::env::var_os("HOME").map(PathBuf::from) {
            Some(home) if home.is_absolute() => Ok(Self {
                folder: home.join(".config").join("vm"),
            }),
            _ => Err(
                "neither XDG_CONFIG_HOME nor HOME is an absolute path, so the tool has no folder to keep its credential in: set HOME, then run the command again"
                    .to_owned(),
            ),
        }
    }

    pub fn file(&self) -> PathBuf {
        self.folder.join(FILE_NAME)
    }

    /// Reads what is kept.
    pub fn read(&self) -> Result<Found, String> {
        let path = self.file();
        let mut bytes = Vec::new();
        let read = File::open(&path).and_then(|file| {
            file.take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(drop)
        });
        match read {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Found::Nothing),
            Err(error) => {
                return Err(format!(
                    "the credential file {} cannot be read ({error}): make it readable by you, or remove it and run `vm sign-in` again",
                    path.display()
                ));
            }
        }
        let garbled = || {
            format!(
                "the credential file {} does not hold what `vm sign-in` wrote there: remove it, then run `vm sign-in` again",
                path.display()
            )
        };
        if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
            return Err(garbled());
        }
        let on_disk: OnDisk = serde_json::from_slice(&bytes).map_err(|_| garbled())?;
        if on_disk.form != FORM
            || !on_disk.connect.ip().is_loopback()
            || on_disk.domain.is_empty()
            || !on_disk.domain.split('.').all(is_dns_label)
        {
            return Err(garbled());
        }
        let credential = Secret::new(on_disk.credential).ok_or_else(garbled)?;
        Ok(Found::Kept(Kept {
            domain: on_disk.domain,
            connect: on_disk.connect,
            user: on_disk.user,
            groups: on_disk.groups,
            credential,
            expires_at: on_disk.expires_at,
        }))
    }

    /// Makes the folder if it is not there; a message naming it otherwise.
    pub fn prepare(&self) -> Result<(), String> {
        let folder = &self.folder;
        let problem = |what: &str, error: &io::Error| {
            format!(
                "the credential folder {} {what} ({error}): make it a folder you can write, then run `vm sign-in` again",
                folder.display()
            )
        };
        match fs::metadata(folder) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(format!(
                "the credential folder {} cannot be made: something that is not a folder is there; move it away, then run `vm sign-in` again",
                folder.display()
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(folder)
                    .map_err(|e| problem("cannot be made", &e))?;
                fs::set_permissions(folder, Permissions::from_mode(0o700))
                    .map_err(|e| problem("cannot be made private", &e))
            }
            Err(error) => Err(problem("cannot be reached", &error)),
        }
    }

    /// Writes `kept` in place of what was kept, whole or not at all. The
    /// folder must have been prepared.
    pub fn write(&self, kept: &Kept) -> Result<(), String> {
        let path = self.file();
        let temporary = self.folder.join(TEMPORARY_NAME);
        let problem = |error: &io::Error| {
            format!(
                "the credential file {} cannot be written ({error}): make its folder writable by you, then run `vm sign-in` again",
                path.display()
            )
        };
        let mut text = serde_json::to_vec(&Written {
            form: FORM,
            domain: &kept.domain,
            connect: kept.connect,
            user: &kept.user,
            groups: &kept.groups,
            credential: kept.credential.expose(),
            expires_at: &kept.expires_at,
        })
        .map_err(|e| problem(&io::Error::other(e.to_string())))?;
        text.push(b'\n');
        // What a crash between writing and renaming left.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(problem(&error)),
        }
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| {
                file.set_permissions(Permissions::from_mode(0o600))?;
                file.write_all(&text)?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temporary, &path));
        if let Err(error) = written {
            drop(fs::remove_file(&temporary));
            return Err(problem(&error));
        }
        // The rename itself made lasting; a folder that cannot be synced
        // leaves the file written all the same.
        if let Ok(folder) = File::open(&self.folder) {
            drop(folder.sync_all());
        }
        Ok(())
    }

    /// Removes what is kept; `false` when there was nothing.
    pub fn remove(&self) -> Result<bool, String> {
        let path = self.file();
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "the credential file {} cannot be removed ({error}): make its folder writable by you, then run `vm sign-out` again",
                path.display()
            )),
        }
    }
}
