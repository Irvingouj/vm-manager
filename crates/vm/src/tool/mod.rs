//! The tool: the same program run on a Builder's machine by their Coding
//! agent (ADR 0006, ADR 0015; blackbox/CONTRACT.md, "The tool").
//!
//! `vm sign-in`, `vm sign-out`, `vm whoami`, `vm resources` and `vm
//! applications`, each with `--json`. Each command is one short run on one
//! thread: a current-thread runtime, at most a few requests to the
//! installation ([`client`]), the credential file ([`credential_file`]), and
//! an [`Ending`] whose exit code says what kind of ending it was:
//!
//! - `0` done; `1` failed; `2` the command line is wrong (clap's, and the
//!   checks below); `3` refused, or waiting for the Builder; `4` not signed
//!   in.
//!
//! Every message says what to do next. None holds the credential. Without
//! `--json`, text that came from the installation reaches words only through
//! [`Theirs`], which escapes control characters. Output is written through
//! handles whose errors are taken: a closed standard output or standard
//! error ends the command, never with a panic.

mod client;
mod credential_file;
mod theirs;

use std::io::{self, Write};
use std::net::SocketAddr;
use std::process::ExitCode;

use hyper::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use client::{Failure, Installation, Reply, Sign};
use credential_file::{Found, Kept, Place, Secret};
use theirs::Theirs;

use crate::registry::is_dns_label;
use crate::sign_in::MAX_STAND_IN_VALUE_BYTES;

/// Appends a formatted line to a `String`.
macro_rules! say {
    ($text:expr, $($arg:tt)*) => {{
        $text.push_str(&format!($($arg)*));
        $text.push('\n');
    }};
}

/// Appends formatted text to a `String`.
macro_rules! put {
    ($text:expr, $($arg:tt)*) => {
        $text.push_str(&format!($($arg)*))
    };
}

/// What `vm --help` says about exit codes.
pub const EXIT_CODES: &str = "\
Exit codes:
  0  done
  1  failed: the installation cannot be reached, it answered something unexpected, or a file cannot be read or written
  2  the command line is wrong
  3  refused, or waiting for the Builder: the platform said no, or the next step is a person's (the message says whose and what)
  4  not signed in (no credential, or one that expired or was signed out): run vm sign-in, then the command again

