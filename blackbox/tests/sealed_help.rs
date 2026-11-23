//! What the installation's key seals is said where an administrator reads
//! (CONTRACT.md, "Resources and Grants in full: At start" and "What changes in
//! earlier sections"): `vibe serve --help`, and `deploy/README.md`, which
//! also says that losing the key means supplying every Resource's logins
//! again.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

/// The blank-line separated paragraphs of `text` that name `VIBE_SECRET_KEY`,
/// in lower case, as one text.
fn about_the_key(text: &str) -> String {
    text.split("\n\n")
        .filter(|p| p.contains("VIBE_SECRET_KEY"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

#[test]
fn serve_help_says_the_key_seals_resources_logins_and_own_databases_passwords() {
    let mut command = std::process::Command::new(blackbox::program::vm_binary());
    command.args(["serve", "--help"]);
    let finished = blackbox::program::run_to_exit(command, Duration::from_secs(15))
        .unwrap_or_else(|problem| panic!("cannot run `vibe serve --help`: {problem}"));
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "expected `vibe serve --help` to end with 0"
    );
    let said = about_the_key(&finished.stdout);
    assert!(said.contains("vibe_secret_key"), "the help names the key");
    for word in ["seal", "login", "password", "own database"] {
        assert!(
            said.contains(word),
            "expected the paragraph of `vibe serve --help` that names VIBE_SECRET_KEY to say that it seals Resources' logins and own databases' passwords (the word {word:?}), got\n{said}"
        );
    }
}

#[test]
fn the_install_readme_says_what_the_key_seals_and_that_losing_it_means_supplying_every_resources_logins_again()
 {
    let path = blackbox::program::manifest_dir()
        .join("..")
        .join("deploy")
        .join("README.md");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let said = text
        .lines()
        .filter(|l| l.contains("VIBE_SECRET_KEY"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    for word in ["login", "password", "own database", "resource"] {
        assert!(
            said.contains(word),
            "expected deploy/README.md to say, where it names VIBE_SECRET_KEY, that it seals Resources' logins and own databases' passwords (the word {word:?}), got\n{said}"
        );
    }
    assert!(
        said.contains("lose") || said.contains("losing") || said.contains("lost"),
        "expected deploy/README.md to say that losing the key means supplying every Resource's logins again, got\n{said}"
    );
}
