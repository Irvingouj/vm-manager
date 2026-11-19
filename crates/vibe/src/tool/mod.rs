//! The tool: the same program run on a Builder's machine by their Coding
//! agent (ADR 0006, ADR 0015; blackbox/CONTRACT.md, "The tool").
//!
//! `vibe sign-in` (through the browser: [`browser`]), `vibe admin
//! connect-oidc` ([`admin`]), `vibe sign-out`, `vibe whoami`, `vibe resources`, `vibe
//! applications`, `vibe pull` ([`pull`]), `vibe new` ([`new`]), `vibe check`
//! ([`check`]), `vibe generate` ([`generate`]), `vibe deploy` ([`deploy`]),
//! `vibe versions` and `vibe go-back` ([`versions`]), `vibe logs` ([`logs`]),
//! `vibe show`, `vibe share`, `vibe unshare` and `vibe contributors` ([`sharing`]),
//! each with `--json`. Each command is one short run on one
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

mod admin;
mod browser;
mod check;
mod child;
mod client;
mod credential_file;
mod data_loss;
mod deploy;
mod drizzle;
mod generate;
mod local;
mod logs;
mod new;
mod project_tools;
mod pull;
mod sharing;
mod template;
mod test_users;
mod theirs;
mod versions;

pub use admin::AdminArgs;
pub use check::CheckArgs;
pub use deploy::DeployArgs;
pub use generate::GenerateArgs;
pub use local::{
    ChangeHistoryArgs, DevArgs, LocalArgs, LocalCommand, OperationLogArgs, RequestArgs, ResetArgs,
};
pub use logs::LogsArgs;
pub use new::NewArgs;
pub use pull::PullArgs;
pub use sharing::{ChangeArgs, ContributorsArgs, ContributorsCommand, ShowArgs};
pub use versions::{GoBackArgs, VersionsArgs};

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hyper::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use client::{Failure, Installation, Reply, Sign};
use credential_file::{Found, Kept, Place, Secret};
use theirs::Theirs;

use crate::registry::is_dns_label;
use crate::sign_in::MAX_STAND_IN_VALUE_BYTES;

/// What `vibe --help` says about exit codes.
pub const EXIT_CODES: &str = "\
Exit codes:
  0  done
  1  failed: the installation cannot be reached, it answered something unexpected, or a file cannot be read or written
  2  the command line is wrong
  3  refused, or waiting for the Builder: the platform said no, or the next step is a person's (the message says whose and what)
  4  not signed in (no credential, or one that expired or was signed out): run vibe sign-in, then the command again

With --json, standard output is one JSON object; a command that does not end with 0 prints { \"error\": { \"reason\", \"message\" } } there.";

/// `--json`, which every command of the tool takes.
#[derive(clap::Args)]
pub struct Output {
    /// Print exactly one JSON object on standard output.
    #[arg(long)]
    pub json: bool,
}

/// `vibe sign-in`.
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
    pub stand_in_user: Option<String>,
    /// Development only: the User's Groups, comma-separated.
    #[arg(long, value_parser = stand_in_value, requires = "stand_in_user")]
    pub stand_in_groups: Option<String>,
    /// Do not open a browser here: show an address to open on any device,
    /// and a check code to type there.
    #[arg(long)]
    pub no_browser: bool,
    /// How long to wait for the sign-in in the browser, in seconds.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(10..=600))]
    pub timeout: u64,
    #[command(flatten)]
    pub output: Output,
}

/// A command of the tool.
pub enum Command {
    SignIn(SignInArgs),
    Admin(AdminArgs),
    SignOut(Output),
    Whoami(Output),
    Resources(Output),
    Applications(Output),
    Pull(PullArgs),
    New(NewArgs),
    Check(CheckArgs),
    Generate(GenerateArgs),
    Deploy(DeployArgs),
    Versions(VersionsArgs),
    GoBack(GoBackArgs),
    Logs(LogsArgs),
    Request(RequestArgs),
    Dev(DevArgs),
    Local(LocalArgs),
    Show(ShowArgs),
    Share(ChangeArgs<true>),
    Unshare(ChangeArgs<true>),
    Contributors(ContributorsArgs),
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
    /// What is printed instead of the error alone: with `--json` this
    /// object (which holds the `error`), else these words.
    detail: Option<Box<Done>>,
}

impl Ending {
    fn new(code: u8, reason: &str, message: String) -> Self {
        Self {
            code,
            reason: json!(reason),
            message: Said::Ours(message),
            detail: None,
        }
    }

    /// A check that found problems: `message` holds text of the project's
    /// and the installation's, and `report` is what is printed.
    fn check_failed(message: &str, report: Done) -> Self {
        Self::detailed(1, "check-failed", message, report)
    }

