//! `vibe admin`, the commands for Platform administrators.
//!
//! `vibe admin connect-oidc` (blackbox/CONTRACT.md, "The tool signs in
//! through the browser, and OIDC is connected from the tool: Connecting
//! OIDC"): `PUT /v1/oidc` with the setup credential (`VIBE_SETUP_CREDENTIAL`,
//! to `--domain`, over HTTPS or at `--connect`) or with the tool's own credential, the
//! client secret from `VIBE_OIDC_CLIENT_SECRET`. Neither secret is printed
//! or kept: each is read from the environment, sent, and dropped.
//!
//! `vibe admin applications`, `disable`, `enable` and `operation-log`
//! (blackbox/CONTRACT.md, "What Platform administrators see and stop: The
//! tool"): one request each to the installation the tool is signed in to.
//! What can be checked before asking is checked by the command line (exit
//! code `2`, nothing asked); with `--json` the API's answer is printed as
//! it is (a projection of it for `disable` and `enable`); in words every
//! text of the installation's goes through [`Theirs`].

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::client::{self, Installation, Sign};
use super::credential_file::{Kept, Place, Secret};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, Reach, domain, failed, get_signed_in, not_success, read_kept, shell_word,
    sign_in_expired, unexpected,
};
use crate::api::{MAX_ENTRY_BYTES, rfc3339_micros};
use crate::oidc::secret::ClientSecret;
use crate::registry::{NameProblem, check_name};
use crate::resources::{is_group, is_resource_name};

/// The variable that holds the client secret.
const CLIENT_SECRET_VARIABLE: &str = "VIBE_OIDC_CLIENT_SECRET";

/// The variables that hold a Resource login's password, one for each role.
const READ_PASSWORD: &str = "VIBE_RESOURCE_READ_PASSWORD";
const WRITE_PASSWORD: &str = "VIBE_RESOURCE_WRITE_PASSWORD";
const STRUCTURE_PASSWORD: &str = "VIBE_RESOURCE_STRUCTURE_PASSWORD";

/// The variable that holds the setup credential.
const SETUP_VARIABLE: &str = "VIBE_SETUP_CREDENTIAL";

/// `vibe admin`.
#[derive(clap::Args)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommand,
}

/// A command for Platform administrators.
#[derive(clap::Subcommand)]
pub enum AdminCommand {
    /// Connect OIDC, with the setup credential or as a Platform
    /// administrator signed in.
    ///
    /// With the setup credential in VIBE_SETUP_CREDENTIAL, --domain says
    /// where the installation is: over HTTPS, with --port and --resolve as
    /// for vibe sign-in, or over plain HTTP on this machine with --connect.
    /// Without it, the command goes to the installation the tool is signed
    /// in to. The client secret is read from VIBE_OIDC_CLIENT_SECRET.
    ConnectOidc(ConnectOidcArgs),
    /// List every Application of the installation, a page at a time: its
    /// Declared access, Contributors, sharing, whether it is disabled, and
    /// its usage over the last 30 days.
    Applications(ApplicationsArgs),
    /// Disable an Application: its address answers that it is disabled,
    /// requests in flight included, and none of its code runs, until it is
    /// enabled.
    Disable(SwitchArgs),
    /// Enable a disabled Application: it is served again by the Version
    /// that serves now.
    Enable(SwitchArgs),
    /// Read the Operation log, newest page first, filtered.
    OperationLog(OperationLogArgs),
    /// Allow Applications to call a host, or stop allowing it: add,
    /// remove.
    Outbound(super::outbound::OutboundAdminArgs),
    /// Every Resource of the installation, a page at a time, or one by
    /// name: its logins (never their values), whether it is served, its
    /// Grants and its place.
    Resources(ResourcesArgs),
    /// Register a Resource, replace its logins, or change its description,
    /// owner and contact.
    Resource(ResourceArgs),
    /// Give a Group access to a Resource, or change its level.
    Grant(GrantArgs),
    /// Take a Group's access to a Resource away.
    Revoke(RevokeArgs),
    /// The Grants of a Resource, or the Resources a Group can reach.
    Grants(GrantsArgs),
}

