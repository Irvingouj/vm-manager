# Vendored crates

Only crates we change are copied here. Everything else is a git or crates.io dependency at a pinned tag (see `../Cargo.lock`).

## openworkers-runtime-v8

- Upstream: https://github.com/openworkers/openworkers-runtime-v8
- Commit: `4ea28c1c26ea10920ad71b8e7bec264f735f7e6e` (tag `v0.18.7`, 2026-11-01)
- Licence: MIT, `LICENSE` kept.
- Copied: `Cargo.toml`, `build.rs`, `LICENSE`, `README.md`, `clippy.toml`, `src/`, `gc-derive/` (via `git archive`). Not copied: `tests/`, `examples/`, `docs/`, `.github/`, `Cargo.lock`, `CLAUDE.md`, `.cargo/`.
- Wired in by `[patch."https://github.com/openworkers/openworkers-runtime-v8"]` in `../Cargo.toml`, so the host's manifest still names the upstream tag.
- The unmodified copy is its own commit on this branch ("Vendor openworkers-runtime-v8 v0.18.7 unmodified"), so `git diff <that commit> -- vendor/` shows exactly our changes.

Changes:

| File | Change |
|---|---|
| `src/security/cpu_enforcer.rs` | The macOS CPU watchdog becomes `watchdog`, shared by macOS and Windows; only reading a thread's CPU time is per OS (`sys`). Windows `sys` holds an `OpenThread` handle per turn-running thread and reads it with `GetThreadTimes`. Gives Windows a CPU-time limit; Linux untouched. |
| `src/security/cpu_timer.rs` | `get_thread_cpu_time` on Windows via `GetThreadTimes(GetCurrentThread())` instead of `None`. |
| `Cargo.toml` | `windows-sys` 0.61 (`Win32_Foundation`, `Win32_System_Threading`) for `cfg(windows)`. |
| `build.rs` | Failing to create the empty snapshot file at `RUNTIME_SNAPSHOT_PATH` is a cargo warning, not a panic, so a deploy path (e.g. `/app/...`) or a Unix path on Windows does not break the build. |
| `src/platform.rs` | New `platform::set_snapshot(&'static [u8]) -> bool`: the host can supply the snapshot at run time instead of the compile-time path. |
| `src/runtime/bindings/fetch.rs` (`setup_invoke`) | Identity spike (`../identity-pass-through`): an invoke call is always polled once inside the guest's JS call, also past the inline budget (16 per turn), so the host's `handle_binding_invoke` is entered while the request that made the call is in flight. Past the budget the (ready) result still goes through the scheduler, so the turn still ends. Upstream sent such calls unpolled to the scheduler, whose task entered the host later, possibly during the next request. |
| `src/runtime/bindings/fetch.rs` (`__nativeFetchStreaming`), `src/runtime/bindings/state.rs`, `src/runtime/scheduler.rs` | Identity spike: outbound `fetch` likewise enters the host (`ops.handle(Operation::Fetch)`) inside the guest's call and polls it once; `SchedulerMessage::FetchStreaming` carries that future instead of the request. `FetchState::ops` is no longer behind the `invoke` feature. Side effect: for a Durable Object, the host's fetch handler is entered before the output gate opens (its future still waits on the gate only if the host defers work to its async part). |
