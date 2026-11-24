//! `deploy/README.md` and the tickets on Resources in full (CONTRACT.md,
//! "Resources and Grants in full: What changes in earlier sections",
//! "Installing"): it says how to register a Resource with the tool (`vibe
//! admin resource register`), that passwords are given in the environment,
//! that a Resource's logins are checked when they are supplied, that a pull
//! lists the tables the read or write login holds a privilege on, and (from
//! the stage before) that `VIBE_SECRET_KEY` seals Resources' logins and that
//! losing it means supplying every Resource's logins again.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::read_repository_file;

fn readme() -> String {
    read_repository_file("deploy/README.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The sentences (cut at `. `) that hold all of these words, lower-cased.
fn sentences_with(text: &str, words: &[&str]) -> Vec<String> {
    text.split(". ")
        .map(str::to_lowercase)
        .filter(|s| words.iter().all(|w| s.contains(&w.to_lowercase())))
        .collect()
}

#[test]
fn the_readme_says_how_to_register_a_resource_with_the_tool() {
    let text = readme();
    assert!(
        text.contains("vibe admin resource register"),
        "expected deploy/README.md to name `vibe admin resource register`"
    );
}

#[test]
fn the_readme_says_passwords_are_given_in_the_environment() {
    let text = readme();
    for variable in [
        "VIBE_RESOURCE_READ_PASSWORD",
        "VIBE_RESOURCE_WRITE_PASSWORD",
        "VIBE_RESOURCE_STRUCTURE_PASSWORD",
    ] {
        assert!(
            text.contains(variable),
            "expected deploy/README.md to name {variable}"
        );
    }
    assert!(
        !sentences_with(&text, &["password", "environment"]).is_empty(),
        "expected a sentence of deploy/README.md saying that passwords are given in the environment"
    );
}

#[test]
fn the_readme_says_a_resources_logins_are_checked_when_they_are_supplied() {
    let text = readme();
    assert!(
        !sentences_with(&text, &["logins", "checked"]).is_empty()
            || !sentences_with(&text, &["login", "checked"]).is_empty(),
        "expected a sentence of deploy/README.md saying that a Resource's logins are checked when they are supplied"
    );
}

#[test]
fn the_readme_says_a_pull_lists_the_tables_the_read_or_write_login_holds_a_privilege_on() {
    let text = readme();
    let found = sentences_with(&text, &["pull", "privilege"]);
    assert!(
        !found.is_empty(),
        "expected a sentence of deploy/README.md saying that a pull lists the tables the read or write login holds a privilege on"
    );
    assert!(
        found
            .iter()
            .any(|s| s.contains("read") && s.contains("write")),
        "expected the sentence to name the read and the write login, got {found:?}"
    );
}

#[test]
fn the_readme_still_says_what_the_key_seals_and_what_losing_it_means() {
    let text = readme();
    assert!(text.contains("VIBE_SECRET_KEY"));
    assert!(
        !sentences_with(&text, &["seals", "logins"]).is_empty(),
        "expected deploy/README.md to say that `VIBE_SECRET_KEY` seals Resources' logins"
    );
    assert!(
        !sentences_with(&text, &["losing", "logins", "again"]).is_empty(),
        "expected deploy/README.md to say that losing the key means supplying every Resource's logins again"
    );
}

// Stage three ("Resources and Grants in full: What changes in earlier
// sections").

#[test]
fn the_readme_says_a_group_is_given_access_with_vibe_admin_grant_or_on_the_dashboards_page_of_the_resource()
 {
    let text = readme();
    // `vibe admin grant`, and not only `vibe admin grants`.
    let names_grant = text
        .match_indices("vibe admin grant")
        .any(|(at, found)| text.as_bytes().get(at + found.len()) != Some(&b's'));
    assert!(
        names_grant,
        "expected deploy/README.md to name `vibe admin grant` (not only `vibe admin grants`)"
    );
    let found = sentences_with(&text, &["vibe admin grant", "group", "dashboard"]);
    assert!(
        found
            .iter()
            .any(|s| s.contains("page") && s.contains("resource")),
        "expected a sentence of deploy/README.md saying that a Group is given access with `vibe admin grant` or on the Dashboard's page of the Resource, got {found:?}"
    );
}

#[test]
fn the_readme_says_vibe_admin_revoke_takes_access_away_at_once_in_every_application() {
    let text = readme();
    let found = sentences_with(
        &text,
        &["vibe admin revoke", "at once", "every application"],
    );
    assert!(
        !found.is_empty(),
        "expected a sentence of deploy/README.md saying that `vibe admin revoke` takes access away at once in every Application"
    );
}

#[test]
fn the_readme_says_vibe_admin_grants_lists_who_can_reach_what() {
    let text = readme();
    let found = sentences_with(&text, &["vibe admin grants", "who can reach"]);
    assert!(
        !found.is_empty(),
        "expected a sentence of deploy/README.md saying that `vibe admin grants` lists who can reach what"
    );
}
