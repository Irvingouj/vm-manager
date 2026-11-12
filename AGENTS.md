# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure. Start with `docs/vision.md`.

Milestone 1 is being built. Its spec is the issue "Spec: Milestone 1"; the work is the open issues labelled `ready-for-agent`, each blocked by the ones it lists. Take the first one with no open blocker.

## Agent skills

### Issue tracker

Issues live in this repo's GitHub Issues, driven with the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default triage labels, unchanged. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` glossary and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## Conventions for this effort

- Use the vocabulary in `CONTEXT.md` in every issue, ADR and document. Refer to map tickets by their title, not by number alone.
- Hard-to-reverse decisions are recorded as ADRs in `docs/adr/`. Research summaries go in `docs/research/`, citing primary sources and marking each claim as verified, read from source, or not verified.
- Written artifacts are in English.

## How code gets written

### Black-box tests first, written by a different agent

Every ticket is built by two agents in turn, never by one.

1. **The test agent** writes tests from the ticket's acceptance criteria against the program from outside. It does not read the implementation.
2. **The implementation agent** makes those tests pass. It does not edit them. If it believes a test is wrong, it says so and the driving session decides.

- Black-box tests live in the `blackbox` crate. That crate depends on no product crate, so it cannot reach inside: it starts the real `vm` binary beside a real Postgres, drives it with commands and HTTP as different Users, and asserts on what an outsider can observe (a response, a command's output and exit code, an Operation log entry, a Change history row, a refusal's reason). Fixture Applications, hostile ones included, live there too.
- The outside interface the tests assume (start-up options, commands, addresses, what an Application is handed) is written down in `blackbox/CONTRACT.md` and changed there first.
- Tests below that seam are allowed only where the spec's Testing Decisions name one (the Authorization decision as a pure function, the Request runner with a scripted Application, the SDK's type tests). The test agent still writes them first.
- A ticket is done when its tests were seen failing before the implementation and passing after, and the gate is clean.

### The gate

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Rust rules

One Cargo workspace at the repository root. Lints are set once in the workspace and inherited by every crate; Clippy enforces what it can, review enforces the rest. Vendored code under `vendor/` is exempt and is not ours to restyle.

1. **Never `unsafe`.** It is forbidden in our crates. Something that needs it goes behind a safe interface in the vendored runtime or a dependency.
2. **Minimize `clone`.** Borrow first. Share with `Arc` and write `Arc::clone(&x)` so a cheap clone reads differently from a copy. Copying owned data on a request's path needs a comment saying why.
3. **Never an unbounded channel**, or any unbounded queue, buffer or pool. Each has a stated bound and a stated behaviour when full. The unbounded constructors are disallowed in `clippy.toml`.
4. **No panic on the serving path.** Outside tests: no `unwrap`, `expect`, `panic!`, `todo!`, indexing or string slicing that can panic. Errors are values.
5. **No lock held across an `await`**, and no blocking call on an async worker thread.
6. **Every spawned task has an owner** that holds its handle and ends it. No detached `spawn`.
7. **Every wait has a timeout**: on the network, on the database, on Application code.
8. **No `as` cast that can truncate or change sign.** Use `try_from`.
9. **A credential or secret is a type that does not print**: no `Debug` or `Display` that shows the value, and it never enters a log or an error message.
10. **No `#[allow]` without a reason.** Use `#[expect(..., reason = "...")]`.

Rules 1 to 3 are the product owner's; 4 to 10 were added by the agent in the same spirit and are open to change.
