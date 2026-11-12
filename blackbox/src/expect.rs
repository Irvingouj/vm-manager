//! Checks on what Application code reports, shared by the tests. Each fails
//! with a message saying what was expected and what came.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::http::User;

/// The Groups in a `{ id, groups }` value, as a set, or `None` when
/// `groups` is not an array of strings. Fails if a Group appears twice.
fn groups_of(user: &Value, context: &str) -> Option<BTreeSet<String>> {
    let list = user["groups"].as_array()?;
    let mut set = BTreeSet::new();
    for group in list {
        let group = group.as_str()?;
        assert!(
            set.insert(group.to_owned()),
            "{context}: Group {group:?} appears twice in {user}"
        );
    }
    Some(set)
}

/// Whether `value` is exactly this User: their id and their Groups (in any
/// order).
pub fn is_user(value: &Value, user: &User) -> bool {
    let want: BTreeSet<String> = user.groups.iter().cloned().collect();
    value["id"].as_str() == Some(user.id.as_str())
        && groups_of(value, "a User").is_some_and(|got| got == want)
}

/// Fails unless `value` is `{ id, groups }` of exactly this User.
pub fn assert_user(value: &Value, user: &User, context: &str) {
    assert!(
        is_user(value, user),
        "{context}: expected the User {:?} with Groups {:?}, got {value}",
        user.id,
        user.groups
    );
}

/// Fails unless `answer` is `whoami`'s
/// `{ ok: { user: { id, groups }, application, request } }` for this User,
/// Application and request.
pub fn assert_whoami(answer: &Value, user: &User, application: &str, request: &str, context: &str) {
    let ok = &answer["ok"];
    assert!(
        ok.is_object(),
        "{context}: expected whoami to answer {{ ok: ... }}, got {answer}"
    );
    assert_user(&ok["user"], user, &format!("{context}, whoami's user"));
    assert_eq!(
        ok["application"].as_str(),
        Some(application),
        "{context}: expected whoami to name the Application {application:?}, got {answer}"
    );
    assert_eq!(
        ok["request"].as_str(),
        Some(request),
        "{context}: expected whoami to name the request {request:?} (its x-vm-request-id), got {answer}"
    );
}

/// For a reading that may legitimately fail (a refusal, a throw) but must
/// never name someone else: if `answer` holds an allowed `whoami` answer,
/// it is exactly this User, Application and request.
pub fn assert_whoami_names_no_one_else(
    answer: &Value,
    user: &User,
    application: &str,
    request: &str,
    context: &str,
) {
    if answer.get("ok").is_some() {
        assert_whoami(answer, user, application, request, context);
    }
}

/// The reason of a refusal `{ refused: { refusal: { reason }, fix } }`.
pub fn refusal_reason(answer: &Value) -> Option<&str> {
    answer["refused"]["refusal"]["reason"].as_str()
}

/// Fails unless `answer` is a refusal with this reason and a `fix` string.
pub fn assert_refused(answer: &Value, reason: &str, context: &str) {
    assert_eq!(
        refusal_reason(answer),
        Some(reason),
        "{context}: expected a refusal with reason {reason:?}, got {answer}"
    );
    assert!(
        answer["refused"]["fix"].is_string(),
        "{context}: expected the refusal to carry a `fix` string, got {answer}"
    );
    assert!(
        answer.get("ok").is_none(),
        "{context}: a refusal must not also carry `ok`, got {answer}"
    );
}
