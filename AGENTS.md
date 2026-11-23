# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure. Start with `docs/vision.md`.

Milestone 1 is being built. Its spec is the issue "Spec: Milestone 1"; the work is the open issues labelled `ready-for-agent`, each blocked by the ones it lists. Take the first one with no open blocker.

## Agent skills

### Issue tracker

Issues live in this repo's GitHub Issues, driven with the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default triage labels, unchanged, and one of our own, `product-decision`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` glossary and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## Conventions for this effort

- Use the vocabulary in `CONTEXT.md` in every issue, ADR and document. Refer to map tickets by their title, not by number alone.
- Hard-to-reverse decisions are recorded as ADRs in `docs/adr/`. Research summaries go in `docs/research/`, citing primary sources and marking each claim as verified, read from source, or not verified.
- Written artifacts are in English.

## How code gets written

### The loop for every ticket

The driving session orchestrates; one agent works at a time.

1. **Spec.** The driving session writes the outside interface the ticket needs into `blackbox/CONTRACT.md`, from the ticket's acceptance criteria.
2. **Red tests.** A test agent writes black-box tests from the contract and the acceptance criteria. It does not read the implementation. The tests are seen failing.
3. **Implementation.** A different agent makes them pass. It does not edit the tests or the contract; if it believes one is wrong, it says so and the driving session decides.
4. **Review.** The driving session reads the diff against the Rust rules below and the ticket.
5. **Fix what the review found.** Everything: a defect, a gap in what the ticket promises, a way round a security claim, a path with no test, a cost nobody measured. Each goes back through the loop (the contract if the outside changes, a red test, the implementation). A ticket is never closed over a list of things left open.
6. **Green.** The driving session runs the checks itself: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the test files that touch what the ticket changed (its red tests and their neighbours). If one fails, back to the implementation agent until it is green. **The whole gate is not run for a ticket or before a push** until the product owner asks for it (the product owner's decision of 2026-11-21: nobody uses the product yet, the tests pin today's behaviour, and a full run before every push only slows the work).

Then the ticket's commits are pushed and the issue is closed with what was run. A finding that cannot be fixed inside the ticket, because only the product owner can decide it, is filed at once as an issue labelled `product-decision` ("Decisions for the product owner", below) and the ticket stays open until it is answered.

- Black-box tests live in the `blackbox` crate. That crate depends on no product crate, so it cannot reach inside: it starts the real `vibe` binary beside a real Postgres, drives it with commands and HTTP as different Users, and asserts on what an outsider can observe (a response, a command's output and exit code, an Operation log entry, a Change history row, a refusal's reason). Fixture Applications, hostile ones included, live there too.
- Tests below that seam are allowed only where the spec's Testing Decisions name one (the Authorization decision as a pure function, the Request runner with a scripted Application, the SDK's type tests). The test agent still writes them first.

### Decisions for the product owner

Whatever needs the product owner's decision is an issue labelled `product-decision`, and nowhere else: not a list in a status message, not a note at the end of a ticket (the product owner's decision of 2026-11-23: they pick decisions up from the tracker when they have time).

- **Only product decisions are asked**: what the product does and for whom, what is in a milestone, the order of the work, when a release is made. A technical choice (a bound, a setting, a dependency, how something is built) is the agent's: it is made, and recorded as "chosen by the agent" in the ADR, the contract or the ticket.
- **The issue stands on its own.** It says what the question is in one sentence, the background somebody needs who has not followed the work, the options and what each means for the people who use the product, the default the work goes on with meanwhile, and what would have to change if the answer is another. One decision an issue, or one issue for the choices of one ADR, each of them a line to tick.
- **The work does not wait** where a default can be taken and changed later: the default is taken, the issue names it, and the ticket is closed as usual. Where the choice is hard to reverse, the ticket stays open (above).
- An answered issue is closed with what was decided, and the ADR, contract or ticket it touches is changed in the same commit.

### Git

Work on `main` and push to `main`. No pull requests. No branch unless something truly needs one, and then it is deleted when merged. The gate is run locally, and nothing of the loop ever waits on a remote check: no workflow runs per push or per pull request. The one workflow there may be is the release pipeline, started by a version tag or by hand, which builds, tests and publishes a release (the product owner's decision of 2026-11-18).

That pipeline is `.github/workflows/release.yml`. A release is made from a commit of `main` like this: change the version in every place `scripts/release-check.sh` names (the package `vibe`, `Cargo.lock`, the `Dockerfile`, `deploy/`, `docs/getting-started.md`) and write the notes `docs/releases/v<version>.md`; run `scripts/release-check.sh` until it passes; run the checks of step 6 on what the release changes (the whole gate and `scripts/install-gate.sh` only when the product owner asks, as above); commit and push to `main`; then push the tag `v<version>` on that commit. The workflow checks the versions, builds the image and the tool, and publishes; **it runs no gate and no install tests** until the product is in use (the product owner's decision of 2026-11-21: a release must not wait two hours for the tests; the release gate comes back at about milestone 4). A run started by hand on a branch is a rehearsal and publishes nothing. The details, and what to do when a run fails, are in `docs/releasing.md`.

### The gate

Since 2026-11-21 the gate is run when the product owner asks for it, not for every ticket ("The loop for every ticket", step 6). This section says what it is when it is run.

`scripts/gate.sh` first looks for the browser the browser tests drive: Firefox 128 or later, the program `VIBE_TEST_BROWSER` names (an absolute path, or a name on the `PATH`), else `firefox` on the `PATH`. Without one it stops at once, before anything is built, in the same words as the browser tests' own failure; with one it says in one line which it found and its version. It then checks that `blackbox` depends on no product crate, and runs:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
npm ci && npm test          # once the root npm workspace exists; needs Node 22 or later
cargo test --workspace
```

The browser walks (`blackbox/tests/browser_*.rs`) are part of `cargo test --workspace`: they start that Firefox, headless, against the real program.

The release pipeline does not run the gate now ("Git", above). When it does again, on its hosted runner the gate is run by `scripts/gate-hosted.sh`: the whole gate once (`scripts/gate.sh --no-fail-fast`), then each test file that failed once more, alone and one test at a time. The gate's time limits are sized for the machines it runs on every day (12 cores); a hosted runner has 4, and there a few test files that start many programs at once run past those limits when every file runs side by side, though each passes alone. Nothing is skipped: a file that fails twice, npm's tests, or a step before the tests failing fails it, and the log says which files were run again and how each ended. Locally, the gate is `scripts/gate.sh` as above.

`scripts/install-gate.sh` is beside it and not part of it: it builds the container image from the working tree and runs the install tests (`blackbox/tests/install_*.rs`, ignored in the gate) against it with Docker and Compose. Like the gate, it is run when the product owner asks; the release pipeline does not run it now.

The SDK (`sdk/`), its type tests and the fixture projects written with it are one npm workspace at the repository root. The black-box tests build those fixture projects with the SDK's tooling and send them to the real program.

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
