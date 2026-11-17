//! The test Users file, `vm.dev.json` (blackbox/CONTRACT.md, "The local
//! loop: The test Users file"): read and checked ([`read`]), and the
//! Builder's own Grant copied into it by a pull ([`TestUsersFile::copy_grant`]).
//!
//! The file is kept as it was read, every key in its order, keys the tool
//! does not know included ([`Json`]), so that what a pull writes back is the
//! file with only the Builder's User and Grant changed.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use super::Ending;
use crate::resources::is_resource_name;

/// The file's name in the project.
pub const FILE: &str = "vm.dev.json";

/// The largest file read; past it the file is refused.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// The User a pull adds, and the Group it is in.
pub const BUILDER: &str = "builder";

/// A JSON value with every object's keys in the order they were read.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Self::Object(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    fn text(text: &str) -> Self {
        Self::String(text.to_owned())
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        serde_json::Number::from_f64(v)
            .map(Json::Number)
            .ok_or_else(|| E::custom("a number JSON cannot hold"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Json, E> {
        Ok(Json::text(v))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut entries: Vec<(String, Json)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            // A key given twice: the last one counts, as JSON readers take it.
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
        }
        Ok(Json::Object(entries))
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(v) => serializer.serialize_bool(*v),
            Self::Number(v) => v.serialize(serializer),
            Self::String(v) => serializer.serialize_str(v),
            Self::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// A test User of the file.
pub struct TestUser {
    pub id: String,
    pub groups: Vec<String>,
}

/// A Grant of the file.
pub struct TestGrant {
    pub group: String,
    pub resource: String,
    /// `structure`, `read` or `write`.
    pub access: String,
}

/// The file, as read and checked.
pub struct TestUsersFile {
    /// The whole file, as it was read.
    pub whole: Json,
    pub users: Vec<TestUser>,
    pub grants: Vec<TestGrant>,
}

/// The file of `project`: `None` when it is not there, else the file as
/// read and checked, or `test-users-file` saying what is wrong where.
/// `command` is named in the message.
pub fn read(project: &Path, command: &str) -> Result<Option<TestUsersFile>, Ending> {
    let path = project.join(FILE);
    let opened = match File::open(&path) {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(wrong(&format!("cannot be read ({error})"), command));
        }
    };
    let mut bytes = Vec::new();
    opened
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| wrong(&format!("cannot be read ({error})"), command))?;
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        return Err(wrong(
            &format!("is larger than {} MiB", MAX_FILE_BYTES / (1024 * 1024)),
            command,
        ));
    }
    let whole: Json = serde_json::from_slice(&bytes)
        .map_err(|error| wrong(&format!("is not JSON ({error})"), command))?;
    checked(whole)
        .map(Some)
        .map_err(|what| wrong(&what, command))
}

/// The file's text as a new project has it, read (the template's).
pub fn as_new() -> Result<TestUsersFile, Ending> {
    let whole: Json = serde_json::from_str(super::template::NEW_TEST_USERS).map_err(|_| {
        Ending::new(
            1,
            "tool-failed",
            "the template's vm.dev.json is not JSON".to_owned(),
        )
    })?;
    checked(whole).map_err(|what| Ending::new(1, "tool-failed", what))
}

/// `test-users-file`, saying what is wrong.
fn wrong(what: &str, command: &str) -> Ending {
    Ending::new(
        1,
        "test-users-file",
        format!(
            "{FILE} {what}. It holds the project's test Users and their Grants: {{ \"users\": [ {{ \"id\", \"displayName\", \"email\", \"groups\": [...] }} ], \"grants\": [ {{ \"group\", \"resource\", \"access\": \"structure\" | \"read\" | \"write\" }} ] }}. Correct it, then run {command} again"
        ),
    )
}

/// Whether `text` is 1 to 256 bytes with no control character: a User's id
/// and a Group, as the API takes them.
fn is_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control)
}

