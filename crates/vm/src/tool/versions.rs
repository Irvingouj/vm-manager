//! `vm versions <application> [--limit <n>] [--json]` and `vm go-back <application>
//! [--to <number>] [--json]` (blackbox/CONTRACT.md, "Versions, going back,
//! and logs: The commands").
//!
//! Each is a few requests to the installation ([`client`]), each with its
//! own end: the Application (`GET /v1/applications/<name>`), then its
//! Versions, or the go-back itself (`POST .../go-back` with the serving
//! Version the tool saw as `from`, so that a deploy or a go-back between
//! the two requests is the API's `serving-changed`, not a surprise).
//! Text of the installation's reaches words only through [`Theirs`].

use std::fmt::Write as _;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;

use super::client::{self, Sign};
use super::credential_file::{Kept, Place};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, get_signed_in, installation, not_success, read_kept, shaped,
    sign_in_expired, unexpected,
};
use crate::registry::check_name;

/// `vm versions`.
#[derive(clap::Args)]
pub struct VersionsArgs {
    /// The Application, by name.
    pub application: String,
    /// How many of the newest Versions to print: 1 to 1000.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    #[command(flatten)]
    pub output: Output,
}

/// `vm go-back`.
#[derive(clap::Args)]
pub struct GoBackArgs {
    /// The Application, by name.
    pub application: String,
    /// The Version to serve instead: an earlier one than the serving
    /// Version. Without it, the one just before.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub to: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// The Application as `GET /v1/applications/<name>` answers it.
#[derive(Deserialize)]
struct Application {
    address: Theirs,
    serving_version: Option<u64>,
    latest_version: Option<u64>,
}

/// A Version as `GET /v1/applications/<name>/versions` lists it.
#[derive(Deserialize)]
struct Version {
    number: u64,
    git_commit: Theirs,
    created_at: Theirs,
    created_by: Theirs,
}

#[derive(Deserialize)]
struct VersionList {
    versions: Vec<Version>,
}

/// What `POST .../go-back` answers.
#[derive(Deserialize)]
struct GoneBack {
    serving_version: u64,
    was: u64,
    migrations_not_undone: Vec<Theirs>,
}

/// The kept sign-in and the Application's name, checked: a name no
/// Application can have is one the installation does not have.
pub(super) fn signed_in_for(name: &str) -> Result<Kept, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    if check_name(name).is_err() {
        return Err(Ending::new(
            3,
            "not-found",
            format!(
                "{} is not the name of an Application: see yours with vm applications",
                Theirs::from(name)
            ),
        ));
    }
    Ok(kept)
}

async fn application(kept: &Kept, name: &str) -> Result<Application, Ending> {
    shaped(
        kept.connect,
        get_signed_in(kept, &format!("/v1/applications/{name}")).await?,
    )
}

pub(super) async fn versions(args: &VersionsArgs) -> Result<Done, Ending> {
    let name = args.application.as_str();
    let kept = signed_in_for(name)?;
    let application = application(&kept, name).await?;
    let list: VersionList = shaped(
        kept.connect,
        get_signed_in(
            &kept,
            &format!("/v1/applications/{name}/versions?limit={}", args.limit),
        )
        .await?,
    )?;
    let serving = application.serving_version;
    // Numbers run from 1 without a gap: those before the first printed are
    // the ones not shown.
    let not_shown = list
        .versions
        .first()
        .map_or(0, |first| first.number.saturating_sub(1));
    let mut text = String::new();
    if list.versions.is_empty() {
        writeln!(
            text,
            "{name} ({}) has no Version yet: vm deploy sends the first.",
            application.address
        )
        .ok();
    } else {
        writeln!(
            text,
            "The Versions of {name} ({}), oldest first:",
            application.address
        )
        .ok();
    }
    for version in &list.versions {
        let mark = if Some(version.number) == serving {
            "  (serving)"
        } else {
            ""
        };
        writeln!(
            text,
            "  Version {}  commit {}  deployed {} by {}{mark}",
            version.number, version.git_commit, version.created_at, version.created_by
        )
        .ok();
    }
    if not_shown > 0 {
        let (versions, are, them) = if not_shown == 1 {
            ("Version", "is", "it")
        } else {
            ("Versions", "are", "them")
        };
        writeln!(
            text,
            "{not_shown} older {versions} {are} not shown: vm versions {name} --limit <n> shows {them} too (up to 1000)."
        )
        .ok();
        if let Some(number) = serving.filter(|&n| n <= not_shown) {
            writeln!(text, "Version {number}, one of those, serves.").ok();
        }
    }
    let versions: Vec<_> = list
        .versions
        .iter()
        .map(|v| {
            json!({
                "number": v.number,
                "git_commit": v.git_commit,
                "deployed_at": v.created_at,
                "deployed_by": v.created_by,
                "serving": Some(v.number) == serving,
            })
        })
        .collect();
    Ok(Done {
        json: json!({
            "application": name,
            "address": application.address,
            "serving_version": serving,
            "latest_version": application.latest_version,
            "versions": versions,
        }),
        text,
    })
}

