//! The platform's API, at `api.<domain>` (blackbox/CONTRACT.md, "The
//! platform's API"): HTTP in, calls on the Application registry, JSON out.
//!
//! Interface: [`Api::handle`], for a request whose caller Ingress has
//! already signed in. It reads the body (up to [`MAX_BODY_BYTES`]), finds
//! the route, and answers. Refusals are decided in the contract's order:
//! `too-large`, `not-found`, `method-not-allowed`, `not-a-contributor`,
//! `malformed`, then the rest. Adding an endpoint is a [`Route`] and a
//! handler.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::{Method, StatusCode};
use serde::de::{self, DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::log;
use crate::platform_db::DbError;
use crate::registry::{
    ApplicationRecord, Created, DeclaredAccess, NameProblem, NewVersion, Registry, Resource, Sent,
    VersionRecord, check_name,
};
use crate::user::User;

/// The largest request body the API reads; past it the answer is `413`.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The longest a client may take to send a request's body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// An answer of the API: a status and a JSON body.
pub struct Answer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Answer {
    /// `value` as JSON, written straight from what it borrows.
    fn json(status: StatusCode, value: &impl Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self {
                status,
                body: Bytes::from(body),
            },
            // Only a map with keys that are not strings fails, and the
            // views below have none.
            Err(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: Bytes::from_static(
                    br#"{"error":{"reason":"internal","message":"the answer could not be written"}}"#,
                ),
            },
        }
    }

    fn error(status: StatusCode, reason: &str, message: &str) -> Self {
        Self::json(
            status,
            &json!({ "error": { "reason": reason, "message": message } }),
        )
    }

    fn not_found(message: &str) -> Self {
        Self::error(StatusCode::NOT_FOUND, "not-found", message)
    }

    fn method_not_allowed(method: &Method, allowed: &str) -> Self {
        Self::error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method-not-allowed",
            &format!("this path does not take {method}: use {allowed}"),
        )
    }

    fn malformed(problem: &str, form: &str) -> Self {
        Self::error(
            StatusCode::BAD_REQUEST,
            "malformed",
            &format!("the body is not the JSON asked for ({problem}): send {form}"),
        )
    }

    fn unavailable(error: &DbError) -> Self {
        log::record(
            "platform-database-failed",
            json!({ "error": error.to_string() }),
        );
        Self::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the platform could not reach its database: try again shortly",
        )
    }
}

/// The paths the API has.
enum Route<'a> {
    /// `/v1/applications`
    Applications,
    /// `/v1/applications/<name>`
    Application(&'a str),
    /// `/v1/applications/<name>/versions`
    Versions(&'a str),
    /// `/v1/applications/<name>/versions/<number>`; `None` when the number
    /// cannot name a Version.
    Version(&'a str, Option<u64>),
}

impl<'a> Route<'a> {
    /// The route of a path, or `None` for a path the API does not have.
    /// The path is matched as sent: nothing is decoded or normalised, so
    /// `..`, `%2F` or an empty segment never reach another route.
    fn of(path: &'a str) -> Option<Self> {
        let rest = path.strip_prefix("/v1/")?;
        let segments: Vec<&str> = rest.split('/').collect();
        match segments.as_slice() {
            ["applications"] => Some(Self::Applications),
            ["applications", name] => Some(Self::Application(name)),
            ["applications", name, "versions"] => Some(Self::Versions(name)),
            ["applications", name, "versions", number] => {
                Some(Self::Version(name, version_number(number)))
            }
            _ => None,
        }
    }
}

/// A Version number as written in a path: digits, no sign, no leading
/// zero, above 0.
fn version_number(text: &str) -> Option<u64> {
    let canonical =
        !text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit());
    canonical.then(|| text.parse().ok()).flatten()
}

/// `POST /v1/applications`
struct CreateApplication {
    name: String,
    description: Option<String>,
}

const CREATE_FORM: &str = r#"{ "name": "<name>", "description": "<text, optional>" }"#;

impl CreateApplication {
    fn read(body: &[u8]) -> Result<Self, String> {
        let object = Object::read(body)?;
        Ok(Self {
            name: object.required("name")?,
            description: object.optional("description")?,
        })
    }
}

/// `POST /v1/applications/<name>/versions`
const VERSION_FORM: &str = r#"{ "bundle": "<the classic script>", "declared_access": { "resources": { "<name>": { "kind": "postgres", "access": "structure|read|write" } } }, "git_commit": "<40 hex characters>" }"#;

fn read_version(body: &[u8]) -> Result<NewVersion, String> {
    let object = Object::read(body)?;
    let git_commit: String = object.required("git_commit")?;
    if git_commit.len() != 40 || !git_commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("git_commit is not 40 hex characters".into());
    }
    let declared = object.object("declared_access")?;
    let mut resources = BTreeMap::new();
    for (name, value) in declared.object("resources")?.0 {
        let resource =
            Object::read_value(value).map_err(|e| format!("the Resource {name:?}: {e}"))?;
        let resource = Resource {
            kind: resource.required("kind")?,
            access: resource.required("access")?,
        };
        resources.insert(name, resource);
    }
    Ok(NewVersion {
        bundle: object.required("bundle")?,
        declared_access: DeclaredAccess { resources },
        git_commit,
    })
}

