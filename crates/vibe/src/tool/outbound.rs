//! `vibe outbound` and `vibe admin outbound add|remove` (blackbox/
//! CONTRACT.md, "Outbound calls: The tool"): the hosts Applications may
//! call, as the Platform administrators listed them, changed one entry at a
//! time.
//!
//! `vibe outbound` reads `GET /v1/outbound-hosts` and prints it as it is
//! with `--json`, one line an entry in words. `add` and `remove` send
//! `PUT` and `DELETE /v1/outbound-hosts/<entry>`; the entry is read before
//! anything is asked, and a text that is not one is exit code `2`. An
//! answer that is not the `200` asked for is the protocol's own refusal or
//! failure, as every admin command's.

use hyper::{Method, StatusCode};
use serde_json::Value;

use super::client::{self, Sign};
use super::credential_file::{Kept, Place};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, failed, get_signed_in, not_success, read_kept, sign_in_expired,
    unexpected,
};
use crate::outbound::{Entry, MAX_FILTER_BYTES, problem_words};

/// The list.
const LIST: &str = "/v1/outbound-hosts";

/// `vibe outbound`.
#[derive(clap::Args)]
pub struct OutboundArgs {
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin outbound`.
#[derive(clap::Args)]
pub struct OutboundAdminArgs {
    #[command(subcommand)]
    pub command: OutboundAdminCommand,
}

/// What `vibe admin outbound` does.
#[derive(clap::Subcommand)]
pub enum OutboundAdminCommand {
    /// Allow Applications to call a host: from the next call on, nothing
    /// deployed again.
    Add(OutboundChangeArgs),
    /// Stop allowing it: from the next call on.
    Remove(OutboundChangeArgs),
}

/// `vibe admin outbound add <host>` and `remove <host>`.
#[derive(clap::Args)]
pub struct OutboundChangeArgs {
    /// The entry: a name, a name with a port, a wildcard (`*.example.com`),
    /// or an address (`203.0.113.7`, `[2001:db8::7]`), each with or without
    /// a port.
    #[arg(value_parser = entry)]
    pub host: String,
    #[command(flatten)]
    pub output: Output,
}

/// An entry, read as the API reads it; the text as typed is what is sent,
/// so that the installation answers with its kept form.
fn entry(text: &str) -> Result<String, String> {
    match Entry::parse(text) {
        Ok(_) => Ok(text.to_owned()),
        Err(problem) => Err(problem_words(problem)),
    }
}

/// `text` as one path segment: every byte but the unreserved ones written
/// `%XX` (the same set the API's paths are read with).
fn path_segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            put!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The day of an RFC 3339 time: its first ten bytes when it has them.
fn day_of(time: &str) -> &str {
    time.get(..10).unwrap_or(time)
}

/// The kept sign-in: exit code `4` when there is none.
fn signed_in() -> Result<Kept, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    read_kept(&place)
}

/// `vibe outbound`: `GET /v1/outbound-hosts`, by anyone signed in.
pub async fn list(_args: &OutboundArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let page = get_signed_in(&kept, LIST).await?;
    let Some(hosts) = page.get("hosts").and_then(Value::as_array) else {
        return Err(unexpected(kept.at(), "its answer has no list of hosts"));
    };
    let mut words = String::new();
    if hosts.is_empty() {
        say!(
            words,
            "No host may be called: nothing leaves an Application until a Platform administrator allows a host, with `vibe admin outbound add <host>`. `vibe outbound` shows what is allowed now."
        );
    }
    for entry in hosts {
        let member =
            |name: &str| Theirs::from(entry.get(name).and_then(Value::as_str).unwrap_or(""));
        let host = member("host");
        let by = member("added_by");
        let day = Theirs::from(day_of(
            entry.get("added_at").and_then(Value::as_str).unwrap_or(""),
        ));
        say!(words, "{host}   added by {by} on {day}");
    }
    Ok(Done {
        json: page,
        text: words,
    })
}

/// Which way a change goes.
#[derive(Clone, Copy)]
pub enum Change {
    Add,
    Remove,
}

impl Change {
    fn method(self) -> Method {
        match self {
            Self::Add => Method::PUT,
            Self::Remove => Method::DELETE,
        }
    }
}

/// `vibe admin outbound add <host>` and `remove <host>`.
pub async fn change(kind: Change, args: &OutboundChangeArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let path = format!("{LIST}/{}", path_segment(&args.host));
    let reply = client::send(
        &super::installation(&kept),
        kind.method(),
        &path,
        Sign::Credential(&kept.credential),
        None,
    )
    .await
    .map_err(|f| failed(kept.at(), f))?;
    match reply.status {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED => return Err(sign_in_expired(&kept)),
        _ => return Err(not_success(kept.at(), &reply)),
    }
    let answer = reply
        .json
        .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON"))?;
    let changed = answer
        .get("changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| unexpected(kept.at(), "its answer does not say whether it changed"))?;
    let written = answer.get("host").and_then(Value::as_str).unwrap_or("");
    let host = Theirs::from(written);
    let mut words = String::new();
    match (kind, changed) {
        (Change::Add, true) => {
            say!(
                words,
                "Applications may now call {host}, from the next call on."
            );
            if written.starts_with("*.") {
                say!(
                    words,
                    "Every name beneath it is allowed, on the same port, from the next call on."
                );
            }
        }
        (Change::Add, false) => {
            say!(
                words,
                "Applications might already call {host}, from the next call on: nothing changed."
            );
        }
        (Change::Remove, true) => {
            say!(
                words,
                "Applications may no longer call {host}, from the next call on."
            );
        }
        (Change::Remove, false) => {
            say!(
                words,
                "Applications could not call {host} before: it was not on the list. Nothing changed."
            );
        }
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// A host asked for in the Operation log's filter: as the list writes an
/// entry, 1 to [`MAX_FILTER_BYTES`] bytes.
pub fn host_filter(text: &str) -> Result<String, String> {
    match Entry::parse(text) {
        Ok(_) if text.len() <= MAX_FILTER_BYTES => Ok(text.to_owned()),
        Ok(_) => Err(format!("not a host: at most {MAX_FILTER_BYTES} bytes")),
        Err(problem) => Err(problem_words(problem)),
    }
}