/// `vibe admin grant <resource> --group <group> --access <level>`.
#[derive(clap::Args)]
pub struct GrantArgs {
    #[arg(value_parser = a_resource_name)]
    pub resource: String,
    /// The Group, exactly as the company's directory names it.
    #[arg(long, value_parser = group_name)]
    pub group: String,
    /// `structure`, `read` or `write`.
    #[arg(long)]
    pub access: String,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin revoke <resource> --group <group>`.
#[derive(clap::Args)]
pub struct RevokeArgs {
    #[arg(value_parser = a_resource_name)]
    pub resource: String,
    /// The Group whose Grant is revoked, exactly as it was given.
    #[arg(long, value_parser = group_name)]
    pub group: String,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin grants (--resource <name> | --group <group>)`.
#[derive(clap::Args)]
pub struct GrantsArgs {
    /// The Resource whose Grants are listed.
    #[arg(long, value_parser = a_resource_name, conflicts_with = "group")]
    pub resource: Option<String>,
    /// The Group whose Grants are listed.
    #[arg(long, value_parser = group_name, conflicts_with = "resource")]
    pub group: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resources [<name>]`.
#[derive(clap::Args)]
pub struct ResourcesArgs {
    /// One Resource by name, instead of the page of them.
    #[arg(value_parser = a_resource_name)]
    pub name: Option<String>,
    /// Only the Resources whose name comes after this one.
    #[arg(long, value_parser = a_resource_name)]
    pub after: Option<String>,
    /// How many at most (1 to 100; 100 without it).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub limit: Option<u8>,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resource`.
#[derive(clap::Args)]
pub struct ResourceArgs {
    #[command(subcommand)]
    pub command: ResourceCommand,
}

/// What `vibe admin resource` does.
#[derive(clap::Subcommand)]
pub enum ResourceCommand {
    /// Register a Resource, its logins checked first. A database's
    /// passwords come from VIBE_RESOURCE_READ_PASSWORD,
    /// VIBE_RESOURCE_WRITE_PASSWORD and VIBE_RESOURCE_STRUCTURE_PASSWORD,
    /// never from the command line.
    Register(RegisterArgs),
    /// Replace a Resource's logins: each member optional, `--no-write` and
    /// `--no-structure` remove one. Passwords come from the environment, as
    /// for `register`.
    Logins(LoginsArgs),
    /// Change a Resource's description, owner or contact.
    Edit(EditArgs),
    /// Change a Storage Resource's directory: files are not moved, and the
    /// old directory's files stay where they are.
    Location(LocationArgs),
    /// Remove a Resource, with its Grants: `--yes` is needed.
    Remove(RemoveArgs),
}

/// `vibe admin resource register <name>`.
#[derive(clap::Args)]
#[command(
    after_help = "A database's passwords are read from VIBE_RESOURCE_READ_PASSWORD, VIBE_RESOURCE_WRITE_PASSWORD and VIBE_RESOURCE_STRUCTURE_PASSWORD, each beside its role, and are never printed."
)]
pub struct RegisterArgs {
    #[arg(value_parser = a_resource_name)]
    pub name: String,
    /// postgres or storage.
    #[arg(long, value_parser = kind)]
    pub kind: String,
    /// What it is for.
    #[arg(long)]
    pub description: String,
    /// Whose it is.
    #[arg(long)]
    pub owner: String,
    /// Whom to ask for access.
    #[arg(long)]
    pub contact: String,
    /// The database's host (postgres).
    #[arg(long)]
    pub host: Option<String>,
    /// The database's port (postgres).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=65535))]
    pub port: Option<u16>,
    /// The database's name (postgres).
    #[arg(long)]
    pub database: Option<String>,
    /// The login the platform reads with (postgres).
    #[arg(long)]
    pub read_role: Option<String>,
    /// The login the platform writes with (postgres).
    #[arg(long)]
    pub write_role: Option<String>,
    /// The login the platform reads the structure with (postgres).
    #[arg(long)]
    pub structure_role: Option<String>,
    /// The directory, beneath the storage root (storage).
    #[arg(long, value_parser = absolute_path)]
    pub directory: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resource logins <name>`.
#[derive(clap::Args)]
#[command(
    after_help = "A role's password is read from its variable (VIBE_RESOURCE_READ_PASSWORD, VIBE_RESOURCE_WRITE_PASSWORD, VIBE_RESOURCE_STRUCTURE_PASSWORD), never printed."
)]
pub struct LoginsArgs {
    #[arg(value_parser = a_resource_name)]
    pub name: String,
    /// The new host (an address change connects every login).
    #[arg(long)]
    pub host: Option<String>,
    /// The new port.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=65535))]
    pub port: Option<u16>,
    /// The new database name.
    #[arg(long)]
    pub database: Option<String>,
    /// A new read login, with its password in its variable.
    #[arg(long)]
    pub read_role: Option<String>,
    /// A new write login, with its password in its variable.
    #[arg(long, conflicts_with = "no_write")]
    pub write_role: Option<String>,
    /// Remove the write login.
    #[arg(long)]
    pub no_write: bool,
    /// A new structure login, with its password in its variable.
    #[arg(long, conflicts_with = "no_structure")]
    pub structure_role: Option<String>,
    /// Remove the structure login.
    #[arg(long)]
    pub no_structure: bool,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resource edit <name>`.
#[derive(clap::Args)]
pub struct EditArgs {
    #[arg(value_parser = a_resource_name)]
    pub name: String,
    /// The new description.
    #[arg(long)]
    pub description: Option<String>,
    /// The new owner.
    #[arg(long)]
    pub owner: Option<String>,
    /// The new contact.
    #[arg(long)]
    pub contact: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resource location <name>`.
#[derive(clap::Args)]
pub struct LocationArgs {
    #[arg(value_parser = a_resource_name)]
    pub name: String,
    /// The new directory, beneath the storage root.
    #[arg(long, value_parser = absolute_path)]
    pub directory: String,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin resource remove <name>`.
#[derive(clap::Args)]
pub struct RemoveArgs {
    #[arg(value_parser = a_resource_name)]
    pub name: String,
    /// Remove it: without this, nothing is asked.
    #[arg(long, action = clap::ArgAction::Count)]
    pub yes: u8,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin applications`.
#[derive(clap::Args)]
pub struct ApplicationsArgs {
    /// Only the Applications whose name comes after this one.
    #[arg(long, value_parser = any_application_name)]
    pub after: Option<String>,
    /// How many Applications at most (1 to 100; 100 without it).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub limit: Option<u8>,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin disable` and `vibe admin enable`.
#[derive(clap::Args)]
pub struct SwitchArgs {
    /// The Application.
    #[arg(value_parser = any_application_name)]
    pub application: String,
    #[command(flatten)]
    pub output: Output,
}

/// `vibe admin operation-log`.
#[derive(clap::Args)]
pub struct OperationLogArgs {
    /// Only the entries of this User (their id).
    #[arg(long, value_parser = user_id)]
    pub user: Option<String>,
    /// Only the entries of this Application.
    #[arg(long, value_parser = any_application_name)]
    pub application: Option<String>,
    /// Only the entries on this Resource.
    #[arg(long, value_parser = a_resource_name)]
    pub resource: Option<String>,
    /// Only the entries whose host is exactly this: a fetch's host, as the
    /// list writes it.
    #[arg(long, value_parser = super::outbound::host_filter)]
    pub host: Option<String>,
    /// Only the entries of this outcome.
    #[arg(long, value_parser = ["allowed", "refused", "failed", "unfinished"])]
    pub outcome: Option<String>,
    /// Only the entries written at or after this time (RFC 3339 with an
    /// offset, such as 2026-11-21T10:00:00Z).
    #[arg(long, value_parser = time)]
    pub since: Option<String>,
    /// Only the entries written before this time (RFC 3339 with an offset).
    #[arg(long, value_parser = time)]
    pub until: Option<String>,
    /// Only the entries whose id is below this one (the `earlier` of a
    /// page).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub before: Option<u64>,
    /// How many entries at most (1 to 1000; 100 without it).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: Option<u16>,
    #[command(flatten)]
    pub output: Output,
}

/// A name an Application could have (a reserved one included: it names no
/// Application, which the installation says).
fn any_application_name(text: &str) -> Result<String, String> {
    match check_name(text) {
        Err(NameProblem::Invalid) => Err("not an Application's name: 1 to 63 lower-case letters, digits and hyphens, not starting or ending with a hyphen, and not beginning with xn--".into()),
        Ok(()) | Err(NameProblem::Reserved) => Ok(text.to_owned()),
    }
}

fn a_resource_name(text: &str) -> Result<String, String> {
    if is_resource_name(text) {
        Ok(text.to_owned())
    } else {
        Err("not a Resource's name: 1 to 63 lower-case letters, digits and underscores, starting with a letter".into())
    }
}

fn group_name(text: &str) -> Result<String, String> {
    if is_group(text) {
        Ok(text.to_owned())
    } else {
        Err("not a Group: 1 to 256 bytes without control characters".into())
    }
}

fn user_id(text: &str) -> Result<String, String> {
    if text.is_empty() || text.len() > MAX_ENTRY_BYTES || text.chars().any(char::is_control) {
        Err("not a user id: 1 to 256 bytes without control characters".into())
    } else {
        Ok(text.to_owned())
    }
}

fn time(text: &str) -> Result<String, String> {
    if rfc3339_micros(text).is_some() {
        Ok(text.to_owned())
    } else {
        Err("not a time: RFC 3339 with a date, a time and an offset, such as 2026-11-21T10:00:00Z or 2026-11-21T11:00:00+01:00, up to microseconds".into())
    }
}

/// What the command line cannot check alone: `--since` before `--until`.
/// Told as a usage error (exit code `2`), before anything is asked.
pub fn usage_problem(args: &AdminArgs) -> Option<&'static str> {
    let AdminCommand::OperationLog(args) = &args.command else {
        return None;
    };
    let since = args.since.as_deref().and_then(rfc3339_micros)?;
    let until = args.until.as_deref().and_then(rfc3339_micros)?;
    (since >= until).then_some("--since must be before --until")
}

/// `vibe admin connect-oidc`.
#[derive(clap::Args)]
pub struct ConnectOidcArgs {
    /// With the setup credential: the installation's domain.
    #[arg(long, value_parser = domain)]
    pub domain: Option<String>,
    /// With the setup credential: how the installation is reached.
    #[command(flatten)]
    pub reach: Reach,
    /// The identity provider's issuer, https://... (http:// only on a
    /// loopback host).
    #[arg(long)]
    pub issuer: String,
    /// The platform's client id at the identity provider.
    #[arg(long)]
    pub client_id: String,
    /// The Group whose members are Platform administrators.
    #[arg(long)]
    pub administrators_group: String,
    /// The ID token's claim that holds the person's Groups (groups without
    /// it).
    #[arg(long)]
    pub groups_claim: Option<String>,
    /// The scopes asked for, separated by spaces; openid is always asked.
    #[arg(long)]
    pub scopes: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

impl AdminArgs {
    pub fn json(&self) -> bool {
        match &self.command {
            AdminCommand::ConnectOidc(args) => args.output.json,
            AdminCommand::Applications(args) => args.output.json,
            AdminCommand::Disable(args) | AdminCommand::Enable(args) => args.output.json,
            AdminCommand::OperationLog(args) => args.output.json,
            AdminCommand::Outbound(args) => match &args.command {
                super::outbound::OutboundAdminCommand::Add(args) => args.output.json,
                super::outbound::OutboundAdminCommand::Remove(args) => args.output.json,
            },
            AdminCommand::Resources(args) => args.output.json,
            AdminCommand::Resource(args) => match &args.command {
                ResourceCommand::Register(args) => args.output.json,
                ResourceCommand::Logins(args) => args.output.json,
                ResourceCommand::Edit(args) => args.output.json,
                ResourceCommand::Location(args) => args.output.json,
                ResourceCommand::Remove(args) => args.output.json,
            },
            AdminCommand::Grant(args) => args.output.json,
            AdminCommand::Revoke(args) => args.output.json,
            AdminCommand::Grants(args) => args.output.json,
        }
    }
}

pub async fn admin(args: &AdminArgs) -> Result<Done, Ending> {
    match &args.command {
        AdminCommand::ConnectOidc(args) => connect_oidc(args).await,
        AdminCommand::Applications(args) => applications(args).await,
        AdminCommand::Disable(args) => switch(args, true).await,
        AdminCommand::Enable(args) => switch(args, false).await,
        AdminCommand::OperationLog(args) => operation_log(args).await,
        AdminCommand::Outbound(args) => match &args.command {
            super::outbound::OutboundAdminCommand::Add(args) => {
                super::outbound::change(super::outbound::Change::Add, args).await
            }
            super::outbound::OutboundAdminCommand::Remove(args) => {
                super::outbound::change(super::outbound::Change::Remove, args).await
            }
        },
        AdminCommand::Resources(args) => resources(args).await,
        AdminCommand::Resource(args) => match &args.command {
            ResourceCommand::Register(args) => register(args).await,
            ResourceCommand::Logins(args) => logins(args).await,
            ResourceCommand::Edit(args) => edit(args).await,
            ResourceCommand::Location(args) => location(args).await,
            ResourceCommand::Remove(args) => remove(args).await,
        },
        AdminCommand::Grant(args) => grant(args).await,
        AdminCommand::Revoke(args) => revoke(args).await,
        AdminCommand::Grants(args) => grants(args).await,
    }
}

/// The kept sign-in: exit code `4` when there is none.
fn signed_in() -> Result<Kept, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    read_kept(&place)
}

/// A query of `pairs` given, `?a=b&c=d`, or nothing.
fn query_of(pairs: &[(&str, Option<String>)]) -> String {
    let mut query = String::new();
    for (name, value) in pairs {
        if let Some(value) = value {
            query.push(if query.is_empty() { '?' } else { '&' });
            put!(query, "{name}={}", crate::resources::encode_segment(value));
        }
    }
    query
}

/// The options of `pairs` given, as a command line takes them: ` --a b`.
fn options_of(pairs: &[(&str, Option<String>)]) -> String {
    let mut options = String::new();
    for (name, value) in pairs {
        if let Some(value) = value {
            put!(options, " --{name} {}", shell_word(value));
        }
    }
    options
}

/// A text member of `value`, as the installation's.
fn text(value: &Value, name: &str) -> Theirs {
    Theirs::from(value.get(name).and_then(Value::as_str).unwrap_or_default())
}

/// `vibe admin applications`: one page of `GET /v1/admin/applications`.
async fn applications(args: &ApplicationsArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let asked = [
        ("limit", args.limit.map(|n| n.to_string())),
        ("after", args.after.clone()),
    ];
    let page = get_signed_in(
        &kept,
        &format!("/v1/admin/applications{}", query_of(&asked)),
    )
    .await?;
    let listed = page
        .get("applications")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected(kept.at(), "its answer has no list of Applications"))?;
    let mut words = String::new();
    if listed.is_empty() {
        // The name given, checked to be one an Application could have, so
        // nothing in it needs escaping; with it, also when the installation
        // has none at all.
        match &args.after {
            Some(after) => say!(words, "No Application comes after {after}."),
            None => say!(words, "The installation has no Application yet."),
        }
    }
    for application in listed {
        application_block(&mut words, application);
        words.push('\n');
    }
    if let Some(next) = page.get("next").and_then(Value::as_str) {
        let more = [
            ("limit", args.limit.map(|n| n.to_string())),
            ("after", Some(next.to_owned())),
        ];
        say!(
            words,
            "More Applications follow: vibe admin applications{}",
            Theirs::from(options_of(&more).as_str())
        );
    }
    Ok(Done {
        json: page,
        text: words,
    })
}

/// One Application of the list, in words: a block of lines.
fn application_block(words: &mut String, application: &Value) {
    let member = |name: &str| application.get(name).unwrap_or(&Value::Null);
    say!(
        words,
        "{}  {}",
        text(application, "name"),
        text(application, "address")
    );
    if let Some(disabled) = member("disabled").as_object() {
        let field = |name: &str| {
            Theirs::from(
                disabled
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
        };
        say!(
            words,
            "  disabled since {} by {}",
            field("since"),
            field("by")
        );
    }
    match member("serving_version").as_u64() {
        Some(number) => say!(words, "  serving Version {number}"),
        None => say!(words, "  no Version serves"),
    }
    let resources = member("declared_access")
        .get("resources")
        .and_then(Value::as_object);
    match resources {
        Some(resources) if !resources.is_empty() => {
            let mut declared = String::new();
            for (n, (name, resource)) in resources.iter().enumerate() {
                if n > 0 {
                    declared.push_str(", ");
                }
                // A Declaration stored before Storage existed has no kind:
                // it is a database's (crate::registry::declared_of).
                let kind = resource
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("postgres");
                put!(
                    declared,
                    "{} ({}) {}",
                    Theirs::from(name.as_str()),
                    Theirs::from(kind),
                    text(resource, "access")
                );
            }
            let more = member("declared_more").as_u64().unwrap_or(0);
            if more > 0 {
                put!(declared, ", and {more} more");
            }
            say!(words, "  declares {declared}");
        }
        _ => say!(words, "  declares no Resource"),
    }
    let contributors: Vec<String> = member("contributors")
        .as_array()
        .map(|entries| entries.iter().map(entry_words).collect())
        .unwrap_or_default();
    say!(words, "  Contributors: {}", contributors.join(", "));
    let shared = member("shared_with");
    let count = |name: &str| shared.get(name).and_then(Value::as_u64).unwrap_or(0);
    let everyone = shared
        .get("everyone")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (groups, people) = (count("groups"), count("people"));
    if everyone {
        say!(words, "  shared with everyone");
    } else if groups == 0 && people == 0 {
        say!(words, "  shared with nobody");
    } else {
        say!(
            words,
            "  shared with {groups} {} and {people} {}",
            if groups == 1 { "Group" } else { "Groups" },
            if people == 1 { "person" } else { "people" }
        );
    }
    let usage = member("usage");
    let number = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    say!(
        words,
        "  {} requests by {} Users in the last 30 days",
        number("requests"),
        number("users")
    );
}

/// An entry of a list in words: a person by name and email when known,
/// and by id; a Group as a Group; everyone.
fn entry_words(entry: &Value) -> String {
    if let Some(id) = entry.get("user").and_then(Value::as_str) {
        let name = entry.get("name").and_then(Value::as_str);
        let email = entry.get("email").and_then(Value::as_str);
        return match (name, email) {
            (Some(name), Some(email)) => format!(
                "{} <{}> ({})",
                Theirs::from(name),
                Theirs::from(email),
                Theirs::from(id)
            ),
            (Some(name), None) => format!("{} ({})", Theirs::from(name), Theirs::from(id)),
            (None, Some(email)) => format!("<{}> ({})", Theirs::from(email), Theirs::from(id)),
            (None, None) => Theirs::from(id).to_string(),
        };
    }
    if let Some(group) = entry.get("group").and_then(Value::as_str) {
        return format!("the Group {}", Theirs::from(group));
    }
    "everyone".to_owned()
}

/// `vibe admin disable` (`disable`) and `vibe admin enable`.
async fn switch(args: &SwitchArgs, disable: bool) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let name = args.application.as_str();
    let action = if disable { "disable" } else { "enable" };
    let reply = client::send(
        &super::installation(&kept),
        Method::POST,
        &format!("/v1/applications/{name}/{action}"),
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
    let disabled = answer.get("disabled").cloned().unwrap_or(Value::Null);
    let serving = answer.get("serving_version").and_then(Value::as_u64);
    let address = text(&answer, "address");
    let mut json = serde_json::Map::new();
    json.insert("application".into(), json!(name));
    json.insert(
        "address".into(),
        answer.get("address").cloned().unwrap_or(Value::Null),
    );
    let mut words = String::new();
    if disable {
        let since = text(&disabled, "since");
        let by = text(&disabled, "by");
        if changed {
            say!(
                words,
                "Disabled {name}: {address} answers every request that the Application is disabled, those that were in flight included, and none of its code runs."
            );
        } else {
            say!(
                words,
                "{name} was disabled already, since {since} by {by}: nothing changed."
            );
        }
        say!(words, "vibe admin enable {name} serves it again.");
    } else if !changed {
        say!(words, "{name} was not disabled: nothing changed.");
    } else {
        match serving {
            Some(number) => say!(
                words,
                "Enabled {name}: it serves again at {address} as Version {number}."
            ),
            None => say!(
                words,
                "Enabled {name}: it has no Version serving, so nothing is at {address} until one is deployed."
            ),
        }
    }
    json.insert("disabled".into(), disabled);
    json.insert("changed".into(), json!(changed));
    if !disable {
        json.insert("serving_version".into(), json!(serving));
    }
    Ok(Done {
        json: Value::Object(json),
        text: words,
    })
}

/// `vibe admin operation-log`: one page of `GET /v1/operation-log`.
async fn operation_log(args: &OperationLogArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let filters = [
        ("user", args.user.clone()),
        ("application", args.application.clone()),
        ("resource", args.resource.clone()),
        ("host", args.host.clone()),
        ("outcome", args.outcome.clone()),
        ("since", args.since.clone()),
        ("until", args.until.clone()),
    ];
    let mut asked: Vec<(&str, Option<String>)> = filters.to_vec();
    asked.push(("limit", args.limit.map(|n| n.to_string())));
    asked.push(("before", args.before.map(|n| n.to_string())));
    let page = get_signed_in(&kept, &format!("/v1/operation-log{}", query_of(&asked))).await?;
    let entries = page
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected(kept.at(), "its answer has no entries"))?;
    let mut words = String::new();
    if entries.is_empty() {
        let given = options_of(&filters);
        if given.is_empty() {
            say!(words, "no entries match");
        } else {
            say!(
                words,
                "no entries match ({})",
                Theirs::from(given.trim_start())
            );
        }
    }
    for entry in entries {
        entry_line(&mut words, entry);
    }
    if let Some(earlier) = page.get("earlier").and_then(Value::as_u64) {
        let mut again: Vec<(&str, Option<String>)> = filters.to_vec();
        again.push(("limit", args.limit.map(|n| n.to_string())));
        again.push(("before", Some(earlier.to_string())));
        say!(
            words,
            "Older entries match: vibe admin operation-log{}",
            Theirs::from(options_of(&again).as_str())
        );
    }
    Ok(Done {
        json: page,
        text: words,
    })
}

/// One entry in words: a line of what it records, its statement on the
/// next, indented.
fn entry_line(words: &mut String, entry: &Value) {
    let member = |name: &str| entry.get(name).unwrap_or(&Value::Null);
    let or_dash = |name: &str| match member(name).as_str() {
        Some(value) => Theirs::from(value).to_string(),
        None => "-".to_owned(),
    };
    let mut line = format!(
        "{} {} {} {}",
        member("id").as_u64().unwrap_or(0),
        or_dash("time"),
        or_dash("outcome"),
        or_dash("user"),
    );
    match (member("application").as_str(), member("version").as_u64()) {
        (Some(application), Some(version)) => {
            put!(line, " {} Version {version}", Theirs::from(application));
        }
        (Some(application), None) => put!(line, " {}", Theirs::from(application)),
        (None, _) => line.push_str(" -"),
    }
    match member("host").as_str() {
        Some(host) => put!(line, " \u{2192} {}", Theirs::from(host)),
        None => match member("resource").as_str() {
            Some(resource) => put!(line, " {}", Theirs::from(resource)),
            None if member("own_database").as_bool() == Some(true) => {
                line.push_str(" own database")
            }
            None => line.push_str(" -"),
        },
    }
    put!(line, " {}", or_dash("operation"));
    for name in ["reason", "error"] {
        if let Some(value) = member(name).as_str() {
            put!(line, " {name} {}", Theirs::from(value));
        }
    }
    for name in ["rows", "duration_ms"] {
        if let Some(value) = member(name).as_u64() {
            put!(line, " {name} {value}");
        }
    }
    say!(words, "{line}");
    if let Some(statement) = member("statement").as_str() {
        say!(words, "    {}", Theirs::from(statement));
    }
}

/// A variable of the environment, as UTF-8 and not empty.
fn variable(name: &str) -> Option<String> {
    std::env::var_os(name)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
}

async fn connect_oidc(args: &ConnectOidcArgs) -> Result<Done, Ending> {
    let Some(client_secret) = variable(CLIENT_SECRET_VARIABLE).map(ClientSecret::new) else {
        return Err(Ending::new(
            2,
            "usage",
            format!(
                "{CLIENT_SECRET_VARIABLE} is not set: put the client secret the identity provider gave the platform in it (never on the command line), then run the command again"
            ),
        ));
    };
    // The body that carries the client secret is bytes sent once, in the
    // one request below; it is never printed or kept.
    let mut body = serde_json::Map::new();
    body.insert("issuer".into(), json!(args.issuer));
    body.insert("client_id".into(), json!(args.client_id));
    body.insert("client_secret".into(), json!(client_secret.expose()));
    body.insert(
        "administrators_group".into(),
        json!(args.administrators_group),
    );
    drop(client_secret);
    if let Some(claim) = &args.groups_claim {
        body.insert("groups_claim".into(), json!(claim));
    }
    if let Some(scopes) = &args.scopes {
        body.insert("scopes".into(), json!(scopes));
    }
    let body = Bytes::from(Value::Object(body).to_string());
    // The setup credential is a Secret from the moment it is read; a value
    // that cannot be one is said so, never shown.
    // What is kept, when the command goes with the tool's own credential:
    // it outlives the match, which lends where it is reached.
    let kept;
    let reply = match variable(SETUP_VARIABLE).map(Secret::new) {
        Some(setup) => {
            let Some(domain) = &args.domain else {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "with {SETUP_VARIABLE}, --domain says where the installation is (with --port and --resolve, or --connect for one on this machine): give it, then run the command again"
                    ),
                ));
            };
            let setup = setup.ok_or_else(|| {
                Ending::new(
                    2,
                    "usage",
                    format!(
                        "{SETUP_VARIABLE} does not hold a setup credential (vmsc_...): put the one `vibe setup` printed in it, then run the command again"
                    ),
                )
            })?;
            let at = Installation {
                domain,
                route: args.reach.route(),
            };
            let reply = client::send(
                &at,
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&setup),
                Some(body),
            )
            .await
            .map_err(|f| failed(at, f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(Ending::new(
                    3,
                    "setup-credential-refused",
                    format!(
                        "the installation at {at} ({domain}) refused the setup credential: it is used up, replaced by a later one or expired. Run `vibe setup` again beside the platform database, put the new one in {SETUP_VARIABLE}, then run this command again"
                    ),
                ));
            }
            (reply, at)
        }
        None => {
            if args.domain.is_some() || args.reach.given() {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "--domain, --port, --resolve and --connect go with the setup credential ({SETUP_VARIABLE}) only: signed in, the command goes to the installation the tool is signed in to. Leave them out, or set {SETUP_VARIABLE}"
                    ),
                ));
            }
            let place = Place::from_environment().map_err(Ending::credential_file)?;
            kept = read_kept(&place)?;
            let reply = client::send(
                &super::installation(&kept),
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&kept.credential),
                Some(body),
            )
            .await
            .map_err(|f| failed(kept.at(), f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(sign_in_expired(&kept));
            }
            (reply, kept.at())
        }
    };
    let (reply, at) = reply;
    if reply.status != StatusCode::OK {
        return Err(not_success(at, &reply));
    }
    let connection = reply
        .json
        .ok_or_else(|| unexpected(at, "its answer is not JSON"))?;
    let text_of = |name: &str| {
        Theirs::from(
            connection
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    };
    let scopes: Vec<Theirs> = connection
        .get("scopes")
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(Value::as_str)
                .map(Theirs::from)
                .collect()
        })
        .unwrap_or_default();
    let mut text = String::new();
    say!(
        text,
        "OIDC is connected: issuer {}, client id {}.",
        text_of("issuer"),
        text_of("client_id")
    );
    say!(
        text,
        "Groups are read from the claim {}; the scopes asked are {}.",
        text_of("groups_claim"),
        super::listed(&scopes, " ")
    );
    say!(
        text,
        "Platform administrators are the members of {}. Connected at {} by {}.",
        text_of("administrators_group"),
        text_of("connected_at"),
        text_of("connected_by")
    );
    Ok(Done {
        json: connection,
        text,
    })
}

