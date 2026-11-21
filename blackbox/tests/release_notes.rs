//! The release's notes (CONTRACT.md, "The first release: The page and the
//! notes"): `docs/releases/v<version>.md` (now v0.1.3) says what works; what is missing;
//! that the release is **not for untrusted Applications or real company
//! data** until the work on credentials, limits and the runtime's
//! hardening is done; that the Postgres a local run downloads is a third
//! party's build pinned by its checksum; which systems the tool is built
//! for; and that the image is for `linux/amd64` only. Held to the words the
//! contract makes bold and to the facts, not to a whole wording.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::{TARGETS, VERSION, read_repository_file};

/// The notes, lowercase, with Markdown's emphasis and code marks taken off
/// and every run of blanks one space.
fn notes() -> String {
    let text = read_repository_file(&format!("docs/releases/v{VERSION}.md"));
    let plain: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`'))
        .collect::<String>()
        .to_lowercase();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_notes_say_the_release_is_not_for_untrusted_applications_or_real_company_data_until_the_hardening_is_done()
 {
    let notes = notes();
    assert!(
        notes.contains("not for untrusted applications or real company data"),
        "expected the notes to say the release is not for untrusted Applications or real company data"
    );
    for work in ["credentials", "limits", "hardening"] {
        assert!(
            notes.contains(work),
            "expected the notes to name the work on {work} that must be done first"
        );
    }
}

#[test]
fn the_notes_say_what_works_and_what_is_missing() {
    let notes = notes();
    for word in ["works", "missing"] {
        assert!(
            notes.contains(word),
            "expected the notes to say what {word}"
        );
    }
}

#[test]
fn the_notes_say_the_image_is_for_linux_amd64_only() {
    let notes = notes();
    assert!(
        notes.contains("linux/amd64"),
        "expected the notes to say the image is for linux/amd64 only"
    );
}

#[test]
fn the_notes_name_the_two_systems_the_tool_is_built_for() {
    let notes = notes();
    for target in TARGETS {
        assert!(
            notes.contains(target),
            "expected the notes to name the target {target} the tool is built for"
        );
    }
}

#[test]
fn the_notes_say_the_postgres_a_local_run_downloads_is_a_third_partys_build_pinned_by_its_checksum()
{
    let notes = notes();
    assert!(
        notes.contains("postgres") && notes.contains("checksum"),
        "expected the notes to say the Postgres a local run downloads is a third party's build, pinned by its checksum"
    );
}
