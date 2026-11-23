//! The files of `deploy/` as the Storage ticket changes them, read from
//! the repository in the daily gate (no Docker needed: what Docker shows
//! of them is in the install tests, `blackbox/tests/install_storage.rs`,
//! which the gate does not run).
//!
//! CONTRACT.md, "Storage: Installing" (ticket "Storage: a shared, named
//! file Resource, on a mounted directory now and S3 later"):
//!
//! - `deploy/compose.yaml` gains one value and one mount: `VIBE_STORAGE_DIR`
//!   (the host's folder for Storage; `./storage` without it) is mounted
//!   **read-write** at `/var/lib/vibe/storage`, and `vibe` is started with
//!   `--storage-root /var/lib/vibe/storage`;
//! - `deploy/README.md` says, with every command written out: make the
//!   folder and give it to the image's user (`mkdir -p storage && sudo
//!   chown 65532:65532 storage`), make one folder in it for each Storage
//!   Resource (`sudo -u '#65532' mkdir storage/marketing`, or `mkdir` and
//!   `chown`), register it as `/var/lib/vibe/storage/<folder>`, and back
//!   the folder up with the host's own tools;
//! - `deploy/.env.example` lists `VIBE_STORAGE_DIR`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::read_repository_file;
use yaml_rust2::{Yaml, YamlLoader};

/// `deploy/compose.yaml`, parsed as YAML, its one document.
fn compose() -> Yaml {
    let text = read_repository_file("deploy/compose.yaml");
    let docs = YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("expected deploy/compose.yaml to be YAML: {e}"));
    docs.into_iter().next().unwrap_or_else(|| {
        panic!("expected deploy/compose.yaml to hold a document");
    })
}

/// The `vibe` service of the parsed file.
fn vibe(compose: &Yaml) -> &Yaml {
    &compose["services"]["vibe"]
}

/// The words of the service's `entrypoint` and `command`, as written.
fn command_of(service: &Yaml) -> Vec<String> {
    let words = |value: &Yaml| -> Vec<String> {
        value
            .as_vec()
            .map(|all| {
                all.iter()
                    .map(|w| {
                        w.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| panic!("expected a word of a command, got {w:?}"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut all = words(&service["entrypoint"]);
    all.extend(words(&service["command"]));
    all
}

/// The bind of `vibe`'s volumes that targets `target`, as written: `((the
/// source, whether it is read-only))`. A volume is taken in either of
/// Compose's two shapes, the long one (`type`, `source`, `target`,
/// `read_only`) and the short one (`<source>:<target>`, read-only only
/// with `:ro`): neither shape is forced on the file.
fn bind_at(compose: &Yaml, target: &str) -> Option<(String, bool)> {
    let field = |entry: &Yaml, name: &str| -> Yaml {
        entry
            .as_hash()
            .and_then(|hash| hash.get(&Yaml::String(name.to_owned())).cloned())
            .unwrap_or(Yaml::BadValue)
    };
    for volume in vibe(compose)["volumes"]
        .as_vec()
        .cloned()
        .unwrap_or_default()
    {
        match volume {
            Yaml::Hash(_) if field(&volume, "target").as_str() == Some(target) => {
                return Some((
                    field(&volume, "source")
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    field(&volume, "read_only").as_bool().unwrap_or(false),
                ));
            }
            Yaml::String(text) if text.contains(&format!(":{target}")) => {
                let read_only = text.contains(":ro");
                return Some((text, read_only));
            }
            _ => {}
        }
    }
    None
}

#[test]
fn compose_gains_the_storage_value_mount_and_storage_root_argument() {
    let compose = compose();
    // `vibe` is started with `--storage-root /var/lib/vibe/storage`.
    let words = command_of(vibe(&compose));
    assert!(
        words
            .windows(2)
            .any(|w| w[0] == "--storage-root" && w[1] == "/var/lib/vibe/storage"),
        "expected `--storage-root /var/lib/vibe/storage` in vibe's command {words:?} (CONTRACT.md, \"Storage: Installing\")"
    );
    // `VIBE_STORAGE_DIR` (the host's folder for Storage; `./storage`
    // without it) is mounted read-write at `/var/lib/vibe/storage`.
    let Some((source, read_only)) = bind_at(&compose, "/var/lib/vibe/storage") else {
        panic!(
            "expected a volume of vibe's mounted at /var/lib/vibe/storage, got {:?}",
            vibe(&compose)["volumes"]
        );
    };
    assert!(
        !read_only,
        "expected the storage mount read-write (CONTRACT.md, \"Storage: Installing\"), got the bind {source:?}"
    );
    assert!(
        source.contains("VIBE_STORAGE_DIR") && source.contains("./storage"),
        "expected the source of the storage bind to name the value VIBE_STORAGE_DIR with its default ./storage (CONTRACT.md, \"Storage: Installing\"), got {source:?}"
    );
    // The `ca/` mount stays as it was, read-only.
    let (ca, read_only) = bind_at(&compose, "/etc/vibe/ca").unwrap_or_else(|| {
        panic!(
            "expected the ca/ bind still there, read-only at /etc/vibe/ca, got {:?}",
            vibe(&compose)["volumes"]
        )
    });
    assert!(
        read_only,
        "expected /etc/vibe/ca still read-only, got the bind {ca:?}"
    );
}

#[test]
fn the_env_example_lists_the_storage_value() {
    let example = read_repository_file("deploy/.env.example");
    assert!(
        example.contains("VIBE_STORAGE_DIR"),
        "expected deploy/.env.example to list VIBE_STORAGE_DIR (CONTRACT.md, \"Storage: Installing\")"
    );
}

/// `text` with every run of white space one space.
fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The at most `chars` characters of `text` that follow byte offset `at`
/// (a character boundary, as `find` gives it): the window to search for a
/// phrase near another, without slicing into a character.
fn window_after(text: &str, at: usize, chars: usize) -> String {
    text.get(at..)
        .unwrap_or_default()
        .chars()
        .take(chars)
        .collect()
}

/// The sentences of `text` (squashed), each lower-cased: its runs between
/// a `.`, `!` or `?` that ends one. A period inside a word (a file name,
/// an abbreviation) ends nothing: the next non-space character must begin
/// a sentence, with a capital letter or markdown punctuation.
fn sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut from = 0;
    for (at, c) in text.char_indices() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let begins = text
            .get(at + c.len_utf8()..)
            .unwrap_or_default()
            .chars()
            .find(|c| !c.is_whitespace())
            .is_some_and(|next| next.is_uppercase() || matches!(next, '-' | '#' | '*' | '`'));
        if begins {
            if let Some(sentence) = text.get(from..at) {
                sentences.push(sentence.to_lowercase());
            }
            from = at + c.len_utf8();
        }
    }
    if let Some(sentence) = text.get(from..) {
        sentences.push(sentence.to_lowercase());
    }
    sentences
}

/// How many characters of `text` are before byte offset `at` (a character
/// boundary).
fn chars_before(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |before| before.chars().count())
}

/// The words of a squashed, lower-cased sentence, with the character
/// offset each begins at: a word is a run of letters or digits.
fn words(text: &str) -> Vec<(&str, usize)> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(at);
        } else if let Some(from) = start.take() {
            words.push((
                text.get(from..at).unwrap_or_default(),
                chars_before(text, from),
            ));
        }
    }
    if let Some(from) = start {
        words.push((
            text.get(from..).unwrap_or_default(),
            chars_before(text, from),
        ));
    }
    words
}