    /// An ending that prints more than its error: `detail` (whose JSON
    /// holds the `error`, with `reason` and `message`). `message` may hold
    /// text of the project's.
    fn detailed(code: u8, reason: &str, message: &str, detail: Done) -> Self {
        Self {
            code,
            reason: json!(reason),
            message: Said::Theirs(Theirs::from(message)),
            detail: Some(Box::new(detail)),
        }
    }

    /// An ending whose `message` holds text of the installation's or of
    /// the project's: escaped when printed as words, as it is in JSON.
    fn theirs(code: u8, reason: &str, message: &str) -> Self {
        Self {
            code,
            reason: json!(reason),
            message: Said::Theirs(Theirs::from(message)),
            detail: None,
        }
    }

    /// The same ending with `sentence` (the tool's own words) after its
    /// message, and `field: value` beside its reason in the JSON error.
    fn amended(self, sentence: &str, field: &str, value: Value) -> Self {
        let join = |message: &str| {
            let message = message.trim_end();
            let ended = if message.is_empty() || message.ends_with('.') {
                ""
            } else {
                "."
            };
            format!("{message}{ended} {sentence}")
        };
        let (mut json, text) = match self.detail {
            Some(detail) => {
                let Done { json, text } = *detail;
                (json, join(&text))
            }
            None => (
                json!({ "error": { "reason": &self.reason, "message": self.message.json() } }),
                join(&self.message.words()),
            ),
        };
        if let Some(error) = json.get_mut("error").and_then(Value::as_object_mut) {
            if let Some(Value::String(message)) = error.get_mut("message") {
                *message = join(message);
            }
            error.insert(field.into(), value);
        }
        Self {
            code: self.code,
            reason: self.reason,
            // Not printed: the detail is.
            message: self.message,
            detail: Some(Box::new(Done {
                json,
                text: format!("{text}\n"),
            })),
        }
    }

    /// What [`run`] prints of this ending: its exit code, the JSON (with
    /// `--json`, on standard output) and the words (on standard error).
    fn printed(self) -> (u8, Done) {
        match self.detail {
            Some(report) => (self.code, *report),
            None => {
                let json =
                    json!({ "error": { "reason": self.reason, "message": self.message.json() } });
                let text = format!("{}\n", self.message.words());
                (self.code, Done { json, text })
            }
        }
    }

