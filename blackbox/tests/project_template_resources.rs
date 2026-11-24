//! The template's instruction file and the tickets on Resources in full
//! (CONTRACT.md, "Resources and Grants in full: The instruction file"):
//! where `AGENTS.md` speaks of pulling it says that `vibe pull` says how
//! many relations are `hidden` when none of the Resource's logins holds a
//! privilege on them, that code written against a hidden table will not
//! type-check, that nothing in the project is wrong, and that the agent
//! asks the Builder to ask the Resource's `contact` (named in the same
//! output) whether the logins should be given a privilege on the table;
//! where it speaks of `unavailable`, that a database Resource that is not
//! served because the installation has no key or the wrong one fails
//! `unavailable` and nothing in the project is wrong; and it names no `vibe
//! admin` command.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::project::{new_project, paragraphs, squashed, vm_commands_in};
use blackbox::tool::Machine;

fn agents_md(label: &str) -> String {
    let machine = Machine::new();
    let dir = machine.root().join(label);
    new_project(&machine, label, &dir);
    std::fs::read_to_string(dir.join("AGENTS.md")).unwrap()
}

#[test]
fn where_it_speaks_of_pulling_agents_md_says_what_hidden_means_and_whom_to_ask() {
    let text = agents_md("hidden-notes");
    let found = paragraphs(&text)
        .into_iter()
        .find(|p| p.contains("vibe pull") && p.contains("hidden"));
    let rule = found.unwrap_or_else(|| {
        panic!("expected a paragraph of AGENTS.md on `vibe pull` that names `hidden`\n{text}")
    });
    let flat = squashed(&rule);
    let lower = flat.to_lowercase();
    for (words, why) in [
        (
            "type-check",
            "that code written against a hidden table will not type-check",
        ),
        ("contact", "that the Resource's `contact` is whom to ask"),
        (
            "privilege",
            "that the question is whether the logins should be given a privilege",
        ),
        ("builder", "that the agent asks the Builder to ask"),
        ("wrong", "that nothing in the project is wrong"),
    ] {
        assert!(
            lower.contains(words),
            "expected the paragraph to say {words:?} ({why}), got\n{rule}"
        );
    }
    assert!(
        lower.contains("nothing in the project is wrong")
            || lower.contains("not wrong")
            || lower.contains("nothing is wrong"),
        "expected the paragraph to say that nothing in the project is wrong, got\n{rule}"
    );
    assert!(
        lower.contains("table"),
        "expected the paragraph to speak of the table, got\n{rule}"
    );
}

#[test]
fn where_it_speaks_of_unavailable_agents_md_says_a_resource_without_its_key_is_nothing_to_fix() {
    let text = agents_md("key-notes");
    let found = paragraphs(&text).into_iter().find(|p| {
        let lower = squashed(p).to_lowercase();
        lower.contains("unavailable") && lower.contains("key")
    });
    let rule = found.unwrap_or_else(|| {
        panic!("expected a paragraph of AGENTS.md saying that a database Resource that is not served because the installation has no key or the wrong one fails `unavailable`\n{text}")
    });
    let lower = squashed(&rule).to_lowercase();
    for (words, why) in [
        ("database", "that it is a database Resource"),
        (
            "wrong",
            "that nothing in the project is wrong, or the wrong key",
        ),
        ("nothing", "that nothing in the project is wrong"),
    ] {
        assert!(
            lower.contains(words),
            "expected the paragraph to say {words:?} ({why}), got\n{rule}"
        );
    }
}

#[test]
fn agents_md_names_no_administrators_command_and_stays_short() {
    let text = agents_md("no-admin-notes");
    let commands = vm_commands_in(&text);
    assert!(
        !commands.iter().any(|c| c == "admin"),
        "expected AGENTS.md to name no `vibe admin` command, it names {commands:?}\n{text}"
    );
    for word in [
        "resource register",
        "resource remove",
        "resource location",
        "resource logins",
    ] {
        assert!(
            !text.contains(word),
            "expected AGENTS.md to name no command of the administrators ({word:?})"
        );
    }
    assert!(
        text.lines().count() <= 120,
        "expected AGENTS.md at most 120 lines, got {}",
        text.lines().count()
    );
}