/// A JSON object read strictly: an object (never an array standing in for
/// one), each key at most once, and its values kept as text until asked
/// for by type. Keys the request does not have are ignored.
struct Object<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys;
        impl<'de> Visitor<'de> for Keys {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object<'de>, M::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if entries.contains_key(&key) {
                        return Err(de::Error::custom(format_args!(
                            "the key {key:?} is given twice"
                        )));
                    }
                    entries.insert(key, value);
                }
                Ok(Object(entries))
            }
        }
        deserializer.deserialize_map(Keys)
    }
}

impl<'a> Object<'a> {
    fn read(json: &'a [u8]) -> Result<Self, String> {
        serde_json::from_slice(json).map_err(|e| e.to_string())
    }

    fn read_value(value: &'a RawValue) -> Result<Self, String> {
        serde_json::from_str(value.get()).map_err(|e| e.to_string())
    }

    fn required<T: DeserializeOwned>(&self, key: &str) -> Result<T, String> {
        let value = self.0.get(key).ok_or_else(|| format!("{key} is missing"))?;
        serde_json::from_str(value.get()).map_err(|e| format!("{key}: {e}"))
    }

    /// Absent and `null` are both `None`.
    fn optional<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        match self.0.get(key) {
            None => Ok(None),
            Some(value) => serde_json::from_str(value.get()).map_err(|e| format!("{key}: {e}")),
        }
    }

    fn object(&self, key: &str) -> Result<Object<'a>, String> {
        let value = self.0.get(key).ok_or_else(|| format!("{key} is missing"))?;
        Self::read_value(value).map_err(|e| format!("{key}: {e}"))
    }
}

pub struct Api {
    registry: Arc<Registry>,
    domain: Arc<str>,
}

impl Api {
    pub fn new(registry: Arc<Registry>, domain: Arc<str>) -> Self {
        Self { registry, domain }
    }

