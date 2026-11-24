//! The instruction file of a new project says what an Application's share
//! is (CONTRACT.md, "Resources and Grants in full: The instruction file",
//! stage four): all its requests together have at most 128 file bodies open
//! and 64 outbound calls under way at once and at most 64 MiB in the
//! arguments of calls not yet answered; an operation past that waits about
//! 5 seconds and fails `unavailable`; the cure is in the code: read a body
//! to its end or `cancel()` it, and do not keep bodies or answers open
//! longer than the work needs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::project::{new_project, paragraphs, squashed};
use blackbox::tool::Machine;

#[test]
fn agents_md_has_a_paragraph_on_an_applications_share_of_bodies_calls_and_arguments() {
    let machine = Machine::new();
    let dir = machine.root().join("share-notes");
    new_project(&machine, "share-notes", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let rule = paragraphs(&text)
        .into_iter()
        .find(|p| p.contains("128") && p.contains("64"))
        .unwrap_or_else(|| {
            panic!(
                "expected a paragraph that names the 128 file bodies and the 64 calls of an Application\n{text}"
            )
        });
    let flat = squashed(&rule);
    let lower = flat.to_lowercase();
    for (words, why) in [
        ("128", "128 file bodies open"),
        ("64 outbound calls", "64 outbound calls under way"),
        (
            "64 MiB",
            "64 MiB in the arguments of calls not yet answered",
        ),
        ("5 seconds", "the wait of about 5 seconds"),
        ("unavailable", "that the operation then fails `unavailable`"),
        ("cancel()", "that a body is read to its end or cancelled"),
    ] {
        assert!(
            flat.contains(words),
            "expected the paragraph to have {words:?} ({why}), got\n{rule}"
        );
    }
    for (words, why) in [
        ("application", "that the share is an Application's"),
        ("file bod", "that it is about file bodies"),
        ("at once", "that the numbers are for what is open at once"),
        ("requests", "that all its requests count together"),
        ("end", "that a body is read to its end"),
    ] {
        assert!(
            lower.contains(words),
            "expected the paragraph to say {words:?} ({why}), got\n{rule}"
        );
    }
    assert!(
        lower.contains("together") || lower.contains("all its requests"),
        "expected the paragraph to say that an Application's requests count together, got\n{rule}"
    );
}