/// `--kind`: the two kinds of Resource.
fn kind(text: &str) -> Result<String, String> {
    match text {
        "postgres" | "storage" => Ok(text.to_owned()),
        _ => Err("a Resource's kind is `postgres` or `storage`".to_owned()),
    }
}

/// `--directory`: an absolute path, as a storage registration needs.
fn absolute_path(text: &str) -> Result<String, String> {
    if std::path::Path::new(text).is_absolute() {
        Ok(text.to_owned())
    } else {
        Err("--directory is an absolute path".to_owned())
    }
}

/// A password of the environment: set and not empty, else exit code 2
/// naming the variable and never a value.
fn password(variable: &str) -> Result<String, Ending> {
    match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => value.into_string().map_err(|_| {
            Ending::new(
                2,
                "usage",
                format!("{variable} does not hold UTF-8: put the login's password in it as text, then run the command again"),
            )
        }),
        _ => Err(Ending::new(
            2,
            "usage",
            format!("a login is given without its password: put it in {variable}, then run the command again"),
        )),
    }
}

/// One login of a body: its role, and its password from `variable`.
fn login_of(role: &Option<String>, variable: &str) -> Result<Option<Value>, Ending> {
    match role {
        None => Ok(None),
        Some(role) => Ok(Some(
            json!({ "role": role, "password": password(variable)? }),
        )),
    }
}