    /// Answers one request of a signed-in `user`.
    pub async fn handle(&self, user: &User, method: &Method, path: &str, body: Incoming) -> Answer {
        let read = Limited::new(body, MAX_BODY_BYTES).collect();
        let body = match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(_)) => {
                return Answer::error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too-large",
                    "the body is larger than 16 MiB: send a smaller bundle",
                );
            }
            Err(_) => {
                return Answer::error(
                    StatusCode::REQUEST_TIMEOUT,
                    "timed-out",
                    "the body did not arrive in time: send it again",
                );
            }
        };
        let Some(route) = Route::of(path) else {
            return Answer::not_found(
                "the API has no such path: its paths are /v1/applications, /v1/applications/<name>, /v1/applications/<name>/versions and /v1/applications/<name>/versions/<number>",
            );
        };
        let answer = match route {
            Route::Applications => self.applications(user, method, &body).await,
            Route::Application(name) => self.application(method, name).await,
            Route::Versions(name) => self.versions(user, method, name, &body).await,
            Route::Version(name, number) => self.version(method, name, number).await,
        };
        answer.unwrap_or_else(|error| Answer::unavailable(&error))
    }

    async fn applications(
        &self,
        user: &User,
        method: &Method,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method != Method::POST {
            return Ok(Answer::method_not_allowed(method, "POST"));
        }
        let request = match CreateApplication::read(body) {
            Ok(request) => request,
            Err(problem) => return Ok(Answer::malformed(&problem, CREATE_FORM)),
        };
        match check_name(&request.name) {
            Ok(()) => {}
            Err(NameProblem::Invalid) => {
                return Ok(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "invalid-name",
                    "an Application's name is its subdomain: use 1 to 63 lower-case letters, digits and hyphens, not starting or ending with a hyphen, and not starting with xn--",
                ));
            }
            Err(NameProblem::Reserved) => {
                return Ok(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "reserved-name",
                    "this name is one of the platform's own (api, auth, dashboard, git, admin, www): choose another",
                ));
            }
        }
        let created = self
            .registry
            .create_application(&request.name, request.description.as_deref(), &user.id)
            .await?;
        Ok(match created {
            Created::Created(application) => {
                Answer::json(StatusCode::CREATED, &self.view(&application))
            }
            Created::NameTaken => Answer::error(
                StatusCode::CONFLICT,
                "name-taken",
                "an Application with this name exists: choose another name",
            ),
        })
    }

    async fn application(&self, method: &Method, name: &str) -> Result<Answer, DbError> {
        let Some(application) = self.find(name).await? else {
            return Ok(no_application());
        };
        Ok(if method == Method::GET {
            Answer::json(StatusCode::OK, &self.view(&application))
        } else {
            Answer::method_not_allowed(method, "GET")
        })
    }

    async fn versions(
        &self,
        user: &User,
        method: &Method,
        name: &str,
        body: &[u8],
    ) -> Result<Answer, DbError> {
        if method == Method::GET {
            let versions = if check_name(name).is_ok() {
                self.registry.versions(name).await?
            } else {
                None
            };
            let Some(versions) = versions else {
                return Ok(no_application());
            };
            let versions = VersionsView {
                versions: versions.iter().map(VersionView::of).collect(),
            };
            return Ok(Answer::json(StatusCode::OK, &versions));
        }
        if method != Method::POST {
            return Ok(match self.find(name).await? {
                Some(_) => Answer::method_not_allowed(method, "GET or POST"),
                None => no_application(),
            });
        }
        let contributor = if check_name(name).is_ok() {
            self.registry.is_contributor(name, &user.id).await?
        } else {
            None
        };
        match contributor {
            None => return Ok(no_application()),
            Some(false) => return Ok(not_a_contributor()),
            Some(true) => {}
        }
        let version = match read_version(body) {
            Ok(version) => version,
            Err(problem) => return Ok(Answer::malformed(&problem, VERSION_FORM)),
        };
        Ok(
            match self.registry.send_version(name, version, &user.id).await? {
                Sent::Sent(version) => {
                    Answer::json(StatusCode::CREATED, &VersionView::of(&version))
                }
                Sent::NoSuchApplication => no_application(),
                Sent::NotAContributor => not_a_contributor(),
            },
        )
    }

    async fn version(
        &self,
        method: &Method,
        name: &str,
        number: Option<u64>,
    ) -> Result<Answer, DbError> {
        let version = match number {
            Some(number) if check_name(name).is_ok() => self.registry.version(name, number).await?,
            _ => None,
        };
        let Some(version) = version else {
            return Ok(Answer::not_found(
                "no such Version: list the Application's Versions at /v1/applications/<name>/versions",
            ));
        };
        Ok(if method == Method::GET {
            Answer::json(StatusCode::OK, &VersionView::of(&version))
        } else {
            Answer::method_not_allowed(method, "GET")
        })
    }

    /// The Application `name`; `None` also for a name no Application can
    /// have, without asking the database.
    async fn find(&self, name: &str) -> Result<Option<ApplicationRecord>, DbError> {
        if check_name(name).is_err() {
            return Ok(None);
        }
        self.registry.application(name).await
    }

    fn view<'a>(&self, application: &'a ApplicationRecord) -> ApplicationView<'a> {
        ApplicationView {
            name: &application.name,
            description: application.description.as_deref(),
            address: format!("{}.{}", application.name, self.domain),
            contributors: application
                .contributors
                .iter()
                .map(|user| ContributorView { user })
                .collect(),
            serving_version: application.serving_version,
            created_by: &application.created_by,
            created_at: &application.created_at,
        }
    }
}