With --json, standard output is one JSON object; a command that does not end with 0 prints { \"error\": { \"reason\", \"message\" } } there.";

/// `--json`, which every command of the tool takes.
#[derive(clap::Args)]
pub struct Output {
    /// Print exactly one JSON object on standard output.
    #[arg(long)]
    pub json: bool,
}

/// `vm sign-in`.
#[derive(clap::Args)]
pub struct SignInArgs {
    /// The installation's domain; the tool sends Host: api.<domain>.
    #[arg(long, value_parser = domain)]
    pub domain: String,
    /// Where the installation listens: a loopback address and port, such as
    /// 127.0.0.1:8080 or [::1]:8080 (plain HTTP until installing brings
    /// HTTPS).
    #[arg(long, value_parser = loopback)]
    pub connect: SocketAddr,
    /// Development only, while sign-in is the stand-in: the User to sign in
    /// as.
    #[arg(long, value_parser = stand_in_value)]
    pub stand_in_user: String,
    /// Development only: the User's Groups, comma-separated.
    #[arg(long, value_parser = stand_in_value)]
    pub stand_in_groups: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

/// A command of the tool.
pub enum Command {
    SignIn(SignInArgs),
    SignOut(Output),
    Whoami(Output),
    Resources(Output),
    Applications(Output),
}

/// A message: the tool's own words, or the installation's (a refusal's
/// `message`), which are escaped when printed as words.
enum Said {
    Ours(String),
    Theirs(Theirs),
}

impl Said {
    fn words(&self) -> String {
        match self {
            Self::Ours(text) => text.clone(),
            Self::Theirs(text) => text.to_string(),
        }
    }

    fn json(&self) -> Value {
        match self {
            Self::Ours(text) => json!(text),
            Self::Theirs(text) => json!(text),
        }
    }
}

/// How a command that did not end with `0` ended.
struct Ending {
    code: u8,
    /// Only ever printed in JSON.
    reason: Value,
    message: Said,
}

impl Ending {
    fn new(code: u8, reason: &str, message: String) -> Self {
        Self {
            code,
            reason: json!(reason),
            message: Said::Ours(message),
        }
    }

    fn credential_file(message: String) -> Self {
        Self::new(1, "credential-file", message)
    }
}

/// What a command printed when it ended with `0`.
struct Done {
    json: Value,
    text: String,
}

/// Runs a command of the tool and prints its outcome.
pub fn run(command: Command) -> ExitCode {
    let json = match &command {
        Command::SignIn(args) => args.output.json,
        Command::SignOut(o)
        | Command::Whoami(o)
        | Command::Resources(o)
        | Command::Applications(o) => o.json,
    };
    let outcome = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime.block_on(async {
            match command {
                Command::SignIn(args) => sign_in(&args).await,
                Command::SignOut(_) => sign_out().await,
                Command::Whoami(_) => whoami().await,
                Command::Resources(_) => resources().await,
                Command::Applications(_) => applications().await,
            }
        }),
        Err(error) => Err(Ending::new(
            1,
            "failed",
            format!("the tool could not start ({error}): run the command again"),
        )),
    };
    let (code, written) = match outcome {
        Ok(done) => {
            let written = if json {
                write_to(&mut io::stdout().lock(), &json_line(&done.json))
            } else {
                write_to(&mut io::stdout().lock(), &done.text)
            };
            (0, written)
        }
        Err(ending) => {
            let written = if json {
                let body = json!({ "error": { "reason": ending.reason, "message": ending.message.json() } });
                write_to(&mut io::stdout().lock(), &json_line(&body))
            } else {
                write_to(
                    &mut io::stderr().lock(),
                    &format!("{}\n", ending.message.words()),
                )
            };
            (ending.code, written)
        }
    };
    match written {
        Ok(()) => ExitCode::from(code),
        Err(error) => {
            // Standard error may be gone too: then there is no one to tell.
            write_to(
                &mut io::stderr().lock(),
                &format!("vm: the output could not be written ({error}); the command's outcome is lost: run it again with its output kept\n"),
            )
            .ok();
            ExitCode::from(1)
        }
    }
}

/// `value` as one line of JSON in which no control character is raw.
/// serde_json escapes C0 on its own but leaves DEL and C1 as they are,
/// which JSON allows; a terminal does not, so they are escaped here too.
/// Outside strings the compact form holds no control character, so this
/// changes only strings, and only how they are written.
fn json_line(value: &Value) -> String {
    let mut line = String::new();
    for c in value.to_string().chars() {
        if c.is_control() {
            put!(line, "\\u{:04x}", u32::from(c));
        } else {
            line.push(c);
        }
    }
    line.push('\n');
    line
}

/// Writes all of `text` and flushes; the error, if any, is the caller's.
fn write_to(out: &mut impl Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes())?;
    out.flush()
}

// The command line's own checks: each failure is exit code 2, by clap.

fn domain(text: &str) -> Result<String, String> {
    let domain = text.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || !domain.split('.').all(is_dns_label) {
        return Err(
            "not a domain name: use DNS labels joined by dots, such as vm.example.com".into(),
        );
    }
    Ok(domain)
}

fn loopback(text: &str) -> Result<SocketAddr, String> {
    let address: SocketAddr = text.parse().map_err(|_| {
        "not an address and port: use a loopback address, such as 127.0.0.1:8080 or [::1]:8080"
            .to_owned()
    })?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(
            "the tool speaks plain HTTP, so only to a loopback address and port, such as 127.0.0.1:8080 or [::1]:8080"
                .into(),
        );
    }
    Ok(address)
}

fn stand_in_value(text: &str) -> Result<String, String> {
    if text.is_empty()
        || text.len() > MAX_STAND_IN_VALUE_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(format!(
            "use 1 to {MAX_STAND_IN_VALUE_BYTES} bytes without control characters"
        ));
    }
    Ok(text.to_owned())
}

/// The installation of what is kept.
fn installation(kept: &Kept) -> Installation<'_> {
    Installation {
        domain: &kept.domain,
        connect: kept.connect,
    }
}