    /// The ending that prints `report` with exit code `code`: one
    /// [`Ending::printed`] gave, sent from elsewhere (a `vibe dev` that
    /// answered the command).
    fn from_printed(code: u8, report: Done) -> Self {
        let reason = report
            .json
            .get("error")
            .and_then(|error| error.get("reason"))
            .cloned()
            .unwrap_or(Value::Null);
        Self {
            code,
            reason,
            message: Said::Ours(String::new()),
            detail: Some(Box::new(report)),
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

impl Done {
    /// A command that printed as it went (`vibe logs --follow`): nothing more
    /// is printed at its end.
    fn printed() -> Self {
        Self {
            json: Value::Null,
            text: String::new(),
        }
    }
}

/// The longest the end of a command waits for a blocking task of its own.
const RUNTIME_END_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs a command of the tool and prints its outcome.
pub fn run(command: Command) -> ExitCode {
    let json = match &command {
        Command::SignIn(args) => args.output.json,
        Command::Admin(args) => args.json(),
        Command::Pull(args) => args.output.json,
        Command::New(args) => args.output.json,
        Command::Check(args) => args.output.json,
        Command::Generate(args) => args.output.json,
        Command::Deploy(args) => args.output.json,
        Command::Versions(args) => args.output.json,
        Command::GoBack(args) => args.output.json,
        Command::Logs(args) => args.output.json,
        Command::Request(args) => args.output.json,
        Command::Dev(args) => args.output.json,
        Command::Local(args) => args.json(),
        Command::Show(args) => args.output.json,
        Command::Share(args) | Command::Unshare(args) => args.json(),
        Command::Contributors(args) => args.json(),
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
        Ok(runtime) => {
            let outcome = runtime.block_on(async {
                match command {
                    Command::SignIn(args) => sign_in(&args).await,
                    Command::Admin(args) => admin::admin(&args).await,
                    Command::SignOut(_) => sign_out().await,
                    Command::Whoami(_) => whoami().await,
                    Command::Resources(_) => resources().await,
                    Command::Applications(_) => applications().await,
                    Command::Pull(args) => pull::pull(&args).await,
                    Command::New(args) => new::new(&args),
                    Command::Check(args) => check::check(&args).await,
                    Command::Generate(args) => generate::generate(&args).await,
                    Command::Deploy(args) => deploy::deploy(&args).await,
                    Command::Versions(args) => versions::versions(&args).await,
                    Command::GoBack(args) => versions::go_back(&args).await,
                    Command::Logs(args) => logs::logs(&args).await,
                    Command::Request(args) => local::request(&args).await,
                    Command::Dev(args) => local::dev(&args).await,
                    Command::Local(args) => local::local(&args).await,
                    Command::Show(args) => sharing::show(&args).await,
                    Command::Share(args) => sharing::change(sharing::Change::Share, &args).await,
                    Command::Unshare(args) => {
                        sharing::change(sharing::Change::Unshare, &args).await
                    }
                    Command::Contributors(args) => match &args.command {
                        ContributorsCommand::Add(args) => {
                            sharing::change(sharing::Change::AddContributors, args).await
                        }
                        ContributorsCommand::Remove(args) => {
                            sharing::change(sharing::Change::RemoveContributors, args).await
                        }
                    },
                }
            });
            // A blocking task still running (a write to an output that
            // takes nothing, `vibe dev`'s) is waited for a bounded time, not
            // for ever.
            runtime.shutdown_timeout(RUNTIME_END_WAIT);
            outcome
        }
        Err(error) => Err(Ending::new(
            1,
            "failed",
            format!("the tool could not start ({error}): run the command again"),
        )),
    };
    let (code, written) = match outcome {
        Ok(done) => {
            let written = if done.json.is_null() && done.text.is_empty() {
                Ok(())
            } else if json {
                write_to(&mut io::stdout().lock(), &json_line(&done.json))
            } else {
                write_to(&mut io::stdout().lock(), &done.text)
            };
            (0, written)
        }
        Err(ending) => {
            let (code, report) = ending.printed();
            let written = if json {
                write_to(&mut io::stdout().lock(), &json_line(&report.json))
            } else {
                write_to(&mut io::stderr().lock(), &report.text)
            };
            (code, written)
        }
    };
    match written {
        Ok(()) => ExitCode::from(code),
        Err(error) => {
            // Standard error may be gone too: then there is no one to tell.
            write_to(
                &mut io::stderr().lock(),
                &format!("vibe: the output could not be written ({error}); the command's outcome is lost: run it again with its output kept\n"),
            )
            .ok();
            ExitCode::from(1)
        }
    }
}

/// `value` as one line of JSON in which nothing [`theirs::is_escaped`] is
/// raw. serde_json escapes C0 on its own but leaves DEL, C1 and the
/// bidirectional marks as they are, which JSON allows; a terminal does not,
/// so they are escaped here too (all are in the BMP: four hex digits).
/// Outside strings the compact form holds no control character, so this
/// changes only strings, and only how they are written.
fn json_line(value: &Value) -> String {
    let mut line = String::new();
    for c in value.to_string().chars() {
        if theirs::is_escaped(c) {
            put!(line, "\\u{:04x}", u32::from(c));
        } else {
            line.push(c);
        }
    }
    line.push('\n');
    line
}

/// Writes a line on standard error, as the command goes; a standard error
/// that takes nothing is not waited for.
fn say_on_stderr(text: &str) {
    let mut line = text.trim_end().to_owned();
    line.push('\n');
    drop(write_to(&mut io::stderr().lock(), &line));
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

/// The project of a command: `given`, or the current directory; one
/// without `manifest.ts` is `not-a-project`.
fn project_of(given: Option<&Path>, command: &str) -> Result<PathBuf, Ending> {
    let project = match given {
        Some(project) => project.to_path_buf(),
        None => std::env::current_dir().map_err(|error| {
            Ending::new(
                1,
                "project-file",
                format!("the current directory cannot be read ({error}): run {command} in your project, or give it with --project"),
            )
        })?,
    };
    if !project.join("manifest.ts").is_file() {
        return Err(Ending::new(
            1,
            "not-a-project",
            format!(
                "{} is not a project: it has no manifest.ts. Run {command} in your project's folder, or give it with --project; nothing was written",
                Theirs::from(project.display().to_string().as_str())
            ),
        ));
    }
    Ok(project)
}

/// A file or folder of the project that cannot be read, written or made.
fn file_problem(what: &str, how: &str, error: &io::Error, command: &str) -> Ending {
    Ending::new(
        1,
        "project-file",
        format!("{what} {how} ({error}): make it writable by you, then run {command} again"),
    )
}

/// Writes `content` to `path` whole or not at all: to a file of its own
/// beside it (`.<name>.<tag>`), synced, then renamed over it.
fn write_whole(path: &Path, content: &[u8], tag: &str) -> io::Result<()> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = folder.join(format!(".{file_name}.{tag}"));
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

/// The whole `vibe sign-in` command that signs in again as what is kept.
fn sign_in_again(kept: &Kept) -> String {
    if kept.through_browser {
        return format!(
            "vibe sign-in --domain {} --connect {}",
            shell_word(&kept.domain),
            kept.connect
        );
    }
    let mut command = format!(
        "vibe sign-in --domain {} --connect {} --stand-in-user {}",
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
        "not signed in: run `vibe sign-in --domain <domain> --connect <ip:port>` (it opens the browser, or shows an address to open), then run this command again".into(),
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
                detail: None,
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

/// What `GET /v1/installation` answers.
#[derive(Deserialize)]
struct InstallationAnswer {
    sign_in: String,
    sign_in_address: Option<String>,
}

fn sign_in_refused(message: String) -> Ending {
    Ending::new(3, "sign-in-refused", message)
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
    let reply = client::send(&at, Method::GET, "/v1/installation", Sign::Anyone, None)
        .await
        .map_err(|f| failed(args.connect, f))?;
    if reply.status != StatusCode::OK {
        return Err(not_success(args.connect, &reply));
    }
    let installation: InstallationAnswer = shaped(
        args.connect,
        reply
            .json
            .ok_or_else(|| unexpected(args.connect, "its answer is not JSON"))?,
    )?;
    let (made, through_browser) = match (installation.sign_in.as_str(), &args.stand_in_user) {
        ("none", _) => {
            return Err(sign_in_refused(format!(
                "the installation at {} ({}) has no sign-in yet: OIDC is not connected. A Platform administrator connects it with `vibe admin connect-oidc`; then run this command again",
                args.connect, args.domain
            )));
        }
        ("stand-in", None) => {
            return Err(sign_in_refused(format!(
                "the installation at {} ({}) takes the stand-in sign-in only, which needs --stand-in-user <id>: run the command again with it",
                args.connect, args.domain
            )));
        }
        ("stand-in", Some(user)) => (stand_in(args, user).await?, false),
        ("oidc", Some(_)) => {
            return Err(sign_in_refused(format!(
                "the installation at {} ({}) signs in through the browser (OIDC), not with --stand-in-user: run the command again without --stand-in-user and --stand-in-groups",
                args.connect, args.domain
            )));
        }
        ("oidc", None) => {
            let address = installation
                .sign_in_address
                .as_deref()
                .ok_or_else(|| unexpected(args.connect, "it gave no sign-in address"))?;
            let answer = browser::sign_in(
                &at,
                address,
                args.no_browser,
                std::time::Duration::from_secs(args.timeout),
            )
            .await?;
            (shaped::<Made>(args.connect, answer)?, true)
        }
        (_, _) => {
            return Err(unexpected(
                args.connect,
                "it names a sign-in this tool does not know",
            ));
        }
    };
    let credential = Secret::new(made.credential)
        .ok_or_else(|| unexpected(args.connect, "the credential it gave is not one"))?;
    let kept = Kept {
        domain: args.domain.clone(),
        connect: args.connect,
        user: made.user,
        groups: made.groups,
        credential,
        expires_at: made.expires_at,
        through_browser,
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
        "The sign-in lasts until {}; then run vibe sign-in again.",
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

/// The stand-in's sign-in: `POST /v1/tool-credentials` as `user`.
async fn stand_in(args: &SignInArgs, user: &str) -> Result<Made, Ending> {
    let at = Installation {
        domain: &args.domain,
        connect: args.connect,
    };
    let reply = client::send(
        &at,
        Method::POST,
        "/v1/tool-credentials",
        Sign::StandIn {
            user,
            groups: args.stand_in_groups.as_deref(),
        },
        Some(bytes::Bytes::from_static(b"{}")),
    )
    .await
    .map_err(|f| failed(args.connect, f))?;
    match reply.status {
        StatusCode::CREATED => {}
        StatusCode::UNAUTHORIZED => {
            return Err(sign_in_refused(format!(
                "the installation at {} ({}) does not accept this sign-in: it takes the stand-in only when started with --stand-in-sign-in; ask whoever runs it, or sign in to another",
                args.connect, args.domain
            )));
        }
        _ => return Err(not_success(args.connect, &reply)),
    }
    shaped(
        args.connect,
        reply
            .json
            .ok_or_else(|| unexpected(args.connect, "its answer is not JSON"))?,
    )
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
            "You contribute to no Application on {}, and none is shared with you yet. vibe deploy creates one; to open or maintain another, ask one of its Contributors to share it with you or to add you.",
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
        } else {
            text.push_str("  Shared with you: you can open it; its Contributors maintain it.\n");
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
