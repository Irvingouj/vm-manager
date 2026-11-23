//! The instruction file of a new project says how Storage is used
//! (CONTRACT.md, "Storage: The instruction file"; ADR 0019, "Bodies: a
//! handle and a stream"). The test makes a project with `vibe new` and
//! reads its `AGENTS.md`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::project::{new_project, squashed};
use blackbox::tool::Machine;

#[test]
fn agents_md_says_a_files_body_is_a_stream_and_how_a_download_and_an_upload_are_written() {
    let machine = Machine::new();
    let dir = machine.root().join("storage-notes");
    new_project(&machine, "storage-notes", &dir);
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let flat = squashed(&text);
    for (words, why) in [
        (
            "ctx.user().storage(",
            "Storage is reached through `ctx.user().storage(name)`",
        ),
        ("ctx.user().db(", "beside `ctx.user().db(name)`"),
        (
            "kind: \"storage\"",
            "a Storage Resource is declared with `kind: \"storage\"`",
        ),
        ("vibe pull", "and pulled with `vibe pull`"),
        ("file.body", "a file's body is `file.body`, a stream"),
        ("text()", "a small file is read with `text()`"),
        ("json()", "or `json()`"),
        ("arrayBuffer()", "or `arrayBuffer()`"),
        (
            "new Response(file.body)",
            "a download is `new Response(file.body)`",
        ),
        (
            "put(key, request.body)",
            "an upload is `put(key, request.body)`",
        ),
        ("5 GiB", "a file is at most 5 GiB"),
        (
            "null",
            "`get` and `head` answer `null` when there is no file",
        ),
        (
            ".vibe/local/storage/",
            "in the local run files are in `.vibe/local/storage/<name>/`",
        ),
    ] {
        assert!(
            flat.contains(words),
            "expected AGENTS.md to have {words:?} ({why})\n{text}"
        );
    }
    assert!(
        flat.contains("stream"),
        "expected AGENTS.md to say that a file's body is a stream\n{text}"
    );
    assert!(
        text.lines().count() <= 120,
        "expected at most 120 lines, got {}",
        text.lines().count()
    );
}