/// `text` as one word of a shell command, quoted when it needs to be.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.,:@/+=[]".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The whole `vm sign-in` command that signs in again as what is kept.
fn sign_in_again(kept: &Kept) -> String {
    let mut command = format!(
        "vm sign-in --domain {} --connect {} --stand-in-user {}",
        shell_word(&kept.domain),
        kept.connect,
        shell_word(&kept.user.to_string())
    );
    if !kept.groups.is_empty() {
        put!(
            command,
            " --stand-in-groups {}",
            shell_word(&listed(&kept.groups, ","))
        );
    }
    command
}

fn not_signed_in() -> Ending {
    Ending::new(
        4,
        "not-signed-in",
        "not signed in: run `vm sign-in --domain <domain> --connect <ip:port> --stand-in-user <id>`, then run this command again".into(),
    )
}

fn sign_in_expired(kept: &Kept) -> Ending {
    Ending::new(
        4,
        "sign-in-expired",
        format!(
            "the sign-in to {} has ended (it expired or was signed out): run `{}`, then run this command again",
            kept.domain,
            sign_in_again(kept)
        ),
    )
}

fn unreachable(connect: SocketAddr, why: &str) -> Ending {
    Ending::new(
        1,
        "unreachable",
        format!(
            "the installation at {connect} cannot be reached ({why}): check that it is running and that the address is right, then run the command again"
        ),
    )
}

fn unexpected(connect: SocketAddr, what: &str) -> Ending {
    Ending::new(
        1,
        "unexpected-answer",
        format!(
            "what answered at {connect} is not an installation of this platform, or not one this tool understands ({what}): check the address and the tool's version, then run the command again"
        ),
    )
}

fn failed(connect: SocketAddr, failure: Failure) -> Ending {
    match failure {
        Failure::Unreachable(why) => unreachable(connect, &why),
        Failure::Unexpected(what) => unexpected(connect, &what),
    }
}

/// An answer that is not the success asked for: the API's own refusal (its
/// reason and message), else what kind of answer it was.
fn not_success(connect: SocketAddr, reply: &Reply) -> Ending {
    let error = reply.json.as_ref().and_then(|body| body.get("error"));
    let reason = error.and_then(|e| e.get("reason")).and_then(Value::as_str);
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str);
    match (reason, message) {
        (Some(reason), Some(message)) => {
            // The platform could not do it now: a failure, not a refusal.
            let code = if reply.status.is_server_error() { 1 } else { 3 };
            Ending {
                code,
                reason: json!(reason),
                message: Said::Theirs(Theirs::from(message)),
            }
        }
        _ => unexpected(
            connect,
            &format!(
                "it answered {} where the API answers otherwise",
                reply.status
            ),
        ),
    }
}

/// Reads what is kept: not signed in when nothing is.
fn read_kept(place: &Place) -> Result<Kept, Ending> {
    match place.read().map_err(Ending::credential_file)? {
        Found::Nothing => Err(not_signed_in()),
        Found::Kept(kept) => Ok(kept),
    }
}

/// `GET <path>` with the kept credential; the answer's JSON on `200`.
async fn get_signed_in(kept: &Kept, path: &str) -> Result<Value, Ending> {
    let reply = client::send(
        &installation(kept),
        Method::GET,
        path,
        Sign::Credential(&kept.credential),
        None,
    )
    .await
    .map_err(|f| failed(kept.connect, f))?;
    match reply.status {
        StatusCode::OK => reply
            .json
            .ok_or_else(|| unexpected(kept.connect, "its answer is not JSON")),
        StatusCode::UNAUTHORIZED => Err(sign_in_expired(kept)),
        _ => Err(not_success(kept.connect, &reply)),
    }
}

/// `value` read as `T`, else an unexpected answer.
fn shaped<T: for<'de> Deserialize<'de>>(connect: SocketAddr, value: Value) -> Result<T, Ending> {
    serde_json::from_value(value)
        .map_err(|_| unexpected(connect, "its answer does not have the fields asked for"))
}

/// What `POST /v1/tool-credentials` answers.
#[derive(Deserialize)]
struct Made {
    credential: String,
    user: Theirs,
    groups: Vec<Theirs>,
    platform_administrator: bool,
    expires_at: Theirs,
}

