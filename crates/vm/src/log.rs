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

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

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
