//! `vibe show`, `vibe share`, `vibe unshare` and `vibe contributors add|remove`
//! (blackbox/CONTRACT.md, "Contributors and sharing: The tool"), and the
//! words of an Application's two lists, which `vibe deploy` says too.
//!
//! `vibe show` is `GET /v1/applications/<name>`, printed as it is answered.
//! The four that change something are one request each (`POST
//! .../contributors` or `.../sharing`), whose answer is the Application as
//! it is after, with the entries this request `added` and `removed`.
//!
//! Entries are given on the command line in any order and mix
//! (`--user`, `--email`, `--group`, and `--everyone` for the sharing);
//! they are sent in the order given, which is the order they are added
//! in. Text of the installation's reaches words only through [`Theirs`].

use std::fmt::Write as _;

use bytes::Bytes;
use clap::{Arg, ArgAction, ArgGroup, ArgMatches, FromArgMatches};
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::client::{self, Reply, Sign};
use super::credential_file::Kept;
use super::theirs::Theirs;
use super::versions::signed_in_for;
use super::{Done, Ending, Output, installation, not_success, sign_in_expired, unexpected};

/// The longest user id, Group or email address taken, in bytes (as the
/// API's).
const MAX_ENTRY_BYTES: usize = 256;

/// `vibe show`.
#[derive(clap::Args)]
pub struct ShowArgs {
    /// The Application, by name.
    pub application: String,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe contributors`.
#[derive(clap::Args)]
pub struct ContributorsArgs {
    #[command(subcommand)]
    pub command: ContributorsCommand,
}

/// What `vibe contributors` does.
#[derive(clap::Subcommand)]
pub enum ContributorsCommand {
    /// Make people or Groups Contributors of the Application: they can
    /// open it, deploy it, and change both of its lists.
    Add(ChangeArgs<false>),
    /// Take people or Groups off the Application's Contributors (one must
    /// be left).
    Remove(ChangeArgs<false>),
}

impl ContributorsArgs {
    pub(super) fn json(&self) -> bool {
        match &self.command {
            ContributorsCommand::Add(args) | ContributorsCommand::Remove(args) => args.json,
        }
    }
}

/// One entry as given on the command line.
enum Given {
    User(String),
    Email(String),
    Group(String),
    Everyone,
}

impl Given {
    /// The entry as the API takes it.
    fn json(&self) -> Value {
        match self {
            Self::User(id) => json!({ "user": id }),
            Self::Email(address) => json!({ "email": address }),
            Self::Group(group) => json!({ "group": group }),
            Self::Everyone => json!({ "everyone": true }),
        }
    }
}

/// The arguments of the four that change a list: the Application and the
/// entries, in the order given. `EVERYONE` is whether `--everyone` is
/// taken (the sharing's, not the Contributors').
pub struct ChangeArgs<const EVERYONE: bool> {
    application: String,
    entries: Vec<Given>,
    json: bool,
}

impl<const EVERYONE: bool> ChangeArgs<EVERYONE> {
    pub(super) fn json(&self) -> bool {
        self.json
    }
}

/// A user id, Group or email address: 1 to 256 bytes, no control
/// character.
fn entry_text(text: &str) -> Result<String, String> {
    if text.is_empty() || text.len() > MAX_ENTRY_BYTES {
        return Err(format!("must be 1 to {MAX_ENTRY_BYTES} bytes"));
    }
    if text.chars().any(char::is_control) {
        return Err("must hold no control character".into());
    }
    Ok(text.to_owned())
}

const ENTRY_ARGS: [&str; 4] = ["user", "email", "group", "everyone"];

impl<const EVERYONE: bool> clap::Args for ChangeArgs<EVERYONE> {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        let named = |id: &'static str, value: &'static str, help: &'static str| {
            Arg::new(id)
                .long(id)
                .value_name(value)
                .help(help)
                .action(ArgAction::Append)
                .value_parser(entry_text)
        };
        let mut cmd = cmd
            .arg(
                Arg::new("application")
                    .value_name("APPLICATION")
                    .help("The Application, by name")
                    .required(true),
            )
            .arg(named(
                "user",
                "ID",
                "A person, by their user id (vibe whoami shows it)",
            ))
            .arg(named(
                "email",
                "ADDRESS",
                "A person, by their email address: the one who signed in here with it",
            ))
            .arg(named("group", "GROUP", "A Group, by its name"));
        let entries: &[&str] = if EVERYONE {
            cmd = cmd.arg(
                Arg::new("everyone")
                    .long("everyone")
                    .help("Everyone who can sign in to the installation")
                    .action(ArgAction::SetTrue),
            );
            &ENTRY_ARGS
        } else {
            ENTRY_ARGS.get(..3).unwrap_or(&[])
        };
        cmd.arg(
            Arg::new("json")
                .long("json")
                .help("Print exactly one JSON object on standard output")
                .action(ArgAction::SetTrue),
        )
        .group(
            ArgGroup::new("entries")
                .args(entries)
                .multiple(true)
                .required(true),
        )
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

impl<const EVERYONE: bool> FromArgMatches for ChangeArgs<EVERYONE> {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let application = matches
            .get_one::<String>("application")
            .cloned()
            .ok_or_else(|| clap::Error::new(clap::error::ErrorKind::MissingRequiredArgument))?;
        // Each entry with where it stood on the command line, then put in
        // that order.
        let mut placed: Vec<(usize, Given)> = Vec::new();
        for (id, make) in [
            ("user", Given::User as fn(String) -> Given),
            ("email", Given::Email),
            ("group", Given::Group),
        ] {
            if let (Some(indices), Some(values)) =
                (matches.indices_of(id), matches.get_many::<String>(id))
            {
                placed.extend(indices.zip(values.cloned().map(make)));
            }
        }
        if EVERYONE
            && matches.get_flag("everyone")
            && let Some(index) = matches.index_of("everyone")
        {
            placed.push((index, Given::Everyone));
        }
        placed.sort_by_key(|(index, _)| *index);
        Ok(Self {
            application,
            entries: placed.into_iter().map(|(_, given)| given).collect(),
            json: matches.get_flag("json"),
        })
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

/// Which list a change is of, and which way.
#[derive(Clone, Copy)]
pub enum Change {
    Share,
    Unshare,
    AddContributors,
    RemoveContributors,
}

impl Change {
    fn path(self) -> &'static str {
        match self {
            Self::Share | Self::Unshare => "sharing",
            Self::AddContributors | Self::RemoveContributors => "contributors",
        }
    }

    fn adds(self) -> bool {
        matches!(self, Self::Share | Self::AddContributors)
    }
}

/// The reasons a refusal ends with exit code `3`; any other refusal is a
/// failure (`1`).
const REFUSED: [&str; 6] = [
    "not-a-contributor",
    "not-shared",
    "not-found",
    "unknown-email",
    "ambiguous-email",
    "last-contributor",
];

/// The API's refusal, with the exit code the contract gives its reason.
fn refused(kept: &Kept, reply: &Reply) -> Ending {
    let mut ending = not_success(kept.at(), reply);
    let listed = ending
        .reason
        .as_str()
        .is_some_and(|reason| REFUSED.contains(&reason));
    if ending.code == 3 && !listed {
        ending.code = 1;
    }
    ending
}

/// `GET /v1/applications/<name>`: the Application, or the reply that was
/// not it.
async fn read(kept: &Kept, name: &str) -> Result<Result<Value, Reply>, Ending> {
    let reply = client::send(
        &installation(kept),
        Method::GET,
        &format!("/v1/applications/{name}"),
        Sign::Credential(&kept.credential),
        None,
    )
    .await
    .map_err(|f| super::failed(kept.at(), f))?;
    match reply.status {
        StatusCode::OK => reply
            .json
            .map(Ok)
            .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON")),
        StatusCode::UNAUTHORIZED => Err(sign_in_expired(kept)),
        _ => Ok(Err(reply)),
    }
}

/// `vibe show`.
pub(super) async fn show(args: &ShowArgs) -> Result<Done, Ending> {
    let name = args.application.as_str();
    let kept = signed_in_for(name)?;
    let application = match read(&kept, name).await? {
        Ok(application) => application,
        Err(reply) => return Err(refused(&kept, &reply)),
    };
    let text = shown(&kept, &application);
    Ok(Done {
        json: application,
        text,
    })
}

/// The words of `vibe show`.
fn shown(kept: &Kept, application: &Value) -> String {
    let field = |key: &str| application.get(key).and_then(Value::as_str);
    let mut text = String::new();
    writeln!(
        text,
        "{} ({})",
        Theirs::from(field("name").unwrap_or("")),
        Theirs::from(field("address").unwrap_or(""))
    )
    .ok();
    if let Some(description) = field("description").filter(|d| !d.is_empty()) {
        writeln!(text, "  {}", Theirs::from(description)).ok();
    }
    match application.get("serving_version").and_then(Value::as_u64) {
        Some(number) => writeln!(text, "  Serving: Version {number}").ok(),
        None => writeln!(text, "  Serving: no Version yet").ok(),
    };
    if let Some(disabled) = application.get("disabled").filter(|d| d.is_object()) {
        let part =
            |key: &str| Theirs::from(disabled.get(key).and_then(Value::as_str).unwrap_or(""));
        writeln!(
            text,
            "  Disabled since {} by {}: its address answers that it is disabled, and none of its code runs. Only a Platform administrator enables it again.",
            part("since"),
            part("by")
        )
        .ok();
    }
    if application.get("contributor") == Some(&Value::Bool(true)) {
        text.push_str("  You are one of its Contributors.\n");
    }
    text.push_str("  Contributors:\n");
    for entry in list(application.get("contributors")) {
        writeln!(text, "    {}", entry_words(kept, entry)).ok();
    }
    match application.get("shared_with").and_then(Value::as_array) {
        None => {
            text.push_str(
                "  Whom it is shared with: only its Contributors see that list (and Platform administrators).\n",
            );
        }
        Some(shared) if shared.is_empty() => {
            text.push_str("  Shared with nobody: it is visible to its Contributors only.\n");
        }
        Some(shared) => {
            text.push_str("  Shared with:\n");
            for entry in shared {
                writeln!(text, "    {}", entry_words(kept, entry)).ok();
            }
        }
    }
    text
}

/// The entries of a list as the API answers it; none when it is not one.
fn list(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// `vibe share`, `vibe unshare`, `vibe contributors add|remove`.
pub(super) async fn change<const EVERYONE: bool>(
    change: Change,
    args: &ChangeArgs<EVERYONE>,
) -> Result<Done, Ending> {
    let name = args.application.as_str();
    let kept = signed_in_for(name)?;
    let entries: Vec<Value> = args.entries.iter().map(Given::json).collect();
    let body = if change.adds() {
        json!({ "add": entries })
    } else {
        json!({ "remove": entries })
    };
    let reply = client::send(
        &installation(&kept),
        Method::POST,
        &format!("/v1/applications/{name}/{}", change.path()),
        Sign::Credential(&kept.credential),
        Some(Bytes::from(body.to_string())),
    )
    .await
    .map_err(|f| super::failed(kept.at(), f))?;
    let after = match reply.status {
        StatusCode::OK => reply
            .json
            .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON"))?,
        StatusCode::UNAUTHORIZED => return Err(sign_in_expired(&kept)),
        _ => return Err(refused(&kept, &reply)),
    };
    let added = list(after.get("added"));
    let removed = list(after.get("removed"));
    let contributors = list(after.get("contributors"));
    // Null to someone who has just taken themselves off the Contributors.
    let shared_with = after
        .get("shared_with")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let address = after.get("address").and_then(Value::as_str).unwrap_or("");
    let mut text = String::new();
    let what = match change {
        Change::Share => format!("Shared {name} with"),
        Change::Unshare => format!("Unshared {name} from"),
        Change::AddContributors => format!("Added to the Contributors of {name}:"),
        Change::RemoveContributors => format!("Removed from the Contributors of {name}:"),
    };
    let changed = if change.adds() { added } else { removed };
    if changed.is_empty() {
        writeln!(
            text,
            "Nothing changed: {name} was so already for everything asked."
        )
        .ok();
    } else {
        writeln!(text, "{what}").ok();
        for entry in changed {
            writeln!(text, "  {}", entry_words(&kept, entry)).ok();
        }
    }
    text.push_str(&visible_to_words(
        &kept,
        name,
        &Theirs::from(address),
        contributors,
        shared_with,
    ));
    Ok(Done {
        json: json!({
            "application": name,
            "address": address,
            "added": added,
            "removed": removed,
            "visible_to": { "contributors": contributors, "shared_with": shared_with },
        }),
        text,
    })
}

/// One entry in words: a person by their name and email when known, and
/// their id; a Group as a Group; everyone as everyone who can sign in to
/// the installation. With OIDC a person the platform never saw sign in is
/// said so beside their id (a mistyped id shows there); the stand-in keeps
/// no record of people, so nothing is said there.
pub(super) fn entry_words(kept: &Kept, entry: &Value) -> String {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(Theirs::from);
    if let Some(id) = text("user") {
        let (name, email) = (text("name"), text("email"));
        return match (name, email) {
            (Some(name), Some(email)) => format!("{name} <{email}> ({id})"),
            (Some(name), None) => format!("{name} ({id})"),
            (None, Some(email)) => format!("<{email}> ({id})"),
            (None, None) if kept.through_browser => {
                format!("{id} (not seen signing in here yet: check the id)")
            }
            (None, None) => id.to_string(),
        };
    }
    if let Some(group) = text("group") {
        return format!("the Group {group}");
    }
    if entry.get("everyone") == Some(&Value::Bool(true)) {
        return format!(
            "everyone who can sign in to {}",
            Theirs::from(kept.domain.as_str())
        );
    }
    "an entry this tool does not know".to_owned()
}

/// Who can see the Application now, each by name; or that it is visible
/// to its Contributors only, and that `vibe share` shares it.
pub(super) fn visible_to_words(
    kept: &Kept,
    name: &str,
    address: &Theirs,
    contributors: &[Value],
    shared_with: Option<&[Value]>,
) -> String {
    let named = |entries: &[Value]| {
        entries
            .iter()
            .map(|e| entry_words(kept, e))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = String::new();
    let Some(shared_with) = shared_with else {
        writeln!(
            text,
            "Who can see {} ({address}): its Contributors, {}, and whom it is shared with, which only its Contributors see.",
            Theirs::from(name),
            named(contributors),
        )
        .ok();
        return text;
    };
    if shared_with.is_empty() {
        writeln!(
            text,
            "{} ({address}) is visible to its Contributors only: {}. Nobody else can open it until it is shared: `vibe share {} --user <id>` (or --email, --group, --everyone) shares it.",
            Theirs::from(name),
            named(contributors),
            Theirs::from(name),
        )
        .ok();
    } else {
        writeln!(text, "Who can see {} ({address}) now:", Theirs::from(name)).ok();
        writeln!(text, "  Its Contributors: {}", named(contributors)).ok();
        writeln!(text, "  Shared with: {}", named(shared_with)).ok();
    }
    text
}