/// What `GET /v1/me` answers.
#[derive(Deserialize)]
struct Me {
    user: Theirs,
    groups: Vec<Theirs>,
    platform_administrator: bool,
}

/// `texts` in words, joined by `between`.
fn listed(texts: &[Theirs], between: &str) -> String {
    let mut joined = String::new();
    for (n, text) in texts.iter().enumerate() {
        if n > 0 {
            joined.push_str(between);
        }
        put!(joined, "{text}");
    }
    joined
}

fn who_in_words(domain: &str, user: &Theirs, groups: &[Theirs], administrator: bool) -> String {
    let mut text = format!("Signed in to {domain} as {user}");
    if groups.is_empty() {
        text.push_str(", in no Group.\n");
    } else {
        say!(text, ", in the Groups {}.", listed(groups, ", "));
    }
    if administrator {
        text.push_str("You are a Platform administrator.\n");
    }
    text
}

async fn sign_in(args: &SignInArgs) -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    // The folder first, so that a credential is never made that cannot be
    // kept.
    place.prepare().map_err(Ending::credential_file)?;
    // An earlier credential is ended once the new one is kept; one that
    // cannot be read is simply replaced.
    let earlier = match place.read() {
        Ok(Found::Kept(kept)) => Some(kept),
        Ok(Found::Nothing) | Err(_) => None,
    };
    let at = Installation {
        domain: &args.domain,
        connect: args.connect,
    };
    let reply = client::send(
        &at,
        Method::POST,
        "/v1/tool-credentials",
        Sign::StandIn {
            user: &args.stand_in_user,
            groups: args.stand_in_groups.as_deref(),
        },
        Some(b"{}"),
    )
    .await
    .map_err(|f| failed(args.connect, f))?;
    match reply.status {
        StatusCode::CREATED => {}
        StatusCode::UNAUTHORIZED => {
            return Err(Ending::new(
                3,
                "sign-in-refused",
                format!(
                    "the installation at {} ({}) does not accept this sign-in: it takes the stand-in only when started with --stand-in-sign-in; ask whoever runs it, or sign in to another",
                    args.connect, args.domain
                ),
            ));
        }
        _ => return Err(not_success(args.connect, &reply)),
    }
    let made: Made = shaped(
        args.connect,
        reply
            .json
            .ok_or_else(|| unexpected(args.connect, "its answer is not JSON"))?,
    )?;
    let credential = Secret::new(made.credential)
        .ok_or_else(|| unexpected(args.connect, "the credential it gave is not one"))?;
    let kept = Kept {
        domain: args.domain.clone(),
        connect: args.connect,
        user: made.user,
        groups: made.groups,
        credential,
        expires_at: made.expires_at,
    };
    if let Err(message) = place.write(&kept) {
        // Not kept, so not left alive at the installation either.
        end_at(&kept).await;
        return Err(Ending::credential_file(message));
    }
    if let Some(earlier) = earlier {
        end_at(&earlier).await;
    }
    let mut text = who_in_words(
        &kept.domain,
        &kept.user,
        &kept.groups,
        made.platform_administrator,
    );
    say!(
        text,
        "The sign-in lasts until {}; then run vm sign-in again.",
        kept.expires_at
    );
    Ok(Done {
        json: json!({
            "user": kept.user,
            "groups": kept.groups,
            "platform_administrator": made.platform_administrator,
            "domain": kept.domain,
            "expires_at": kept.expires_at,
        }),
        text,
    })
}

/// Ends `kept`'s credential at its installation; whether it is known to be
/// ended there now (also when it had already ended).
async fn end_at(kept: &Kept) -> bool {
    let reply = client::send(
        &installation(kept),
        Method::DELETE,
        "/v1/tool-credentials/current",
        Sign::Credential(&kept.credential),
        None,
    )
    .await;
    matches!(
        reply,
        Ok(Reply {
            status: StatusCode::NO_CONTENT | StatusCode::UNAUTHORIZED,
            ..
        })
    )
}

