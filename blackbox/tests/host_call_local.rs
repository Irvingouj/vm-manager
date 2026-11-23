//! The bounds of a host call's arguments are the same in the local run
//! (CONTRACT.md, "A host call's arguments are bounded": "The bounds are the
//! same in the local run (`vibe dev`, `vibe request`)"): through `vibe
//! request`, a call of 8 MiB is taken (`unknown-operation`, the operation
//! being none the host has), one byte more is `{ error: { kind: "limit",
//! limit: "arguments" } }`, and so are 262,145 values; the refused calls
//! leave no operation in the run's output.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::host_call::CALL_BOUND;
use blackbox::local::{BUILDER, Board};
use serde_json::{Value, json};

/// A handler that makes one raw host call as the request asks and answers
/// what it got.
const HANDLER: &str = r#"import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

declare const env: { VM: { invoke(operation: string, args: unknown): Promise<unknown> } };

export default defineApp(manifest, {
  async fetch(request) {
    const url = new URL(request.url);
    const n = Number(url.searchParams.get("n") ?? "0");
    // `{"pad":"..."}` is 10 bytes of text and the pad's; `{ z: [0, ...] }` is
    // n + 3 values.
    const args = url.pathname === "/zeros" ? { z: new Array(n - 3).fill(0) } : { pad: "a".repeat(n - 10) };
    try {
      return Response.json({ answer: await env.VM.invoke("nonesuch", args) });
    } catch (e) {
      return Response.json({ threw: String((e && (e as Error).message) || e) });
    }
  },
});
"#;

fn answer_of(board: &Board, path: &str) -> (Value, usize) {
    let requested = board.request_ok(path, BUILDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    (requested.json(), requested.operations().len())
}

#[test]
fn vibe_request_answers_limit_arguments_for_a_call_one_byte_past_8_mib_and_for_262145_values() {
    let board = Board::start("read", "read");
    board.project.write("src/index.ts", HANDLER);
    let limit = json!({ "error": { "kind": "limit", "limit": "arguments" } });
    let (taken, _) = answer_of(&board, &format!("/pad?n={CALL_BOUND}"));
    assert_eq!(
        taken["answer"]["refused"]["refusal"]["reason"],
        json!("unknown-operation"),
        "expected arguments of exactly 8,388,608 bytes to be taken by the local run (then refused as an unknown operation), got {taken}"
    );
    let (over, operations) = answer_of(&board, &format!("/pad?n={}", CALL_BOUND + 1));
    assert_eq!(
        over["answer"], limit,
        "expected arguments of 8,388,609 bytes to be answered `limit` `arguments` in the local run, got {over}"
    );
    assert_eq!(operations, 0, "expected no operation for a refused call");
    let (taken, _) = answer_of(&board, &format!("/zeros?n={}", CALL_BOUND / 32));
    assert_eq!(
        taken["answer"]["refused"]["refusal"]["reason"],
        json!("unknown-operation"),
        "expected 262,144 values to be taken in the local run, got {taken}"
    );
    let (many, _) = answer_of(&board, &format!("/zeros?n={}", CALL_BOUND / 32 + 1));
    assert_eq!(
        many["answer"], limit,
        "expected 262,145 values to be answered `limit` `arguments` in the local run, got {many}"
    );
}
