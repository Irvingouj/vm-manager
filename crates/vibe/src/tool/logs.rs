//! `vibe logs <application> [--request <id>] [--after <cursor>] [--limit
//! <n>] [--follow] [--json]` (blackbox/CONTRACT.md, "Versions, going back,
//! and logs: The commands").
//!
//! Without `--follow`: one `GET /v1/applications/<name>/logs`, printed as
//! the other commands print. With it, the newest entries, then the entries
//! after the last `next`, asked about once every [`POLL`] (each request
//! with its own end, [`client`]'s), printed as they come: one JSON object a
//! line with `--json`, the entry with `"application"` beside its fields.
//! It ends with `0` on `SIGINT`, `SIGTERM` or `SIGHUP` ([`Signals`]); when
//! the installation stops answering or the sign-in ends, with that ending,
//! printed after what was printed; when standard output is closed, with
//! `1` and a line on standard error, never a panic.
//!
//! Everything of an entry is the Application's or a User's: in words it
//! reaches the terminal only through [`Theirs`], a line end inside a
//! message beginning a new, indented line.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::credential_file::Kept;
use super::project_tools::{Bounded, Signals, Who};
use super::theirs::Theirs;
use super::versions::signed_in_for;
use super::{Done, Ending, Output, get_signed_in, json_line, shaped};

/// `vibe logs`.
#[derive(clap::Args)]
pub struct LogsArgs {
    /// The Application, by name.
    pub application: String,
    /// Only the entry of this request: a response's x-vm-request-id.
    #[arg(long)]
    pub request: Option<String>,
    /// Only the entries after this cursor: the `next` of an earlier run.
    #[arg(long)]
    pub after: Option<String>,
    /// How many entries, 1 to 1000 (100 without it): the newest, or the
    /// oldest after --after.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: Option<u16>,
    /// Go on printing new entries as they are made, until interrupted.
    #[arg(long)]
    pub follow: bool,
    #[command(flatten)]
    pub output: Output,
}

/// How often `--follow` asks again.
const POLL: Duration = Duration::from_secs(1);

/// The entries one ask of `--follow` takes; when it gets this many, it asks
/// again at once.
const FOLLOW_LIMIT: u16 = 1000;

/// How the signals' messages name the command.
const WHO: Who = Who {
    command: "vibe logs",
    the: "vibe logs",
    folder: "vibe-logs",
};

/// One answer of the API.
#[derive(Deserialize)]
struct Page {
    entries: Vec<Value>,
    next: String,
}

pub(super) async fn logs(args: &LogsArgs) -> Result<Done, Ending> {
    let name = args.application.as_str();
    let kept = signed_in_for(name)?;
    if args.follow {
        return follow(args, &kept).await;
    }
    let page = read(
        &kept,
        name,
        args.request.as_deref(),
        args.after.as_deref(),
        args.limit,
    )
    .await?;
    let mut text = String::new();
    if page.entries.is_empty() {
        let since = if args.after.is_some() {
            " after that cursor"
        } else {
            ""
        };
        writeln!(text, "No entries{since} yet for {name}.").ok();
    }
    for entry in &page.entries {
        entry_words(entry, &mut text);
    }
    let next = Theirs::from(page.next.as_str());
    writeln!(
        text,
        "Next: {next} (vibe logs {name} --after {next} prints what comes after these)."
    )
    .ok();
    Ok(Done {
        json: json!({ "application": name, "entries": page.entries, "next": page.next }),
        text,
    })
}

/// `--follow`: prints as it goes, until a signal or an ending.
async fn follow(args: &LogsArgs, kept: &Kept) -> Result<Done, Ending> {
    if stdout_was_closed() {
        return Err(Ending::new(
            1,
            "output-closed",
            "vibe logs --follow has no standard output (it was closed when the command started), so it would print nothing for ever; it stopped. Run it with its output kept".into(),
        ));
    }
    let mut signals = Signals::listen(&WHO)?;
    // Ends only by a signal or its own ending; the far end is tokio's own.
    match signals.within(Duration::MAX, following(args, kept)).await {
        Bounded::Ended(Err(ending)) => Err(ending),
        Bounded::Ended(Ok(())) | Bounded::TimedOut | Bounded::Interrupted(_) => Ok(Done::printed()),
    }
}

async fn following(args: &LogsArgs, kept: &Kept) -> Result<(), Ending> {
    let name = args.application.as_str();
    let json = args.output.json;
    let request = args.request.as_deref();
    let first = read(kept, name, request, args.after.as_deref(), args.limit).await?;
    if first.entries.is_empty() && !json {
        print(&format!(
            "No entries yet for {name}; waiting for new ones (interrupt to end).\n"
        ))?;
    }
    print_entries(name, &first.entries, json)?;
    let mut next = first.next;
    loop {
        tokio::time::sleep(POLL).await;
        loop {
            let page = read(kept, name, request, Some(&next), Some(FOLLOW_LIMIT)).await?;
            print_entries(name, &page.entries, json)?;
            next = page.next;
            if page.entries.len() < usize::from(FOLLOW_LIMIT) {
                break;
            }
        }
    }
}