/// A signed-in request with a JSON body; its answer must be `200` or `201`.
async fn send_signed_in(
    kept: &Kept,
    method: Method,
    path: &str,
    body: Value,
) -> Result<Value, Ending> {
    let bytes = Bytes::from(serde_json::to_vec(&body).map_err(|_| {
        Ending::new(
            1,
            "unexpected-answer",
            "the request could not be written: run the command again".to_owned(),
        )
    })?);
    let reply = client::send(
        &super::installation(kept),
        method,
        path,
        Sign::Credential(&kept.credential),
        Some(bytes),
    )
    .await
    .map_err(|f| failed(kept.at(), f))?;
    match reply.status {
        StatusCode::OK | StatusCode::CREATED => reply
            .json
            .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON")),
        StatusCode::UNAUTHORIZED => Err(sign_in_expired(kept)),
        _ => Err(reply_ending(kept, &reply)),
    }
}

/// A refusal of the installation: `login-failed` in words, one line for
/// each login that failed, and the API's answer as it is with `--json`;
/// every other as the tool says them everywhere.
fn reply_ending(kept: &Kept, reply: &client::Reply) -> Ending {
    let Some(body) = reply.json.clone() else {
        return not_success(kept.at(), reply);
    };
    let error = body.get("error");
    if error
        .and_then(|error| error.get("reason"))
        .and_then(Value::as_str)
        != Some("login-failed")
    {
        return not_success(kept.at(), reply);
    }
    let mut text = String::new();
    if let Some(failed) = error
        .and_then(|error| error.get("failed"))
        .and_then(Value::as_array)
    {
        for failure in failed {
            say!(
                text,
                "the {} login failed: {}",
                text_of(failure, "login"),
                text_of(failure, "why")
            );
        }
    }
    text.push_str(
        "Look at the role and its password at the shared database, and at the host and port; then run the command again.\n",
    );
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("a login of the Resource cannot be used")
        .to_owned();
    Ending::detailed(3, "login-failed", &message, Done { json: body, text })
}

