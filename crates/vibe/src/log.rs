//! The program's log: one JSON object per line on standard error, each with
//! `event` and `time_ms` (blackbox/CONTRACT.md, "What the log records per
//! request").
//!
//! A record is written whole, under the standard-error lock, at the moment
//! it is made. So the order of the log is the order things happened in:
//! everything about a request is written before its `request` record, which
//! Ingress writes after the request's context has let go of the request.
//!
//! Writing is synchronous. A record is often made inside Application code's
//! call into the host (a print, a host call), where nothing can be awaited,
//! and the order above depends on it. If whoever reads standard error
//! stops reading, the program stops with it.
//!
//! The local run (`vibe request`) sends the records to a file of the
//! project's local state instead ([`to_file`]): its standard error is the
//! Builder's terminal, and the command prints what matters of them itself.
//! That file is bounded: once what was written into it passes
//! [`MAX_FILE_BYTES`], it is emptied and written again from its start
//! (`vibe dev` runs for days).

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// The most a file of records holds before it is started anew.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A file records go to, and how much is in it.
struct Sink {
    file: File,
    written: u64,
}

impl Sink {
    /// Writes one line; the file started anew first when the line would
    /// take it past [`MAX_FILE_BYTES`].
    fn write(&mut self, line: &str) {
        let size = u64::try_from(line.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if self.written.saturating_add(size) > MAX_FILE_BYTES {
            let emptied = self
                .file
                .set_len(0)
                .and_then(|()| self.file.seek(SeekFrom::Start(0)));
            if emptied.is_ok() {
                self.written = 0;
            }
        }
        if writeln!(self.file, "{line}").is_ok() {
            self.written = self.written.saturating_add(size);
        }
    }
}

/// Where records go instead of standard error, once set.
static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// From now on, records are written to `file` and not to standard error.
/// Once per process; `false` when a file was set already.
pub fn to_file(file: File) -> bool {
    let written = file.metadata().map_or(0, |m| m.len());
    SINK.set(Mutex::new(Sink { file, written })).is_ok()
}

/// Writes one record. `fields` should be a JSON object; anything else is
/// written under `value`.
pub fn record(event: &str, fields: Value) {
    let mut object = match fields {
        Value::Object(object) => object,
        other => {
            let mut object = Map::new();
            object.insert("value".into(), other);
            object
        }
    };
    object.insert("event".into(), event.into());
    object.insert("time_ms".into(), now_ms().into());
    let line = Value::Object(object).to_string();
    if let Some(file) = SINK.get() {
        // Held for this one write, never across an await.
        file.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(&line);
        return;
    }
    let mut err = std::io::stderr().lock();
    // A log that cannot be written is not worth stopping the program for.
    writeln!(err, "{line}").and_then(|()| err.flush()).ok();
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}
