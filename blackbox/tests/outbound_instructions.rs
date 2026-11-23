//! The instruction file of a new project says how outbound calls work
//! (CONTRACT.md, "Outbound calls: The instruction file"; ticket "Outbound
//! calls"): in one short paragraph, that `fetch` to the outside is allowed
//! only to hosts the Platform administrators listed, over `https`; that
//! `vibe outbound` shows them; that a call elsewhere rejects with `Refused`
//! and the reason `host-not-listed`, which the agent reports to the Builder
//! rather than works around; and how to allow a host for the local run
//! (`outbound.hosts` in `vibe.dev.json`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::project::{new_project, paragraphs, squashed, vm_commands_in};
use blackbox::tool::Machine;

#[test]
fn agents_md_has_one_short_paragraph_on_outbound_calls() {
    let machine = Machine::new();
    let dir = machine.root().join("outbound-notes");
    new_project(&machine, "outbound-notes", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let rule = paragraphs(&text)
        .into_iter()
        .find(|p| p.contains("host-not-listed"))
        .unwrap_or_else(|| {
            panic!("expected a paragraph that names the reason `host-not-listed`\n{text}")
        });
    let flat = squashed(&rule);
    let lower = flat.to_lowercase();
    for (words, why) in [
        ("fetch", "that `fetch` to the outside is what it is about"),
        ("https", "that only `https` is allowed"),
        ("vibe outbound", "that `vibe outbound` shows the hosts"),
        ("Refused", "that the rejection is a `Refused`"),
        ("host-not-listed", "the reason"),
        ("outbound.hosts", "how to allow a host for the local run"),
        ("vibe.dev.json", "in which file"),
    ] {
        assert!(
            flat.contains(words),
            "expected the paragraph to have {words:?} ({why}), got\n{rule}"
        );
    }
    for (words, why) in [
        (
            "platform administrator",
            "that the Platform administrators list the hosts",
        ),
        ("listed", "that only listed hosts are allowed"),
        ("builder", "that the agent tells the Builder"),
        ("report", "that the agent reports it"),
        ("around", "that the agent does not work around it"),
    ] {
        assert!(
            lower.contains(words),
            "expected the paragraph to say {words:?} ({why}), got\n{rule}"
        );
    }
    assert!(
        rule.lines().count() <= 14,
        "expected one short paragraph (at most 14 lines), got {} lines\n{rule}",
        rule.lines().count()
    );
}

#[test]
fn agents_md_names_the_command_a_coding_agent_may_run_and_no_administrators_command() {
    let machine = Machine::new();
    let dir = machine.root().join("outbound-commands");
    new_project(&machine, "outbound-commands", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let commands = vm_commands_in(&text);
    assert!(
        commands.iter().any(|c| c == "outbound"),
        "expected AGENTS.md to name `vibe outbound` in code, it names {commands:?}\n{text}"
    );
    // The agent reports a refusal; it does not run an administrator's
    // command (the instruction file of "What Platform administrators see
    // and stop" names no `vibe admin` command).
    assert!(
        !commands.iter().any(|c| c == "admin"),
        "expected AGENTS.md to name no `vibe admin` command, it names {commands:?}\n{text}"
    );
}
