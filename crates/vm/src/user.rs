//! The User a request belongs to.

use serde_json::{Value, json};

/// An employee signed in to an Application, with the Groups the identity
/// provider (for now: the stand-in sign-in) reports for them, in the order
/// they were first stated.
#[derive(Debug, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub groups: Vec<String>,
}

impl User {
    /// `{ id, groups }`, as Application code receives it.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "groups": self.groups })
    }
}