async fn sign_out() -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let found = place.read();
    let (domain, ended) = match &found {
        Ok(Found::Kept(kept)) => (Some(kept.domain.as_str()), end_at(kept).await),
        // A file that cannot be read is removed all the same.
        Ok(Found::Nothing) | Err(_) => (None, false),
    };
    let removed = place.remove().map_err(Ending::credential_file)?;
    let text = match (removed, ended) {
        (false, _) => "Not signed in: there was nothing to sign out of.\n".to_owned(),
        (true, true) => format!(
            "Signed out of {}: the credential is ended at the installation and removed from this machine.\n",
            domain.unwrap_or("the installation")
        ),
        (true, false) => format!(
            "Signed out: the credential is removed from this machine, but {} could not be told to end it; it ends by itself when its time is up.\n",
            domain.unwrap_or("the installation")
        ),
    };
    Ok(Done {
        json: json!({ "signed_out": removed, "ended_at_installation": removed && ended }),
        text,
    })
}

async fn whoami() -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let me: Me = shaped(kept.connect, get_signed_in(&kept, "/v1/me").await?)?;
    Ok(Done {
        text: who_in_words(
            &kept.domain,
            &me.user,
            &me.groups,
            me.platform_administrator,
        ),
        json: json!({
            "user": me.user,
            "groups": me.groups,
            "platform_administrator": me.platform_administrator,
            "domain": kept.domain,
        }),
    })
}

/// A Resource as the tool shows it: its public face and the Builder's
/// Access level, nothing of its connection or what is inside it.
#[derive(Deserialize, serde::Serialize)]
struct ListedResource {
    name: Theirs,
    kind: Theirs,
    description: Theirs,
    owner: Theirs,
    contact: Theirs,
    your_access: Option<Theirs>,
}

#[derive(Deserialize)]
struct ResourceList {
    resources: Vec<ListedResource>,
}

async fn resources() -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let list: ResourceList = shaped(kept.connect, get_signed_in(&kept, "/v1/resources").await?)?;
    let mut text = String::new();
    if list.resources.is_empty() {
        say!(
            text,
            "No Resource is registered on {} yet. A Platform administrator registers them: ask one.",
            kept.domain
        );
    }
    for r in &list.resources {
        say!(text, "{} ({})", r.name, r.kind);
        say!(text, "  {}", r.description);
        say!(text, "  Owner: {}", r.owner);
        match &r.your_access {
            None => {
                say!(text, "  Your access: no access: ask {}", r.contact);
            }
            Some(level) => {
                let meaning = if level.is("structure") {
                    " (its structure only: rows cannot be read at this level)"
                } else if level.is("read") {
                    " (rows can be read)"
                } else if level.is("write") {
                    " (rows can be read and written)"
                } else {
                    ""
                };
                say!(text, "  Your access: {level}{meaning}");
                say!(text, "  For more, ask: {}", r.contact);
            }
        }
        text.push('\n');
    }
    Ok(Done {
        json: json!({ "resources": list.resources }),
        text,
    })
}

/// An Application as `GET /v1/applications` lists it.
#[derive(Deserialize, serde::Serialize)]
struct ListedApplication {
    name: Theirs,
    description: Option<Theirs>,
    address: Theirs,
    contributor: bool,
    serving_version: Option<u64>,
}

#[derive(Deserialize)]
struct ApplicationList {
    applications: Vec<ListedApplication>,
}

async fn applications() -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let list: ApplicationList = shaped(
        kept.connect,
        get_signed_in(&kept, "/v1/applications").await?,
    )?;
    let mut text = String::new();
    if list.applications.is_empty() {
        say!(
            text,
            "You contribute to no Application on {} yet. Create one with POST /v1/applications on the platform's API, or ask a Contributor of one to add you.",
            kept.domain
        );
    }
    for a in &list.applications {
        say!(text, "{}", a.name);
        if let Some(description) = &a.description {
            say!(text, "  {description}");
        }
        say!(text, "  Address: {}", a.address);
        if a.contributor {
            text.push_str("  You are a Contributor.\n");
        }
        match a.serving_version {
            Some(number) => {
                say!(text, "  Serving: Version {number}");
            }
            None => text.push_str("  Serving: no Version yet\n"),
        }
        text.push('\n');
    }
    Ok(Done {
        json: json!({ "applications": list.applications }),
        text,
    })
}