/// Whether this sentence (squashed, lower-cased) says to back the folder
/// up with the host's own tools, in any wording: a form of "back ... up"
/// or "backup", the word "folder" and the word "tool", each within 200
/// characters of the others. The contract fixes the meaning and not the
/// words it is said in.
fn says_to_back_the_folder_up_with_tools(sentence: &str) -> bool {
    let words = words(sentence);
    let first_word_at = |prefix: &str| {
        words
            .iter()
            .find(|(word, _)| word.starts_with(prefix))
            .map(|(_, at)| *at)
    };
    let (Some(folder), Some(tool)) = (first_word_at("folder"), first_word_at("tool")) else {
        return false;
    };
    let back = words.iter().enumerate().find_map(|(index, (word, at))| {
        if word.starts_with("backup") {
            return Some(*at);
        }
        if !matches!(*word, "back" | "backs" | "backed" | "backing") {
            return None;
        }
        words
            .iter()
            .skip(index + 1)
            .take(4)
            .any(|(word, _)| *word == "up")
            .then_some(*at)
    });
    let Some(back) = back else {
        return false;
    };
    let span = back.max(folder).max(tool) - back.min(folder).min(tool);
    span <= 200
}

#[test]
fn the_readme_writes_out_the_storage_commands() {
    let readme = squashed(&read_repository_file("deploy/README.md"));
    for command in [
        "mkdir -p storage && sudo chown 65532:65532 storage",
        "sudo -u '#65532' mkdir storage/marketing",
    ] {
        assert!(
            readme.contains(command),
            "expected deploy/README.md to write out {command:?} (CONTRACT.md, \"Storage: Installing\")"
        );
    }
    // Register it as /var/lib/vibe/storage/<folder>, with the folder the
    // document's own example names.
    assert!(
        readme.contains("/var/lib/vibe/storage/marketing"),
        "expected deploy/README.md to say to register the folder as /var/lib/vibe/storage/<folder>, with marketing as the example"
    );
    // The other way of making the folder: `mkdir` and `chown`.
    let at = readme
        .find("sudo -u '#65532' mkdir storage/marketing")
        .unwrap_or_else(|| panic!("unreachable: the command was found above"));
    assert!(
        window_after(&readme, at, 400)
            .to_lowercase()
            .contains("chown"),
        "expected deploy/README.md to give the other way of making the folder (`mkdir` and `chown`) beside `sudo -u '#65532' mkdir storage/marketing`"
    );
    // Back the folder up with the host's own tools (CONTRACT.md,
    // "Storage: Installing"): any sentence of the README that says so, in
    // any wording, since the contract fixes the meaning and not the words.
    assert!(
        sentences(&readme)
            .iter()
            .any(|sentence| says_to_back_the_folder_up_with_tools(sentence)),
        "expected deploy/README.md to say to back the folder up with the host's own tools (CONTRACT.md, \"Storage: Installing\")"
    );
}