/// An Application as the API answers it.
#[derive(Serialize)]
struct ApplicationView<'a> {
    name: &'a str,
    description: Option<&'a str>,
    address: String,
    contributors: Vec<ContributorView<'a>>,
    serving_version: Option<u64>,
    created_by: &'a str,
    created_at: &'a str,
}

#[derive(Serialize)]
struct ContributorView<'a> {
    user: &'a str,
}

/// A Version as the API answers it.
#[derive(Serialize)]
struct VersionView<'a> {
    application: &'a str,
    number: u64,
    declared_access: &'a Value,
    git_commit: &'a str,
    bundle_sha256: &'a str,
    created_by: &'a str,
    created_at: &'a str,
}

impl<'a> VersionView<'a> {
    fn of(version: &'a VersionRecord) -> Self {
        Self {
            application: &version.application,
            number: version.number,
            declared_access: &version.declared_access,
            git_commit: &version.git_commit,
            bundle_sha256: &version.bundle_sha256,
            created_by: &version.created_by,
            created_at: &version.created_at,
        }
    }
}

#[derive(Serialize)]
struct VersionsView<'a> {
    versions: Vec<VersionView<'a>>,
}

fn no_application() -> Answer {
    Answer::not_found("no such Application: create it with POST /v1/applications")
}

fn not_a_contributor() -> Answer {
    Answer::error(
        StatusCode::FORBIDDEN,
        "not-a-contributor",
        "you are not a Contributor of this Application: ask one of its Contributors to send the Version, or to add you",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_numbers_in_paths_are_canonical_decimals_above_zero() {
        assert_eq!(version_number("1"), Some(1));
        assert_eq!(version_number("42"), Some(42));
        for text in [
            "",
            "0",
            "01",
            "-1",
            "+1",
            "1.5",
            "1e0",
            "%31",
            "18446744073709551616",
        ] {
            assert_eq!(version_number(text), None, "{text}");
        }
    }

    #[test]
    fn bodies_are_objects_with_each_key_once() {
        assert!(CreateApplication::read(br#"{"name":"a","other":[1]}"#).is_ok());
        for body in [
            &br#"["a"]"#[..],
            br#"{"name":"a","name":"b"}"#,
            br#"{"name":5}"#,
            br#"{}"#,
            b"{\"name\":\"\xff\"}",
        ] {
            assert!(
                CreateApplication::read(body).is_err(),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        let good = br#"{"bundle":"x","declared_access":{"resources":{"r":{"kind":"postgres","access":"read"}}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#;
        assert!(read_version(good).is_ok());
        for body in [
            r#"{"bundle":"x","declared_access":[{"resources":{}}],"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{"r":["postgres","read"]}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{"r":{"kind":"postgres","access":"read"},"r":{"kind":"postgres","access":"write"}}},"git_commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            r#"{"bundle":"x","declared_access":{"resources":{}},"git_commit":"0123456789abcdef0123456789abcdef0123456"}"#,
        ] {
            assert!(read_version(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn paths_are_matched_as_sent() {
        assert!(matches!(
            Route::of("/v1/applications"),
            Some(Route::Applications)
        ));
        assert!(matches!(
            Route::of("/v1/applications/a/versions"),
            Some(Route::Versions("a"))
        ));
        for path in [
            "/",
            "/v1",
            "/v1/",
            "/v2/applications",
            "/V1/applications",
            "/v1/applications/a/b",
            "/v1/applications/a/versions/1/x",
        ] {
            assert!(Route::of(path).is_none(), "{path}");
        }
    }
}
