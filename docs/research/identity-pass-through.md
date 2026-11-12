# Identity pass-through: findings

Date: 2026-11-12 (UTC). Code: branch `spike/identity-pass-through`, under `prototypes/identity-pass-through/`. Resolves the map ticket "Identity pass-through prototype", the gate on ADR 0002.

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently checked:

- the branch and its four commits, and that `main` was not touched;
- the runtime patch, by reading the diff: 3 files, +41/−23 lines;
- [CI run 43147958423](https://github.com/Irvingouj/vm-manager/actions/runs/43147958423) (commit `fcf07ad`): ubuntu, macos and windows jobs all succeeded; the Windows log shows 14 attack tests and 4 pool tests passing by name;
- the attack before the patch, by running `cargo test --release --test attacks` at commit `a25cede` on the development machine: 9 passed, 3 failed (`many_calls_past_the_inline_budget`, `calls_past_the_budget_current_thread_runtime`, `calls_past_the_budget_multi_thread_runtime`). On the multi-thread runtime, 2643, 2155, 527 and 630 of alice's 20000 calls were attached to bob.

Everything else, including the cost measurements and the source line references, is the agent's account.

## 1. Verdict

The seam works, but only after a small patch to the runtime crate. The host can tell which User and which Application an operation belongs to without the Application supplying either. The User sits in a per-worker slot that the host sets before the event runs and clears after it ends. The Application is fixed from host configuration when the worker is built. The host reads the slot synchronously at the moment the runtime enters its handler.

That is only sound if two things hold:

- **One request at a time per context.** A worker or context runs at most one request at a time. This holds in the runtime (see 3). So the request model is a pool of warm contexts, with one context per in-flight request: either the prototype's `AppWorker` pool or upstream `execute_pinned`, which also allows two requests on one isolate in two separate contexts.
- **The host is entered inside the guest's JS call.** Upstream does **not** guarantee this, and the attack succeeded. On a multi-thread tokio runtime, thousands of User A's calls were attributed to User B.

The fix is a patch to the vendored `openworkers-runtime-v8` only: 3 files, +41/−23 lines (`fetch.rs`, `state.rs`, `scheduler.rs`). It covers invoke and outbound `fetch`. No change to `openworkers-core`.

Global state does leak between Users in a reused context, as expected. Two ways to avoid it, both measured in section 4:

- a fresh context on a reused isolate: about 0.6 ms per request, versus about 0.08 ms for a warm one;
- a warm context keyed by (Application, User): tested, no leak.

## 2. Attacks

**Claim another User** (`claim_another_user`, plus smoke): **fails** (ran on the development machine + CI).

- The guest put another User in the arguments, in raw `__nativeBindingInvoke` arguments, in a `Request` it built with the header, and in globals. It also tried an outbound fetch.
- The host attached alice every time. The guest never sees `x-vm-user`, in any case.
- Over HTTP, a request with no header gets 401 and one with two headers gets 400 (smoke, ran on the development machine + CI).

**Claim another Application** (`claim_another_application`): **fails** (ran on the development machine + CI).

- Other binding names (`PAYROLL`, `host`, `""`) reach the host, which refuses them with "no binding X".
- `application` in the arguments is ignored.
- Redefining `env` is refused (it is non-configurable).
- A second worker configured as `payroll` attaches `payroll`.
- Caveat: the binding name is a free string from the guest (read in source, `fetch.rs` `setup_invoke`). The host must check it, and must never use it as identity.

**Keep handles after the request** (`handles_kept_after_the_request_*`, two variants: next request at once, and next request after 500 ms) (ran on the development machine + CI):

- A closure, a detached `invoke`, or the old `env` kept by alice and called during bob's request is attached to **bob**. The call happens in bob's request, so this is by design.
- alice's timer and `waitUntil` work after her answer ran inside her event and were attached to alice. `Worker::exec` drains waitUntil before returning.
- alice's pending host op and a 300 ms timer that outlived her event **never ran JS again**. They are not in the host log.

**No request in flight** (`no_request_in_flight_at_load_and_between_requests`) (ran on the development machine + CI):

- A top-level call at load reaches the host with no identity attached, and the host rejects it.
- A timer firing between requests never runs: reset discards it.
- If the host itself calls `Worker::process_callbacks()` between requests, the timer's call runs exactly once and gets no identity. It fails closed.

**Global state** (`global_state_is_shared_by_users_in_a_reused_context`; pool `a_context_per_application_and_user`) (ran on the development machine + CI):

- Bob reads alice's `globalThis` value in a reused context. This is the expected leak, and the test asserts it.
- A fresh worker per request has no leak.
- A pool context keyed by `demo:<user>` has no leak.
- Related (`work_queued_by_one_user_runs_as_the_next`): alice queues a closure in a global; bob's request runs it and the host attributes it to **bob**. Attribution means "the request in flight at call time". Guest globals can carry one User's intent into another User's request. Only a context that is not shared across Users prevents that.

**Interleaving** (`interleaved_requests_on_two_workers`; pool `two_requests_at_once_on_one_isolate`; smoke) (ran on the development machine + CI):

- Two Users were in flight at once. Each awaited a pending host op (150 ms) and a timer (100 ms), then asked again.
- Each always saw its own identity, on two Workers, and on one isolate with two contexts (`max_concurrent_per_isolate=2`, `max_per_thread=1`, isolate count asserted to be 1).
- One `Worker` cannot have two requests in flight, because `exec` takes `&mut self` (section 3).

**Calls past the inline budget, and unawaited fetch.** This is where the attack **succeeded before the patch**.

- What the guest did: alice makes N calls without awaiting them and answers; bob's request follows.
- Before the patch:
  - With 40 calls: only 16 reached the host during alice's request. The other 24 reached it later with no identity, or never (ran on the development machine).
  - With 2000 calls × 10 rounds on a **multi-thread** runtime: 2504, 2369, 834 and 802 of alice's 20000 calls (for k = 0–3 yields between the requests) were **attached to bob**.
  - On a current-thread runtime: no calls attached to bob were seen, but 19330 had no identity.
- Outbound `fetch` behaves the same, because the runtime never enters the host inline for it. Before its patch: 145, 29, 21 and 49 of 2000 were attached to bob on multi-thread.
- Cause (read in source): calls past `MAX_INLINE_CALLS = 16` (`fetch.rs:677`) and every fetch were sent **unpolled** as `SchedulerMessage`s. A `tokio::spawn`ed task entered `ops.handle` later (`scheduler.rs:416-431` for invoke, `268-296` for fetch), possibly after the host had already attached the next request.
- Fix (commit `ecef5a4`):
  - Invoke is always polled once inside the JS call; past the budget, the ready result still goes through the scheduler.
  - Fetch is entered and polled once in `__nativeFetchStreaming`, and `FetchStreaming` now carries that future.
  - After the fix: all calls are attached to alice in every variant (tests `many_calls_past_the_inline_budget`, `calls_past_the_budget_*`, `unawaited_fetches_*`; ran on the development machine + CI).

**Tampering with the JS glue** (`tampering_with_the_runtime_glue`) (ran on the development machine + CI):

- alice's request wrapped `__nativeBindingInvoke`, broke `Map`/`Set.prototype.clear` and `Reflect.apply`, hooked `Promise.prototype.then`, replaced `__ow.asyncContextSet` and `Object.freeze`, and tried to redefine `__timerCallbacks`.
- The next request (bob), and one after a 100 ms wait (carol), were still attached correctly, and alice's timer never fired.
- Why: identity never passes through JS, and reset uses builtins captured before guest code runs (`execution_context.rs:1435-1455`).

## 3. Mechanism and invariants

- **Where identity lives** (`host/src/lib.rs`): `HostOps { application, current: Mutex<Option<Attached>>, audit }`, one per worker/context.
  - `AppWorker::serve` strips the header, calls `begin(user)` (User + Application + request id), runs `Worker::exec`, and clears the slot through a Drop guard, so it is also cleared if the future is dropped.
  - `handle_binding_invoke` and `handle_fetch` read the slot first, synchronously.
- **Path of a call**: `env.HOST.invoke` (`js/env.js:257-263`) → `__nativeBindingInvoke` (`fetch.rs:684`, `setup_invoke`) → `ops.handle(Operation::BindingInvoke)`, polled inline (patched) → the core's default `handle` → the host's `handle_binding_invoke`.
- **Invariants**, all read in source; line numbers are in the patched vendored copy:
  1. One event at a time per context:
     - `Worker::exec(&mut self)` (`worker.rs:146`), which runs exec and then `drain_waituntil`.
     - The pool removes a cached context from the cache for the duration of a request (`pool.rs:573`). Concurrent requests get other contexts, each with its own ops.
  2. Guest JS runs only while the host drives the context: load, `exec`/drain, or `process_callbacks` (`execution_context.rs:600`).
     - Reset clears timers and async context through builtins captured before guest code (`1435-1455`, `1552`).
     - Reset drains queued callbacks (`1561`) and clears promise callbacks (`1567`).
     - `BeginRequest` cancels the previous request's ops (`scheduler.rs:626-628`).
  3. The host is entered inside the JS call. This holds only with the patch, and only for invoke and fetch.
  4. The host must read identity synchronously in its handler, not after an `.await`. This is a host rule.
  5. On a warm hit, `execute_pinned` **ignores the request's `ops`** and uses the cached context's ops (`pool.rs:621`, `646`). Identity must therefore be set on the cached ops in `on_warm_hit` (test `naive_per_request_ops_are_ignored_on_a_warm_hit` shows bob getting alice; `slot_on_the_context_ops_follows_each_request` shows the fix).

## 4. Measured costs

Each request is one `/whoami` with one host op, n=300.

On the development machine (AMD Ryzen 9 7900), command `idp-host bench guest/attacks.js <mode> 300`, with and without `EC_SNAPSHOT`:

| Mode | Mean, no snapshot | p50 | Mean, with snapshot |
|---|---|---|---|
| warm (one Worker reused) | 86 µs | 76 µs | 96 µs |
| fresh isolate per request (`Worker::new`) | 4.06 ms | — | 3.00 ms |
| pool-warm | 106 µs | 79 µs | 103 µs |
| pool-fresh-context (new context on a reused isolate) | 684 µs | 584 µs | 656 µs |

Same command in [CI run 43147379024](https://github.com/Irvingouj/vm-manager/actions/runs/43147379024), means:

| Mode | ubuntu | macos | windows |
|---|---|---|---|
| warm | 103 µs | 78 µs | 134 µs |
| fresh-isolate | 6.03 ms | 5.43 ms | 8.80 ms |
| pool-fresh-context | 0.97 ms | 1.08 ms | 1.41 ms |

## 5. What was built, commits, CI

Files:

- `prototypes/identity-pass-through/`:
  - `host/src/lib.rs` (HostOps, AppWorker);
  - `host/src/main.rs` (HTTP server with a worker-per-in-flight-request pool, `bench`, `snapshot`);
  - `host/tests/attacks.rs` (14 tests), `host/tests/pool.rs` (4 tests);
  - `guest/attacks.js`, `smoke.sh`.
- `.github/workflows/identity-pass-through.yml`.
- The execution-core vendored runtime is reused by path and patched in place, documented in `prototypes/execution-core/vendor/VENDORED.md`. The execution-core tests and smoke still pass on the development machine.

Commits:

- `a25cede`: host, attacks and tests before the patch (three tests fail).
- `ecef5a4`: runtime patch, plus the fetch attack.
- `8178b03`: pool tests, smoke, bench, CI.
- `fcf07ad`: tightened one assertion.

CI is green on ubuntu, macos and windows: [run 43147379024](https://github.com/Irvingouj/vm-manager/actions/runs/43147379024) and [run 43147958423](https://github.com/Irvingouj/vm-manager/actions/runs/43147958423) (the latter for `fcf07ad`).

To repeat:

```bash
cd prototypes/identity-pass-through
CARGO_TARGET_DIR=/tmp/ec-target cargo test --release
./smoke.sh /tmp/ec-target/release/idp-host
```

To see the failure, check out `a25cede` and run `cargo test --release --test attacks`.

## 6. Still unknown

- **Other op classes are unpatched.** Binding fetch, storage, KV, database, D1, worker binding, Durable Objects and WebSocket connect still enter the host from spawned scheduler tasks, so they have the same race (from source, not tested). Each would need the same treatment of about 10 lines, or must not be exposed to Applications.
- Whether the unpatched race can attach the wrong User on a current-thread runtime: not observed, not proven impossible.
- The fetch patch enters the host before a Durable Object's output gate opens. The platform does not use Durable Objects; not tested.
- The vendored crate's own upstream test suite was not run against the patch.
- Memory per cached context, which matters for a context per (Application, User): not measured.
- The behaviour after a CPU-limit kill mid-request, with ops left in the queue: not tested for identity.
- Real sign-in, and a guest's outbound fetch to the platform's own ingress with a forged header: out of scope. The prototype's egress stub only answers `http://host.test/`.