/// A member of a body, set without indexing.
fn set(body: &mut Value, name: &str, value: Value) {
    if let Some(object) = body.as_object_mut() {
        object.insert(name.to_owned(), value);
    }
}

/// `vibe admin resources [<name>]`.
async fn resources(args: &ResourcesArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let page = match &args.name {
        Some(name) => get_signed_in(&kept, &format!("/v1/admin/resources/{name}")).await?,
        None => {
            let asked = [
                ("limit", args.limit.map(|n| n.to_string())),
                ("after", args.after.clone()),
            ];
            get_signed_in(&kept, &format!("/v1/admin/resources{}", query_of(&asked))).await?
        }
    };
    let mut words = String::new();
    match &args.name {
        Some(_) => resource_block(&mut words, &page),
        None => {
            let listed = page
                .get("resources")
                .and_then(Value::as_array)
                .ok_or_else(|| unexpected(kept.at(), "its answer has no list of Resources"))?;
            if listed.is_empty() {
                match &args.after {
                    Some(after) => say!(words, "No Resource comes after {after}."),
                    None => say!(
                        words,
                        "No Resource is registered on the installation yet. A Platform administrator registers them: vibe admin resource register <name> ..."
                    ),
                }
            }
            for resource in listed {
                resource_block(&mut words, resource);
                words.push('\n');
            }
            if let Some(next) = page.get("next").and_then(Value::as_str) {
                say!(
                    words,
                    "More Resources follow: vibe admin resources --after {}",
                    shell_word(next)
                );
            }
        }
    }
    Ok(Done {
        json: page,
        text: words,
    })
}