pub(super) async fn go_back(args: &GoBackArgs) -> Result<Done, Ending> {
    let name = args.application.as_str();
    let kept = signed_in_for(name)?;
    let application = application(&kept, name).await?;
    let from = match application.serving_version {
        Some(from) if from > 1 => from,
        serving => {
            let why = match serving {
                Some(_) => "its serving Version, Version 1, is its first",
                None => "it has no Version",
            };
            return Err(Ending::new(
                3,
                "no-previous-version",
                format!(
                    "{name} cannot go back: {why}, so there is nothing to go back to. Nothing was changed"
                ),
            ));
        }
    };
    let to = args.to.unwrap_or(from - 1);
    if to >= from {
        let earlier = if from == 2 {
            "the only earlier Version is 1".to_owned()
        } else {
            format!("the earlier Versions are 1 to {}", from - 1)
        };
        return Err(Ending::new(
            3,
            "no-such-version",
            format!(
                "Version {to} is not an earlier Version of {name}: Version {from} serves, and {earlier} (vm versions {name} lists them). Going back only goes to an earlier Version; to serve a later one again, deploy it again. Nothing was changed"
            ),
        ));
    }
    let body = Bytes::from(json!({ "from": from, "to": to }).to_string());
    let reply = client::send(
        &installation(&kept),
        Method::POST,
        &format!("/v1/applications/{name}/go-back"),
        Sign::Credential(&kept.credential),
        Some(body),
    )
    .await
    .map_err(|f| super::failed(kept.connect, f))?;
    let gone: GoneBack = match reply.status {
        StatusCode::OK => shaped(
            kept.connect,
            reply
                .json
                .ok_or_else(|| unexpected(kept.connect, "its answer is not JSON"))?,
        )?,
        StatusCode::UNAUTHORIZED => return Err(sign_in_expired(&kept)),
        _ => return Err(not_success(kept.connect, &reply)),
    };
    let mut text = String::new();
    writeln!(
        text,
        "Version {} of {name} now serves at {}, in place of Version {}.",
        gone.serving_version, application.address, gone.was
    )
    .ok();
    if gone.migrations_not_undone.is_empty() {
        writeln!(
            text,
            "Migrations are not undone by going back; Version {} has every migration Version {} had, so its own database is as its code expects.",
            gone.serving_version,
            gone.was
        ).ok();
    } else {
        writeln!(
            text,
            "Migrations are not undone by going back: the own database keeps what {} made, and Version {}'s code now runs against it. The next deploy must still carry them in migrations/ (a deployed migration is never removed).",
            super::listed(&gone.migrations_not_undone, ", "),
            gone.serving_version
        ).ok();
    }
    Ok(Done {
        json: json!({
            "application": name,
            "address": application.address,
            "serving_version": gone.serving_version,
            "was": gone.was,
            "migrations_not_undone": gone.migrations_not_undone,
        }),
        text,
    })
}
