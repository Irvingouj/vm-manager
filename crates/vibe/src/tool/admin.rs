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
use crate::resources::is_resource_name;

/// The variable that holds the client secret.
const CLIENT_SECRET_VARIABLE: &str = "VIBE_OIDC_CLIENT_SECRET";

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
    #[arg(long, value_parser = resource_name)]
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

fn resource_name(text: &str) -> Result<String, String> {
    if is_resource_name(text) {
        Ok(text.to_owned())
    } else {
        Err("not a Resource's name: 1 to 63 lower-case letters, digits and underscores, starting with a letter".into())
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
    }
}

/// The kept sign-in: exit code `4` when there is none.
fn signed_in() -> Result<Kept, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    read_kept(&place)
}

/// `text` as a query's value: every byte but the unreserved ones written
/// `%XX` (so `+` is `%2B`, never a space).
fn query_value(text: &str) -> String {
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

/// A query of `pairs` given, `?a=b&c=d`, or nothing.
fn query_of(pairs: &[(&str, Option<String>)]) -> String {
    let mut query = String::new();
    for (name, value) in pairs {
        if let Some(value) = value {
            query.push(if query.is_empty() { '?' } else { '&' });
            put!(query, "{name}={}", query_value(value));
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