/// One Resource in words: its face, its logins, whether it is served, its
/// place and its Grants. Nothing of a login's value.
fn resource_block(words: &mut String, resource: &Value) {
    let name = text(resource, "name");
    let kind = text(resource, "kind");
    say!(words, "{name} ({kind})");
    let description = text(resource, "description");
    if !description.to_string().is_empty() {
        say!(words, "  {description}");
    }
    say!(words, "  Owner: {}", text(resource, "owner"));
    say!(words, "  For access, ask: {}", text(resource, "contact"));
    if let Some(logins) = resource.get("logins").and_then(Value::as_object) {
        let mut held: Vec<&str> = Vec::new();
        for (member, shown) in [
            ("read", "read"),
            ("write", "write"),
            ("structure", "structure"),
        ] {
            if logins.get(member).and_then(Value::as_bool) == Some(true) {
                held.push(shown);
            }
        }
        say!(words, "  logins: {}", held.join(", "));
    }
    if let Some(directory) = resource
        .get("location")
        .and_then(|location| location.get("directory"))
        .and_then(Value::as_str)
    {
        say!(words, "  location: {}", Theirs::from(directory));
    }
    if resource.get("usable").and_then(Value::as_bool) == Some(false) {
        let problem = resource
            .get("problem")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        say!(words, "  not usable: {problem}");
        let remedy = match problem {
            "no-secret-key" => {
                "start the program with VIBE_SECRET_KEY, then it is served".to_owned()
            }
            "does-not-open" | "not-sealed" => {
                "supply its logins again with vibe admin resource logins".to_owned()
            }
            _ => "look at its directory: the problem is the directory's, and a Platform administrator fixes it there".to_owned(),
        };
        say!(words, "  what to do: {remedy}");
    }
    let grants = resource
        .get("grants")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if grants.is_empty() {
        say!(words, "  no Group holds a Grant");
    } else {
        for grant in &grants {
            say!(
                words,
                "  {}  {}",
                text(grant, "group"),
                text(grant, "access")
            );
        }
        let more = resource
            .get("grants_more")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if more > 0 {
            say!(words, "  and {more} more");
        }
    }
}

/// `vibe admin resource register`.
async fn register(args: &RegisterArgs) -> Result<Done, Ending> {
    let mut body = json!({
        "name": args.name,
        "kind": args.kind,
        "description": args.description,
        "owner": args.owner,
        "contact": args.contact,
    });
    match args.kind.as_str() {
        "postgres" => {
            let usage = |what: &str| {
                Ending::new(
                    2,
                    "usage",
                    format!(
                        "a database Resource registers with {what}: give it, and the login's password in its variable, then run the command again"
                    ),
                )
            };
            let host = args.host.clone().ok_or_else(|| usage("--host"))?;
            let port = args.port.ok_or_else(|| usage("--port"))?;
            let database = args.database.clone().ok_or_else(|| usage("--database"))?;
            let read = args.read_role.clone().ok_or_else(|| usage("--read-role"))?;
            if args.directory.is_some() {
                return Err(Ending::new(
                    2,
                    "usage",
                    "a database Resource registers with --host, --port, --database and --read-role, not --directory: leave it out".to_owned(),
                ));
            }
            let mut connection = json!({
                "host": host,
                "port": port,
                "database": database,
                "read": { "role": read, "password": password(READ_PASSWORD)? },
            });
            if let Some(write) = login_of(&args.write_role, WRITE_PASSWORD)? {
                set(&mut connection, "write", write);
            }
            if let Some(structure) = login_of(&args.structure_role, STRUCTURE_PASSWORD)? {
                set(&mut connection, "structure", structure);
            }
            set(&mut body, "connection", connection);
        }
        _ => {
            let forbidden = [
                ("--host", args.host.is_some()),
                ("--port", args.port.is_some()),
                ("--database", args.database.is_some()),
                ("--read-role", args.read_role.is_some()),
                ("--write-role", args.write_role.is_some()),
                ("--structure-role", args.structure_role.is_some()),
            ];
            if let Some((what, _)) = forbidden.iter().find(|(_, given)| *given) {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "a storage Resource registers with --directory, not {what}: leave it out"
                    ),
                ));
            }
            let directory = args.directory.clone().ok_or_else(|| {
                Ending::new(
                    2,
                    "usage",
                    "a storage Resource registers with --directory: give it, then run the command again"
                        .to_owned(),
                )
            })?;
            set(
                &mut body,
                "location",
                json!({ "backend": "directory", "directory": directory }),
            );
        }
    }
    let kept = signed_in()?;
    let answer = send_signed_in(&kept, Method::POST, "/v1/resources", body).await?;
    let mut words = String::new();
    say!(
        words,
        "Registered {}. Every login connected.",
        text(&answer, "name")
    );
    say_warnings(&mut words, &answer);
    say!(
        words,
        "No Group has access until a Platform administrator gives a Grant: vibe admin grant {} --group <group> --access <level>.",
        text(&answer, "name")
    );
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// A member of a JSON object, as the installation's text.
fn text_of(value: &Value, name: &str) -> Theirs {
    text(value, name)
}