fn print_entries(name: &str, entries: &[Value], json: bool) -> Result<(), Ending> {
    let mut text = String::new();
    for entry in entries {
        if json {
            let mut object = entry.clone();
            if let Some(fields) = object.as_object_mut() {
                fields.insert("application".into(), json!(name));
            }
            text.push_str(&json_line(&object));
        } else {
            entry_words(entry, &mut text);
        }
    }
    if text.is_empty() {
        return Ok(());
    }
    print(&text)
}

/// Whether standard output was closed when the program started. Rust's
/// start-up opens `/dev/null` read and write on a closed standard stream,
/// and writes to it then succeed, so `--follow` would never learn that
/// nobody reads it: that is told apart from output sent to `/dev/null` on
/// purpose (opened for writing only) by its access mode.
fn stdout_was_closed() -> bool {
    use rustix::fs::{OFlags, fcntl_getfl, fstat, stat};
    use std::os::fd::AsFd;
    let out = io::stdout();
    let Ok(flags) = fcntl_getfl(out.as_fd()) else {
        return true;
    };
    if !flags.contains(OFlags::RDWR) {
        return false;
    }
    match (fstat(out.as_fd()), stat("/dev/null")) {
        (Ok(ours), Ok(null)) => ours.st_dev == null.st_dev && ours.st_ino == null.st_ino,
        _ => false,
    }
}

/// Writes `text` on standard output now; a closed one ends the command.
fn print(text: &str) -> Result<(), Ending> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| {
            Ending::new(
                1,
                "output-closed",
                format!("vibe logs could not write its output ({error}), so it stopped"),
            )
        })
}

/// `GET /v1/applications/<name>/logs` with these parameters.
async fn read(
    kept: &Kept,
    name: &str,
    request: Option<&str>,
    after: Option<&str>,
    limit: Option<u16>,
) -> Result<Page, Ending> {
    let mut query = String::new();
    let mut add = |key: &str, value: &str| {
        query.push(if query.is_empty() { '?' } else { '&' });
        query.push_str(key);
        query.push('=');
        query.push_str(&encoded(value));
    };
    if let Some(limit) = limit {
        add("limit", &limit.to_string());
    }
    if let Some(after) = after {
        add("after", after);
    }
    if let Some(request) = request {
        add("request", request);
    }
    shaped(
        kept.connect,
        get_signed_in(kept, &format!("/v1/applications/{name}/logs{query}")).await?,
    )
}

/// `text` for a URL's query: every byte but letters, digits and `-._~`
/// written `%XX`.
fn encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").ok();
        }
    }
    out
}

/// An entry as words are made of it.
#[derive(Deserialize)]
struct EntryWords {
    time: Theirs,
    request: Theirs,
    version: u64,
    user: Theirs,
    method: Theirs,
    path: Theirs,
    status: u64,
    printed: Vec<PrintedWords>,
    uncaught: Vec<String>,
    truncated: bool,
    dropped_before: u64,
}

#[derive(Deserialize)]
struct PrintedWords {
    level: Theirs,
    message: String,
    loading: bool,
}

/// One entry in words: its first line, its printed lines, its uncaught
/// errors indented. An entry not of the API's shape is shown as JSON.
fn entry_words(entry: &Value, text: &mut String) {
    let Ok(entry) = EntryWords::deserialize(entry) else {
        text.push_str(&super::json_line(entry));
        return;
    };
    if entry.dropped_before > 0 {
        writeln!(
            text,
            "({} entries dropped here: they were made faster than the platform could keep them)",
            entry.dropped_before
        )
        .ok();
    }
    let cut = if entry.truncated {
        "  (cut: past a bound of what an entry keeps)"
    } else {
        ""
    };
    writeln!(
        text,
        "{}  request {}  user {}  Version {}  {} {}  {}{cut}",
        entry.time,
        entry.request,
        entry.user,
        entry.version,
        entry.method,
        entry.path,
        entry.status,
    )
    .ok();
    for printed in &entry.printed {
        // A line the script printed while it was loaded for the request.
        let loading = if printed.loading { " (loading)" } else { "" };
        let head = format!("  {}{loading}: ", printed.level);
        lines(text, &head, "      ", &printed.message);
    }
    for error in &entry.uncaught {
        lines(text, "  uncaught: ", "      ", error);
    }
}

/// `message` after `head`, each of its further lines on a line of its own
/// after `indent`; every line through [`Theirs`].
fn lines(text: &mut String, head: &str, indent: &str, message: &str) {
    for (n, line) in message.split('\n').enumerate() {
        let lead = if n == 0 { head } else { indent };
        writeln!(text, "{lead}{}", Theirs::from(line)).ok();
    }
}