/// The file checked: what is wrong where, else the Users and Grants.
fn checked(whole: Json) -> Result<TestUsersFile, String> {
    if !matches!(whole, Json::Object(_)) {
        return Err("is not a JSON object".to_owned());
    }
    let users = match whole.get("users") {
        None => return Err("has no `users`: it is required (a list, which may be empty)".into()),
        Some(Json::Array(users)) => users,
        Some(_) => return Err("has `users` that is not a list".into()),
    };
    let mut read_users = Vec::with_capacity(users.len());
    let mut ids = BTreeSet::new();
    for (index, user) in users.iter().enumerate() {
        let at = format!("users[{index}]");
        if !matches!(user, Json::Object(_)) {
            return Err(format!("has {at} that is not an object"));
        }
        let id = match user.get("id") {
            None => return Err(format!("has {at} without an `id`")),
            Some(Json::String(id)) if is_name(id) => id,
            Some(_) => {
                return Err(format!(
                    "has {at}.id that is not 1 to 256 bytes of text without control characters"
                ));
            }
        };
        if !ids.insert(id.as_str()) {
            return Err(format!("has two Users of the id {id}"));
        }
        let groups = match user.get("groups") {
            None => {
                return Err(format!(
                    "has {at} ({id}) without `groups`: it is required (a list, which may be empty)"
                ));
            }
            Some(Json::Array(groups)) => groups,
            Some(_) => return Err(format!("has {at}.groups ({id}) that is not a list")),
        };
        let mut read_groups = Vec::with_capacity(groups.len());
        for (g, group) in groups.iter().enumerate() {
            match group {
                Json::String(group) if is_name(group) => read_groups.push(group.clone()),
                _ => {
                    return Err(format!(
                        "has {at}.groups[{g}] ({id}) that is not a group: 1 to 256 bytes of text without control characters"
                    ));
                }
            }
        }
        for key in ["displayName", "email"] {
            if let Some(value) = user.get(key)
                && !matches!(value, Json::String(_))
            {
                return Err(format!("has {at}.{key} ({id}) that is not text"));
            }
        }
        read_users.push(TestUser {
            id: id.clone(),
            groups: read_groups,
        });
    }
    let grants = match whole.get("grants") {
        None => &Vec::new(),
        Some(Json::Array(grants)) => grants,
        Some(_) => return Err("has `grants` that is not a list".into()),
    };
    let mut read_grants = Vec::with_capacity(grants.len());
    let mut pairs = BTreeSet::new();
    for (index, grant) in grants.iter().enumerate() {
        let at = format!("grants[{index}]");
        if !matches!(grant, Json::Object(_)) {
            return Err(format!("has {at} that is not an object"));
        }
        let group = match grant.get("group") {
            None => return Err(format!("has {at} without a `group`")),
            Some(Json::String(group)) if is_name(group) => group,
            Some(_) => {
                return Err(format!(
                    "has {at}.group that is not a group: 1 to 256 bytes of text without control characters"
                ));
            }
        };
        let resource = match grant.get("resource") {
            None => return Err(format!("has {at} without a `resource`")),
            Some(Json::String(resource)) if is_resource_name(resource) => resource,
            Some(_) => {
                return Err(format!(
                    "has {at}.resource that is not a Resource's name: 1 to 63 lower-case letters, digits and underscores, starting with a letter"
                ));
            }
        };
        let access = match grant.get("access").and_then(Json::as_str) {
            Some(access @ ("structure" | "read" | "write")) => access,
            None if grant.get("access").is_none() => {
                return Err(format!("has {at} without an `access`"));
            }
            _ => {
                return Err(format!(
                    "has {at}.access that is not \"structure\", \"read\" or \"write\""
                ));
            }
        };
        if !pairs.insert((group.as_str(), resource.as_str())) {
            return Err(format!(
                "has two Grants of the group {group} on the Resource {resource}: a group has at most one Grant on a Resource"
            ));
        }
        read_grants.push(TestGrant {
            group: group.clone(),
            resource: resource.clone(),
            access: access.to_owned(),
        });
    }
    Ok(TestUsersFile {
        whole,
        users: read_users,
        grants: read_grants,
    })
}

impl TestUsersFile {
    /// Copies the Builder's own Grant on `resource`, at `access`, into the
    /// file: the User `builder` there (kept as it is, in the Group
    /// `builder`), and the Grant of the Group `builder` on `resource`
    /// replaced or added. Whether anything changed.
    pub fn copy_grant(&mut self, resource: &str, access: &str) -> bool {
        let Json::Object(entries) = &mut self.whole else {
            return false;
        };
        let mut changed = false;
        if !entries.iter().any(|(k, _)| k == "users") {
            entries.push(("users".to_owned(), Json::Array(Vec::new())));
            changed = true;
        }
        if !entries.iter().any(|(k, _)| k == "grants") {
            entries.push(("grants".to_owned(), Json::Array(Vec::new())));
            changed = true;
        }
        if let Some(Json::Array(users)) = self.whole.get_mut("users") {
            let existing = users
                .iter_mut()
                .find(|u| u.get("id").and_then(Json::as_str) == Some(BUILDER));
            match existing {
                Some(user) => {
                    if let Some(Json::Array(groups)) = user.get_mut("groups")
                        && !groups.iter().any(|g| g.as_str() == Some(BUILDER))
                    {
                        groups.push(Json::text(BUILDER));
                        changed = true;
                    }
                }
                None => {
                    users.push(Json::Object(vec![
                        ("id".to_owned(), Json::text(BUILDER)),
                        (
                            "displayName".to_owned(),
                            Json::text("The Builder (your own access)"),
                        ),
                        ("email".to_owned(), Json::text("builder@example.com")),
                        ("groups".to_owned(), Json::Array(vec![Json::text(BUILDER)])),
                    ]));
                    changed = true;
                }
            }
        }
        if let Some(Json::Array(grants)) = self.whole.get_mut("grants") {
            let existing = grants.iter_mut().find(|g| {
                g.get("group").and_then(Json::as_str) == Some(BUILDER)
                    && g.get("resource").and_then(Json::as_str) == Some(resource)
            });
            match existing {
                Some(grant) => {
                    if grant.get("access").and_then(Json::as_str) != Some(access)
                        && let Some(level) = grant.get_mut("access")
                    {
                        *level = Json::text(access);
                        changed = true;
                    }
                }
                None => {
                    grants.push(Json::Object(vec![
                        ("group".to_owned(), Json::text(BUILDER)),
                        ("resource".to_owned(), Json::text(resource)),
                        ("access".to_owned(), Json::text(access)),
                    ]));
                    changed = true;
                }
            }
        }
        changed
    }

    /// The file's text: two spaces a level, a line end at the end.
    pub fn text(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut text = serde_json::to_vec_pretty(&self.whole)?;
        text.push(b'\n');
        Ok(text)
    }

    /// The User of `id`.
    pub fn user(&self, id: &str) -> Option<&TestUser> {
        self.users.iter().find(|u| u.id == id)
    }
}