/// The warnings of a registration or a replacement, one line each.
fn say_warnings(words: &mut String, answer: &Value) {
    let warnings = answer
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for warning in &warnings {
        let login = text_of(warning, "login");
        let reason = warning
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let evidence = warning
            .get("evidence")
            .and_then(Value::as_array)
            .map(|kinds| {
                kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let said = match reason {
            "read-login-can-write" => format!(
                "the {login} login can write ({}): query still runs in a read-only transaction, and a login that cannot write is what the platform asks for",
                Theirs::from(evidence.as_str())
            ),
            "login-sees-no-table" => format!(
                "the {login} login holds a privilege on no table: nothing is listed in a pull"
            ),
            "structure-login-can-read" => format!(
                "the {login} login can read rows ({}): it is there so that pulls never use a login that can read rows",
                Theirs::from(evidence.as_str())
            ),
            "probe-incomplete" => format!(
                "the probe of the {login} login did not finish: the login connected, and its warning is not known until the next change of the logins"
            ),
            _ => format!("the {login} login gave a warning"),
        };
        let message = warning
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        say!(words, "warning: {said}. {}", Theirs::from(message));
    }
}

/// `vibe admin resource logins`.
async fn logins(args: &LoginsArgs) -> Result<Done, Ending> {
    let mut body = serde_json::Map::new();
    if let Some(host) = &args.host {
        body.insert("host".into(), json!(host));
    }
    if let Some(port) = args.port {
        body.insert("port".into(), json!(port));
    }
    if let Some(database) = &args.database {
        body.insert("database".into(), json!(database));
    }
    if let Some(read) = login_of(&args.read_role, READ_PASSWORD)? {
        body.insert("read".into(), read);
    }
    if let Some(write) = login_of(&args.write_role, WRITE_PASSWORD)? {
        body.insert("write".into(), write);
    }
    if args.no_write {
        body.insert("write".into(), Value::Null);
    }
    if let Some(structure) = login_of(&args.structure_role, STRUCTURE_PASSWORD)? {
        body.insert("structure".into(), structure);
    }
    if args.no_structure {
        body.insert("structure".into(), Value::Null);
    }
    if body.is_empty() {
        return Err(Ending::new(
            2,
            "usage",
            "a replacement gives at least one member: --host, --port, --database, --read-role, --write-role, --no-write, --structure-role or --no-structure".to_owned(),
        ));
    }
    let kept = signed_in()?;
    let path = format!("/v1/admin/resources/{}", args.name);
    let before = get_signed_in(&kept, &path).await.ok();
    let was_not_served = before
        .as_ref()
        .and_then(|view| view.get("usable"))
        .and_then(Value::as_bool)
        != Some(true);
    let answer = send_signed_in(
        &kept,
        Method::PUT,
        &format!("/v1/resources/{}/connection", args.name),
        Value::Object(body),
    )
    .await?;
    let mut words = String::new();
    say_warnings(&mut words, &answer);
    say!(
        words,
        "The new logins hold from the next operation on, in every Application; no Application is deployed again."
    );
    if was_not_served && answer.get("usable").and_then(Value::as_bool) == Some(true) {
        say!(words, "The Resource is served now.");
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// `vibe admin resource edit`.
async fn edit(args: &EditArgs) -> Result<Done, Ending> {
    let mut body = serde_json::Map::new();
    for (name, value) in [
        ("description", &args.description),
        ("owner", &args.owner),
        ("contact", &args.contact),
    ] {
        if let Some(value) = value {
            body.insert(name.into(), json!(value));
        }
    }
    if body.is_empty() {
        return Err(Ending::new(
            2,
            "usage",
            "an edit gives at least one of --description, --owner or --contact".to_owned(),
        ));
    }
    let kept = signed_in()?;
    let answer = send_signed_in(
        &kept,
        Method::PATCH,
        &format!("/v1/resources/{}", args.name),
        Value::Object(body),
    )
    .await?;
    let mut changed: Vec<&str> = Vec::new();
    if args.description.is_some() {
        changed.push("description");
    }
    if args.owner.is_some() {
        changed.push("owner");
    }
    if args.contact.is_some() {
        changed.push("contact");
    }
    let mut words = String::new();
    say!(
        words,
        "{}: {} changed.",
        text(&answer, "name"),
        changed.join(", ")
    );
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// `vibe admin resource location`.
async fn location(args: &LocationArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let answer = send_signed_in(
        &kept,
        Method::PUT,
        &format!("/v1/resources/{}/location", args.name),
        json!({ "backend": "directory", "directory": args.directory }),
    )
    .await?;
    let directory = answer
        .get("location")
        .and_then(|location| location.get("directory"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let changed = answer.get("changed").and_then(Value::as_bool) == Some(true);
    let mut words = String::new();
    if changed {
        say!(
            words,
            "{}: its location is now {}; its files were not moved, and the old directory's files stay where they are.",
            text(&answer, "resource"),
            Theirs::from(directory)
        );
    } else {
        say!(
            words,
            "{}: it is already on {}; nothing was changed, and its files stay where they are.",
            text(&answer, "resource"),
            Theirs::from(directory)
        );
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// `vibe admin resource remove`.
async fn remove(args: &RemoveArgs) -> Result<Done, Ending> {
    if args.yes != 1 {
        return Err(Ending::new(
            2,
            "usage",
            "remove needs --yes: it removes the Resource and its Grants, and nothing can undo it"
                .to_owned(),
        ));
    }
    let kept = signed_in()?;
    let answer = send_signed_in(
        &kept,
        Method::DELETE,
        &format!("/v1/resources/{}", args.name),
        Value::Null,
    )
    .await?;
    let name = text(&answer, "name");
    let grants = answer
        .get("grants_removed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut words = String::new();
    say!(
        words,
        "{name} removed; {grants} {} went with it.",
        if grants == 1 { "Grant" } else { "Grants" }
    );
    if let Some(declared) = answer.get("declared_by").and_then(Value::as_array) {
        for entry in declared {
            say!(
                words,
                "  {} (Version {}, declared at {}) is stopped: operations on {name} are refused, and nothing is deployed again.",
                text(entry, "application"),
                entry.get("version").and_then(Value::as_u64).unwrap_or(0),
                text(entry, "access")
            );
        }
    }
    if let Some(more) = answer.get("declared_by_more").and_then(Value::as_u64)
        && more > 0
    {
        say!(
            words,
            "  and {more} more Applications declare it and are stopped."
        );
    }
    if answer.get("kind").and_then(Value::as_str) == Some("storage") {
        match answer.get("directory_cleaned").and_then(Value::as_bool) {
            Some(true) => say!(
                words,
                "  Its files were not touched; the directory's `.vibe-resource` and an empty `.vibe-staging/` were removed."
            ),
            Some(false) => say!(
                words,
                "  Its files were not touched; `.vibe-resource` and an empty `.vibe-staging/` were left where they were, so another name sees the directory in use until `.vibe-resource` is removed by hand."
            ),
            None => {}
        }
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// The day of an RFC 3339 time: its first ten bytes when it has them.
fn day_of(time: &str) -> &str {
    time.get(..10).unwrap_or(time)
}

/// `vibe admin grant <resource> --group <group> --access <level>`.
async fn grant(args: &GrantArgs) -> Result<Done, Ending> {
    if crate::resources::level_of(&args.access).is_none() {
        return Err(Ending::new(
            2,
            "usage",
            "access is not one of structure, read and write: give one of those".to_owned(),
        ));
    }
    let kept = signed_in()?;
    let answer = send_signed_in(
        &kept,
        Method::POST,
        &format!("/v1/resources/{}/grants", args.resource),
        json!({ "group": args.group, "access": args.access }),
    )
    .await?;
    let resource = text(&answer, "resource");
    let group = text(&answer, "group");
    let access = text(&answer, "access");
    let was = answer.get("was").and_then(Value::as_str);
    let changed = answer.get("changed").and_then(Value::as_bool) == Some(true);
    let mut words = String::new();
    match (changed, was) {
        (true, Some(was)) => say!(
            words,
            "The Group {group} held {was} on {resource} and holds {access} from now on, in every Application."
        ),
        (true, None) => say!(
            words,
            "The Group {group} holds {access} on {resource} from now on, in every Application."
        ),
        _ => say!(
            words,
            "The Group {group} already held {access} on {resource}: nothing changed."
        ),
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// `vibe admin revoke <resource> --group <group>`.
async fn revoke(args: &RevokeArgs) -> Result<Done, Ending> {
    let kept = signed_in()?;
    let answer = send_signed_in(
        &kept,
        Method::DELETE,
        &format!(
            "/v1/resources/{}/grants/{}",
            args.resource,
            crate::resources::encode_segment(&args.group)
        ),
        Value::Null,
    )
    .await?;
    let resource = text(&answer, "resource");
    let group = text(&answer, "group");
    let changed = answer.get("changed").and_then(Value::as_bool) == Some(true);
    let mut words = String::new();
    if changed {
        let was = text(&answer, "was");
        say!(
            words,
            "The Group {group} no longer holds a Grant on {resource}: it held {was}. This holds from now on in every Application; vibe admin grant gives it again."
        );
    } else {
        say!(
            words,
            "The Group {group} held none on {resource}: nothing changed."
        );
    }
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// A Grant in words: who set it and when, or `not recorded` for one from
/// before stage three.
fn grant_line(words: &mut String, group: &Theirs, access: &Theirs, grant: &Value) {
    let by = grant.get("granted_by").and_then(Value::as_str);
    let at = grant.get("granted_at").and_then(Value::as_str);
    match (by, at) {
        (Some(by), Some(at)) => say!(
            words,
            "  {group}  {access}  set by {} on {}",
            Theirs::from(by),
            Theirs::from(day_of(at))
        ),
        _ => say!(words, "  {group}  {access}  not recorded"),
    }
}

/// `vibe admin grants (--resource <name> | --group <group>)`.
async fn grants(args: &GrantsArgs) -> Result<Done, Ending> {
    let asked = match (&args.resource, &args.group) {
        (Some(resource), None) => Ok(Either::Resource(resource)),
        (None, Some(group)) => Ok(Either::Group(group)),
        _ => Err(Ending::new(
            2,
            "usage",
            "grants lists one Resource's or one Group's: give exactly one of --resource and --group"
                .to_owned(),
        )),
    };
    // What the tool checks itself comes before the sign-in is looked for.
    let asked = asked?;
    let kept = signed_in()?;
    let mut words = String::new();
    let answer = match asked {
        Either::Resource(resource) => {
            let answer = get_signed_in(&kept, &format!("/v1/resources/{resource}/grants")).await?;
            let listed = answer
                .get("grants")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if listed.is_empty() {
                say!(
                    words,
                    "No Group holds a Grant on {resource}: a Platform administrator gives one with vibe admin grant {resource} --group <group> --access <level>."
                );
            } else {
                for grant in &listed {
                    grant_line(
                        &mut words,
                        &text(grant, "group"),
                        &text(grant, "access"),
                        grant,
                    );
                }
                let more = answer
                    .get("grants_more")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if more > 0 {
                    say!(words, "  and {more} more");
                }
            }
            answer
        }
        Either::Group(group) => {
            let answer = get_signed_in(
                &kept,
                &format!(
                    "/v1/admin/grants?group={}",
                    crate::resources::encode_segment(group)
                ),
            )
            .await?;
            let listed = answer
                .get("grants")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if listed.is_empty() {
                say!(
                    words,
                    "The Group {} holds none: a Platform administrator gives one with vibe admin grant <resource> --group {} --access <level>.",
                    Theirs::from(group),
                    Theirs::from(group)
                );
            } else {
                for grant in &listed {
                    let resource = text(grant, "resource");
                    let kind = text(grant, "kind");
                    let access = text(grant, "access");
                    let by = grant.get("granted_by").and_then(Value::as_str);
                    let at = grant.get("granted_at").and_then(Value::as_str);
                    match (by, at) {
                        (Some(by), Some(at)) => say!(
                            words,
                            "  {resource} ({kind})  {access}  set by {} on {}",
                            Theirs::from(by),
                            Theirs::from(day_of(at))
                        ),
                        _ => say!(words, "  {resource} ({kind})  {access}  not recorded"),
                    }
                }
            }
            answer
        }
    };
    Ok(Done {
        json: answer,
        text: words,
    })
}

/// Which of `grants`' two lists was asked for.
enum Either<'a> {
    Resource(&'a str),
    Group(&'a str),
}
