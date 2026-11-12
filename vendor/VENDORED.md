# Vendored crates

Only crates we change are copied here (ADR 0002). Everything else is a git or crates.io dependency at a pinned tag; see `../Cargo.lock`. In particular `openworkers-core` is the unmodified upstream tag `v0.18.1` (commit `0bdc4e4e`).

## openworkers-runtime-v8

- Upstream: https://github.com/openworkers/openworkers-runtime-v8
- Commit: `4ea28c1c26ea10920ad71b8e7bec264f735f7e6e` (tag `v0.18.7`, 2026-11-01)
- Licence: MIT, `LICENSE` kept.
- Copied: `Cargo.toml`, `build.rs`, `LICENSE`, `README.md`, `clippy.toml`, `src/`, `gc-derive/` (via `git archive`). Not copied: `tests/`, `examples/`, `docs/`, `.github/`, `Cargo.lock`, `CLAUDE.md`, `.cargo/`.
- Wired in by `[patch."https://github.com/openworkers/openworkers-runtime-v8"]` in `../Cargo.toml`, so our manifests still name the upstream tag. The crate is excluded from our workspace and is not held to our lint settings.
- The unmodified copy is its own commit ("Vendor openworkers-runtime-v8 v0.18.7 unmodified"), so `git diff <that commit> -- vendor/openworkers-runtime-v8` shows exactly our changes.
- Taken from the throwaway spike branches `spike/execution-core-cross-platform` and `spike/identity-pass-through`, where the patches were written and tested (`docs/research/execution-core-on-three-platforms.md`, `docs/research/identity-pass-through.md`).

Every upstream upgrade must re-apply both patches.

### Patch 1: a CPU-time limit on Windows

| File | Change |
|---|---|
| `src/security/cpu_enforcer.rs` | The macOS CPU watchdog becomes `watchdog`, shared by macOS and Windows; only reading a thread's CPU time is per OS (`sys`). Windows `sys` holds an `OpenThread` handle per turn-running thread and reads it with `GetThreadTimes`. Linux untouched. |
| `src/security/cpu_timer.rs` | `get_thread_cpu_time` on Windows via `GetThreadTimes(GetCurrentThread())` instead of `None`. |
| `Cargo.toml` | `windows-sys` 0.61 (`Win32_Foundation`, `Win32_System_Threading`) for `cfg(windows)`. |
| `build.rs` | Failing to create the empty snapshot file at `RUNTIME_SNAPSHOT_PATH` is a cargo warning, not a panic. |
| `src/platform.rs` | New `platform::set_snapshot(&'static [u8]) -> bool`: the host can supply the snapshot at run time instead of the compile-time path. |

### Patch 2: the host is entered inside the Application's call

Upstream sent an `invoke` call past the inline budget (16 per turn), and every outbound `fetch`, unpolled to the scheduler, whose task entered the host later, possibly while the next request was in flight. The host reads the request in flight when it is entered, so it could attach one User's operation to another User's request.

| File | Change |
|---|---|
| `src/runtime/bindings/fetch.rs` (`setup_invoke`) | An invoke call is always polled once inside the guest's JS call, also past the inline budget, so the host's `handle_binding_invoke` is entered while the request that made the call is in flight. Past the budget the (ready) result still goes through the scheduler, so the turn still ends. |
| `src/runtime/bindings/fetch.rs` (`__nativeFetchStreaming`), `src/runtime/bindings/state.rs`, `src/runtime/scheduler.rs` | Outbound `fetch` likewise enters the host (`ops.handle(Operation::Fetch)`) inside the guest's call and polls it once; `SchedulerMessage::FetchStreaming` carries that future instead of the request. `FetchState::ops` is no longer behind the `invoke` feature. |

Only these two operation kinds (host functions through `invoke`, and outbound `fetch`) are patched. Every other operation kind of the runtime (binding fetch, storage, KV, database, D1, worker, Durable Objects, WebSocket connect) still enters the host from a scheduler task, so the host must not expose them to Applications.

### Patch 3: `console.log` reaches the host as `Log`

Upstream sent `console.log` and `console.info` to the host with the same level (`Info`), so a host could not report which one the Application called. `openworkers-core`'s `LogLevel` already has a `Log` variant that nothing produced.

| File | Change |
|---|---|
| `src/runtime/bindings/console.rs` | `console.log` passes level code 5, and the native side maps 5 to `LogLevel::Log`. The other methods are unchanged. |
